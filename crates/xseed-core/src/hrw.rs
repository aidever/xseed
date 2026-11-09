//! 加权 HRW 选择桶的所有者节点（03 §1.3）。

use sha2::{Digest, Sha256};

#[derive(Debug, Clone)]
pub struct NodeInfo {
    pub id: [u8; 32],
    /// 有效权重 = 清单权重 × 上线爬坡系数，必须 > 0。
    pub weight: f64,
}

fn score(n: &NodeInfo, bucket: u16) -> f64 {
    let mut h = Sha256::new();
    h.update(b"xseed/hrw/v1");
    h.update(n.id);
    h.update(bucket.to_be_bytes());
    let d = h.finalize();
    let x = u64::from_be_bytes(d[0..8].try_into().unwrap());
    let u = (x as f64 + 0.5) / 18446744073709551616.0;
    -u.ln() / n.weight
}

/// 返回分数最小的 `min(r, nodes.len())` 个节点（下标）。
pub fn owners(bucket: u16, nodes: &[NodeInfo], r: usize) -> Vec<usize> {
    let mut s: Vec<(f64, usize)> = nodes.iter().enumerate().filter(|(_, n)| n.weight > 0.0).map(|(i, n)| (score(n, bucket), i)).collect();
    s.sort_by(|a, b| a.0.total_cmp(&b.0));
    s.into_iter().take(r).map(|x| x.1).collect()
}

/// 上线爬坡：14 天内从 0.1 线性升到 1.0。
pub fn ramp(age_days: f64) -> f64 {
    (0.1 + 0.9 * (age_days / 14.0)).clamp(0.1, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nodes(n: usize) -> Vec<NodeInfo> {
        (0..n).map(|i| NodeInfo { id: [i as u8; 32], weight: 1.0 }).collect()
    }

    #[test]
    fn stable_and_distinct() {
        let ns = nodes(5);
        let a = owners(123, &ns, 2);
        assert_eq!(a.len(), 2);
        assert_ne!(a[0], a[1]);
        assert_eq!(a, owners(123, &ns, 2));
        assert_eq!(owners(1, &ns[..1], 2).len(), 1);
    }

    #[test]
    fn removing_a_node_only_moves_its_buckets() {
        let ns = nodes(6);
        let fewer: Vec<NodeInfo> = ns[..5].to_vec();
        let mut moved = 0;
        for b in 0..2000u16 {
            let before = owners(b, &ns, 1)[0];
            let after = owners(b, &fewer, 1)[0];
            if before != 5 {
                assert_eq!(before, after);
            } else {
                moved += 1;
            }
        }
        assert!(moved > 200 && moved < 500, "moved={moved}");
    }

    #[test]
    fn weight_shifts_share() {
        let mut ns = nodes(2);
        ns[0].weight = 3.0;
        let first = (0..4000u16).filter(|&b| owners(b, &ns, 1)[0] == 0).count();
        assert!(first > 2700 && first < 3300, "first={first}");
        assert!((ramp(0.0) - 0.1).abs() < 1e-9 && ramp(30.0) == 1.0);
    }
}
