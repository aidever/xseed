//! PT-depiler 备份解析（03 §1.12.2）。格式依据 PT-depiler `src/packages/backupServer/utils.ts`：
//! - zip 内 `manifest.json`：`{version:"PT-Depiler (x)", encryption:bool, files:{<key>:{name,hash}}}`
//! - `hash` = 文件内容（字符串，加密时为 Base64 密文）的 MD5 十六进制
//! - 加密：`aesEncryptOpenSsl(json, md5Hex(userKey)[0..16])`，即 crypto-js 口令格式
//!   （`Salted__` + 8 字节 salt，EVP_BytesToKey(MD5) 派生 AES-256-CBC 的 key 与 IV，Base64）
//! - `cookies.json`：`Record<host, chrome.cookies.Cookie[]>`
//! - `metadata.json`：`{sites: Record<siteKey, {...}>, siteHostMap: Record<host, siteKey>, ...}`

use crate::db::{now, Db};
use crate::sites;
use crate::vault::Vault;
use aes::cipher::{block_padding::Pkcs7, BlockDecryptMut, KeyIvInit};
use base64::Engine;
use md5::{Digest, Md5};
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::io::Read;
use xseed_core::site::SiteDef;

pub const MAX_UPLOAD: usize = 64 << 20;
const MAX_ENTRY: u64 = 32 << 20;
const MAX_TOTAL: u64 = 64 << 20;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PtdError {
    #[error("ptd_format_unknown")]
    Format(String),
    #[error("ptd_decrypt_failed")]
    Decrypt,
    #[error("ptd_hash_mismatch")]
    Hash,
}

#[derive(Deserialize)]
struct Manifest {
    #[serde(default)]
    version: String,
    #[serde(default)]
    encryption: bool,
    #[serde(default)]
    files: BTreeMap<String, ManifestFile>,
}

#[derive(Deserialize)]
struct ManifestFile {
    name: String,
    hash: String,
}

#[derive(Debug, Default)]
pub struct Parsed {
    pub version: String,
    /// host → Cookie 头
    pub cookies: BTreeMap<String, String>,
    /// siteKey → 显示名
    pub sites: BTreeMap<String, String>,
    /// host → siteKey
    pub host_map: BTreeMap<String, String>,
}

fn md5_hex(b: &[u8]) -> String {
    hex::encode(Md5::digest(b))
}

fn evp_bytes_to_key(pass: &[u8], salt: &[u8]) -> ([u8; 32], [u8; 16]) {
    let mut out = Vec::with_capacity(48);
    let mut prev: Vec<u8> = Vec::new();
    while out.len() < 48 {
        let mut h = Md5::new();
        h.update(&prev);
        h.update(pass);
        h.update(salt);
        prev = h.finalize().to_vec();
        out.extend_from_slice(&prev);
    }
    (out[..32].try_into().unwrap(), out[32..48].try_into().unwrap())
}

/// crypto-js 口令格式解密；口令为 `md5Hex(userKey)[0..16]`。
pub fn decrypt(ct_b64: &str, user_key: &str) -> Result<String, PtdError> {
    let pass = &md5_hex(user_key.as_bytes())[..16];
    let data = base64::engine::general_purpose::STANDARD.decode(ct_b64.trim()).map_err(|_| PtdError::Decrypt)?;
    let (salt, body) = if data.len() >= 16 && &data[..8] == b"Salted__" { (&data[8..16], &data[16..]) } else { (&data[..0], &data[..]) };
    let (key, iv) = evp_bytes_to_key(pass.as_bytes(), salt);
    let plain = cbc::Decryptor::<aes::Aes256>::new(&key.into(), &iv.into()).decrypt_padded_vec_mut::<Pkcs7>(body).map_err(|_| PtdError::Decrypt)?;
    String::from_utf8(plain).map_err(|_| PtdError::Decrypt)
}

