//! 自动加种（03 §1.8–§1.10）：告知 → 任务 → 下载种子 → 核对 → 加入下载器 → 观察。
//!
//! 任务状态：`pending → fetching → verifying → adding → observing → seeding`；
//! 失败为 `failed(reason)`，用户撤回为 `removed`。一次只推进一个任务，避免同时冲击站点与下载器。
//! 新种子一律以停止状态加入，观察时确认数据完整（进度 100%）后才启动，绝不会让下载器往已有文件里写数据。

use crate::app::Shared;
use crate::db::now;
use crate::downloader::{AddRequest, Announce, Client, Health, Layout};
use crate::fetch::{self, FetchError, Purpose};
use anyhow::Result;
use rand::Rng;
use rusqlite::{params, OptionalExtension};
use std::time::Duration;
use xseed_core::site::{normalize_tracker_host, site_matches, SiteRef};

/// 加入后多久开始检查（秒）。
const OBSERVE_FIRST: i64 = 30;
/// 观察多久后，没有明确失败就视为做种中（秒）。
const OBSERVE_MAX: i64 = 30 * 60;
/// 站点暂时连不上时最多重试几次。
const NET_RETRIES: i64 = 3;

pub async fn run(app: Shared) {
    recover(&app);
    loop {
        if app.cfg().setup_done {
            if let Err(e) = tick(&app).await {
                tracing::warn!("加种：{e:#}");
            }
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(15)) => {}
            _ = app.jobs_kick.notified() => {}
        }
    }
}

/// 进程重启时，把中途打断的任务放回队列（已加入的会在“已存在”检查中接上观察）。
fn recover(app: &Shared) {
    let _ = app.db.lock().execute("UPDATE jobs SET state='pending' WHERE state IN ('fetching','verifying','adding')", []);
}

async fn tick(app: &Shared) -> Result<()> {
    create_jobs(app)?;
    observe(app).await?;
    if app.cfg().dry_run {
        return Ok(());
    }
    for _ in 0..5 {
        let id: Option<i64> = app.db.lock().query_row("SELECT id FROM jobs WHERE state='pending' AND not_before<=?1 ORDER BY not_before, id LIMIT 1", [now()], |r| r.get(0)).optional()?;
        let Some(id) = id else { break };
        process(app, id).await;
    }
    Ok(())
}

