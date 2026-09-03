//! 调度器：插件实例的统一管理与工具调用入口。
//!
//! 设计文档 §6.2 的六步调用链在 [`Supervisor::call_tool`] 中实现：
//! 工具名解析 → 权限校验 → 调用深度检查 → 按需唤醒 → RPC 转发 → 审计日志。
//!
//! UI、AI 编排插件、MCP 客户端三条路径共用 [`ToolInvoker`]，
//! 差别仅在传入的 [`CallerIdentity`]。

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value as JsonValue};
use tokio::sync::Mutex;

use crate::config::{self, JsonIoError};
use crate::permission::{CallerIdentity, PermissionChecker, PermissionError, PermissionPrompter};
use crate::protocol::manifest::{LifecycleMode, RestartPolicy, ToolDescriptor};
use crate::registry::Registry;
use crate::runtime::instance::{InstanceError, InstanceState, PluginInstance};
use crate::runtime::process::{spawn_plugin, ProcessConfig, ProcessError};
use crate::runtime::transport::Transport;

/// 调用链最大深度。超过即拒绝，防止插件间循环调用打爆栈。
pub const MAX_CALL_DEPTH: u8 = 5;

/// 崩溃重启的退避序列（秒），对应设计文档 §8：1 → 2 → 4 → 8 → 16，最多 5 次。
pub const RESTART_BACKOFF_SECS: [u64; 5] = [1, 2, 4, 8, 16];

/// 最大重启次数。
pub const MAX_RESTART_ATTEMPTS: u32 = RESTART_BACKOFF_SECS.len() as u32;

// ─────────────────── 调用入口抽象 ───────────────────

/// 工具调用的唯一通道。
///
/// 抽成 trait 是为了让反向 RPC（Phase 6）与 MCP 网关（Phase 8）
/// 依赖接口而非具体的 [`Supervisor`]，便于各自单测。
#[async_trait::async_trait]
pub trait ToolInvoker: Send + Sync {
    /// 列出全部可用工具。走缓存，不唤醒任何插件。
    async fn list_tools(&self) -> Vec<(String, ToolDescriptor)>;

    /// 调用工具。
    async fn call_tool(
        &self,
        tool_name: &str,
        args: JsonValue,
        caller: CallerIdentity,
    ) -> Result<JsonValue, InvokeError>;
}

// ─────────────────── 传输工厂 ───────────────────

/// 如何为插件创建传输通道。
///
/// `spawn_plugin` 返回具体的 `StdioTransport`，若 supervisor 直接调用它，
/// 每个测试都得真拉起子进程。抽出该 trait 后测试可注入 mock，
/// 与 Phase 3 用 `Transport` trait 隔离真实进程是同一手法。
#[async_trait::async_trait]
pub trait TransportFactory: Send + Sync {
    async fn create(&self, spec: &LaunchSpec) -> Result<Arc<dyn Transport>, ProcessError>;
}

/// 拉起一个插件进程所需的信息，从 manifest 提炼而来。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchSpec {
    pub plugin_id: String,
    pub plugin_dir: PathBuf,
    pub command: String,
    pub args: Vec<String>,
    /// stderr 转存目标。做成字段而非内部取 `paths::plugin_stderr_log()`，
    /// 同样是为了测试能指向临时目录。
    pub stderr_log: Option<PathBuf>,
}

/// 生产实现：真的 fork 子进程。
#[derive(Debug, Default, Clone, Copy)]
pub struct ProcessTransportFactory;

#[async_trait::async_trait]
impl TransportFactory for ProcessTransportFactory {
    async fn create(&self, spec: &LaunchSpec) -> Result<Arc<dyn Transport>, ProcessError> {
        let mut config = ProcessConfig::new(
            spec.plugin_id.clone(),
            spec.plugin_dir.clone(),
            spec.command.clone(),
            spec.args.clone(),
        );
        config.stderr_log = spec.stderr_log.clone();
        let transport = spawn_plugin(config).await?;
        Ok(transport as Arc<dyn Transport>)
    }
}

// ─────────────────── 工具缓存 ───────────────────

/// `~/.intools/cache/tools.json` 的内容：plugin id → 工具列表。
///
/// 有了它，宿主启动后无需唤醒任何插件就能把工具列表交给 AI，
/// 这是「按需运行」能成立的前提。
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ToolCache {
    #[serde(flatten)]
    entries: BTreeMap<String, Vec<ToolDescriptor>>,
}

impl ToolCache {
    pub fn load(path: &std::path::Path) -> Result<Self, JsonIoError> {
        Ok(config::read_json(path)?.unwrap_or_default())
    }

    pub fn get(&self, plugin_id: &str) -> Option<&[ToolDescriptor]> {
        self.entries.get(plugin_id).map(|v| v.as_slice())
    }

    pub fn put(&mut self, plugin_id: impl Into<String>, tools: Vec<ToolDescriptor>) {
        self.entries.insert(plugin_id.into(), tools);
    }

    pub fn remove(&mut self, plugin_id: &str) {
        self.entries.remove(plugin_id);
    }

