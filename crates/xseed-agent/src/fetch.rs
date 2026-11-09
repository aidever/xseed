//! 从站点下载种子文件（03 §1.11.3）。用于出题、答题与加种。
//!
//! - 凭证只发往该站已知域名：站点定义的 `domains`，或 Host 身份下的 tracker 主机及其可注册域名；
//! - 不跟随重定向（避免把凭证带到其他域名）；
//! - 按站限速（默认每分钟 2 次），成功的下载地址模板记入 `site_traits.download`。

use crate::app::Shared;
use rusqlite::params;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;
use tokio::time::Instant;
use xseed_core::site::SiteRef;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum FetchError {
    #[error("没有该站的登录信息")]
    NoCredential,
    #[error("站点拒绝访问（登录失效或无权限）")]
    Denied,
    #[error("种子不存在")]
    NotFound,
    #[error("站点限速")]
    RateLimited,
    #[error("站点无法访问：{0}")]
    Network(String),
    #[error("返回的不是种子文件")]
    NotTorrent,
}

#[derive(Default)]
pub struct Fetcher {
    next: Mutex<HashMap<String, Instant>>,
}

/// 用途，用于统计（03 §8 `xseed_site_downloads_total{purpose}`）。
#[derive(Clone, Copy, Debug)]
pub enum Purpose {
    Pool,
    Challenge,
    #[allow(dead_code)] // M4 加种时使用
    Target,
}

impl Fetcher {
    /// 预约一个下载时段；需要等待超过 `max_wait` 时返回 None。
    fn reserve(&self, site: &str, per_min: u32, max_wait: Duration) -> Option<Instant> {
        let gap = Duration::from_millis(60_000 / per_min.max(1) as u64);
        let now = Instant::now();
        let mut m = self.next.lock().unwrap();
        let at = m.get(site).copied().filter(|t| *t > now).unwrap_or(now);
        if at - now > max_wait {
            return None;
        }
        m.insert(site.to_string(), at + gap);
        Some(at)
    }
}

fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(30))
        .connect_timeout(Duration::from_secs(10))
        .user_agent(concat!("Mozilla/5.0 (compatible; xseed/", env!("CARGO_PKG_VERSION"), ")"))
        .build()
        .expect("HTTP 客户端")
}

/// 可注册域名的简化判断：取最后两段（`tracker.example.org` → `example.org`）。IP 与带端口的主机原样返回。
fn registrable(host: &str) -> String {
    let (h, port) = match host.rsplit_once(':') {
        Some((h, p)) if p.chars().all(|c| c.is_ascii_digit()) => (h, Some(p)),
        _ => (host, None),
    };
    if h.parse::<std::net::IpAddr>().is_ok() || port.is_some() {
        return host.to_string();
    }
    let parts: Vec<&str> = h.split('.').collect();
    if parts.len() <= 2 {
        h.to_string()
    } else {
        parts[parts.len() - 2..].join(".")
    }
}

/// 该站可以携带凭证访问的域名（按优先级）。
pub fn domains(app: &Shared, site_ref: &str) -> Vec<String> {
    match SiteRef::parse_key(site_ref) {
        Some(SiteRef::Site(id)) => app
            .db
            .lock()
            .query_row("SELECT domains FROM site_defs WHERE id=?1", [&id], |r| r.get::<_, String>(0))
            .ok()
            .and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok())
            .unwrap_or_default()
            .into_iter()
            .filter(|d| !d.starts_with("www."))
            .collect(),
        Some(SiteRef::Host(h)) => {
            let r = registrable(&h);
            if r == h {
                vec![h]
            } else {
                vec![r, h]
            }
        }
        None => vec![],
    }
}

fn cred(app: &Shared, site_ref: &str, kind: &str) -> Option<String> {
    let sources: &[&str] = if kind == "cookie" { &["manual", "ptd"] } else { &["manual", "tracker"] };
    sources.iter().find_map(|s| app.vault.credential(site_ref, kind, s)).filter(|v| !v.trim().is_empty())
}

pub fn has_credential(app: &Shared, site_ref: &str) -> bool {
    cred(app, site_ref, "cookie").is_some() || cred(app, site_ref, "passkey").is_some()
}

fn trait_get(app: &Shared, site_ref: &str, name: &str) -> Option<String> {
    app.db
        .lock()
        .query_row("SELECT value FROM site_traits WHERE site_ref=?1 AND trait=?2 AND expires_at > ?3", params![site_ref, name, crate::db::now()], |r| r.get(0))
        .ok()
}

