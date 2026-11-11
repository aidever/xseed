//! 注册中心维护工具（03 §1.13）：生成签名密钥、汇总清单、签名与验证。

use anyhow::{anyhow, bail, Context, Result};
use std::path::Path;
use xseed_transport::registry::{self, NodeManifest, Registry, SiteEntry};

pub fn run(args: &[String]) -> Result<()> {
    let a = |i: usize| args.get(i).map(String::as_str);
    match (a(0), a(1), a(2)) {
        (Some("keygen"), Some(file), _) => keygen(Path::new(file)),
        (Some("build"), Some(dir), out) => {
            let r = build(Path::new(dir), xseed_transport::now())?;
            let json = serde_json::to_string_pretty(&r)? + "\n";
            match out {
                Some(o) => std::fs::write(o, json).with_context(|| format!("写入 {o}"))?,
                None => print!("{json}"),
            }
            eprintln!("版本 {}：{} 个节点、{} 个站点、{} 个公共 tracker", r.version, r.nodes.len(), r.sites.len(), r.public_trackers.len());
            Ok(())
        }
        (Some("sign"), Some(key), Some(file)) => {
            let sk = read_key(Path::new(key))?;
            let body = std::fs::read(file).with_context(|| format!("读取 {file}"))?;
            serde_json::from_slice::<Registry>(&body).context("清单格式错误")?.validate()?;
            let s = registry::sign(&sk, &body);
            std::fs::write(format!("{file}.sig"), serde_json::to_string_pretty(&s)? + "\n")?;
            eprintln!("已写出 {file}.sig（{}）", s.key);
            Ok(())
        }
        (Some("verify"), Some(file), Some(_)) => {
            let body = std::fs::read(file).with_context(|| format!("读取 {file}"))?;
            let sig = std::fs::read(format!("{file}.sig")).with_context(|| format!("读取 {file}.sig"))?;
            let l = registry::parse(&body, Some(&sig), &args[2..], true)?;
            println!("签名有效（{}）：版本 {}，{} 个节点、{} 个站点", l.signed_by.unwrap_or_default(), l.registry.version, l.registry.nodes.len(), l.registry.sites.len());
            Ok(())
        }
        _ => bail!("用法：xseed-node registry keygen KEY_FILE | build DIR [OUT] | sign KEY_FILE REGISTRY | verify REGISTRY PUBKEY..."),
    }
}

fn keygen(file: &Path) -> Result<()> {
    if file.exists() {
        bail!("{} 已存在，不会覆盖", file.display());
    }
    let sk = ed25519_dalek::SigningKey::generate(&mut rand::rngs::OsRng);
    write_private(file, &hex::encode(sk.to_bytes()))?;
    println!("ed25519:{}", hex::encode(sk.verifying_key().to_bytes()));
    eprintln!("私钥已写入 {}，请离线妥善保存；上面一行是公钥", file.display());
    Ok(())
}

#[cfg(unix)]
fn write_private(file: &Path, s: &str) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(file)?;
    f.write_all(s.as_bytes())?;
    Ok(())
}

#[cfg(not(unix))]
fn write_private(file: &Path, s: &str) -> Result<()> {
    std::fs::write(file, s)?;
    Ok(())
}

fn read_key(file: &Path) -> Result<ed25519_dalek::SigningKey> {
    let s = std::fs::read_to_string(file).with_context(|| format!("读取 {}", file.display()))?;
    let b: [u8; 32] = hex::decode(s.trim())?.try_into().map_err(|_| anyhow!("私钥长度错误"))?;
    Ok(ed25519_dalek::SigningKey::from_bytes(&b))
}

fn read_dir<T: serde::de::DeserializeOwned>(dir: &Path) -> Result<Vec<T>> {
    let mut files: Vec<_> = match std::fs::read_dir(dir) {
        Ok(rd) => rd.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.extension().is_some_and(|x| x == "json")).collect(),
        Err(_) => return Ok(vec![]),
    };
    files.sort();
    files.iter().map(|p| serde_json::from_str(&std::fs::read_to_string(p)?).with_context(|| format!("{} 格式错误", p.display()))).collect()
}

/// 汇总注册中心仓库目录。版本号取生成时间，保证单调递增。
pub fn build(dir: &Path, now: u64) -> Result<Registry> {
    let nodes: Vec<NodeManifest> = read_dir(&dir.join("nodes"))?;
    let sites: Vec<SiteEntry> = read_dir(&dir.join("sites"))?;
    let mut ids = std::collections::BTreeSet::new();
    for s in &sites {
        if !ids.insert(&s.id) {
            bail!("站点 ID 重复：{}", s.id);
        }
        if !matches!(s.p2p.as_str(), "allow" | "deny" | "unknown") {
            bail!("站点 {} 的 p2p 只能是 allow / deny / unknown", s.id);
        }
    }
    let public_trackers = std::fs::read_to_string(dir.join("public-trackers.txt"))
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(String::from)
        .collect();
    let r = Registry { version: now, generated_at: now, nodes, sites, public_trackers };
    r.validate()?;
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_sign_verify() {
        let d = std::env::temp_dir().join(format!("xseed-reg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("nodes")).unwrap();
        std::fs::create_dir_all(d.join("sites")).unwrap();
        let keys = crate::node::Keys::load_or_create(&d).unwrap();
        let n = crate::node::Node::new(&d, crate::config::Config::default(), &keys).unwrap();
        std::fs::write(d.join("nodes/a.json"), serde_json::to_string(&n.manifest("https://n.example.org")).unwrap()).unwrap();
        std::fs::write(d.join("sites/ex.json"), r#"{"id":"ex","name":"Example","domains":["example.org"],"p2p":"allow"}"#).unwrap();
        std::fs::write(d.join("public-trackers.txt"), "# 注释\ntracker.opentrackr.org:1337\n").unwrap();
        let r = build(&d, 100).unwrap();
        assert_eq!((r.version, r.nodes.len(), r.sites.len(), r.public_trackers.len()), (100, 1, 1, 1));
        assert!(r.sites[0].allow_skip_check);

        let key = d.join("reg.key");
        keygen(&key).unwrap();
        assert!(keygen(&key).is_err(), "不覆盖已有私钥");
        let sk = read_key(&key).unwrap();
        let body = serde_json::to_vec(&r).unwrap();
        let sig = serde_json::to_vec(&registry::sign(&sk, &body)).unwrap();
        let pk = format!("ed25519:{}", hex::encode(sk.verifying_key().to_bytes()));
        assert!(registry::parse(&body, Some(&sig), &[pk], true).is_ok());
        std::fs::write(d.join("sites/dup.json"), r#"{"id":"ex","name":"Dup"}"#).unwrap();
        assert!(build(&d, 101).is_err(), "站点 ID 重复");
        let _ = std::fs::remove_dir_all(&d);
    }
}
