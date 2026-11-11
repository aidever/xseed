//! 辅种网络（03 §1.3、§1.4、§1.5.1、§1.7）：节点清单、入口节点、信箱长连接、
//! 两跳登记与查询、探测。M2 只做到“发现同款”——确认社区里有人持有同样结构的文件；
//! 会员答题与告知种子编号在 M3 实现。

use crate::app::Shared;
use crate::db::Db;
use anyhow::{anyhow, Result};
use rand::seq::SliceRandom;
use rand::Rng;
use serde::Serialize;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;
use tokio::sync::{mpsc, Notify};
use xseed_transport::client::{Event, MailboxCred, NodeClient, NodeError};
use xseed_transport::registry::{NodeManifest, Registry};
use xseed_transport::wire::*;
use xseed_transport::{now, probe, week, PROTO};

/// 每次向入口节点申请代号的上限。
const ALIAS_CHUNK: usize = 4096;
/// 节点健康检查间隔。
const HEALTH_EVERY: u64 = 600;
/// 节点清单刷新间隔。
const REGISTRY_EVERY: u64 = 3600;
/// 探测等待回应的时间（03 §1.5.3 Probing）。
const PROBE_TIMEOUT: u64 = 15;

#[derive(Serialize, Clone, Debug, Default)]
pub struct NodeView {
    pub id: String,
    pub host: String,
    pub rtt_ms: Option<u32>,
    pub ok: bool,
    pub entry: bool,
}

/// 网络状态，供 Web UI 展示。
#[derive(Serialize, Clone, Debug)]
pub struct NetStatus {
    /// off（未完成设置）| no_registry | connecting | online | error
    pub state: &'static str,
    pub error: Option<String>,
    pub entry: Option<NodeView>,
    pub nodes: Vec<NodeView>,
    /// 本周登记的持有者代号数
    pub holders: u64,
    /// 本轮已登记（含副本）的条目数
    pub registered: u64,
    pub last_register_at: Option<u64>,
    pub last_lookup_at: Option<u64>,
    pub probes_today: u64,
    pub acks_today: u64,
    /// 今天回应了多少次别人的探测
    pub answered_today: u64,
    /// 今天发起的询问会话
    pub sessions_today: u64,
    /// 今天作为查询方答题次数
    pub answers_today: u64,
    /// 今天收到的告知（种子编号）
    pub offers_today: u64,
    /// 今天作为持有者接待的询问
    pub served_today: u64,
    /// 今天作为持有者告知了几次
    pub offered_today: u64,
    pub day: u16,
}

impl Default for NetStatus {
    fn default() -> Self {
        NetStatus { state: "off", error: None, entry: None, nodes: vec![], holders: 0, registered: 0, last_register_at: None, last_lookup_at: None, probes_today: 0, acks_today: 0, answered_today: 0, sessions_today: 0, answers_today: 0, offers_today: 0, served_today: 0, offered_today: 0, day: 0 }
    }
}

#[derive(Default)]
struct Health {
    rtts: VecDeque<u32>,
    wk: Option<WellKnown>,
    fails: u32,
    down_until: u64,
}

impl Health {
    fn median(&self) -> Option<u32> {
        let mut v: Vec<u32> = self.rtts.iter().copied().collect();
        v.sort_unstable();
        v.get(v.len() / 2).copied()
    }
}

struct Pending {
    sfp: [u8; 32],
    to: Addr,
    at: u64,
}

#[derive(Default)]
pub struct Net {
    pub status: Mutex<NetStatus>,
    reg: RwLock<Option<Arc<Registry>>>,
    reg_src: Mutex<(String, u64)>,
    health: Mutex<HashMap<String, Health>>,
    last_health: Mutex<u64>,
    /// 当前入口：（节点清单, 信箱凭证, 发送端）
    entry: Mutex<Option<(NodeManifest, MailboxCred, mpsc::Sender<Envelope>)>>,
    pending: Mutex<HashMap<[u8; 16], Pending>>,
    reconnect_fails: Mutex<u32>,
    next_connect_at: Mutex<u64>,
    register_running: tokio::sync::Mutex<()>,
    lookup_running: tokio::sync::Mutex<()>,
    pub kick: Notify,
    /// 进行中的会话：sid → 消息通道
    pub(crate) sessions: Mutex<HashMap<[u8; 16], mpsc::Sender<Envelope>>>,
    pub(crate) responders: std::sync::atomic::AtomicUsize,
    pub(crate) refilling: Mutex<HashSet<String>>,
    pub(crate) asking: tokio::sync::Mutex<()>,
    replies: Mutex<Vec<(u16, [u8; 16])>>,
    last_pools: Mutex<u64>,
}

fn bump(st: &mut NetStatus, f: impl FnOnce(&mut NetStatus)) {
    let d = xseed_transport::day(now());
    if st.day != d {
        st.day = d;
        st.probes_today = 0;
        st.acks_today = 0;
        st.answered_today = 0;
        st.sessions_today = 0;
        st.answers_today = 0;
        st.offers_today = 0;
        st.served_today = 0;
        st.offered_today = 0;
    }
    f(st)
}

impl Net {
    pub fn bump(&self, f: impl FnOnce(&mut NetStatus)) {
        bump(&mut self.status.lock().unwrap(), f)
    }

