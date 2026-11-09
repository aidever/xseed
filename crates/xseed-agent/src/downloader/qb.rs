//! qBittorrent WebUI API v2（03 §2.4）。

use super::{base, http, real_tracker, AddRequest, Layout, LocalTorrent, Meta, TAG};
use anyhow::{anyhow, bail, Result};
use reqwest::{Response, StatusCode};
use serde::Deserialize;
use tokio::sync::Mutex;

pub struct Qb {
    base: url::Url,
    user: String,
    pass: String,
    http: reqwest::Client,
    logged_in: Mutex<bool>,
    major: Mutex<Option<u32>>,
}

#[derive(Deserialize)]
struct Info {
    hash: String,
    name: String,
    save_path: String,
    #[serde(default)]
    category: String,
    #[serde(default)]
    total_size: i64,
    #[serde(default)]
    size: i64,
    progress: f64,
    state: String,
    #[serde(default)]
    infohash_v1: String,
    #[serde(default)]
    infohash_v2: String,
}

#[derive(Deserialize)]
struct Tracker {
    url: String,
}

#[derive(Deserialize)]
struct Props {
    #[serde(default)]
    comment: String,
}

#[derive(Deserialize)]
struct File {
    name: String,
    size: u64,
}

fn nonempty(s: String) -> Option<String> {
    (!s.is_empty()).then_some(s)
}

impl Qb {
    pub fn new(url: &str, user: &str, pass: &str) -> Result<Self> {
        Ok(Qb {
            base: base(url)?,
            user: user.into(),
            pass: pass.into(),
            http: http().cookie_store(true).build()?,
            logged_in: Mutex::new(false),
            major: Mutex::new(None),
        })
    }

    fn url(&self, path: &str) -> url::Url {
        self.base.join(path).expect("join")
    }

    async fn login(&self) -> Result<()> {
        let r = self
            .http
            .post(self.url("api/v2/auth/login"))
            .header("Referer", self.base.as_str())
            .form(&[("username", self.user.as_str()), ("password", self.pass.as_str())])
            .send()
            .await
            .map_err(|e| anyhow!("连不上下载器：{e}"))?;
        let st = r.status();
        let body = r.text().await.unwrap_or_default();
        if st == StatusCode::FORBIDDEN {
            bail!("登录失败次数过多，qBittorrent 暂时封禁了这个地址");
        }
        if !st.is_success() || body.trim() != "Ok." {
            bail!("用户名或密码不对");
        }
        *self.logged_in.lock().await = true;
        Ok(())
    }

    /// 带自动登录与一次 403 重试的请求。
    async fn send(&self, mk: impl Fn() -> reqwest::RequestBuilder) -> Result<Response> {
        if !*self.logged_in.lock().await {
            self.login().await?;
        }
        let mut r = mk().header("Referer", self.base.as_str()).send().await?;
        if r.status() == StatusCode::FORBIDDEN {
            self.login().await?;
            r = mk().header("Referer", self.base.as_str()).send().await?;
        }
        if !r.status().is_success() {
            bail!("qBittorrent 返回 {}", r.status());
        }
        Ok(r)
    }

    pub async fn version(&self) -> Result<String> {
        let v = self.send(|| self.http.get(self.url("api/v2/app/version"))).await?.text().await?;
        let v = v.trim().trim_start_matches('v').to_string();
        *self.major.lock().await = v.split('.').next().and_then(|m| m.parse().ok());
        Ok(format!("qBittorrent {v}"))
    }

    async fn major(&self) -> u32 {
        if let Some(m) = *self.major.lock().await {
            return m;
        }
        let _ = self.version().await;
        self.major.lock().await.unwrap_or(4)
    }

    pub async fn list_completed(&self) -> Result<Vec<LocalTorrent>> {
        let list: Vec<Info> = self.send(|| self.http.get(self.url("api/v2/torrents/info")).query(&[("filter", "completed")])).await?.json().await?;
        Ok(list
            .into_iter()
            .filter(|t| t.progress >= 1.0)
            .map(|t| LocalTorrent {
                broken: matches!(t.state.as_str(), "missingFiles" | "error"),
                hash: t.hash.to_ascii_lowercase(),
                ih1: nonempty(t.infohash_v1),
                ih2: nonempty(t.infohash_v2),
                name: t.name,
                save_path: t.save_path,
                category: nonempty(t.category),
                size: t.total_size.max(t.size).max(0) as u64,
                comment: None,
            })
            .collect())
    }

