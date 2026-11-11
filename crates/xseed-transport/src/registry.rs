//! 注册中心清单（03 §1.13）。
//!
//! 发布物为 `registry.json` 与同目录的 `registry.json.sig`：
//! `sig = Ed25519(主私钥, "xseed/registry/v1" ‖ 0x00 ‖ registry.json 原始字节)`，`.sig` 文件内容为
//! `{"key": "ed25519:<hex>", "sig": "<hex>"}`。经网络获取的清单必须带有受信任公钥的签名；本地文件可以不签名（自建测试网络）。

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use xseed_core::hrw::{self, NodeInfo};

/// 官方注册中心地址（配置项 `registry` 为空时使用），仓库 aidever/xseed-registry。
pub const OFFICIAL_REGISTRY: &str = "https://aidever.github.io/xseed-registry/registry.json";
/// 内置受信任的注册中心公钥（主公钥与备用公钥）。用户自建注册中心时在配置中追加 `registry_keys`。
pub const OFFICIAL_KEYS: &[&str] = &[
    "ed25519:4a8a494445a09b8e16ae8eae3002fce4c5cfd6a5b3a850590af9464a4940bc72", // 主
    "ed25519:650d91272c4c9acb14ba1d2545d8197b580212c03047d7845b1f6b0919e3cd13", // 备用
];

/// 受信任公钥：内置公钥 + 配置追加的公钥。
pub fn trusted_keys(extra: &[String]) -> Vec<String> {
    OFFICIAL_KEYS.iter().map(|s| s.to_string()).chain(extra.iter().cloned()).collect()
}

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

/// 站点定义（03 §1.13.3），字段为明文（不再使用 ROT13）。
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct SiteEntry {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub domains: Vec<String>,
    #[serde(default)]
    pub tracker_hosts: Vec<String>,
    /// allow | deny | unknown
    #[serde(default = "unknown")]
    pub p2p: String,
    #[serde(default = "yes")]
    pub allow_skip_check: bool,
    /// 下载地址模板，`{id}` 与 `{passkey}` 在使用时替换；为空时按通用规则推导。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub download_template: Option<String>,
}

