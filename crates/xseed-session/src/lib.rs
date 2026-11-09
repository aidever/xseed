//! 会话（03 §1.5.2、§1.6、§2.2）：不含 IO，只负责握手、加解密与消息编解码。
//!
//! - 模式 `Noise_XXpsk3_25519_ChaChaPoly_BLAKE2s`，查询方为发起方；
//! - `psk = HKDF(SFP, "xseed/psk/v1")`：不持有同一文件结构的人无法完成握手；
//! - `prologue = "xseed/v2" ‖ sid`；静态密钥每个会话重新生成；
//! - `K_cb = HKDF(握手 hash, "xseed/cb/v1")`，答题 MAC 用它绑定到本次会话。

use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

const PATTERN: &str = "Noise_XXpsk3_25519_ChaChaPoly_BLAKE2s";
/// 单条应用消息上限。
pub const MAX_MSG: usize = 4096;
const PAD: usize = 256;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("握手失败")]
    Handshake,
    #[error("解密失败")]
    Decrypt,
    #[error("消息格式错误")]
    Format,
    #[error("状态错误")]
    State,
}

/// 会话消息（03 §2.2）。`site` 为 SiteRef 的规范字符串（`site:<id>` / `host:<host>`）。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Msg {
    Ask { site: String },
    NotMember,
    Chal { torrent_id: String },
    Answer {
        #[serde(with = "serde_bytes")]
        mac: [u8; 32],
    },
    Offer {
        torrent_id: String,
        #[serde(with = "serde_bytes", default, skip_serializing_if = "Option::is_none")]
        ih1: Option<Vec<u8>>,
        #[serde(with = "serde_bytes", default, skip_serializing_if = "Option::is_none")]
        ih2: Option<Vec<u8>>,
        size: u64,
    },
    None,
    Error { code: u16, #[serde(default, skip_serializing_if = "Option::is_none")] msg: Option<String> },
}

/// 会话错误码（03 §9.2）。
pub mod code {
    pub const VERSION: u16 = 1;
    pub const TIMEOUT: u16 = 2;
    pub const BAD_ANSWER: u16 = 21;
    pub const CHAL_UNAVAILABLE: u16 = 22;
    pub const QUOTA: u16 = 40;
    pub const INTERNAL: u16 = 99;
}

fn hkdf(ikm: &[u8], info: &[u8]) -> [u8; 32] {
    let mut k = [0u8; 32];
    Hkdf::<Sha256>::new(None, ikm).expand(info, &mut k).expect("长度合法");
    k
}

fn prologue(sid: &[u8; 16]) -> Vec<u8> {
    [b"xseed/v2".as_slice(), sid].concat()
}

fn builder<'a>(psk: &'a [u8; 32], prologue: &'a [u8]) -> snow::Builder<'a> {
    snow::Builder::new(PATTERN.parse().expect("模式合法")).psk(3, psk).prologue(prologue)
}

/// 一次会话。
pub struct Session {
    hs: Option<snow::HandshakeState>,
    ts: Option<snow::TransportState>,
    cb: Option<[u8; 32]>,
    initiator: bool,
}

impl Session {
    fn new(sfp: &[u8; 32], sid: &[u8; 16], initiator: bool) -> Result<Self, Error> {
        let psk = hkdf(sfp, b"xseed/psk/v1");
        let pro = prologue(sid);
        let b = builder(&psk, &pro);
        let key = b.generate_keypair().map_err(|_| Error::Handshake)?;
        let b = builder(&psk, &pro).local_private_key(&key.private);
        let hs = if initiator { b.build_initiator() } else { b.build_responder() }.map_err(|_| Error::Handshake)?;
        Ok(Session { hs: Some(hs), ts: None, cb: None, initiator })
    }

    /// 查询方：创建会话并产生握手第 1 条消息。
    pub fn initiate(sfp: &[u8; 32], sid: &[u8; 16]) -> Result<(Self, Vec<u8>), Error> {
        let mut s = Self::new(sfp, sid, true)?;
        let m = s.write_hs()?;
        Ok((s, m))
    }