    /// 当前入口的发送端与 node_tag。
    pub fn sender(&self) -> Option<(mpsc::Sender<Envelope>, u32)> {
        self.entry.lock().unwrap().as_ref().filter(|e| !e.2.is_closed()).map(|e| (e.2.clone(), e.0.tag()))
    }

    /// 取一个新的回信代号（批量向入口节点申请并缓存，只用本周的）。
    pub async fn reply_alias(&self, app: &Shared) -> Result<[u8; 16]> {
        let w = week(now());
        let (entry, cred) = {
            let e = self.entry.lock().unwrap();
            let e = e.as_ref().ok_or_else(|| anyhow!("未连接入口节点"))?;
            (e.0.clone(), e.1.clone())
        };
        {
            let mut r = self.replies.lock().unwrap();
            r.retain(|x| x.0 == w);
            if let Some((_, a)) = r.pop() {
                return Ok(a);
            }
        }
        let got = app.net.client(&entry).aliases(&cred, 32).await?;
        let mut r = self.replies.lock().unwrap();
        r.extend(got.into_iter().map(|a| (w, a)));
        r.pop().map(|x| x.1).ok_or_else(|| anyhow!("没有代号"))
    }

    fn set_state(&self, state: &'static str, error: Option<String>) {
        let mut s = self.status.lock().unwrap();
        s.state = state;
        s.error = error;
    }

    pub fn registry(&self) -> Option<Arc<Registry>> {
        self.reg.read().unwrap().clone()
    }

    fn client(&self, m: &NodeManifest) -> NodeClient {
        NodeClient::new(m.clone(), xseed_transport::client::http_client())
    }

    fn healthy(&self, m: &NodeManifest) -> bool {
        self.health.lock().unwrap().get(&m.id).is_some_and(|h| h.wk.is_some() && h.down_until <= now())
    }

    fn wk(&self, id: &str) -> Option<WellKnown> {
        self.health.lock().unwrap().get(id).and_then(|h| h.wk.clone())
    }

    fn entry_tag(&self) -> Option<u32> {
        self.entry.lock().unwrap().as_ref().map(|e| e.0.tag())
    }

    pub fn online(&self) -> bool {
        self.status.lock().unwrap().state == "online"
    }

    /// 本地已知的发现数（本机多少个种子在社区里找到了同款）。
    pub fn discovered(db: &Db) -> (u64, u64) {
        let w = week(now()) as i64;
        let c = db.lock();
        let torrents = c
            .query_row("SELECT COUNT(*) FROM local_torrents WHERE state='ok' AND sfp IN (SELECT sfp FROM peers WHERE week >= ?1)", [w - 1], |r| r.get::<_, i64>(0))
            .unwrap_or(0);
        let peers = c.query_row("SELECT COUNT(DISTINCT alias) FROM peers WHERE week >= ?1", [w - 1], |r| r.get::<_, i64>(0)).unwrap_or(0);
        (torrents as u64, peers as u64)
    }

    fn update_views(&self) {
        let reg = self.registry();
        let entry = self.entry.lock().unwrap().as_ref().map(|e| e.0.id.clone());
        let h = self.health.lock().unwrap();
        let views: Vec<NodeView> = reg
            .iter()
            .flat_map(|r| r.nodes.iter())
            .map(|m| {
                let x = h.get(&m.id);
                NodeView { id: m.id.clone(), host: m.host(), rtt_ms: x.and_then(Health::median), ok: x.is_some_and(|x| x.wk.is_some() && x.down_until <= now()), entry: entry.as_deref() == Some(&m.id) }
            })
            .collect();
        drop(h);
        let mut s = self.status.lock().unwrap();
        s.entry = views.iter().find(|v| v.entry).cloned();
        s.nodes = views;
    }
}

// ───────── 主循环 ─────────

/// 网络后台任务：每分钟检查一次（配置变化时立即检查）。
/// 读取并验证节点清单：经网络获取时必须带受信任公钥的签名；版本不得低于上次接受的版本（防回退）；
/// 生成超过 30 天时告警但继续使用。通过后应用其中的站点定义与公共 tracker。
async fn load_registry(app: &Shared, src: &str, extra_keys: &[String]) -> Result<Registry> {
    use xseed_transport::registry;
    let l = registry::load_signed(src, &xseed_transport::client::http_client(), &registry::trusted_keys(extra_keys)).await?;
    let r = l.registry;
    let key = format!("registry_version:{src}");
    let last: u64 = app.db.meta_get(&key).and_then(|v| v.parse().ok()).unwrap_or(0);
    if r.version < last {
        anyhow::bail!("节点清单版本 {} 低于上次的 {}，拒绝回退", r.version, last);
    }
    if r.generated_at > 0 && now().saturating_sub(r.generated_at) > 30 * 86_400 {
        tracing::warn!("节点清单已超过 30 天没有更新（版本 {}），请确认注册中心地址是否正确", r.version);
    }
    app.db.meta_set(&key, &r.version.to_string())?;
    if l.signed_by.is_some() || !r.sites.is_empty() || !r.public_trackers.is_empty() {
        crate::sites::apply_registry(&app.db, &r.sites, &r.public_trackers)?;
    }
    tracing::debug!("节点清单版本 {}（{}）", r.version, l.signed_by.as_deref().unwrap_or("本地未签名"));
    Ok(r)
}

