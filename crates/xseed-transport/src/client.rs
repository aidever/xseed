//! 小助手侧的节点客户端：探活、开信箱、领代号、两跳请求、信箱长连接。

use crate::auth;
use crate::registry::NodeManifest;
use crate::wire::*;
use anyhow::{anyhow, bail, Result};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

/// 节点返回的错误（HTTP 状态 + 错误码）。
#[derive(Debug, thiserror::Error)]
#[error("{code}（HTTP {status}）")]
pub struct NodeError {
    pub status: u16,
    pub code: String,
}

/// 信箱凭证，保存在小助手的加密存储中。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct MailboxCred {
    #[serde(with = "hex_arr")]
    pub slot_token: [u8; 16],
    #[serde(with = "hex_arr")]
    pub secret: [u8; 32],
}

mod hex_arr {
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer, const N: usize>(v: &[u8; N], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&hex::encode(v))
    }
    pub fn deserialize<'de, D: Deserializer<'de>, const N: usize>(d: D) -> Result<[u8; N], D::Error> {
        let s = String::deserialize(d)?;
        hex::decode(s).map_err(serde::de::Error::custom)?.try_into().map_err(|_| serde::de::Error::custom("长度错误"))
    }
}

#[derive(Clone)]
pub struct NodeClient {
    pub m: NodeManifest,
    base: String,
    http: reqwest::Client,
}

