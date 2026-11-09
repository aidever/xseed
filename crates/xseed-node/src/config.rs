//! 节点配置（03 §4.2，`xseed-node.toml`）。

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Config {
    /// 监听地址。生产环境建议在前面放反向代理（Caddy / Nginx）终止 TLS。
    pub listen: String,
    /// 对外地址，写入节点清单。
    pub url: String,
    /// 节点清单：http(s) 地址或本地文件路径。
    pub registry: String,
    /// 信任反向代理的 `X-Forwarded-For`（只在前面确有代理时开启）。
    pub trust_proxy: bool,
    pub pow: PowCfg,
    pub index: IndexCfg,
    pub limits: Limits,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct PowCfg {
    pub mailbox_bits: u32,
    pub register_bits: u32,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct IndexCfg {
    pub max_per_bucket: usize,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Limits {
    pub mailboxes_per_ip_day: u32,
    pub aliases_per_mailbox_day: u32,
    pub ohttp_per_mailbox_hour: u32,
    pub probes_out_per_mailbox_min: u32,
    pub probes_in_per_alias_hour: u32,
    pub envelopes_in_per_mailbox_min: u32,
    pub max_connections: usize,
}

impl Default for Config {
    fn default() -> Self {
        Config { listen: "0.0.0.0:7800".into(), url: String::new(), registry: "registry.json".into(), trust_proxy: false, pow: PowCfg::default(), index: IndexCfg::default(), limits: Limits::default() }
    }
}
impl Default for PowCfg {
    fn default() -> Self {
        PowCfg { mailbox_bits: 20, register_bits: 16 }
    }
}
impl Default for IndexCfg {
    fn default() -> Self {
        IndexCfg { max_per_bucket: 8192 }
    }
}
impl Default for Limits {
    fn default() -> Self {
        Limits {
            mailboxes_per_ip_day: 10,
            aliases_per_mailbox_day: 20_000,
            ohttp_per_mailbox_hour: 600,
            probes_out_per_mailbox_min: 60,
            probes_in_per_alias_hour: 30,
            envelopes_in_per_mailbox_min: 600,
            max_connections: 20_000,
        }
    }
}
