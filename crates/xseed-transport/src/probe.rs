//! 探测标签（03 §1.5.1）：只有持有同一文件结构指纹（SFP）的人才能算出。

use hkdf::Hkdf;
use sha2::Sha256;

fn key(sfp: &[u8; 32], info: &[u8]) -> [u8; 32] {
    let mut k = [0u8; 32];
    Hkdf::<Sha256>::new(None, sfp).expand(info, &mut k).expect("长度合法");
    k
}

/// `tag = HMAC(HKDF(SFP, "xseed/probe/v1"), nonce ‖ to.alias ‖ from.alias)[0..16]`
pub fn tag(sfp: &[u8; 32], nonce: &[u8; 16], to: &[u8; 16], from: &[u8; 16]) -> [u8; 16] {
    let h = crate::auth::hmac(&key(sfp, b"xseed/probe/v1"), &[nonce, to, from]);
    h[..16].try_into().unwrap()
}

pub fn verify(sfp: &[u8; 32], nonce: &[u8; 16], to: &[u8; 16], from: &[u8; 16], got: &[u8; 16]) -> bool {
    crate::auth::ct_eq(&tag(sfp, nonce, to, from), got)
}

#[cfg(test)]
mod tests {
    #[test]
    fn tag_binds_everything() {
        let t = super::tag(&[1; 32], &[2; 16], &[3; 16], &[4; 16]);
        assert!(super::verify(&[1; 32], &[2; 16], &[3; 16], &[4; 16], &t));
        assert!(!super::verify(&[9; 32], &[2; 16], &[3; 16], &[4; 16], &t));
        assert!(!super::verify(&[1; 32], &[2; 16], &[3; 16], &[5; 16], &t));
    }
}