    pub fn save(&self, path: &std::path::Path) -> Result<(), JsonIoError> {
        config::write_json(path, self)
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

// ─────────────────── 审计 ───────────────────

/// 一条调用审计记录，对应设计文档 §6.2 第六步。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuditEntry {
    pub caller: String,
    pub tool: String,
    pub plugin_id: String,
    /// 参数摘要而非全文——参数里可能有敏感内容，且日志不该被大 payload 撑爆。
    pub args_summary: String,
    pub duration_ms: u64,
    pub outcome: String,
}

/// 审计日志落点。做成 trait 便于测试断言，也便于后续换成写文件或数据库。
pub trait AuditSink: Send + Sync {
    fn record(&self, entry: AuditEntry);
}

/// 默认实现：写 tracing。
#[derive(Debug, Default, Clone, Copy)]
pub struct TracingAuditSink;

impl AuditSink for TracingAuditSink {
    fn record(&self, entry: AuditEntry) {
        tracing::info!(
            caller = %entry.caller,
            tool = %entry.tool,
            plugin = %entry.plugin_id,
            args = %entry.args_summary,
            duration_ms = entry.duration_ms,
            outcome = %entry.outcome,
            "tool call"
        );
    }
}

/// 生成参数摘要：只保留顶层键名与值的类型，不记录具体值。
fn summarize_args(args: &JsonValue) -> String {
    match args {
        JsonValue::Object(map) if map.is_empty() => "{}".to_string(),
        JsonValue::Object(map) => {
            let parts: Vec<String> = map
                .iter()
                .map(|(k, v)| format!("{k}:{}", value_kind(v)))
                .collect();
            format!("{{{}}}", parts.join(","))
        }
        other => value_kind(other).to_string(),
    }
}

fn value_kind(v: &JsonValue) -> &'static str {
    match v {
        JsonValue::Null => "null",
        JsonValue::Bool(_) => "bool",
        JsonValue::Number(_) => "number",
        JsonValue::String(_) => "string",
        JsonValue::Array(_) => "array",
        JsonValue::Object(_) => "object",
    }
}

// ─────────────────── 实例记录 ───────────────────

#[derive(Debug)]
struct ManagedInstance {
    instance: Arc<PluginInstance>,
    restart_attempts: u32,
}

// ─────────────────── 调度器 ───────────────────

/// 插件调度器。持有注册表、权限判定器与全部活跃实例。
pub struct Supervisor<P: PermissionPrompter> {
    registry: Registry,
    checker: Mutex<PermissionChecker<P>>,
    factory: Arc<dyn TransportFactory>,
    audit: Arc<dyn AuditSink>,
    instances: Mutex<BTreeMap<String, ManagedInstance>>,
    cache: Mutex<ToolCache>,
    cache_path: Option<PathBuf>,
    logs_dir: Option<PathBuf>,
}

impl<P: PermissionPrompter> Supervisor<P> {
    pub fn new(
        registry: Registry,
        checker: PermissionChecker<P>,
        factory: Arc<dyn TransportFactory>,
    ) -> Self {
        Self {
            registry,
            checker: Mutex::new(checker),
            factory,
            audit: Arc::new(TracingAuditSink),
            instances: Mutex::new(BTreeMap::new()),
            cache: Mutex::new(ToolCache::default()),
            cache_path: None,
            logs_dir: None,
        }
    }

    /// 指定 tools.json 位置。不设置则缓存只存在于内存，不落盘。
    pub fn with_cache_path(mut self, path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        if let Ok(loaded) = ToolCache::load(&path) {
            self.cache = Mutex::new(loaded);
        }
        self.cache_path = Some(path);
        self
    }

