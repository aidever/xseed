//! 会员答题会话（03 §1.5.2–§1.6）：持有者出题、判题、告知；查询方答题、收下告知。
//!
//! 会话经入口节点以 `noise` 信封传递：探测 → 回应 → 握手 3 条 → 询问 → 出题 → 答题 → 告知。
//! 一次会话只问一个站点，查询方每次使用新的回信代号。

use crate::app::Shared;
use crate::fetch::{self, FetchError, Purpose};
use anyhow::{anyhow, Result};
use rand::seq::SliceRandom;
use rand::Rng;
use rusqlite::params;
use std::collections::HashSet;
use std::sync::atomic::Ordering::Relaxed;
use std::time::Duration;
use tokio::sync::mpsc;
use xseed_session::{answer_mac, check_answer, code as ecode, Msg, Session};
use xseed_transport::wire::*;
use xseed_transport::{now, probe, week, PROTO};

/// 同时进行的会话上限（每个角色，03 §7.3）。
pub const MAX_SESSIONS: usize = 8;

const T_PROBE: Duration = Duration::from_secs(15);
const T_HANDSHAKE: Duration = Duration::from_secs(20);
const T_ASK: Duration = Duration::from_secs(10);
const T_ANSWER: Duration = Duration::from_secs(120);
const T_TOTAL: Duration = Duration::from_secs(240);

/// 询问的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Offer { torrent_id: String },
    None,
    NotMember,
    Failed(String),
}

impl Outcome {
    fn label(&self) -> &str {
        match self {
            Outcome::Offer { .. } => "offer",
            Outcome::None => "none",
            Outcome::NotMember => "not_member",
            Outcome::Failed(_) => "failed",
        }
    }
}

struct Route {
    app: Shared,
    sid: [u8; 16],
}

impl Drop for Route {
    fn drop(&mut self) {
        self.app.net.sessions.lock().unwrap().remove(&self.sid);
    }
}

fn route(app: &Shared, sid: [u8; 16]) -> (Route, mpsc::Receiver<Envelope>) {
    let (tx, rx) = mpsc::channel(16);
    app.net.sessions.lock().unwrap().insert(sid, tx);
    (Route { app: app.clone(), sid }, rx)
}

async fn recv(rx: &mut mpsc::Receiver<Envelope>, t: Duration, kind: Kind) -> Result<Envelope> {
    loop {
        let env = tokio::time::timeout(t, rx.recv()).await.map_err(|_| anyhow!("timeout"))?.ok_or_else(|| anyhow!("closed"))?;
        if env.kind == Kind::Close {
            return Err(anyhow!("peer_closed"));
        }
        if env.kind == kind {
            return Ok(env);
        }
    }
}

async fn recv_msg(s: &mut Session, rx: &mut mpsc::Receiver<Envelope>, t: Duration) -> Result<Msg> {
    let env = recv(rx, t, Kind::Noise).await?;
    s.open(&env.body).map_err(|e| anyhow!("{e}"))
}

fn env(to: Addr, from: Addr, sid: [u8; 16], kind: Kind, body: Vec<u8>) -> Envelope {
    Envelope { v: PROTO, to, from, sid, kind, body }
}

/// 今日该站的计数（答题下载）；超过上限返回 false。
fn take_answer_quota(app: &Shared, site: &str) -> bool {
    let d = xseed_transport::day(now()) as i64;
    let limit = app.cfg().daily_answer_limit as i64;
    let c = app.db.lock();
    let _ = c.execute("INSERT OR IGNORE INTO sites(site_ref, enabled) VALUES(?1, 0)", [site]);
    let _ = c.execute("UPDATE sites SET answered_today=0, added_today=0, day=?2 WHERE site_ref=?1 AND day<>?2", params![site, d]);
    let used: i64 = c.query_row("SELECT answered_today FROM sites WHERE site_ref=?1", [site], |r| r.get(0)).unwrap_or(0);
    if used >= limit {
        return false;
    }
    let _ = c.execute("UPDATE sites SET answered_today=answered_today+1 WHERE site_ref=?1", [site]);
    true
}

