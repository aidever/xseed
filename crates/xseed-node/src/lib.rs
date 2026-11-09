//! xseed 社区节点（03 §1.4、§1.7、§2.1、§3.2）。

pub mod config;
pub mod http;
pub mod limit;
pub mod node;

use anyhow::Result;
use std::net::SocketAddr;

/// 在给定监听器上运行节点，直到 `shutdown` 完成；随后写快照。
pub async fn serve(node: node::Shared, listener: tokio::net::TcpListener, shutdown: impl std::future::Future<Output = ()> + Send + 'static) -> Result<()> {
    let bg = node.clone();
    let maint = tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(600));
        let mut n = 0u64;
        loop {
            tick.tick().await;
            n += 1;
            if n.is_multiple_of(6) {
                bg.sweep();
                if let Err(e) = bg.reload_registry().await {
                    tracing::warn!("刷新节点清单失败：{e:#}");
                }
            }
            if let Err(e) = bg.save_snapshot() {
                tracing::warn!("写快照失败：{e:#}");
            }
        }
    });
    axum::serve(listener, http::router(node.clone()).into_make_service_with_connect_info::<SocketAddr>()).with_graceful_shutdown(shutdown).await?;
    maint.abort();
    node.save_snapshot()?;
    Ok(())
}
