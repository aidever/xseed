//! 站点定义、识别与站点列表汇总（02 §3.2）。

use crate::db::{now, Db};
use crate::vault::Vault;
use anyhow::Result;
use rusqlite::params;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use xseed_core::site::{SiteDef, SiteRef};

/// 常见公共 tracker（不能作为站点身份）。M5 起由注册中心下发。
pub fn public_trackers() -> BTreeSet<String> {
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