pub async fn run(app: Shared) {
    loop {
        if let Err(e) = tick(&app).await {
            tracing::warn!("辅种网络：{e:#}");
            app.net.set_state(if app.net.online() { "online" } else { "error" }, Some(e.to_string()));
        }
        app.net.update_views();
        expire_pending(&app);
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(60)) => {}
            _ = app.net.kick.notified() => {}
        }
    }
}

async fn tick(app: &Shared) -> Result<()> {
    let cfg = app.cfg();
    let net = &app.net;
    if !cfg.setup_done {
        disconnect(app);
        net.set_state("off", None);
        return Ok(());
    }
    // 配置为空时使用官方注册中心
    let src = match cfg.registry.trim() {
        "" => xseed_transport::registry::OFFICIAL_REGISTRY.to_string(),
        s => s.to_string(),
    };
    if src.is_empty() {
        disconnect(app);
        *net.reg.write().unwrap() = None;
        net.set_state("no_registry", None);
        return Ok(());
    }
    // 节点清单
    let stale = {
        let r = net.reg_src.lock().unwrap();
        r.0 != src || now().saturating_sub(r.1) >= REGISTRY_EVERY
    };
    if stale {
        match load_registry(app, &src, &cfg.registry_keys).await {
            Ok(r) => {
                *net.reg.write().unwrap() = Some(Arc::new(r));
                *net.reg_src.lock().unwrap() = (src.clone(), now());
                *net.last_health.lock().unwrap() = 0;
            }
            Err(e) if net.registry().is_none() || net.reg_src.lock().unwrap().0 != src => {
                net.set_state("error", Some(format!("读取节点清单失败：{e}")));
                return Ok(());
            }
            Err(e) => tracing::warn!("刷新节点清单失败，继续使用上次的清单：{e:#}"),
        }
    }
    let reg = net.registry().ok_or_else(|| anyhow!("没有节点清单"))?;
    if reg.nodes.is_empty() {
        net.set_state("error", Some("节点清单为空".into()));
        return Ok(());
    }
    // 健康检查
    if now().saturating_sub(*net.last_health.lock().unwrap()) >= HEALTH_EVERY {
        health_check(net, &reg).await;
        *net.last_health.lock().unwrap() = now();
    }
    // 入口节点
    let want = choose_entry(net, &reg, &cfg.pin_entry);
    let cur = net.entry.lock().unwrap().as_ref().map(|e| (e.0.id.clone(), e.2.is_closed()));
    let need = match (&cur, &want) {
        (_, None) => {
            disconnect(app);
            net.set_state("error", Some("没有可用的社区节点".into()));
            return Ok(());
        }
        (None, Some(_)) => true,
        (Some((id, closed)), Some(w)) => *closed || *id != w.id,
    };
    if !need && net.status.lock().unwrap().state != "online" {
        // 入口连接一直正常，之前的错误（例如清单暂时读取失败）已经恢复
        net.set_state("online", None);
    }
    if need {
        if now() < *net.next_connect_at.lock().unwrap() {
            return Ok(());
        }
        disconnect(app);
        net.set_state("connecting", None);
        let w = want.unwrap();
        if let Err(e) = connect(app, &w).await {
            let mut f = net.reconnect_fails.lock().unwrap();
            *f += 1;
            // 随机退避，避免节点重启后大量小助手同时重连（03 §7.1）
            let cap = 2u64.saturating_pow(*f).min(120);
            *net.next_connect_at.lock().unwrap() = now() + rand::thread_rng().gen_range(1..=cap.max(1));
            if let Some(h) = net.health.lock().unwrap().get_mut(&w.id) {
                h.fails += 1;
                if h.fails >= 3 {
                    h.down_until = now() + 3600;
                    h.fails = 0;
                }
            }
            return Err(anyhow!("连接入口节点 {} 失败：{e}", w.host()));
        }
        *net.reconnect_fails.lock().unwrap() = 0;
        net.set_state("online", None);
        net.update_views();
    }
    // 登记与查询在后台进行，不阻塞主循环
    let a = app.clone();
    tokio::spawn(async move {
        if let Err(e) = register_cycle(&a).await {
            tracing::warn!("登记失败：{e:#}");
        }
    });
    let a = app.clone();
    tokio::spawn(async move {
        if let Err(e) = lookup_cycle(&a).await {
            tracing::warn!("查询失败：{e:#}");
        }
        if let Err(e) = crate::session::ask_cycle(&a).await {
            tracing::warn!("询问失败：{e:#}");
        }
    });
    let pools_due = now().saturating_sub(*net.last_pools.lock().unwrap()) >= 3600;
    if pools_due {
        *net.last_pools.lock().unwrap() = now();
        let a = app.clone();
        tokio::spawn(async move { crate::session::keep_pools(&a).await });
    }
    Ok(())
}

async fn health_check(net: &Net, reg: &Registry) {
    let checks = reg.nodes.iter().map(|m| {
        let c = net.client(m);
        let id = m.id.clone();
        async move { (id, c.well_known().await) }
    });
    for (id, r) in futures_join(checks).await {
        let mut h = net.health.lock().unwrap();
        let e = h.entry(id).or_default();
        match r {
            Ok((wk, rtt)) => {
                e.rtts.push_back(rtt.as_millis().min(u32::MAX as u128) as u32);
                while e.rtts.len() > 3 {
                    e.rtts.pop_front();
                }
                e.wk = Some(wk);
            }
            Err(_) => {
                e.wk = None;
                e.fails += 1;
                if e.fails >= 3 {
                    e.down_until = now() + 3600;
                    e.fails = 0;
                }
            }
        }
    }
}