/// 为新的告知创建任务：找到本机同款种子作为数据来源。
fn create_jobs(app: &Shared) -> Result<()> {
    let max_delay = app.cfg().net.add_delay_max_secs;
    let c = app.db.lock();
    let offers: Vec<(i64, Vec<u8>)> = c.prepare("SELECT id, sfp FROM offers WHERE status='new'")?.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.filter_map(Result::ok).collect();
    for (oid, sfp) in offers {
        let local: Option<(String, String)> = c
            .query_row("SELECT downloader, hash FROM local_torrents WHERE sfp=?1 AND state<>'invalid' ORDER BY first_seen LIMIT 1", [&sfp], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()?;
        match local {
            Some((dl, hash)) => {
                let t = now();
                // 启动时间随机错开（03 §4.1 start_delay）
                let delay = if max_delay == 0 { 0 } else { rand::thread_rng().gen_range(0..=max_delay) as i64 };
                c.execute(
                    "INSERT INTO jobs(offer_id, local_downloader, local_hash, state, created_at, updated_at, not_before) VALUES(?1, ?2, ?3, 'pending', ?4, ?4, ?5)",
                    params![oid, dl, hash, t, t + delay],
                )?;
                c.execute("UPDATE offers SET status='used' WHERE id=?1", [oid])?;
            }
            None => {
                c.execute("UPDATE offers SET status='invalid' WHERE id=?1", [oid])?;
            }
        }
    }
    Ok(())
}

struct Job {
    id: i64,
    attempts: i64,
    downloader: String,
    local_hash: String,
    site: String,
    torrent_id: String,
    sfp: Vec<u8>,
    ih1: Option<Vec<u8>>,
    ih2: Option<Vec<u8>>,
    /// 本机种子：(save_path, root_name, single_file, category)
    local: Option<(String, String, bool, Option<String>)>,
}

fn load(app: &Shared, id: i64) -> Result<Option<Job>> {
    Ok(app
        .db
        .lock()
        .query_row(
            "SELECT j.id, j.attempts, j.local_downloader, j.local_hash, o.site_ref, o.torrent_id, o.sfp, o.ih1, o.ih2,
                    lt.save_path, lt.root_name, lt.single_file, lt.category
             FROM jobs j JOIN offers o ON o.id=j.offer_id
             LEFT JOIN local_torrents lt ON lt.downloader=j.local_downloader AND lt.hash=j.local_hash
             WHERE j.id=?1",
            [id],
            |r| {
                let save: Option<String> = r.get(9)?;
                Ok(Job {
                    id: r.get(0)?,
                    attempts: r.get(1)?,
                    downloader: r.get(2)?,
                    local_hash: r.get(3)?,
                    site: r.get(4)?,
                    torrent_id: r.get(5)?,
                    sfp: r.get(6)?,
                    ih1: r.get(7)?,
                    ih2: r.get(8)?,
                    local: match save {
                        Some(s) => Some((s, r.get(10)?, r.get::<_, i64>(11)? != 0, r.get(12)?)),
                        None => None,
                    },
                })
            },
        )
        .optional()?)
}

fn set_state(app: &Shared, id: i64, state: &str, reason: Option<&str>) {
    let _ = app.db.lock().execute("UPDATE jobs SET state=?2, reason=?3, updated_at=?4 WHERE id=?1", params![id, state, reason, now()]);
}

fn fail(app: &Shared, j: &Job, reason: &str) {
    tracing::info!("加种失败 {} #{}：{reason}", j.site, j.torrent_id);
    set_state(app, j.id, "failed", Some(reason));
}

/// 暂缓：回到队列，`secs` 秒后再试。
fn defer(app: &Shared, id: i64, secs: i64, reason: &str, count_attempt: bool) {
    let _ = app.db.lock().execute(
        "UPDATE jobs SET state='pending', reason=?2, not_before=?3, attempts=attempts+?4, updated_at=?5 WHERE id=?1",
        params![id, reason, now() + secs, count_attempt as i64, now()],
    );
}

/// 今日该站还能加几个（站点设置与全局设置取小者）。
fn quota_left(app: &Shared, site: &str) -> i64 {
    let d = xseed_transport::day(now() as u64) as i64;
    let global = app.cfg().daily_add_limit as i64;
    let c = app.db.lock();
    let _ = c.execute("INSERT OR IGNORE INTO sites(site_ref, enabled) VALUES(?1, 0)", [site]);
    let _ = c.execute("UPDATE sites SET answered_today=0, added_today=0, day=?2 WHERE site_ref=?1 AND day<>?2", params![site, d]);
    c.query_row("SELECT MIN(daily_add_limit, ?2) - added_today FROM sites WHERE site_ref=?1", params![site, global], |r| r.get(0)).unwrap_or(0)
}

fn secs_to_tomorrow() -> i64 {
    let t = now();
    86_400 - t.rem_euclid(86_400) + rand::thread_rng().gen_range(60..3600)
}

/// 新种子的全部 tracker 都属于该站（03 §1.8 foreign_tracker）。
fn trackers_belong(app: &Shared, site: &str, announces: &[String]) -> bool {
    let hosts: Vec<String> = announces.iter().filter_map(|u| normalize_tracker_host(u)).collect();
    if hosts.is_empty() {
        return false;
    }
    match SiteRef::parse_key(site) {
        Some(SiteRef::Site(id)) => crate::sites::load_defs(&app.db).into_iter().find(|d| d.id == id).is_some_and(|d| hosts.iter().all(|h| site_matches(&d, h))),
        Some(SiteRef::Host(h)) => hosts.iter().all(|x| *x == h),
        None => false,
    }
}

fn site_trait(app: &Shared, site: &str, name: &str) -> Option<String> {
    app.db.lock().query_row("SELECT value FROM site_traits WHERE site_ref=?1 AND trait=?2 AND expires_at > ?3", params![site, name, now()], |r| r.get(0)).ok()
}

async fn process(app: &Shared, id: i64) {
    let j = match load(app, id) {
        Ok(Some(j)) => j,
        _ => return,
    };
    let Some((save_path, root_name, single_file, category)) = j.local.clone() else {
        return fail(app, &j, "source_state_changed");
    };
    // 用户关掉了该站，或站长表态不允许：不再加种
    let enabled: bool = app.db.lock().query_row("SELECT enabled FROM sites WHERE site_ref=?1", [&j.site], |r| r.get(0)).unwrap_or(true);
    if !enabled {
        tracing::info!("{} 已关闭，取消 #{}", j.site, j.torrent_id);
        return set_state(app, j.id, "removed", Some("site_disabled"));
    }
    if quota_left(app, &j.site) <= 0 {
        tracing::info!("{} 今日加种已达上限，明天继续", j.site);
        return defer(app, j.id, secs_to_tomorrow(), "daily_limit", false);
    }

    // 1. 用自己的账号下载种子
    set_state(app, j.id, "fetching", None);
    let bytes = match fetch::download(app, &j.site, &j.torrent_id, Purpose::Target, Duration::from_secs(120)).await {
        Ok(b) => b,
        Err(FetchError::RateLimited) => return defer(app, j.id, 120, "site_rate_limited", false),
        Err(FetchError::Network(_)) if j.attempts < NET_RETRIES => return defer(app, j.id, 600 * (j.attempts + 1), "site_unreachable", true),
        Err(FetchError::Network(_)) => return fail(app, &j, "site_unreachable"),
        Err(FetchError::NoCredential) => return fail(app, &j, "site_auth_failed"),
        Err(FetchError::Denied) => return fail(app, &j, "site_denied"),
        Err(FetchError::NotFound | FetchError::NotTorrent) => return fail(app, &j, "no_source"),
    };

    // 2. 核对：info_hash 与告知一致、文件结构与本机一致、tracker 属于该站
    set_state(app, j.id, "verifying", None);
    let Ok(t) = xseed_core::torrent::parse(&bytes) else {
        return fail(app, &j, "no_source");
    };
    let ih_ok = j.ih1.as_deref().is_none_or(|h| t.ih1.as_ref().is_some_and(|x| x[..] == *h)) && j.ih2.as_deref().is_none_or(|h| t.ih2.as_ref().is_some_and(|x| x[..] == *h)) && (j.ih1.is_some() || j.ih2.is_some());
    if !ih_ok {
        return fail(app, &j, "ih_mismatch");
    }
    if xseed_core::fingerprint::sfp(&t.entries)[..] != j.sfp[..] {
        return fail(app, &j, "sfp_mismatch");
    }
    if !trackers_belong(app, &j.site, &t.announces) {
        return fail(app, &j, "foreign_tracker");
    }

    let client = match app.client(&j.downloader) {
        Ok(c) => c,
        Err(_) => return fail(app, &j, "downloader_error"),
    };
    let Some(new_hash) = client.hash_of(t.ih1.as_ref().map(|x| &x[..]), t.ih2.as_ref().map(|x| &x[..])) else {
        return fail(app, &j, "no_source");
    };
    // 幂等：下载器里已经有这个种子
    match client.status(&new_hash).await {
        Err(_) => return defer(app, j.id, 300, "downloader_unreachable", false),
        Ok(Some(_)) => {
            let ours: bool = app.db.lock().query_row("SELECT 1 FROM added_torrents WHERE downloader=?1 AND hash=?2 AND job_id=?3 AND removed_at IS NULL", params![j.downloader, new_hash, j.id], |_| Ok(true)).unwrap_or(false);
            if ours {
                // 上次加入后进程被打断，接着观察
                let _ = app.db.lock().execute("UPDATE jobs SET state='observing', new_hash=?2, reason=NULL, updated_at=?3 WHERE id=?1", params![j.id, new_hash, now()]);
            } else {
                tracing::info!("{} #{} 已在下载器中", j.site, j.torrent_id);
                set_state(app, j.id, "seeding", Some("already_present"));
            }
            return;
        }
        Ok(None) => {}
    }

    // 3. 再检查一次原种子：仍然完整、位置没变（03 §1.9）
    let src = match client.status(&j.local_hash).await {
        Ok(Some(s)) => s,
        Ok(None) => return fail(app, &j, "source_state_changed"),
        Err(_) => return defer(app, j.id, 300, "downloader_unreachable", false),
    };
    if src.health != Health::Ok || src.progress < 1.0 || src.save_path.trim_end_matches(['/', '\\']) != save_path.trim_end_matches(['/', '\\']) {
        return fail(app, &j, "source_state_changed");
    }

    // 4. 加入下载器（停止状态，指向已有文件；03 §1.10 布局表）
    set_state(app, j.id, "adding", None);
    let is_qb = matches!(client, Client::Qb(_));
    let req = add_request(is_qb, single_file || t.entries.single_file, &src, &t.entries.root_name, &root_name, bytes, category.clone());
    let req = AddRequest { skip_check: site_trait(app, &j.site, "allow_skip_check").as_deref() != Some("false"), ..req };
    let t0 = now();
    let _ = app.db.lock().execute("INSERT OR REPLACE INTO added_torrents(downloader, hash, job_id, added_at, removed_at) VALUES(?1, ?2, ?3, ?4, NULL)", params![j.downloader, new_hash, j.id, t0]);
    if let Err(e) = client.add(req).await {
        tracing::warn!("加入下载器失败：{e:#}");
        let _ = app.db.lock().execute("DELETE FROM added_torrents WHERE downloader=?1 AND hash=?2 AND job_id=?3", params![j.downloader, new_hash, j.id]);
        return fail(app, &j, "downloader_error");
    }
    {
        let c = app.db.lock();
        let _ = c.execute("UPDATE sites SET added_today=added_today+1 WHERE site_ref=?1", [&j.site]);
        let _ = c.execute("UPDATE jobs SET state='observing', new_hash=?2, reason=NULL, updated_at=?3 WHERE id=?1", params![j.id, new_hash, t0]);
    }
    app.db.audit("torrent_added", &format!("{} #{} {}", j.site, j.torrent_id, new_hash));
    tracing::info!("已加入 {} #{}（{}），观察中", j.site, j.torrent_id, new_hash);
}

/// 按 03 §1.10 布局表决定新种子的保存位置。`src` 为原种子在下载器中的状态，`local_root` 为原种子的根目录名。
fn add_request(is_qb: bool, single: bool, src: &crate::downloader::Status, new_root: &str, local_root: &str, torrent: Vec<u8>, category: Option<String>) -> AddRequest {
    let base = AddRequest { torrent, save_path: src.save_path.clone(), layout: Layout::Original, rename_root: None, skip_check: true, category };
    if single {
        // 单文件：文件名包含在指纹里，一定相同
        base
    } else if is_qb {
        // qB：多文件一律以“无子文件夹”加入原内容目录，根目录名相同或不同都适用
        AddRequest { save_path: src.content_path.clone(), layout: Layout::NoSubfolder, ..base }
    } else {
        // TR：加入同一目录，根目录名不同时改名为原根目录名
        AddRequest { rename_root: (new_root != local_root).then(|| (new_root.to_string(), local_root.to_string())), ..base }
    }
}

/// tracker 返回的失败信息归类；无法判断的视为暂时性问题。
pub fn tracker_reason(msg: &str) -> Option<&'static str> {
    let m = msg.to_lowercase();
    const UNREG: [&str; 7] = ["unregistered", "not registered", "torrent not found", "not exist", "未注册", "不存在", "没有找到"];
    const AUTH: [&str; 9] = ["passkey", "authkey", "auth", "denied", "forbidden", "invalid key", "banned", "未授权", "无效"];
    if UNREG.iter().any(|k| m.contains(k)) {
        Some("tracker_unregistered")
    } else if AUTH.iter().any(|k| m.contains(k)) {
        Some("tracker_auth_failed")
    } else {
        None
    }
}