    /// 指定 stderr 日志目录。
    pub fn with_logs_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.logs_dir = Some(dir.into());
        self
    }

    pub fn with_audit_sink(mut self, sink: Arc<dyn AuditSink>) -> Self {
        self.audit = sink;
        self
    }

    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    /// 当前存在实例的插件 id。
    pub async fn live_plugin_ids(&self) -> Vec<String> {
        self.instances.lock().await.keys().cloned().collect()
    }

    pub async fn instance_state(&self, plugin_id: &str) -> Option<InstanceState> {
        self.instances
            .lock()
            .await
            .get(plugin_id)
            .map(|m| m.instance.state())
    }

    /// 启动所有 `startup` / `background` 模式的插件。
    pub async fn start_eager_plugins(&self) -> Vec<(String, InvokeError)> {
        let eager: Vec<String> = self
            .registry
            .list_plugins_sorted()
            .into_iter()
            .filter(|p| {
                matches!(
                    p.manifest.lifecycle.mode,
                    LifecycleMode::Startup | LifecycleMode::Background
                )
            })
            .map(|p| p.id().to_string())
            .collect();

        let mut failures = Vec::new();
        for id in eager {
            if let Err(err) = self.ensure_running(&id).await {
                failures.push((id, err));
            }
        }
        failures
    }

    /// 停止全部实例。宿主退出时调用。
    pub async fn shutdown_all(&self) {
        let mut guard = self.instances.lock().await;
        for managed in guard.values() {
            managed.instance.stop().await;
        }
        guard.clear();
    }

    /// 确保插件在运行，必要时拉起并握手。返回已就绪的实例。
    async fn ensure_running(&self, plugin_id: &str) -> Result<Arc<PluginInstance>, InvokeError> {
        {
            let guard = self.instances.lock().await;
            if let Some(managed) = guard.get(plugin_id) {
                if managed.instance.state().accepts_requests() {
                    return Ok(Arc::clone(&managed.instance));
                }
            }
        }
        self.spawn_and_handshake(plugin_id).await
    }

    async fn spawn_and_handshake(
        &self,
        plugin_id: &str,
    ) -> Result<Arc<PluginInstance>, InvokeError> {
        let plugin = self
            .registry
            .get_plugin(plugin_id)
            .ok_or_else(|| InvokeError::PluginNotFound {
                plugin_id: plugin_id.to_string(),
            })?;

        let spec = LaunchSpec {
            plugin_id: plugin_id.to_string(),
            plugin_dir: plugin.plugin_dir.clone(),
            command: plugin.manifest.exec.command.clone(),
            args: plugin.manifest.exec.args.clone(),
            stderr_log: self
                .logs_dir
                .as_ref()
                .map(|d| d.join(format!("{plugin_id}.log"))),
        };

        let transport = self.factory.create(&spec).await?;
        let instance = Arc::new(PluginInstance::new(
            plugin_id.to_string(),
            plugin.manifest.lifecycle.clone(),
            transport,
        ));

        let plugin_dir = plugin.plugin_dir.display().to_string();
        let handshake = instance.start(json!({}), plugin_dir).await?;

        // 握手拿到的是插件运行时自报的工具表，比 manifest 更权威——写入缓存。
        {
            let mut cache = self.cache.lock().await;
            cache.put(plugin_id, handshake.tools.clone());
            if let Some(path) = &self.cache_path {
                if let Err(err) = cache.save(path) {
                    tracing::warn!(plugin = plugin_id, error = %err, "写入工具缓存失败");
                }
            }
        }

        let mut guard = self.instances.lock().await;
        let attempts = guard
            .get(plugin_id)
            .map(|m| m.restart_attempts)
            .unwrap_or(0);
        guard.insert(
            plugin_id.to_string(),
            ManagedInstance {
                instance: Arc::clone(&instance),
                restart_attempts: attempts,
            },
        );

        Ok(instance)
    }

    /// 插件崩溃后按 `restart_policy` 重启，退避 1/2/4/8/16 秒，最多 5 次。
    pub async fn restart_after_crash(&self, plugin_id: &str) -> Result<(), InvokeError> {
        let policy = self
            .registry
            .get_plugin(plugin_id)
            .map(|p| p.manifest.lifecycle.restart_policy)
            .ok_or_else(|| InvokeError::PluginNotFound {
                plugin_id: plugin_id.to_string(),
            })?;

        if policy == RestartPolicy::Never {
            self.instances.lock().await.remove(plugin_id);
            return Err(InvokeError::RestartDeclined {
                plugin_id: plugin_id.to_string(),
                reason: "restart_policy = never",
            });
        }

        let attempt = {
            let mut guard = self.instances.lock().await;
            match guard.get_mut(plugin_id) {
                Some(managed) => {
                    managed.restart_attempts += 1;
                    managed.restart_attempts
                }
                None => 1,
            }
        };

        if attempt > MAX_RESTART_ATTEMPTS {
            self.instances.lock().await.remove(plugin_id);
            return Err(InvokeError::RestartExhausted {
                plugin_id: plugin_id.to_string(),
                attempts: MAX_RESTART_ATTEMPTS,
            });
        }

        let delay = Duration::from_secs(RESTART_BACKOFF_SECS[(attempt - 1) as usize]);
        tokio::time::sleep(delay).await;
        self.spawn_and_handshake(plugin_id).await.map(|_| ())
    }

    /// 第 n 次重启前应等待的时长。`n` 从 1 计。
    pub fn backoff_for_attempt(attempt: u32) -> Option<Duration> {
        RESTART_BACKOFF_SECS
            .get((attempt.checked_sub(1)?) as usize)
            .map(|s| Duration::from_secs(*s))
    }
}

#[async_trait::async_trait]
impl<P: PermissionPrompter> ToolInvoker for Supervisor<P> {
    /// 列工具优先读缓存，缓存缺失才回退到 manifest 声明。
    /// 两者都不唤醒插件——这是「体积小、启动快」目标下的关键路径。
    async fn list_tools(&self) -> Vec<(String, ToolDescriptor)> {
        let cache = self.cache.lock().await;
        let mut out = Vec::new();
        for plugin in self.registry.list_plugins_sorted() {
            let id = plugin.id();
            match cache.get(id) {
                Some(tools) => out.extend(tools.iter().map(|t| (id.to_string(), t.clone()))),
                None => out.extend(
                    plugin
                        .manifest
                        .tools
                        .iter()
                        .map(|t| (id.to_string(), t.clone())),
                ),
            }
        }
        out
    }

    async fn call_tool(
        &self,
        tool_name: &str,
        args: JsonValue,
        caller: CallerIdentity,
    ) -> Result<JsonValue, InvokeError> {
        let started = Instant::now();
        let args_summary = summarize_args(&args);
        let result = self.call_tool_inner(tool_name, args, &caller).await;

        // 第六步：无论成败都要留痕。
        let (plugin_id, outcome) = match &result {
            Ok(_) => (
                self.registry
                    .resolve_tool(tool_name)
                    .map(|p| p.id().to_string())
                    .unwrap_or_default(),
                "ok".to_string(),
            ),
            Err(err) => (
                self.registry
                    .resolve_tool(tool_name)
                    .map(|p| p.id().to_string())
                    .unwrap_or_default(),
                format!("error: {err}"),
            ),
        };
        self.audit.record(AuditEntry {
            caller: caller.label(),
            tool: tool_name.to_string(),
            plugin_id,
            args_summary,
            duration_ms: started.elapsed().as_millis() as u64,
            outcome,
        });

        result
    }
}

