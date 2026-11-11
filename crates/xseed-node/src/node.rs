//! 节点状态与业务逻辑：信箱、代号、桶索引、投递与两跳网关。

use crate::config::Config;
use crate::limit::Limiter;
use anyhow::{Context, Result};
use ed25519_dalek::SigningKey;
use rand::seq::SliceRandom;
use serde::{Deserialize, Serialize};
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex, RwLock};
use tokio::sync::mpsc;
use xseed_transport::alias::AliasKey;
use xseed_transport::registry::{self, NodeManifest, Registry};
use xseed_transport::wire::{code, *};
use xseed_transport::{auth, day, now, ohttp, week};

/// 节点密钥（`node.key`，权限 600）。各项为 32/16 字节随机种子的十六进制。
#[derive(Serialize, Deserialize)]
pub struct Keys {
    pub sign: String,
    pub hpke: String,
    pub alias: String,
    pub mailbox: String,
}

impl Keys {
    pub fn generate() -> Self {
        Keys { sign: hex::encode(xseed_transport::random::<32>()), hpke: hex::encode(xseed_transport::random::<32>()), alias: hex::encode(xseed_transport::random::<16>()), mailbox: hex::encode(xseed_transport::random::<32>()) }
    }

    pub fn load_or_create(dir: &Path) -> Result<Self> {
        let p = dir.join("node.key");
        if let Ok(s) = std::fs::read_to_string(&p) {
            return serde_json::from_str(&s).context("node.key 格式错误");
        }
        let k = Keys::generate();
        write_private(&p, serde_json::to_string_pretty(&k)?.as_bytes())?;
        Ok(k)
    }
}

pub fn write_private(p: &Path, data: &[u8]) -> Result<()> {
    let tmp = p.with_extension("tmp");
    {
        use std::io::Write;
        let mut o = std::fs::OpenOptions::new();
        o.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
        o.open(&tmp)?.write_all(data)?;
    }
    std::fs::rename(&tmp, p)?;
    Ok(())
}

fn arr<const N: usize>(h: &str) -> Result<[u8; N]> {
    hex::decode(h)?.try_into().map_err(|_| anyhow::anyhow!("密钥长度错误"))
}

struct Slot {
    gen: u16,
    used: bool,
    last_seen: u64,
    conn: Option<(u64, mpsc::Sender<WsMsg>)>,
}

#[derive(Clone, Copy, Serialize, Deserialize)]
struct Entry {
    #[serde(with = "serde_bytes")]
    alias: Alias,
    node: u32,
    exp_day: u16,
}

struct PowState {
    cur: [u8; 16],
    prev: [u8; 16],
    hour: u64,
    used: std::collections::HashSet<[u8; 16]>,
}

#[derive(Default)]
pub struct Metrics {
    pub fwd_ok: AtomicU64,
    pub fwd_fail: AtomicU64,
    pub gw_register: AtomicU64,
    pub gw_lookup: AtomicU64,
    pub gw_fail: AtomicU64,
    pub pow_rejected: AtomicU64,
    pub delivered: AtomicU64,
    pub offline: AtomicU64,
}

pub struct Node {
    pub id: NodeId,
    pub tag: u32,
    pub sign: SigningKey,
    hpke_sk: [u8; 32],
    pub hpke_pk: [u8; 32],
    alias: AliasKey,
    mbox_key: [u8; 32],
    pub cfg: Config,
    dir: PathBuf,
    registry: RwLock<Arc<Registry>>,
    slots: Mutex<(Vec<Slot>, Vec<u32>)>,
    index: Vec<RwLock<Vec<Entry>>>,
    pow: Mutex<PowState>,
    conn_seq: AtomicU64,
    pub http: reqwest::Client,
    pub metrics: Metrics,
    lim_ip: Limiter<IpAddr>,
    lim_alias: Limiter<u32>,
    lim_ohttp: Limiter<u32>,
    lim_probe_out: Limiter<u32>,
    lim_probe_in: Limiter<Alias>,
    lim_env_in: Limiter<u32>,
}

pub type Shared = Arc<Node>;
pub type Code = &'static str;

