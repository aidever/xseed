//! Web UI 静态资源、本地 JSON API（03 §2.3）与 PT-depiler WebDAV 接收端（03 §1.12.2）。

use crate::app::Shared;
use crate::auth::{self, COOKIE};
use crate::config::{DownloaderCfg, DownloaderKind};
use crate::db::now;
use crate::downloader::Client;
use crate::{inventory, ptd, sites};
use axum::body::{Body, Bytes};
use axum::extract::{ConnectInfo, DefaultBodyLimit, Path, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post, put};
use axum::{Json, Router};
use base64::Engine;
use rust_embed::Embed;
use serde::Deserialize;
use serde_json::{json, Value};
use std::net::SocketAddr;

#[derive(Embed)]
#[folder = "web/"]
struct Assets;

type Res = Result<Response, ApiError>;

pub struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"error": self.1}))).into_response()
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        ApiError(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}"))
    }
}

fn bad(msg: impl Into<String>) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, msg.into())
}

fn ok(v: Value) -> Res {
    Ok(Json(v).into_response())
}

pub fn router(app: Shared) -> Router {
    let api = Router::new()
        .route("/session", get(session))
        .route("/setup/admin", post(setup_admin))
        .route("/login", post(login))
        .route("/logout", post(logout))
        .route("/password", post(change_password))
        .route("/setup/discover", get(discover))
        .route("/setup/downloader/test", post(test_downloader))
        .route("/setup/downloader", post(save_downloader))
        .route("/scan", get(scan_state).post(rescan))
        .route("/status", get(status))
        .route("/sites", get(list_sites))
        .route("/sites/{site_ref}", put(update_site))
        .route("/ptd", get(ptd_info).put(ptd_update))
        .route("/ptd/rotate", post(ptd_rotate))
        .route("/dry-run", get(dry_run))
        .route("/dry-run/confirm", post(dry_run_confirm))
        .route("/jobs", get(jobs))
        .route("/remove-all", post(remove_all))
        .route("/config", get(get_config).put(put_config))
        .route("/audit", get(audit))
        .layer(middleware::from_fn_with_state(app.clone(), guard));
    Router::new()
        .nest("/api", api)
        .route("/ptd", any(webdav))
        .route("/ptd/", any(webdav))
        .route("/ptd/{*path}", any(webdav))
        .fallback(get(asset))
        .layer(DefaultBodyLimit::max(ptd::MAX_UPLOAD))
        .with_state(app)
}

// ───────────── 静态资源 ─────────────

async fn asset(uri: axum::http::Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let (file, path) = match Assets::get(path) {
        Some(f) if !path.is_empty() => (f, path),
        _ => (Assets::get("index.html").expect("index.html"), "index.html"),
    };
    let mime = mime_guess::from_path(path).first_or_octet_stream();
    let cache = if path == "index.html" { "no-cache" } else { "public, max-age=300" };
    (
        [
            (header::CONTENT_TYPE, mime.as_ref().to_string()),
            (header::CACHE_CONTROL, cache.to_string()),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_string()),
            (header::X_FRAME_OPTIONS, "DENY".to_string()),
            (header::REFERRER_POLICY, "no-referrer".to_string()),
            (header::CONTENT_SECURITY_POLICY, "default-src 'self'; style-src 'self' 'unsafe-inline' https://fonts.googleapis.com; font-src https://fonts.gstatic.com; img-src 'self' data:; connect-src 'self'; frame-ancestors 'none'".to_string()),
        ],
        file.data.into_owned(),
    )
        .into_response()
}

// ───────────── 鉴权中间件 ─────────────

fn cookie(h: &HeaderMap, name: &str) -> Option<String> {
    h.get_all(header::COOKIE).iter().filter_map(|v| v.to_str().ok()).flat_map(|s| s.split(';')).find_map(|kv| {
        let (k, v) = kv.trim().split_once('=')?;
        (k == name).then(|| v.to_string())
    })
}