impl<P: PermissionPrompter> Supervisor<P> {
    async fn call_tool_inner(
        &self,
        tool_name: &str,
        args: JsonValue,
        caller: &CallerIdentity,
    ) -> Result<JsonValue, InvokeError> {
        // 第一步：工具名解析。
        let plugin =
            self.registry
                .resolve_tool(tool_name)
                .ok_or_else(|| InvokeError::ToolNotFound {
                    tool: tool_name.to_string(),
                })?;
        let plugin_id = plugin.id().to_string();
        let declared = plugin.manifest.capabilities.permissions.clone();

        // 第二步：权限校验（按调用方身份）。
        self.checker
            .lock()
            .await
            .check_all(&plugin_id, &declared, caller)
            .await?;

        // 第三步：调用深度检查。超过上限说明插件间出现了过深或循环的调用。
        if caller.depth() >= MAX_CALL_DEPTH {
            return Err(InvokeError::DepthExceeded {
                depth: caller.depth(),
                max: MAX_CALL_DEPTH,
            });
        }

        // 第四步：实例按需唤醒。
        let instance = self.ensure_running(&plugin_id).await?;

        // 第五步：RPC 转发（超时由 instance 依 lifecycle 配置处理）。
        let result = instance
            .call(
                "tools/call",
                json!({ "name": tool_name, "arguments": args }),
            )
            .await?;

        Ok(result)
    }
}

// ─────────────────── 错误 ───────────────────

#[derive(Debug, thiserror::Error)]
pub enum InvokeError {
    #[error("未找到工具 `{tool}`")]
    ToolNotFound { tool: String },

    #[error("未找到插件 `{plugin_id}`")]
    PluginNotFound { plugin_id: String },

    #[error("调用链深度 {depth} 已达上限 {max}")]
    DepthExceeded { depth: u8, max: u8 },