async fn futures_join<F>(fs: impl IntoIterator<Item = F>) -> Vec<F::Output>
where
    F: std::future::Future + Send + 'static,
    F::Output: Send + 'static,
{
    let hs: Vec<_> = fs.into_iter().map(tokio::spawn).collect();
    let mut out = Vec::new();
    for h in hs {
        if let Ok(v) = h.await {
            out.push(v);
        }
    }
    out
}

/// 入口节点：固定的优先；否则取 RTT 中位数最小的健康节点（03 §1.3）。
fn choose_entry(net: &Net, reg: &Registry, pin: &str) -> Option<NodeManifest> {
    if !pin.is_empty() {
        if let Some(m) = reg.nodes.iter().find(|m| m.id == pin) {
            if net.healthy(m) {
                return Some(m.clone());
            }
        }
    }
    let h = net.health.lock().unwrap();
    let cur = net.entry.lock().unwrap().as_ref().map(|e| e.0.id.clone());
    let mut best: Vec<(&NodeManifest, u32)> = reg.nodes.iter().filter_map(|m| h.get(&m.id).filter(|x| x.wk.is_some() && x.down_until <= now()).and_then(Health::median).map(|r| (m, r))).collect();
    best.sort_by_key(|x| x.1);
    let first = best.first()?;
    // 当前入口仍健康且延迟没有明显更差时保持不变，避免来回切换
    if let Some(c) = cur.and_then(|id| best.iter().find(|x| x.0.id == id)) {
        if c.1 <= first.1.saturating_mul(2).saturating_add(20) {
            return Some(c.0.clone());
        }
    }
    Some(first.0.clone())
}

fn disconnect(app: &Shared) {
    app.net.replies.lock().unwrap().clear();
    if app.net.entry.lock().unwrap().take().is_some() {
        tracing::info!("已断开入口节点");
    }
}

fn cred_name(m: &NodeManifest) -> String {
    format!("mailbox/{}", m.id)
}

/// 取得（必要时创建）某节点上的信箱凭证。
async fn mailbox_cred(app: &Shared, m: &NodeManifest, fresh: bool) -> Result<MailboxCred> {
    if !fresh {
        if let Some(c) = app.vault.secret(&cred_name(m)).and_then(|s| serde_json::from_str(&s).ok()) {
            return Ok(c);
        }
    }
    let c = app.net.client(m);
    let wk = match app.net.wk(&m.id) {
        Some(w) => w,
        None => c.well_known().await?.0,
    };
    let cred = c.create_mailbox(&wk).await?;
    app.vault.set_secret(&cred_name(m), &serde_json::to_string(&cred)?)?;
    app.db.audit("mailbox_created", &m.host());
    Ok(cred)
}

fn is_auth_failed(e: &anyhow::Error) -> bool {
    e.downcast_ref::<NodeError>().is_some_and(|n| n.code == code::AUTH_FAILED)
}

async fn connect(app: &Shared, m: &NodeManifest) -> Result<()> {
    let c = app.net.client(m);
    let mut cred = mailbox_cred(app, m, false).await?;
    let mb = match c.connect(&cred).await {
        Ok(mb) => mb,
        Err(e) if is_auth_failed(&e) => {
            // 节点回收了信箱（长期未使用）或节点重置：重新开一个
            cred = mailbox_cred(app, m, true).await?;
            c.connect(&cred).await?
        }
        Err(e) => return Err(e),
    };
    let tx = mb.tx.clone();
    *app.net.entry.lock().unwrap() = Some((m.clone(), cred, tx.clone()));
    tracing::info!("已连接入口节点 {}", m.host());
    let a = app.clone();
    // 读取任务只持有弱引用：断开或切换入口时丢掉 entry 中的发送端，旧连接随之关闭
    tokio::spawn(reader(a, mb.rx, tx.downgrade()));
    Ok(())
}

async fn reader(app: Shared, mut rx: mpsc::Receiver<Event>, tx: mpsc::WeakSender<Envelope>) {
    while let Some(ev) = rx.recv().await {
        match ev {
            Event::Env(env) if app.net.sessions.lock().unwrap().contains_key(&env.sid) => {
                let ch = app.net.sessions.lock().unwrap().get(&env.sid).cloned();
                if let Some(ch) = ch {
                    let _ = ch.try_send(env);
                }
            }
            Event::Env(env) => match env.kind {
                Kind::Probe => {
                    if let (Some(ack), Some(tx)) = (answer_probe(&app, &env), tx.upgrade()) {
                        let _ = tx.send(ack).await;
                        bump(&mut app.net.status.lock().unwrap(), |s| s.answered_today += 1);
                    }
                }
                Kind::ProbeAck => on_ack(&app, &env),
                // 未知会话的握手第 1 条：作为持有者接待
                Kind::Noise => crate::session::on_handshake(&app, env),
                Kind::Close => {}
            },
            Event::Nack { sid, code } => {
                // 会话中的信封未送达：关闭该会话的通道，会话随即超时结束
                app.net.sessions.lock().unwrap().remove(&sid);
                if app.net.pending.lock().unwrap().remove(&sid).is_some() {
                    tracing::debug!("探测未送达：{code}");
                }
            }
            Event::Closed(reason) => {
                // 主动断开或已切换到新入口时，旧连接的关闭不影响状态
                let current = app.net.entry.lock().unwrap().as_ref().is_some_and(|e| !e.2.is_closed());
                if !current {
                    tracing::info!("入口节点连接断开：{reason}");
                    app.net.set_state("connecting", Some(reason));
                    app.net.kick.notify_one();
                }
                break;
            }
        }
    }
}