fn read_entry(zip: &mut zip::ZipArchive<std::io::Cursor<&[u8]>>, name: &str, total: &mut u64) -> Result<Option<String>, PtdError> {
    let mut f = match zip.by_name(name) {
        Ok(f) => f,
        Err(_) => return Ok(None),
    };
    let mut buf = Vec::new();
    (&mut f).take(MAX_ENTRY + 1).read_to_end(&mut buf).map_err(|e| PtdError::Format(e.to_string()))?;
    if buf.len() as u64 > MAX_ENTRY {
        return Err(PtdError::Format(format!("{name} 太大")));
    }
    *total += buf.len() as u64;
    if *total > MAX_TOTAL {
        return Err(PtdError::Format("解压后总大小超限".into()));
    }
    String::from_utf8(buf).map(Some).map_err(|_| PtdError::Format(format!("{name} 不是 UTF-8")))
}

pub fn parse(zip_bytes: &[u8], key: Option<&str>) -> Result<Parsed, PtdError> {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(zip_bytes)).map_err(|e| PtdError::Format(e.to_string()))?;
    let mut total = 0;
    let manifest: Manifest = serde_json::from_str(&read_entry(&mut zip, "manifest.json", &mut total)?.ok_or_else(|| PtdError::Format("缺少 manifest.json".into()))?).map_err(|e| PtdError::Format(e.to_string()))?;
    if !manifest.version.starts_with("PT-Depiler (") {
        return Err(PtdError::Format(format!("version={}", manifest.version)));
    }
    let mut load = |k: &str| -> Result<Option<Value>, PtdError> {
        let Some(mf) = manifest.files.get(k) else { return Ok(None) };
        if mf.name != format!("{k}.json") {
            return Err(PtdError::Format(format!("意外的文件名 {}", mf.name)));
        }
        let Some(content) = read_entry(&mut zip, &mf.name, &mut total)? else { return Ok(None) };
        if md5_hex(content.as_bytes()) != mf.hash {
            return Err(PtdError::Hash);
        }
        let json = if manifest.encryption {
            let key = key.filter(|k| !k.is_empty()).ok_or(PtdError::Decrypt)?;
            decrypt(&content, key)?
        } else {
            content
        };
        serde_json::from_str(&json).map(Some).map_err(|e| PtdError::Format(e.to_string()))
    };
    let cookies = load("cookies")?;
    let metadata = load("metadata")?;

    let mut out = Parsed { version: manifest.version, ..Default::default() };
    if let Some(Value::Object(m)) = cookies {
        for (host, list) in m {
            let header = list
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|c| Some(format!("{}={}", c.get("name")?.as_str()?, c.get("value")?.as_str()?)))
                        .collect::<Vec<_>>()
                        .join("; ")
                })
                .unwrap_or_default();
            if !header.is_empty() {
                out.cookies.insert(host.trim_start_matches('.').to_ascii_lowercase(), header);
            }
        }
    }
    if let Some(md) = metadata {
        if let Some(Value::Object(s)) = md.get("sites") {
            for (k, v) in s {
                let name = v.pointer("/merge/name").and_then(Value::as_str).unwrap_or(k).to_string();
                out.sites.insert(k.clone(), name);
            }
        }
        if let Some(Value::Object(h)) = md.get("siteHostMap") {
            for (host, k) in h {
                if let Some(k) = k.as_str() {
                    out.host_map.insert(host.to_ascii_lowercase(), k.to_string());
                }
            }
        }
    }
    Ok(out)
}

#[derive(Debug, serde::Serialize, serde::Deserialize, Default, Clone)]
pub struct SyncResult {
    pub at: i64,
    pub ok: bool,
    pub sites: usize,
    pub cookies: usize,
    pub error: Option<String>,
    pub version: String,
}