    #[error("权限校验未通过：{0}")]
    Permission(#[from] PermissionError),

    #[error("插件进程启动失败：{0}")]
    Process(#[from] ProcessError),

    #[error("插件调用失败：{0}")]
    Instance(#[from] InstanceError),

    #[error("插件 `{plugin_id}` 不重启：{reason}")]
    RestartDeclined {
        plugin_id: String,
        reason: &'static str,
    },

    #[error("插件 `{plugin_id}` 重启 {attempts} 次仍失败，放弃")]
    RestartExhausted { plugin_id: String, attempts: u32 },
}

impl InvokeError {
    /// 是否因权限被拒。UI 与 MCP 据此区分「不允许」与「出故障」。
    pub fn is_permission_denied(&self) -> bool {
        matches!(self, InvokeError::Permission(e) if e.is_denied())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex as StdMutex;

    use crate::permission::{FixedPrompter, PermissionStore, PromptDecision};
    use crate::protocol::manifest::{
        Capabilities, ExecInfo, Lifecycle, Manifest, Permission, PluginInfo,
    };
    use crate::protocol::message::{
        IncomingMessage, JsonRpcNotification, JsonRpcResponse, OutgoingMessage,
    };
    use crate::registry::discovery::LoadedPlugin;
    use crate::runtime::transport::MockTransport;

    // ─────────── 构造 manifest / registry ───────────

    fn tool(name: &str) -> ToolDescriptor {
        ToolDescriptor {
            name: name.to_string(),
            description: String::new(),
            input_schema: json!({"type":"object","properties":{},"required":[]}),
        }
    }

    fn manifest_of(
        id: &str,
        tools: Vec<&str>,
        perms: Vec<&str>,
        lifecycle: Lifecycle,
    ) -> Manifest {
        Manifest {
            plugin: PluginInfo {
                id: id.to_string(),
                name: id.to_string(),
                version: "1.0.0".to_string(),
                author: String::new(),
                description: String::new(),
            },
            exec: ExecInfo {
                command: "python".to_string(),
                args: vec!["main.py".to_string()],
            },
            tools: tools.into_iter().map(tool).collect(),
            capabilities: Capabilities {
                permissions: perms
                    .into_iter()
                    .map(|p| Permission::parse(p).expect("测试权限串应可解析"))
                    .collect(),
            },
            lifecycle,
        }
    }

    fn registry_of(manifests: Vec<Manifest>) -> Registry {
        let mut reg = Registry::new();
        for m in manifests {
            reg.insert_loaded(LoadedPlugin {
                plugin_dir: PathBuf::from(format!("/plugins/{}", m.plugin.id)),
                manifest: m,
            });
        }
        reg
    }

    /// 只声明低危权限的单插件注册表，供多数用例复用。
    fn simple_registry() -> Registry {
        registry_of(vec![manifest_of(
            "com.example.demo",
            vec!["demo:echo"],
            vec!["net:http"],
            Lifecycle::default(),
        )])
    }

    // ─────────── 自动应答的假插件 ───────────

    /// 后台任务：盯着 mock 的出站队列，替真插件自动应答。
    ///
    /// 直接预置消息（instance.rs 单测的做法）在这里行不通——supervisor 何时唤醒
    /// 哪个插件由被测逻辑自己决定，测试无法预知请求 ID 与时序。所以改成
    /// 「请求-驱动」的应答者：收到什么就答什么。
    fn spawn_fake_plugin(mock: Arc<MockTransport>, tools: Vec<ToolDescriptor>) {
        mock.push_incoming(IncomingMessage::Notification(JsonRpcNotification::new(
            "plugin/hello",
            json!({
                "protocol_version": "1.0",
                "tools": serde_json::to_value(&tools).unwrap(),
            }),
        )));

        tokio::spawn(async move {
            while let Some(out) = mock.next_sent().await {
                if let OutgoingMessage::Request(req) = out {
                    let result = match req.method.as_str() {
                        "plugin/ready" => json!({}),
                        "tools/call" => {
                            let params = req.params.clone().unwrap_or(json!({}));
                            json!({ "echo": params })
                        }
                        _ => json!({}),
                    };
                    mock.push_incoming(IncomingMessage::Response(JsonRpcResponse::success(
                        req.id, result,
                    )));
                }
            }
        });
    }

    /// 记录每次创建请求的 transport 工厂，插件永远握手成功。
    struct RecordingFactory {
        created: StdMutex<Vec<String>>,
        tools: Vec<ToolDescriptor>,
        fail_times: AtomicUsize,
    }

    impl RecordingFactory {
        fn new(tools: Vec<&str>) -> Arc<Self> {
            Arc::new(Self {
                created: StdMutex::new(Vec::new()),
                tools: tools.into_iter().map(tool).collect(),
                fail_times: AtomicUsize::new(0),
            })
        }

        /// 前 `n` 次创建失败，用于验证重启退避。
        fn failing_first(tools: Vec<&str>, n: usize) -> Arc<Self> {
            let f = Self::new(tools);
            f.fail_times.store(n, Ordering::SeqCst);
            f
        }

        fn create_count(&self) -> usize {
            self.created.lock().unwrap().len()
        }
    }

    #[async_trait::async_trait]
    impl TransportFactory for RecordingFactory {
        async fn create(&self, spec: &LaunchSpec) -> Result<Arc<dyn Transport>, ProcessError> {
            self.created.lock().unwrap().push(spec.plugin_id.clone());
            if self.fail_times.load(Ordering::SeqCst) > 0 {
                self.fail_times.fetch_sub(1, Ordering::SeqCst);
                return Err(ProcessError::Spawn {
                    plugin_id: spec.plugin_id.clone(),
                    command: spec.command.clone(),
                    message: "测试注入的启动失败".to_string(),
                });
            }
            let mock = Arc::new(MockTransport::new());
            spawn_fake_plugin(Arc::clone(&mock), self.tools.clone());
            Ok(mock as Arc<dyn Transport>)
        }
    }

    /// 收集审计记录，供断言。
    #[derive(Default)]
    struct CollectingAudit {
        entries: StdMutex<Vec<AuditEntry>>,
    }

    impl CollectingAudit {
        fn entries(&self) -> Vec<AuditEntry> {
            self.entries.lock().unwrap().clone()
        }
    }

    impl AuditSink for CollectingAudit {
        fn record(&self, entry: AuditEntry) {
            self.entries.lock().unwrap().push(entry);
        }
    }

    fn checker_with(dir: &std::path::Path, decision: PromptDecision) -> PermissionChecker<FixedPrompter> {
        let store = PermissionStore::open_at(dir.join("permissions.json"))
            .expect("测试目录下应能打开权限存储");
        PermissionChecker::new(store, FixedPrompter(decision))
    }

    fn tmp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "intools-sup-{tag}-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH_FOR_TEST)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    const UNIX_EPOCH_FOR_TEST: std::time::SystemTime = std::time::UNIX_EPOCH;

    // ─────────── 六步调用链 ───────────

    #[tokio::test]
    async fn 调用未知工具应报工具不存在() {
        let dir = tmp_dir("unknown");
        let sup = Supervisor::new(
            simple_registry(),
            checker_with(&dir, PromptDecision::AllowAlways),
            RecordingFactory::new(vec!["demo:echo"]),
        );
        let err = sup
            .call_tool("nope:missing", json!({}), CallerIdentity::Ui)
            .await
            .unwrap_err();
        assert!(matches!(err, InvokeError::ToolNotFound { .. }), "得到 {err:?}");
    }

    #[tokio::test]
    async fn 成功调用应转发参数并返回插件结果() {
        let dir = tmp_dir("ok");
        let sup = Supervisor::new(
            simple_registry(),
            checker_with(&dir, PromptDecision::AllowAlways),
            RecordingFactory::new(vec!["demo:echo"]),
        );
        let out = sup
            .call_tool("demo:echo", json!({"text":"hi"}), CallerIdentity::Ui)
            .await
            .expect("低危权限应放行");

        // 假插件把 tools/call 的 params 原样回传，可据此验证转发内容。
        assert_eq!(out["echo"]["name"], json!("demo:echo"));
        assert_eq!(out["echo"]["arguments"], json!({"text":"hi"}));
    }

    #[tokio::test]
    async fn 权限被拒时调用失败且不唤醒插件() {
        let dir = tmp_dir("denied");
        let factory = RecordingFactory::new(vec!["scr:shot"]);
        let sup = Supervisor::new(
            registry_of(vec![manifest_of(
                "com.example.scr",
                vec!["scr:shot"],
                vec!["screen:capture"], // 中危，会触发询问
                Lifecycle::default(),
            )]),
            checker_with(&dir, PromptDecision::DenyAlways),
            Arc::clone(&factory) as Arc<dyn TransportFactory>,
        );

        let err = sup
            .call_tool("scr:shot", json!({}), CallerIdentity::Ui)
            .await
            .unwrap_err();

        assert!(err.is_permission_denied(), "得到 {err:?}");
        // 权限是第二步，早于第四步唤醒：被拒就不该拉起任何进程。
        assert_eq!(factory.create_count(), 0, "被拒的调用不应唤醒插件");
        assert!(sup.live_plugin_ids().await.is_empty());
    }

    #[tokio::test]
    async fn 调用深度达到上限应被拒绝() {
        let dir = tmp_dir("depth");
        let factory = RecordingFactory::new(vec!["demo:echo"]);
        let sup = Supervisor::new(
            simple_registry(),
            checker_with(&dir, PromptDecision::AllowAlways),
            Arc::clone(&factory) as Arc<dyn TransportFactory>,
        );

        let err = sup
            .call_tool(
                "demo:echo",
                json!({}),
                CallerIdentity::Plugin {
                    id: "com.example.a".to_string(),
                    depth: MAX_CALL_DEPTH,
                },
            )
            .await
            .unwrap_err();

        match err {
            InvokeError::DepthExceeded { depth, max } => {
                assert_eq!(depth, MAX_CALL_DEPTH);
                assert_eq!(max, MAX_CALL_DEPTH);
            }
            other => panic!("应为深度超限，得到 {other:?}"),
        }
        assert_eq!(factory.create_count(), 0, "深度检查先于唤醒");
    }

    #[tokio::test]
    async fn 深度未达上限应放行() {
        let dir = tmp_dir("depth-ok");
        let sup = Supervisor::new(
            simple_registry(),
            checker_with(&dir, PromptDecision::AllowAlways),
            RecordingFactory::new(vec!["demo:echo"]),
        );
        let out = sup
            .call_tool(
                "demo:echo",
                json!({}),
                CallerIdentity::Plugin {
                    id: "com.example.a".to_string(),
                    depth: MAX_CALL_DEPTH - 1,
                },
            )
            .await;
        assert!(out.is_ok(), "depth={} 应放行", MAX_CALL_DEPTH - 1);
    }

    // ─────────── 实例复用 ───────────

    #[tokio::test]
    async fn 连续调用同一插件只唤醒一次() {
        let dir = tmp_dir("reuse");
        let factory = RecordingFactory::new(vec!["demo:echo"]);
        let sup = Supervisor::new(
            simple_registry(),
            checker_with(&dir, PromptDecision::AllowAlways),
            Arc::clone(&factory) as Arc<dyn TransportFactory>,
        );

        for _ in 0..3 {
            sup.call_tool("demo:echo", json!({}), CallerIdentity::Ui)
                .await
                .unwrap();
        }
        assert_eq!(factory.create_count(), 1, "已就绪的实例应被复用");
        assert_eq!(sup.live_plugin_ids().await, vec!["com.example.demo"]);
        assert_eq!(
            sup.instance_state("com.example.demo").await,
            Some(InstanceState::Idle)
        );
    }

    #[tokio::test]
    async fn 仅启动eager插件而不碰按需插件() {
        let dir = tmp_dir("eager");
        let factory = RecordingFactory::new(vec!["a:x", "b:x", "c:x"]);
        let sup = Supervisor::new(
            registry_of(vec![
                manifest_of("com.example.a", vec!["a:x"], vec![], Lifecycle::default()),
                manifest_of(
                    "com.example.b",
                    vec!["b:x"],
                    vec![],
                    Lifecycle {
                        mode: LifecycleMode::Background,
                        ..Lifecycle::default()
                    },
                ),
                manifest_of(
                    "com.example.c",
                    vec!["c:x"],
                    vec![],
                    Lifecycle {
                        mode: LifecycleMode::Startup,
                        ..Lifecycle::default()
                    },
                ),
            ]),
            checker_with(&dir, PromptDecision::AllowAlways),
            Arc::clone(&factory) as Arc<dyn TransportFactory>,
        );

        let failures = sup.start_eager_plugins().await;
        assert!(failures.is_empty(), "不应有启动失败：{failures:?}");

        let mut live = sup.live_plugin_ids().await;
        live.sort();
        assert_eq!(live, vec!["com.example.b", "com.example.c"]);
        assert_eq!(factory.create_count(), 2, "on-demand 插件不应被启动");
    }

    #[tokio::test]
    async fn 全部停止后不再保留实例() {
        let dir = tmp_dir("shutdown");
        let sup = Supervisor::new(
            simple_registry(),
            checker_with(&dir, PromptDecision::AllowAlways),
            RecordingFactory::new(vec!["demo:echo"]),
        );
        sup.call_tool("demo:echo", json!({}), CallerIdentity::Ui)
            .await
            .unwrap();
        assert_eq!(sup.live_plugin_ids().await.len(), 1);

        sup.shutdown_all().await;
        assert!(sup.live_plugin_ids().await.is_empty());
    }

    // ─────────── 工具缓存 ───────────

    #[tokio::test]
    async fn 列工具不唤醒任何插件() {
        let dir = tmp_dir("list");
        let factory = RecordingFactory::new(vec!["demo:echo"]);
        let sup = Supervisor::new(
            simple_registry(),
            checker_with(&dir, PromptDecision::AllowAlways),
            Arc::clone(&factory) as Arc<dyn TransportFactory>,
        );

        let tools = sup.list_tools().await;
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].0, "com.example.demo");
        assert_eq!(tools[0].1.name, "demo:echo");
        assert_eq!(factory.create_count(), 0, "列工具绝不能拉起进程");
    }

    #[tokio::test]
    async fn 缓存为空时回退到manifest声明() {
        let dir = tmp_dir("fallback");
        let sup = Supervisor::new(
            simple_registry(),
            checker_with(&dir, PromptDecision::AllowAlways),
            RecordingFactory::new(vec!["demo:echo"]),
        );
        let names: Vec<String> = sup.list_tools().await.into_iter().map(|(_, t)| t.name).collect();
        assert_eq!(names, vec!["demo:echo"]);
    }

    #[tokio::test]
    async fn 握手后应以运行时工具表覆盖manifest() {
        let dir = tmp_dir("cache-write");
        // manifest 声明 demo:echo，但插件运行时自报 demo:echo + demo:extra。
        let sup = Supervisor::new(
            simple_registry(),
            checker_with(&dir, PromptDecision::AllowAlways),
            RecordingFactory::new(vec!["demo:echo", "demo:extra"]),
        )
        .with_cache_path(dir.join("tools.json"));

        sup.call_tool("demo:echo", json!({}), CallerIdentity::Ui)
            .await
            .unwrap();

        let names: Vec<String> = sup.list_tools().await.into_iter().map(|(_, t)| t.name).collect();
        assert_eq!(
            names,
            vec!["demo:echo", "demo:extra"],
            "握手拿到的工具表比 manifest 更权威"
        );
    }

    #[tokio::test]
    async fn 工具缓存应落盘并可跨实例读回() {
        let dir = tmp_dir("cache-roundtrip");
        let cache_path = dir.join("tools.json");
        {
            let sup = Supervisor::new(
                simple_registry(),
                checker_with(&dir, PromptDecision::AllowAlways),
                RecordingFactory::new(vec!["demo:echo", "demo:extra"]),
            )
            .with_cache_path(&cache_path);
            sup.call_tool("demo:echo", json!({}), CallerIdentity::Ui)
                .await
                .unwrap();
        }

        // 新 supervisor 只加载缓存文件，不做任何调用。
        let factory = RecordingFactory::new(vec![]);
        let sup2 = Supervisor::new(
            simple_registry(),
            checker_with(&dir, PromptDecision::AllowAlways),
            Arc::clone(&factory) as Arc<dyn TransportFactory>,
        )
        .with_cache_path(&cache_path);

        let names: Vec<String> = sup2.list_tools().await.into_iter().map(|(_, t)| t.name).collect();
        assert_eq!(names, vec!["demo:echo", "demo:extra"], "缓存应从磁盘读回");
        assert_eq!(factory.create_count(), 0, "读缓存不该唤醒插件");
    }

    #[test]
    fn 工具缓存的落盘格式为插件id到工具表的映射() {
        let dir = tmp_dir("cache-format");
        let path = dir.join("tools.json");
        let mut cache = ToolCache::default();
        cache.put("com.example.demo", vec![tool("demo:echo")]);
        cache.save(&path).unwrap();

        let raw: JsonValue = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(raw.is_object());
        assert_eq!(raw["com.example.demo"][0]["name"], json!("demo:echo"));

        let back = ToolCache::load(&path).unwrap();
        assert_eq!(back.get("com.example.demo").unwrap().len(), 1);
    }

    // ─────────── 崩溃重启 ───────────

    #[test]
    fn 退避序列应为1_2_4_8_16且第6次为空() {
        let secs: Vec<u64> = (1..=5)
            .map(|n| {
                Supervisor::<FixedPrompter>::backoff_for_attempt(n)
                    .unwrap()
                    .as_secs()
            })
            .collect();
        assert_eq!(secs, vec![1, 2, 4, 8, 16]);
        assert!(Supervisor::<FixedPrompter>::backoff_for_attempt(6).is_none());
        assert!(Supervisor::<FixedPrompter>::backoff_for_attempt(0).is_none());
    }

    #[tokio::test]
    async fn never策略的插件崩溃后不重启() {
        let dir = tmp_dir("never");
        let factory = RecordingFactory::new(vec!["demo:echo"]);
        let sup = Supervisor::new(
            registry_of(vec![manifest_of(
                "com.example.demo",
                vec!["demo:echo"],
                vec![],
                Lifecycle {
                    restart_policy: RestartPolicy::Never,
                    ..Lifecycle::default()
                },
            )]),
            checker_with(&dir, PromptDecision::AllowAlways),
            Arc::clone(&factory) as Arc<dyn TransportFactory>,
        );

        sup.call_tool("demo:echo", json!({}), CallerIdentity::Ui)
            .await
            .unwrap();
        assert_eq!(factory.create_count(), 1);

        let err = sup.restart_after_crash("com.example.demo").await.unwrap_err();
        assert!(matches!(err, InvokeError::RestartDeclined { .. }), "得到 {err:?}");
        assert_eq!(factory.create_count(), 1, "never 策略不得再次拉起");
        assert!(sup.live_plugin_ids().await.is_empty(), "应移除实例记录");
    }

    #[tokio::test]
    async fn on_failure策略崩溃后应重启() {
        let dir = tmp_dir("restart");
        let factory = RecordingFactory::new(vec!["demo:echo"]);
        let sup = Supervisor::new(
            simple_registry(),
            checker_with(&dir, PromptDecision::AllowAlways),
            Arc::clone(&factory) as Arc<dyn TransportFactory>,
        );

        sup.call_tool("demo:echo", json!({}), CallerIdentity::Ui)
            .await
            .unwrap();

        // 首次退避 1 秒，用虚拟时钟跳过等待。
        tokio::time::pause();
        let handle = {
            let fut = sup.restart_after_crash("com.example.demo");
            tokio::time::timeout(Duration::from_secs(120), fut)
        };
        let res = handle.await.expect("不应超时");
        assert!(res.is_ok(), "on-failure 应重启成功：{res:?}");
        assert_eq!(factory.create_count(), 2, "应重新创建一次 transport");
    }

    #[tokio::test]
    async fn 未知插件崩溃应报插件不存在() {
        let dir = tmp_dir("crash-unknown");
        let sup = Supervisor::new(
            simple_registry(),
            checker_with(&dir, PromptDecision::AllowAlways),
            RecordingFactory::new(vec!["demo:echo"]),
        );
        let err = sup.restart_after_crash("com.example.ghost").await.unwrap_err();
        assert!(matches!(err, InvokeError::PluginNotFound { .. }), "得到 {err:?}");
    }

    #[tokio::test]
    async fn 启动失败应作为错误上报而非静默() {
        let dir = tmp_dir("spawn-fail");
        let factory = RecordingFactory::failing_first(vec!["demo:echo"], 1);
        let sup = Supervisor::new(
            simple_registry(),
            checker_with(&dir, PromptDecision::AllowAlways),
            Arc::clone(&factory) as Arc<dyn TransportFactory>,
        );

        let err = sup
            .call_tool("demo:echo", json!({}), CallerIdentity::Ui)
            .await
            .unwrap_err();
        assert!(matches!(err, InvokeError::Process(_)), "得到 {err:?}");
        assert!(!err.is_permission_denied(), "启动失败不应被误判为权限问题");
        assert!(sup.live_plugin_ids().await.is_empty());
    }

    // ─────────── 审计 ───────────

    #[tokio::test]
    async fn 成功调用应留下审计记录() {
        let dir = tmp_dir("audit-ok");
        let audit = Arc::new(CollectingAudit::default());
        let sup = Supervisor::new(
            simple_registry(),
            checker_with(&dir, PromptDecision::AllowAlways),
            RecordingFactory::new(vec!["demo:echo"]),
        )
        .with_audit_sink(Arc::clone(&audit) as Arc<dyn AuditSink>);

        sup.call_tool("demo:echo", json!({"text":"hi"}), CallerIdentity::Ui)
            .await
            .unwrap();

        let entries = audit.entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].caller, "ui");
        assert_eq!(entries[0].tool, "demo:echo");
        assert_eq!(entries[0].plugin_id, "com.example.demo");
        assert_eq!(entries[0].outcome, "ok");
    }