// ───────── 持有者：回应探测 ─────────

/// 校验探测标签；本机持有该内容且允许回答时返回回应信封（03 §1.5.1）。
pub fn answer_probe(app: &Shared, env: &Envelope) -> Option<Envelope> {
    let cfg = app.cfg();
    if !cfg.respond || !cfg.setup_done {
        return None;
    }
    let body: ProbeBody = from_cbor(&env.body).ok()?;
    let cur = week(now()) as i64;
    let sfp: Vec<u8> = app
        .db
        .lock()
        .query_row(
            "SELECT lt.sfp FROM aliases a JOIN local_torrents lt ON lt.downloader=a.downloader AND lt.hash=a.hash
             JOIN sites s ON s.site_ref=lt.site_ref AND s.enabled=1
             WHERE a.alias=?1 AND a.purpose='holder' AND a.week>=?2 AND lt.state='ok' AND lt.sfp IS NOT NULL",
            rusqlite::params![env.to.alias.to_vec(), cur - xseed_transport::alias::MAX_AGE_WEEKS as i64],
            |r| r.get(0),
        )
        .ok()?;
    let sfp: [u8; 32] = sfp.try_into().ok()?;
    if !probe::verify(&sfp, &body.nonce, &env.to.alias, &env.from.alias, &body.tag) {
        return None;
    }
    Some(Envelope { v: PROTO, to: env.from, from: env.to, sid: env.sid, kind: Kind::ProbeAck, body: vec![] })
}

// ───────── 查询方：探测回应 ─────────

fn on_ack(app: &Shared, env: &Envelope) {
    let Some(p) = app.net.pending.lock().unwrap().remove(&env.sid) else { return };
    if env.from != p.to {
        return;
    }
    let _ = app.db.lock().execute(
        "INSERT INTO peers (sfp, node, alias, week, acked_at) VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT (sfp, alias) DO UPDATE SET week=excluded.week, acked_at=excluded.acked_at",
        rusqlite::params![p.sfp.to_vec(), p.to.node, p.to.alias.to_vec(), week(now()), now() as i64],
    );
    bump(&mut app.net.status.lock().unwrap(), |s| s.acks_today += 1);
}

fn expire_pending(app: &Shared) {
    let t = now();
    app.net.pending.lock().unwrap().retain(|_, p| t.saturating_sub(p.at) < PROBE_TIMEOUT * 4);
}

// ───────── 两跳请求 ─────────

/// 经入口节点把请求两跳发给 `target`。目标恰好是入口时，改经另一个节点转发，
/// 避免同一节点同时看到 IP 与桶号（03 §1.4.2）。
async fn two_hop(app: &Shared, target: &NodeManifest, req: &GwReq) -> Result<GwResp> {
    let (entry, cred) = {
        let e = app.net.entry.lock().unwrap();
        let e = e.as_ref().ok_or_else(|| anyhow!("未连接入口节点"))?;
        (e.0.clone(), e.1.clone())
    };
    let (relay, cred) = if entry.id == target.id {
        let reg = app.net.registry().ok_or_else(|| anyhow!("没有节点清单"))?;
        let alt = {
            let h = app.net.health.lock().unwrap();
            let mut v: Vec<(&NodeManifest, u32)> = reg.nodes.iter().filter(|m| m.id != entry.id).filter_map(|m| h.get(&m.id).filter(|x| x.wk.is_some() && x.down_until <= now()).and_then(Health::median).map(|r| (m, r))).collect();
            v.sort_by_key(|x| x.1);
            v.first().map(|x| x.0.clone())
        };
        match alt {
            Some(m) => {
                let c = mailbox_cred(app, &m, false).await?;
                (m, c)
            }
            // 只有一个节点时只能直接发给它
            None => (entry, cred),
        }
    } else {
        (entry, cred)
    };
    let c = app.net.client(&relay);
    match c.ohttp(&cred, target, req).await {
        Err(e) if is_auth_failed(&e) && relay.id != app.net.entry.lock().unwrap().as_ref().map(|e| e.0.id.clone()).unwrap_or_default() => {
            let cred = mailbox_cred(app, &relay, true).await?;
            c.ohttp(&cred, target, req).await
        }
        r => r,
    }
}

// ───────── 登记 ─────────

