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

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
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
    IncomingMessage, JsonRpcError, JsonRpcNotification, JsonRpcRequest, OutgoingMessage, RequestId,
    ResponseBody,
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
            }),
            inflight: AtomicU64::new(0),
            idle_epoch: AtomicU64::new(0),
            reader: Mutex::new(None),
        }
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
        let full_feature = negotiate_version(ProtocolVersion::V1_0, plugin_version).map_err(
            |mismatch| InstanceError::VersionMismatch {
                reason: mismatch.to_string(),
            },
        )?;

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
        let result = self.call_inner(&method, params, id.clone(), rx).await;
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
                // 插件发来的请求（反向 RPC）与通知在 Phase 5 由 Supervisor 接管，
                // 这里先安静丢弃，保证读循环不会因未知消息中断。
                Ok(_) => {}
                Err(_) => break,
            }
        }
        self.on_disconnect();
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

        let info = instance.start(json!({"k": 1}), "/plugins/demo").await.unwrap();

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

        instance.start(json!({"token": "abc"}), "/plugins/demo").await.unwrap();

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

        let err = instance.start(json!({}), "/plugins/demo").await.unwrap_err();

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

        let err = instance.start(json!({}), "/plugins/demo").await.unwrap_err();

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

        let err = instance.start(json!({}), "/plugins/demo").await.unwrap_err();

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
}
