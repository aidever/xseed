//! 本地 SQLite（03 §3.1）。M1 用到的表在这里建好；后续里程碑的表也一并建出，避免迁移。

use anyhow::Result;
use rusqlite::Connection;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

#[derive(Clone)]
pub struct Db(Arc<Mutex<Connection>>);

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS meta (k TEXT PRIMARY KEY, v TEXT NOT NULL);

-- 本机清单。主键用下载器给出的 hash 字符串（v1 为 40 位，v2 为 64 位），避免可空主键。
CREATE TABLE IF NOT EXISTS local_torrents (
  downloader TEXT NOT NULL, hash TEXT NOT NULL, ih1 BLOB, ih2 BLOB,
  name TEXT NOT NULL, root_name TEXT NOT NULL, single_file INTEGER NOT NULL,
  save_path TEXT NOT NULL, category TEXT, total_size INTEGER NOT NULL,
  sfp BLOB, bucket INTEGER,
  site_kind TEXT, site_ref TEXT, hosts TEXT NOT NULL,
  torrent_id TEXT,
  state TEXT NOT NULL,            -- ok | invalid | ambiguous | unknown_site
  reason TEXT,
  first_seen INTEGER NOT NULL, last_seen INTEGER NOT NULL,
  PRIMARY KEY (downloader, hash)
);
CREATE INDEX IF NOT EXISTS lt_sfp ON local_torrents(sfp);
CREATE INDEX IF NOT EXISTS lt_site ON local_torrents(site_ref);