/// 本周的持有者代号：每个可回答的本机种子一个（03 §1.4.1）。返回 (alias, bucket)。
async fn holder_aliases(app: &Shared) -> Result<Vec<([u8; 16], u16)>> {
    let (entry, cred) = {
        let e = app.net.entry.lock().unwrap();
        let e = e.as_ref().ok_or_else(|| anyhow!("未连接入口节点"))?;
        (e.0.clone(), e.1.clone())
    };
    let tag = entry.tag();
    let w = week(now());
    crate::sites::list(&app.db, &app.vault); // 确保每个站点都有开关记录
    let holders: Vec<(String, String, u16)> = {
        let c = app.db.lock();
        let mut st = c.prepare(
            "SELECT lt.downloader, lt.hash, lt.bucket FROM local_torrents lt JOIN sites s ON s.site_ref=lt.site_ref AND s.enabled=1
             WHERE lt.state='ok' AND lt.torrent_id IS NOT NULL AND lt.sfp IS NOT NULL",
        )?;
        let v = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get::<_, i64>(2)? as u16)))?.filter_map(Result::ok).collect();
        v
    };
    let have: HashMap<(String, String), [u8; 16]> = {
        let c = app.db.lock();
        let mut st = c.prepare("SELECT downloader, hash, alias FROM aliases WHERE purpose='holder' AND week=?1 AND node_tag=?2")?;
        let v = st
            .query_map(rusqlite::params![w, tag], |r| Ok(((r.get(0)?, r.get(1)?), r.get::<_, Vec<u8>>(2)?)))?
            .filter_map(Result::ok)
            .filter_map(|(k, a)| a.try_into().ok().map(|a| (k, a)))
            .collect();
        v
    };
    let missing: Vec<&(String, String, u16)> = holders.iter().filter(|h| !have.contains_key(&(h.0.clone(), h.1.clone()))).collect();
    if !missing.is_empty() {
        let client = app.net.client(&entry);
        for chunk in missing.chunks(ALIAS_CHUNK) {
            let aliases = client.aliases(&cred, chunk.len() as u16).await?;
            let c = app.db.lock();
            for (h, a) in chunk.iter().zip(aliases) {
                c.execute("INSERT OR REPLACE INTO aliases (alias, node_tag, purpose, downloader, hash, week, used) VALUES (?1, ?2, 'holder', ?3, ?4, ?5, 0)", rusqlite::params![a.to_vec(), tag, h.0, h.1, w])?;
            }
        }
    }
    // 清理过期代号与过期的同款记录
    {
        let c = app.db.lock();
        c.execute("DELETE FROM aliases WHERE week < ?1", [w as i64 - xseed_transport::alias::MAX_AGE_WEEKS as i64])?;
        c.execute("DELETE FROM peers WHERE week < ?1", [w as i64 - 1])?;
    }
    let c = app.db.lock();
    let mut st = c.prepare(
        "SELECT a.alias, lt.bucket FROM aliases a JOIN local_torrents lt ON lt.downloader=a.downloader AND lt.hash=a.hash
         JOIN sites s ON s.site_ref=lt.site_ref AND s.enabled=1
         WHERE a.purpose='holder' AND a.week=?1 AND a.node_tag=?2 AND lt.state='ok' AND lt.torrent_id IS NOT NULL",
    )?;
    let v = st
        .query_map(rusqlite::params![w, tag], |r| Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, i64>(1)? as u16)))?
        .filter_map(Result::ok)
        .filter_map(|(a, b)| a.try_into().ok().map(|a| (a, b)))
        .collect();
    Ok(v)
}

/// 每天一轮：把全部（桶，代号）按 HRW 分组，每批 ≤ 64 条、桶号打乱，分散在一段时间内随机发出（03 §1.4.3）。
async fn register_cycle(app: &Shared) -> Result<()> {
    let Ok(_g) = app.net.register_running.try_lock() else { return Ok(()) };
    let cfg = app.cfg();
    if !cfg.respond {
        app.net.status.lock().unwrap().holders = 0;
        return Ok(());
    }
    let w = week(now());
    let last_at: u64 = app.db.meta_get("net_register_at").and_then(|s| s.parse().ok()).unwrap_or(0);
    let last_week: u16 = app.db.meta_get("net_register_week").and_then(|s| s.parse().ok()).unwrap_or(0);
    let last_entry = app.db.meta_get("net_register_entry").unwrap_or_default();
    let entry_id = app.net.entry.lock().unwrap().as_ref().map(|e| e.0.id.clone()).unwrap_or_default();
    let due = now().saturating_sub(last_at) >= 20 * 3600 || last_week != w || last_entry != entry_id;
    let holders = holder_aliases(app).await?;
    app.net.status.lock().unwrap().holders = holders.len() as u64;
    if !due || holders.is_empty() {
        return Ok(());
    }
    let reg = app.net.registry().ok_or_else(|| anyhow!("没有节点清单"))?;
    let tag = app.net.entry_tag().ok_or_else(|| anyhow!("未连接入口节点"))?;
    let t = now();
    let mut by_owner: BTreeMap<String, Vec<RegEntry>> = BTreeMap::new();
    for (alias, b) in &holders {
        for o in reg.owners(*b, t) {
            by_owner.entry(o.id.clone()).or_default().push(RegEntry { b: *b, node: tag, alias: *alias });
        }
    }
    let first = last_at == 0;
    let spread = if first { cfg.net.first_register_spread_secs } else { cfg.net.register_spread_secs };
    let mut batches: Vec<(u64, String, Vec<RegEntry>)> = Vec::new();
    {
        let mut rng = rand::thread_rng();
        for (owner, mut entries) in by_owner {
            entries.shuffle(&mut rng);
            for chunk in entries.chunks(MAX_REGISTER) {
                batches.push((rng.gen_range(0..=spread.max(1)), owner.clone(), chunk.to_vec()));
            }
        }
    }
    batches.sort_by_key(|b| b.0);
    app.db.meta_set("net_register_at", &t.to_string())?;
    app.db.meta_set("net_register_week", &w.to_string())?;
    app.db.meta_set("net_register_entry", &entry_id)?;
    app.net.status.lock().unwrap().registered = 0;
    tracing::info!("开始登记：{} 个持有者代号，{} 批，分散在 {} 秒内", holders.len(), batches.len(), spread);
    let start = tokio::time::Instant::now();
    let mut ok = 0u64;
    for (at, owner, entries) in batches {
        tokio::time::sleep_until(start + Duration::from_secs(at)).await;
        if !app.net.online() {
            // 断线：下次连上后重新开始一轮
            let _ = app.db.meta_set("net_register_at", "0");
            return Ok(());
        }
        let Some(target) = reg.nodes.iter().find(|m| m.id == owner) else { continue };
        match register_batch(app, target, &entries).await {
            Ok(n) => {
                ok += n as u64;
                let mut s = app.net.status.lock().unwrap();
                s.registered = ok;
                s.last_register_at = Some(now());
            }
            Err(e) => tracing::warn!("向 {} 登记失败：{e:#}", target.host()),
        }
    }
    Ok(())
}