impl Node {
    pub fn new(dir: &Path, cfg: Config, keys: &Keys) -> Result<Shared> {
        let sign = SigningKey::from_bytes(&arr::<32>(&keys.sign)?);
        let id = registry::node_id(&sign.verifying_key().to_bytes());
        let (hpke_sk, hpke_pk) = ohttp::keypair(&arr::<32>(&keys.hpke)?);
        let l = &cfg.limits;
        let n = Node {
            id,
            tag: registry::tag_of(&id),
            sign,
            hpke_sk,
            hpke_pk,
            alias: AliasKey::new(&arr::<16>(&keys.alias)?),
            mbox_key: arr::<32>(&keys.mailbox)?,
            dir: dir.to_path_buf(),
            registry: RwLock::new(Arc::new(Registry::default())),
            slots: Mutex::new((Vec::new(), Vec::new())),
            index: (0..65536).map(|_| RwLock::new(Vec::new())).collect(),
            pow: Mutex::new(PowState { cur: xseed_transport::random(), prev: xseed_transport::random(), hour: now() / 3600, used: Default::default() }),
            conn_seq: AtomicU64::new(1),
            http: reqwest::Client::builder().timeout(std::time::Duration::from_secs(10)).build()?,
            metrics: Metrics::default(),
            lim_ip: Limiter::new(l.mailboxes_per_ip_day, 86_400),
            lim_alias: Limiter::new(l.aliases_per_mailbox_day, 86_400),
            lim_ohttp: Limiter::new(l.ohttp_per_mailbox_hour, 3600),
            lim_probe_out: Limiter::new(l.probes_out_per_mailbox_min, 60),
            lim_probe_in: Limiter::new(l.probes_in_per_alias_hour, 3600),
            lim_env_in: Limiter::new(l.envelopes_in_per_mailbox_min, 60),
            cfg,
        };
        let n = Arc::new(n);
        n.load_snapshot();
        Ok(n)
    }

    pub fn manifest(&self, url: &str) -> NodeManifest {
        NodeManifest {
            id: hex::encode(self.id),
            url: url.to_string(),
            weight: 1.0,
            region: String::new(),
            sign_pk: format!("ed25519:{}", hex::encode(self.sign.verifying_key().to_bytes())),
            hpke_pk: format!("x25519:{}", hex::encode(self.hpke_pk)),
            operator: String::new(),
            contact: String::new(),
            since: now(),
        }
    }

    pub fn registry(&self) -> Arc<Registry> {
        self.registry.read().unwrap().clone()
    }

    pub fn set_registry(&self, r: Registry) {
        if r.by_id(&self.id).is_none() {
            tracing::warn!("节点清单中没有本节点（{}），其他节点将拒绝本节点的转发", hex::encode(self.id));
        }
        *self.registry.write().unwrap() = Arc::new(r);
    }

    /// 当前清单的版本（尚未载入或为空清单时为 None）。
    fn registry_version(&self) -> Option<u64> {
        let r = self.registry.read().unwrap();
        (!r.nodes.is_empty()).then_some(r.version)
    }

    pub async fn reload_registry(&self) -> Result<()> {
        let src = if self.cfg.registry.starts_with("http") || Path::new(&self.cfg.registry).is_absolute() { self.cfg.registry.clone() } else { self.dir.join(&self.cfg.registry).to_string_lossy().into_owned() };
        let l = registry::load_signed(&src, &self.http, &registry::trusted_keys(&self.cfg.registry_keys)).await?;
        if let Some(cur) = self.registry_version() {
            anyhow::ensure!(l.registry.version >= cur, "节点清单版本 {} 低于当前的 {}，拒绝回退", l.registry.version, cur);
        }
        self.set_registry(l.registry);
        Ok(())
    }

    // ───────── 工作量证明 ─────────

    fn rotate_pow(&self, now: u64) {
        let mut p = self.pow.lock().unwrap();
        if now / 3600 != p.hour {
            p.prev = p.cur;
            p.cur = xseed_transport::random();
            p.hour = now / 3600;
            p.used.clear();
        }
    }

    pub fn pow_salt(&self) -> [u8; 16] {
        self.rotate_pow(now());
        self.pow.lock().unwrap().cur
    }

    fn check_pow(&self, pow: &Pow, bits: u32) -> bool {
        self.rotate_pow(now());
        let mut p = self.pow.lock().unwrap();
        let ok = [p.cur, p.prev].iter().any(|s| xseed_core::pow::verify(s, &pow.salt, pow.nonce, bits)) && p.used.insert(pow.salt);
        if !ok {
            self.metrics.pow_rejected.fetch_add(1, Relaxed);
        }
        ok
    }

