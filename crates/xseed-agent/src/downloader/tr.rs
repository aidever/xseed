//! Transmission RPC（03 §2.5）。

use super::{base, http, real_tracker, AddRequest, Announce, Health, LocalTorrent, Meta, Status, TAG};
use anyhow::{anyhow, bail, Result};
use base64::Engine;
use reqwest::StatusCode;
use serde_json::{json, Value};
use tokio::sync::Mutex;

pub struct Tr {
    rpc: url::Url,
    user: String,
    pass: String,
    http: reqwest::Client,
    sid: Mutex<String>,
}

impl Tr {
    pub fn new(url: &str, user: &str, pass: &str) -> Result<Self> {
        let b = base(url)?;
        // 允许用户填 http://host:9091 或完整的 .../transmission/rpc
        let rpc = if b.path().ends_with("/rpc/") { url::Url::parse(url.trim())? } else { b.join("transmission/rpc")? };
        Ok(Tr { rpc, user: user.into(), pass: pass.into(), http: http().build()?, sid: Mutex::new(String::new()) })
    }

    async fn call(&self, method: &str, args: Value) -> Result<Value> {
        let body = json!({"method": method, "arguments": args});
        for _ in 0..2 {
            let sid = self.sid.lock().await.clone();
            let mut rb = self.http.post(self.rpc.clone()).header("X-Transmission-Session-Id", sid).json(&body);
            if !self.user.is_empty() || !self.pass.is_empty() {
                rb = rb.basic_auth(&self.user, Some(&self.pass));
            }
            let r = rb.send().await.map_err(|e| anyhow!("连不上下载器：{e}"))?;
            match r.status() {
                StatusCode::CONFLICT => {
                    let s = r.headers().get("X-Transmission-Session-Id").and_then(|v| v.to_str().ok()).unwrap_or_default().to_string();
                    *self.sid.lock().await = s;
                    continue;
                }
                StatusCode::UNAUTHORIZED => bail!("用户名或密码不对"),
                s if !s.is_success() => bail!("Transmission 返回 {s}"),
                _ => {}
            }
            let v: Value = r.json().await?;
            if v["result"] != "success" {
                bail!("Transmission：{}", v["result"].as_str().unwrap_or("未知错误"));
            }
            return Ok(v["arguments"].clone());
        }
        bail!("Transmission 会话握手失败")
    }

    pub async fn version(&self) -> Result<String> {
        let a = self.call("session-get", json!({"fields": ["version"]})).await?;
        let v = a["version"].as_str().unwrap_or("?");
        Ok(format!("Transmission {}", v.split(' ').next().unwrap_or(v)))
    }

    async fn get(&self, ids: Option<&str>, fields: &[&str]) -> Result<Vec<Value>> {
        let mut args = json!({"fields": fields});
        if let Some(h) = ids {
            args["ids"] = json!([h]);
        }
        let a = self.call("torrent-get", args).await?;
        Ok(a["torrents"].as_array().cloned().unwrap_or_default())
    }

    pub async fn list_completed(&self) -> Result<Vec<LocalTorrent>> {
        let list = self.get(None, &["hashString", "name", "percentDone", "downloadDir", "totalSize", "comment", "error", "labels"]).await?;
        Ok(list
            .into_iter()
            .filter(|t| t["percentDone"].as_f64() == Some(1.0))
            .map(|t| {
                let hash = t["hashString"].as_str().unwrap_or_default().to_ascii_lowercase();
                LocalTorrent {
                    ih1: (hash.len() == 40).then(|| hash.clone()),
                    ih2: (hash.len() == 64).then(|| hash.clone()),
                    hash,
                    name: t["name"].as_str().unwrap_or_default().into(),
                    save_path: t["downloadDir"].as_str().unwrap_or_default().into(),
                    category: t["labels"].as_array().and_then(|l| l.iter().filter_map(Value::as_str).find(|l| *l != TAG)).map(str::to_string),
                    size: t["totalSize"].as_u64().unwrap_or(0),
                    comment: t["comment"].as_str().filter(|s| !s.is_empty()).map(str::to_string),
                    broken: t["error"].as_i64().unwrap_or(0) == 3,
                }
            })
            .collect())
    }

    pub async fn meta(&self, t: &LocalTorrent) -> Result<Meta> {
        let v = self.get(Some(&t.hash), &["name", "files"]).await?;
        let x = v.first().ok_or_else(|| anyhow!("种子不存在"))?;
        let name = x["name"].as_str().unwrap_or_default();
        let files: Vec<(String, u64)> = x["files"].as_array().unwrap_or(&vec![]).iter().map(|f| (f["name"].as_str().unwrap_or_default().to_string(), f["length"].as_u64().unwrap_or(0))).collect();
        let single = files.len() == 1 && !files[0].0.contains('/');
        Ok(Meta::Entries(xseed_core::torrent::entries_from_file_list(name, &files, single)?))
    }