/// 写入站点定义与 Cookie。只覆盖来源为 ptd 的数据。
pub fn apply(db: &Db, vault: &Vault, p: &Parsed) -> SyncResult {
    let mut domains: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for (host, key) in &p.host_map {
        domains.entry(key.as_str()).or_default().push(host.clone());
    }
    for (key, ds) in &domains {
        let def = SiteDef { id: key.to_string(), name: p.sites.get(*key).cloned().unwrap_or_else(|| key.to_string()), domains: ds.clone(), tracker_hosts: vec![] };
        let _ = sites::upsert_def(db, &def, "ptd");
    }
    let mut n = 0;
    for (host, header) in &p.cookies {
        if let Some(key) = p.host_map.get(host) {
            if vault.set_credential(&format!("site:{key}"), "cookie", "ptd", header).is_ok() {
                n += 1;
            }
        }
    }
    SyncResult { at: now(), ok: true, sites: domains.len(), cookies: n, error: None, version: p.version.clone() }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use std::io::Write;

    /// 按 PT-depiler 的方式加密（固定 salt，便于测试）。
    pub fn encrypt(plain: &str, user_key: &str) -> String {
        use aes::cipher::BlockEncryptMut;
        let pass = &md5_hex(user_key.as_bytes())[..16];
        let salt = *b"12345678";
        let (key, iv) = evp_bytes_to_key(pass.as_bytes(), &salt);
        let ct = cbc::Encryptor::<aes::Aes256>::new(&key.into(), &iv.into()).encrypt_padded_vec_mut::<Pkcs7>(plain.as_bytes());
        let mut all = b"Salted__".to_vec();
        all.extend_from_slice(&salt);
        all.extend_from_slice(&ct);
        base64::engine::general_purpose::STANDARD.encode(all)
    }

    pub fn make_zip(key: Option<&str>) -> Vec<u8> {
        let cookies = r#"{"www.site-y.example":[{"name":"uid","value":"1"},{"name":"pass","value":"abc"}]}"#;
        let metadata = r#"{"sites":{"sitey":{"id":"sitey","merge":{"name":"站点 Y"}}},"siteHostMap":{"www.site-y.example":"sitey","site-y.example":"sitey"}}"#;
        let enc = |s: &str| key.map_or(s.to_string(), |k| encrypt(s, k));
        let (c, m) = (enc(cookies), enc(metadata));
        let manifest = serde_json::json!({
            "version": "PT-Depiler (0.0.5)", "time": 1, "encryption": key.is_some(),
            "files": {"cookies": {"name": "cookies.json", "hash": md5_hex(c.as_bytes())}, "metadata": {"name": "metadata.json", "hash": md5_hex(m.as_bytes())}}
        });
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut z = zip::ZipWriter::new(&mut buf);
            let o = zip::write::SimpleFileOptions::default();
            for (n, d) in [("cookies.json", c), ("metadata.json", m), ("manifest.json", manifest.to_string())] {
                z.start_file(n, o).unwrap();
                z.write_all(d.as_bytes()).unwrap();
            }
            z.finish().unwrap();
        }
        buf.into_inner()
    }

    #[test]
    fn plain_backup() {
        let p = parse(&make_zip(None), None).unwrap();
        assert_eq!(p.cookies["www.site-y.example"], "uid=1; pass=abc");
        assert_eq!(p.sites["sitey"], "站点 Y");
        assert_eq!(p.host_map["site-y.example"], "sitey");
    }

    #[test]
    fn encrypted_backup() {
        let z = make_zip(Some("my secret"));
        assert_eq!(parse(&z, Some("my secret")).unwrap().cookies.len(), 1);
        assert_eq!(parse(&z, Some("wrong")).unwrap_err(), PtdError::Decrypt);
        assert_eq!(parse(&z, None).unwrap_err(), PtdError::Decrypt);
    }

    #[test]
    fn known_crypto_js_vector() {
        // 独立向量：openssl enc -aes-256-cbc -md md5 -S 3132333435363738 -pass pass:$(md5('k')[0..16])，
        // 与 crypto-js 口令格式相同（OpenSSL EVP_BytesToKey MD5）
        let ct = "U2FsdGVkX18xMjM0NTY3OITIbu67U4JxOoleTqiTB7Y=";
        assert_eq!(decrypt(ct, "k").unwrap(), r#"{"a":1}"#);
        assert_eq!(encrypt(r#"{"a":1}"#, "k"), ct);
    }

    #[test]
    fn apply_writes_defs_and_cookies() {
        let db = Db::memory();
        let v = Vault::memory(db.clone());
        let r = apply(&db, &v, &parse(&make_zip(None), None).unwrap());
        assert_eq!((r.sites, r.cookies), (1, 1));
        assert_eq!(v.credential("site:sitey", "cookie", "ptd").as_deref(), Some("uid=1; pass=abc"));
        let defs = sites::load_defs(&db);
        assert_eq!(defs[0].domains, vec!["site-y.example".to_string(), "www.site-y.example".to_string()]);
    }

    #[test]
    fn rejects_unknown_format() {
        assert!(matches!(parse(b"not a zip", None), Err(PtdError::Format(_))));
    }
}
