//! 本地清单扫描（03 §1.2）。只读下载器元数据，不读数据文件。

use crate::db::{now, Db};
use crate::downloader::{Client, LocalTorrent, Meta};
use crate::sites;
use crate::vault::Vault;
use anyhow::Result;
use rusqlite::params;
use serde::Serialize;
use std::sync::{Arc, Mutex};
use xseed_core::fingerprint;
use xseed_core::site::{extract_passkey, identify, torrent_id_from_text, Identified, SiteDef, SiteRef};
use xseed_core::torrent;

#[derive(Default, Clone, Serialize)]
pub struct ScanState {
    pub running: bool,
    pub done: usize,
    pub total: usize,
    pub last_at: Option<i64>,
    pub error: Option<String>,
}

pub type Progress = Arc<Mutex<ScanState>>;

struct Row {
    hash: String,
    ih1: Option<Vec<u8>>,
    ih2: Option<Vec<u8>>,
    name: String,
    root_name: String,
    single_file: bool,
    save_path: String,
    category: Option<String>,
    total_size: u64,
    sfp: Option<Vec<u8>>,
    bucket: Option<u16>,
    site: Option<SiteRef>,
    hosts: Vec<String>,
    torrent_id: Option<String>,
    state: &'static str,
    reason: Option<String>,
    /// 私有 tracker 使用 http://（站点可能只有 http，下载种子时允许回退）
    tracker_http: bool,
}

fn known(db: &Db, downloader: &str, hash: &str) -> bool {
    db.lock().query_row("SELECT 1 FROM local_torrents WHERE downloader=?1 AND hash=?2 AND state!='invalid'", params![downloader, hash], |_| Ok(())).is_ok()
}

/// 扫描一个下载器。`full` 为 true 时重新解析所有种子（站点定义更新后用）。
pub async fn scan(db: &Db, vault: &Vault, downloader_id: &str, client: &Client, full: bool, progress: &Progress) -> Result<usize> {
    let list = client.list_completed().await?;
    let defs = sites::load_defs(db);
    let public = sites::public_trackers(db);
    let started = now();
    {
        let mut p = progress.lock().unwrap();
        p.total = list.len();
        p.done = 0;
    }
    for t in &list {
        if !full && known(db, downloader_id, &t.hash) {
            db.lock().execute("UPDATE local_torrents SET last_seen=?3 WHERE downloader=?1 AND hash=?2", params![downloader_id, t.hash, started])?;
        } else {
            let row = analyze(client, t, &defs, &public, vault).await;
            upsert(db, downloader_id, &row, started)?;
        }
        progress.lock().unwrap().done += 1;
    }
    // 下载器里已经没有的种子从清单移除
    db.lock().execute("DELETE FROM local_torrents WHERE downloader=?1 AND last_seen < ?2", params![downloader_id, started])?;
    Ok(list.len())
}

async fn analyze(client: &Client, t: &LocalTorrent, defs: &[SiteDef], public: &std::collections::BTreeSet<String>, vault: &Vault) -> Row {
    let mut row = Row {
        hash: t.hash.clone(),
        ih1: t.ih1.as_deref().and_then(|h| hex::decode(h).ok()),
        ih2: t.ih2.as_deref().and_then(|h| hex::decode(h).ok()),
        name: t.name.clone(),
        root_name: t.name.clone(),
        single_file: false,
        save_path: t.save_path.clone(),
        category: t.category.clone(),
        total_size: t.size,
        sfp: None,
        bucket: None,
        site: None,
        hosts: vec![],
        torrent_id: None,
        state: "invalid",
        reason: None,
        tracker_http: false,
    };
    if t.broken {
        row.reason = Some("missing_files".into());
        return row;
    }
    // 元数据 → 指纹
    let mut comment = None;
    let mut trackers = Vec::new();
    match client.meta(t).await {
        Ok(Meta::Raw(raw)) => match torrent::parse(&raw) {
            Ok(tt) => {
                comment = tt.comment.clone().or(tt.source.clone());
                trackers = tt.announces.clone();
                set_entries(&mut row, &tt.entries);
            }
            Err(e) => row.reason = Some(format!("invalid_torrent: {e}")),
        },
        Ok(Meta::Entries(e)) => set_entries(&mut row, &e),
        Err(e) => row.reason = Some(e.to_string()),
    }
    if row.sfp.is_none() {
        return row;
    }
    // 下载器的 tracker 列表比种子里的更新（用户可能改过 passkey）
    if let Ok(tl) = client.trackers(t).await {
        if !tl.is_empty() {
            trackers = tl;
        }
    }
    if comment.is_none() {
        comment = client.comment(t).await.ok().flatten();
    }
    row.hosts = trackers.iter().filter_map(|u| xseed_core::site::normalize_tracker_host(u)).filter(|h| !public.contains(h)).collect::<std::collections::BTreeSet<_>>().into_iter().collect();
    row.tracker_http = trackers.iter().any(|u| u.starts_with("http://") && xseed_core::site::normalize_tracker_host(u).is_some_and(|h| !public.contains(&h)));
    match identify(&trackers, defs, public) {
        Identified::One(sr) => {
            if let (Some(text), SiteRef::Site(id)) = (&comment, &sr) {
                if let Some(def) = defs.iter().find(|d| &d.id == id) {
                    row.torrent_id = torrent_id_from_text(text, def);
                }
            }
            if let (Some(text), SiteRef::Host(h)) = (&comment, &sr) {
                let pseudo = SiteDef { domains: vec![h.split(':').next().unwrap_or(h).to_string()], ..Default::default() };
                row.torrent_id = torrent_id_from_text(text, &pseudo);
            }
            if let Some(pk) = trackers.iter().find_map(|u| extract_passkey(u)) {
                let key = sr.key();
                if vault.credential(&key, "passkey", "tracker").as_deref() != Some(pk.as_str()) {
                    let _ = vault.set_credential(&key, "passkey", "tracker", &pk);
                }
            }
            row.site = Some(sr);
            row.state = "ok";
        }
        Identified::Ambiguous => {
            row.state = "ambiguous";
            row.reason = Some("ambiguous_site".into());
        }
        Identified::None => {
            row.state = "unknown_site";
            row.reason = Some("no_private_tracker".into());
        }
    }
    row
}