/// 观察刚加入的种子：数据完整才启动；tracker 明确拒绝就移除（保留文件）。
async fn observe(app: &Shared) -> Result<()> {
    let list: Vec<(i64, String, String, i64)> = app
        .db
        .lock()
        .prepare("SELECT j.id, j.local_downloader, j.new_hash, j.updated_at FROM jobs j WHERE j.state='observing' AND j.new_hash IS NOT NULL")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .filter_map(Result::ok)
        .collect();
    for (id, dl, hash, since) in list {
        let age = now() - since;
        if age < OBSERVE_FIRST {
            continue;
        }
        let Ok(client) = app.client(&dl) else { continue };
        let st = match client.status(&hash).await {
            Ok(s) => s,
            Err(_) => continue,
        };
        let remove = |reason: &'static str| {
            let client = &client;
            let hash = hash.clone();
            let dl = dl.clone();
            async move {
                let _ = client.remove_keep_data(&hash).await;
                let c = app.db.lock();
                let _ = c.execute("UPDATE added_torrents SET removed_at=?3 WHERE downloader=?1 AND hash=?2", params![dl, hash, now()]);
                let _ = c.execute("UPDATE jobs SET state='failed', reason=?2, updated_at=?3 WHERE id=?1", params![id, reason, now()]);
                drop(c);
                app.db.audit("torrent_removed", &format!("{hash} {reason}"));
                tracing::info!("已移除 {hash}（保留文件）：{reason}");
            }
        };
        let Some(st) = st else {
            // 用户在下载器里自己删掉了
            let c = app.db.lock();
            let _ = c.execute("UPDATE added_torrents SET removed_at=?3 WHERE downloader=?1 AND hash=?2", params![dl, hash, now()]);
            let _ = c.execute("UPDATE jobs SET state='removed', updated_at=?2 WHERE id=?1", params![id, now()]);
            continue;
        };
        match st.health {
            Health::Checking => continue,
            Health::Broken(_) => {
                remove("missing_files").await;
                continue;
            }
            Health::Ok if st.progress < 1.0 => {
                remove("missing_files").await;
                continue;
            }
            Health::Ok => {}
        }
        // 数据完整：启动（重复启动无副作用）
        let _ = client.start(&hash).await;
        match client.announce(&hash).await {
            Ok(Announce::Working) => {
                set_state(app, id, "seeding", None);
                tracing::info!("{hash} 开始做种");
            }
            Ok(Announce::Failed(msg)) => match tracker_reason(&msg) {
                Some(r) => remove(r).await,
                None if age > OBSERVE_MAX => set_state(app, id, "seeding", None),
                None => {}
            },
            _ if age > OBSERVE_MAX => set_state(app, id, "seeding", None),
            _ => {}
        }
    }
    Ok(())
}

