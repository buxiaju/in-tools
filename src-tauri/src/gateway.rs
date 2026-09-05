//! MCP 网关的生命周期管理：把「开关」这个瞬时动作变成真正的启停。
//!
//! 单独成模块而不是塞进 `commands.rs`，是因为它有自己的状态机（未运行 / 运行中）
//! 且必须保证同一时刻只有一个监听实例——重复开启会撞端口，而端口冲突的报错
//! 对用户来说毫无意义（明明是自己开了两次）。
//!
//! 这里**刻意不引用 `Supervisor`**，只吃 [`ToolInvoker`] 与 [`ToolTableSource`]
//! 两个 trait。好处是本模块无需构造完整内核即可单测，坏处只是 `main.rs`
//! 多写一次类型转换——这笔买卖划算。

use std::net::SocketAddr;
use std::sync::Arc;

use tokio::sync::Mutex;

use intools::config::McpExposure;
use intools::mcp::bridge::McpToolTable;
use intools::mcp::server::{self, McpServerHandle, ToolTableSource};
use intools::permission::PermissionPrompter;
use intools::runtime::supervisor::{Supervisor, ToolInvoker};

/// 网关句柄的持有者。
///
/// 用异步 [`Mutex`] 而非 `std::sync::Mutex`：关停要 `await` join handle，
/// 在同步锁里 await 会把锁跨越 await 点，编译不过也不该那么写。
#[derive(Default)]
pub struct McpGateway {
    running: Mutex<Option<McpServerHandle>>,
}

impl std::fmt::Debug for McpGateway {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpGateway").finish_non_exhaustive()
    }
}

impl McpGateway {
    pub fn new() -> Self {
        Self::default()
    }

    /// 当前是否已在监听。
    pub async fn is_running(&self) -> bool {
        self.running.lock().await.is_some()
    }

    /// 已启动时返回实际监听地址。测试里用它拿随机端口。
    pub async fn local_addr(&self) -> Option<SocketAddr> {
        self.running.lock().await.as_ref().map(|h| h.local_addr())
    }

    /// 启动网关。
    ///
    /// 幂等：已在运行则直接返回 `Ok`，不会去撞自己的端口。
    /// 失败时**不残留半启动状态**——`server::start` 要么绑上要么什么都没发生。
    pub async fn start(
        &self,
        addr: SocketAddr,
        token: impl Into<String>,
        invoker: Arc<dyn ToolInvoker>,
        table: Arc<dyn ToolTableSource>,
    ) -> Result<SocketAddr, server::ServerError> {
        let mut slot = self.running.lock().await;
        if let Some(handle) = slot.as_ref() {
            return Ok(handle.local_addr());
        }

        let state = Arc::new(server::McpState::new(token, invoker, table));
        let handle = server::start(addr, state).await?;
        let bound = handle.local_addr();
        *slot = Some(handle);
        Ok(bound)
    }

    /// 关停网关并等待端口真正释放。
    ///
    /// 幂等：没在运行就什么都不做。**必须 await 到 join 完成**，否则用户
    /// 关了再开会撞上还没来得及释放的端口。
    pub async fn stop(&self) {
        let handle = self.running.lock().await.take();
        if let Some(handle) = handle {
            handle.shutdown().await;
            tracing::info!("MCP 网关已关停");
        }
    }
}

/// 用 Supervisor 的注册表 + 暴露白名单造一个「每次调用都重新算」的映射表源。
///
/// **不缓存**是有意的：用户在权限页取消勾选后，下一次 `tools/list` 就该看不到
/// 那个工具。缓存会让撤销延迟生效，而安全相关的变更只能即时收紧、不能滞后。
///
/// 白名单读失败时退化为 `McpExposure::default()`（零暴露）而不是沿用旧值或报错：
/// 配置文件坏了的时候，「什么都不暴露」是唯一安全的默认。
pub fn supervisor_table_source<P>(supervisor: Arc<Supervisor<P>>) -> Arc<dyn ToolTableSource>
where
    P: PermissionPrompter + 'static,
{
    Arc::new(move || {
        let exposure = McpExposure::load().unwrap_or_else(|e| {
            tracing::warn!(error = %e, "读取 MCP 暴露白名单失败，本次按零暴露处理");
            McpExposure::default()
        });
        McpToolTable::build(&supervisor.registry(), &exposure)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use intools::permission::CallerIdentity;
    use intools::protocol::manifest::ToolDescriptor;
    use intools::runtime::supervisor::InvokeError;
    use serde_json::Value as JsonValue;

    struct NoopInvoker;

    #[async_trait]
    impl ToolInvoker for NoopInvoker {
        async fn list_tools(&self) -> Vec<(String, ToolDescriptor)> {
            Vec::new()
        }

        async fn call_tool(
            &self,
            tool_name: &str,
            _args: JsonValue,
            _caller: CallerIdentity,
        ) -> Result<JsonValue, InvokeError> {
            Err(InvokeError::ToolNotFound {
                tool: tool_name.to_string(),
            })
        }
    }

    fn parts() -> (Arc<dyn ToolInvoker>, Arc<dyn ToolTableSource>) {
        (
            Arc::new(NoopInvoker),
            Arc::new(McpToolTable::default) as Arc<dyn ToolTableSource>,
        )
    }

    /// 端口 0 让内核分配空闲端口，避免测试之间抢固定端口。
    fn any_port() -> SocketAddr {
        SocketAddr::from(([127, 0, 0, 1], 0))
    }

    #[tokio::test]
    async fn 初始状态未运行() {
        let gw = McpGateway::new();
        assert!(!gw.is_running().await);
        assert!(gw.local_addr().await.is_none());
    }

    #[tokio::test]
    async fn 启动后进入运行态() {
        let gw = McpGateway::new();
        let (invoker, table) = parts();

        let addr = gw.start(any_port(), "tok", invoker, table).await.unwrap();

        assert!(gw.is_running().await);
        assert_eq!(gw.local_addr().await, Some(addr));
        assert_ne!(addr.port(), 0, "应当拿到内核分配的真实端口");

        gw.stop().await;
    }

    #[tokio::test]
    async fn 重复启动不会撞端口() {
        let gw = McpGateway::new();
        let (invoker, table) = parts();

        let first = gw
            .start(any_port(), "tok", invoker.clone(), table.clone())
            .await
            .unwrap();
        // 第二次仍传 0 号端口，如果实现没做幂等就会绑到另一个端口上，
        // 旧实例泄漏且地址对不上。
        let second = gw.start(any_port(), "tok", invoker, table).await.unwrap();

        assert_eq!(first, second, "已在运行时应当复用现有实例");

        gw.stop().await;
    }

    #[tokio::test]
    async fn 关停后回到未运行态且端口可复用() {
        let gw = McpGateway::new();
        let (invoker, table) = parts();

        let addr = gw
            .start(any_port(), "tok", invoker.clone(), table.clone())
            .await
            .unwrap();
        gw.stop().await;

        assert!(!gw.is_running().await);
        // 拿刚释放的端口原样再绑一次：这是「关了再开」能不能用的直接证据。
        gw.start(addr, "tok", invoker, table)
            .await
            .expect("关停后同一端口应当可以立刻重新绑定");

        gw.stop().await;
    }

    #[tokio::test]
    async fn 未运行时关停是安全的空操作() {
        let gw = McpGateway::new();
        gw.stop().await;
        gw.stop().await;
        assert!(!gw.is_running().await);
    }
}
