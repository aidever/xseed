//! 结构指纹、位置键与桶（03 §1.1.2）。

use crate::torrent::Entries;
use sha2::{Digest, Sha256};

pub type Sfp = [u8; 32];

pub fn sfp(e: &Entries) -> Sfp {
    let mut h = Sha256::new();
    h.update(b"xseed/sfp/v1");
    h.update([0u8]);
    h.update((e.files.len() as u32).to_be_bytes());
    for (p, len) in &e.files {
        h.update((p.len() as u32).to_be_bytes());
        h.update(p.as_bytes());
        h.update(len.to_be_bytes());
    }
    h.finalize().into()
}

pub fn loc(sfp: &Sfp) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(b"xseed/loc/v1");
    h.update(sfp);
    h.finalize().into()
}

pub fn bucket(sfp: &Sfp) -> u16 {
    let l = loc(sfp);
    u16::from_be_bytes([l[0], l[1]])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(files: &[(&str, u64)]) -> Entries {
        Entries { root_name: "r".into(), single_file: false, files: files.iter().map(|(p, l)| (p.to_string(), *l)).collect() }
    }

    #[test]
    fn same_files_same_sfp_regardless_of_root() {
        let a = e(&[("a.mkv", 1), ("b/c.srt", 2)]);
        let mut b = a.clone();
        b.root_name = "different".into();
        assert_eq!(sfp(&a), sfp(&b));
        assert_ne!(sfp(&a), sfp(&e(&[("a.mkv", 1), ("b/c.srt", 3)])));
        assert_ne!(sfp(&a), sfp(&e(&[("b/c.srt", 2), ("a.mkv", 1)])));
    }

    #[test]
    fn known_vector() {
        // 固定测试向量：变更算法会导致此测试失败，必须同步修改协议版本标签。
        let s = sfp(&e(&[("a", 1)]));
        assert_eq!(hex::encode(s), "00e0e53097b2bdd10677ca5a368c293471740d45e86c079b3474e20dbd7ec472");
        assert_eq!(bucket(&s), 9104);
    }
}