    #[tokio::test]
    async fn 失败调用同样应留下审计记录() {
        let dir = tmp_dir("audit-err");
        let audit = Arc::new(CollectingAudit::default());
        let sup = Supervisor::new(
            registry_of(vec![manifest_of(
                "com.example.scr",
                vec!["scr:shot"],
                vec!["screen:capture"],
                Lifecycle::default(),
            )]),
            checker_with(&dir, PromptDecision::DenyAlways),
            RecordingFactory::new(vec!["scr:shot"]),
        )
        .with_audit_sink(Arc::clone(&audit) as Arc<dyn AuditSink>);

        let _ = sup
            .call_tool("scr:shot", json!({}), CallerIdentity::Ui)
            .await;

        let entries = audit.entries();
        assert_eq!(entries.len(), 1, "失败也必须留痕，否则拒绝行为不可追溯");
        assert!(entries[0].outcome.starts_with("error:"), "得到 {}", entries[0].outcome);
    }

    #[tokio::test]
    async fn 审计应记录调用方身份() {
        let dir = tmp_dir("audit-caller");
        let audit = Arc::new(CollectingAudit::default());
        let sup = Supervisor::new(
            simple_registry(),
            checker_with(&dir, PromptDecision::AllowAlways),
            RecordingFactory::new(vec!["demo:echo"]),
        )
        .with_audit_sink(Arc::clone(&audit) as Arc<dyn AuditSink>);

        sup.call_tool(
            "demo:echo",
            json!({}),
            CallerIdentity::Mcp {
                client_name: "claude".to_string(),
            },
        )
        .await
        .unwrap();

        assert_eq!(audit.entries()[0].caller, "mcp:claude");
    }