async fn register_batch(app: &Shared, target: &NodeManifest, entries: &[RegEntry]) -> Result<u32> {
    for attempt in 0..2 {
        let wk = match (attempt, app.net.wk(&target.id)) {
            (0, Some(w)) => w,
            _ => {
                let w = app.net.client(target).well_known().await?.0;
                if let Some(h) = app.net.health.lock().unwrap().get_mut(&target.id) {
                    h.wk = Some(w.clone());
                }
                w
            }
        };
        let pow = xseed_transport::client::solve_pow(&wk.pow_salt, wk.register_bits).await?;
        match two_hop(app, target, &GwReq::Register { entries: entries.to_vec(), ttl_d: 3, pow }).await {
            Ok(GwResp::Registered { n }) => return Ok(n),
            Ok(other) => return Err(anyhow!("意外的响应 {other:?}")),
            Err(e) if attempt == 0 && e.downcast_ref::<NodeError>().is_some_and(|n| n.code == code::POW_INVALID) => continue,
            Err(e) => return Err(e),
        }
    }
    Err(anyhow!("工作量证明一直无效"))
}

// ───────── 查询与探测 ─────────

/// 每天一轮：查询本机内容所在的桶，探测候选持有者，确认谁有同款（03 §1.5.1）。
async fn lookup_cycle(app: &Shared) -> Result<()> {
    let Ok(_g) = app.net.lookup_running.try_lock() else { return Ok(()) };
    let cfg = app.cfg();
    let t = now();
    let reg = app.net.registry().ok_or_else(|| anyhow!("没有节点清单"))?;
    // 需要查询的内容（按 SFP 去重）
    let todo: Vec<([u8; 32], u16)> = {
        let c = app.db.lock();
        let mut st = c.prepare(
            "SELECT DISTINCT lt.sfp, lt.bucket FROM local_torrents lt LEFT JOIN lookups l ON l.sfp = lt.sfp
             WHERE lt.state='ok' AND lt.sfp IS NOT NULL AND (l.at IS NULL OR l.at < ?1)",
        )?;
        let v = st
            .query_map([t.saturating_sub(cfg.net.lookup_every_secs) as i64], |r| Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, i64>(1)? as u16)))?
            .filter_map(Result::ok)
            .filter_map(|(s, b)| s.try_into().ok().map(|s| (s, b)))
            .collect();
        v
    };
    if todo.is_empty() {
        return Ok(());
    }
    let own: HashSet<[u8; 16]> = {
        let c = app.db.lock();
        let mut st = c.prepare("SELECT alias FROM aliases")?;
        let v = st.query_map([], |r| r.get::<_, Vec<u8>>(0))?.filter_map(Result::ok).filter_map(|a| a.try_into().ok()).collect();
        v
    };
    let mut by_bucket: BTreeMap<u16, Vec<[u8; 32]>> = BTreeMap::new();
    for (s, b) in &todo {
        by_bucket.entry(*b).or_default().push(*s);
    }
    // 按首选所有者分组，每次 ≤ 16 个桶
    let mut by_owner: BTreeMap<String, Vec<u16>> = BTreeMap::new();
    for b in by_bucket.keys() {
        if let Some(o) = reg.owners(*b, t).first() {
            by_owner.entry(o.id.clone()).or_default().push(*b);
        }
    }
    tracing::info!("开始查询：{} 个内容，{} 个桶", todo.len(), by_bucket.len());
    let mut probes: Vec<([u8; 32], Addr)> = Vec::new();
    for (owner, buckets) in by_owner {
        let Some(target) = reg.nodes.iter().find(|m| m.id == owner) else { continue };
        for chunk in buckets.chunks(MAX_LOOKUP) {
            if !app.net.online() {
                return Ok(());
            }
            let resp = match two_hop(app, target, &GwReq::Lookup { buckets: chunk.to_vec() }).await {
                Ok(r) => r,
                Err(e) => {
                    // 首选所有者不可用时问第二所有者
                    let alt = reg.owners(chunk[0], t).get(1).map(|m| (*m).clone());
                    match alt {
                        Some(m) => two_hop(app, &m, &GwReq::Lookup { buckets: chunk.to_vec() }).await.map_err(|e2| anyhow!("{e}; {e2}"))?,
                        None => return Err(e),
                    }
                }
            };
            let GwResp::Found { buckets: hits } = resp else { continue };
            let c = app.db.lock();
            for bh in hits {
                let cands: Vec<Addr> = bh.hits.into_iter().filter(|a| !own.contains(&a.alias)).collect();
                for sfp in by_bucket.get(&bh.b).into_iter().flatten() {
                    c.execute("INSERT OR REPLACE INTO lookups (sfp, at, hits) VALUES (?1, ?2, ?3)", rusqlite::params![sfp.to_vec(), now() as i64, cands.len() as i64])?;
                    let mut cs = cands.clone();
                    cs.shuffle(&mut rand::thread_rng());
                    probes.extend(cs.into_iter().take(cfg.net.probes_per_content).map(|a| (*sfp, a)));
                }
            }
        }
    }
    app.net.status.lock().unwrap().last_lookup_at = Some(now());
    if probes.is_empty() {
        return Ok(());
    }
    probes.shuffle(&mut rand::thread_rng());
    send_probes(app, probes, cfg.net.probe_gap_ms).await
}

