//! xseed 社区节点协议（03 §1.4、§1.7、§2.1）。
//!
//! 节点与小助手共用：消息格式（CBOR）、两跳封装（HPKE）、代号加解密、
//! 信箱鉴权、节点间请求签名、探测标签、注册中心清单，以及小助手侧的节点客户端。

pub mod alias;
pub mod auth;
pub mod client;
pub mod ohttp;
pub mod probe;
pub mod registry;
pub mod wire;

/// 协议版本。
pub const PROTO: u8 = 2;

pub fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// 代号使用的周序号：自 Unix 纪元起的整周数（u16 可用到 3226 年）。
pub fn week(now: u64) -> u16 {
    (now / 604_800) as u16
}

/// 登记过期使用的日序号：自 Unix 纪元起的天数。
pub fn day(now: u64) -> u16 {
    (now / 86_400) as u16
}

pub fn sha256(parts: &[&[u8]]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    for p in parts {
        h.update(p);
    }
    h.finalize().into()
}

pub fn random<const N: usize>() -> [u8; N] {
    use rand::RngCore;
    let mut b = [0u8; N];
    rand::rngs::OsRng.fill_bytes(&mut b);
    b
}