    // ───────── 信箱 ─────────

    fn secret_for(&self, token: &[u8; 16]) -> [u8; 32] {
        auth::hmac(&self.mbox_key, &[b"xseed/mbox/v1", token])
    }

    pub fn create_mailbox(&self, ip: IpAddr, pow: &Pow) -> Result<MailboxResp, Code> {
        if !self.check_pow(pow, self.cfg.pow.mailbox_bits) {
            return Err(code::POW_INVALID);
        }
        if !self.lim_ip.hit(&ip, now()) {
            return Err(code::RATE_LIMITED);
        }
        let mut g = self.slots.lock().unwrap();
        let (slots, free) = &mut *g;
        let slot = match free.pop() {
            Some(s) => s,
            None => {
                slots.push(Slot { gen: 0, used: false, last_seen: 0, conn: None });
                (slots.len() - 1) as u32
            }
        };
        let s = &mut slots[slot as usize];
        s.used = true;
        s.last_seen = now();
        let mut token = [0u8; 16];
        token[0..4].copy_from_slice(&slot.to_be_bytes());
        token[4..6].copy_from_slice(&s.gen.to_be_bytes());
        token[6..16].copy_from_slice(&xseed_transport::random::<10>());
        Ok(MailboxResp { slot_token: token, secret: self.secret_for(&token) })
    }

    /// 校验信箱 MAC，返回 (slot, gen)。
    fn auth(&self, token: &[u8; 16], mac_ok: impl Fn(&[u8; 32]) -> bool) -> Result<(u32, u16), Code> {
        let slot = u32::from_be_bytes(token[0..4].try_into().unwrap());
        let gen = u16::from_be_bytes([token[4], token[5]]);
        if !mac_ok(&self.secret_for(token)) {
            return Err(code::AUTH_FAILED);
        }
        let mut g = self.slots.lock().unwrap();
        match g.0.get_mut(slot as usize) {
            Some(s) if s.used && s.gen == gen => {
                s.last_seen = now();
                Ok((slot, gen))
            }
            _ => Err(code::AUTH_FAILED),
        }
    }

    pub fn auth_request(&self, header: Option<&str>, path: &str, body: &[u8]) -> Result<(u32, u16), Code> {
        let (token, ts, mac) = header.and_then(auth::parse_auth_header).ok_or(code::AUTH_FAILED)?;
        if !auth::fresh(ts, now()) {
            return Err(code::AUTH_FAILED);
        }
        self.auth(&token, |secret| auth::ct_eq(&auth::request_mac(secret, path, ts, body), &mac))
    }

    pub fn auth_ws(&self, token: &[u8; 16], challenge: &[u8; 32], mac: &[u8; 32]) -> Result<(u32, u16), Code> {
        self.auth(token, |secret| auth::ct_eq(&auth::ws_mac(secret, challenge), mac))
    }

    /// 绑定长连接；同一信箱只保留最新的一条。返回连接序号。
    pub fn attach(&self, slot: u32, tx: mpsc::Sender<WsMsg>) -> Result<u64, Code> {
        let mut g = self.slots.lock().unwrap();
        let online = g.0.iter().filter(|s| s.conn.is_some()).count();
        let s = &mut g.0[slot as usize];
        if s.conn.is_none() && online >= self.cfg.limits.max_connections {
            return Err(code::OVERLOADED);
        }
        let id = self.conn_seq.fetch_add(1, Relaxed);
        s.conn = Some((id, tx)); // 旧连接的发送端被丢弃，旧连接随之结束
        Ok(id)
    }

    pub fn detach(&self, slot: u32, conn_id: u64) {
        let mut g = self.slots.lock().unwrap();
        if let Some(s) = g.0.get_mut(slot as usize) {
            if s.conn.as_ref().map(|c| c.0) == Some(conn_id) {
                s.conn = None;
                s.last_seen = now();
            }
        }
    }

    pub fn online(&self) -> usize {
        self.slots.lock().unwrap().0.iter().filter(|s| s.conn.is_some()).count()
    }

    pub fn aliases(&self, slot: u32, gen: u16, n: u16) -> Result<Vec<u8>, Code> {
        let n = n.clamp(1, 4096);
        let t = now();
        if !self.lim_alias.hit_n(&slot, n as u32, t) {
            return Err(code::RATE_LIMITED);
        }
        let w = week(t);
        Ok((0..n).flat_map(|_| self.alias.issue(slot, gen, w)).collect())
    }

