//! 应用状态与后台任务。

use crate::auth::{Lockout, Sessions};
use crate::config::Config;
use crate::db::Db;
use crate::downloader::Client;
use crate::inventory::{self, Progress};
use crate::ptd::SyncResult;
use crate::vault::Vault;
use anyhow::{anyhow, Result};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

pub struct App {
    pub dir: PathBuf,
    pub db: Db,
    pub vault: Vault,
    pub cfg: RwLock<Config>,
    pub sessions: Sessions,
    pub login_lock: Lockout,
    pub dav_lock: Lockout,
    pub scan: Progress,
    pub scan_lock: tokio::sync::Mutex<()>,
    pub ptd_last: Mutex<Option<SyncResult>>,
    /// 最近一次连接下载器的结果：（描述或错误, 是否成功）
    pub downloader_status: Mutex<Option<(String, bool)>>,
    pub net: crate::network::Net,
}

pub type Shared = Arc<App>;

impl App {
    pub fn open(dir: PathBuf) -> Result<Shared> {
        std::fs::create_dir_all(&dir)?;
        let db = Db::open(&dir.join("xseed-agent.db"))?;
        let vault = Vault::open(&dir, db.clone())?;
        let mut cfg = Config::load(&dir)?;
        // 环境变量只在配置文件未设置时生效
        if cfg.registry.is_empty() {
            if let Ok(r) = std::env::var("XSEED_REGISTRY") {
                cfg.registry = r;
            }
        }
        let ptd_last = db.meta_get("ptd_last").and_then(|s| serde_json::from_str(&s).ok());
        Ok(Arc::new(App {
            dir,
            db,
            vault,
            cfg: RwLock::new(cfg),
            sessions: Sessions::default(),
            login_lock: Lockout::new(5, Duration::from_secs(15 * 60)),
            dav_lock: Lockout::new(10, Duration::from_secs(3600)),
            scan: Default::default(),
            scan_lock: Default::default(),
            ptd_last: Mutex::new(ptd_last),
            downloader_status: Mutex::new(None),
            net: Default::default(),
        }))
    }

    pub fn cfg(&self) -> Config {
        self.cfg.read().unwrap().clone()
    }

    pub fn update_cfg(&self, f: impl FnOnce(&mut Config)) -> Result<()> {
        let mut c = self.cfg.write().unwrap();
        f(&mut c);
        c.save(&self.dir)?;
        drop(c);
        // 配置变化（完成设置、节点清单、入口节点）立即生效
        self.net.kick.notify_one();
        Ok(())
    }

    pub fn client(&self, id: &str) -> Result<Client> {
        let cfg = self.cfg();
        let d = cfg.downloaders.iter().find(|d| d.id == id).ok_or_else(|| anyhow!("下载器不存在"))?;
        Client::new(d, &self.vault.secret(&format!("downloader/{}", d.id)).unwrap_or_default())
    }

    /// 扫描全部下载器；同一时间只跑一次。
    pub async fn scan_all(self: &Arc<Self>, full: bool) {
        let Ok(_g) = self.scan_lock.try_lock() else { return };
        {
            let mut p = self.scan.lock().unwrap();
            p.running = true;
            p.error = None;
        }
        let mut err = None;
        for d in self.cfg().downloaders {
            let res = match self.client(&d.id) {
                Ok(c) => match c.version().await {
                    Ok(v) => {
                        *self.downloader_status.lock().unwrap() = Some((v, true));
                        inventory::scan(&self.db, &self.vault, &d.id, &c, full, &self.scan).await
                    }
                    Err(e) => Err(e),
                },
                Err(e) => Err(e),
            };
            if let Err(e) = res {
                tracing::warn!("扫描下载器 {} 失败：{e:#}", d.id);
                *self.downloader_status.lock().unwrap() = Some((e.to_string(), false));
                err = Some(format!("downloader_unreachable: {e}"));
            }
        }
        let mut p = self.scan.lock().unwrap();
        p.running = false;
        p.last_at = Some(crate::db::now());
        p.error = err;
    }

    pub fn set_ptd_result(&self, r: SyncResult) {
        let _ = self.db.meta_set("ptd_last", &serde_json::to_string(&r).unwrap_or_default());
        *self.ptd_last.lock().unwrap() = Some(r);
    }
}

/// 后台循环：每 10 分钟增量扫描（03 §4.5 `inventory.scan`）。
pub async fn background(app: Shared) {
    let mut tick = tokio::time::interval(Duration::from_secs(600));
    loop {
        tick.tick().await;
        if !app.cfg().downloaders.is_empty() {
            app.scan_all(false).await;
        }
    }
}
