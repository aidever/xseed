//! 固定窗口计数限流（03 §2.1 限流与配额）。

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::Mutex;

pub struct Limiter<K> {
    limit: u32,
    window: u64,
    m: Mutex<HashMap<K, (u64, u32)>>,
}

impl<K: Hash + Eq + Clone> Limiter<K> {
    pub fn new(limit: u32, window_secs: u64) -> Self {
        Limiter { limit, window: window_secs, m: Mutex::new(HashMap::new()) }
    }

    /// 计数一次；超过上限返回 false。
    pub fn hit(&self, k: &K, now: u64) -> bool {
        self.hit_n(k, 1, now)
    }

    /// 一次计数 n 次。
    pub fn hit_n(&self, k: &K, n: u32, now: u64) -> bool {
        let w = now / self.window;
        let mut m = self.m.lock().unwrap();
        let e = m.entry(k.clone()).or_insert((w, 0));
        if e.0 != w {
            *e = (w, 0);
        }
        e.1 = e.1.saturating_add(n);
        e.1 <= self.limit
    }

    /// 清理过期窗口。
    pub fn sweep(&self, now: u64) {
        let w = now / self.window;
        self.m.lock().unwrap().retain(|_, v| v.0 == w);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn window() {
        let l = super::Limiter::new(2, 60);
        assert!(l.hit(&1, 0) && l.hit(&1, 1));
        assert!(!l.hit(&1, 2));
        assert!(l.hit(&2, 2));
        assert!(l.hit(&1, 60));
    }
}