    pub fn check_ohttp_quota(&self, slot: u32) -> bool {
        self.lim_ohttp.hit(&slot, now())
    }

    // ───────── 投递 ─────────

    /// 本节点信箱用户发出的信封。
    pub async fn send_from(&self, slot: u32, gen: u16, env: Envelope) -> Result<(), Code> {
        if env.v != xseed_transport::PROTO || env.body.len() > MAX_BODY {
            return Err(code::BAD_REQUEST);
        }
        if env.from.node != self.tag || self.alias.open_fresh(&env.from.alias, week(now())) != Some((slot, gen)) {
            return Err(code::ALIAS_INVALID);
        }
        if env.kind == Kind::Probe && !self.lim_probe_out.hit(&slot, now()) {
            return Err(code::RATE_LIMITED);
        }
        if env.to.node == self.tag {
            self.deliver_local(env)
        } else {
            self.forward(env).await
        }
    }

    /// 投递给本节点上的信箱。
    pub fn deliver_local(&self, env: Envelope) -> Result<(), Code> {
        let t = now();
        let (slot, gen) = self.alias.open_fresh(&env.to.alias, week(t)).ok_or(code::ALIAS_INVALID)?;
        if env.kind == Kind::Probe && !self.lim_probe_in.hit(&env.to.alias, t) {
            return Err(code::RATE_LIMITED);
        }
        if !self.lim_env_in.hit(&slot, t) {
            return Err(code::RATE_LIMITED);
        }
        let tx = {
            let g = self.slots.lock().unwrap();
            match g.0.get(slot as usize) {
                Some(s) if s.used && s.gen == gen => s.conn.as_ref().map(|c| c.1.clone()),
                _ => return Err(code::ALIAS_INVALID),
            }
        };
        let Some(tx) = tx else {
            self.metrics.offline.fetch_add(1, Relaxed);
            return Err(code::MAILBOX_OFFLINE);
        };
        match tx.try_send(WsMsg::Env { env }) {
            Ok(()) => {
                self.metrics.delivered.fetch_add(1, Relaxed);
                Ok(())
            }
            Err(mpsc::error::TrySendError::Full(_)) => Err(code::OVERLOADED),
            Err(_) => Err(code::MAILBOX_OFFLINE),
        }
    }

    async fn post_signed(&self, url: String, ctx: &[u8], body: Vec<u8>) -> Result<Vec<u8>, String> {
        let ts = now();
        let sig = auth::node_sign(&self.sign, ctx, &body, ts);
        let r = self
            .http
            .post(url)
            .header(auth::H_NODE, hex::encode(self.id))
            .header(auth::H_TS, ts.to_string())
            .header(auth::H_SIG, hex::encode(sig))
            .body(body)
            .send()
            .await
            .map_err(|_| code::OVERLOADED.to_string())?;
        let ok = r.status().is_success();
        let b = r.bytes().await.map_err(|_| code::OVERLOADED.to_string())?.to_vec();
        if ok {
            Ok(b)
        } else {
            Err(serde_json::from_slice::<ErrorBody>(&b).map(|e| e.error).unwrap_or_else(|_| code::OVERLOADED.into()))
        }
    }

    async fn forward(&self, env: Envelope) -> Result<(), Code> {
        let Some(target) = self.registry().by_tag(env.to.node).cloned() else { return Err(code::UNKNOWN_NODE) };
        let r = self.post_signed(format!("{}/v1/fwd", target.url.trim_end_matches('/')), auth::CTX_FWD, to_cbor(&env)).await;
        match r {
            Ok(_) => {
                self.metrics.fwd_ok.fetch_add(1, Relaxed);
                Ok(())
            }
            Err(c) => {
                self.metrics.fwd_fail.fetch_add(1, Relaxed);
                Err(static_code(&c))
            }
        }
    }

