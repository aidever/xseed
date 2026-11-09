//! 线上消息格式。除 `/.well-known/xseed` 与错误响应为 JSON 外，均为 CBOR。

use serde::{de::DeserializeOwned, Deserialize, Serialize};

pub type NodeId = [u8; 16];
pub type Alias = [u8; 16];

/// 节点错误码（03 §9.1）。
pub mod code {
    pub const BAD_REQUEST: &str = "xseed.node.bad_request";
    pub const POW_INVALID: &str = "xseed.node.pow_invalid";
    pub const AUTH_FAILED: &str = "xseed.node.auth_failed";
    pub const SIG_INVALID: &str = "xseed.node.sig_invalid";
    pub const ALIAS_INVALID: &str = "xseed.node.alias_invalid";
    pub const UNKNOWN_NODE: &str = "xseed.node.unknown_node";
    pub const MAILBOX_OFFLINE: &str = "xseed.node.mailbox_offline";
    pub const RATE_LIMITED: &str = "xseed.node.rate_limited";
    pub const OVERLOADED: &str = "xseed.node.overloaded";
}

/// 单个信封正文上限（会话消息 ≤ 4 KiB，加上填充与 Noise 开销）。
pub const MAX_BODY: usize = 4608;
/// 两跳请求上限。
pub const MAX_OHTTP: usize = 16 * 1024;
/// 每次登记最多条目数。
pub const MAX_REGISTER: usize = 64;
/// 每次查询最多桶数。
pub const MAX_LOOKUP: usize = 16;
/// 每个桶最多返回条目数。
pub const MAX_HITS: usize = 128;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Addr {
    /// 代号所在入口节点的 node_tag（node_id 前 4 字节）。
    pub node: u32,
    #[serde(with = "serde_bytes")]
    pub alias: Alias,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Probe,
    ProbeAck,
    Noise,
    Close,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Envelope {
    pub v: u8,
    pub to: Addr,
    pub from: Addr,
    #[serde(with = "serde_bytes")]
    pub sid: [u8; 16],
    pub kind: Kind,
    #[serde(with = "serde_bytes")]
    pub body: Vec<u8>,
}

/// 信箱 WebSocket 帧。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum WsMsg {
    Chal {
        #[serde(with = "serde_bytes")]
        challenge: [u8; 32],
    },
    Auth {
        #[serde(with = "serde_bytes")]
        slot_token: [u8; 16],
        #[serde(with = "serde_bytes")]
        mac: [u8; 32],
    },
    Ok,
    Env {
        env: Envelope,
    },
    /// 信封无法投递（目标不在线、代号无效、限流）。
    Nack {
        #[serde(with = "serde_bytes")]
        sid: [u8; 16],
        code: String,
    },
    Ping,
    Pong,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pow {
    #[serde(with = "serde_bytes")]
    pub salt: [u8; 16],
    pub nonce: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct MailboxReq {
    pub pow: Pow,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct MailboxResp {
    #[serde(with = "serde_bytes")]
    pub slot_token: [u8; 16],
    #[serde(with = "serde_bytes")]
    pub secret: [u8; 32],
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct AliasReq {
    pub n: u16,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct AliasResp {
    /// n 个 16 字节代号首尾相接。
    #[serde(with = "serde_bytes")]
    pub aliases: Vec<u8>,
}

impl AliasResp {
    pub fn split(&self) -> Vec<Alias> {
        self.aliases.as_chunks::<16>().0.to_vec()
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct OhttpReq {
    #[serde(with = "serde_bytes")]
    pub target: NodeId,
    #[serde(with = "serde_bytes")]
    pub req: Vec<u8>,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct RegEntry {
    pub b: u16,
    pub node: u32,
    #[serde(with = "serde_bytes")]
    pub alias: Alias,
}

/// 两跳封装内的桶节点操作（03 §1.4.3）。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum GwReq {
    Register { entries: Vec<RegEntry>, ttl_d: u8, pow: Pow },
    Lookup { buckets: Vec<u16> },
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct BucketHits {
    pub b: u16,
    pub hits: Vec<Addr>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "r", rename_all = "snake_case")]
pub enum GwResp {
    Registered { n: u32 },
    Found { buckets: Vec<BucketHits> },
    Error { code: String },
}

/// 探测正文（03 §1.5.1）。
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ProbeBody {
    #[serde(with = "serde_bytes")]
    pub nonce: [u8; 16],
    #[serde(with = "serde_bytes")]
    pub tag: [u8; 16],
}

/// `GET /.well-known/xseed`
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct WellKnown {
    pub proto: Vec<u8>,
    pub node_id: String,
    pub load: f32,
    pub time: u64,
    pub pow_salt: String,
    pub mailbox_bits: u32,
    pub register_bits: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub challenge_sig: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ErrorBody {
    pub error: String,
}

pub fn to_cbor<T: Serialize>(v: &T) -> Vec<u8> {
    let mut out = Vec::new();
    ciborium::into_writer(v, &mut out).expect("CBOR 编码不会失败");
    out
}

pub fn from_cbor<T: DeserializeOwned>(b: &[u8]) -> Result<T, String> {
    ciborium::from_reader(b).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let env = Envelope { v: 2, to: Addr { node: 1, alias: [1; 16] }, from: Addr { node: 2, alias: [2; 16] }, sid: [3; 16], kind: Kind::ProbeAck, body: vec![9, 9] };
        for m in [WsMsg::Env { env: env.clone() }, WsMsg::Ok, WsMsg::Chal { challenge: [5; 32] }, WsMsg::Nack { sid: [1; 16], code: "x".into() }] {
            assert_eq!(from_cbor::<WsMsg>(&to_cbor(&m)).unwrap(), m);
        }
        let r = GwReq::Register { entries: vec![RegEntry { b: 7, node: 9, alias: [4; 16] }], ttl_d: 3, pow: Pow { salt: [1; 16], nonce: 42 } };
        assert_eq!(from_cbor::<GwReq>(&to_cbor(&r)).unwrap(), r);
        let f = GwResp::Found { buckets: vec![BucketHits { b: 1, hits: vec![env.to] }] };
        assert_eq!(from_cbor::<GwResp>(&to_cbor(&f)).unwrap(), f);
    }
}
