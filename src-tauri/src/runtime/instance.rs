//! 插件实例状态机：握手、请求超时、空闲回收、连接断开处理。
//!
//! 状态转移遵循设计文档 §5.6：
//!
//! ```text
//! Stopped ──→ Starting ──→ Idle ⇄ Busy ──→ Stopping ──→ Stopped
//!                │           │
//!             (失败)     (空闲超时)
//!                ▼           ▼
//!              Error      Stopping
//! ```
//!
//! 握手期采用**内联收消息**：`start()` 自己从 [`Transport`] 读取直到拿到
//! `plugin/hello`，再发出 `plugin/ready` 并等待其响应。握手完成后才启动
//! 读循环。这样避免了「读循环与握手互相等待」的死结，启动路径是纯串行的。

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use serde::Deserialize;
use serde_json::{json, Value as JsonValue};
use thiserror::Error;
use tokio::task::JoinHandle;
use tokio::time;

use crate::protocol::manifest::{
    negotiate_version, Lifecycle, LifecycleMode, ProtocolVersion, ToolDescriptor,
};
use crate::protocol::message::{
    IncomingMessage, JsonRpcError, JsonRpcNotification, JsonRpcRequest, JsonRpcResponse,
    OutgoingMessage, RequestId, ResponseBody,
};

use super::transport::{PendingTable, Transport, TransportError};

/// 握手超时：10 秒内未收到 `plugin/hello` 判定启动失败。
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// 请求超时缺省值，可被 `Lifecycle::request_timeout_sec` 覆盖。
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// 空闲超时缺省值，可被 `Lifecycle::idle_timeout_sec` 覆盖。
pub const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(300);

/// 插件握手方法名：插件 → 宿主，通知。
pub const METHOD_HELLO: &str = "plugin/hello";

/// 宿主就绪方法名：宿主 → 插件，请求。
pub const METHOD_READY: &str = "plugin/ready";

/// 反向 RPC 的方法名前缀。插件发来的请求只有以此开头才会被受理。
pub const HOST_METHOD_PREFIX: &str = "host/";

/// 反向 RPC 方法名（设计文档 §5.3 的五项 + 第三期扩展）。
pub const HOST_LIST_TOOLS: &str = "host/listTools";
pub const HOST_CALL_TOOL: &str = "host/callTool";
pub const HOST_GET_CONFIG: &str = "host/getConfig";
pub const HOST_SET_CONFIG: &str = "host/setConfig";
pub const HOST_NOTIFY: &str = "host/notify";
pub const HOST_UI_REQUEST: &str = "host/uiRequest";

/// 插件推给 UI 的通知前缀（设计文档 §5.4：`notify/progress` 等）。
pub const NOTIFY_PREFIX: &str = "notify/";

// ─────────────────── 反向 RPC 宿主侧接口 ───────────────────

/// 插件回调宿主的能力集合（设计文档 §5.3）。
///
/// 抽成 trait 而非让实例直接持有 `Supervisor`，有两个原因：
///
/// 1. **破循环引用**：`Supervisor` 持有 `Arc<PluginInstance>`，若实例反过来
///    持有 `Arc<Supervisor>`，引用计数成环，两者永远不会被释放。实例侧一律
///    用 [`std::sync::Weak`] 持有本 trait 对象。
/// 2. **可测性**：实例的单测不必构造完整 `Supervisor`（那需要注册表、权限
///    存储、传输工厂），注入一个记录调用的假实现即可。
///
/// `depth` 由**宿主**跟踪并传入，绝不接受插件在 params 里自报——否则插件
/// 永远填 0 就能绕开 `MAX_CALL_DEPTH`，深度限制形同虚设。
#[async_trait::async_trait]
pub trait HostHandler: Send + Sync {
    /// 列出全部可用工具，不唤醒任何插件。
    async fn host_list_tools(&self) -> Result<JsonValue, JsonRpcError>;

    /// 调用其他插件的工具。
    ///
    /// `caller_plugin` 是发起方插件 id，`depth` 是**发起方当前所处的调用深度**；
    /// 实现方需以 `depth + 1` 构造 `CallerIdentity::Plugin` 后走完整六步调用链。
    async fn host_call_tool(
        &self,
        caller_plugin: &str,
        depth: u8,
        tool: &str,
        args: JsonValue,
    ) -> Result<JsonValue, JsonRpcError>;

    /// 读取该插件自己的配置。
    async fn host_get_config(&self, caller_plugin: &str) -> Result<JsonValue, JsonRpcError>;

    /// 写入该插件自己的配置。
    async fn host_set_config(
        &self,
        caller_plugin: &str,
        value: JsonValue,
    ) -> Result<JsonValue, JsonRpcError>;

    /// 插件推送给 UI 的通知。Phase 6 先落日志，Phase 7 接 Tauri 事件。
    async fn host_notify(&self, caller_plugin: &str, method: &str, params: JsonValue);

    /// 插件请求宿主弹出特定 UI（第三期扩展）。
    ///
    /// `ui_type` 是 UI 类型（如 "overlay", "dialog", "form" 等），
    /// `schema` 是声明式 UI schema，宿主根据它渲染界面，
    /// `callback_method` 是用户操作完成后插件希望宿主调用的方法名。
    async fn host_ui_request(
        &self,
        caller_plugin: &str,
        ui_type: &str,
        schema: JsonValue,
        callback_method: &str,
    ) -> Result<JsonValue, JsonRpcError>;
}

/// 实例生命周期状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InstanceState {
    /// 未启动，或已正常停止。
    Stopped,
    /// 子进程已拉起，正在握手。
    Starting,
    /// 握手完成，空闲待命。
    Idle,
    /// 至少有一个请求在途。
    Busy,
    /// 正在收尾。
    Stopping,
    /// 启动失败或运行中不可恢复，需人工干预或重启。
    Error,
}

impl InstanceState {
    /// 是否可以接受新的工具调用。
    pub fn accepts_requests(self) -> bool {
        matches!(self, Self::Idle | Self::Busy)
    }

    /// 是否属于终态（不会自行再转移）。
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Stopped | Self::Error)
    }
}

/// 实例层错误。
///
/// 一律用具名字段，避免 thiserror 位置参数的歧义。
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum InstanceError {
    #[error("握手超时：{seconds} 秒内未收到 plugin/hello")]
    HandshakeTimeout { seconds: u64 },

    #[error("握手失败：{reason}")]
    Handshake { reason: String },

    #[error("协议版本不兼容：{reason}")]
    VersionMismatch { reason: String },

    #[error("插件当前状态 {state:?} 不接受请求")]
    NotReady { state: String },

    #[error("请求超时：{method} 超过 {seconds} 秒未返回")]
    RequestTimeout { method: String, seconds: u64 },

    #[error("插件返回错误：[{code}] {message}")]
    Plugin { code: i32, message: String },

    #[error("传输层错误：{source}")]
    Transport {
        #[from]
        source: TransportError,
    },
}

impl InstanceError {
    fn from_rpc(err: JsonRpcError) -> Self {
        Self::Plugin {
            code: err.code,
            message: err.message,
        }
    }
}

/// 插件通过 `plugin/hello` 上报的握手信息。
///
/// 注意 [`ToolDescriptor`] 未启用 `rename_all`，其 JSON 字段是
/// snake_case 的 `input_schema`。
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct HelloParams {
    /// 形如 `"1.0"` 的协议版本串。
    pub protocol_version: String,
    /// 插件运行期实际提供的工具清单。
    #[serde(default)]
    pub tools: Vec<ToolDescriptor>,
}