fn trait_set(app: &Shared, site_ref: &str, name: &str, value: &str) {
    let t = crate::db::now();
    let _ = app.db.lock().execute(
        "INSERT OR REPLACE INTO site_traits(site_ref, trait, value, learned_at, expires_at) VALUES(?1, ?2, ?3, ?4, ?5)",
        params![site_ref, name, value, t, t + 90 * 86_400],
    );
}

/// 候选下载地址：（模板，是否带 Cookie）。模板中的 `{id}` 在请求时替换。
fn candidates(app: &Shared, site_ref: &str) -> Vec<(String, bool)> {
    let cookie = cred(app, site_ref, "cookie").is_some();
    let passkey = cred(app, site_ref, "passkey");
    let mut schemes = vec!["https"];
    if trait_get(app, site_ref, "tracker_scheme").as_deref() == Some("http") {
        schemes.push("http");
    }
    let mut out = Vec::new();
    if let Some(t) = trait_get(app, site_ref, "download") {
        out.push((t, cookie));
    }
    for d in domains(app, site_ref) {
        for s in &schemes {
            if let Some(pk) = &passkey {
                out.push((format!("{s}://{d}/download.php?id={{id}}&passkey={pk}"), false));
            }
            if cookie {
                out.push((format!("{s}://{d}/download.php?id={{id}}"), true));
            }
        }
    }
    let mut seen = std::collections::HashSet::new();
    out.retain(|c| seen.insert(c.clone()));
    out
}

/// 下载一个种子文件。`max_wait` 为限速排队的最长等待。
pub async fn download(app: &Shared, site_ref: &str, torrent_id: &str, purpose: Purpose, max_wait: Duration) -> Result<Vec<u8>, FetchError> {
    if !torrent_id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') || torrent_id.is_empty() || torrent_id.len() > 32 {
        return Err(FetchError::NotFound);
    }
    let cands = candidates(app, site_ref);
    if cands.is_empty() {
        return Err(FetchError::NoCredential);
    }
    let per_min = app.cfg().net.site_rate_per_min;
    let cookie = cred(app, site_ref, "cookie");
    let client = http();
    let mut last = FetchError::NotFound;
    for (tpl, with_cookie) in cands {
        let at = app.fetch.reserve(site_ref, per_min, max_wait).ok_or(FetchError::RateLimited)?;
        tokio::time::sleep_until(at).await;
        let url = tpl.replace("{id}", torrent_id);
        let mut req = client.get(&url);
        if with_cookie {
            if let Some(c) = &cookie {
                req = req.header("cookie", c);
            }
        }
        tracing::debug!("下载种子 {site_ref} #{torrent_id}（{purpose:?}）");
        let r = match req.send().await {
            Ok(r) => r,
            Err(e) => {
                last = FetchError::Network(if e.is_connect() { "连接失败".into() } else if e.is_timeout() { "超时".into() } else { "请求失败".into() });
                continue;
            }
        };
        let status = r.status().as_u16();
        match status {
            200 => {
                let body = r.bytes().await.map_err(|_| FetchError::Network("读取失败".into()))?;
                if body.len() > 16 * 1024 * 1024 {
                    return Err(FetchError::NotTorrent);
                }
                if xseed_core::torrent::parse(&body).is_ok() {
                    trait_set(app, site_ref, "download", &tpl);
                    return Ok(body.to_vec());
                }
                // NexusPHP 对不存在的编号常返回 200 + 提示页
                last = FetchError::NotTorrent;
            }
            404 => last = FetchError::NotFound,
            401 | 403 => last = FetchError::Denied,
            429 => return Err(FetchError::RateLimited),
            _ => last = FetchError::Network(format!("HTTP {status}")),
        }
    }
    Err(last)
}

/// 种子文件的答题用 info_hash：v1 取 20 字节，纯 v2 取 32 字节（03 §1.6.3）。
pub fn challenge_ih(torrent: &[u8]) -> Option<Vec<u8>> {
    let t = xseed_core::torrent::parse(torrent).ok()?;
    t.ih1.map(|h| h.to_vec()).or(t.ih2.map(|h| h.to_vec()))
}

#[cfg(test)]
mod tests {
    #[test]
    fn registrable_domain() {
        assert_eq!(super::registrable("tracker.example.org"), "example.org");
        assert_eq!(super::registrable("example.org"), "example.org");
        assert_eq!(super::registrable("127.0.0.1:18091"), "127.0.0.1:18091");
        assert_eq!(super::registrable("t.example.org:8443"), "t.example.org:8443");
    }
}