fn set_entries(row: &mut Row, e: &torrent::Entries) {
    let s = fingerprint::sfp(e);
    row.bucket = Some(fingerprint::bucket(&s));
    row.sfp = Some(s.to_vec());
    row.root_name = e.root_name.clone();
    row.single_file = e.single_file;
    row.total_size = e.total_size();
}

fn upsert(db: &Db, downloader: &str, r: &Row, at: i64) -> Result<()> {
    let (kind, sref) = match &r.site {
        Some(s @ SiteRef::Site(_)) => (Some("site"), Some(s.key())),
        Some(s @ SiteRef::Host(_)) => (Some("host"), Some(s.key())),
        None => (None, None),
    };
    db.lock().execute(
        "INSERT INTO local_torrents(downloader,hash,ih1,ih2,name,root_name,single_file,save_path,category,total_size,sfp,bucket,site_kind,site_ref,hosts,torrent_id,state,reason,first_seen,last_seen)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?19)
         ON CONFLICT(downloader,hash) DO UPDATE SET ih1=excluded.ih1, ih2=excluded.ih2, name=excluded.name, root_name=excluded.root_name,
           single_file=excluded.single_file, save_path=excluded.save_path, category=excluded.category, total_size=excluded.total_size,
           sfp=excluded.sfp, bucket=excluded.bucket, site_kind=excluded.site_kind, site_ref=excluded.site_ref, hosts=excluded.hosts,
           torrent_id=excluded.torrent_id, state=excluded.state, reason=excluded.reason, last_seen=excluded.last_seen",
        params![
            downloader,
            r.hash,
            r.ih1,
            r.ih2,
            r.name,
            r.root_name,
            r.single_file as i64,
            r.save_path,
            r.category,
            r.total_size as i64,
            r.sfp,
            r.bucket,
            kind,
            sref,
            serde_json::to_string(&r.hosts)?,
            r.torrent_id,
            r.state,
            r.reason,
            at
        ],
    )?;
    if let (true, Some(sr)) = (r.tracker_http, &sref) {
        db.lock().execute(
            "INSERT OR REPLACE INTO site_traits(site_ref, trait, value, learned_at, expires_at) VALUES(?1, 'tracker_scheme', 'http', ?2, ?3)",
            params![sr, at, at + 365 * 86_400],
        )?;
    }
    Ok(())
}

#[derive(Serialize, Default)]
pub struct Stats {
    pub torrents: i64,
    pub ok: i64,
    pub holders: i64,
    pub sites: i64,
    pub total_size: i64,
    pub invalid: i64,
    pub ambiguous: i64,
    pub unknown_site: i64,
    /// 本机已在多个站做种的内容数（同一指纹出现在 ≥2 个站）
    pub multi_site: i64,
}

pub fn stats(db: &Db) -> Stats {
    let c = db.lock();
    let q = |sql: &str| c.query_row(sql, [], |r| r.get::<_, Option<i64>>(0)).ok().flatten().unwrap_or(0);
    Stats {
        torrents: q("SELECT COUNT(*) FROM local_torrents"),
        ok: q("SELECT COUNT(*) FROM local_torrents WHERE state='ok'"),
        holders: q("SELECT COUNT(*) FROM local_torrents WHERE state='ok' AND torrent_id IS NOT NULL"),
        sites: q("SELECT COUNT(DISTINCT site_ref) FROM local_torrents WHERE state='ok'"),
        total_size: q("SELECT SUM(total_size) FROM local_torrents WHERE state='ok'"),
        invalid: q("SELECT COUNT(*) FROM local_torrents WHERE state='invalid'"),
        ambiguous: q("SELECT COUNT(*) FROM local_torrents WHERE state='ambiguous'"),
        unknown_site: q("SELECT COUNT(*) FROM local_torrents WHERE state='unknown_site'"),
        multi_site: q("SELECT COUNT(*) FROM (SELECT sfp FROM local_torrents WHERE state='ok' GROUP BY sfp HAVING COUNT(DISTINCT site_ref) >= 2)"),
    }
}
