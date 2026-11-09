//! 工作量证明（03 §1.4.3）：`H("xseed/pow/v1" ‖ server_salt ‖ salt ‖ u64be(nonce))` 至少 `bits` 个前导零比特。

use sha2::{Digest, Sha256};

fn digest(server_salt: &[u8], salt: &[u8; 16], nonce: u64) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(b"xseed/pow/v1");
    h.update(server_salt);
    h.update(salt);
    h.update(nonce.to_be_bytes());
    h.finalize().into()
}

fn leading_zero_bits(d: &[u8]) -> u32 {
    let mut n = 0;
    for &b in d {
        if b == 0 {
            n += 8;
        } else {
            return n + b.leading_zeros();
        }
    }
    n
}

pub fn verify(server_salt: &[u8], salt: &[u8; 16], nonce: u64, bits: u32) -> bool {
    leading_zero_bits(&digest(server_salt, salt, nonce)) >= bits
}

pub fn solve(server_salt: &[u8], salt: &[u8; 16], bits: u32) -> u64 {
    (0u64..).find(|&n| verify(server_salt, salt, n, bits)).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn solve_and_verify() {
        let salt = [7u8; 16];
        let n = solve(b"srv", &salt, 12);
        assert!(verify(b"srv", &salt, n, 12));
        assert_eq!(leading_zero_bits(&[0, 0b0001_0000]), 11);
    }
}