/// 五步进度轨上已完成的步数（发现、答题、下载、校验、做种）。
pub fn step(state: &str, reason: Option<&str>, added: bool) -> u8 {
    match state {
        "seeding" => 5,
        "adding" | "observing" => 4,
        "verifying" => 3,
        "failed" => match reason.unwrap_or_default() {
            "ih_mismatch" | "sfp_mismatch" | "foreign_tracker" => 3,
            "source_state_changed" | "downloader_error" | "downloader_unreachable" | "missing_files" | "tracker_unregistered" | "tracker_auth_failed" => 4,
            _ => 2,
        },
        "removed" if added => 4,
        _ => 2,
    }
}

/// 漏斗：（已下载种子, 已校验, 做种中）。
pub fn funnel(db: &crate::db::Db) -> (i64, i64, i64) {
    let c = db.lock();
    let Ok(mut st) = c.prepare("SELECT state, reason, new_hash IS NOT NULL FROM jobs") else { return (0, 0, 0) };
    let mut f = (0, 0, 0);
    if let Ok(rows) = st.query_map([], |r| Ok(step(&r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?.as_deref(), r.get(2)?))) {
        for s in rows.flatten() {
            f.0 += (s >= 3) as i64;
            f.1 += (s >= 4) as i64;
            f.2 += (s == 5) as i64;
        }
    }
    f
}

