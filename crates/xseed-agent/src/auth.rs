//! 管理员登录、会话、CSRF、失败锁定（03 §2.3）。

use argon2::password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use rand::RngCore;
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub const COOKIE: &str = "xseed_session";
const SESSION_TTL: Duration = Duration::from_secs(7 * 24 * 3600);

pub fn hash_password(p: &str) -> String {
    Argon2::default().hash_password(p.as_bytes(), &SaltString::generate(&mut OsRng)).expect("hash").to_string()
}

pub fn verify_password(p: &str, phc: &str) -> bool {
    PasswordHash::new(phc).map(|h| Argon2::default().verify_password(p.as_bytes(), &h).is_ok()).unwrap_or(false)
}

pub fn random_token(n: usize) -> String {
    let mut b = vec![0u8; n];
    rand::rngs::OsRng.fill_bytes(&mut b);
    hex::encode(b)
}

/// 便于手动输入的随机密码（去掉易混字符）。
pub fn random_password(len: usize) -> String {
    const A: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz23456789";
    let mut b = vec![0u8; len];
    rand::rngs::OsRng.fill_bytes(&mut b);
    b.iter().map(|x| A[*x as usize % A.len()] as char).collect()
}

pub struct Session {
    pub csrf: String,
    expires: Instant,
}

#[derive(Default)]
pub struct Sessions(Mutex<HashMap<String, Session>>);

impl Sessions {
    pub fn create(&self) -> (String, String) {
        let token = random_token(32);
        let csrf = random_token(16);
        let mut m = self.0.lock().unwrap();
        m.retain(|_, s| s.expires > Instant::now());
        m.insert(token.clone(), Session { csrf: csrf.clone(), expires: Instant::now() + SESSION_TTL });
        (token, csrf)
    }

    pub fn csrf(&self, token: &str) -> Option<String> {
        let m = self.0.lock().unwrap();
        m.get(token).filter(|s| s.expires > Instant::now()).map(|s| s.csrf.clone())
    }

    pub fn remove(&self, token: &str) {
        self.0.lock().unwrap().remove(token);
    }

    pub fn clear(&self) {
        self.0.lock().unwrap().clear();
    }
}

/// 按来源 IP 的失败计数：`max` 次失败后封禁 `ban`。
pub struct Lockout {
    max: u32,
    ban: Duration,
    m: Mutex<HashMap<IpAddr, (u32, Option<Instant>)>>,
}

impl Lockout {
    pub fn new(max: u32, ban: Duration) -> Self {
        Lockout { max, ban, m: Mutex::new(HashMap::new()) }
    }

    /// 是否被封禁中
    pub fn blocked(&self, ip: IpAddr) -> bool {
        let mut m = self.m.lock().unwrap();
        match m.get(&ip) {
            Some((_, Some(until))) if *until > Instant::now() => true,
            Some((_, Some(_))) => {
                m.remove(&ip);
                false
            }
            _ => false,
        }
    }

    pub fn fail(&self, ip: IpAddr) {
        let mut m = self.m.lock().unwrap();
        let e = m.entry(ip).or_insert((0, None));
        e.0 += 1;
        if e.0 >= self.max {
            e.1 = Some(Instant::now() + self.ban);
            e.0 = 0;
        }
    }

    pub fn ok(&self, ip: IpAddr) {
        self.m.lock().unwrap().remove(&ip);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_roundtrip() {
        let h = hash_password("correct horse");
        assert!(verify_password("correct horse", &h));
        assert!(!verify_password("wrong", &h));
        assert_eq!(random_password(24).len(), 24);
    }

    #[test]
    fn lockout() {
        let l = Lockout::new(3, Duration::from_secs(60));
        let ip: IpAddr = "1.2.3.4".parse().unwrap();
        l.fail(ip);
        l.fail(ip);
        assert!(!l.blocked(ip));
        l.fail(ip);
        assert!(l.blocked(ip));
    }

    #[test]
    fn sessions() {
        let s = Sessions::default();
        let (t, c) = s.create();
        assert_eq!(s.csrf(&t), Some(c));
        s.remove(&t);
        assert_eq!(s.csrf(&t), None);
    }
}