    #[test]
    fn 参数摘要只保留键名与类型不泄漏值() {
        let s = summarize_args(&json!({
            "path": "/home/me/secret.txt",
            "count": 3,
            "flag": true
        }));
        assert!(s.contains("path:string"));
        assert!(s.contains("count:number"));
        assert!(s.contains("flag:bool"));
        assert!(!s.contains("secret.txt"), "摘要不得包含参数值：{s}");
        assert_eq!(summarize_args(&json!({})), "{}");
        assert_eq!(summarize_args(&json!("bare")), "string");
    }

    // ─────────── MCP 与权限的联动 ───────────

    #[tokio::test]
    async fn mcp调用高危插件应被拒且不唤醒() {
        let dir = tmp_dir("mcp-high");
        let factory = RecordingFactory::new(vec!["ctl:click"]);
        let sup = Supervisor::new(
            registry_of(vec![manifest_of(
                "com.example.ctl",
                vec!["ctl:click"],
                vec!["input:control"], // 高危
                Lifecycle::default(),
            )]),
            checker_with(&dir, PromptDecision::AllowAlways), // 即便一律允许也应被拦
            Arc::clone(&factory) as Arc<dyn TransportFactory>,
        );

        let err = sup
            .call_tool(
                "ctl:click",
                json!({}),
                CallerIdentity::Mcp {
                    client_name: "claude".to_string(),
                },
            )
            .await
            .unwrap_err();

        assert!(err.is_permission_denied(), "得到 {err:?}");
        assert_eq!(factory.create_count(), 0);
    }

    #[tokio::test]
    async fn ui调用高危插件在用户同意后应放行() {
        let dir = tmp_dir("ui-high");
        let sup = Supervisor::new(
            registry_of(vec![manifest_of(
                "com.example.ctl",
                vec!["ctl:click"],
                vec!["input:control"],
                Lifecycle::default(),
            )]),
            checker_with(&dir, PromptDecision::AllowSession),
            RecordingFactory::new(vec!["ctl:click"]),
        );

        let out = sup
            .call_tool("ctl:click", json!({}), CallerIdentity::Ui)
            .await;
        assert!(out.is_ok(), "UI 前用户在场，同意后应放行：{out:?}");
    }
}
