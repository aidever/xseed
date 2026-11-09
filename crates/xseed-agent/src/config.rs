//! `xseed-agent.toml`（03 §4.1）。机密不写进配置文件，存在 vault。

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum DownloaderKind {
    Qbittorrent,
    Transmission,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloaderCfg {
    /// 稳定标识，写入本机清单。
    pub id: String,
    pub kind: DownloaderKind,
    pub url: String,
    pub user: String,
    // 密码在 vault：secret `downloader/<id>`
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WebUi {
    pub listen: String,
    /// 允许的 Host（Host / Origin 校验）；为空时首次访问记录。
    pub allowed_hosts: Vec<String>,
}

impl Default for WebUi {
    fn default() -> Self {
        WebUi { listen: "0.0.0.0:7731".into(), allowed_hosts: vec![] }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Ptd {
    pub enabled: bool,
    pub user: String,
    // 密码：secret `ptd/password`；加密密钥：secret `ptd/key`
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub webui: WebUi,
    pub downloaders: Vec<DownloaderCfg>,
    pub ptd: Ptd,
    pub dry_run: bool,
    pub respond: bool,
    pub daily_add_limit: u32,
    pub daily_answer_limit: u32,
    pub setup_done: bool,
    pub log_level: String,
    /// 固定入口节点（node_id）；为空时自动选择延迟最低的节点。
    pub pin_entry: String,
    /// 节点清单：http(s) 地址或本地文件。为空时不连接辅种网络。
    pub registry: String,
    pub net: NetTuning,
}

/// 辅种网络节奏（一般不需要修改）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct NetTuning {
    /// 每轮登记分散在多长时间内发出（秒）。
    pub register_spread_secs: u64,
    /// 首次登记的分散时间（秒）。
    pub first_register_spread_secs: u64,
    /// 同一内容多久重新查询一次（秒）。
    pub lookup_every_secs: u64,
    /// 每个内容最多探测几个候选持有者。
    pub probes_per_content: usize,
    /// 两次探测之间的最小间隔（毫秒）。
    pub probe_gap_ms: u64,
}

impl Default for NetTuning {
    fn default() -> Self {
        NetTuning { register_spread_secs: 86_400, first_register_spread_secs: 600, lookup_every_secs: 86_400, probes_per_content: 8, probe_gap_ms: 1_500 }
    }
}

impl Default for Config {
    fn default() -> Self {
        Config {
            webui: WebUi::default(),
            downloaders: vec![],
            ptd: Ptd::default(),
            dry_run: true,
            respond: true,
            daily_add_limit: 50,
            daily_answer_limit: 30,
            setup_done: false,
            log_level: "info".into(),
            pin_entry: String::new(),
            registry: String::new(),
            net: NetTuning::default(),
        }
    }
}

pub fn path(dir: &Path) -> PathBuf {
    dir.join("xseed-agent.toml")
}

impl Config {
    pub fn load(dir: &Path) -> Result<Self> {
        let p = path(dir);
        if !p.exists() {
            return Ok(Config::default());
        }
        Ok(toml::from_str(&std::fs::read_to_string(p)?)?)
    }

    pub fn save(&self, dir: &Path) -> Result<()> {
        let p = path(dir);
        let tmp = p.with_extension("toml.tmp");
        std::fs::write(&tmp, toml::to_string_pretty(self)?)?;
        std::fs::rename(tmp, p)?;
        Ok(())
    }

    pub fn validate(&self) -> Result<(), String> {
        if !(1..=500).contains(&self.daily_add_limit) {
            return Err("每站每天最多加种需在 1–500 之间".into());
        }
        if !(1..=200).contains(&self.daily_answer_limit) {
            return Err("每站每天最多答题需在 1–200 之间".into());
        }
        Ok(())
    }
}