/// 握手成功后沉淀下来的插件自述信息。
#[derive(Debug, Clone, PartialEq)]
pub struct HandshakeInfo {
    pub protocol_version: ProtocolVersion,
    pub tools: Vec<ToolDescriptor>,
    /// 插件 MINOR 低于宿主时为 `false`，表示宿主需要降级使用新特性。
    pub full_feature: bool,
}

/// 实例的可变部分，统一用一把同步锁看护。
#[derive(Debug)]
struct InstanceInner {
    state: InstanceState,
    handshake: Option<HandshakeInfo>,
    last_error: Option<String>,
    /// 在途**入站**调用的深度多重集：深度 → 该深度上的在途请求数。
    ///
    /// 插件发起 `host/callTool` 时，JSON-RPC 不提供"这条出站请求由哪条入站
    /// 请求触发"的关联信息。因此取在途入站深度的**最大值**作为出站深度依据：
    /// 递归链上的深度只增不减，最深的那条入站请求正是可能在递归的那条，
    /// 取最大值绝不会低估，深度上限因而守得住。空集时为 0（插件自发调用）。
    inbound_depths: BTreeMap<u8, u32>,
}

impl InstanceInner {
    fn enter_inbound(&mut self, depth: u8) {
        *self.inbound_depths.entry(depth).or_insert(0) += 1;
    }

    fn leave_inbound(&mut self, depth: u8) {
        if let Some(count) = self.inbound_depths.get_mut(&depth) {
            *count -= 1;
            if *count == 0 {
                self.inbound_depths.remove(&depth);
            }
        }
    }

    fn max_inbound_depth(&self) -> u8 {
        self.inbound_depths.keys().next_back().copied().unwrap_or(0)
    }
}

/// 一个插件子进程在宿主侧的抽象。
///
/// 只依赖 [`Transport`] trait，因此单测可以完全用 `MockTransport` 驱动，
/// 不需要真实进程；Phase 4 换成 `StdioTransport` 即可对接真实插件。
pub struct PluginInstance {
    plugin_id: String,
    lifecycle: Lifecycle,
    transport: Arc<dyn Transport>,
    pending: Arc<PendingTable>,
    inner: Mutex<InstanceInner>,
    /// 反向 RPC 的宿主回调。
    ///
    /// 必须是 [`Weak`]：`Supervisor` 持有 `Arc<PluginInstance>`，若这里用
    /// `Arc<dyn HostHandler>` 则引用计数成环，两者永不释放。
    /// `Option` 是因为 `Weak::new()` 要求 `T: Sized`，无法凭空造出
    /// 空的 `Weak<dyn HostHandler>`。
    host: Mutex<Option<Weak<dyn HostHandler>>>,
    /// 在途请求计数，用于 Idle ⇄ Busy 的转移。
    inflight: AtomicU64,
    /// 空闲计时的代次，每次活动自增使旧的计时任务作废。
    idle_epoch: AtomicU64,
    reader: Mutex<Option<JoinHandle<()>>>,
}

impl std::fmt::Debug for PluginInstance {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginInstance")
            .field("plugin_id", &self.plugin_id)
            .field("state", &self.state())
            .field("inflight", &self.inflight.load(Ordering::Relaxed))
            .finish()
    }
}

impl PluginInstance {
    /// 用给定传输通道创建实例，初始状态 [`InstanceState::Stopped`]。
    pub fn new(
        plugin_id: impl Into<String>,
        lifecycle: Lifecycle,
        transport: Arc<dyn Transport>,
    ) -> Self {
        Self {
            plugin_id: plugin_id.into(),
            lifecycle,
            transport,
            pending: Arc::new(PendingTable::new()),
            inner: Mutex::new(InstanceInner {
                state: InstanceState::Stopped,
                handshake: None,
                last_error: None,
                inbound_depths: BTreeMap::new(),
            }),
            host: Mutex::new(None),
            inflight: AtomicU64::new(0),
            idle_epoch: AtomicU64::new(0),
            reader: Mutex::new(None),
        }
    }

    /// 注入反向 RPC 的宿主回调。
    ///
    /// 必须在 `Arc::new(supervisor)` 之后调用：`Weak` 只能从既有的 `Arc`
    /// 降级而来。未注入时插件发来的 `host/*` 请求会收到内部错误。
    pub fn set_host(&self, host: Weak<dyn HostHandler>) {
        *self.host.lock().unwrap_or_else(|e| e.into_inner()) = Some(host);
    }

    /// 取出宿主回调的强引用。宿主已析构或未注入时为 `None`。
    fn host(&self) -> Option<Arc<dyn HostHandler>> {
        self.host
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .and_then(Weak::upgrade)
    }

    /// 当前在途入站请求的最大深度快照，供出站 `host/callTool` 定深度。
    pub fn current_inbound_depth(&self) -> u8 {
        self.lock().max_inbound_depth()
    }

    pub fn plugin_id(&self) -> &str {
        &self.plugin_id
    }

    pub fn state(&self) -> InstanceState {
        self.lock().state
    }

    /// 握手信息，未完成握手时为 `None`。
    pub fn handshake(&self) -> Option<HandshakeInfo> {
        self.lock().handshake.clone()
    }

    /// 最近一次导致 [`InstanceState::Error`] 的原因，供 UI 展示。
    pub fn last_error(&self) -> Option<String> {
        self.lock().last_error.clone()
    }

    /// 在途请求数。
    pub fn inflight(&self) -> u64 {
        self.inflight.load(Ordering::Relaxed)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, InstanceInner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn set_state(&self, next: InstanceState) {
        self.lock().state = next;
    }

    fn fail(&self, reason: String) {
        let mut inner = self.lock();
        inner.state = InstanceState::Error;
        inner.last_error = Some(reason);
    }

    fn request_timeout(&self) -> Duration {
        self.lifecycle
            .request_timeout_sec
            .map(|s| Duration::from_secs(u64::from(s)))
            .unwrap_or(DEFAULT_REQUEST_TIMEOUT)
    }

    fn idle_timeout(&self) -> Duration {
        self.lifecycle
            .idle_timeout_sec
            .map(|s| Duration::from_secs(u64::from(s)))
            .unwrap_or(DEFAULT_IDLE_TIMEOUT)
    }

    /// 只有按需模式才会因空闲被回收；background / startup 常驻。
    fn idle_reclaimable(&self) -> bool {
        matches!(self.lifecycle.mode, LifecycleMode::OnDemand)
    }
}

impl PluginInstance {
    /// 执行完整握手：等 `plugin/hello` → 版本协商 → 发 `plugin/ready` → 等结果。
    ///
    /// 成功后状态变为 [`InstanceState::Idle`] 并启动读循环；
    /// 任何失败都会把状态置为 [`InstanceState::Error`] 并记录原因。
    ///
    /// `config` 与 `plugin_dir` 会原样放进 `plugin/ready` 的 params。
    pub async fn start(
        self: &Arc<Self>,
        config: JsonValue,
        plugin_dir: impl Into<String>,
    ) -> Result<HandshakeInfo, InstanceError> {
        self.set_state(InstanceState::Starting);

        let info = match self.handshake_inner(config, plugin_dir.into()).await {
            Ok(info) => info,
            Err(err) => {
                self.fail(err.to_string());
                return Err(err);
            }
        };

        {
            let mut inner = self.lock();
            inner.handshake = Some(info.clone());
            inner.state = InstanceState::Idle;
            inner.last_error = None;
        }

        self.spawn_reader();
        self.arm_idle_timer();
        Ok(info)
    }

