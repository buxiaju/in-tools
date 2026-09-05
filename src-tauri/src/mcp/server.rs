//! MCP 服务端：Streamable HTTP 端点与 Bearer Token 鉴权。
//!
//! 对外只开一个 `POST /mcp`，绑定回环地址，实现 MCP 的三个核心方法
//! `initialize` / `tools/list` / `tools/call`。
//!
//! # 为什么后端是抽象的
//!
//! 本模块位于 lib 内，而 lib **不允许链接 tauri**（见 `lib.rs` 的说明）。
//! 因此这里不能直接持有 `Supervisor<SharedPrompter>`，只能依赖两个抽象：
//!
//! - [`ToolInvoker`]：真正把调用打到插件进程上；
//! - [`ToolTableSource`]：给出「此刻应当暴露哪些工具」的快照。
//!
//! `main.rs` 用闭包捕获 Supervisor 来满足后者，测试则可以自造映射表。
//!
//! # 快照为什么每次请求都重建
//!
//! 用户在权限页勾选／取消勾选后，期望下一次 `tools/list` 立刻生效。缓存会让
//! 「取消勾选」延迟生效——这是安全方向上的错误，所以宁可每次重建。

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{json, Value as JsonValue};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::mcp::bridge::McpToolTable;
use crate::permission::CallerIdentity;
use crate::protocol::message::JsonRpcError;
use crate::runtime::supervisor::ToolInvoker;

/// 网关默认端口。规格里写死，不做自动选端口——端口飘移会让客户端配置失效。
pub const DEFAULT_MCP_PORT: u16 = 7801;

/// 声明支持的 MCP 协议版本。
pub const MCP_PROTOCOL_VERSION: &str = "2025-06-18";

/// 客户端未在 `initialize` 里报名时的兜底名字，会出现在审计日志中。
const UNKNOWN_CLIENT: &str = "unknown";

/// 默认监听地址：**只绑回环**。绑 0.0.0.0 等于把插件能力暴露给局域网。
pub fn default_addr() -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], DEFAULT_MCP_PORT))
}

// ─────────────────── 后端抽象 ───────────────────

/// 工具映射表的快照来源。
///
/// 每次 `tools/list` / `tools/call` 都会取一次，实现方负责读注册表与白名单。
pub trait ToolTableSource: Send + Sync {
    fn snapshot(&self) -> McpToolTable;
}

impl<F> ToolTableSource for F
where
    F: Fn() -> McpToolTable + Send + Sync,
{
    fn snapshot(&self) -> McpToolTable {
        self()
    }
}

// ─────────────────── 服务端状态 ───────────────────

/// 端点的共享状态。
pub struct McpState {
    token: String,
    invoker: Arc<dyn ToolInvoker>,
    table: Arc<dyn ToolTableSource>,
    /// 客户端在 `initialize` 时报的名字，仅用于审计日志归因。
    client_name: Mutex<String>,
}

impl McpState {
    pub fn new(
        token: impl Into<String>,
        invoker: Arc<dyn ToolInvoker>,
        table: Arc<dyn ToolTableSource>,
    ) -> Self {
        Self {
            token: token.into(),
            invoker,
            table,
            client_name: Mutex::new(UNKNOWN_CLIENT.to_string()),
        }
    }

    fn current_client(&self) -> String {
        self.client_name
            .lock()
            .map(|g| g.clone())
            .unwrap_or_else(|_| UNKNOWN_CLIENT.to_string())
    }

    fn remember_client(&self, name: &str) {
        if let Ok(mut g) = self.client_name.lock() {
            *g = name.to_string();
        }
    }
}

// ─────────────────── 启动与关停 ───────────────────

#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    #[error("端口 {port} 已被占用，MCP 网关未能启动。请关闭占用该端口的程序后重试。")]
    PortInUse { port: u16 },

    #[error("绑定 {addr} 失败：{source}")]
    Bind {
        addr: SocketAddr,
        #[source]
        source: std::io::Error,
    },
}

