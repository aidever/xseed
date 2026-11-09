//! 凭证加密存储：XChaCha20-Poly1305，密钥来自数据目录中权限 600 的随机密钥文件（03 §3.1）。
//! 密文绑定用途（associated data），防止把一条密文挪作他用。

use crate::db::{now, Db};
use anyhow::{anyhow, Context, Result};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use rand::RngCore;
use rusqlite::{params, OptionalExtension};
use std::path::Path;

#[derive(Clone)]
pub struct Vault {
    cipher: XChaCha20Poly1305,
    db: Db,
}

impl Vault {
    pub fn open(dir: &Path, db: Db) -> Result<Self> {
        let path = dir.join("vault.key");
        let key = if path.exists() {
            let k = std::fs::read(&path).context("读取 vault.key")?;
            if k.len() != 32 {
                return Err(anyhow!("vault.key 长度不对"));
            }
            k
        } else {
            let mut k = vec![0u8; 32];
            rand::rngs::OsRng.fill_bytes(&mut k);
            write_private(&path, &k)?;
            k
        };
        Ok(Vault { cipher: XChaCha20Poly1305::new_from_slice(&key).unwrap(), db })
    }

    #[cfg(test)]
    pub fn memory(db: Db) -> Self {
        Vault { cipher: XChaCha20Poly1305::new_from_slice(&[9u8; 32]).unwrap(), db }
    }

    fn seal(&self, aad: &str, plain: &[u8]) -> (Vec<u8>, Vec<u8>) {
        let mut nonce = [0u8; 24];
        rand::rngs::OsRng.fill_bytes(&mut nonce);
        let ct = self.cipher.encrypt(XNonce::from_slice(&nonce), Payload { msg: plain, aad: aad.as_bytes() }).expect("encrypt");
        (nonce.to_vec(), ct)
    }

    fn open_ct(&self, aad: &str, nonce: &[u8], ct: &[u8]) -> Result<Vec<u8>> {
        if nonce.len() != 24 {
            return Err(anyhow!("nonce 长度不对"));
        }
        self.cipher.decrypt(XNonce::from_slice(nonce), Payload { msg: ct, aad: aad.as_bytes() }).map_err(|_| anyhow!("解密失败"))
    }

    pub fn set_secret(&self, name: &str, value: &str) -> Result<()> {
        let (n, ct) = self.seal(&format!("secret/{name}"), value.as_bytes());
        self.db.lock().execute("INSERT INTO secrets(name,nonce,ct) VALUES(?1,?2,?3) ON CONFLICT(name) DO UPDATE SET nonce=excluded.nonce, ct=excluded.ct", params![name, n, ct])?;
        Ok(())
    }

    pub fn secret(&self, name: &str) -> Option<String> {
        let row: Option<(Vec<u8>, Vec<u8>)> = self.db.lock().query_row("SELECT nonce,ct FROM secrets WHERE name=?1", [name], |r| Ok((r.get(0)?, r.get(1)?))).optional().ok()?;
        let (n, ct) = row?;
        String::from_utf8(self.open_ct(&format!("secret/{name}"), &n, &ct).ok()?).ok()
    }

    pub fn delete_secret(&self, name: &str) -> Result<()> {
        self.db.lock().execute("DELETE FROM secrets WHERE name=?1", [name])?;
        Ok(())
    }

    pub fn set_credential(&self, site_ref: &str, kind: &str, source: &str, value: &str) -> Result<()> {
        let (n, ct) = self.seal(&format!("cred/{site_ref}/{kind}/{source}"), value.as_bytes());
        self.db.lock().execute(
            "INSERT INTO credentials(site_ref,kind,source,nonce,ct,updated_at) VALUES(?1,?2,?3,?4,?5,?6)
             ON CONFLICT(site_ref,kind,source) DO UPDATE SET nonce=excluded.nonce, ct=excluded.ct, updated_at=excluded.updated_at",
            params![site_ref, kind, source, n, ct, now()],
        )?;
        Ok(())
    }

    pub fn credential(&self, site_ref: &str, kind: &str, source: &str) -> Option<String> {
        let (n, ct): (Vec<u8>, Vec<u8>) = self
            .db
            .lock()
            .query_row("SELECT nonce,ct FROM credentials WHERE site_ref=?1 AND kind=?2 AND source=?3", params![site_ref, kind, source], |r| Ok((r.get(0)?, r.get(1)?)))
            .ok()?;
        String::from_utf8(self.open_ct(&format!("cred/{site_ref}/{kind}/{source}"), &n, &ct).ok()?).ok()
    }
}

pub fn write_private(path: &Path, data: &[u8]) -> Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(path)?;
        f.write_all(data)?;
    }
    #[cfg(not(unix))]
    std::fs::write(path, data)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_binding() {
        let db = Db::memory();
        let v = Vault::memory(db.clone());
        v.set_secret("a", "hello").unwrap();
        assert_eq!(v.secret("a").as_deref(), Some("hello"));
        v.set_credential("site:y", "cookie", "ptd", "c=1").unwrap();
        assert_eq!(v.credential("site:y", "cookie", "ptd").as_deref(), Some("c=1"));
        // 把密文挪到别的站点名下必须解不开
        db.lock().execute("UPDATE credentials SET site_ref='site:z'", []).unwrap();
        assert_eq!(v.credential("site:z", "cookie", "ptd"), None);
    }
}