    pub async fn meta(&self, t: &LocalTorrent) -> Result<Meta> {
        // 优先导出原始种子（qB ≥ 4.5）；失败再用文件列表
        let exported = self.send(|| self.http.get(self.url("api/v2/torrents/export")).query(&[("hash", t.hash.as_str())])).await;
        if let Ok(r) = exported {
            let b = r.bytes().await?;
            if b.first() == Some(&b'd') {
                return Ok(Meta::Raw(b.to_vec()));
            }
        }
        let files: Vec<File> = self.send(|| self.http.get(self.url("api/v2/torrents/files")).query(&[("hash", t.hash.as_str())])).await?.json().await?;
        let list: Vec<(String, u64)> = files.into_iter().map(|f| (f.name, f.size)).collect();
        // 文件列表不带根目录名时（单文件，或以“无子文件夹”布局加入的多文件）无法区分，只接受能确定的情况
        let has_root = list.iter().all(|(p, _)| p.contains('/'));
        if list.len() == 1 && !has_root {
            return Ok(Meta::Entries(xseed_core::torrent::entries_from_file_list(&t.name, &list, true)?));
        }
        if has_root {
            let root = list[0].0.split('/').next().unwrap_or_default().to_string();
            return Ok(Meta::Entries(xseed_core::torrent::entries_from_file_list(&root, &list, false)?));
        }
        bail!("layout_unknown")
    }

    pub async fn comment(&self, hash: &str) -> Result<Option<String>> {
        let p: Props = self.send(|| self.http.get(self.url("api/v2/torrents/properties")).query(&[("hash", hash)])).await?.json().await?;
        Ok(nonempty(p.comment))
    }

    pub async fn trackers(&self, hash: &str) -> Result<Vec<String>> {
        let t: Vec<Tracker> = self.send(|| self.http.get(self.url("api/v2/torrents/trackers")).query(&[("hash", hash)])).await?.json().await?;
        Ok(t.into_iter().map(|t| t.url).filter(|u| real_tracker(u)).collect())
    }

    pub(crate) fn add_form(req: &AddRequest, major: u32) -> Vec<(&'static str, String)> {
        let mut f = vec![
            ("savepath", req.save_path.clone()),
            ("skip_checking", req.skip_check.to_string()),
            (if major >= 5 { "stopped" } else { "paused" }, "true".into()),
            ("autoTMM", "false".into()),
            ("tags", TAG.into()),
            ("contentLayout", if req.layout == Layout::NoSubfolder { "NoSubfolder" } else { "Original" }.into()),
        ];
        if let Some(c) = &req.category {
            f.push(("category", c.clone()));
        }
        f
    }

    pub async fn add(&self, req: AddRequest) -> Result<()> {
        let major = self.major().await;
        let fields = Self::add_form(&req, major);
        let r = self
            .send(|| {
                let mut form = reqwest::multipart::Form::new().part("torrents", reqwest::multipart::Part::bytes(req.torrent.clone()).file_name("xseed.torrent").mime_str("application/x-bittorrent").unwrap());
                for (k, v) in &fields {
                    form = form.text(*k, v.clone());
                }
                self.http.post(self.url("api/v2/torrents/add")).multipart(form)
            })
            .await?;
        let body = r.text().await.unwrap_or_default();
        if body.trim() == "Fails." {
            bail!("qBittorrent 拒绝加入这个种子");
        }
        Ok(())
    }

    pub async fn start(&self, hash: &str) -> Result<()> {
        let path = if self.major().await >= 5 { "api/v2/torrents/start" } else { "api/v2/torrents/resume" };
        self.send(|| self.http.post(self.url(path)).form(&[("hashes", hash)])).await?;
        Ok(())
    }

    pub(crate) fn delete_form(hash: &str) -> [(&'static str, String); 2] {
        // 硬编码 deleteFiles=false：本软件永远不删除数据
        [("hashes", hash.to_string()), ("deleteFiles", "false".to_string())]
    }

    pub async fn remove_keep_data(&self, hash: &str) -> Result<()> {
        let form = Self::delete_form(hash);
        self.send(|| self.http.post(self.url("api/v2/torrents/delete")).form(&form)).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delete_never_deletes_files() {
        let f = Qb::delete_form("abc");
        assert!(f.contains(&("deleteFiles", "false".to_string())));
        assert!(!f.iter().any(|(k, v)| *k == "deleteFiles" && v != "false"));
    }

    #[test]
    fn add_form_by_version() {
        let req = AddRequest { torrent: vec![], save_path: "/d/Show".into(), layout: Layout::NoSubfolder, rename_root: None, skip_check: true, category: Some("tv".into()) };
        let f5 = Qb::add_form(&req, 5);
        assert!(f5.contains(&("stopped", "true".into())) && f5.contains(&("contentLayout", "NoSubfolder".into())) && f5.contains(&("tags", "xseed".into())) && f5.contains(&("autoTMM", "false".into())));
        let f4 = Qb::add_form(&req, 4);
        assert!(f4.contains(&("paused", "true".into())));
    }
}