    /// 校验来自其他节点的签名请求，返回来源节点清单。
    pub fn verify_node(&self, node: Option<&str>, ts: Option<&str>, sig: Option<&str>, ctx: &[u8], body: &[u8]) -> Result<NodeManifest, Code> {
        let id: [u8; 16] = node.and_then(|h| hex::decode(h).ok()).and_then(|v| v.try_into().ok()).ok_or(code::SIG_INVALID)?;
        let ts: u64 = ts.and_then(|t| t.parse().ok()).ok_or(code::SIG_INVALID)?;
        let sig = sig.and_then(|h| hex::decode(h).ok()).ok_or(code::SIG_INVALID)?;
        let m = self.registry().by_id(&id).cloned().ok_or(code::UNKNOWN_NODE)?;
        let pk = m.sign_key().map_err(|_| code::UNKNOWN_NODE)?;
        if !auth::fresh(ts, now()) || !auth::node_verify(&pk, ctx, body, ts, &sig) {
            return Err(code::SIG_INVALID);
        }
        Ok(m)
    }

    // ───────── 两跳网关 ─────────

    /// 入口：把封装好的请求交给目标节点（或自己）。
    pub async fn relay(&self, target: &NodeId, req: Vec<u8>) -> Result<Vec<u8>, Code> {
        if req.len() > MAX_OHTTP {
            return Err(code::BAD_REQUEST);
        }
        if *target == self.id {
            return self.gateway(&req);
        }
        let Some(m) = self.registry().by_id(target).cloned() else { return Err(code::UNKNOWN_NODE) };
        self.post_signed(format!("{}/v1/gateway", m.url.trim_end_matches('/')), auth::CTX_GW, req).await.map_err(|c| static_code(&c))
    }

    /// 桶节点：解开两跳封装并执行登记 / 查询。
    pub fn gateway(&self, req: &[u8]) -> Result<Vec<u8>, Code> {
        let (plain, key) = ohttp::open_request(&self.hpke_sk, &self.id, req).map_err(|_| code::BAD_REQUEST)?;
        let resp = match from_cbor::<GwReq>(&plain) {
            Ok(GwReq::Register { entries, ttl_d, pow }) => {
                self.metrics.gw_register.fetch_add(1, Relaxed);
                self.register(&entries, ttl_d, &pow).map(|n| GwResp::Registered { n }).unwrap_or_else(|c| GwResp::Error { code: c.into() })
            }
            Ok(GwReq::Lookup { buckets }) => {
                self.metrics.gw_lookup.fetch_add(1, Relaxed);
                if buckets.len() > MAX_LOOKUP {
                    GwResp::Error { code: code::BAD_REQUEST.into() }
                } else {
                    GwResp::Found { buckets: buckets.iter().map(|&b| BucketHits { b, hits: self.lookup(b) }).collect() }
                }
            }
            Err(_) => GwResp::Error { code: code::BAD_REQUEST.into() },
        };
        if matches!(resp, GwResp::Error { .. }) {
            self.metrics.gw_fail.fetch_add(1, Relaxed);
        }
        Ok(ohttp::seal_response(&key, &to_cbor(&resp)))
    }

    fn owns(&self, reg: &Registry, bucket: u16, t: u64) -> bool {
        // 清单为空或不含本节点时（单机测试）接受全部桶
        reg.by_id(&self.id).is_none() || reg.owners(bucket, t).iter().any(|m| m.id_bytes().ok() == Some(self.id))
    }

    fn register(&self, entries: &[RegEntry], ttl_d: u8, pow: &Pow) -> Result<u32, Code> {
        if entries.is_empty() || entries.len() > MAX_REGISTER || !(1..=3).contains(&ttl_d) {
            return Err(code::BAD_REQUEST);
        }
        if !self.check_pow(pow, self.cfg.pow.register_bits) {
            return Err(code::POW_INVALID);
        }
        let t = now();
        let today = day(t);
        let exp = today + ttl_d as u16;
        let reg = self.registry();
        let mut n = 0;
        for e in entries {
            if !self.owns(&reg, e.b, t) {
                continue;
            }
            let mut v = self.index[e.b as usize].write().unwrap();
            v.retain(|x| x.exp_day >= today);
            if let Some(x) = v.iter_mut().find(|x| x.alias == e.alias) {
                x.exp_day = exp;
                x.node = e.node;
            } else {
                if v.len() >= self.cfg.index.max_per_bucket {
                    // 淘汰最早过期的一条
                    if let Some(i) = v.iter().enumerate().min_by_key(|(_, x)| x.exp_day).map(|(i, _)| i) {
                        v.swap_remove(i);
                    }
                }
                v.push(Entry { alias: e.alias, node: e.node, exp_day: exp });
            }
            n += 1;
        }
        Ok(n)
    }