    /// 持有者：读入第 1 条消息，产生第 2 条。
    pub fn respond(sfp: &[u8; 32], sid: &[u8; 16], msg1: &[u8]) -> Result<(Self, Vec<u8>), Error> {
        let mut s = Self::new(sfp, sid, false)?;
        s.read_hs(msg1)?;
        let m = s.write_hs()?;
        Ok((s, m))
    }

    /// 查询方：读入第 2 条消息，产生第 3 条，握手完成。
    pub fn initiator_finish(&mut self, msg2: &[u8]) -> Result<Vec<u8>, Error> {
        if !self.initiator {
            return Err(Error::State);
        }
        self.read_hs(msg2)?;
        let m = self.write_hs()?;
        self.enter_transport()?;
        Ok(m)
    }

    /// 持有者：读入第 3 条消息，握手完成。psk 不一致时在这里失败。
    pub fn responder_finish(&mut self, msg3: &[u8]) -> Result<(), Error> {
        if self.initiator {
            return Err(Error::State);
        }
        self.read_hs(msg3)?;
        self.enter_transport()
    }

    fn write_hs(&mut self) -> Result<Vec<u8>, Error> {
        let hs = self.hs.as_mut().ok_or(Error::State)?;
        let mut buf = vec![0u8; 1024];
        let n = hs.write_message(&[], &mut buf).map_err(|_| Error::Handshake)?;
        buf.truncate(n);
        Ok(buf)
    }

    fn read_hs(&mut self, m: &[u8]) -> Result<(), Error> {
        let hs = self.hs.as_mut().ok_or(Error::State)?;
        let mut buf = vec![0u8; 1024];
        hs.read_message(m, &mut buf).map_err(|_| Error::Handshake)?;
        Ok(())
    }

    fn enter_transport(&mut self) -> Result<(), Error> {
        let hs = self.hs.take().ok_or(Error::State)?;
        if !hs.is_handshake_finished() {
            return Err(Error::Handshake);
        }
        self.cb = Some(hkdf(hs.get_handshake_hash(), b"xseed/cb/v1"));
        self.ts = Some(hs.into_transport_mode().map_err(|_| Error::Handshake)?);
        Ok(())
    }

    pub fn established(&self) -> bool {
        self.ts.is_some()
    }

    /// 通道绑定密钥 `K_cb`。
    pub fn binding(&self) -> Option<[u8; 32]> {
        self.cb
    }

    /// 加密一条消息：`u16be(len) ‖ cbor ‖ 0…` 填充到 256 字节的倍数后再加密。
    pub fn seal(&mut self, m: &Msg) -> Result<Vec<u8>, Error> {
        let ts = self.ts.as_mut().ok_or(Error::State)?;
        let mut body = Vec::new();
        ciborium::into_writer(m, &mut body).map_err(|_| Error::Format)?;
        if body.len() > MAX_MSG {
            return Err(Error::Format);
        }
        let mut plain = (body.len() as u16).to_be_bytes().to_vec();
        plain.extend_from_slice(&body);
        plain.resize(plain.len().div_ceil(PAD) * PAD, 0);
        let mut out = vec![0u8; plain.len() + 16];
        let n = ts.write_message(&plain, &mut out).map_err(|_| Error::Format)?;
        out.truncate(n);
        Ok(out)
    }

    pub fn open(&mut self, ct: &[u8]) -> Result<Msg, Error> {
        let ts = self.ts.as_mut().ok_or(Error::State)?;
        if ct.len() > MAX_MSG + PAD + 16 {
            return Err(Error::Format);
        }
        let mut plain = vec![0u8; ct.len()];
        let n = ts.read_message(ct, &mut plain).map_err(|_| Error::Decrypt)?;
        let len = u16::from_be_bytes(plain.get(0..2).ok_or(Error::Format)?.try_into().unwrap()) as usize;
        let body = plain.get(2..2 + len).filter(|_| 2 + len <= n).ok_or(Error::Format)?;
        ciborium::from_reader(body).map_err(|_| Error::Format)
    }
}

/// 答题 MAC：`HMAC(K_cb, "xseed/ans/v1" ‖ site ‖ 0x00 ‖ torrent_id ‖ 0x00 ‖ ih)`（03 §1.6.1）。
pub fn answer_mac(cb: &[u8; 32], site: &str, torrent_id: &str, ih: &[u8]) -> [u8; 32] {
    let mut m = <Hmac<Sha256> as Mac>::new_from_slice(cb).expect("任意长度");
    for p in [b"xseed/ans/v1".as_slice(), site.as_bytes(), &[0], torrent_id.as_bytes(), &[0], ih] {
        m.update(p);
    }
    m.finalize().into_bytes().into()
}