fn host_of(h: &HeaderMap) -> String {
    h.get(header::HOST).and_then(|v| v.to_str().ok()).unwrap_or_default().to_ascii_lowercase()
}

fn host_allowed(app: &Shared, host: &str) -> bool {
    let allowed = app.cfg().webui.allowed_hosts;
    let bare = host.rsplit_once(':').map_or(host, |(h, _)| h);
    allowed.is_empty() || allowed.iter().any(|a| a.eq_ignore_ascii_case(host)) || matches!(bare, "localhost" | "127.0.0.1" | "[::1]")
}

async fn guard(State(app): State<Shared>, req: Request, next: Next) -> Response {
    let h = req.headers();
    let host = host_of(h);
    // 防 DNS 重绑定：Host 必须是已知访问地址
    if !host_allowed(&app, &host) {
        return ApiError(StatusCode::FORBIDDEN, format!("不认识的访问地址 {host}，请在 xseed-agent.toml 的 webui.allowed_hosts 中添加")).into_response();
    }
    // 防跨站：带 Origin 时必须与 Host 一致
    if let Some(o) = h.get(header::ORIGIN).and_then(|v| v.to_str().ok()) {
        let oh = url::Url::parse(o).ok().map(|u| match u.port() {
            Some(p) => format!("{}:{p}", u.host_str().unwrap_or_default()),
            None => u.host_str().unwrap_or_default().to_string(),
        });
        if oh.as_deref().map(str::to_ascii_lowercase) != Some(host.clone()) {
            return ApiError(StatusCode::FORBIDDEN, "跨站请求被拒绝".into()).into_response();
        }
    }
    let path = req.uri().path().to_string();
    let public = matches!(path.as_str(), "/session" | "/setup/admin" | "/login");
    if !public {
        let token = cookie(h, COOKIE).unwrap_or_default();
        let Some(csrf) = app.sessions.csrf(&token) else {
            return ApiError(StatusCode::UNAUTHORIZED, "请先登录".into()).into_response();
        };
        if req.method() != Method::GET && h.get("x-csrf-token").and_then(|v| v.to_str().ok()) != Some(csrf.as_str()) {
            return ApiError(StatusCode::FORBIDDEN, "CSRF 校验失败，请刷新页面".into()).into_response();
        }
    }
    next.run(req).await
}

fn session_cookie(token: &str) -> HeaderValue {
    HeaderValue::from_str(&format!("{COOKIE}={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age=604800")).unwrap()
}

// ───────────── 会话 ─────────────

