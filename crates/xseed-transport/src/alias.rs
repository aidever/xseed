//! 代号（03 §1.4.1）：`AES-128-Enc(K_alias, u32be(slot) ‖ u16be(gen) ‖ u16be(week) ‖ rand(4) ‖ 0x00000000)`。

use aes::cipher::{BlockDecrypt, BlockEncrypt, KeyInit};
use aes::Aes128;

/// 代号的有效期：签发后 3 周内。
pub const MAX_AGE_WEEKS: u16 = 3;

pub struct AliasKey(Aes128);

impl AliasKey {
    pub fn new(key: &[u8; 16]) -> Self {
        AliasKey(Aes128::new(key.into()))
    }

    pub fn issue(&self, slot: u32, gen: u16, week: u16) -> [u8; 16] {
        let mut b = [0u8; 16];
        b[0..4].copy_from_slice(&slot.to_be_bytes());
        b[4..6].copy_from_slice(&gen.to_be_bytes());
        b[6..8].copy_from_slice(&week.to_be_bytes());
        b[8..12].copy_from_slice(&crate::random::<4>());
        let mut blk = b.into();
        self.0.encrypt_block(&mut blk);
        blk.into()
    }

    /// 解出 `(slot, gen, week)`；末 4 字节不为 0 即视为伪造。
    pub fn open(&self, alias: &[u8; 16]) -> Option<(u32, u16, u16)> {
        let mut blk = (*alias).into();
        self.0.decrypt_block(&mut blk);
        let b: [u8; 16] = blk.into();
        if b[12..16] != [0; 4] {
            return None;
        }
        Some((u32::from_be_bytes(b[0..4].try_into().unwrap()), u16::from_be_bytes([b[4], b[5]]), u16::from_be_bytes([b[6], b[7]])))
    }

    /// 解出并检查有效期。
    pub fn open_fresh(&self, alias: &[u8; 16], cur_week: u16) -> Option<(u32, u16)> {
        let (slot, gen, w) = self.open(alias)?;
        (w <= cur_week && cur_week - w <= MAX_AGE_WEEKS).then_some((slot, gen))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issue_open() {
        let k = AliasKey::new(&[7; 16]);
        let a = k.issue(12345, 3, 2900);
        assert_eq!(k.open(&a), Some((12345, 3, 2900)));
        assert_ne!(a, k.issue(12345, 3, 2900), "同一槽位每次签发的代号不同");
        assert_eq!(k.open_fresh(&a, 2903), Some((12345, 3)));
        assert_eq!(k.open_fresh(&a, 2904), None);
        assert_eq!(AliasKey::new(&[8; 16]).open(&a), None);
    }
}
