//! 节点 HTTP 接口（03 §2.1）。

use crate::node::{status_of, Code, Shared};
use axum::body::Bytes;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, DefaultBodyLimit, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::{SinkExt, StreamExt};
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::Ordering::Relaxed;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use xseed_transport::wire::*;
use xseed_transport::{auth, now};

pub fn router(node: Shared) -> Router {
    Router::new()
        .route("/.well-known/xseed", get(well_known))
        .route("/v1/mailbox", post(mailbox))
        .route("/v1/mailbox/ws", get(ws))
        .route("/v1/aliases", post(aliases))
        .route("/v1/ohttp", post(ohttp))
        .route("/v1/gateway", post(gateway))
        .route("/v1/fwd", post(fwd))
        .route("/metrics", get(metrics))
        .layer(DefaultBodyLimit::max(32 * 1024))
        .with_state(node)
}

fn err(c: Code) -> Response {
    (StatusCode::from_u16(status_of(c)).unwrap(), Json(ErrorBody { error: c.into() })).into_response()
}

fn cbor<T: serde::Serialize>(v: &T) -> Response {
    ([("content-type", "application/cbor")], to_cbor(v)).into_response()
}

fn hdr<'a>(h: &'a HeaderMap, k: &str) -> Option<&'a str> {
    h.get(k).and_then(|v| v.to_str().ok())
}

fn client_ip(node: &Shared, h: &HeaderMap, peer: SocketAddr) -> IpAddr {
    if node.cfg.trust_proxy {
        if let Some(ip) = hdr(h, "x-forwarded-for").and_then(|v| v.split(',').next()).and_then(|v| v.trim().parse().ok()) {
            return ip;
        }
    }
    peer.ip()
}

#[derive(serde::Deserialize)]
struct WkQuery {
    challenge: Option<String>,
}

async fn well_known(State(n): State<Shared>, Query(q): Query<WkQuery>) -> Response {
    let challenge_sig = q.challenge.and_then(|c| hex::decode(c).ok()).filter(|c| c.len() == 32).map(|c| {
        use ed25519_dalek::Signer;
        hex::encode(n.sign.sign(&[auth::CTX_WK, &c].concat()).to_bytes())
    });
    let load = n.online() as f32 / n.cfg.limits.max_connections.max(1) as f32;
    Json(WellKnown {
        proto: vec![xseed_transport::PROTO],
        node_id: hex::encode(n.id),
        load,
        time: now(),
        pow_salt: hex::encode(n.pow_salt()),
        mailbox_bits: n.cfg.pow.mailbox_bits,
        register_bits: n.cfg.pow.register_bits,
        challenge_sig,
    })
    .into_response()
}

async fn mailbox(State(n): State<Shared>, ConnectInfo(peer): ConnectInfo<SocketAddr>, h: HeaderMap, body: Bytes) -> Response {
    let Ok(req) = from_cbor::<MailboxReq>(&body) else { return err(code::BAD_REQUEST) };
    match n.create_mailbox(client_ip(&n, &h, peer), &req.pow) {
        Ok(r) => cbor(&r),
        Err(c) => err(c),
    }
}

async fn aliases(State(n): State<Shared>, h: HeaderMap, body: Bytes) -> Response {
    let (slot, gen) = match n.auth_request(hdr(&h, "authorization"), "/v1/aliases", &body) {
        Ok(x) => x,
        Err(c) => return err(c),
    };
    let Ok(req) = from_cbor::<AliasReq>(&body) else { return err(code::BAD_REQUEST) };
    match n.aliases(slot, gen, req.n) {
        Ok(a) => cbor(&AliasResp { aliases: a }),
        Err(c) => err(c),
    }
}

async fn ohttp(State(n): State<Shared>, h: HeaderMap, body: Bytes) -> Response {
    let (slot, _) = match n.auth_request(hdr(&h, "authorization"), "/v1/ohttp", &body) {
        Ok(x) => x,
        Err(c) => return err(c),
    };
    if !n.check_ohttp_quota(slot) {
        return err(code::RATE_LIMITED);
    }
    let Ok(req) = from_cbor::<OhttpReq>(&body) else { return err(code::BAD_REQUEST) };
    match n.relay(&req.target, req.req).await {
        Ok(b) => ([("content-type", "application/octet-stream")], b).into_response(),
        Err(c) => err(c),
    }
}

async fn gateway(State(n): State<Shared>, h: HeaderMap, body: Bytes) -> Response {
    if let Err(c) = n.verify_node(hdr(&h, auth::H_NODE), hdr(&h, auth::H_TS), hdr(&h, auth::H_SIG), auth::CTX_GW, &body) {
        return err(c);
    }
    match n.gateway(&body) {
        Ok(b) => ([("content-type", "application/octet-stream")], b).into_response(),
        Err(c) => err(c),
    }
}

