//! 信箱鉴权（agent → 入口节点）与节点间请求签名（03 §1.7、§2.1）。

use ed25519_dalek::{Signer, SigningKey, Verifier, VerifyingKey};
use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// 签名与鉴权允许的时间偏差。
pub const MAX_SKEW: u64 = 60;

pub const CTX_FWD: &[u8] = b"xseed/fwd/v1";
pub const CTX_GW: &[u8] = b"xseed/gw/v1";
pub const CTX_WK: &[u8] = b"xseed/wk/v1";

pub const H_NODE: &str = "x-xseed-node";
pub const H_TS: &str = "x-xseed-ts";
pub const H_SIG: &str = "x-xseed-sig";

pub fn hmac(key: &[u8], parts: &[&[u8]]) -> [u8; 32] {
    let mut m = <HmacSha256 as Mac>::new_from_slice(key).expect("HMAC 接受任意长度密钥");
    for p in parts {
        m.update(p);
    }
    m.finalize().into_bytes().into()
}

pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    use subtle::ConstantTimeEq;
    a.len() == b.len() && bool::from(a.ct_eq(b))
}

/// 信箱请求 MAC：`HMAC(secret, "xseed/req/v1" ‖ path ‖ 0x00 ‖ u64be(ts) ‖ H(body))`。
pub fn request_mac(secret: &[u8; 32], path: &str, ts: u64, body: &[u8]) -> [u8; 32] {
    hmac(secret, &[b"xseed/req/v1", path.as_bytes(), &[0], &ts.to_be_bytes(), &crate::sha256(&[body])])
}

pub fn ws_mac(secret: &[u8; 32], challenge: &[u8; 32]) -> [u8; 32] {
    hmac(secret, &[b"xseed/ws/v1", challenge])
}

pub fn auth_header(slot_token: &[u8; 16], ts: u64, mac: &[u8; 32]) -> String {
    format!("XSeed slot={}, ts={}, mac={}", hex::encode(slot_token), ts, hex::encode(mac))
}

pub fn parse_auth_header(v: &str) -> Option<([u8; 16], u64, [u8; 32])> {
    let rest = v.strip_prefix("XSeed ")?;
    let (mut slot, mut ts, mut mac) = (None, None, None);
    for kv in rest.split(',') {
        let (k, v) = kv.trim().split_once('=')?;
        match k {
            "slot" => slot = hex::decode(v).ok()?.try_into().ok(),
            "ts" => ts = v.parse().ok(),
            "mac" => mac = hex::decode(v).ok()?.try_into().ok(),
            _ => {}
        }
    }
    Some((slot?, ts?, mac?))
}

fn sig_msg(ctx: &[u8], body: &[u8], ts: u64) -> Vec<u8> {
    [ctx, &crate::sha256(&[body]), &ts.to_be_bytes()].concat()
}

/// 节点间请求签名：`Ed25519(sk, ctx ‖ H(body) ‖ u64be(ts))`。
pub fn node_sign(sk: &SigningKey, ctx: &[u8], body: &[u8], ts: u64) -> [u8; 64] {
    sk.sign(&sig_msg(ctx, body, ts)).to_bytes()
}

pub fn node_verify(pk: &VerifyingKey, ctx: &[u8], body: &[u8], ts: u64, sig: &[u8]) -> bool {
    let Ok(sig) = ed25519_dalek::Signature::from_slice(sig) else { return false };
    pk.verify(&sig_msg(ctx, body, ts), &sig).is_ok()
}

pub fn fresh(ts: u64, now: u64) -> bool {
    ts.abs_diff(now) <= MAX_SKEW
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_and_sig() {
        let h = auth_header(&[1; 16], 99, &[2; 32]);
        assert_eq!(parse_auth_header(&h), Some(([1; 16], 99, [2; 32])));
        let sk = SigningKey::from_bytes(&[3; 32]);
        let s = node_sign(&sk, CTX_FWD, b"body", 5);
        assert!(node_verify(&sk.verifying_key(), CTX_FWD, b"body", 5, &s));
        assert!(!node_verify(&sk.verifying_key(), CTX_GW, b"body", 5, &s));
        assert!(!node_verify(&sk.verifying_key(), CTX_FWD, b"body", 6, &s));
    }
}
