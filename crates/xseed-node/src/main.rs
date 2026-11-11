//! xseed-node：社区节点。

use anyhow::{Context, Result};
use std::path::PathBuf;
use xseed_node::{config::Config, node};

const HELP: &str = "xseed-node — xseed 社区节点

用法：
  xseed-node [run]              启动节点
  xseed-node manifest [URL]     打印本节点的清单条目（JSON），提交到注册中心

注册中心维护者：
  xseed-node registry keygen KEY_FILE             生成签名私钥，打印公钥
  xseed-node registry build DIR [OUT]             由 DIR/nodes/*.json、DIR/sites/*.json、DIR/public-trackers.txt
                                                  生成 registry.json（版本号取当前时间）
  xseed-node registry sign KEY_FILE REGISTRY      写出 REGISTRY.sig
  xseed-node registry verify REGISTRY PUBKEY...   验证签名与内容
  xseed-node --version

环境变量：
  XSEED_NODE_DATA      数据目录（默认 ./node-data）：xseed-node.toml、node.key、snapshot.cbor
  XSEED_NODE_LISTEN    监听地址（覆盖配置文件）
  XSEED_NODE_URL       对外地址（覆盖配置文件）
  XSEED_NODE_REGISTRY  节点清单地址或文件（覆盖配置文件）
  RUST_LOG             日志级别（默认 info）";

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("run");
    match cmd {
        "-h" | "--help" | "help" => {
            println!("{HELP}");
            return Ok(());
        }
        "-V" | "--version" => {
            println!("xseed-node {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        "registry" => return xseed_node::registry_tool::run(&args[1..]),
        "run" | "manifest" => {}
        other => anyhow::bail!("未知命令 {other}\n\n{HELP}"),
    }
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into())).init();

    let dir = PathBuf::from(std::env::var("XSEED_NODE_DATA").unwrap_or_else(|_| "node-data".into()));
    std::fs::create_dir_all(&dir)?;
    let cfg_path = dir.join("xseed-node.toml");
    let mut cfg: Config = match std::fs::read_to_string(&cfg_path) {
        Ok(s) => toml::from_str(&s).context("xseed-node.toml 格式错误")?,
        Err(_) => {
            let c = Config::default();
            std::fs::write(&cfg_path, toml::to_string_pretty(&c)?)?;
            c
        }
    };
    if let Ok(v) = std::env::var("XSEED_NODE_LISTEN") {
        cfg.listen = v;
    }
    if let Ok(v) = std::env::var("XSEED_NODE_URL") {
        cfg.url = v;
    }
    if let Ok(v) = std::env::var("XSEED_NODE_REGISTRY") {
        cfg.registry = v;
    }
    let keys = node::Keys::load_or_create(&dir)?;
    let n = node::Node::new(&dir, cfg.clone(), &keys)?;

    if cmd == "manifest" {
        let url = args.get(1).cloned().unwrap_or_else(|| cfg.url.clone());
        anyhow::ensure!(!url.is_empty(), "请给出对外地址：xseed-node manifest https://node.example.org");
        println!("{}", serde_json::to_string_pretty(&n.manifest(&url))?);
        return Ok(());
    }

    if let Err(e) = n.reload_registry().await {
        tracing::warn!("读取节点清单失败（{}）：{e:#}。在清单可用之前，本节点只能服务直接连接的信箱", cfg.registry);
    }
    let listener = tokio::net::TcpListener::bind(&cfg.listen).await.with_context(|| format!("监听 {}", cfg.listen))?;
    tracing::info!("xseed-node {} 已启动：{}  node_id {}", env!("CARGO_PKG_VERSION"), cfg.listen, hex::encode(n.id));
    let shutdown = async {
        let ctrl_c = tokio::signal::ctrl_c();
        #[cfg(unix)]
        {
            let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("信号");
            tokio::select! { _ = ctrl_c => {}, _ = term.recv() => {} }
        }
        #[cfg(not(unix))]
        let _ = ctrl_c.await;
        tracing::info!("正在停止，写入快照…");
    };
    xseed_node::serve(n, listener, shutdown).await
}
