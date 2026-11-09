//! 注册中心清单（03 §1.13）。M2 使用未签名的 JSON 清单；签名与发布流程在 M5 实现。

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use xseed_core::hrw::{self, NodeInfo};

/// 每个桶的登记副本数。
pub const REPLICAS: usize = 2;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct NodeManifest {
    /// 16 字节十六进制；前 4 字节即 node_tag。
    pub id: String,
    pub url: String,
    #[serde(default = "one")]
    pub weight: f64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub region: String,
    /// `ed25519:<hex>`
    pub sign_pk: String,
    /// `x25519:<hex>`
    pub hpke_pk: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub operator: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub contact: String,
    /// 上线时间（Unix 秒），用于权重爬坡。
    #[serde(default)]
    pub since: u64,
}

fn one() -> f64 {
    1.0
}

fn hex_key<const N: usize>(s: &str, prefix: &str) -> Result<[u8; N]> {
    let h = s.strip_prefix(prefix).unwrap_or(s);
    hex::decode(h)?.try_into().map_err(|_| anyhow!("密钥长度错误"))
}

/// `node_id = H("xseed/node/v1" ‖ sign_pk)[0..16]`
pub fn node_id(sign_pk: &[u8; 32]) -> [u8; 16] {
    crate::sha256(&[b"xseed/node/v1", sign_pk])[..16].try_into().unwrap()
}

pub fn tag_of(id: &[u8; 16]) -> u32 {
    u32::from_be_bytes(id[0..4].try_into().unwrap())
}

impl NodeManifest {
    pub fn id_bytes(&self) -> Result<[u8; 16]> {
        hex::decode(&self.id)?.try_into().map_err(|_| anyhow!("节点 ID 长度错误"))
    }
    pub fn tag(&self) -> u32 {
        self.id_bytes().map(|i| tag_of(&i)).unwrap_or(0)
    }
    pub fn sign_key(&self) -> Result<ed25519_dalek::VerifyingKey> {
        Ok(ed25519_dalek::VerifyingKey::from_bytes(&hex_key::<32>(&self.sign_pk, "ed25519:")?)?)
    }
    pub fn hpke_key(&self) -> Result<[u8; 32]> {
        hex_key(&self.hpke_pk, "x25519:")
    }
    pub fn effective_weight(&self, now: u64) -> f64 {
        let age = now.saturating_sub(self.since) as f64 / 86_400.0;
        self.weight.max(0.0) * if self.since == 0 { 1.0 } else { hrw::ramp(age) }
    }
    /// 显示用短名：URL 的主机部分。
    pub fn host(&self) -> String {
        url_host(&self.url)
    }
}

pub fn url_host(u: &str) -> String {
    let s = u.split("://").nth(1).unwrap_or(u);
    s.split('/').next().unwrap_or(s).to_string()
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Registry {
    #[serde(default)]
    pub version: u64,
    #[serde(default)]
    pub generated_at: u64,
    pub nodes: Vec<NodeManifest>,
}

impl Registry {
    /// 检查 ID 与签名公钥一致、node_tag 唯一。
    pub fn validate(&self) -> Result<()> {
        let mut tags = std::collections::BTreeSet::new();
        for n in &self.nodes {
            let id = n.id_bytes().with_context(|| format!("节点 {}", n.id))?;
            if node_id(&n.sign_key()?.to_bytes()) != id {
                bail!("节点 {} 的 ID 与签名公钥不符", n.id);
            }
            n.hpke_key()?;
            if !tags.insert(tag_of(&id)) {
                bail!("node_tag 重复：{}", n.id);
            }
        }
        Ok(())
    }

    pub fn by_tag(&self, tag: u32) -> Option<&NodeManifest> {
        self.nodes.iter().find(|n| n.tag() == tag)
    }

    pub fn by_id(&self, id: &[u8; 16]) -> Option<&NodeManifest> {
        self.nodes.iter().find(|n| n.id_bytes().ok().as_ref() == Some(id))
    }

    /// 桶的所有者节点（HRW，按优先级排序）。
    pub fn owners(&self, bucket: u16, now: u64) -> Vec<&NodeManifest> {
        let infos: Vec<NodeInfo> = self.nodes.iter().map(|n| NodeInfo { id: n.id_bytes().unwrap_or_default(), weight: n.effective_weight(now) }).collect();
        hrw::owners(bucket, &infos, REPLICAS).into_iter().map(|i| &self.nodes[i]).collect()
    }
}

/// 从 http(s) 地址或本地文件读取清单。
pub async fn load(src: &str, http: &reqwest::Client) -> Result<Registry> {
    let text = if src.starts_with("http://") || src.starts_with("https://") {
        http.get(src).send().await?.error_for_status()?.text().await?
    } else {
        tokio::fs::read_to_string(src).await.with_context(|| format!("读取 {src}"))?
    };
    let r: Registry = serde_json::from_str(&text).context("节点清单格式错误")?;
    r.validate()?;
    Ok(r)
}