    async fn handshake_inner(
        &self,
        config: JsonValue,
        plugin_dir: String,
    ) -> Result<HandshakeInfo, InstanceError> {
        let hello = time::timeout(HANDSHAKE_TIMEOUT, self.await_hello())
            .await
            .map_err(|_| InstanceError::HandshakeTimeout {
                seconds: HANDSHAKE_TIMEOUT.as_secs(),
            })??;

        let plugin_version = ProtocolVersion::parse(&hello.protocol_version)
            .map_err(|reason| InstanceError::VersionMismatch { reason })?;
        let full_feature =
            negotiate_version(ProtocolVersion::V1_0, plugin_version).map_err(|mismatch| {
                InstanceError::VersionMismatch {
                    reason: mismatch.to_string(),
                }
            })?;

        let params = json!({ "config": config, "plugin_dir": plugin_dir });
        self.send_ready(params).await?;

        Ok(HandshakeInfo {
            protocol_version: plugin_version,
            tools: hello.tools,
            full_feature,
        })
    }

    /// 循环读取入站消息，直到拿到 `plugin/hello`。
    ///
    /// 握手前插件不该发别的东西，但真实实现难免有噪声（如日志类通知），
    /// 这里一律忽略而不是报错，保持宽进严出。
    async fn await_hello(&self) -> Result<HelloParams, InstanceError> {
        loop {
            let msg = self.transport.recv().await?;
            let IncomingMessage::Notification(note) = msg else {
                continue;
            };
            if note.method != METHOD_HELLO {
                continue;
            }
            let params = note.params.unwrap_or(JsonValue::Null);
            return serde_json::from_value::<HelloParams>(params).map_err(|e| {
                InstanceError::Handshake {
                    reason: format!("plugin/hello 参数无法解析：{e}"),
                }
            });
        }
    }

    /// 发出 `plugin/ready` 并等待插件确认。
    ///
    /// 握手阶段读循环尚未启动，所以这里自己读消息来完成配对。
    async fn send_ready(&self, params: JsonValue) -> Result<(), InstanceError> {
        let id = self.pending.next_id();
        let request = JsonRpcRequest::new(id.clone(), METHOD_READY, params);
        self.transport
            .send(OutgoingMessage::Request(request))
            .await?;

        loop {
            let msg = self.transport.recv().await?;
            let IncomingMessage::Response(resp) = msg else {
                continue;
            };
            if resp.id.as_ref() != Some(&id) {
                continue;
            }
            return match resp.body {
                ResponseBody::Success { .. } => Ok(()),
                ResponseBody::Error { error } => Err(InstanceError::from_rpc(error)),
            };
        }
    }

    /// 发起一次请求并等待结果，带超时。
    ///
    /// 超时只让本次调用失败，不改变实例状态——插件可能只是这一个工具慢。
    pub async fn call(
        self: &Arc<Self>,
        method: impl Into<String>,
        params: JsonValue,
    ) -> Result<JsonValue, InstanceError> {
        self.call_at_depth(method, params, 0).await
    }

    /// 与 [`call`](Self::call) 相同，但额外声明**发起方所处的调用深度**。
    ///
    /// 这个深度是插件在处理本次请求期间发起 `host/callTool` 时的深度依据。
    /// 深度必须由宿主传入：若改成让插件在 params 里自报，插件永远填 0
    /// 就能绕开 `MAX_CALL_DEPTH`，深度限制形同虚设。
    pub async fn call_at_depth(
        self: &Arc<Self>,
        method: impl Into<String>,
        params: JsonValue,
        inbound_depth: u8,
    ) -> Result<JsonValue, InstanceError> {
        let method = method.into();
        let state = self.state();
        if !state.accepts_requests() {
            return Err(InstanceError::NotReady {
                state: format!("{state:?}"),
            });
        }

        let id = self.pending.next_id();
        let rx = self.pending.register(id.clone())?;

        self.enter_busy();
        self.lock().enter_inbound(inbound_depth);
        let result = self.call_inner(&method, params, id.clone(), rx).await;
        self.lock().leave_inbound(inbound_depth);
        self.leave_busy();

        result
    }

    async fn call_inner(
        &self,
        method: &str,
        params: JsonValue,
        id: RequestId,
        rx: tokio::sync::oneshot::Receiver<super::transport::PendingOutcome>,
    ) -> Result<JsonValue, InstanceError> {
        let request = JsonRpcRequest::new(id.clone(), method, params);
        if let Err(err) = self.transport.send(OutgoingMessage::Request(request)).await {
            self.pending.cancel(&id);
            return Err(err.into());
        }

        let timeout = self.request_timeout();
        match time::timeout(timeout, rx).await {
            Ok(Ok(Ok(value))) => Ok(value),
            Ok(Ok(Err(rpc))) => Err(InstanceError::from_rpc(rpc)),
            // 发送端被丢弃：通常是 fail_all 之后表已封闭，按传输关闭处理。
            Ok(Err(_)) => Err(InstanceError::Transport {
                source: TransportError::Closed,
            }),
            Err(_) => {
                self.pending.cancel(&id);
                Err(InstanceError::RequestTimeout {
                    method: method.to_string(),
                    seconds: timeout.as_secs(),
                })
            }
        }
    }

    /// 发送一条通知，不等待回应。
    pub async fn notify(
        &self,
        method: impl Into<String>,
        params: JsonValue,
    ) -> Result<(), InstanceError> {
        let note = JsonRpcNotification::new(method, params);
        self.transport
            .send(OutgoingMessage::Notification(note))
            .await?;
        Ok(())
    }

    fn enter_busy(&self) {
        self.inflight.fetch_add(1, Ordering::SeqCst);
        self.idle_epoch.fetch_add(1, Ordering::SeqCst);
        let mut inner = self.lock();
        if inner.state == InstanceState::Idle {
            inner.state = InstanceState::Busy;
        }
    }

    fn leave_busy(self: &Arc<Self>) {
        let remaining = self.inflight.fetch_sub(1, Ordering::SeqCst) - 1;
        if remaining > 0 {
            return;
        }
        let became_idle = {
            let mut inner = self.lock();
            if inner.state == InstanceState::Busy {
                inner.state = InstanceState::Idle;
                true
            } else {
                false
            }
        };
        if became_idle {
            self.arm_idle_timer();
        }
    }
}

impl PluginInstance {
    /// 启动后台读循环：把响应派发给 [`PendingTable`]，直到通道关闭。
    fn spawn_reader(self: &Arc<Self>) {
        let this = Arc::clone(self);
        let handle = tokio::spawn(async move {
            this.read_loop().await;
        });
        *self.reader.lock().unwrap_or_else(|e| e.into_inner()) = Some(handle);
    }

    async fn read_loop(self: Arc<Self>) {
        loop {
            match self.transport.recv().await {
                Ok(IncomingMessage::Response(resp)) => {
                    self.pending.complete(resp);
                }
                Ok(IncomingMessage::Request(req)) => {
                    // 必须 spawn 而不能在此 await：处理 host/callTool 可能回过头
                    // 来调用本插件，那条调用的响应要靠**本读循环**收取。若同步
                    // 等待，读循环卡在等响应上，响应又永远读不到，必然死锁。
                    let this = Arc::clone(&self);
                    tokio::spawn(async move {
                        this.dispatch_request(req).await;
                    });
                }
                Ok(IncomingMessage::Notification(note)) => {
                    let this = Arc::clone(&self);
                    tokio::spawn(async move {
                        this.dispatch_notification(note).await;
                    });
                }
                Err(_) => break,
            }
        }
        self.on_disconnect();
    }