/// 运行中的服务端句柄。
///
/// 丢弃时会自动发出关停信号，避免测试里忘记 `shutdown` 导致端口泄漏。
#[derive(Debug)]
pub struct McpServerHandle {
    local_addr: SocketAddr,
    shutdown: Option<oneshot::Sender<()>>,
    join: JoinHandle<()>,
}

impl McpServerHandle {
    /// 实际监听地址。传入端口 0 时用它拿到系统分配的端口。
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// 优雅关停并等待任务收尾。
    pub async fn shutdown(mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        // 句柄被 take 过，Drop 里不会重复发信号。
        let join = std::mem::replace(&mut self.join, tokio::spawn(async {}));
        let _ = join.await;
    }
}

impl Drop for McpServerHandle {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
    }
}

/// 构造路由。独立出来便于测试直接挂 Service。
pub fn router(state: Arc<McpState>) -> Router {
    Router::new()
        .route("/mcp", post(handle_mcp))
        .with_state(state)
}

/// 绑定并启动服务端。
///
/// 端口被占用时返回 [`ServerError::PortInUse`]，由调用方决定是提示用户还是回滚
/// `mcp_enabled` 开关——本模块不擅自改配置。
pub async fn start(addr: SocketAddr, state: Arc<McpState>) -> Result<McpServerHandle, ServerError> {
    let listener = TcpListener::bind(addr).await.map_err(|e| {
        if e.kind() == std::io::ErrorKind::AddrInUse {
            ServerError::PortInUse { port: addr.port() }
        } else {
            ServerError::Bind { addr, source: e }
        }
    })?;

    let local_addr = listener.local_addr().unwrap_or(addr);
    let (tx, rx) = oneshot::channel::<()>();

    let app = router(state);
    let join = tokio::spawn(async move {
        let served = axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = rx.await;
            })
            .await;
        if let Err(e) = served {
            tracing::error!(error = %e, "MCP 服务端异常退出");
        }
    });

    tracing::info!(addr = %local_addr, "MCP 网关已启动");
    Ok(McpServerHandle {
        local_addr,
        shutdown: Some(tx),
        join,
    })
}

// ─────────────────── 请求处理 ───────────────────

async fn handle_mcp(
    State(state): State<Arc<McpState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if !token_ok(&state.token, &headers) {
        tracing::warn!("MCP 请求鉴权失败");
        return unauthorized();
    }

    let parsed: JsonValue = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            return rpc_error_response(
                JsonValue::Null,
                JsonRpcError::new(JsonRpcError::CODE_PARSE_ERROR, format!("JSON 解析失败：{e}")),
            )
        }
    };

    // 没有 id 的是通知（如 notifications/initialized），按 JSON-RPC 语义不回响应。
    let Some(id) = parsed.get("id").cloned() else {
        return StatusCode::ACCEPTED.into_response();
    };

    let Some(method) = parsed.get("method").and_then(JsonValue::as_str) else {
        return rpc_error_response(
            id,
            JsonRpcError::new(JsonRpcError::CODE_INVALID_REQUEST, "缺少 method 字段"),
        );
    };

    let params = parsed
        .get("params")
        .cloned()
        .unwrap_or_else(|| json!({}));

    match method {
        "initialize" => {
            let name = params
                .get("clientInfo")
                .and_then(|c| c.get("name"))
                .and_then(JsonValue::as_str)
                .unwrap_or(UNKNOWN_CLIENT);
            state.remember_client(name);
            tracing::info!(client = %name, "MCP 客户端已握手");
            rpc_ok_response(id, initialize_result())
        }
        "tools/list" => {
            let table = state.table.snapshot();
            tracing::debug!(count = table.len(), "MCP tools/list");
            rpc_ok_response(id, json!({ "tools": table.to_mcp_tools_json() }))
        }
        "tools/call" => handle_tools_call(&state, id, &params).await,
        other => rpc_error_response(
            id,
            JsonRpcError::new(
                JsonRpcError::CODE_METHOD_NOT_FOUND,
                format!("不支持的方法 `{other}`"),
            ),
        ),
    }
}

