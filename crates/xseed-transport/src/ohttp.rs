//! 两跳封装（03 §1.4.2）：入口节点只看到目标节点与密文长度，桶节点看不到来源 IP。
//!
//! HPKE Base 模式，DHKEM(X25519, HKDF-SHA256) + HKDF-SHA256 + ChaCha20Poly1305，
//! `info = "xseed/ohttp/v1" ‖ target_node_id`；请求 = `enc ‖ ct`。
//! 响应用 `export("xseed/ohttp/resp", 32)` 作 ChaCha20Poly1305 密钥，随机 12 字节 nonce 置于密文前。
//! 明文先填充为 512 字节的倍数：`u32be(len) ‖ 明文 ‖ 0…`。

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::ChaCha20Poly1305 as Chacha;
use hpke::aead::ChaCha20Poly1305;
use hpke::kdf::HkdfSha256;
use hpke::kem::X25519HkdfSha256;
use hpke::{Deserializable, Kem, OpModeR, OpModeS, Serializable};

const ENC_LEN: usize = 32;
const PAD: usize = 512;

#[derive(Debug, thiserror::Error)]
#[error("两跳封装错误：{0}")]
pub struct Error(&'static str);

pub fn pad(plain: &[u8]) -> Vec<u8> {
    let n = 4 + plain.len();
    let total = n.div_ceil(PAD) * PAD;
    let mut v = Vec::with_capacity(total);
    v.extend_from_slice(&(plain.len() as u32).to_be_bytes());
    v.extend_from_slice(plain);
    v.resize(total, 0);
    v
}

pub fn unpad(p: &[u8]) -> Result<&[u8], Error> {
    let n = u32::from_be_bytes(p.get(0..4).ok_or(Error("填充"))?.try_into().unwrap()) as usize;
    p.get(4..4 + n).ok_or(Error("填充"))
}

/// 由 32 字节种子确定性地派生 HPKE 密钥对，返回 `(私钥, 公钥)`。
pub fn keypair(seed: &[u8; 32]) -> ([u8; 32], [u8; 32]) {
    let (sk, pk) = X25519HkdfSha256::derive_keypair(seed);
    (sk.to_bytes().into(), pk.to_bytes().into())
}

fn info(target: &[u8; 16]) -> Vec<u8> {
    [b"xseed/ohttp/v1".as_slice(), target].concat()
}

/// 用于解开响应的密钥（请求方与桶节点各持一份）。
pub struct RespKey([u8; 32]);

pub fn seal_request(pk: &[u8; 32], target: &[u8; 16], plain: &[u8]) -> Result<(Vec<u8>, RespKey), Error> {
    let pk = <X25519HkdfSha256 as Kem>::PublicKey::from_bytes(pk).map_err(|_| Error("公钥"))?;
    let (enc, mut ctx) = hpke::setup_sender::<ChaCha20Poly1305, HkdfSha256, X25519HkdfSha256, _>(&OpModeS::Base, &pk, &info(target), &mut rand::rngs::OsRng).map_err(|_| Error("封装"))?;
    let ct = ctx.seal(&pad(plain), b"").map_err(|_| Error("加密"))?;
    let mut k = [0u8; 32];
    ctx.export(b"xseed/ohttp/resp", &mut k).map_err(|_| Error("导出"))?;
    let mut out = enc.to_bytes().to_vec();
    out.extend_from_slice(&ct);
    Ok((out, RespKey(k)))
}

pub fn open_request(sk: &[u8; 32], self_id: &[u8; 16], req: &[u8]) -> Result<(Vec<u8>, RespKey), Error> {
    if req.len() < ENC_LEN + 16 {
        return Err(Error("长度"));
    }
    let sk = <X25519HkdfSha256 as Kem>::PrivateKey::from_bytes(sk).map_err(|_| Error("私钥"))?;
    let enc = <X25519HkdfSha256 as Kem>::EncappedKey::from_bytes(&req[..ENC_LEN]).map_err(|_| Error("enc"))?;
    let mut ctx = hpke::setup_receiver::<ChaCha20Poly1305, HkdfSha256, X25519HkdfSha256>(&OpModeR::Base, &sk, &enc, &info(self_id)).map_err(|_| Error("解封装"))?;
    let p = ctx.open(&req[ENC_LEN..], b"").map_err(|_| Error("解密"))?;
    let mut k = [0u8; 32];
    ctx.export(b"xseed/ohttp/resp", &mut k).map_err(|_| Error("导出"))?;
    Ok((unpad(&p)?.to_vec(), RespKey(k)))
}

pub fn seal_response(k: &RespKey, plain: &[u8]) -> Vec<u8> {
    let nonce: [u8; 12] = crate::random();
    let ct = Chacha::new((&k.0).into()).encrypt((&nonce).into(), pad(plain).as_slice()).expect("加密不会失败");
    [nonce.as_slice(), &ct].concat()
}

pub fn open_response(k: &RespKey, resp: &[u8]) -> Result<Vec<u8>, Error> {
    if resp.len() < 12 + 16 {
        return Err(Error("长度"));
    }
    let p = Chacha::new((&k.0).into()).decrypt(resp[..12].into(), &resp[12..]).map_err(|_| Error("解密"))?;
    Ok(unpad(&p)?.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_padding() {
        let (sk, pk) = keypair(&[9; 32]);
        let id = [1u8; 16];
        let (req, ck) = seal_request(&pk, &id, b"hello").unwrap();
        assert_eq!(req.len(), 32 + 512 + 16, "明文填充到 512 字节");
        let (plain, sk2) = open_request(&sk, &id, &req).unwrap();
        assert_eq!(plain, b"hello");
        assert!(open_request(&sk, &[2; 16], &req).is_err(), "目标节点不符时无法解开");
        let r = seal_response(&sk2, b"world");
        assert_eq!(open_response(&ck, &r).unwrap(), b"world");
    }
}