/// 重试一个失败的任务。
pub fn retry(app: &Shared, id: i64) -> Result<bool> {
    let n = app.db.lock().execute("UPDATE jobs SET state='pending', reason=NULL, not_before=0, attempts=attempts+1, updated_at=?2 WHERE id=?1 AND state='failed'", params![id, now()])?;
    app.jobs_kick.notify_one();
    Ok(n > 0)
}

/// 移除本软件加入的全部种子（保留文件）。只动 `added_torrents` 中有记录、并且仍带 `xseed` 标签的种子。
/// 返回（已移除, 跳过）。
pub async fn remove_all(app: &Shared) -> Result<(usize, usize)> {
    let list: Vec<(String, String, i64)> = app.db.lock().prepare("SELECT downloader, hash, job_id FROM added_torrents WHERE removed_at IS NULL")?.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.filter_map(Result::ok).collect();
    let (mut removed, mut skipped) = (0, 0);
    for (dl, hash, job) in list {
        let client = app.client(&dl)?;
        match client.status(&hash).await? {
            Some(s) if s.tagged => client.remove_keep_data(&hash).await?,
            Some(_) => {
                // 标签被去掉了：可能用户想保留，不动
                skipped += 1;
                continue;
            }
            None => {}
        }
        let c = app.db.lock();
        c.execute("UPDATE added_torrents SET removed_at=?3 WHERE downloader=?1 AND hash=?2", params![dl, hash, now()])?;
        c.execute("UPDATE jobs SET state='removed', updated_at=?2 WHERE id=?1", params![job, now()])?;
        removed += 1;
    }
    // 还没加入的任务一并取消
    app.db.lock().execute("UPDATE jobs SET state='removed', reason='cancelled', updated_at=?1 WHERE state='pending'", [now()])?;
    app.db.audit("remove_all", &format!("removed={removed} skipped={skipped}"));
    Ok((removed, skipped))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::downloader::Status;

    fn src() -> Status {
        Status { health: Health::Ok, progress: 1.0, save_path: "/d".into(), content_path: "/d/Show.S01".into(), tagged: false }
    }

    #[test]
    fn layout_by_downloader() {
        let q = add_request(true, false, &src(), "Show.S01.Other", "Show.S01", vec![], None);
        assert_eq!((q.save_path.as_str(), &q.layout, &q.rename_root), ("/d/Show.S01", &Layout::NoSubfolder, &None));
        let t = add_request(false, false, &src(), "Show.S01.Other", "Show.S01", vec![], None);
        assert_eq!((t.save_path.as_str(), &t.layout), ("/d", &Layout::Original));
        assert_eq!(t.rename_root, Some(("Show.S01.Other".into(), "Show.S01".into())));
        let same = add_request(false, false, &src(), "Show.S01", "Show.S01", vec![], None);
        assert_eq!(same.rename_root, None);
        let single = add_request(true, true, &src(), "a.mkv", "a.mkv", vec![], Some("tv".into()));
        assert_eq!((single.save_path.as_str(), &single.layout, single.category.as_deref()), ("/d", &Layout::Original, Some("tv")));
    }

    #[test]
    fn steps_on_track() {
        assert_eq!(step("pending", None, false), 2);
        assert_eq!(step("verifying", None, false), 3);
        assert_eq!(step("observing", None, true), 4);
        assert_eq!(step("seeding", None, true), 5);
        assert_eq!(step("failed", Some("site_auth_failed"), false), 2);
        assert_eq!(step("failed", Some("sfp_mismatch"), false), 3);
        assert_eq!(step("failed", Some("tracker_unregistered"), true), 4);
    }

    #[test]
    fn classify_tracker_messages() {
        assert_eq!(tracker_reason("Unregistered torrent"), Some("tracker_unregistered"));
        assert_eq!(tracker_reason("种子未注册"), Some("tracker_unregistered"));
        assert_eq!(tracker_reason("Invalid passkey"), Some("tracker_auth_failed"));
        assert_eq!(tracker_reason("Connection refused"), None);
        assert_eq!(tracker_reason("timed out"), None);
    }
}