async fn handle_tools_call(state: &McpState, id: JsonValue, params: &JsonValue) -> Response {
    let Some(mcp_name) = params.get("name").and_then(JsonValue::as_str) else {
        return rpc_error_response(
            id,
            JsonRpcError::new(JsonRpcError::CODE_INVALID_PARAMS, "缺少 name 参数"),
        );
    };

    // 反解只走映射表。to_mcp_name 是有损的，绝不能试图从 mcp_name 还原原名。
    let table = state.table.snapshot();
    let Some(entry) = table.resolve(mcp_name) else {
        return rpc_error_response(
            id,
            JsonRpcError::new(
                JsonRpcError::CODE_TOOL_NOT_FOUND,
                format!("未暴露的工具 `{mcp_name}`"),
            ),
        );
    };

    let args = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let caller = CallerIdentity::Mcp {
        client_name: state.current_client(),
    };

    match state
        .invoker
        .call_tool(&entry.original_name, args, caller)
        .await
    {
        Ok(value) => rpc_ok_response(id, tool_call_result(value)),
        Err(e) => {
            tracing::warn!(tool = %entry.original_name, error = %e, "MCP 工具调用失败");
            rpc_error_response(id, e.to_rpc_error())
        }
    }
}

// ─────────────────── 辅助 ───────────────────

/// 校验 `Authorization: Bearer <token>`。
///
/// 用定长累加比较而非 `==`，避免按前缀提前返回泄漏 token 长度与内容。
fn token_ok(expected: &str, headers: &HeaderMap) -> bool {
    let Some(raw) = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
    else {
        return false;
    };
    let Some(got) = raw.strip_prefix("Bearer ") else {
        return false;
    };
    constant_time_eq(expected.as_bytes(), got.trim().as_bytes())
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [(
            axum::http::header::WWW_AUTHENTICATE,
            "Bearer realm=\"intools\"",
        )],
        Json(json!({ "error": "缺少或错误的 Bearer token" })),
    )
        .into_response()
}

fn initialize_result() -> JsonValue {
    json!({
        "protocolVersion": MCP_PROTOCOL_VERSION,
        "capabilities": { "tools": { "listChanged": false } },
        "serverInfo": {
            "name": "InTools",
            "version": env!("CARGO_PKG_VERSION"),
        }
    })
}

/// 把插件返回值包成 MCP 的 `tools/call` 结果。
///
/// 同时给出 `content`（所有客户端都认）与 `structuredContent`（新客户端可直接
/// 拿到结构化数据），避免把 JSON 压成字符串后让对端再解析一次。
fn tool_call_result(value: JsonValue) -> JsonValue {
    let text = match &value {
        JsonValue::String(s) => s.clone(),
        other => serde_json::to_string_pretty(other).unwrap_or_else(|_| other.to_string()),
    };
    json!({
        "content": [{ "type": "text", "text": text }],
        "structuredContent": value,
        "isError": false,
    })
}

fn rpc_ok_response(id: JsonValue, result: JsonValue) -> Response {
    Json(json!({ "jsonrpc": "2.0", "id": id, "result": result })).into_response()
}

