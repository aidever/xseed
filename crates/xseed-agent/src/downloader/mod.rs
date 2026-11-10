//! 下载器适配（03 §1.10）。只有“保留数据移除”，没有任何删除数据的接口。

mod qb;
mod tr;

pub use qb::Qb;
pub use tr::Tr;

use crate::config::{DownloaderCfg, DownloaderKind};
use anyhow::Result;
use xseed_core::torrent::Entries;

#[derive(Debug, Clone)]
pub struct LocalTorrent {
    /// 下载器使用的 hash 字符串（小写十六进制）。
    pub hash: String,
    pub ih1: Option<String>,
    pub ih2: Option<String>,
    pub name: String,
    pub save_path: String,
    pub category: Option<String>,
    pub size: u64,
    pub comment: Option<String>,
    /// 下载器报告文件缺失或出错
    pub broken: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Layout {
    Original,
    NoSubfolder,
}

#[derive(Debug, Clone)]
pub struct AddRequest {
    pub torrent: Vec<u8>,
    pub save_path: String,
    pub layout: Layout,
    pub rename_root: Option<(String, String)>,
    pub skip_check: bool,
    pub category: Option<String>,
}

/// 某个种子在下载器中的当前状态（加入前检查原种子、加入后观察新种子）。
#[derive(Debug, Clone)]
pub struct Status {
    pub health: Health,
    pub progress: f64,
    pub save_path: String,
    /// 内容所在位置：多文件种子为根目录的完整路径（qB `content_path`；TR `downloadDir/name`）。
    pub content_path: String,
    /// 带有 `xseed` 标签
    pub tagged: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Health {
    Ok,
    Checking,
    /// 文件缺失或下载器报错
    Broken(String),
}

/// tracker 汇报结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Announce {
    Pending,
    Working,
    Failed(String),
}

/// 种子的元数据来源：原始种子或文件列表。
pub enum Meta {
    Raw(Vec<u8>),
    Entries(Entries),
}

pub const TAG: &str = "xseed";

pub enum Client {
    Qb(Qb),
    Tr(Tr),
}

impl Client {
    pub fn new(cfg: &DownloaderCfg, password: &str) -> Result<Self> {
        Ok(match cfg.kind {
            DownloaderKind::Qbittorrent => Client::Qb(Qb::new(&cfg.url, &cfg.user, password)?),
            DownloaderKind::Transmission => Client::Tr(Tr::new(&cfg.url, &cfg.user, password)?),
        })
    }

    /// 测试连接，返回“qBittorrent 5.0.2”这样的描述。
    pub async fn version(&self) -> Result<String> {
        match self {
            Client::Qb(c) => c.version().await,
            Client::Tr(c) => c.version().await,
        }
    }

    pub async fn list_completed(&self) -> Result<Vec<LocalTorrent>> {
        match self {
            Client::Qb(c) => c.list_completed().await,
            Client::Tr(c) => c.list_completed().await,
        }
    }

    pub async fn meta(&self, t: &LocalTorrent) -> Result<Meta> {
        match self {
            Client::Qb(c) => c.meta(t).await,
            Client::Tr(c) => c.meta(t).await,
        }
    }

    pub async fn comment(&self, t: &LocalTorrent) -> Result<Option<String>> {
        match self {
            Client::Qb(c) => c.comment(&t.hash).await,
            Client::Tr(_) => Ok(t.comment.clone()),
        }
    }

    pub async fn trackers(&self, t: &LocalTorrent) -> Result<Vec<String>> {
        match self {
            Client::Qb(c) => c.trackers(&t.hash).await,
            Client::Tr(c) => c.trackers(&t.hash).await,
        }
    }

    pub async fn add(&self, req: AddRequest) -> Result<()> {
        match self {
            Client::Qb(c) => c.add(req).await,
            Client::Tr(c) => c.add(req).await,
        }
    }

    pub async fn start(&self, hash: &str) -> Result<()> {
        match self {
            Client::Qb(c) => c.start(hash).await,
            Client::Tr(c) => c.start(hash).await,
        }
    }

    pub async fn status(&self, hash: &str) -> Result<Option<Status>> {
        match self {
            Client::Qb(c) => c.status(hash).await,
            Client::Tr(c) => c.status(hash).await,
        }
    }

    pub async fn announce(&self, hash: &str) -> Result<Announce> {
        match self {
            Client::Qb(c) => c.announce(hash).await,
            Client::Tr(c) => c.announce(hash).await,
        }
    }

    /// 新种子在该下载器中的 hash 字符串。qB 对纯 v2 种子使用截短到 20 字节的 v2 hash。
    pub fn hash_of(&self, ih1: Option<&[u8]>, ih2: Option<&[u8]>) -> Option<String> {
        if let Some(h) = ih1 {
            return Some(hex::encode(h));
        }
        let h = ih2?;
        Some(match self {
            Client::Qb(_) => hex::encode(&h[..20]),
            Client::Tr(_) => hex::encode(h),
        })
    }

    pub async fn remove_keep_data(&self, hash: &str) -> Result<()> {
        match self {
            Client::Qb(c) => c.remove_keep_data(hash).await,
            Client::Tr(c) => c.remove_keep_data(hash).await,
        }
    }
}

pub(crate) fn http() -> reqwest::ClientBuilder {
    reqwest::Client::builder().timeout(std::time::Duration::from_secs(30)).connect_timeout(std::time::Duration::from_secs(8)).user_agent(concat!("xseed-agent/", env!("CARGO_PKG_VERSION")))
}

pub(crate) fn base(url: &str) -> Result<url::Url> {
    let mut u = url::Url::parse(url.trim())?;
    if !matches!(u.scheme(), "http" | "https") {
        anyhow::bail!("地址必须以 http:// 或 https:// 开头");
    }
    if !u.path().ends_with('/') {
        let p = format!("{}/", u.path());
        u.set_path(&p);
    }
    Ok(u)
}

/// 只保留 http / https / udp 的 tracker 地址（过滤 DHT、PeX 等伪条目）。
pub(crate) fn real_tracker(u: &str) -> bool {
    u.starts_with("http://") || u.starts_with("https://") || u.starts_with("udp://")
}