// ───────── 查询方 ─────────

/// 向一个同款持有者询问某站点。
pub async fn ask(app: &Shared, sfp: [u8; 32], site: &str, peer: Addr) -> Outcome {
    match tokio::time::timeout(T_TOTAL, ask_inner(app, sfp, site, peer)).await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => Outcome::Failed(e.to_string()),
        Err(_) => Outcome::Failed("timeout".into()),
    }
}

async fn ask_inner(app: &Shared, sfp: [u8; 32], site: &str, peer: Addr) -> Result<Outcome> {
    let (tx, my_node) = app.net.sender().ok_or_else(|| anyhow!("offline"))?;
    let reply = app.net.reply_alias(app).await?;
    let me = Addr { node: my_node, alias: reply };
    let sid: [u8; 16] = xseed_transport::random();
    let (_route, mut rx) = route(app, sid);
    // 探测
    let nonce: [u8; 16] = xseed_transport::random();
    let tag = probe::tag(&sfp, &nonce, &peer.alias, &me.alias);
    tx.send(env(peer, me, sid, Kind::Probe, to_cbor(&ProbeBody { nonce, tag }))).await?;
    recv(&mut rx, T_PROBE, Kind::ProbeAck).await.map_err(|_| anyhow!("no_ack"))?;
    // 握手
    let (mut s, m1) = Session::initiate(&sfp, &sid)?;
    tx.send(env(peer, me, sid, Kind::Noise, m1)).await?;
    let m2 = recv(&mut rx, T_HANDSHAKE, Kind::Noise).await?;
    let m3 = s.initiator_finish(&m2.body)?;
    tx.send(env(peer, me, sid, Kind::Noise, m3)).await?;
    let cb = s.binding().ok_or_else(|| anyhow!("handshake"))?;
    let close = |tx: mpsc::Sender<Envelope>| async move {
        let _ = tx.send(env(peer, me, sid, Kind::Close, vec![])).await;
    };
    // 询问
    tx.send(env(peer, me, sid, Kind::Noise, s.seal(&Msg::Ask { site: site.into() })?)).await?;
    let out = match recv_msg(&mut s, &mut rx, T_ASK).await? {
        Msg::NotMember => Outcome::NotMember,
        Msg::Chal { torrent_id } => {
            let cached: Option<Vec<u8>> = app.db.lock().query_row("SELECT ih FROM answer_cache WHERE site_ref=?1 AND torrent_id=?2", params![site, torrent_id], |r| r.get(0)).ok();
            let ih = match cached {
                Some(ih) => ih,
                None => {
                    if !take_answer_quota(app, site) {
                        tx.send(env(peer, me, sid, Kind::Noise, s.seal(&Msg::Error { code: ecode::QUOTA, msg: None })?)).await?;
                        close(tx).await;
                        return Ok(Outcome::Failed("answer_quota".into()));
                    }
                    let got = fetch::download(app, site, &torrent_id, Purpose::Challenge, Duration::from_secs(90)).await.ok().and_then(|b| fetch::challenge_ih(&b));
                    let Some(ih) = got else {
                        tx.send(env(peer, me, sid, Kind::Noise, s.seal(&Msg::Error { code: ecode::CHAL_UNAVAILABLE, msg: None })?)).await?;
                        close(tx).await;
                        return Ok(Outcome::Failed("challenge_download".into()));
                    };
                    let _ = app.db.lock().execute(
                        "INSERT OR REPLACE INTO answer_cache(site_ref, torrent_id, ih, fetched_at) VALUES(?1, ?2, ?3, ?4)",
                        params![site, torrent_id, ih, now() as i64],
                    );
                    ih
                }
            };
            app.net.bump(|st| st.answers_today += 1);
            tx.send(env(peer, me, sid, Kind::Noise, s.seal(&Msg::Answer { mac: answer_mac(&cb, site, &torrent_id, &ih) })?)).await?;
            match recv_msg(&mut s, &mut rx, T_ANSWER).await? {
                Msg::Offer { torrent_id, ih1, ih2, size } => {
                    if store_offer(app, &sfp, site, &torrent_id, ih1, ih2, size)? {
                        Outcome::Offer { torrent_id }
                    } else {
                        Outcome::Failed("bad_offer".into())
                    }
                }
                Msg::None => Outcome::None,
                Msg::Error { code, .. } => Outcome::Failed(format!("peer_error_{code}")),
                other => Outcome::Failed(format!("unexpected {other:?}")),
            }
        }
        Msg::Error { code, .. } => Outcome::Failed(format!("peer_error_{code}")),
        other => Outcome::Failed(format!("unexpected {other:?}")),
    };
    close(tx).await;
    Ok(out)
}