/// JSON-RPC 的业务错误走 HTTP 200：传输层是通的，错的是这一次调用。
/// 只有鉴权失败才用 401——那是传输层就该拦下的。
fn rpc_error_response(id: JsonValue, err: JsonRpcError) -> Response {
    Json(json!({ "jsonrpc": "2.0", "id": id, "error": err })).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::bridge::McpToolEntry;
    use crate::protocol::manifest::ToolDescriptor;
    use crate::runtime::supervisor::InvokeError;
    use std::sync::Mutex as StdMutex;

    /// 记录调用参数的假后端，用来断言「服务端把 mcp_name 反解成了原始名」。
    struct FakeInvoker {
        calls: StdMutex<Vec<(String, JsonValue, String)>>,
        fail: bool,
    }

    impl FakeInvoker {
        fn new() -> Self {
            Self {
                calls: StdMutex::new(Vec::new()),
                fail: false,
            }
        }

        fn failing() -> Self {
            Self {
                calls: StdMutex::new(Vec::new()),
                fail: true,
            }
        }

        fn last_call(&self) -> Option<(String, JsonValue, String)> {
            self.calls.lock().unwrap().last().cloned()
        }
    }

    #[async_trait::async_trait]
    impl ToolInvoker for FakeInvoker {
        async fn list_tools(&self) -> Vec<(String, ToolDescriptor)> {
            Vec::new()
        }

        async fn call_tool(
            &self,
            tool_name: &str,
            args: JsonValue,
            caller: CallerIdentity,
        ) -> Result<JsonValue, InvokeError> {
            let who = match &caller {
                CallerIdentity::Mcp { client_name } => client_name.clone(),
                other => format!("{other:?}"),
            };
            self.calls
                .lock()
                .unwrap()
                .push((tool_name.to_string(), args.clone(), who));
            if self.fail {
                return Err(InvokeError::ToolNotFound {
                    tool: tool_name.to_string(),
                });
            }
            Ok(json!({ "echo": args }))
        }
    }

    fn table_with(entries: Vec<McpToolEntry>) -> McpToolTable {
        McpToolTable::from_entries(entries)
    }

    fn demo_entry() -> McpToolEntry {
        McpToolEntry {
            mcp_name: "ocr_recognize".to_string(),
            original_name: "ocr:recognize".to_string(),
            plugin_id: "com.demo.ocr".to_string(),
            description: "识别图片文字".to_string(),
            input_schema: json!({ "type": "object" }),
        }
    }

    async fn spawn(state: Arc<McpState>) -> McpServerHandle {
        start(SocketAddr::from(([127, 0, 0, 1], 0)), state)
            .await
            .expect("端口 0 应当总能绑定成功")
    }

    fn state_with(invoker: Arc<FakeInvoker>, entries: Vec<McpToolEntry>) -> Arc<McpState> {
        let table = table_with(entries);
        Arc::new(McpState::new(
            "secret-token",
            invoker,
            Arc::new(move || table.clone()),
        ))
    }

    async fn post_json(
        addr: SocketAddr,
        token: Option<&str>,
        body: JsonValue,
    ) -> (u16, Option<JsonValue>) {
        let client = reqwest::Client::new();
        let mut req = client.post(format!("http://{addr}/mcp")).json(&body);
        if let Some(t) = token {
            req = req.bearer_auth(t);
        }
        let resp = req.send().await.expect("请求应当能送达");
        let status = resp.status().as_u16();
        let json = resp.json::<JsonValue>().await.ok();
        (status, json)
    }

    #[tokio::test]
    async fn 无token返回401() {
        let state = state_with(Arc::new(FakeInvoker::new()), vec![demo_entry()]);
        let server = spawn(state).await;
        let (status, _) = post_json(
            server.local_addr(),
            None,
            json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }),
        )
        .await;
        assert_eq!(status, 401, "缺少 Authorization 头必须被拒");
        server.shutdown().await;
    }

    #[tokio::test]
    async fn 错误token返回401() {
        let state = state_with(Arc::new(FakeInvoker::new()), vec![demo_entry()]);
        let server = spawn(state).await;
        let (status, _) = post_json(
            server.local_addr(),
            Some("wrong-token"),
            json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }),
        )
        .await;
        assert_eq!(status, 401, "token 不匹配必须被拒");
        server.shutdown().await;
    }

    #[tokio::test]
    async fn 等长错误token同样返回401() {
        // 定长比较的边界：长度一致但内容不同，不能因为提前 return 而放行。
        let state = state_with(Arc::new(FakeInvoker::new()), vec![demo_entry()]);
        let server = spawn(state).await;
        let (status, _) = post_json(
            server.local_addr(),
            Some("secret-tokeX"),
            json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }),
        )
        .await;
        assert_eq!(status, 401, "等长但不同的 token 必须被拒");
        server.shutdown().await;
    }

    #[tokio::test]
    async fn 正确token可以列出工具() {
        let state = state_with(Arc::new(FakeInvoker::new()), vec![demo_entry()]);
        let server = spawn(state).await;
        let (status, body) = post_json(
            server.local_addr(),
            Some("secret-token"),
            json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }),
        )
        .await;
        assert_eq!(status, 200);
        let body = body.expect("应当有 JSON 响应体");
        let tools = body["result"]["tools"]
            .as_array()
            .expect("result.tools 应当是数组");
        assert_eq!(tools.len(), 1, "实际响应：{body}");
        assert_eq!(tools[0]["name"], "ocr_recognize");
        assert!(
            tools[0].get("inputSchema").is_some(),
            "必须用 MCP 的 inputSchema 驼峰命名，实际：{}",
            tools[0]
        );
        server.shutdown().await;
    }

    #[tokio::test]
    async fn 空映射表返回空列表而非报错() {
        let state = state_with(Arc::new(FakeInvoker::new()), vec![]);
        let server = spawn(state).await;
        let (status, body) = post_json(
            server.local_addr(),
            Some("secret-token"),
            json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }),
        )
        .await;
        assert_eq!(status, 200);
        let body = body.unwrap();
        assert_eq!(
            body["result"]["tools"].as_array().map(|a| a.len()),
            Some(0),
            "默认零暴露时应当是空数组，实际：{body}"
        );
        server.shutdown().await;
    }

    #[tokio::test]
    async fn initialize返回协议版本并记住客户端名() {
        let invoker = Arc::new(FakeInvoker::new());
        let state = state_with(Arc::clone(&invoker), vec![demo_entry()]);
        let server = spawn(Arc::clone(&state)).await;
        let (status, body) = post_json(
            server.local_addr(),
            Some("secret-token"),
            json!({
                "jsonrpc": "2.0", "id": 1, "method": "initialize",
                "params": { "clientInfo": { "name": "claude-desktop" } }
            }),
        )
        .await;
        assert_eq!(status, 200);
        let body = body.unwrap();
        assert_eq!(body["result"]["protocolVersion"], MCP_PROTOCOL_VERSION);

        post_json(
            server.local_addr(),
            Some("secret-token"),
            json!({
                "jsonrpc": "2.0", "id": 2, "method": "tools/call",
                "params": { "name": "ocr_recognize", "arguments": {} }
            }),
        )
        .await;
        let (_, _, who) = invoker.last_call().expect("应当发生过调用");
        assert_eq!(who, "claude-desktop", "审计归因必须用 initialize 报的名字");
        server.shutdown().await;
    }

    #[tokio::test]
    async fn 调用时mcp名被反解回原始名() {
        let invoker = Arc::new(FakeInvoker::new());
        let state = state_with(Arc::clone(&invoker), vec![demo_entry()]);
        let server = spawn(state).await;
        let (status, body) = post_json(
            server.local_addr(),
            Some("secret-token"),
            json!({
                "jsonrpc": "2.0", "id": 7, "method": "tools/call",
                "params": { "name": "ocr_recognize", "arguments": { "path": "a.png" } }
            }),
        )
        .await;
        assert_eq!(status, 200);
        let body = body.unwrap();
        assert_eq!(body["id"], 7, "id 必须原样回填");
        assert_eq!(body["result"]["isError"], false);
        assert_eq!(
            body["result"]["structuredContent"]["echo"]["path"], "a.png",
            "实际响应：{body}"
        );

        let (tool, args, _) = invoker.last_call().expect("应当发生过调用");
        assert_eq!(tool, "ocr:recognize", "必须查表反解，而不是直接用 mcp_name");
        assert_eq!(args["path"], "a.png");
        server.shutdown().await;
    }

    #[tokio::test]
    async fn 调用未暴露的工具返回工具未找到() {
        let state = state_with(Arc::new(FakeInvoker::new()), vec![demo_entry()]);
        let server = spawn(state).await;
        let (status, body) = post_json(
            server.local_addr(),
            Some("secret-token"),
            json!({
                "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                "params": { "name": "never_exposed", "arguments": {} }
            }),
        )
        .await;
        assert_eq!(status, 200, "业务错误走 200，只有鉴权失败才 401");
        let body = body.unwrap();
        assert_eq!(
            body["error"]["code"], JsonRpcError::CODE_TOOL_NOT_FOUND,
            "实际响应：{body}"
        );
        server.shutdown().await;
    }

    #[tokio::test]
    async fn 内核错误被转成对应的rpc错误码() {
        let state = state_with(Arc::new(FakeInvoker::failing()), vec![demo_entry()]);
        let server = spawn(state).await;
        let (_, body) = post_json(
            server.local_addr(),
            Some("secret-token"),
            json!({
                "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                "params": { "name": "ocr_recognize", "arguments": {} }
            }),
        )
        .await;
        let body = body.unwrap();
        assert_eq!(body["error"]["code"], JsonRpcError::CODE_TOOL_NOT_FOUND);
        server.shutdown().await;
    }

    #[tokio::test]
    async fn 未知方法返回方法未找到() {
        let state = state_with(Arc::new(FakeInvoker::new()), vec![demo_entry()]);
        let server = spawn(state).await;
        let (_, body) = post_json(
            server.local_addr(),
            Some("secret-token"),
            json!({ "jsonrpc": "2.0", "id": 1, "method": "resources/list" }),
        )
        .await;
        let body = body.unwrap();
        assert_eq!(body["error"]["code"], JsonRpcError::CODE_METHOD_NOT_FOUND);
        server.shutdown().await;
    }

    #[tokio::test]
    async fn 通知不返回响应体() {
        let state = state_with(Arc::new(FakeInvoker::new()), vec![demo_entry()]);
        let server = spawn(state).await;
        let client = reqwest::Client::new();
        let resp = client
            .post(format!("http://{}/mcp", server.local_addr()))
            .bearer_auth("secret-token")
            .json(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 202, "无 id 的通知应当只回 202");
        server.shutdown().await;
    }

    #[tokio::test]
    async fn 非法json返回解析错误() {
        let state = state_with(Arc::new(FakeInvoker::new()), vec![demo_entry()]);
        let server = spawn(state).await;
        let client = reqwest::Client::new();
        let resp = client
            .post(format!("http://{}/mcp", server.local_addr()))
            .bearer_auth("secret-token")
            .header("content-type", "application/json")
            .body("{ not json")
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 200);
        let body: JsonValue = resp.json().await.unwrap();
        assert_eq!(body["error"]["code"], JsonRpcError::CODE_PARSE_ERROR);
        server.shutdown().await;
    }

    #[tokio::test]
    async fn 端口被占用时给出可识别的错误() {
        let first = spawn(state_with(Arc::new(FakeInvoker::new()), vec![])).await;
        let addr = first.local_addr();
        let err = start(addr, state_with(Arc::new(FakeInvoker::new()), vec![]))
            .await
            .expect_err("同一端口再绑一次应当失败");
        match err {
            ServerError::PortInUse { port } => assert_eq!(port, addr.port()),
            other => panic!("应当是 PortInUse，实际：{other:?}"),
        }
        first.shutdown().await;
    }

    #[tokio::test]
    async fn 关停后端口可以立刻再次绑定() {
        let server = spawn(state_with(Arc::new(FakeInvoker::new()), vec![])).await;
        let addr = server.local_addr();
        server.shutdown().await;
        let again = start(addr, state_with(Arc::new(FakeInvoker::new()), vec![])).await;
        assert!(again.is_ok(), "优雅关停后应当能重新绑定同一端口");
    }
}