pub fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .connect_timeout(Duration::from_secs(8))
        .user_agent(concat!("xseed/", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("HTTP 客户端")
}

async fn check(r: reqwest::Response) -> Result<Vec<u8>> {
    let status = r.status();
    let body = r.bytes().await?.to_vec();
    if status.is_success() {
        return Ok(body);
    }
    let code = serde_json::from_slice::<ErrorBody>(&body).map(|e| e.error).unwrap_or_else(|_| format!("http_{}", status.as_u16()));
    Err(NodeError { status: status.as_u16(), code }.into())
}

impl NodeClient {
    pub fn new(m: NodeManifest, http: reqwest::Client) -> Self {
        let base = m.url.trim_end_matches('/').to_string();
        NodeClient { m, base, http }
    }

    /// 探活，返回节点信息与往返时间。
    pub async fn well_known(&self) -> Result<(WellKnown, Duration)> {
        let t = Instant::now();
        let r = self.http.get(format!("{}/.well-known/xseed", self.base)).send().await?;
        let wk: WellKnown = serde_json::from_slice(&check(r).await?)?;
        if wk.node_id != self.m.id {
            bail!("节点 ID 不符");
        }
        Ok((wk, t.elapsed()))
    }

    pub async fn create_mailbox(&self, wk: &WellKnown) -> Result<MailboxCred> {
        let pow = solve_pow(&wk.pow_salt, wk.mailbox_bits).await?;
        let r = self.http.post(format!("{}/v1/mailbox", self.base)).body(to_cbor(&MailboxReq { pow })).send().await?;
        let m: MailboxResp = from_cbor(&check(r).await?).map_err(|e| anyhow!(e))?;
        Ok(MailboxCred { slot_token: m.slot_token, secret: m.secret })
    }

    async fn authed(&self, cred: &MailboxCred, path: &str, body: Vec<u8>) -> Result<Vec<u8>> {
        let ts = crate::now();
        let mac = auth::request_mac(&cred.secret, path, ts, &body);
        let r = self.http.post(format!("{}{}", self.base, path)).header("authorization", auth::auth_header(&cred.slot_token, ts, &mac)).body(body).send().await?;
        check(r).await
    }

    pub async fn aliases(&self, cred: &MailboxCred, n: u16) -> Result<Vec<Alias>> {
        let b = self.authed(cred, "/v1/aliases", to_cbor(&AliasReq { n })).await?;
        Ok(from_cbor::<AliasResp>(&b).map_err(|e| anyhow!(e))?.split())
    }

    /// 经本节点（入口）把请求两跳转给 `target`。
    pub async fn ohttp(&self, cred: &MailboxCred, target: &NodeManifest, req: &GwReq) -> Result<GwResp> {
        let tid = target.id_bytes()?;
        let (ct, key) = crate::ohttp::seal_request(&target.hpke_key()?, &tid, &to_cbor(req))?;
        let resp = self.authed(cred, "/v1/ohttp", to_cbor(&OhttpReq { target: tid, req: ct })).await?;
        let plain = crate::ohttp::open_response(&key, &resp)?;
        let r: GwResp = from_cbor(&plain).map_err(|e| anyhow!(e))?;
        if let GwResp::Error { code } = &r {
            return Err(NodeError { status: 400, code: code.clone() }.into());
        }
        Ok(r)
    }

    /// 连接信箱。返回发送端与事件接收端；连接断开时接收端收到 `Closed`。
    pub async fn connect(&self, cred: &MailboxCred) -> Result<Mailbox> {
        let url = format!("{}/v1/mailbox/ws", self.base.replacen("http", "ws", 1));
        let (mut ws, _) = tokio::time::timeout(Duration::from_secs(15), tokio_tungstenite::connect_async(url)).await.map_err(|_| anyhow!("连接超时"))??;
        let first = tokio::time::timeout(Duration::from_secs(10), ws.next()).await.map_err(|_| anyhow!("等待挑战超时"))?;
        let Some(Ok(Message::Binary(b))) = first else { bail!("信箱握手失败") };
        let WsMsg::Chal { challenge } = from_cbor(&b).map_err(|e| anyhow!(e))? else { bail!("信箱握手失败") };
        let auth = WsMsg::Auth { slot_token: cred.slot_token, mac: auth::ws_mac(&cred.secret, &challenge) };
        ws.send(Message::Binary(to_cbor(&auth).into())).await?;
        match tokio::time::timeout(Duration::from_secs(10), ws.next()).await {
            Ok(Some(Ok(Message::Binary(b)))) if from_cbor::<WsMsg>(&b) == Ok(WsMsg::Ok) => {}
            _ => return Err(NodeError { status: 401, code: code::AUTH_FAILED.into() }.into()),
        }
        let (out_tx, mut out_rx) = mpsc::channel::<Envelope>(256);
        let (ev_tx, ev_rx) = mpsc::channel::<Event>(1024);
        tokio::spawn(async move {
            let (mut sink, mut stream) = ws.split();
            let mut ping = tokio::time::interval(Duration::from_secs(30));
            let mut last_rx = Instant::now();
            let reason = loop {
                tokio::select! {
                    m = out_rx.recv() => match m {
                        Some(env) => if sink.send(Message::Binary(to_cbor(&WsMsg::Env { env }).into())).await.is_err() { break "发送失败".to_string() },
                        None => { let _ = sink.close().await; break "已关闭".into() }
                    },
                    m = stream.next() => match m {
                        Some(Ok(Message::Binary(b))) => {
                            last_rx = Instant::now();
                            match from_cbor::<WsMsg>(&b) {
                                Ok(WsMsg::Env { env }) => { let _ = ev_tx.send(Event::Env(env)).await; }
                                Ok(WsMsg::Nack { sid, code }) => { let _ = ev_tx.send(Event::Nack { sid, code }).await; }
                                Ok(WsMsg::Ping) => { let _ = sink.send(Message::Binary(to_cbor(&WsMsg::Pong).into())).await; }
                                _ => {}
                            }
                        }
                        Some(Ok(Message::Close(_))) | None => break "节点断开".into(),
                        Some(Ok(_)) => last_rx = Instant::now(),
                        Some(Err(e)) => break e.to_string(),
                    },
                    _ = ping.tick() => {
                        if last_rx.elapsed() > Duration::from_secs(90) { break "心跳超时".into() }
                        if sink.send(Message::Binary(to_cbor(&WsMsg::Ping).into())).await.is_err() { break "发送失败".into() }
                    }
                }
            };
            let _ = ev_tx.send(Event::Closed(reason)).await;
        });
        Ok(Mailbox { tx: out_tx, rx: ev_rx })
    }
}

#[derive(Debug)]
pub enum Event {
    Env(Envelope),
    Nack { sid: [u8; 16], code: String },
    Closed(String),
}

pub struct Mailbox {
    pub tx: mpsc::Sender<Envelope>,
    pub rx: mpsc::Receiver<Event>,
}

/// 在阻塞线程池里求解工作量证明。
pub async fn solve_pow(server_salt_hex: &str, bits: u32) -> Result<Pow> {
    let ss = hex::decode(server_salt_hex)?;
    let salt: [u8; 16] = crate::random();
    let nonce = tokio::task::spawn_blocking(move || xseed_core::pow::solve(&ss, &salt, bits)).await?;
    Ok(Pow { salt, nonce })
}
