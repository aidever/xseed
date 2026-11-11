//! 站点定义、识别与站点列表汇总（02 §3.2）。

use crate::db::{now, Db};
use crate::vault::Vault;
use anyhow::Result;
use rusqlite::params;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use xseed_core::site::{SiteDef, SiteRef};

/// 公共 tracker（不能作为站点身份）：内置列表 + 注册中心下发的列表。
pub fn public_trackers(db: &Db) -> BTreeSet<String> {
    let mut s = builtin_public_trackers();
    if let Some(v) = db.meta_get("public_trackers").and_then(|j| serde_json::from_str::<Vec<String>>(&j).ok()) {
        s.extend(v);
    }
    s
}

fn builtin_public_trackers() -> BTreeSet<String> {
    [
        "tracker.opentrackr.org:1337",
        "open.stealth.si:80",
        "tracker.torrent.eu.org:451",
        "exodus.desync.com:6969",
        "open.demonii.com:1337",
        "tracker.openbittorrent.com:6969",
        "tracker.openbittorrent.com",
        "opentracker.i2p.rocks:6969",
        "tracker.tiny-vps.com:6969",
        "explodie.org:6969",
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

pub fn load_defs(db: &Db) -> Vec<SiteDef> {
    let c = db.lock();
    let mut st = c.prepare("SELECT id,name,domains,tracker_hosts FROM site_defs").unwrap();
    st.query_map([], |r| {
        let d: String = r.get(2)?;
        let t: String = r.get(3)?;
        Ok(SiteDef { id: r.get(0)?, name: r.get(1)?, domains: serde_json::from_str(&d).unwrap_or_default(), tracker_hosts: serde_json::from_str(&t).unwrap_or_default() })
    })
    .unwrap()
    .filter_map(Result::ok)
    .collect()
}

pub fn upsert_def(db: &Db, d: &SiteDef, source: &str) -> Result<()> {
    db.lock().execute(
        "INSERT INTO site_defs(id,name,domains,tracker_hosts,source,updated_at) VALUES(?1,?2,?3,?4,?5,?6)
         ON CONFLICT(id) DO UPDATE SET name=excluded.name, domains=excluded.domains, tracker_hosts=excluded.tracker_hosts, source=excluded.source, updated_at=excluded.updated_at",
        params![d.id, d.name, serde_json::to_string(&d.domains)?, serde_json::to_string(&d.tracker_hosts)?, source, now()],
    )?;
    Ok(())
}

/// 应用注册中心的站点定义（03 §1.13.3）：
/// - 新站点写入定义（来源 registry）；已有定义（如来自 PT-depiler）保留名称，域名与 tracker 主机取并集；
/// - 站长态度以注册中心为准，`deny` 的站点立即关闭；
/// - `allow_skip_check`、`download_template` 写入站点特性。
pub fn apply_registry(db: &Db, list: &[xseed_transport::registry::SiteEntry], public: &[String]) -> Result<()> {
    let t = now();
    let c = db.lock();
    for e in list {
        let old: Option<(String, String)> = c.query_row("SELECT domains, tracker_hosts FROM site_defs WHERE id=?1", [&e.id], |r| Ok((r.get(0)?, r.get(1)?))).ok();
        let union = |old: Option<&str>, new: &[String]| -> String {
            let mut v: BTreeSet<String> = old.and_then(|o| serde_json::from_str::<Vec<String>>(o).ok()).unwrap_or_default().into_iter().collect();
            v.extend(new.iter().map(|x| x.to_ascii_lowercase()));
            serde_json::to_string(&v.into_iter().collect::<Vec<_>>()).unwrap_or_else(|_| "[]".into())
        };
        let domains = union(old.as_ref().map(|o| o.0.as_str()), &e.domains);
        let hosts = union(old.as_ref().map(|o| o.1.as_str()), &e.tracker_hosts);
        if old.is_some() {
            c.execute("UPDATE site_defs SET domains=?2, tracker_hosts=?3, p2p=?4, updated_at=?5 WHERE id=?1", params![e.id, domains, hosts, e.p2p, t])?;
        } else {
            c.execute("INSERT INTO site_defs(id,name,domains,tracker_hosts,p2p,source,updated_at) VALUES(?1,?2,?3,?4,?5,'registry',?6)", params![e.id, e.name, domains, hosts, e.p2p, t])?;
        }
        let sr = SiteRef::Site(e.id.clone()).key();
        if e.p2p == "deny" {
            c.execute("UPDATE sites SET enabled=0 WHERE site_ref=?1", [&sr])?;
        }
        let set = |name: &str, v: &str| c.execute("INSERT OR REPLACE INTO site_traits(site_ref, trait, value, learned_at, expires_at) VALUES(?1,?2,?3,?4,?5)", params![sr, name, v, t, t + 365 * 86_400]);
        set("allow_skip_check", if e.allow_skip_check { "true" } else { "false" })?;
        match &e.download_template {
            Some(tpl) => set("download_template", tpl)?,
            None => c.execute("DELETE FROM site_traits WHERE site_ref=?1 AND trait='download_template'", [&sr])?,
        };
    }
    drop(c);
    db.meta_set("public_trackers", &serde_json::to_string(public)?)?;
    Ok(())
}

#[derive(Serialize, Debug)]
pub struct SiteView {
    pub site_ref: String,
    pub name: String,
    pub host: String,
    pub torrents: i64,
    pub holders: i64,
    pub added: i64,
    pub enabled: bool,
    /// ok | need_cookie | denied
    pub status: &'static str,
    /// ptd | manual | tracker | none
    pub cred: &'static str,
    /// allow | unknown | deny
    pub p2p: String,
    pub added_today: i64,
    pub daily_add_limit: i64,
}

/// 站点列表：按本机种子数降序。
pub fn list(db: &Db, vault: &Vault) -> Vec<SiteView> {
    let rows: Vec<(String, i64, i64, String)> = {
        let c = db.lock();
        let mut st = c
            .prepare(
                "SELECT site_ref, COUNT(*), SUM(torrent_id IS NOT NULL), MIN(hosts) FROM local_torrents
                 WHERE site_ref IS NOT NULL AND state='ok' GROUP BY site_ref ORDER BY COUNT(*) DESC",
            )
            .unwrap();
        st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).unwrap().filter_map(Result::ok).collect()
    };
    let defs: BTreeMap<String, (String, String)> = {
        let c = db.lock();
        let mut st = c.prepare("SELECT id,name,p2p FROM site_defs").unwrap();
        st.query_map([], |r| Ok((r.get::<_, String>(0)?, (r.get(1)?, r.get(2)?)))).unwrap().filter_map(Result::ok).collect()
    };
    rows.into_iter()
        .map(|(site_ref, torrents, holders, hosts)| {
            let sr = SiteRef::parse_key(&site_ref);
            let (name, p2p) = match &sr {
                Some(SiteRef::Site(id)) => defs.get(id).cloned().unwrap_or((id.clone(), "unknown".into())),
                Some(SiteRef::Host(h)) => (h.clone(), "unknown".into()),
                None => (site_ref.clone(), "unknown".into()),
            };
            let host: String = serde_json::from_str::<Vec<String>>(&hosts).ok().and_then(|v| v.into_iter().next()).unwrap_or_default();
            let cred = if vault.credential(&site_ref, "cookie", "ptd").is_some() {
                "ptd"
            } else if vault.credential(&site_ref, "cookie", "manual").is_some() || vault.credential(&site_ref, "passkey", "manual").is_some() {
                "manual"
            } else if vault.credential(&site_ref, "passkey", "tracker").is_some() {
                "tracker"
            } else {
                "none"
            };
            let status = if p2p == "deny" {
                "denied"
            } else if cred == "none" {
                "need_cookie"
            } else {
                "ok"
            };
            let default_on = status == "ok";
            let (enabled, added_today, limit) = ensure_site(db, &site_ref, default_on);
            SiteView {
                site_ref,
                name,
                host,
                torrents,
                holders,
                added: 0,
                enabled: enabled && status != "denied",
                status,
                cred,
                p2p,
                added_today,
                daily_add_limit: limit,
            }
        })
        .collect()
}

/// 首次见到站点时按默认规则写入开关；返回（开关, 今日已加, 每日上限）。
fn ensure_site(db: &Db, site_ref: &str, default_on: bool) -> (bool, i64, i64) {
    let c = db.lock();
    let _ = c.execute("INSERT OR IGNORE INTO sites(site_ref, enabled) VALUES(?1, ?2)", params![site_ref, default_on as i64]);
    c.query_row("SELECT enabled, added_today, daily_add_limit FROM sites WHERE site_ref=?1", [site_ref], |r| Ok((r.get::<_, i64>(0)? != 0, r.get(1)?, r.get(2)?))).unwrap_or((default_on, 0, 50))
}

pub fn set_enabled(db: &Db, site_ref: &str, on: bool) -> Result<bool> {
    let n = db.lock().execute("UPDATE sites SET enabled=?2 WHERE site_ref=?1", params![site_ref, on as i64])?;
    Ok(n > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use xseed_transport::registry::SiteEntry;

    #[test]
    fn registry_sites_merge_and_deny() {
        let db = Db::memory();
        upsert_def(&db, &SiteDef { id: "ex".into(), name: "PTD 名称".into(), domains: vec!["example.org".into()], tracker_hosts: vec![] }, "ptd").unwrap();
        db.lock().execute("INSERT INTO sites(site_ref, enabled) VALUES('site:ex', 1)", []).unwrap();
        let list = vec![
            SiteEntry { id: "ex".into(), name: "注册中心名称".into(), domains: vec!["example.net".into()], tracker_hosts: vec!["t.example.org".into()], p2p: "deny".into(), allow_skip_check: false, download_template: None },
            SiteEntry { id: "new".into(), name: "New".into(), p2p: "allow".into(), allow_skip_check: true, ..Default::default() },
        ];
        apply_registry(&db, &list, &["pub.example:6969".into()]).unwrap();
        let (name, domains, p2p, source): (String, String, String, String) = db.lock().query_row("SELECT name, domains, p2p, source FROM site_defs WHERE id='ex'", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).unwrap();
        assert_eq!((name.as_str(), p2p.as_str(), source.as_str()), ("PTD 名称", "deny", "ptd"));
        assert_eq!(domains, r#"["example.net","example.org"]"#);
        let enabled: bool = db.lock().query_row("SELECT enabled FROM sites WHERE site_ref='site:ex'", [], |r| r.get(0)).unwrap();
        assert!(!enabled, "站长不允许的站点立即关闭");
        assert_eq!(load_defs(&db).len(), 2);
        assert!(public_trackers(&db).contains("pub.example:6969"));
    }
}