/// 记下告知；大小与本机内容不符、info_hash 长度不对时丢弃。
fn store_offer(app: &Shared, sfp: &[u8; 32], site: &str, torrent_id: &str, ih1: Option<Vec<u8>>, ih2: Option<Vec<u8>>, size: u64) -> Result<bool> {
    if ih1.as_ref().is_some_and(|h| h.len() != 20) || ih2.as_ref().is_some_and(|h| h.len() != 32) || (ih1.is_none() && ih2.is_none()) {
        return Ok(false);
    }
    if torrent_id.is_empty() || torrent_id.len() > 32 || !torrent_id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Ok(false);
    }
    let c = app.db.lock();
    let local: Option<i64> = c.query_row("SELECT total_size FROM local_torrents WHERE sfp=?1 AND state='ok' LIMIT 1", [sfp.to_vec()], |r| r.get(0)).ok();
    if local != Some(size as i64) {
        return Ok(false);
    }
    c.execute(
        "INSERT OR IGNORE INTO offers(sfp, site_ref, torrent_id, ih1, ih2, size, received_at, status) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, 'new')",
        params![sfp.to_vec(), site, torrent_id, ih1, ih2, size as i64, now() as i64],
    )?;
    drop(c);
    app.net.bump(|st| st.offers_today += 1);
    app.db.audit("offer", &format!("{site} #{torrent_id}"));
    Ok(true)
}

// ───────── 持有者 ─────────

struct Holder {
    sfp: [u8; 32],
    site_ref: String,
    torrent_id: Option<String>,
    ih1: Option<Vec<u8>>,
    ih2: Option<Vec<u8>>,
    size: u64,
}

fn holder(app: &Shared, alias: &[u8; 16]) -> Option<Holder> {
    let cfg = app.cfg();
    if !cfg.respond || !cfg.setup_done {
        return None;
    }
    let cur = week(now()) as i64;
    app.db
        .lock()
        .query_row(
            "SELECT lt.sfp, lt.site_ref, lt.torrent_id, lt.ih1, lt.ih2, lt.total_size FROM aliases a
             JOIN local_torrents lt ON lt.downloader=a.downloader AND lt.hash=a.hash
             JOIN sites s ON s.site_ref=lt.site_ref AND s.enabled=1
             WHERE a.alias=?1 AND a.purpose='holder' AND a.week>=?2 AND lt.state='ok' AND lt.sfp IS NOT NULL",
            params![alias.to_vec(), cur - xseed_transport::alias::MAX_AGE_WEEKS as i64],
            |r| {
                Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<String>>(2)?, r.get::<_, Option<Vec<u8>>>(3)?, r.get::<_, Option<Vec<u8>>>(4)?, r.get::<_, i64>(5)?))
            },
        )
        .ok()
        .and_then(|(sfp, site_ref, torrent_id, ih1, ih2, size)| Some(Holder { sfp: sfp.try_into().ok()?, site_ref, torrent_id, ih1, ih2, size: size as u64 }))
}

/// 收到未知会话的握手第 1 条消息：作为持有者开一个会话。
pub fn on_handshake(app: &Shared, first: Envelope) {
    if app.net.responders.load(Relaxed) >= MAX_SESSIONS {
        return;
    }
    let Some(h) = holder(app, &first.to.alias) else { return };
    let (route, rx) = route(app, first.sid);
    app.net.responders.fetch_add(1, Relaxed);
    let a = app.clone();
    tokio::spawn(async move {
        let _route = route;
        let r = tokio::time::timeout(T_TOTAL, serve(&a, first, h, rx)).await;
        if let Ok(Err(e)) = r {
            tracing::debug!("会话结束：{e}");
        }
        a.net.responders.fetch_sub(1, Relaxed);
    });
}

