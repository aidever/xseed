//! xseed-agent：去中心化辅种小助手。
// Windows：以托盘程序运行，不弹出控制台窗口
#![cfg_attr(windows, windows_subsystem = "windows")]

mod app;
mod auth;
mod config;
mod db;
mod downloader;
mod fetch;
mod inventory;
mod jobs;
mod network;
mod ptd;
mod session;
mod sites;
#[cfg(windows)]
mod tray;
mod vault;
mod web;

use std::net::SocketAddr;
use std::path::PathBuf;

const HELP: &str = "xseed-agent —— 去中心化辅种小助手

用法：
  xseed-agent [run]            启动（默认）
  xseed-agent reset-password   清除管理员密码（下次打开界面时重新设置）
  xseed-agent --version

环境变量：
  XSEED_DATA     数据目录（默认 ./data；Windows 为 %APPDATA%\\xseed；Docker 镜像中为 /data）
  XSEED_LISTEN   监听地址（覆盖配置文件，默认 0.0.0.0:7731；Windows 默认 127.0.0.1:7731）
  XSEED_REGISTRY 社区节点清单地址或文件（配置文件未设置时使用）
  RUST_LOG       日志级别（默认 info）";

/// 数据目录：`XSEED_DATA`；未设置时 Windows 用 `%APPDATA%\xseed`，其他系统用 `./data`。
fn data_dir() -> PathBuf {
    if let Ok(d) = std::env::var("XSEED_DATA") {
        return PathBuf::from(d);
    }
    #[cfg(windows)]
    if let Ok(a) = std::env::var("APPDATA") {
        return PathBuf::from(a).join("xseed");
    }
    PathBuf::from("data")
}

fn init_logging(dir: &std::path::Path) {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into());
    // Windows 托盘程序没有控制台，日志写入数据目录
    if cfg!(windows) {
        let _ = std::fs::create_dir_all(dir);
        if let Ok(f) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("xseed-agent.log")) {
            tracing_subscriber::fmt().with_env_filter(filter).with_ansi(false).with_writer(std::sync::Mutex::new(f)).init();
            return;
        }
    }
    tracing_subscriber::fmt().with_env_filter(filter).init();
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("run");
    match cmd {
        "-h" | "--help" | "help" => {
            println!("{HELP}");
            return Ok(());
        }
        "-V" | "--version" => {
            println!("xseed-agent {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        "run" | "reset-password" => {}
        other => anyhow::bail!("未知命令 {other}\n\n{HELP}"),
    }
    let dir = data_dir();
    init_logging(&dir);
    let app = app::App::open(dir.clone())?;
    if cmd == "reset-password" {
        app.db.lock().execute("DELETE FROM meta WHERE k='admin_hash'", [])?;
        println!("已清除管理员密码，打开界面重新设置。");
        return Ok(());
    }
    let listen = std::env::var("XSEED_LISTEN").unwrap_or_else(|_| app.cfg().webui.listen);
    let addr: SocketAddr = listen.parse()?;
    let rt = tokio::runtime::Runtime::new()?;

    #[cfg(windows)]
    {
        let host = if addr.ip().is_unspecified() { std::net::SocketAddr::from(([127, 0, 0, 1], addr.port())) } else { addr };
        let url = format!("http://{host}/");
        let first = !app.cfg().setup_done;
        std::thread::spawn(move || {
            if let Err(e) = rt.block_on(serve(app, addr, dir)) {
                tracing::error!("启动失败：{e:#}");
                tray::fatal(&format!("xseed 无法启动：{e:#}"));
            }
            std::process::exit(0);
        });
        tray::run(&url, first)
    }
    #[cfg(not(windows))]
    rt.block_on(serve(app, addr, dir))
}

async fn serve(app: app::Shared, addr: SocketAddr, dir: PathBuf) -> anyhow::Result<()> {
    tokio::spawn(app::background(app.clone()));
    tokio::spawn(network::run(app.clone()));
    tokio::spawn(jobs::run(app.clone()));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("xseed-agent {} 已启动：http://{addr}  数据目录 {}", env!("CARGO_PKG_VERSION"), dir.display());
    axum::serve(listener, web::router(app).into_make_service_with_connect_info::<SocketAddr>())
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}