async fn fwd(State(n): State<Shared>, h: HeaderMap, body: Bytes) -> Response {
    let origin = match n.verify_node(hdr(&h, auth::H_NODE), hdr(&h, auth::H_TS), hdr(&h, auth::H_SIG), auth::CTX_FWD, &body) {
        Ok(m) => m,
        Err(c) => return err(c),
    };
    let Ok(env) = from_cbor::<Envelope>(&body) else { return err(code::BAD_REQUEST) };
    if env.from.node != origin.tag() || env.body.len() > MAX_BODY {
        return err(code::ALIAS_INVALID);
    }
    match n.deliver_local(env) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(c) => err(c),
    }
}

async fn metrics(State(n): State<Shared>) -> String {
    let m = &n.metrics;
    format!(
        "xseed_mailboxes_online {}\nxseed_index_entries {}\nxseed_fwd_total{{result=\"ok\"}} {}\nxseed_fwd_total{{result=\"fail\"}} {}\nxseed_gateway_requests_total{{op=\"register\"}} {}\nxseed_gateway_requests_total{{op=\"lookup\"}} {}\nxseed_gateway_failures_total {}\nxseed_pow_rejected_total {}\nxseed_delivered_total {}\nxseed_mailbox_offline_total {}\n",
        n.online(),
        n.index_entries(),
        m.fwd_ok.load(Relaxed),
        m.fwd_fail.load(Relaxed),
        m.gw_register.load(Relaxed),
        m.gw_lookup.load(Relaxed),
        m.gw_fail.load(Relaxed),
        m.pow_rejected.load(Relaxed),
        m.delivered.load(Relaxed),
        m.offline.load(Relaxed),
    )
}

async fn ws(State(n): State<Shared>, up: WebSocketUpgrade) -> Response {
    up.max_message_size(MAX_BODY + 1024).on_upgrade(move |s| session(n, s))
}

async fn send(ws: &mut futures_util::stream::SplitSink<WebSocket, Message>, m: &WsMsg) -> bool {
    ws.send(Message::Binary(to_cbor(m).into())).await.is_ok()
}

async fn session(n: Shared, socket: WebSocket) {
    let (mut sink, mut stream) = socket.split();
    let challenge: [u8; 32] = xseed_transport::random();
    if !send(&mut sink, &WsMsg::Chal { challenge }).await {
        return;
    }
    let auth = tokio::time::timeout(Duration::from_secs(10), stream.next()).await;
    let Ok(Some(Ok(Message::Binary(b)))) = auth else { return };
    let Ok(WsMsg::Auth { slot_token, mac }) = from_cbor::<WsMsg>(&b) else { return };
    let (slot, gen) = match n.auth_ws(&slot_token, &challenge, &mac) {
        Ok(x) => x,
        Err(c) => {
            let _ = send(&mut sink, &WsMsg::Nack { sid: [0; 16], code: c.into() }).await;
            return;
        }
    };
    let (tx, mut rx) = mpsc::channel::<WsMsg>(256);
    let conn = match n.attach(slot, tx.clone()) {
        Ok(c) => c,
        Err(c) => {
            let _ = send(&mut sink, &WsMsg::Nack { sid: [0; 16], code: c.into() }).await;
            return;
        }
    };
    if !send(&mut sink, &WsMsg::Ok).await {
        n.detach(slot, conn);
        return;
    }
    let mut ping = tokio::time::interval(Duration::from_secs(30));
    let mut last_rx = Instant::now();
    loop {
        tokio::select! {
            m = rx.recv() => match m {
                Some(m) => if !send(&mut sink, &m).await { break },
                None => break, // 被新连接顶替
            },
            m = stream.next() => match m {
                Some(Ok(Message::Binary(b))) => {
                    last_rx = Instant::now();
                    match from_cbor::<WsMsg>(&b) {
                        Ok(WsMsg::Env { env }) => {
                            let sid = env.sid;
                            // 投递可能要访问其他节点，放到后台，避免阻塞接收
                            let (n2, tx2) = (n.clone(), tx.clone());
                            tokio::spawn(async move {
                                if let Err(c) = n2.send_from(slot, gen, env).await {
                                    let _ = tx2.try_send(WsMsg::Nack { sid, code: c.into() });
                                }
                            });
                        }
                        Ok(WsMsg::Ping) => { let _ = tx.try_send(WsMsg::Pong); }
                        Ok(_) => {}
                        Err(_) => break,
                    }
                }
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                Some(Ok(_)) => last_rx = Instant::now(),
            },
            _ = ping.tick() => {
                if last_rx.elapsed() > Duration::from_secs(90) { break }
                if !send(&mut sink, &WsMsg::Ping).await { break }
            }
        }
    }
    n.detach(slot, conn);
}