    fn lookup(&self, b: u16) -> Vec<Addr> {
        let today = day(now());
        let v = self.index[b as usize].read().unwrap();
        let mut hits: Vec<Addr> = v.iter().filter(|x| x.exp_day >= today).map(|x| Addr { node: x.node, alias: x.alias }).collect();
        if hits.len() > MAX_HITS {
            hits.shuffle(&mut rand::thread_rng());
            hits.truncate(MAX_HITS);
        }
        hits
    }

    pub fn index_entries(&self) -> usize {
        self.index.iter().map(|b| b.read().unwrap().len()).sum()
    }

    // ───────── 维护与快照 ─────────

    /// 每小时：清理过期登记、回收 30 天未用的信箱槽位、清理限流计数。
    pub fn sweep(&self) {
        let t = now();
        let today = day(t);
        for b in &self.index {
            b.write().unwrap().retain(|x| x.exp_day >= today);
        }
        let mut g = self.slots.lock().unwrap();
        let (slots, free) = &mut *g;
        for (i, s) in slots.iter_mut().enumerate() {
            if s.used && s.conn.is_none() && t.saturating_sub(s.last_seen) > 30 * 86_400 {
                s.used = false;
                s.gen = s.gen.wrapping_add(1);
                free.push(i as u32);
            }
        }
        drop(g);
        self.rotate_pow(t);
        self.lim_ip.sweep(t);
        self.lim_alias.sweep(t);
        self.lim_ohttp.sweep(t);
        self.lim_probe_out.sweep(t);
        self.lim_probe_in.sweep(t);
        self.lim_env_in.sweep(t);
    }

    fn snapshot_path(&self) -> PathBuf {
        self.dir.join("snapshot.cbor")
    }

    pub fn save_snapshot(&self) -> Result<()> {
        let slots: Vec<(u16, bool, u64)> = self.slots.lock().unwrap().0.iter().map(|s| (s.gen, s.used, s.last_seen)).collect();
        let index: Vec<(u16, Vec<Entry>)> = self.index.iter().enumerate().filter_map(|(b, v)| {
            let v = v.read().unwrap();
            (!v.is_empty()).then(|| (b as u16, v.clone()))
        }).collect();
        write_private(&self.snapshot_path(), &to_cbor(&Snapshot { slots, index }))?;
        Ok(())
    }

    fn load_snapshot(&self) {
        let Ok(b) = std::fs::read(self.snapshot_path()) else { return };
        let Ok(s) = from_cbor::<Snapshot>(&b) else {
            tracing::warn!("快照无法读取，忽略");
            return;
        };
        let mut g = self.slots.lock().unwrap();
        g.0 = s.slots.iter().map(|&(gen, used, last_seen)| Slot { gen, used, last_seen, conn: None }).collect();
        g.1 = g.0.iter().enumerate().filter(|(_, s)| !s.used).map(|(i, _)| i as u32).collect();
        for (b, v) in s.index {
            *self.index[b as usize].write().unwrap() = v;
        }
        tracing::info!("已载入快照：{} 个信箱，{} 条登记", g.0.iter().filter(|s| s.used).count(), self.index_entries());
    }
}

#[derive(Serialize, Deserialize)]
struct Snapshot {
    slots: Vec<(u16, bool, u64)>,
    index: Vec<(u16, Vec<Entry>)>,
}

/// 把远端返回的错误码映射回静态字符串。
fn static_code(c: &str) -> Code {
    [code::BAD_REQUEST, code::POW_INVALID, code::AUTH_FAILED, code::SIG_INVALID, code::ALIAS_INVALID, code::UNKNOWN_NODE, code::MAILBOX_OFFLINE, code::RATE_LIMITED]
        .into_iter()
        .find(|x| *x == c)
        .unwrap_or(code::OVERLOADED)
}

pub fn status_of(c: Code) -> u16 {
    match c {
        code::BAD_REQUEST | code::POW_INVALID => 400,
        code::AUTH_FAILED | code::SIG_INVALID => 401,
        code::ALIAS_INVALID | code::UNKNOWN_NODE => 403,
        code::MAILBOX_OFFLINE => 404,
        code::RATE_LIMITED => 429,
        _ => 503,
    }
}