    /// 处理插件发来的反向 RPC 请求，并把结果写回通道。
    async fn dispatch_request(self: Arc<Self>, req: JsonRpcRequest) {
        let id = req.id.clone();
        let outcome = self.handle_host_method(&req).await;
        let response = match outcome {
            Ok(value) => JsonRpcResponse::success(id, value),
            Err(err) => JsonRpcResponse::error(Some(id), err),
        };
        // 发送失败说明通道已断，读循环随后自会走 on_disconnect，这里不必处理。
        let _ = self
            .transport
            .send(OutgoingMessage::Response(response))
            .await;
    }

    async fn handle_host_method(&self, req: &JsonRpcRequest) -> Result<JsonValue, JsonRpcError> {
        if !req.method.starts_with(HOST_METHOD_PREFIX) {
            return Err(JsonRpcError::new(
                JsonRpcError::CODE_METHOD_NOT_FOUND,
                format!("宿主不接受非 host/ 前缀的请求：{}", req.method),
            ));
        }

        let host = self.host().ok_or_else(|| {
            JsonRpcError::new(
                JsonRpcError::CODE_INTERNAL_ERROR,
                "宿主回调不可用，无法处理反向 RPC",
            )
        })?;
        let params = req.params.clone().unwrap_or(JsonValue::Null);

        match req.method.as_str() {
            HOST_LIST_TOOLS => host.host_list_tools().await,
            HOST_CALL_TOOL => {
                let tool = params
                    .get("name")
                    .and_then(JsonValue::as_str)
                    .ok_or_else(|| {
                        JsonRpcError::new(
                            JsonRpcError::CODE_INVALID_PARAMS,
                            "host/callTool 缺少字符串字段 name",
                        )
                    })?;
                let args = params
                    .get("arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                // 深度取在途入站请求的最大值，绝不采信插件自报。
                let depth = self.current_inbound_depth();
                host.host_call_tool(&self.plugin_id, depth, tool, args)
                    .await
            }
            HOST_GET_CONFIG => host.host_get_config(&self.plugin_id).await,
            HOST_SET_CONFIG => host.host_set_config(&self.plugin_id, params).await,
            HOST_NOTIFY => {
                let method = params
                    .get("method")
                    .and_then(JsonValue::as_str)
                    .ok_or_else(|| {
                        JsonRpcError::new(
                            JsonRpcError::CODE_INVALID_PARAMS,
                            "host/notify 缺少字符串字段 method",
                        )
                    })?;
                let payload = params.get("params").cloned().unwrap_or_else(|| json!({}));
                host.host_notify(&self.plugin_id, method, payload).await;
                Ok(json!({}))
            }
            HOST_UI_REQUEST => {
                let ui_type = params
                    .get("ui_type")
                    .and_then(JsonValue::as_str)
                    .ok_or_else(|| {
                        JsonRpcError::new(
                            JsonRpcError::CODE_INVALID_PARAMS,
                            "host/uiRequest 缺少字符串字段 ui_type",
                        )
                    })?;
                let schema = params
                    .get("schema")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                let callback_method = params
                    .get("callback_method")
                    .and_then(JsonValue::as_str)
                    .ok_or_else(|| {
                        JsonRpcError::new(
                            JsonRpcError::CODE_INVALID_PARAMS,
                            "host/uiRequest 缺少字符串字段 callback_method",
                        )
                    })?;
                host.host_ui_request(&self.plugin_id, ui_type, schema, callback_method)
                    .await
            }
            other => Err(JsonRpcError::new(
                JsonRpcError::CODE_METHOD_NOT_FOUND,
                format!("未知的宿主方法：{other}"),
            )),
        }
    }

    /// 处理插件发来的通知。`notify/*` 转交宿主推给 UI，其余落日志即可。
    async fn dispatch_notification(self: Arc<Self>, note: JsonRpcNotification) {
        if !note.method.starts_with(NOTIFY_PREFIX) {
            tracing::debug!(
                plugin = %self.plugin_id,
                method = %note.method,
                "忽略插件发来的未知通知"
            );
            return;
        }
        let Some(host) = self.host() else {
            tracing::warn!(
                plugin = %self.plugin_id,
                method = %note.method,
                "宿主回调不可用，通知被丢弃"
            );
            return;
        };
        let params = note.params.clone().unwrap_or_else(|| json!({}));
        host.host_notify(&self.plugin_id, &note.method, params)
            .await;
    }

    /// 传输断开：让所有待响应请求一起失败，并把状态推进到终态。
    fn on_disconnect(&self) {
        let error = JsonRpcError::new(
            JsonRpcError::CODE_INTERNAL_ERROR,
            format!("插件 {} 连接已断开", self.plugin_id),
        );
        self.pending.fail_all(error);

        let mut inner = self.lock();
        match inner.state {
            // 正常停止流程走到这里，属于预期收尾。
            InstanceState::Stopping | InstanceState::Stopped => {
                inner.state = InstanceState::Stopped;
            }
            InstanceState::Error => {}
            _ => {
                inner.state = InstanceState::Error;
                inner.last_error = Some("插件连接意外断开".to_string());
            }
        }
    }

    /// 装载一轮空闲计时。
    ///
    /// 用「代次」而非取消句柄：每次活动让 `idle_epoch` 自增，旧计时任务醒来
    /// 时发现代次已变就自行退出。这样不必持有 `JoinHandle`，也不存在
    /// 取消与超时的竞态。
    fn arm_idle_timer(self: &Arc<Self>) {
        if !self.idle_reclaimable() {
            return;
        }
        let epoch = self.idle_epoch.fetch_add(1, Ordering::SeqCst) + 1;
        let timeout = self.idle_timeout();
        let this = Arc::clone(self);
        tokio::spawn(async move {
            time::sleep(timeout).await;
            if this.idle_epoch.load(Ordering::SeqCst) != epoch {
                return;
            }
            if this.state() != InstanceState::Idle {
                return;
            }
            this.stop().await;
        });
    }

    /// 主动停止实例：置 Stopping、关闭传输、让待响应请求失败，最终 Stopped。
    ///
    /// 允许重复调用；已处于终态时直接返回。
    pub async fn stop(&self) {
        {
            let mut inner = self.lock();
            if inner.state == InstanceState::Stopped || inner.state == InstanceState::Stopping {
                return;
            }
            inner.state = InstanceState::Stopping;
        }
        self.idle_epoch.fetch_add(1, Ordering::SeqCst);

        self.transport.close().await;
        self.pending.fail_all(JsonRpcError::new(
            JsonRpcError::CODE_INTERNAL_ERROR,
            format!("插件 {} 正在停止", self.plugin_id),
        ));

        self.set_state(InstanceState::Stopped);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::message::JsonRpcResponse;
    use crate::runtime::transport::MockTransport;

    /// 构造一个带 mock 传输的实例，同时返回 mock 句柄供测试注入消息。
    fn instance_with(lifecycle: Lifecycle) -> (Arc<PluginInstance>, Arc<MockTransport>) {
        let mock = Arc::new(MockTransport::new());
        let transport: Arc<dyn Transport> = Arc::clone(&mock) as Arc<dyn Transport>;
        let instance = Arc::new(PluginInstance::new(
            "com.example.demo",
            lifecycle,
            transport,
        ));
        (instance, mock)
    }

    fn hello(version: &str, tools: JsonValue) -> IncomingMessage {
        IncomingMessage::Notification(JsonRpcNotification::new(
            METHOD_HELLO,
            json!({ "protocol_version": version, "tools": tools }),
        ))
    }

    /// 预置握手所需的两条消息：`plugin/hello` 与 `plugin/ready` 的成功响应。
    ///
    /// `plugin/ready` 用的是 `PendingTable` 分配的第一个 ID，即 `Int(1)`。
    fn preload_handshake(mock: &MockTransport, version: &str, tools: JsonValue) {
        mock.push_incoming(hello(version, tools));
        mock.push_incoming(IncomingMessage::Response(JsonRpcResponse::success(
            1_i64,
            json!({}),
        )));
    }

    /// 完成握手并把 `plugin/ready` 请求从出站队列中排掉，
    /// 让后续断言能直接拿到业务请求。
    async fn started(lifecycle: Lifecycle) -> (Arc<PluginInstance>, Arc<MockTransport>) {
        let (instance, mock) = instance_with(lifecycle);
        preload_handshake(&mock, "1.0", json!([]));
        instance.start(json!({}), "/plugins/demo").await.unwrap();
        mock.next_sent().await.expect("plugin/ready 应已发出");
        (instance, mock)
    }

    /// 等待宿主发出的下一条请求，返回其 ID。
    async fn next_request_id(mock: &MockTransport) -> RequestId {
        match mock.next_sent().await.expect("应有请求发出") {
            OutgoingMessage::Request(req) => req.id,
            other => panic!("期望请求，实际为 {other:?}"),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn 握手成功后进入空闲并记录工具清单() {
        let (instance, mock) = instance_with(Lifecycle::default());
        preload_handshake(
            &mock,
            "1.0",
            json!([{ "name": "demo:echo", "description": "回声" }]),
        );

        let info = instance
            .start(json!({"k": 1}), "/plugins/demo")
            .await
            .unwrap();

        assert_eq!(info.protocol_version, ProtocolVersion::V1_0);
        assert_eq!(info.tools.len(), 1);
        assert_eq!(info.tools[0].name, "demo:echo");
        assert!(info.full_feature);
        assert_eq!(instance.state(), InstanceState::Idle);
        assert_eq!(instance.handshake().unwrap().tools[0].description, "回声");
    }

    #[tokio::test(start_paused = true)]
    async fn 握手时宿主发出携带配置的_plugin_ready() {
        let (instance, mock) = instance_with(Lifecycle::default());
        preload_handshake(&mock, "1.0", json!([]));

        instance
            .start(json!({"token": "abc"}), "/plugins/demo")
            .await
            .unwrap();

        let sent = mock.next_sent().await.unwrap();
        let OutgoingMessage::Request(req) = sent else {
            panic!("plugin/ready 应为请求");
        };
        assert_eq!(req.method, METHOD_READY);
        let params = req.params.unwrap();
        assert_eq!(params["config"]["token"], json!("abc"));
        assert_eq!(params["plugin_dir"], json!("/plugins/demo"));
    }

    #[tokio::test(start_paused = true)]
    async fn 十秒内收不到_hello_判定握手超时() {
        let (instance, _mock) = instance_with(Lifecycle::default());

        let err = instance
            .start(json!({}), "/plugins/demo")
            .await
            .unwrap_err();

        assert_eq!(err, InstanceError::HandshakeTimeout { seconds: 10 });
        assert_eq!(instance.state(), InstanceState::Error);
        assert!(instance.last_error().unwrap().contains("握手超时"));
    }

    #[tokio::test(start_paused = true)]
    async fn 握手前的无关通知不影响成功() {
        let (instance, mock) = instance_with(Lifecycle::default());
        mock.push_incoming(IncomingMessage::Notification(JsonRpcNotification::new(
            "notify/progress",
            json!({"phase": "boot"}),
        )));
        preload_handshake(&mock, "1.0", json!([]));

        instance.start(json!({}), "/plugins/demo").await.unwrap();

        assert_eq!(instance.state(), InstanceState::Idle);
    }

    #[tokio::test(start_paused = true)]
    async fn 主版本不一致拒绝加载() {
        let (instance, mock) = instance_with(Lifecycle::default());
        mock.push_incoming(hello("2.0", json!([])));

        let err = instance
            .start(json!({}), "/plugins/demo")
            .await
            .unwrap_err();

        assert!(matches!(err, InstanceError::VersionMismatch { .. }));
        assert_eq!(instance.state(), InstanceState::Error);
    }

    #[tokio::test(start_paused = true)]
    async fn 无法解析的_hello_参数判定握手失败() {
        let (instance, mock) = instance_with(Lifecycle::default());
        mock.push_incoming(IncomingMessage::Notification(JsonRpcNotification::new(
            METHOD_HELLO,
            json!({"tools": []}),
        )));

        let err = instance
            .start(json!({}), "/plugins/demo")
            .await
            .unwrap_err();

        assert!(matches!(err, InstanceError::Handshake { .. }));
        assert_eq!(instance.state(), InstanceState::Error);
    }

    #[tokio::test(start_paused = true)]
    async fn 未握手时拒绝请求() {
        let (instance, _mock) = instance_with(Lifecycle::default());

        let err = instance.call("demo:echo", json!({})).await.unwrap_err();

        assert_eq!(
            err,
            InstanceError::NotReady {
                state: "Stopped".to_string()
            }
        );
    }

    #[tokio::test(start_paused = true)]
    async fn 请求正常往返并回到空闲() {
        let (instance, mock) = started(Lifecycle::default()).await;

        let caller = tokio::spawn({
            let instance = Arc::clone(&instance);
            async move { instance.call("demo:echo", json!({"text": "hi"})).await }
        });

        let id = next_request_id(&mock).await;
        assert_eq!(instance.state(), InstanceState::Busy);
        mock.push_incoming(IncomingMessage::Response(JsonRpcResponse::success(
            id,
            json!({"text": "hi"}),
        )));

        let value = caller.await.unwrap().unwrap();
        assert_eq!(value, json!({"text": "hi"}));
        assert_eq!(instance.state(), InstanceState::Idle);
        assert_eq!(instance.inflight(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn 插件错误响应转为实例错误() {
        let (instance, mock) = started(Lifecycle::default()).await;

        let caller = tokio::spawn({
            let instance = Arc::clone(&instance);
            async move { instance.call("demo:boom", json!({})).await }
        });

        let id = next_request_id(&mock).await;
        mock.push_incoming(IncomingMessage::Response(JsonRpcResponse::error(
            Some(id),
            JsonRpcError::new(JsonRpcError::CODE_PLUGIN_ERROR, "内部炸了"),
        )));

        let err = caller.await.unwrap().unwrap_err();
        assert_eq!(
            err,
            InstanceError::Plugin {
                code: JsonRpcError::CODE_PLUGIN_ERROR,
                message: "内部炸了".to_string(),
            }
        );
        // 插件报错属业务失败，实例本身仍然健康。
        assert_eq!(instance.state(), InstanceState::Idle);
    }

    #[tokio::test(start_paused = true)]
    async fn 请求超时后实例仍然可用() {
        let lifecycle = Lifecycle {
            request_timeout_sec: Some(30),
            ..Default::default()
        };
        let (instance, mock) = started(lifecycle).await;

        let caller = tokio::spawn({
            let instance = Arc::clone(&instance);
            async move { instance.call("demo:slow", json!({})).await }
        });
        let _id = next_request_id(&mock).await;

        let err = caller.await.unwrap().unwrap_err();
        assert_eq!(
            err,
            InstanceError::RequestTimeout {
                method: "demo:slow".to_string(),
                seconds: 30,
            }
        );
        // 超时只让这一次调用失败，状态回到空闲，槽位也已回收。
        assert_eq!(instance.state(), InstanceState::Idle);
        assert_eq!(instance.inflight(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn 自定义请求超时优先于缺省值() {
        let lifecycle = Lifecycle {
            request_timeout_sec: Some(3),
            ..Default::default()
        };
        let (instance, mock) = started(lifecycle).await;

        let caller = tokio::spawn({
            let instance = Arc::clone(&instance);
            async move { instance.call("demo:slow", json!({})).await }
        });
        let _id = next_request_id(&mock).await;

        let err = caller.await.unwrap().unwrap_err();
        assert_eq!(
            err,
            InstanceError::RequestTimeout {
                method: "demo:slow".to_string(),
                seconds: 3,
            }
        );
    }

    #[tokio::test(start_paused = true)]
    async fn 传输中断导致待响应请求全部失败() {
        let (instance, mock) = started(Lifecycle::default()).await;

        let first = tokio::spawn({
            let instance = Arc::clone(&instance);
            async move { instance.call("demo:a", json!({})).await }
        });
        let _ = next_request_id(&mock).await;
        let second = tokio::spawn({
            let instance = Arc::clone(&instance);
            async move { instance.call("demo:b", json!({})).await }
        });
        let _ = next_request_id(&mock).await;

        mock.disconnect();

        let first = first.await.unwrap().unwrap_err();
        let second = second.await.unwrap().unwrap_err();
        assert!(matches!(first, InstanceError::Plugin { .. }));
        assert!(matches!(second, InstanceError::Plugin { .. }));
        assert_eq!(instance.state(), InstanceState::Error);
        assert!(instance.last_error().unwrap().contains("意外断开"));
    }

    #[tokio::test(start_paused = true)]
    async fn 按需模式空闲超时自动停止() {
        let lifecycle = Lifecycle {
            mode: LifecycleMode::OnDemand,
            idle_timeout_sec: Some(5),
            ..Default::default()
        };
        let (instance, _mock) = started(lifecycle).await;
        assert_eq!(instance.state(), InstanceState::Idle);

        time::sleep(Duration::from_secs(6)).await;

        assert_eq!(instance.state(), InstanceState::Stopped);
    }

    #[tokio::test(start_paused = true)]
    async fn 后台模式不因空闲而停止() {
        let lifecycle = Lifecycle {
            mode: LifecycleMode::Background,
            idle_timeout_sec: Some(5),
            ..Default::default()
        };
        let (instance, _mock) = started(lifecycle).await;

        time::sleep(Duration::from_secs(600)).await;

        assert_eq!(instance.state(), InstanceState::Idle);
    }

    #[tokio::test(start_paused = true)]
    async fn 期间有活动则空闲计时重新开始() {
        let lifecycle = Lifecycle {
            mode: LifecycleMode::OnDemand,
            idle_timeout_sec: Some(10),
            ..Default::default()
        };
        let (instance, mock) = started(lifecycle).await;

        time::sleep(Duration::from_secs(8)).await;

        let caller = tokio::spawn({
            let instance = Arc::clone(&instance);
            async move { instance.call("demo:echo", json!({})).await }
        });
        let id = next_request_id(&mock).await;
        mock.push_incoming(IncomingMessage::Response(JsonRpcResponse::success(
            id,
            json!({}),
        )));
        caller.await.unwrap().unwrap();

        // 若旧计时未作废，此刻早已超过最初的 10 秒。
        time::sleep(Duration::from_secs(5)).await;
        assert_eq!(instance.state(), InstanceState::Idle);

        time::sleep(Duration::from_secs(6)).await;
        assert_eq!(instance.state(), InstanceState::Stopped);
    }

    #[tokio::test(start_paused = true)]
    async fn 主动停止后不再接受请求() {
        let (instance, _mock) = started(Lifecycle::default()).await;

        instance.stop().await;

        assert_eq!(instance.state(), InstanceState::Stopped);
        let err = instance.call("demo:echo", json!({})).await.unwrap_err();
        assert_eq!(
            err,
            InstanceError::NotReady {
                state: "Stopped".to_string()
            }
        );
    }

    #[tokio::test(start_paused = true)]
    async fn 重复停止是安全的() {
        let (instance, _mock) = started(Lifecycle::default()).await;

        instance.stop().await;
        instance.stop().await;

        assert_eq!(instance.state(), InstanceState::Stopped);
    }

    #[test]
    fn 状态谓词与设计文档一致() {
        assert!(InstanceState::Idle.accepts_requests());
        assert!(InstanceState::Busy.accepts_requests());
        assert!(!InstanceState::Starting.accepts_requests());
        assert!(!InstanceState::Stopping.accepts_requests());
        assert!(InstanceState::Stopped.is_terminal());
        assert!(InstanceState::Error.is_terminal());
        assert!(!InstanceState::Idle.is_terminal());
    }

    // ===== 反向 RPC（设计文档 §5.3 / §5.4）=====

    /// 宿主收到的一次回调，字段全展开以便测试逐项断言。
    #[derive(Debug, Clone, PartialEq)]
    enum HostCall {
        ListTools,
        CallTool {
            caller: String,
            depth: u8,
            tool: String,
            args: JsonValue,
        },
        GetConfig {
            caller: String,
        },
        SetConfig {
            caller: String,
            value: JsonValue,
        },
        Notify {
            caller: String,
            method: String,
            params: JsonValue,
        },
        UiRequest {
            caller: String,
            ui_type: String,
            schema: JsonValue,
            callback_method: String,
        },
    }

    /// 只记账、不做事的假宿主。`HostHandler` 抽成 trait 就是为了让实例侧
    /// 的反向 RPC 能脱离 `Supervisor` 单独验证。
    #[derive(Default)]
    struct RecordingHost {
        calls: Mutex<Vec<HostCall>>,
        fail: bool,
    }

    impl RecordingHost {
        fn new() -> Arc<Self> {
            Arc::new(Self::default())
        }

        /// 让 `host_list_tools` 返回错误，用于验证错误原样写回。
        fn failing() -> Arc<Self> {
            Arc::new(Self {
                calls: Mutex::new(Vec::new()),
                fail: true,
            })
        }

        fn calls(&self) -> Vec<HostCall> {
            self.calls.lock().unwrap_or_else(|e| e.into_inner()).clone()
        }

        fn record(&self, call: HostCall) {
            self.calls
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(call);
        }
    }

    #[async_trait::async_trait]
    impl HostHandler for RecordingHost {
        async fn host_list_tools(&self) -> Result<JsonValue, JsonRpcError> {
            self.record(HostCall::ListTools);
            if self.fail {
                return Err(JsonRpcError::new(
                    JsonRpcError::CODE_TOOL_NOT_FOUND,
                    "宿主拒绝列举",
                ));
            }
            Ok(json!([{ "name": "demo:echo" }]))
        }

        async fn host_call_tool(
            &self,
            caller_plugin: &str,
            depth: u8,
            tool: &str,
            args: JsonValue,
        ) -> Result<JsonValue, JsonRpcError> {
            self.record(HostCall::CallTool {
                caller: caller_plugin.to_string(),
                depth,
                tool: tool.to_string(),
                args,
            });
            Ok(json!({ "called": tool }))
        }

        async fn host_get_config(&self, caller_plugin: &str) -> Result<JsonValue, JsonRpcError> {
            self.record(HostCall::GetConfig {
                caller: caller_plugin.to_string(),
            });
            Ok(json!({ "theme": "dark" }))
        }

        async fn host_set_config(
            &self,
            caller_plugin: &str,
            value: JsonValue,
        ) -> Result<JsonValue, JsonRpcError> {
            self.record(HostCall::SetConfig {
                caller: caller_plugin.to_string(),
                value,
            });
            Ok(json!({}))
        }

        async fn host_notify(&self, caller_plugin: &str, method: &str, params: JsonValue) {
            self.record(HostCall::Notify {
                caller: caller_plugin.to_string(),
                method: method.to_string(),
                params,
            });
        }

        async fn host_ui_request(
            &self,
            caller_plugin: &str,
            ui_type: &str,
            schema: JsonValue,
            callback_method: &str,
        ) -> Result<JsonValue, JsonRpcError> {
            self.record(HostCall::UiRequest {
                caller: caller_plugin.to_string(),
                ui_type: ui_type.to_string(),
                schema,
                callback_method: callback_method.to_string(),
            });
            Ok(json!({ "status": "requested" }))
        }
    }

    /// 握手完成并注入假宿主。
    ///
    /// 返回的 `Arc<RecordingHost>` 必须由测试持有到最后：实例侧存的是 `Weak`，
    /// 强引用一掉，反向调用立刻退化成「宿主不可用」。
    async fn started_with_host(
        lifecycle: Lifecycle,
    ) -> (Arc<PluginInstance>, Arc<MockTransport>, Arc<RecordingHost>) {
        let (instance, mock) = started(lifecycle).await;
        let host = RecordingHost::new();
        let weak = Arc::downgrade(&(Arc::clone(&host) as Arc<dyn HostHandler>));
        instance.set_host(weak);
        (instance, mock, host)
    }

    /// 模拟插件发起一次反向请求，取回宿主写到通道上的响应体。
    async fn reverse_call(mock: &MockTransport, method: &str, params: JsonValue) -> ResponseBody {
        mock.push_incoming(IncomingMessage::Request(JsonRpcRequest::new(
            77_i64, method, params,
        )));
        match mock.next_sent().await.expect("宿主应写回响应") {
            OutgoingMessage::Response(resp) => {
                assert_eq!(
                    resp.id,
                    Some(RequestId::Int(77)),
                    "响应必须回填插件发来的请求 ID"
                );
                resp.body
            }
            other => panic!("期望响应，实际为 {other:?}"),
        }
    }

    fn expect_success(body: ResponseBody) -> JsonValue {
        match body {
            ResponseBody::Success { result } => result,
            ResponseBody::Error { error } => panic!("期望成功，实际为错误：{error:?}"),
        }
    }

    fn expect_error(body: ResponseBody) -> JsonRpcError {
        match body {
            ResponseBody::Error { error } => error,
            ResponseBody::Success { result } => panic!("期望错误，实际为成功：{result}"),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn 反向列举工具被转交宿主并写回结果() {
        let (_instance, mock, host) = started_with_host(Lifecycle::default()).await;

        let result = expect_success(reverse_call(&mock, HOST_LIST_TOOLS, json!({})).await);

        assert_eq!(result, json!([{ "name": "demo:echo" }]));
        assert_eq!(host.calls(), vec![HostCall::ListTools]);
    }

    #[tokio::test(start_paused = true)]
    async fn 反向调用工具提取工具名与参数() {
        let (_instance, mock, host) = started_with_host(Lifecycle::default()).await;

        let result = expect_success(
            reverse_call(
                &mock,
                HOST_CALL_TOOL,
                json!({ "name": "demo:echo", "arguments": { "text": "hi" } }),
            )
            .await,
        );

        assert_eq!(result, json!({ "called": "demo:echo" }));
        assert_eq!(
            host.calls(),
            vec![HostCall::CallTool {
                caller: "com.example.demo".to_string(),
                depth: 0,
                tool: "demo:echo".to_string(),
                args: json!({ "text": "hi" }),
            }]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn 反向调用工具省略参数时按空对象处理() {
        let (_instance, mock, host) = started_with_host(Lifecycle::default()).await;

        expect_success(reverse_call(&mock, HOST_CALL_TOOL, json!({ "name": "demo:echo" })).await);

        assert_eq!(
            host.calls(),
            vec![HostCall::CallTool {
                caller: "com.example.demo".to_string(),
                depth: 0,
                tool: "demo:echo".to_string(),
                args: json!({}),
            }]
        );
    }

    /// 深度限制的命门：插件可以在 params 里随便写 depth，宿主一概不看，
    /// 只认自己记的在途入站深度。否则插件永远填 0 即可无限递归。
    #[tokio::test(start_paused = true)]
    async fn 反向调用工具的深度取自在途入站请求而非插件自报() {
        let (instance, mock, host) = started_with_host(Lifecycle::default()).await;

        // 宿主以深度 3 调用本插件，插件在处理期间回调 host/callTool。
        let caller = tokio::spawn({
            let instance = Arc::clone(&instance);
            async move { instance.call_at_depth("demo:outer", json!({}), 3).await }
        });
        let id = next_request_id(&mock).await;

        expect_success(
            reverse_call(
                &mock,
                HOST_CALL_TOOL,
                json!({ "name": "demo:inner", "arguments": {}, "depth": 0 }),
            )
            .await,
        );

        mock.push_incoming(IncomingMessage::Response(JsonRpcResponse::success(
            id,
            json!({}),
        )));
        caller.await.unwrap().unwrap();

        assert_eq!(
            host.calls(),
            vec![HostCall::CallTool {
                caller: "com.example.demo".to_string(),
                depth: 3,
                tool: "demo:inner".to_string(),
                args: json!({}),
            }]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn 反向调用工具缺少工具名返回参数错误() {
        let (_instance, mock, host) = started_with_host(Lifecycle::default()).await;

        let err = expect_error(reverse_call(&mock, HOST_CALL_TOOL, json!({ "name": 42 })).await);

        assert_eq!(err.code, JsonRpcError::CODE_INVALID_PARAMS);
        assert_eq!(err.message, "host/callTool 缺少字符串字段 name");
        assert!(host.calls().is_empty(), "参数不合法时不应惊动宿主");
    }

    #[tokio::test(start_paused = true)]
    async fn 反向获取配置透传插件身份() {
        let (_instance, mock, host) = started_with_host(Lifecycle::default()).await;

        let result = expect_success(reverse_call(&mock, HOST_GET_CONFIG, json!({})).await);

        assert_eq!(result, json!({ "theme": "dark" }));
        assert_eq!(
            host.calls(),
            vec![HostCall::GetConfig {
                caller: "com.example.demo".to_string(),
            }]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn 反向设置配置把整段参数交给宿主() {
        let (_instance, mock, host) = started_with_host(Lifecycle::default()).await;

        expect_success(reverse_call(&mock, HOST_SET_CONFIG, json!({ "theme": "light" })).await);

        assert_eq!(
            host.calls(),
            vec![HostCall::SetConfig {
                caller: "com.example.demo".to_string(),
                value: json!({ "theme": "light" }),
            }]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn 反向notify提取方法名与载荷() {
        let (_instance, mock, host) = started_with_host(Lifecycle::default()).await;

        let result = expect_success(
            reverse_call(
                &mock,
                HOST_NOTIFY,
                json!({ "method": "notify/progress", "params": { "pct": 42 } }),
            )
            .await,
        );

        assert_eq!(result, json!({}));
        assert_eq!(
            host.calls(),
            vec![HostCall::Notify {
                caller: "com.example.demo".to_string(),
                method: "notify/progress".to_string(),
                params: json!({ "pct": 42 }),
            }]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn 反向notify缺少方法名返回参数错误() {
        let (_instance, mock, host) = started_with_host(Lifecycle::default()).await;

        let err = expect_error(reverse_call(&mock, HOST_NOTIFY, json!({ "params": {} })).await);

        assert_eq!(err.code, JsonRpcError::CODE_INVALID_PARAMS);
        assert_eq!(err.message, "host/notify 缺少字符串字段 method");
        assert!(host.calls().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn 未知的host方法返回方法不存在() {
        let (_instance, mock, host) = started_with_host(Lifecycle::default()).await;

        let err = expect_error(reverse_call(&mock, "host/rmrf", json!({})).await);

        assert_eq!(err.code, JsonRpcError::CODE_METHOD_NOT_FOUND);
        assert_eq!(err.message, "未知的宿主方法：host/rmrf");
        assert!(host.calls().is_empty());
    }

    /// 插件只能走 `host/` 这一个入口，不得反过来调 `tools/call` 之类的宿主内部方法。
    #[tokio::test(start_paused = true)]
    async fn 非host前缀的反向请求被拒绝() {
        let (_instance, mock, host) = started_with_host(Lifecycle::default()).await;

        let err = expect_error(reverse_call(&mock, "tools/call", json!({})).await);

        assert_eq!(err.code, JsonRpcError::CODE_METHOD_NOT_FOUND);
        assert_eq!(err.message, "宿主不接受非 host/ 前缀的请求：tools/call");
        assert!(host.calls().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn 未注入宿主时反向请求返回内部错误() {
        let (_instance, mock) = started(Lifecycle::default()).await;

        let err = expect_error(reverse_call(&mock, HOST_LIST_TOOLS, json!({})).await);

        assert_eq!(err.code, JsonRpcError::CODE_INTERNAL_ERROR);
        assert_eq!(err.message, "宿主回调不可用，无法处理反向 RPC");
    }

    /// `Weak` 是有意为之：宿主一旦析构，实例不得靠悬垂引用继续跑。
    #[tokio::test(start_paused = true)]
    async fn 宿主析构后反向请求返回内部错误() {
        let (instance, mock) = started(Lifecycle::default()).await;
        let host = RecordingHost::new();
        let weak = Arc::downgrade(&(Arc::clone(&host) as Arc<dyn HostHandler>));
        instance.set_host(weak);
        drop(host);

        let err = expect_error(reverse_call(&mock, HOST_LIST_TOOLS, json!({})).await);

        assert_eq!(err.code, JsonRpcError::CODE_INTERNAL_ERROR);
    }

    #[tokio::test(start_paused = true)]
    async fn 宿主返回的错误原样写回插件() {
        let (instance, mock) = started(Lifecycle::default()).await;
        let host = RecordingHost::failing();
        let weak = Arc::downgrade(&(Arc::clone(&host) as Arc<dyn HostHandler>));
        instance.set_host(weak);

        let err = expect_error(reverse_call(&mock, HOST_LIST_TOOLS, json!({})).await);

        assert_eq!(err.code, JsonRpcError::CODE_TOOL_NOT_FOUND);
        assert_eq!(err.message, "宿主拒绝列举");
    }

    #[tokio::test(start_paused = true)]
    async fn notify前缀的通知被转交宿主() {
        let (_instance, mock, host) = started_with_host(Lifecycle::default()).await;

        mock.push_incoming(IncomingMessage::Notification(JsonRpcNotification::new(
            "notify/toast",
            json!({ "text": "完成" }),
        )));
        // 通知无响应可等，借一次往返把派发任务推进完。
        expect_success(reverse_call(&mock, HOST_LIST_TOOLS, json!({})).await);

        assert_eq!(
            host.calls(),
            vec![
                HostCall::Notify {
                    caller: "com.example.demo".to_string(),
                    method: "notify/toast".to_string(),
                    params: json!({ "text": "完成" }),
                },
                HostCall::ListTools,
            ]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn 非notify前缀的通知被忽略() {
        let (_instance, mock, host) = started_with_host(Lifecycle::default()).await;

        mock.push_incoming(IncomingMessage::Notification(JsonRpcNotification::new(
            "plugin/whatever",
            json!({}),
        )));
        expect_success(reverse_call(&mock, HOST_LIST_TOOLS, json!({})).await);

        assert_eq!(
            host.calls(),
            vec![HostCall::ListTools],
            "未知前缀的通知不应转交宿主"
        );
    }

    /// 会回过头调用本实例的假宿主，用来钉死「读循环必须 spawn 派发」这条约束。
    struct ReentrantHost {
        instance: Mutex<Weak<PluginInstance>>,
    }

    impl ReentrantHost {
        fn bind(instance: &Arc<PluginInstance>) -> Arc<Self> {
            Arc::new(Self {
                instance: Mutex::new(Arc::downgrade(instance)),
            })
        }
    }

    #[async_trait::async_trait]
    impl HostHandler for ReentrantHost {
        async fn host_list_tools(&self) -> Result<JsonValue, JsonRpcError> {
            Ok(json!([]))
        }

        async fn host_call_tool(
            &self,
            _caller_plugin: &str,
            _depth: u8,
            tool: &str,
            args: JsonValue,
        ) -> Result<JsonValue, JsonRpcError> {
            let instance = self
                .instance
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .upgrade()
                .expect("实例应存活");
            instance
                .call(tool, args)
                .await
                .map_err(|e| JsonRpcError::new(JsonRpcError::CODE_PLUGIN_ERROR, e.to_string()))
        }

        async fn host_get_config(&self, _caller_plugin: &str) -> Result<JsonValue, JsonRpcError> {
            Ok(json!({}))
        }

        async fn host_set_config(
            &self,
            _caller_plugin: &str,
            _value: JsonValue,
        ) -> Result<JsonValue, JsonRpcError> {
            Ok(json!({}))
        }

        async fn host_notify(&self, _caller_plugin: &str, _method: &str, _params: JsonValue) {}

        async fn host_ui_request(
            &self,
            _caller_plugin: &str,
            _ui_type: &str,
            _schema: JsonValue,
            _callback_method: &str,
        ) -> Result<JsonValue, JsonRpcError> {
            Ok(json!({ "status": "requested" }))
        }
    }

    /// 插件 → 宿主 → 同一个插件的自递归。若 `read_loop` 同步 await 派发，
    /// 内层调用的响应就没人收，必然超时——这条测试专门盯住那个回归。
    #[tokio::test(start_paused = true)]
    async fn 反向调用递归回本插件时读循环不死锁() {
        let (instance, mock) = started(Lifecycle::default()).await;
        let host = ReentrantHost::bind(&instance);
        let weak = Arc::downgrade(&(Arc::clone(&host) as Arc<dyn HostHandler>));
        instance.set_host(weak);

        mock.push_incoming(IncomingMessage::Request(JsonRpcRequest::new(
            77_i64,
            HOST_CALL_TOOL,
            json!({ "name": "demo:echo", "arguments": { "n": 1 } }),
        )));

        // 宿主回调本插件产生的出站请求，其响应必须由同一个读循环收取。
        let id = next_request_id(&mock).await;
        mock.push_incoming(IncomingMessage::Response(JsonRpcResponse::success(
            id,
            json!({ "n": 1 }),
        )));

        match mock.next_sent().await.expect("反向调用应有响应写回") {
            OutgoingMessage::Response(resp) => {
                assert_eq!(expect_success(resp.body), json!({ "n": 1 }));
            }
            other => panic!("期望响应，实际为 {other:?}"),
        }
    }
}