CREATE TABLE IF NOT EXISTS aliases (
  alias BLOB PRIMARY KEY, node_tag INTEGER NOT NULL, purpose TEXT NOT NULL,
  downloader TEXT, hash TEXT, week INTEGER NOT NULL, used INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS aliases_owner ON aliases(downloader, hash, week);
-- 探测确认过的同款持有者（M2）：同一 SFP 的匿名地址，供 M3 会话使用
CREATE TABLE IF NOT EXISTS peers (
  sfp BLOB NOT NULL, node INTEGER NOT NULL, alias BLOB NOT NULL, week INTEGER NOT NULL, acked_at INTEGER NOT NULL,
  PRIMARY KEY (sfp, alias)
);
-- 每个 SFP 最近一次查询
CREATE TABLE IF NOT EXISTS lookups (sfp BLOB PRIMARY KEY, at INTEGER NOT NULL, hits INTEGER NOT NULL);
-- 询问记录（M3）：同一内容同一站点问过后，一段时间内不再问
CREATE TABLE IF NOT EXISTS asks (sfp BLOB NOT NULL, site_ref TEXT NOT NULL, at INTEGER NOT NULL, result TEXT NOT NULL, PRIMARY KEY (sfp, site_ref));
CREATE TABLE IF NOT EXISTS challenge_pool (
  site_ref TEXT NOT NULL, torrent_id TEXT NOT NULL, ih BLOB NOT NULL,
  fetched_at INTEGER NOT NULL, uses INTEGER NOT NULL DEFAULT 0, PRIMARY KEY (site_ref, torrent_id)
);
CREATE TABLE IF NOT EXISTS answer_cache (
  site_ref TEXT NOT NULL, torrent_id TEXT NOT NULL, ih BLOB NOT NULL, fetched_at INTEGER NOT NULL,
  PRIMARY KEY (site_ref, torrent_id)
);
CREATE TABLE IF NOT EXISTS offers (
  id INTEGER PRIMARY KEY, sfp BLOB NOT NULL, site_ref TEXT NOT NULL, torrent_id TEXT NOT NULL,
  ih1 BLOB, ih2 BLOB, size INTEGER NOT NULL, received_at INTEGER NOT NULL, status TEXT NOT NULL,
  UNIQUE (site_ref, torrent_id)
);
CREATE TABLE IF NOT EXISTS jobs (
  id INTEGER PRIMARY KEY, offer_id INTEGER NOT NULL REFERENCES offers(id),
  local_downloader TEXT NOT NULL, local_hash TEXT NOT NULL,
  state TEXT NOT NULL, attempts INTEGER NOT NULL DEFAULT 0, reason TEXT,
  created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL,
  new_hash TEXT, not_before INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS jobs_state ON jobs(state);
CREATE TABLE IF NOT EXISTS added_torrents (
  downloader TEXT NOT NULL, hash TEXT NOT NULL, job_id INTEGER NOT NULL,
  added_at INTEGER NOT NULL, removed_at INTEGER, PRIMARY KEY (downloader, hash)
);

-- 站点开关与配额
CREATE TABLE IF NOT EXISTS sites (
  site_ref TEXT PRIMARY KEY, enabled INTEGER NOT NULL,
  daily_add_limit INTEGER NOT NULL DEFAULT 50, daily_answer_limit INTEGER NOT NULL DEFAULT 30,
  added_today INTEGER NOT NULL DEFAULT 0, answered_today INTEGER NOT NULL DEFAULT 0, day INTEGER NOT NULL DEFAULT 0
);
-- 站点定义（M1 来自 PT-depiler 元数据；M5 起来自注册中心）
CREATE TABLE IF NOT EXISTS site_defs (
  id TEXT PRIMARY KEY, name TEXT NOT NULL, domains TEXT NOT NULL, tracker_hosts TEXT NOT NULL,
  p2p TEXT NOT NULL DEFAULT 'unknown', source TEXT NOT NULL, updated_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS site_traits (
  site_ref TEXT NOT NULL, trait TEXT NOT NULL, value TEXT NOT NULL,
  learned_at INTEGER NOT NULL, expires_at INTEGER NOT NULL, PRIMARY KEY (site_ref, trait)
);
CREATE TABLE IF NOT EXISTS credentials (
  site_ref TEXT NOT NULL, kind TEXT NOT NULL, source TEXT NOT NULL,
  nonce BLOB NOT NULL, ct BLOB NOT NULL, updated_at INTEGER NOT NULL,
  PRIMARY KEY (site_ref, kind, source)
);
-- 其他机密（下载器密码、WebDAV 密码、PT-depiler 加密密钥）
CREATE TABLE IF NOT EXISTS secrets (name TEXT PRIMARY KEY, nonce BLOB NOT NULL, ct BLOB NOT NULL);
CREATE TABLE IF NOT EXISTS audit_log (id INTEGER PRIMARY KEY, at INTEGER NOT NULL, kind TEXT NOT NULL, detail TEXT NOT NULL);
"#;

/// M3 及更早建出的库补列（列已存在时忽略错误）。
fn migrate(c: &Connection) {
    let _ = c.execute_batch("ALTER TABLE jobs ADD COLUMN new_hash TEXT");
    let _ = c.execute_batch("ALTER TABLE jobs ADD COLUMN not_before INTEGER NOT NULL DEFAULT 0");
}

impl Db {
    pub fn open(path: &Path) -> Result<Self> {
        let c = Connection::open(path)?;
        c.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON; PRAGMA busy_timeout=5000;")?;
        c.execute_batch(SCHEMA)?;
        migrate(&c);
        Ok(Db(Arc::new(Mutex::new(c))))
    }

    #[cfg(test)]
    pub fn memory() -> Self {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(SCHEMA).unwrap();
        Db(Arc::new(Mutex::new(c)))
    }

    pub fn lock(&self) -> MutexGuard<'_, Connection> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn meta_get(&self, k: &str) -> Option<String> {
        self.lock().query_row("SELECT v FROM meta WHERE k=?1", [k], |r| r.get(0)).ok()
    }

    pub fn meta_set(&self, k: &str, v: &str) -> Result<()> {
        self.lock().execute("INSERT INTO meta(k,v) VALUES(?1,?2) ON CONFLICT(k) DO UPDATE SET v=excluded.v", [k, v])?;
        Ok(())
    }

    pub fn audit(&self, kind: &str, detail: &str) {
        let _ = self.lock().execute("INSERT INTO audit_log(at,kind,detail) VALUES(?1,?2,?3)", rusqlite::params![now(), kind, detail]);
    }
}

pub fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}