async fn send_probes(app: &Shared, probes: Vec<([u8; 32], Addr)>, gap_ms: u64) -> Result<()> {
    let (entry, cred, tx) = {
        let e = app.net.entry.lock().unwrap();
        let e = e.as_ref().ok_or_else(|| anyhow!("未连接入口节点"))?;
        (e.0.clone(), e.1.clone(), e.2.clone())
    };
    let from_node = entry.tag();
    let client = app.net.client(&entry);
    tracing::info!("开始探测 {} 个候选持有者", probes.len());
    for chunk in probes.chunks(ALIAS_CHUNK) {
        // 每次探测用一个新的回信代号（03 §1.6.5）
        let replies = client.aliases(&cred, chunk.len() as u16).await?;
        for ((sfp, to), reply) in chunk.iter().zip(replies) {
            if tx.is_closed() {
                return Ok(());
            }
            let nonce: [u8; 16] = xseed_transport::random();
            let sid: [u8; 16] = xseed_transport::random();
            let from = Addr { node: from_node, alias: reply };
            let tag = probe::tag(sfp, &nonce, &to.alias, &from.alias);
            app.net.pending.lock().unwrap().insert(sid, Pending { sfp: *sfp, to: *to, at: now() });
            let env = Envelope { v: PROTO, to: *to, from, sid, kind: Kind::Probe, body: to_cbor(&ProbeBody { nonce, tag }) };
            if tx.send(env).await.is_err() {
                return Ok(());
            }
            bump(&mut app.net.status.lock().unwrap(), |s| s.probes_today += 1);
            tokio::time::sleep(Duration::from_millis(gap_ms)).await;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_only_matching_probe() {
        let dir = tempfile::tempdir().unwrap();
        let app = crate::app::App::open(dir.path().to_path_buf()).unwrap();
        app.update_cfg(|c| c.setup_done = true).unwrap();
        let sfp = [5u8; 32];
        let holder = [1u8; 16];
        {
            let c = app.db.lock();
            c.execute(
                "INSERT INTO local_torrents (downloader, hash, name, root_name, single_file, save_path, total_size, sfp, bucket, site_ref, hosts, torrent_id, state, first_seen, last_seen)
                 VALUES ('main', 'h1', 'n', 'n', 0, '/d', 1, ?1, 7, 'host:x.example', '[]', '1', 'ok', 0, 0)",
                [sfp.to_vec()],
            )
            .unwrap();
            c.execute("INSERT INTO sites (site_ref, enabled) VALUES ('host:x.example', 1)", []).unwrap();
            c.execute("INSERT INTO aliases (alias, node_tag, purpose, downloader, hash, week) VALUES (?1, 1, 'holder', 'main', 'h1', ?2)", rusqlite::params![holder.to_vec(), week(now())]).unwrap();
        }
        let from = Addr { node: 2, alias: [2; 16] };
        let to = Addr { node: 1, alias: holder };
        let nonce = [3u8; 16];
        let mk = |s: &[u8; 32]| Envelope { v: PROTO, to, from, sid: [4; 16], kind: Kind::Probe, body: to_cbor(&ProbeBody { nonce, tag: probe::tag(s, &nonce, &to.alias, &from.alias) }) };
        let ack = answer_probe(&app, &mk(&sfp)).expect("同款应回应");
        assert_eq!((ack.to, ack.from, ack.kind, ack.sid), (from, to, Kind::ProbeAck, [4; 16]));
        assert!(answer_probe(&app, &mk(&[6; 32])).is_none(), "不同内容不回应");
        app.db.lock().execute("UPDATE sites SET enabled=0", []).unwrap();
        assert!(answer_probe(&app, &mk(&sfp)).is_none(), "站点关闭后不回应");
        app.db.lock().execute("UPDATE sites SET enabled=1", []).unwrap();
        app.update_cfg(|c| c.respond = false).unwrap();
        assert!(answer_probe(&app, &mk(&sfp)).is_none(), "关闭“帮别人找到辅种”后不回应");
    }
}
