//! 站点识别（03 §1.11）、passkey 提取（§1.12.1）与本机种子编号解析（§1.2.1）。

use std::collections::BTreeSet;
use url::Url;

/// `TrackerHost = lowercase(IDNA-ASCII(host)) ‖ (":" ‖ port，若不是默认端口)`。
pub fn normalize_tracker_host(raw: &str) -> Option<String> {
    let u = Url::parse(raw.trim()).ok()?;
    let default = match u.scheme() {
        "http" => Some(80),
        "https" => Some(443),
        "udp" => None,
        _ => return None,
    };
    let host = u.host_str()?.trim_end_matches('.');
    let host = idna::domain_to_ascii(host).ok()?.to_ascii_lowercase();
    if host.is_empty() {
        return None;
    }
    match u.port() {
        Some(p) if Some(p) != default => Some(format!("{host}:{p}")),
        _ => Some(host),
    }
}

fn bare(host: &str) -> &str {
    // 去掉端口；IPv6 字面量保持原样
    if host.starts_with('[') {
        return host;
    }
    host.rsplit_once(':').map_or(host, |(h, _)| h)
}

#[derive(Debug, Clone, Default)]
pub struct SiteDef {
    pub id: String,
    pub name: String,
    /// 站点域名（不含端口），匹配自身及其子域名。
    pub domains: Vec<String>,
    /// 精确匹配的 tracker 主机（规范化形式）。
    pub tracker_hosts: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum SiteRef {
    Site(String),
    Host(String),
}

impl SiteRef {
    pub fn key(&self) -> String {
        match self {
            SiteRef::Site(id) => format!("site:{id}"),
            SiteRef::Host(h) => format!("host:{h}"),
        }
    }
    pub fn parse_key(k: &str) -> Option<Self> {
        if let Some(id) = k.strip_prefix("site:") {
            Some(SiteRef::Site(id.into()))
        } else {
            k.strip_prefix("host:").map(|h| SiteRef::Host(h.into()))
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Identified {
    One(SiteRef),
    Ambiguous,
    None,
}

pub fn site_matches(s: &SiteDef, host: &str) -> bool {
    let b = bare(host);
    s.tracker_hosts.iter().any(|t| t == host) || s.domains.iter().any(|d| b == d || b.ends_with(&format!(".{d}")))
}

/// 03 §1.11.2。`public` 为公共 tracker 主机集合。
pub fn identify(tracker_urls: &[String], sites: &[SiteDef], public: &BTreeSet<String>) -> Identified {
    let hosts: BTreeSet<String> = tracker_urls.iter().filter_map(|u| normalize_tracker_host(u)).filter(|h| !public.contains(h)).collect();
    let matched: BTreeSet<&str> = sites.iter().filter(|s| hosts.iter().any(|h| site_matches(s, h))).map(|s| s.id.as_str()).collect();
    match matched.len() {
        1 => Identified::One(SiteRef::Site(matched.into_iter().next().unwrap().to_string())),
        0 => hosts.into_iter().next().map_or(Identified::None, |h| Identified::One(SiteRef::Host(h))),
        _ => Identified::Ambiguous,
    }
}

/// 从 announce 地址中提取 passkey：查询参数 `passkey` / `authkey` / `torrent_pass`，或路径中 32 位十六进制段。
pub fn extract_passkey(announce: &str) -> Option<String> {
    let u = Url::parse(announce.trim()).ok()?;
    for (k, v) in u.query_pairs() {
        if matches!(k.as_ref(), "passkey" | "authkey" | "torrent_pass") && !v.is_empty() {
            return Some(v.into_owned());
        }
    }
    u.path_segments()?.find(|s| s.len() == 32 && s.bytes().all(|b| b.is_ascii_hexdigit())).map(str::to_string)
}

/// 从种子 comment / source 中属于该站域名的 URL 提取本机种子编号。
/// 默认规则依次匹配查询参数 `id`、`torrentid`、`tid`，以及路径 `/torrents/<数字>`。
pub fn torrent_id_from_text(text: &str, site: &SiteDef) -> Option<String> {
    for word in text.split(|c: char| c.is_whitespace() || c == '"' || c == '\'' || c == '<' || c == '>') {
        if !(word.starts_with("http://") || word.starts_with("https://")) {
            continue;
        }
        let Ok(u) = Url::parse(word) else { continue };
        let Some(h) = u.host_str().map(str::to_ascii_lowercase) else { continue };
        let host = match u.port() {
            Some(p) => format!("{h}:{p}"),
            None => h,
        };
        if !site_matches(site, &host) {
            continue;
        }
        for key in ["id", "torrentid", "tid"] {
            if let Some((_, v)) = u.query_pairs().find(|(k, _)| k == key) {
                if !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()) {
                    return Some(v.into_owned());
                }
            }
        }
        let segs: Vec<&str> = u.path_segments().map(|s| s.collect()).unwrap_or_default();
        if let Some(i) = segs.iter().position(|s| *s == "torrents") {
            if let Some(n) = segs.get(i + 1).filter(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())) {
                return Some(n.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn site(id: &str, domains: &[&str]) -> SiteDef {
        SiteDef { id: id.into(), name: id.into(), domains: domains.iter().map(|s| s.to_string()).collect(), tracker_hosts: vec![] }
    }

    #[test]
    fn normalize() {
        assert_eq!(normalize_tracker_host("HTTPS://Tracker.Example.org:443/announce").as_deref(), Some("tracker.example.org"));
        assert_eq!(normalize_tracker_host("http://t.example:8080/a").as_deref(), Some("t.example:8080"));
        assert_eq!(normalize_tracker_host("udp://open.example:6969").as_deref(), Some("open.example:6969"));
        assert_eq!(normalize_tracker_host("http://例子.测试/a").as_deref(), Some("xn--fsqu00a.xn--0zwm56d"));
        assert_eq!(normalize_tracker_host("wss://x.example"), None);
    }

    #[test]
    fn identify_cases() {
        let sites = vec![site("y", &["site-y.example"]), site("z", &["site-z.example"])];
        let public: BTreeSet<String> = ["open.example:6969".to_string()].into();
        let one = |u: &[&str]| identify(&u.iter().map(|s| s.to_string()).collect::<Vec<_>>(), &sites, &public);
        assert_eq!(one(&["https://tracker.site-y.example/announce", "udp://open.example:6969"]), Identified::One(SiteRef::Site("y".into())));
        assert_eq!(one(&["https://tracker.site-y.example/a", "https://site-z.example/a"]), Identified::Ambiguous);
        assert_eq!(one(&["https://t.unknown.example/a"]), Identified::One(SiteRef::Host("t.unknown.example".into())));
        assert_eq!(one(&["udp://open.example:6969"]), Identified::None);
        assert!(!site_matches(&sites[0], "evilsite-y.example"));
    }

    #[test]
    fn passkey() {
        assert_eq!(extract_passkey("https://t.example/announce.php?passkey=abc123").as_deref(), Some("abc123"));
        assert_eq!(extract_passkey("https://t.example/0123456789abcdef0123456789abcdef/announce").as_deref(), Some("0123456789abcdef0123456789abcdef"));
        assert_eq!(extract_passkey("https://t.example/announce"), None);
    }

    #[test]
    fn torrent_id() {
        let s = site("y", &["site-y.example"]);
        assert_eq!(torrent_id_from_text("https://site-y.example/details.php?id=48211&hit=1", &s).as_deref(), Some("48211"));
        assert_eq!(torrent_id_from_text("see https://www.site-y.example/torrents/77 thanks", &s).as_deref(), Some("77"));
        assert_eq!(torrent_id_from_text("https://other.example/details.php?id=1", &s), None);
        assert_eq!(SiteRef::parse_key(&SiteRef::Host("a:1".into()).key()), Some(SiteRef::Host("a:1".into())));
    }
}
