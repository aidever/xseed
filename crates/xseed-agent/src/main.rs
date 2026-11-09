//! xseed-agent：去中心化辅种小助手。

mod app;
mod auth;
mod config;
mod db;
mod downloader;
mod inventory;
mod network;
mod ptd;
mod sites;
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
  XSEED_DATA     数据目录（默认 ./data；Docker 镜像中为 /data）
  XSEED_LISTEN   监听地址（覆盖配置文件，默认 0.0.0.0:7731）
  XSEED_REGISTRY 社区节点清单地址或文件（配置文件未设置时使用）
  RUST_LOG       日志级别（默认 info）";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
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
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into())).init();
    let dir = PathBuf::from(std::env::var("XSEED_DATA").unwrap_or_else(|_| "data".into()));
    let app = app::App::open(dir.clone())?;
    if cmd == "reset-password" {
        app.db.lock().execute("DELETE FROM meta WHERE k='admin_hash'", [])?;
        println!("已清除管理员密码，打开界面重新设置。");
        return Ok(());
    }
    let listen = std::env::var("XSEED_LISTEN").unwrap_or_else(|_| app.cfg().webui.listen);
    let addr: SocketAddr = listen.parse()?;
    tokio::spawn(app::background(app.clone()));
    tokio::spawn(network::run(app.clone()));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("xseed-agent {} 已启动：http://{addr}  数据目录 {}", env!("CARGO_PKG_VERSION"), dir.display());
    axum::serve(listener, web::router(app).into_make_service_with_connect_info::<SocketAddr>())
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}
