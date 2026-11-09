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
    pub pin_entry: String,
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