fn unknown() -> String {
    "unknown".into()
}
fn yes() -> bool {
    true
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Registry {
    #[serde(default)]
    pub version: u64,
    #[serde(default)]
    pub generated_at: u64,
    pub nodes: Vec<NodeManifest>,
    #[serde(default)]
    pub sites: Vec<SiteEntry>,
    /// 公共 tracker 主机（规范化形式），不能作为站点身份。
    #[serde(default)]
    pub public_trackers: Vec<String>,
}

/// 签名文件内容。
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Signature {
    /// `ed25519:<hex>`
    pub key: String,
    pub sig: String,
}

const SIG_CTX: &[u8] = b"xseed/registry/v1\0";

pub fn sign(sk: &ed25519_dalek::SigningKey, body: &[u8]) -> Signature {
    use ed25519_dalek::Signer;
    let sig = sk.sign(&[SIG_CTX, body].concat());
    Signature { key: format!("ed25519:{}", hex::encode(sk.verifying_key().to_bytes())), sig: hex::encode(sig.to_bytes()) }
}

/// 用受信任公钥之一验证签名；返回验证通过的公钥（`ed25519:<hex>`）。
pub fn verify(trusted: &[String], body: &[u8], s: &Signature) -> Result<String> {
    let norm = |k: &str| k.trim().trim_start_matches("ed25519:").to_ascii_lowercase();
    let key = norm(&s.key);
    if !trusted.iter().any(|t| norm(t) == key) {
        bail!("签名公钥不受信任：{}", s.key);
    }
    let pk = ed25519_dalek::VerifyingKey::from_bytes(&hex_key::<32>(&key, "")?)?;
    let sig = ed25519_dalek::Signature::from_bytes(&hex::decode(&s.sig)?.try_into().map_err(|_| anyhow!("签名长度错误"))?);
    pk.verify_strict(&[SIG_CTX, body].concat(), &sig).map_err(|_| anyhow!("节点清单签名无效"))?;
    Ok(format!("ed25519:{key}"))
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

/// 读取到的清单与其来源信息。
pub struct Loaded {
    pub registry: Registry,
    /// 验证通过的签名公钥；本地未签名文件为 None。
    pub signed_by: Option<String>,
}

async fn fetch(src: &str, http: &reqwest::Client) -> Result<Option<Vec<u8>>> {
    if src.starts_with("http://") || src.starts_with("https://") {
        let r = http.get(src).send().await?;
        if r.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        Ok(Some(r.error_for_status()?.bytes().await?.to_vec()))
    } else {
        match tokio::fs::read(src).await {
            Ok(b) => Ok(Some(b)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).with_context(|| format!("读取 {src}")),
        }
    }
}

/// 解析并验证清单字节。`sig` 为同目录 `.sig` 文件内容（若有）。
pub fn parse(body: &[u8], sig: Option<&[u8]>, trusted: &[String], require_sig: bool) -> Result<Loaded> {
    let signed_by = match sig {
        Some(s) => Some(verify(trusted, body, &serde_json::from_slice(s).context("签名文件格式错误")?)?),
        None if require_sig => bail!("节点清单没有签名（缺少 .sig 文件）"),
        None => None,
    };
    let r: Registry = serde_json::from_slice(body).context("节点清单格式错误")?;
    r.validate()?;
    Ok(Loaded { registry: r, signed_by })
}

/// 从 http(s) 地址或本地文件读取清单。经网络获取时必须带有受信任公钥的签名。
pub async fn load_signed(src: &str, http: &reqwest::Client, trusted: &[String]) -> Result<Loaded> {
    let body = fetch(src, http).await?.ok_or_else(|| anyhow!("节点清单不存在：{src}"))?;
    let sig = fetch(&format!("{src}.sig"), http).await?;
    let remote = src.starts_with("http://") || src.starts_with("https://");
    parse(&body, sig.as_deref(), trusted, remote)
}

/// 读取清单（不要求签名、不检查公钥），用于节点读取运营者自己指定的清单与测试。
pub async fn load(src: &str, http: &reqwest::Client) -> Result<Registry> {
    let body = fetch(src, http).await?.ok_or_else(|| anyhow!("节点清单不存在：{src}"))?;
    let r: Registry = serde_json::from_slice(&body).context("节点清单格式错误")?;
    r.validate()?;
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn official_keys_are_valid() {
        assert!(OFFICIAL_REGISTRY.starts_with("https://"));
        assert!(OFFICIAL_KEYS.len() >= 2, "主公钥 + 备用公钥");
        for k in OFFICIAL_KEYS {
            assert!(k.starts_with("ed25519:"), "{k}");
            ed25519_dalek::VerifyingKey::from_bytes(&hex_key::<32>(k, "ed25519:").unwrap()).unwrap();
        }
    }

    #[test]
    fn signature_round_trip_and_tamper() {
        let sk = ed25519_dalek::SigningKey::generate(&mut rand::rngs::OsRng);
        let body = br#"{"version":3,"generated_at":1,"nodes":[]}"#;
        let s = sign(&sk, body);
        let trusted = vec![s.key.clone()];
        let sig = serde_json::to_vec(&s).unwrap();
        let l = parse(body, Some(&sig), &trusted, true).unwrap();
        assert_eq!(l.registry.version, 3);
        assert_eq!(l.signed_by.as_deref(), Some(s.key.as_str()));
        // 篡改正文
        assert!(parse(br#"{"version":4,"generated_at":1,"nodes":[]}"#, Some(&sig), &trusted, true).is_err());
        // 不受信任的公钥
        let other = ed25519_dalek::SigningKey::generate(&mut rand::rngs::OsRng);
        assert!(parse(body, Some(&serde_json::to_vec(&sign(&other, body)).unwrap()), &trusted, true).is_err());
        // 网络来源缺少签名
        assert!(parse(body, None, &trusted, true).is_err());
        assert!(parse(body, None, &trusted, false).unwrap().signed_by.is_none());
    }
}