async fn serve(app: &Shared, first: Envelope, h: Holder, mut rx: mpsc::Receiver<Envelope>) -> Result<()> {
    let (tx, _) = app.net.sender().ok_or_else(|| anyhow!("offline"))?;
    let (me, peer, sid) = (first.to, first.from, first.sid);
    let (mut s, m2) = Session::respond(&h.sfp, &sid, &first.body)?;
    tx.send(env(peer, me, sid, Kind::Noise, m2)).await?;
    let m3 = recv(&mut rx, T_HANDSHAKE, Kind::Noise).await?;
    s.responder_finish(&m3.body)?;
    let cb = s.binding().ok_or_else(|| anyhow!("handshake"))?;
    let Msg::Ask { site } = recv_msg(&mut s, &mut rx, T_ASK).await? else { return Err(anyhow!("expected ask")) };
    app.net.bump(|st| st.served_today += 1);
    let pick: Option<(String, Vec<u8>)> = app
        .db
        .lock()
        .query_row(
            "SELECT torrent_id, ih FROM challenge_pool WHERE site_ref=?1 AND uses<50 AND fetched_at>?2 ORDER BY RANDOM() LIMIT 1",
            params![site, now() as i64 - 30 * 86_400],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok();
    let Some((tid, expected)) = pick else {
        // 本机不是该站会员，或题库暂时为空（03 §1.6.1）
        tx.send(env(peer, me, sid, Kind::Noise, s.seal(&Msg::NotMember)?)).await?;
        let a = app.clone();
        tokio::spawn(async move { refill(&a, &site).await });
        return Ok(());
    };
    let _ = app.db.lock().execute("UPDATE challenge_pool SET uses=uses+1 WHERE site_ref=?1 AND torrent_id=?2", params![site, tid]);
    tx.send(env(peer, me, sid, Kind::Noise, s.seal(&Msg::Chal { torrent_id: tid.clone() })?)).await?;
    let Msg::Answer { mac } = recv_msg(&mut s, &mut rx, T_ANSWER).await? else { return Err(anyhow!("expected answer")) };
    if !check_answer(&cb, &site, &tid, &expected, &mac) {
        tx.send(env(peer, me, sid, Kind::Noise, s.seal(&Msg::Error { code: ecode::BAD_ANSWER, msg: None })?)).await?;
        return Err(anyhow!("bad_answer"));
    }
    // 答对之后才区分“有”与“没有”（03 §1.6.1）
    let reply = match (&h.torrent_id, h.site_ref == site) {
        (Some(id), true) => {
            app.net.bump(|st| st.offered_today += 1);
            Msg::Offer { torrent_id: id.clone(), ih1: h.ih1.clone(), ih2: h.ih2.clone(), size: h.size }
        }
        _ => Msg::None,
    };
    tx.send(env(peer, me, sid, Kind::Noise, s.seal(&reply)?)).await?;
    let _ = recv(&mut rx, Duration::from_secs(5), Kind::Close).await;
    Ok(())
}

// ───────── 题库 ─────────

/// 补充某站的题库（03 §1.6.2）：每站每周最多 4 次，每次最多新增 4 题，保持 8–16 题。
pub async fn refill(app: &Shared, site: &str) {
    if !app.net.refilling.lock().unwrap().insert(site.to_string()) {
        return;
    }
    if let Err(e) = refill_inner(app, site).await {
        tracing::debug!("补充 {site} 题库：{e}");
    }
    app.net.refilling.lock().unwrap().remove(site);
}

async fn refill_inner(app: &Shared, site: &str) -> Result<()> {
    if !fetch::has_credential(app, site) {
        return Ok(());
    }
    let t = now() as i64;
    let w = week(now());
    let (count, weekly): (i64, String) = {
        let c = app.db.lock();
        c.execute("DELETE FROM challenge_pool WHERE site_ref=?1 AND (uses>=50 OR fetched_at<?2)", params![site, t - 30 * 86_400])?;
        let n = c.query_row("SELECT COUNT(*) FROM challenge_pool WHERE site_ref=?1", [site], |r| r.get(0))?;
        let wk = c.query_row("SELECT value FROM site_traits WHERE site_ref=?1 AND trait='pool_refills'", [site], |r| r.get(0)).unwrap_or_default();
        (n, wk)
    };
    if count >= 8 {
        return Ok(());
    }
    let (wk, done) = weekly.split_once(':').map(|(a, b)| (a.parse::<u16>().unwrap_or(0), b.parse::<u32>().unwrap_or(0))).unwrap_or((0, 0));
    let done = if wk == w { done } else { 0 };
    if done >= 4 {
        return Ok(());
    }
    // 已知的最大数字编号
    let (own, max): (HashSet<i64>, i64) = {
        let c = app.db.lock();
        let mut st = c.prepare("SELECT torrent_id FROM local_torrents WHERE site_ref=?1 AND torrent_id IS NOT NULL")?;
        let own: HashSet<i64> = st.query_map([site], |r| r.get::<_, String>(0))?.filter_map(Result::ok).filter_map(|s| s.parse().ok()).collect();
        let mut ids: Vec<i64> = own.iter().copied().collect();
        for sql in ["SELECT torrent_id FROM answer_cache WHERE site_ref=?1", "SELECT torrent_id FROM offers WHERE site_ref=?1"] {
            let mut st = c.prepare(sql)?;
            ids.extend(st.query_map([site], |r| r.get::<_, String>(0))?.filter_map(Result::ok).filter_map(|s| s.parse::<i64>().ok()));
        }
        (own, ids.into_iter().max().unwrap_or(0))
    };
    if max <= 0 {
        // 非数字编号的站点不出题，等价于不作为持有者
        return Ok(());
    }
    let mut added = 0;
    let mut reached = false;
    let count_attempt = |app: &Shared| {
        // 只有真正访问到站点的补充才计入每周次数；站点连不上不消耗
        let _ = app.db.lock().execute(
            "INSERT OR REPLACE INTO site_traits(site_ref, trait, value, learned_at, expires_at) VALUES(?1, 'pool_refills', ?2, ?3, ?4)",
            params![site, format!("{w}:{}", done + 1), t, t + 14 * 86_400],
        );
    };
    for _ in 0..12 {
        if added >= 4 {
            break;
        }
        let id = rand::thread_rng().gen_range((max - 5000).max(1)..=max);
        if own.contains(&id) {
            continue;
        }
        match fetch::download(app, site, &id.to_string(), Purpose::Pool, Duration::from_secs(600)).await {
            Ok(b) => {
                reached = true;
                if let Some(ih) = fetch::challenge_ih(&b) {
                    app.db.lock().execute("INSERT OR IGNORE INTO challenge_pool(site_ref, torrent_id, ih, fetched_at) VALUES(?1, ?2, ?3, ?4)", params![site, id.to_string(), ih, now() as i64])?;
                    added += 1;
                }
            }
            Err(FetchError::NotFound) | Err(FetchError::NotTorrent) => reached = true,
            Err(e) => {
                if reached {
                    count_attempt(app);
                }
                return Err(anyhow!("{e}"));
            }
        }
    }
    if reached {
        count_attempt(app);
    }
    tracing::info!("{site} 题库新增 {added} 题");
    Ok(())
}

/// 本机作为持有者参与的站点：保持题库不空。
pub async fn keep_pools(app: &Shared) {
    if !app.cfg().respond {
        return;
    }
    let sites: Vec<String> = crate::sites::list(&app.db, &app.vault).into_iter().filter(|s| s.enabled && s.status == "ok").map(|s| s.site_ref).collect();
    for s in sites {
        refill(app, &s).await;
    }
}

// ───────── 询问调度 ─────────

/// 为“社区里有同款”的内容，向本机是会员、但还没有该内容的站点逐个询问（03 §1.5.3）。
pub async fn ask_cycle(app: &Shared) -> Result<()> {
    let Ok(_g) = app.net.asking.try_lock() else { return Ok(()) };
    let cfg = app.cfg();
    let my_sites: Vec<String> = crate::sites::list(&app.db, &app.vault).into_iter().filter(|s| s.enabled && s.status == "ok" && fetch::has_credential(app, &s.site_ref)).map(|s| s.site_ref).collect();
    if my_sites.is_empty() {
        return Ok(());
    }
    let w = week(now()) as i64;
    let t = now() as i64;
    // （内容, 已有的站点, 同款持有者）
    let contents: Vec<([u8; 32], HashSet<String>, Vec<Addr>)> = {
        let c = app.db.lock();
        let mut st = c.prepare("SELECT DISTINCT sfp FROM peers WHERE week >= ?1")?;
        let sfps: Vec<Vec<u8>> = st.query_map([w - 1], |r| r.get(0))?.filter_map(Result::ok).collect();
        let mut out = Vec::new();
        for sfp in sfps {
            let mut st = c.prepare("SELECT site_ref FROM local_torrents WHERE sfp=?1 AND site_ref IS NOT NULL")?;
            let have: HashSet<String> = st.query_map([&sfp], |r| r.get(0))?.filter_map(Result::ok).collect();
            if have.is_empty() {
                continue;
            }
            let mut st = c.prepare("SELECT node, alias FROM peers WHERE sfp=?1 AND week >= ?2")?;
            let peers: Vec<Addr> = st
                .query_map(params![sfp, w - 1], |r| Ok((r.get::<_, u32>(0)?, r.get::<_, Vec<u8>>(1)?)))?
                .filter_map(Result::ok)
                .filter_map(|(node, a)| a.try_into().ok().map(|alias| Addr { node, alias }))
                .collect();
            if let Ok(s) = sfp.try_into() {
                out.push((s, have, peers));
            }
        }
        out
    };
    let mut todo: Vec<([u8; 32], String, Vec<Addr>)> = Vec::new();
    for (sfp, have, peers) in contents {
        for site in &my_sites {
            if have.contains(site) {
                continue;
            }
            let c = app.db.lock();
            let offered = c.query_row("SELECT 1 FROM offers WHERE sfp=?1 AND site_ref=?2", params![sfp.to_vec(), site], |_| Ok(())).is_ok();
            let recent = c.query_row("SELECT 1 FROM asks WHERE sfp=?1 AND site_ref=?2 AND at>?3", params![sfp.to_vec(), site, t - cfg.net.ask_retry_secs as i64], |_| Ok(())).is_ok();
            if !offered && !recent {
                todo.push((sfp, site.clone(), peers.clone()));
            }
        }
    }
    if todo.is_empty() {
        return Ok(());
    }
    todo.shuffle(&mut rand::thread_rng());
    tracing::info!("开始询问：{} 组（内容 × 站点）", todo.len());
    let mut first = true;
    for (sfp, site, mut peers) in todo {
        if !app.net.online() {
            return Ok(());
        }
        peers.shuffle(&mut rand::thread_rng());
        let mut result = "none".to_string();
        for peer in peers.into_iter().take(5) {
            if !first {
                let (lo, hi) = (cfg.net.session_gap_min_secs, cfg.net.session_gap_max_secs.max(cfg.net.session_gap_min_secs));
                let gap = rand::thread_rng().gen_range(lo..=hi);
                tokio::time::sleep(Duration::from_secs(gap)).await;
            }
            first = false;
            app.net.bump(|st| st.sessions_today += 1);
            let out = ask(app, sfp, &site, peer).await;
            match &out {
                Outcome::Failed(why) => tracing::info!("询问 {site}：failed（{why}）"),
                _ => tracing::info!("询问 {site}：{}", out.label()),
            }
            result = out.label().to_string();
            if matches!(out, Outcome::Offer { .. }) {
                break;
            }
        }
        app.db.lock().execute("INSERT OR REPLACE INTO asks(sfp, site_ref, at, result) VALUES(?1, ?2, ?3, ?4)", params![sfp.to_vec(), site, now() as i64, result])?;
    }
    Ok(())
}