    pub async fn trackers(&self, hash: &str) -> Result<Vec<String>> {
        let v = self.get(Some(hash), &["trackers"]).await?;
        Ok(v.first().and_then(|x| x["trackers"].as_array().cloned()).unwrap_or_default().iter().filter_map(|t| t["announce"].as_str()).filter(|u| real_tracker(u)).map(str::to_string).collect())
    }

    pub async fn add(&self, req: AddRequest) -> Result<()> {
        let a = self
            .call(
                "torrent-add",
                json!({
                    "metainfo": base64::engine::general_purpose::STANDARD.encode(&req.torrent),
                    "download-dir": req.save_path,
                    "paused": true,
                    "labels": [TAG],
                }),
            )
            .await?;
        let added = a.get("torrent-added").or_else(|| a.get("torrent-duplicate")).ok_or_else(|| anyhow!("Transmission 没有返回新种子"))?;
        if let (Some((from, to)), Some(h)) = (req.rename_root, added["hashString"].as_str()) {
            self.call("torrent-rename-path", json!({"ids": [h], "path": from, "name": to})).await?;
        }
        // Transmission 没有“跳过校验”：显式校验一次，确认已有文件完整后才会启动（03 §1.10）
        if let Some(h) = added["hashString"].as_str() {
            self.call("torrent-verify", json!({"ids": [h]})).await?;
        }
        Ok(())
    }

    pub async fn start(&self, hash: &str) -> Result<()> {
        self.call("torrent-start", json!({"ids": [hash]})).await?;
        Ok(())
    }

    pub async fn status(&self, hash: &str) -> Result<Option<Status>> {
        let v = self.get(Some(hash), &["hashString", "name", "status", "error", "errorString", "percentDone", "downloadDir", "labels"]).await?;
        Ok(v.into_iter().find(|t| t["hashString"].as_str().is_some_and(|h| h.eq_ignore_ascii_case(hash))).map(|t| {
            let dir = t["downloadDir"].as_str().unwrap_or_default().to_string();
            let name = t["name"].as_str().unwrap_or_default();
            // status：1 等待校验、2 校验中；error：3 本地错误（文件缺失等）
            let health = if t["error"].as_i64() == Some(3) {
                Health::Broken(t["errorString"].as_str().unwrap_or("local error").into())
            } else if matches!(t["status"].as_i64(), Some(1 | 2)) {
                Health::Checking
            } else {
                Health::Ok
            };
            Status {
                health,
                progress: t["percentDone"].as_f64().unwrap_or(0.0),
                content_path: format!("{}/{}", dir.trim_end_matches('/'), name),
                save_path: dir,
                tagged: t["labels"].as_array().is_some_and(|l| l.iter().any(|x| x.as_str() == Some(TAG))),
            }
        }))
    }

    pub async fn announce(&self, hash: &str) -> Result<Announce> {
        let v = self.get(Some(hash), &["trackerStats"]).await?;
        let stats = v.first().and_then(|x| x["trackerStats"].as_array().cloned()).unwrap_or_default();
        Ok(tr_announce(&stats))
    }

    pub(crate) fn remove_args(hash: &str) -> Value {
        // 硬编码 delete-local-data=false：本软件永远不删除数据
        json!({"ids": [hash], "delete-local-data": false})
    }

    pub async fn remove_keep_data(&self, hash: &str) -> Result<()> {
        self.call("torrent-remove", Self::remove_args(hash)).await?;
        Ok(())
    }
}

fn tr_announce(stats: &[Value]) -> Announce {
    let mut failed = None;
    for s in stats {
        if s["lastAnnounceSucceeded"].as_bool() == Some(true) {
            return Announce::Working;
        }
        if s["hasAnnounced"].as_bool() == Some(true) {
            let msg = s["lastAnnounceResult"].as_str().unwrap_or_default();
            if !msg.is_empty() && msg != "Success" {
                failed = Some(msg.to_string());
            }
        }
    }
    failed.map_or(Announce::Pending, Announce::Failed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn announce_states() {
        assert_eq!(tr_announce(&[json!({"hasAnnounced": false})]), Announce::Pending);
        assert_eq!(tr_announce(&[json!({"hasAnnounced": true, "lastAnnounceSucceeded": false, "lastAnnounceResult": "unregistered torrent"})]), Announce::Failed("unregistered torrent".into()));
        assert_eq!(tr_announce(&[json!({"hasAnnounced": true, "lastAnnounceSucceeded": true, "lastAnnounceResult": "Success"})]), Announce::Working);
    }

    #[test]
    fn remove_never_deletes_data() {
        assert_eq!(Tr::remove_args("h")["delete-local-data"], json!(false));
    }

    #[test]
    fn rpc_url() {
        assert_eq!(Tr::new("http://nas:9091", "", "").unwrap().rpc.as_str(), "http://nas:9091/transmission/rpc");
        assert_eq!(Tr::new("http://nas:9091/transmission/rpc", "", "").unwrap().rpc.as_str(), "http://nas:9091/transmission/rpc");
    }
}