pub fn check_answer(cb: &[u8; 32], site: &str, torrent_id: &str, ih: &[u8], mac: &[u8; 32]) -> bool {
    use subtle::ConstantTimeEq;
    bool::from(answer_mac(cb, site, torrent_id, ih).ct_eq(mac))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handshake(sfp_a: [u8; 32], sfp_b: [u8; 32]) -> Result<(Session, Session), Error> {
        let sid = [7u8; 16];
        let (mut b, m1) = Session::initiate(&sfp_b, &sid)?;
        let (mut a, m2) = Session::respond(&sfp_a, &sid, &m1)?;
        let m3 = b.initiator_finish(&m2)?;
        a.responder_finish(&m3)?;
        Ok((a, b))
    }

    #[test]
    fn all_messages_round_trip_padded() {
        let (mut a, mut b) = handshake([1; 32], [1; 32]).unwrap();
        let msgs = [
            Msg::Ask { site: "host:t.example.org".into() },
            Msg::NotMember,
            Msg::Chal { torrent_id: "123456".into() },
            Msg::Answer { mac: [9; 32] },
            Msg::Offer { torrent_id: "42".into(), ih1: Some(vec![1; 20]), ih2: None, size: 1 << 40 },
            Msg::None,
            Msg::Error { code: code::QUOTA, msg: None },
        ];
        for m in msgs {
            let ct = a.seal(&m).unwrap();
            assert_eq!(ct.len() % 256, 16, "填充到 256 的倍数（另加 16 字节标签）");
            assert_eq!(b.open(&ct).unwrap(), m);
        }
    }

    #[test]
    fn answer_fields_are_separated() {
        let cb = [3u8; 32];
        // 站点与编号拼接相同但切分不同，答案必须不同
        assert_ne!(answer_mac(&cb, "site:a", "12", &[0; 20]), answer_mac(&cb, "site:a1", "2", &[0; 20]));
        let mac = answer_mac(&cb, "site:a", "12", &[5; 20]);
        assert!(check_answer(&cb, "site:a", "12", &[5; 20], &mac));
        assert!(!check_answer(&cb, "site:a", "12", &[6; 20], &mac));
    }

    #[test]
    fn full_exchange() {
        let (mut a, mut b) = handshake([1; 32], [1; 32]).unwrap();
        assert_eq!(a.binding(), b.binding());
        let ask = Msg::Ask { site: "site:y".into() };
        let ct = b.seal(&ask).unwrap();
        assert_eq!(ct.len() % PAD, 16, "填充到 256 字节倍数");
        assert_eq!(a.open(&ct).unwrap(), ask);
        let offer = Msg::Offer { torrent_id: "123".into(), ih1: Some(vec![9; 20]), ih2: None, size: 42 };
        assert_eq!(b.open(&a.seal(&offer).unwrap()).unwrap(), offer);
        let cb = a.binding().unwrap();
        let mac = answer_mac(&cb, "site:y", "5", &[1; 20]);
        assert!(check_answer(&b.binding().unwrap(), "site:y", "5", &[1; 20], &mac));
        assert!(!check_answer(&cb, "site:y", "5", &[2; 20], &mac));
        assert!(!check_answer(&cb, "site:z", "5", &[1; 20], &mac));
    }

    #[test]
    fn different_content_cannot_handshake() {
        assert!(handshake([1; 32], [2; 32]).is_err());
    }

    #[test]
    fn relayed_answer_fails_under_other_binding() {
        // 中间人 M 与 A、C 分别握手：两侧绑定密钥不同，C 的答案不能转给 A
        let (a_side, _m1) = handshake([1; 32], [1; 32]).unwrap();
        let (c_side, _m2) = handshake([1; 32], [1; 32]).unwrap();
        let mac_from_c = answer_mac(&c_side.binding().unwrap(), "site:y", "5", &[1; 20]);
        assert!(!check_answer(&a_side.binding().unwrap(), "site:y", "5", &[1; 20], &mac_from_c));
    }
}