async fn session(State(app): State<Shared>, h: HeaderMap) -> Res {
    let has_admin = app.db.meta_get("admin_hash").is_some();
    let csrf = cookie(&h, COOKIE).and_then(|t| app.sessions.csrf(&t));
    let cfg = app.cfg();
    ok(json!({
        "has_admin": has_admin,
        "logged_in": csrf.is_some(),
        "csrf": csrf,
        "has_downloader": !cfg.downloaders.is_empty(),
        "setup_done": cfg.setup_done,
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

#[derive(Deserialize)]
struct PasswordBody {
    password: String,
    #[serde(default)]
    old: String,
}

fn login_response(app: &Shared) -> Res {
    let (token, csrf) = app.sessions.create();
    let mut r = Json(json!({"ok": true, "csrf": csrf})).into_response();
    r.headers_mut().insert(header::SET_COOKIE, session_cookie(&token));
    Ok(r)
}

async fn setup_admin(State(app): State<Shared>, h: HeaderMap, Json(b): Json<PasswordBody>) -> Res {
    if app.db.meta_get("admin_hash").is_some() {
        return Err(ApiError(StatusCode::CONFLICT, "管理员密码已经设置过了".into()));
    }
    if b.password.chars().count() < 8 {
        return Err(bad("密码至少 8 位"));
    }
    app.db.meta_set("admin_hash", &auth::hash_password(&b.password))?;
    // 记录首次访问地址，用于之后的 Host 校验
    let host = host_of(&h);
    if !host.is_empty() {
        app.update_cfg(|c| {
            if !c.webui.allowed_hosts.contains(&host) {
                c.webui.allowed_hosts.push(host.clone());
            }
        })?;
    }
    app.db.audit("admin_setup", "");
    login_response(&app)
}

async fn login(State(app): State<Shared>, ConnectInfo(addr): ConnectInfo<SocketAddr>, Json(b): Json<PasswordBody>) -> Res {
    let ip = addr.ip();
    if app.login_lock.blocked(ip) {
        return Err(ApiError(StatusCode::TOO_MANY_REQUESTS, "失败次数过多，请 15 分钟后再试".into()));
    }
    let Some(hash) = app.db.meta_get("admin_hash") else { return Err(bad("还没有设置管理员密码")) };
    if !auth::verify_password(&b.password, &hash) {
        app.login_lock.fail(ip);
        app.db.audit("login_failed", &ip.to_string());
        return Err(ApiError(StatusCode::UNAUTHORIZED, "密码不对".into()));
    }
    app.login_lock.ok(ip);
    login_response(&app)
}

async fn logout(State(app): State<Shared>, h: HeaderMap) -> Res {
    if let Some(t) = cookie(&h, COOKIE) {
        app.sessions.remove(&t);
    }
    let mut r = Json(json!({"ok": true})).into_response();
    r.headers_mut().insert(header::SET_COOKIE, HeaderValue::from_static("xseed_session=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0"));
    Ok(r)
}

async fn change_password(State(app): State<Shared>, Json(b): Json<PasswordBody>) -> Res {
    let hash = app.db.meta_get("admin_hash").unwrap_or_default();
    if !auth::verify_password(&b.old, &hash) {
        return Err(bad("原密码不对"));
    }
    if b.password.chars().count() < 8 {
        return Err(bad("新密码至少 8 位"));
    }
    app.db.meta_set("admin_hash", &auth::hash_password(&b.password))?;
    app.db.meta_set("admin_changed_at", &now().to_string())?;
    app.sessions.clear();
    app.db.audit("password_changed", "");
    login_response(&app)
}

// ───────────── 下载器 ─────────────

#[derive(Deserialize)]
struct DownloaderBody {
    kind: DownloaderKind,
    url: String,
    #[serde(default)]
    user: String,
    #[serde(default)]
    password: String,
}

async fn discover() -> Res {
    // 探测常见地址（03 §5.1）
    let candidates = [("qbittorrent", "http://localhost:8080"), ("transmission", "http://localhost:9091"), ("qbittorrent", "http://127.0.0.1:8080"), ("transmission", "http://127.0.0.1:9091")];
    let http = reqwest::Client::builder().timeout(std::time::Duration::from_millis(1200)).build().map_err(anyhow::Error::from)?;
    for (kind, url) in candidates {
        let probe = if kind == "qbittorrent" { format!("{url}/api/v2/app/webapiVersion") } else { format!("{url}/transmission/rpc") };
        if let Ok(r) = http.get(&probe).send().await {
            let s = r.status().as_u16();
            let hit = if kind == "qbittorrent" { s == 200 || s == 403 } else { s == 409 || s == 401 };
            if hit {
                return ok(json!({"found": {"kind": kind, "url": url}}));
            }
        }
    }
    ok(json!({"found": null}))
}

async fn test_downloader(State(app): State<Shared>, Json(b): Json<DownloaderBody>) -> Res {
    let cfg = DownloaderCfg { id: "test".into(), kind: b.kind, url: b.url, user: b.user };
    // 修改时密码留空表示沿用已保存的密码
    let password = if b.password.is_empty() { app.vault.secret("downloader/main").unwrap_or_default() } else { b.password };
    let c = Client::new(&cfg, &password).map_err(|e| bad(e.to_string()))?;
    match c.version().await {
        Ok(v) => {
            let n = c.list_completed().await.map(|l| l.len()).unwrap_or(0);
            ok(json!({"ok": true, "version": v, "completed": n}))
        }
        Err(e) => ok(json!({"ok": false, "error": format!("{e:#}")})),
    }
}

async fn save_downloader(State(app): State<Shared>, Json(b): Json<DownloaderBody>) -> Res {
    let id = "main".to_string();
    let cfg = DownloaderCfg { id: id.clone(), kind: b.kind, url: b.url.trim().to_string(), user: b.user };
    Client::new(&cfg, &b.password).map_err(|e| bad(e.to_string()))?;
    // 密码留空表示沿用原密码
    if !b.password.is_empty() || app.vault.secret("downloader/main").is_none() {
        app.vault.set_secret(&format!("downloader/{id}"), &b.password)?;
    }
    app.update_cfg(|c| c.downloaders = vec![cfg.clone()])?;
    app.db.audit("downloader_saved", &cfg.url);
    let a = app.clone();
    tokio::spawn(async move { a.scan_all(true).await });
    ok(json!({"ok": true}))
}

async fn scan_state(State(app): State<Shared>) -> Res {
    ok(serde_json::to_value(app.scan.lock().unwrap().clone()).unwrap())
}

async fn rescan(State(app): State<Shared>) -> Res {
    let a = app.clone();
    tokio::spawn(async move { a.scan_all(true).await });
    ok(json!({"ok": true}))
}

// ───────────── 概览 / 站点 ─────────────

async fn status(State(app): State<Shared>) -> Res {
    let cfg = app.cfg();
    let stats = inventory::stats(&app.db);
    let sites = sites::list(&app.db, &app.vault);
    let dl = app.downloader_status.lock().unwrap().clone();
    let alerts: Vec<Value> = sites
        .iter()
        .filter(|s| s.enabled && s.status == "need_cookie")
        .map(|s| json!({"kind": "need_cookie", "site": s.name}))
        .chain(dl.iter().filter(|d| !d.1).map(|d| json!({"kind": "downloader_unreachable", "detail": d.0})))
        .collect();
    ok(json!({
        "stats": stats,
        "scan": app.scan.lock().unwrap().clone(),
        "downloader": cfg.downloaders.first().map(|d| json!({"kind": d.kind, "url": d.url, "user": d.user, "status": dl.as_ref().map(|x| &x.0), "ok": dl.as_ref().map(|x| x.1)})),
        "network": network_json(&app),
        "dry_run": cfg.dry_run,
        "respond": cfg.respond,
        "today": {"added": 0, "size": 0},
        "sites": sites,
        "alerts": alerts,
        "ptd": app.ptd_last.lock().unwrap().clone(),
    }))
}

fn network_json(app: &Shared) -> Value {
    let st = app.net.status.lock().unwrap().clone();
    let (discovered, peers) = crate::network::Net::discovered(&app.db);
    let mut v = serde_json::to_value(st).unwrap_or_default();
    v["discovered"] = json!(discovered);
    v["peers"] = json!(peers);
    v["offers"] = json!(app.db.lock().query_row("SELECT COUNT(*) FROM offers", [], |r| r.get::<_, i64>(0)).unwrap_or(0));
    v
}

async fn list_sites(State(app): State<Shared>) -> Res {
    ok(json!({"sites": sites::list(&app.db, &app.vault)}))
}

#[derive(Deserialize)]
struct SiteBody {
    enabled: Option<bool>,
    cookie: Option<String>,
    daily_add_limit: Option<i64>,
}

async fn update_site(State(app): State<Shared>, Path(site_ref): Path<String>, Json(b): Json<SiteBody>) -> Res {
    let list = sites::list(&app.db, &app.vault);
    let s = list.iter().find(|s| s.site_ref == site_ref).ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "站点不存在".into()))?;
    if let Some(on) = b.enabled {
        if on && s.status == "denied" {
            return Err(bad("站长不允许在该站点使用 P2P 辅种"));
        }
        sites::set_enabled(&app.db, &site_ref, on)?;
        app.db.audit("site_toggle", &format!("{site_ref} {on}"));
    }
    if let Some(c) = b.cookie {
        if c.trim().is_empty() {
            app.db.lock().execute("DELETE FROM credentials WHERE site_ref=?1 AND kind='cookie' AND source='manual'", [&site_ref]).map_err(anyhow::Error::from)?;
        } else {
            app.vault.set_credential(&site_ref, "cookie", "manual", c.trim())?;
        }
        app.db.audit("site_cookie", &site_ref);
    }
    if let Some(l) = b.daily_add_limit {
        if !(1..=500).contains(&l) {
            return Err(bad("每日上限需在 1–500 之间"));
        }
        app.db.lock().execute("UPDATE sites SET daily_add_limit=?2 WHERE site_ref=?1", rusqlite::params![site_ref, l]).map_err(anyhow::Error::from)?;
    }
    ok(json!({"ok": true}))
}

// ───────────── PT-depiler ─────────────

fn ptd_password(app: &Shared) -> anyhow::Result<String> {
    if let Some(p) = app.vault.secret("ptd/password") {
        return Ok(p);
    }
    let p = auth::random_password(24);
    app.vault.set_secret("ptd/password", &p)?;
    Ok(p)
}

fn ptd_user(app: &Shared) -> anyhow::Result<String> {
    let u = app.cfg().ptd.user;
    if !u.is_empty() {
        return Ok(u);
    }
    let u = format!("ptd-{}", auth::random_token(3));
    app.update_cfg(|c| c.ptd.user = u.clone())?;
    Ok(u)
}

async fn ptd_info(State(app): State<Shared>, h: HeaderMap) -> Res {
    let host = host_of(&h);
    let cfg = app.cfg();
    ok(json!({
        "url": format!("http://{host}/ptd/"),
        "user": ptd_user(&app)?,
        "password": ptd_password(&app)?,
        "enabled": cfg.ptd.enabled,
        "has_key": app.vault.secret("ptd/key").is_some_and(|k| !k.is_empty()),
        "last": app.ptd_last.lock().unwrap().clone(),
    }))
}

#[derive(Deserialize)]
struct PtdBody {
    enabled: Option<bool>,
    key: Option<String>,
}

async fn ptd_update(State(app): State<Shared>, Json(b): Json<PtdBody>) -> Res {
    if let Some(e) = b.enabled {
        app.update_cfg(|c| c.ptd.enabled = e)?;
    }
    if let Some(k) = b.key {
        if k.is_empty() {
            app.vault.delete_secret("ptd/key")?;
        } else {
            app.vault.set_secret("ptd/key", &k)?;
        }
    }
    ok(json!({"ok": true}))
}

async fn ptd_rotate(State(app): State<Shared>) -> Res {
    let p = auth::random_password(24);
    app.vault.set_secret("ptd/password", &p)?;
    app.db.audit("ptd_rotate", "");
    ok(json!({"password": p}))
}

const DAV_EMPTY: &str = r#"<?xml version="1.0" encoding="utf-8"?><d:multistatus xmlns:d="DAV:"><d:response><d:href>/ptd/</d:href><d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype><d:displayname>ptd</d:displayname></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#;

fn dav_status(s: StatusCode) -> Response {
    (s, [(header::HeaderName::from_static("dav"), "1")], Body::empty()).into_response()
}

async fn webdav(State(app): State<Shared>, ConnectInfo(addr): ConnectInfo<SocketAddr>, method: Method, path: Option<Path<String>>, h: HeaderMap, body: Bytes) -> Response {
    let ip = addr.ip();
    let cfg = app.cfg();
    if !cfg.ptd.enabled {
        return dav_status(StatusCode::NOT_FOUND);
    }
    if app.dav_lock.blocked(ip) {
        return dav_status(StatusCode::TOO_MANY_REQUESTS);
    }
    // HTTP Basic，独立于 Web UI 的凭证
    let authed = h
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Basic "))
        .and_then(|b| base64::engine::general_purpose::STANDARD.decode(b).ok())
        .and_then(|b| String::from_utf8(b).ok())
        .is_some_and(|s| {
            let (u, p) = s.split_once(':').unwrap_or_default();
            u == cfg.ptd.user && Some(p.to_string()) == app.vault.secret("ptd/password") && !u.is_empty()
        });
    if !authed {
        app.dav_lock.fail(ip);
        return (StatusCode::UNAUTHORIZED, [(header::WWW_AUTHENTICATE, "Basic realm=\"xseed\"")], Body::empty()).into_response();
    }
    app.dav_lock.ok(ip);
    let name = path.map(|p| p.0).unwrap_or_default();
    match method.as_str() {
        "OPTIONS" => (StatusCode::OK, [(header::HeaderName::from_static("dav"), "1"), (header::ALLOW, "OPTIONS, PROPFIND, PUT, GET, DELETE")], Body::empty()).into_response(),
        "PROPFIND" => (StatusCode::MULTI_STATUS, [(header::CONTENT_TYPE, "application/xml; charset=utf-8")], DAV_EMPTY).into_response(),
        "PUT" => {
            let valid = name.len() == "PTD_backup_20260101T0000.zip".len() && name.starts_with("PTD_backup_") && name.ends_with(".zip") && name[11..19].bytes().all(|b| b.is_ascii_digit()) && &name[19..20] == "T" && name[20..24].bytes().all(|b| b.is_ascii_digit());
            if !valid {
                return dav_status(StatusCode::FORBIDDEN);
            }
            let key = app.vault.secret("ptd/key");
            let r = match ptd::parse(&body, key.as_deref()) {
                Ok(p) => ptd::apply(&app.db, &app.vault, &p),
                Err(e) => {
                    tracing::warn!("PT-depiler 备份解析失败：{e:?}");
                    ptd::SyncResult { at: now(), ok: false, error: Some(e.to_string()), ..Default::default() }
                }
            };
            app.db.audit("ptd_sync", &serde_json::to_string(&r).unwrap_or_default());
            let success = r.ok;
            app.set_ptd_result(r);
            if success {
                // 站点定义更新后重新识别清单
                let a = app.clone();
                tokio::spawn(async move { a.scan_all(true).await });
            }
            // 解析失败也返回 201，避免 PT-depiler 反复重试；结果在界面上提示
            dav_status(StatusCode::CREATED)
        }
        "GET" | "HEAD" => dav_status(StatusCode::NOT_FOUND),
        "DELETE" => dav_status(StatusCode::NO_CONTENT),
        "MKCOL" => dav_status(StatusCode::METHOD_NOT_ALLOWED),
        _ => dav_status(StatusCode::METHOD_NOT_ALLOWED),
    }
}

// ───────────── 试运行 / 任务 / 设置 ─────────────

async fn dry_run(State(app): State<Shared>) -> Res {
    let stats = inventory::stats(&app.db);
    let sites = sites::list(&app.db, &app.vault);
    ok(json!({"stats": stats, "sites": sites, "network": network_json(&app), "candidates": null}))
}

async fn dry_run_confirm(State(app): State<Shared>) -> Res {
    app.update_cfg(|c| {
        c.setup_done = true;
        c.dry_run = false;
    })?;
    app.db.audit("dry_run_confirmed", "");
    ok(json!({"ok": true}))
}

/// 辅种记录。M3 只有“已拿到种子编号”的告知；加种（M4）后补充后续步骤。
async fn jobs(State(app): State<Shared>) -> Res {
    let names: std::collections::HashMap<String, String> = sites::list(&app.db, &app.vault).into_iter().map(|s| (s.site_ref, s.name)).collect();
    let c = app.db.lock();
    let mut st = c
        .prepare(
            "SELECT o.id, o.site_ref, o.torrent_id, o.size, o.received_at, o.status,
                    (SELECT name FROM local_torrents lt WHERE lt.sfp=o.sfp ORDER BY lt.first_seen LIMIT 1)
             FROM offers o ORDER BY o.received_at DESC LIMIT 500",
        )
        .map_err(anyhow::Error::from)?;
    let rows: Vec<Value> = st
        .query_map([], |r| {
            let site_ref: String = r.get(1)?;
            Ok(json!({
                "id": r.get::<_, i64>(0)?,
                "site_ref": site_ref,
                "torrent_id": r.get::<_, String>(2)?,
                "size": r.get::<_, i64>(3)?,
                "at": r.get::<_, i64>(4)?,
                "status": r.get::<_, String>(5)?,
                "name": r.get::<_, Option<String>>(6)?.unwrap_or_default(),
                "step": 2,
            }))
        })
        .map_err(anyhow::Error::from)?
        .filter_map(Result::ok)
        .map(|mut v| {
            let n = names.get(v["site_ref"].as_str().unwrap_or_default()).cloned();
            v["site"] = json!(n.unwrap_or_else(|| v["site_ref"].as_str().unwrap_or_default().split_once(':').map(|x| x.1.to_string()).unwrap_or_default()));
            v
        })
        .collect();
    let n = rows.len();
    ok(json!({"jobs": rows, "counts": {"all": n, "seeding": 0, "running": n, "failed": 0}}))
}

async fn remove_all(State(app): State<Shared>) -> Res {
    let n: i64 = app.db.lock().query_row("SELECT COUNT(*) FROM added_torrents WHERE removed_at IS NULL", [], |r| r.get(0)).unwrap_or(0);
    app.db.audit("remove_all", &n.to_string());
    ok(json!({"removed": 0, "pending": n}))
}

async fn get_config(State(app): State<Shared>) -> Res {
    let c = app.cfg();
    ok(json!({
        "respond": c.respond, "daily_add_limit": c.daily_add_limit, "daily_answer_limit": c.daily_answer_limit,
        "log_level": c.log_level, "pin_entry": c.pin_entry, "dry_run": c.dry_run, "registry": c.registry,
        "admin_changed_at": app.db.meta_get("admin_changed_at").and_then(|s| s.parse::<i64>().ok()),
    }))
}

#[derive(Deserialize)]
struct ConfigBody {
    respond: Option<bool>,
    daily_add_limit: Option<u32>,
    daily_answer_limit: Option<u32>,
    log_level: Option<String>,
    pin_entry: Option<String>,
    registry: Option<String>,
}

async fn put_config(State(app): State<Shared>, Json(b): Json<ConfigBody>) -> Res {
    let mut next = app.cfg();
    if let Some(v) = b.respond {
        next.respond = v;
    }
    if let Some(v) = b.daily_add_limit {
        next.daily_add_limit = v;
    }
    if let Some(v) = b.daily_answer_limit {
        next.daily_answer_limit = v;
    }
    if let Some(v) = b.log_level {
        if !matches!(v.as_str(), "info" | "debug") {
            return Err(bad("日志级别只能是 info 或 debug"));
        }
        next.log_level = v;
    }
    if let Some(v) = b.pin_entry {
        next.pin_entry = v.trim().to_string();
    }
    if let Some(v) = b.registry {
        let v = v.trim().to_string();
        if !(v.is_empty() || v.starts_with("http://") || v.starts_with("https://") || v.starts_with('/')) {
            return Err(bad("节点清单需填写 http(s) 地址或绝对路径"));
        }
        next.registry = v;
    }
    next.validate().map_err(bad)?;
    app.update_cfg(|c| *c = next)?;
    app.db.audit("config_saved", "");
    ok(json!({"ok": true}))
}

async fn audit(State(app): State<Shared>) -> Res {
    let c = app.db.lock();
    let mut st = c.prepare("SELECT at,kind,detail FROM audit_log ORDER BY id DESC LIMIT 200").map_err(anyhow::Error::from)?;
    let rows: Vec<Value> = st.query_map([], |r| Ok(json!({"at": r.get::<_, i64>(0)?, "kind": r.get::<_, String>(1)?, "detail": r.get::<_, String>(2)?}))).map_err(anyhow::Error::from)?.filter_map(Result::ok).collect();
    ok(json!({"items": rows}))
}
