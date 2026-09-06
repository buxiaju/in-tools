//! 调度器：插件实例的统一管理与工具调用入口。
//!
//! 设计文档 §6.2 的六步调用链在 [`Supervisor::call_tool`] 中实现：
//! 工具名解析 → 权限校验 → 调用深度检查 → 按需唤醒 → RPC 转发 → 审计日志。
//!
//! UI、AI 编排插件、MCP 客户端三条路径共用 [`ToolInvoker`]，
//! 差别仅在传入的 [`CallerIdentity`]。

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock, RwLock, RwLockReadGuard, Weak};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value as JsonValue};
use tokio::sync::Mutex;

use crate::config::{self, JsonIoError};
use crate::permission::{
    CallerIdentity, GrantRecord, PermissionChecker, PermissionError, PermissionPrompter,
};
use crate::protocol::manifest::{LifecycleMode, Manifest, RestartPolicy, ToolDescriptor};
use crate::protocol::message::JsonRpcError;
use crate::registry::Registry;
use crate::runtime::instance::{HostHandler, InstanceError, InstanceState, PluginInstance};
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

// ─────────────────── 插件通知 ───────────────────

/// 插件经 `host/notify` 主动推来的一条通知。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginNotification {
    /// 发出通知的插件 id。
    pub plugin_id: String,
    /// 通知方法名，如 `notify` / `stream`。
    pub method: String,
    /// 原样透出的通知负载。
    pub params: JsonValue,
}

/// 插件通知的落点。生产实现向前端 emit Tauri 事件，测试实现收集到内存里断言。
///
/// 与 [`AuditSink`] 同样做成 trait，好处是 `Supervisor` 不必依赖 `tauri::AppHandle`，
/// 内核仍可脱离 GUI 单测。
pub trait NotificationSink: Send + Sync {
    fn emit(&self, notification: PluginNotification);
}

/// 默认实现：只写 tracing。未接前端时的行为与 Phase 6 保持一致。
#[derive(Debug, Default, Clone, Copy)]
pub struct TracingNotificationSink;

impl NotificationSink for TracingNotificationSink {
    fn emit(&self, notification: PluginNotification) {
        tracing::info!(
            plugin = %notification.plugin_id,
            method = %notification.method,
            params = %summarize_args(&notification.params),
            "插件通知"
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

/// 单个插件实例的运行状况快照，供 UI 展示。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstanceStatus {
    pub state: InstanceState,
    /// 最近一次错误，用于在列表里给出失败原因。
    pub last_error: Option<String>,
    /// 在途请求数。
    pub inflight: u64,
    /// 已累积的自动重启次数。
    pub restart_attempts: u32,
}

/// 某插件的落盘授权集合：权限字符串 → 授权记录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginGrants {
    pub plugin_id: String,
    pub grants: BTreeMap<String, GrantRecord>,
}

/// 一次注册表热替换的前后差异，供「重载插件」向用户汇报。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistryDiff {
    /// 本次新出现的插件 id。
    pub added: Vec<String>,
    /// 本次消失的插件 id（目录被删或 manifest 变得无法解析）。
    pub removed: Vec<String>,
    /// 替换后的插件总数。
    pub total: usize,
}

// ─────────────────── 调度器 ───────────────────

/// 插件调度器。持有注册表、权限判定器与全部活跃实例。
pub struct Supervisor<P: PermissionPrompter> {
    /// 注册表可热替换，所以包了一层读写锁。
    ///
    /// 用 `std::sync::RwLock` 而非 `tokio::sync::RwLock` 有两个原因：
    /// 一是 [`crate::gateway::supervisor_table_source`] 要在一个**同步** `Fn`
    /// 闭包里读注册表，`.await` 在那里根本写不出来；二是 `std` 的读守卫不是
    /// `Send`，谁想跨 `.await` 持有它都会被编译器当场拒掉——这正是我们想要的
    /// 约束，逼着每条 async 路径先把数据克隆出来再等待。
    registry: RwLock<Registry>,
    /// 被用户禁用的插件 id 集合。与 `HostConfig.disabled_plugins` 同步。
    /// 用 `std::sync::RwLock` 与注册表同样的理由：同步访问、守卫不跨 `.await`。
    disabled: RwLock<BTreeSet<String>>,
    checker: Mutex<PermissionChecker<P>>,
    factory: Arc<dyn TransportFactory>,
    audit: Arc<dyn AuditSink>,
    notifier: Arc<dyn NotificationSink>,
    instances: Mutex<BTreeMap<String, ManagedInstance>>,
    cache: Mutex<ToolCache>,
    cache_path: Option<PathBuf>,
    logs_dir: Option<PathBuf>,
    plugin_configs_dir: Option<PathBuf>,
    self_ref: OnceLock<Weak<dyn HostHandler>>,
}

impl<P: PermissionPrompter> Supervisor<P> {
    pub fn new(
        registry: Registry,
        checker: PermissionChecker<P>,
        factory: Arc<dyn TransportFactory>,
    ) -> Self {
        Self {
            registry: RwLock::new(registry),
            disabled: RwLock::new(BTreeSet::new()),
            checker: Mutex::new(checker),
            factory,
            audit: Arc::new(TracingAuditSink),
            notifier: Arc::new(TracingNotificationSink),
            instances: Mutex::new(BTreeMap::new()),
            cache: Mutex::new(ToolCache::default()),
            cache_path: None,
            logs_dir: None,
            plugin_configs_dir: None,
            self_ref: OnceLock::new(),
        }
    }

    /// 指定每插件配置目录。不设置则回退到 `~/.intools/plugin-configs`。
    ///
    /// 与 [`LaunchSpec::stderr_log`] 同样的理由：测试不能往真实家目录写文件。
    pub fn with_plugin_configs_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.plugin_configs_dir = Some(dir.into());
        self
    }

    /// 把自身登记为反向 RPC 的宿主回调。
    ///
    /// 必须单独一步、且在 `Arc::new(supervisor)` 之后调用：`Weak` 只能由已存在
    /// 的 `Arc` 降级而来，而 `spawn_and_handshake` 只拿得到 `&self`。用 `Weak`
    /// 而非 `Arc` 是因为 supervisor 持有实例、实例又要回指 supervisor，
    /// 直接互持会形成谁也放不掉谁的循环引用。
    ///
    /// 未调用本方法时插件的 `host/*` 请求会得到 `CODE_INTERNAL_ERROR`，
    /// 不影响正向调用——这让绝大多数单测无需构造 `Arc`。
    pub fn install_self_ref(self: &Arc<Self>)
    where
        P: 'static,
    {
        let weak: Weak<dyn HostHandler> = Arc::downgrade(self) as Weak<dyn HostHandler>;
        let _ = self.self_ref.set(weak);
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

    /// 指定插件通知的落点。不设置则只写 tracing。
    pub fn with_notification_sink(mut self, sink: Arc<dyn NotificationSink>) -> Self {
        self.notifier = sink;
        self
    }

    /// 设置初始禁用列表。`main.rs` 从 `HostConfig.disabled_plugins` 读出后传入。
    pub fn with_disabled_plugins(mut self, ids: impl IntoIterator<Item = String>) -> Self {
        self.disabled = RwLock::new(ids.into_iter().collect());
        self
    }

    /// 借出注册表的读守卫。
    ///
    /// 返回守卫而非 `&Registry`，是为了让「跨 `.await` 持有注册表」这件错事
    /// 变成编译错误（`std` 读守卫不是 `Send`）。同步调用点因为 `Deref`
    /// 基本可以原样写：`supervisor.registry().list_plugins_sorted()`。
    pub fn registry(&self) -> RwLockReadGuard<'_, Registry> {
        self.registry.read().unwrap_or_else(|e| e.into_inner())
    }

    /// 用重新扫描出的注册表整体替换旧的，返回本次变动。
    ///
    /// 「重载插件」的落点。整体替换而非逐个 `insert_loaded`，是因为只有整体
    /// 替换才能表达「插件被删掉了」——增量合入永远只增不减。
    ///
    /// 替换后会做两件收尾：把消失插件的工具缓存删掉（否则 `list_tools`
    /// 还会从缓存里吐出已经不存在的工具），以及停掉它们仍在跑的进程
    /// （注册表里没了却还留着进程，等于泄漏一个再也管不到的子进程）。
    pub async fn replace_registry(&self, next: Registry) -> RegistryDiff {
        let diff = {
            // 写守卫也不是 Send，因此这个块必须在任何 .await 之前闭合。
            let mut guard = self.registry.write().unwrap_or_else(|e| e.into_inner());
            let before: std::collections::BTreeSet<String> =
                guard.plugin_ids().into_iter().map(str::to_string).collect();
            let after: std::collections::BTreeSet<String> =
                next.plugin_ids().into_iter().map(str::to_string).collect();
            *guard = next;
            RegistryDiff {
                added: after.difference(&before).cloned().collect(),
                removed: before.difference(&after).cloned().collect(),
                total: after.len(),
            }
        };

        if !diff.removed.is_empty() {
            {
                let mut cache = self.cache.lock().await;
                for id in &diff.removed {
                    cache.remove(id);
                }
                if let Some(path) = &self.cache_path {
                    if let Err(err) = cache.save(path) {
                        tracing::warn!(error = %err, "重载后写入工具缓存失败");
                    }
                }
            }
            for id in &diff.removed {
                let managed = self.instances.lock().await.remove(id);
                if let Some(managed) = managed {
                    tracing::info!(plugin = %id, "插件已从注册表消失，回收其进程");
                    managed.instance.stop().await;
                }
            }
        }

        diff
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

    /// 供 UI 展示的实例明细：状态、最近一次错误、在途请求数。
    pub async fn instance_status(&self, plugin_id: &str) -> Option<InstanceStatus> {
        self.instances.lock().await.get(plugin_id).map(|m| {
            let instance = &m.instance;
            InstanceStatus {
                state: instance.state(),
                last_error: instance.last_error(),
                inflight: instance.inflight(),
                restart_attempts: m.restart_attempts,
            }
        })
    }

    /// 该插件是否被用户禁用。同步读、无副作用。
    pub fn is_disabled(&self, plugin_id: &str) -> bool {
        self.disabled.read().unwrap_or_else(|e| e.into_inner()).contains(plugin_id)
    }

    /// 当前被禁用的插件 id 列表（按字典序）。供前端渲染。
    pub fn disabled_plugins(&self) -> Vec<String> {
        self.disabled
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .cloned()
            .collect()
    }

    /// 设置插件的启用 / 禁用态。返回操作是否真的改变了状态（无变化 = false）。
    ///
    /// 副作用：如果是从启用→禁用且插件此刻正在跑，会顺手把进程停了——否则
    /// 「禁用」只是个标志位，下一次按需调用还会被拉起来，跟用户的期待不符。
    /// 反方向（禁用→启用）不主动拉起，按需唤醒路径自会处理。
    pub async fn set_plugin_enabled(&self, plugin_id: &str, enabled: bool) -> bool {
        let changed = {
            let mut guard = self.disabled.write().unwrap_or_else(|e| e.into_inner());
            let before_disabled = guard.contains(plugin_id);
            // `enabled: true` ⇔ 不在禁用集 ⇔ `!before_disabled`。
            // 状态真发生变化 ⇔ 「之前禁用？ ≠ 现在禁用？」。
            let now_disabled = !enabled;
            if now_disabled { guard.insert(plugin_id.to_string()); } else { guard.remove(plugin_id); }
            before_disabled != now_disabled
        };

        if changed && !enabled {
            // 禁用且原本在跑：回收进程。
            let managed = self.instances.lock().await.remove(plugin_id);
            if let Some(managed) = managed {
                tracing::info!(plugin = %plugin_id, "插件被禁用，回收其进程");
                managed.instance.stop().await;
            }
            // 工具缓存里它的描述也该清掉，否则 list_tools 会吐出禁用插件的工具。
            self.cache.lock().await.remove(plugin_id);
        }

        changed
    }

    /// 手动拉起单个插件（插件列表页的「启动」）。已在运行时为幂等空操作。
    pub async fn start_plugin(&self, plugin_id: &str) -> Result<(), InvokeError> {
        // 手动启动视为一次全新的尝试，清掉此前累积的重启计数，
        // 否则崩溃 5 次后用户再也无法从界面把插件拉起来。
        if let Some(managed) = self.instances.lock().await.get_mut(plugin_id) {
            managed.restart_attempts = 0;
        }
        self.ensure_running(plugin_id).await.map(|_| ())
    }

    /// 手动停止单个插件（插件列表页的「停止」）。
    ///
    /// 未在运行时返回 `Ok`——目标状态已达成，不该报错。停止后从实例表移除，
    /// 下次调用会按 `on-demand` 语义重新唤醒。
    pub async fn stop_plugin(&self, plugin_id: &str) -> Result<(), InvokeError> {
        if self.registry().get_plugin(plugin_id).is_none() {
            return Err(InvokeError::PluginNotFound {
                plugin_id: plugin_id.to_string(),
            });
        }
        let managed = self.instances.lock().await.remove(plugin_id);
        if let Some(managed) = managed {
            managed.instance.stop().await;
        }
        Ok(())
    }

    /// 撤销某插件的全部授权（权限页的「撤销」）。
    ///
    /// 落盘与会话记录一并清除，因此下次调用会重新弹窗询问。
    pub async fn revoke_plugin_grants(&self, plugin_id: &str) -> Result<(), PermissionError> {
        self.checker
            .lock()
            .await
            .store_mut()
            .revoke_plugin(plugin_id)
    }

    /// 列出所有插件的落盘授权，供权限页展示。
    ///
    /// 只含 `Always` 记录：会话授权随进程消失，展示出来会误导用户以为可撤销。
    pub async fn list_persisted_grants(&self) -> Vec<PluginGrants> {
        let checker = self.checker.lock().await;
        let store = checker.store();
        self.registry()
            .list_plugins_sorted()
            .into_iter()
            .filter_map(|plugin| {
                let id = plugin.id();
                let grants = store.persisted_grants(id)?;
                if grants.is_empty() {
                    return None;
                }
                Some(PluginGrants {
                    plugin_id: id.to_string(),
                    grants: grants
                        .iter()
                        .map(|(perm, record)| (perm.clone(), record.clone()))
                        .collect(),
                })
            })
            .collect()
    }

    /// 清空全部会话授权。高危权限「每会话首次询问」依赖启动时调用这里。
    pub async fn clear_session_grants(&self) {
        self.checker.lock().await.store_mut().clear_session();
    }

    /// 启动所有 `startup` / `background` 模式的插件。
    pub async fn start_eager_plugins(&self) -> Vec<(String, InvokeError)> {
        let eager: Vec<String> = self
            .registry()
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
            // 禁用插件不在启动时拉起——这里不能用 `ensure_running` 内部的拦
            // 截来吞掉，否则「启动时拉起 N 个 startup 插件」和「其中 M 个被
            // 禁用」混在一起会污染外部对失败列表的判断。
            if self.is_disabled(&id) {
                continue;
            }
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
        // 禁用插件不响应按需唤醒——这是「禁用」语义的核心：它不应该在被任何
        // 路径拉起来，工具列表和 MCP 暴露也会被一并过滤（见 `list_tools` /
        // `McpToolTable::build` 处的拦截）。
        if self.is_disabled(plugin_id) {
            return Err(InvokeError::PluginDisabled {
                plugin_id: plugin_id.to_string(),
            });
        }
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
        // 启动所需的一切都在这个块里从注册表拷出来。
        //
        // 不能像从前那样握着 `&RegisteredPlugin` 走完全程：注册表现在是
        // 可热替换的，读守卫跨不过下面的 `.await`（也不该跨——重载正好卡在
        // 这中间的话，后半程用的就是一份已被替换掉的旧数据）。
        let (spec, lifecycle) = {
            let registry = self.registry();
            let plugin =
                registry
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
            (spec, plugin.manifest.lifecycle.clone())
        };
        let plugin_dir = spec.plugin_dir.display().to_string();

        let transport = self.factory.create(&spec).await?;
        let instance = Arc::new(PluginInstance::new(
            plugin_id.to_string(),
            lifecycle,
            transport,
        ));

        // 必须在 start() 之前注入：握手一完成插件就可能立刻回调 host/*，
        // 晚一步注入会让最早的那批反向请求撞上「宿主回调不可用」。
        if let Some(weak) = self.self_ref.get() {
            instance.set_host(weak.clone());
        }

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
            .registry()
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
        // 先把注册表里要用的部分拷成 owned，再去拿 cache 锁：
        // 读守卫不是 Send，握着它 await 是编译错误。
        let disabled = self.disabled_plugins();
        let disabled: std::collections::HashSet<String> = disabled.into_iter().collect();
        let declared: Vec<(String, Vec<ToolDescriptor>)> = self
            .registry()
            .list_plugins_sorted()
            .into_iter()
            .filter(|p| !disabled.contains(p.id()))
            .map(|p| (p.id().to_string(), p.manifest.tools.clone()))
            .collect();

        let cache = self.cache.lock().await;
        let mut out = Vec::new();
        for (id, manifest_tools) in declared {
            match cache.get(&id) {
                Some(tools) => out.extend(tools.iter().map(|t| (id.clone(), t.clone()))),
                None => out.extend(manifest_tools.into_iter().map(|t| (id.clone(), t))),
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
        let plugin_id = self
            .registry()
            .resolve_tool(tool_name)
            .map(|p| p.id().to_string())
            .unwrap_or_default();
        let outcome = match &result {
            Ok(_) => "ok".to_string(),
            Err(err) => format!("error: {err}"),
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
    /// 返回工具名对应的插件 ID（用于设置合并等）。
    ///
    /// 返回 owned `String` 而非 `&str`：注册表在锁后面，借用只能活到守卫
    /// 被丢弃为止，没法交给调用方。
    pub fn plugin_id_for_tool(&self, tool_name: &str) -> Option<String> {
        self.registry()
            .resolve_tool(tool_name)
            .map(|p| p.id().to_string())
    }

    /// 列出所有插件 ID 及其 manifest 副本（同步，不唤醒插件）。
    pub fn list_plugins_with_manifests(&self) -> Vec<(String, Manifest)> {
        self.registry()
            .list_plugins_sorted()
            .into_iter()
            .map(|p| (p.id().to_string(), p.manifest.clone()))
            .collect()
    }

    /// 返回插件的安装目录。
    ///
    /// 给「读取插件自带说明文档」用：manifest 里的 `docs.usage_file` 是**纯文件名**，
    /// 必须拼上这个目录才知道文件在哪。之所以不把绝对路径直接放进 manifest，
    /// 是因为插件包会被复制到 `~/.intools/plugins/` 下，作者写死路径必然失效。
    pub fn plugin_dir(&self, plugin_id: &str) -> Option<PathBuf> {
        self.registry()
            .get_plugin(plugin_id)
            .map(|p| p.plugin_dir.clone())
    }

    /// 返回单个插件的 manifest 副本（同步，不唤醒插件）。
    pub fn manifest_of(&self, plugin_id: &str) -> Option<Manifest> {
        self.registry()
            .get_plugin(plugin_id)
            .map(|p| p.manifest.clone())
    }

    async fn call_tool_inner(
        &self,
        tool_name: &str,
        args: JsonValue,
        caller: &CallerIdentity,
    ) -> Result<JsonValue, InvokeError> {
        // 第一步：工具名解析。权限清单当场拷出来，守卫过不了下面的 await。
        let (plugin_id, declared) = {
            let registry = self.registry();
            let plugin =
                registry
                    .resolve_tool(tool_name)
                    .ok_or_else(|| InvokeError::ToolNotFound {
                        tool: tool_name.to_string(),
                    })?;
            (
                plugin.id().to_string(),
                plugin.manifest.capabilities.permissions.clone(),
            )
        };

        // 第二步：权限校验（按调用方身份）。
        // 把工具名和参数摘要透传给弹窗——危险操作的弹窗需要显示「这次到底要干什么」
        // 才能让用户判断该不该放行，光给个「插件 X 想调用危险权限」毫无判断依据。
        let args_summary = summarize_args(&args);
        self.checker
            .lock()
            .await
            .check_all(&plugin_id, &declared, caller, Some(tool_name), Some(&args_summary))
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
        //
        // 把调用方深度一并交给实例：若被调插件又回调 host/callTool，
        // 实例会以这个深度为基准继续累加，depth 才不会在链路上被重置为 0。
        let result = instance
            .call_at_depth(
                "tools/call",
                json!({ "name": tool_name, "arguments": args }),
                caller.depth(),
            )
            .await?;

        Ok(result)
    }
}

// ─────────────────── 反向 RPC：宿主侧实现 ───────────────────

impl<P: PermissionPrompter> Supervisor<P> {
    /// 解析某插件的配置文件路径。
    ///
    /// 消毒交给 `plugin_config_json_in` 内部的 `check_plugin_id`，而不是在这里
    /// 另写一遍——plugin_id 来自插件自报的身份，若能拼出 `../` 就等于把整个
    /// 数据目录交了出去，这条校验必须与生产路径共用同一份实现。
    fn plugin_config_path(&self, plugin_id: &str) -> Result<PathBuf, config::PathError> {
        match &self.plugin_configs_dir {
            Some(dir) => config::paths::plugin_config_json_in(dir, plugin_id),
            None => config::paths::plugin_config_json(plugin_id),
        }
    }
}

#[async_trait::async_trait]
impl<P: PermissionPrompter + 'static> HostHandler for Supervisor<P> {
    async fn host_list_tools(&self) -> Result<JsonValue, JsonRpcError> {
        let tools: Vec<JsonValue> = ToolInvoker::list_tools(self)
            .await
            .into_iter()
            .map(|(plugin_id, desc)| {
                json!({
                    "plugin_id": plugin_id,
                    "name": desc.name,
                    "description": desc.description,
                    "input_schema": desc.input_schema,
                })
            })
            .collect();
        Ok(json!({ "tools": tools }))
    }

    /// 插件发起的工具调用。
    ///
    /// 关键在于这里**不走任何捷径**：构造 `CallerIdentity::Plugin` 后原样走
    /// [`ToolInvoker::call_tool`]，六步链一步不少。设计文档 §5.3 的要求正是
    /// 如此——否则 AI 编排插件就成了绕开权限系统的万能后门。
    ///
    /// `depth + 1` 由宿主自己算：`depth` 是实例侧记录的在途入站深度，
    /// 插件无从伪造。
    async fn host_call_tool(
        &self,
        caller_plugin: &str,
        depth: u8,
        tool: &str,
        args: JsonValue,
    ) -> Result<JsonValue, JsonRpcError> {
        let caller = CallerIdentity::Plugin {
            id: caller_plugin.to_string(),
            depth: depth.saturating_add(1),
        };
        ToolInvoker::call_tool(self, tool, args, caller)
            .await
            .map_err(|err| err.to_rpc_error())
    }

    async fn host_get_config(&self, caller_plugin: &str) -> Result<JsonValue, JsonRpcError> {
        let path = self
            .plugin_config_path(caller_plugin)
            .map_err(path_error_to_rpc)?;
        // 没有配置文件视作「还没配过」，返回空对象而非报错，插件才好写默认值兜底。
        let value: Option<JsonValue> = config::read_json(&path).map_err(json_io_error_to_rpc)?;
        Ok(value.unwrap_or_else(|| json!({})))
    }

    async fn host_set_config(
        &self,
        caller_plugin: &str,
        value: JsonValue,
    ) -> Result<JsonValue, JsonRpcError> {
        let path = self
            .plugin_config_path(caller_plugin)
            .map_err(path_error_to_rpc)?;
        config::write_json(&path, &value).map_err(json_io_error_to_rpc)?;
        Ok(json!({ "ok": true }))
    }

    /// 转交给 [`NotificationSink`]，由其决定是写日志还是 emit 到前端。
    async fn host_notify(&self, caller_plugin: &str, method: &str, params: JsonValue) {
        self.notifier.emit(PluginNotification {
            plugin_id: caller_plugin.to_string(),
            method: method.to_string(),
            params,
        });
    }
}

fn path_error_to_rpc(err: config::PathError) -> JsonRpcError {
    // 非法 plugin_id 属于参数问题，其余（无家目录、IO 失败）是宿主自身故障。
    let code = match err {
        config::PathError::InvalidPluginId(_) => JsonRpcError::CODE_INVALID_PARAMS,
        _ => JsonRpcError::CODE_INTERNAL_ERROR,
    };
    JsonRpcError::new(code, err.to_string())
}

fn json_io_error_to_rpc(err: JsonIoError) -> JsonRpcError {
    match err {
        JsonIoError::Path(inner) => path_error_to_rpc(inner),
        other => JsonRpcError::new(JsonRpcError::CODE_INTERNAL_ERROR, other.to_string()),
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

    #[error("插件 `{plugin_id}` 已被禁用")]
    PluginDisabled { plugin_id: String },
}

impl InvokeError {
    /// 是否因权限被拒。UI 与 MCP 据此区分「不允许」与「出故障」。
    pub fn is_permission_denied(&self) -> bool {
        matches!(self, InvokeError::Permission(e) if e.is_denied())
    }

    /// 转成回给插件的 JSON-RPC 错误。
    ///
    /// 码位分得细是为了让插件能程序化地判断该重试还是该放弃：深度超限和权限
    /// 被拒都属于「再试也没用」，找不到工具则可能是拼错了名字。
    pub fn to_rpc_error(&self) -> JsonRpcError {
        let code = match self {
            InvokeError::ToolNotFound { .. } | InvokeError::PluginNotFound { .. } => {
                JsonRpcError::CODE_TOOL_NOT_FOUND
            }
            InvokeError::DepthExceeded { .. } => JsonRpcError::CODE_CALL_DEPTH,
            InvokeError::Permission(_) => JsonRpcError::CODE_PERMISSION_DENIED,
            InvokeError::Instance(InstanceError::RequestTimeout { .. })
            | InvokeError::Instance(InstanceError::HandshakeTimeout { .. }) => {
                JsonRpcError::CODE_TIMEOUT
            }
            InvokeError::Instance(_) | InvokeError::Process(_) => JsonRpcError::CODE_PLUGIN_ERROR,
            InvokeError::RestartDeclined { .. } | InvokeError::RestartExhausted { .. } => {
                JsonRpcError::CODE_INTERNAL_ERROR
            }
            InvokeError::PluginDisabled { .. } => JsonRpcError::CODE_TOOL_NOT_FOUND,
        };
        JsonRpcError::new(code, self.to_string())
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
        IncomingMessage, JsonRpcNotification, JsonRpcRequest, JsonRpcResponse, OutgoingMessage,
        RequestId, ResponseBody,
    };
    use crate::registry::discovery::LoadedPlugin;
    use crate::registry::PluginCategory;
    use crate::runtime::instance::{
        HOST_CALL_TOOL, HOST_GET_CONFIG, HOST_LIST_TOOLS, HOST_NOTIFY, HOST_SET_CONFIG,
    };
    use crate::runtime::transport::MockTransport;

    // ─────────── 构造 manifest / registry ───────────

    fn tool(name: &str) -> ToolDescriptor {
        ToolDescriptor {
            name: name.to_string(),
            description: String::new(),
            input_schema: json!({"type":"object","properties":{},"required":[]}),
        }
    }

    fn manifest_of(id: &str, tools: Vec<&str>, perms: Vec<&str>, lifecycle: Lifecycle) -> Manifest {
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
            shortcut: None,
            settings: Vec::new(),
            docs: None,
            result_display: Vec::new(),
        }
    }

    fn registry_of(manifests: Vec<Manifest>) -> Registry {
        let mut reg = Registry::new();
        for m in manifests {
            reg.insert_loaded(LoadedPlugin {
                plugin_dir: PathBuf::from(format!("/plugins/{}", m.plugin.id)),
                manifest: m,
            }, PluginCategory::User);
        }
        reg
    }

    /// 只声明低危权限的单插件注册表，供多数用例复用。
    fn simple_registry() -> Registry {
        registry_of(vec![manifest_of(
            "com.example.demo",
            vec!["demo:echo"],
            vec!["network:http"],
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

        /// `Arc<Self>` → `Arc<dyn TransportFactory>`，喂给 [`Supervisor::new`]
        /// 时需要做一次类型抹除。同一个 factory 实例两份 Arc，共享内部状态。
        fn into_dyn(self: Arc<Self>) -> Arc<dyn TransportFactory> {
            self
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

    fn checker_with(
        dir: &std::path::Path,
        decision: PromptDecision,
    ) -> PermissionChecker<FixedPrompter> {
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

    /// 返回固定 mock 的传输工厂，供 E2E 测试精细控制握手与反向调用。
    struct FixedTransportFactory {
        mock: Arc<MockTransport>,
    }

    #[async_trait::async_trait]
    impl TransportFactory for FixedTransportFactory {
        async fn create(&self, _spec: &LaunchSpec) -> Result<Arc<dyn Transport>, ProcessError> {
            Ok(Arc::clone(&self.mock) as Arc<dyn Transport>)
        }
    }

    /// 只驱动握手的应答者：自报家门并答完 `plugin/ready` 就退出。
    ///
    /// 与 [`spawn_fake_plugin`] 的区别在于「答完就撒手」。那个常驻应答者会把
    /// 出站通道吃干净，反向调用的响应也会被它吞掉；E2E 用例必须自己读到那条
    /// 响应，所以这里只应答一条消息。
    fn drive_handshake(
        mock: Arc<MockTransport>,
        tools: Vec<ToolDescriptor>,
    ) -> tokio::task::JoinHandle<()> {
        mock.push_incoming(IncomingMessage::Notification(JsonRpcNotification::new(
            "plugin/hello",
            json!({
                "protocol_version": "1.0",
                "tools": serde_json::to_value(&tools).unwrap(),
            }),
        )));

        tokio::spawn(async move {
            match mock.next_sent().await {
                Some(OutgoingMessage::Request(req)) => {
                    assert_eq!(req.method, "plugin/ready", "握手第二步应是 plugin/ready");
                    mock.push_incoming(IncomingMessage::Response(JsonRpcResponse::success(
                        req.id,
                        json!({}),
                    )));
                }
                other => panic!("握手期间期望收到请求，实际为 {other:?}"),
            }
        })
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
        assert!(
            matches!(err, InvokeError::ToolNotFound { .. }),
            "得到 {err:?}"
        );
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
            RecordingFactory::into_dyn(Arc::clone(&factory)),
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
            RecordingFactory::into_dyn(Arc::clone(&factory)),
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
            RecordingFactory::into_dyn(Arc::clone(&factory)),
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
            RecordingFactory::into_dyn(Arc::clone(&factory)),
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
            RecordingFactory::into_dyn(Arc::clone(&factory)),
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
        let names: Vec<String> = sup
            .list_tools()
            .await
            .into_iter()
            .map(|(_, t)| t.name)
            .collect();
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

        let names: Vec<String> = sup
            .list_tools()
            .await
            .into_iter()
            .map(|(_, t)| t.name)
            .collect();
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
            RecordingFactory::into_dyn(Arc::clone(&factory)),
        )
        .with_cache_path(&cache_path);

        let names: Vec<String> = sup2
            .list_tools()
            .await
            .into_iter()
            .map(|(_, t)| t.name)
            .collect();
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

        let raw: JsonValue =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
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
            RecordingFactory::into_dyn(Arc::clone(&factory)),
        );

        sup.call_tool("demo:echo", json!({}), CallerIdentity::Ui)
            .await
            .unwrap();
        assert_eq!(factory.create_count(), 1);

        let err = sup
            .restart_after_crash("com.example.demo")
            .await
            .unwrap_err();
        assert!(
            matches!(err, InvokeError::RestartDeclined { .. }),
            "得到 {err:?}"
        );
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
            RecordingFactory::into_dyn(Arc::clone(&factory)),
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
        let err = sup
            .restart_after_crash("com.example.ghost")
            .await
            .unwrap_err();
        assert!(
            matches!(err, InvokeError::PluginNotFound { .. }),
            "得到 {err:?}"
        );
    }

    #[tokio::test]
    async fn 启动失败应作为错误上报而非静默() {
        let dir = tmp_dir("spawn-fail");
        let factory = RecordingFactory::failing_first(vec!["demo:echo"], 1);
        let sup = Supervisor::new(
            simple_registry(),
            checker_with(&dir, PromptDecision::AllowAlways),
            RecordingFactory::into_dyn(Arc::clone(&factory)),
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
        assert!(
            entries[0].outcome.starts_with("error:"),
            "得到 {}",
            entries[0].outcome
        );
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
            RecordingFactory::into_dyn(Arc::clone(&factory)),
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

    // ─────────── 反向 RPC：直接调用 HostHandler ───────────

    #[tokio::test]
    async fn 反向列工具应带上插件归属() {
        let dir = tmp_dir("host-list");
        let sup = Supervisor::new(
            simple_registry(),
            checker_with(&dir, PromptDecision::AllowAlways),
            RecordingFactory::new(vec!["demo:echo"]),
        );

        let out = sup.host_list_tools().await.unwrap();
        let tools = out["tools"].as_array().expect("应返回 tools 数组");

        assert_eq!(tools.len(), 1);
        // 插件拿到的是全局工具表，必须知道每个工具属于谁，否则无从判断调用边界。
        assert_eq!(tools[0]["plugin_id"], "com.example.demo");
        assert_eq!(tools[0]["name"], "demo:echo");
        assert!(tools[0].get("input_schema").is_some(), "应带上入参 schema");
    }

    #[tokio::test]
    async fn 反向调用工具应转发参数并返回插件结果() {
        let dir = tmp_dir("host-call-ok");
        let sup = Supervisor::new(
            simple_registry(),
            checker_with(&dir, PromptDecision::AllowAlways),
            RecordingFactory::new(vec!["demo:echo"]),
        );

        let out = sup
            .host_call_tool("com.example.caller", 0, "demo:echo", json!({"text":"hi"}))
            .await
            .unwrap();

        assert_eq!(out["echo"]["name"], "demo:echo");
        assert_eq!(out["echo"]["arguments"], json!({"text":"hi"}));
    }

    #[tokio::test]
    async fn 反向调用的深度应在实例自报值上加一() {
        let dir = tmp_dir("host-depth-plus1");
        let sup = Supervisor::new(
            simple_registry(),
            checker_with(&dir, PromptDecision::AllowAlways),
            RecordingFactory::new(vec!["demo:echo"]),
        );

        // 实例侧记录的在途深度是 MAX-1，宿主加一后恰好撞上上限。
        let err = sup
            .host_call_tool("com.example.a", MAX_CALL_DEPTH - 1, "demo:echo", json!({}))
            .await
            .unwrap_err();
        assert_eq!(err.code, JsonRpcError::CODE_CALL_DEPTH, "得到 {err:?}");

        // 再少一层就应放行——这一对断言合起来才能证明「恰好加一」，
        // 单看被拒那次无法排除「加二」或「直接取上限」。
        sup.host_call_tool("com.example.a", MAX_CALL_DEPTH - 2, "demo:echo", json!({}))
            .await
            .expect("深度未达上限应放行");
    }

    #[tokio::test]
    async fn 反向调用未知工具应报工具不存在() {
        let dir = tmp_dir("host-call-404");
        let sup = Supervisor::new(
            simple_registry(),
            checker_with(&dir, PromptDecision::AllowAlways),
            RecordingFactory::new(vec!["demo:echo"]),
        );

        let err = sup
            .host_call_tool("com.example.caller", 0, "nope:missing", json!({}))
            .await
            .unwrap_err();

        assert_eq!(err.code, JsonRpcError::CODE_TOOL_NOT_FOUND, "得到 {err:?}");
    }

    #[tokio::test]
    async fn 反向调用权限被拒应报权限错误且不唤醒插件() {
        let dir = tmp_dir("host-call-denied");
        let factory = RecordingFactory::new(vec!["scr:shot"]);
        let sup = Supervisor::new(
            registry_of(vec![manifest_of(
                "com.example.scr",
                vec!["scr:shot"],
                vec!["screen:capture"],
                Lifecycle::default(),
            )]),
            checker_with(&dir, PromptDecision::DenyAlways),
            RecordingFactory::into_dyn(Arc::clone(&factory)),
        );

        let err = sup
            .host_call_tool("com.example.caller", 0, "scr:shot", json!({}))
            .await
            .unwrap_err();

        // 设计文档 §5.3：插件发起的调用不得绕开六步链，
        // 所以这里的表现必须与 UI 发起时完全一致。
        assert_eq!(
            err.code,
            JsonRpcError::CODE_PERMISSION_DENIED,
            "得到 {err:?}"
        );
        assert_eq!(factory.create_count(), 0, "被拒的反向调用不应唤醒插件");
        assert!(sup.live_plugin_ids().await.is_empty());
    }

    // ─────────── 反向 RPC：每插件配置 ───────────

    #[tokio::test]
    async fn 未配置过的插件读配置应得空对象() {
        let dir = tmp_dir("host-cfg-empty");
        let sup = Supervisor::new(
            simple_registry(),
            checker_with(&dir, PromptDecision::AllowAlways),
            RecordingFactory::new(vec!["demo:echo"]),
        )
        .with_plugin_configs_dir(dir.join("configs"));

        let out = sup.host_get_config("com.example.demo").await.unwrap();
        // 返回空对象而非报错，插件才好用默认值兜底。
        assert_eq!(out, json!({}));
    }

    #[tokio::test]
    async fn 写入配置后应能原样读回并落到以插件id命名的文件() {
        let dir = tmp_dir("host-cfg-rw");
        let sup = Supervisor::new(
            simple_registry(),
            checker_with(&dir, PromptDecision::AllowAlways),
            RecordingFactory::new(vec!["demo:echo"]),
        )
        .with_plugin_configs_dir(dir.join("configs"));

        let ack = sup
            .host_set_config("com.example.demo", json!({"lang":"zh","retries":3}))
            .await
            .unwrap();
        assert_eq!(ack, json!({"ok": true}));

        let out = sup.host_get_config("com.example.demo").await.unwrap();
        assert_eq!(out, json!({"lang":"zh","retries":3}));

        assert!(
            dir.join("configs").join("com.example.demo.json").exists(),
            "配置应按插件 id 分文件存放，互不串扰"
        );
    }

    #[tokio::test]
    async fn 非法插件id的配置读写应报参数错误且不落盘() {
        let dir = tmp_dir("host-cfg-evil");
        let configs = dir.join("configs");
        let sup = Supervisor::new(
            simple_registry(),
            checker_with(&dir, PromptDecision::AllowAlways),
            RecordingFactory::new(vec!["demo:echo"]),
        )
        .with_plugin_configs_dir(&configs);

        // plugin_id 直接参与拼路径，含分隔符就能穿越到配置目录之外。
        let err = sup.host_get_config("../evil").await.unwrap_err();
        assert_eq!(err.code, JsonRpcError::CODE_INVALID_PARAMS, "得到 {err:?}");

        let err = sup
            .host_set_config("../evil", json!({"pwned":true}))
            .await
            .unwrap_err();
        assert_eq!(err.code, JsonRpcError::CODE_INVALID_PARAMS, "得到 {err:?}");

        // 消毒必须发生在落盘之前，否则拦得再准也已经写出去了。
        let leaked = std::fs::read_dir(&configs)
            .map(|it| it.count())
            .unwrap_or(0);
        assert_eq!(leaked, 0, "被拒的写入不该留下任何文件");
    }

    #[tokio::test]
    async fn 插件通知只记日志不影响后续调用() {
        let dir = tmp_dir("host-notify");
        let sup = Supervisor::new(
            simple_registry(),
            checker_with(&dir, PromptDecision::AllowAlways),
            RecordingFactory::new(vec!["demo:echo"]),
        );

        // Phase 6 的 host/notify 是只进不出的单向口，唯一要求是别把宿主搞崩。
        sup.host_notify("com.example.demo", "progress", json!({"pct": 50}))
            .await;
        sup.host_notify("com.example.demo", "progress", JsonValue::Null)
            .await;

        sup.host_call_tool("com.example.demo", 0, "demo:echo", json!({}))
            .await
            .expect("通知之后宿主应照常工作");
    }

    // ─────────── 反向 RPC：经 MockTransport 的端到端闭环 ───────────

    /// 组装 E2E 夹具：测试独占 mock 通道，因而能亲自读到宿主写回的响应。
    fn e2e_fixture(dir: &std::path::Path) -> (Arc<Supervisor<FixedPrompter>>, Arc<MockTransport>) {
        let mock = Arc::new(MockTransport::new());
        let factory = Arc::new(FixedTransportFactory {
            mock: Arc::clone(&mock),
        });
        let sup = Arc::new(
            Supervisor::new(
                simple_registry(),
                checker_with(dir, PromptDecision::AllowAlways),
                factory as Arc<dyn TransportFactory>,
            )
            .with_plugin_configs_dir(dir.join("configs")),
        );
        (sup, mock)
    }

    #[tokio::test]
    async fn 握手后插件可经反向rpc访问宿主() {
        let dir = tmp_dir("e2e-host-ok");
        let (sup, mock) = e2e_fixture(&dir);
        sup.install_self_ref();

        let handshake = drive_handshake(Arc::clone(&mock), vec![tool("demo:echo")]);
        sup.ensure_running("com.example.demo")
            .await
            .expect("握手应成功");
        handshake.await.expect("握手应答者不应 panic");

        let tools = expect_success(reverse_call(&mock, HOST_LIST_TOOLS, json!({})).await);
        assert_eq!(tools["tools"][0]["name"], "demo:echo");

        let ack =
            expect_success(reverse_call(&mock, HOST_SET_CONFIG, json!({"theme":"dark"})).await);
        assert_eq!(ack, json!({"ok": true}));

        // setConfig 把整个 params 当作配置值写入，读回来应当一模一样。
        let cfg = expect_success(reverse_call(&mock, HOST_GET_CONFIG, json!({})).await);
        assert_eq!(cfg, json!({"theme":"dark"}));

        let acked = expect_success(
            reverse_call(
                &mock,
                HOST_NOTIFY,
                json!({"method":"progress","params":{"pct":1}}),
            )
            .await,
        );
        assert_eq!(acked, json!({}));
    }

    #[tokio::test]
    async fn 反向调用缺少工具名应报参数错误() {
        let dir = tmp_dir("e2e-host-badparam");
        let (sup, mock) = e2e_fixture(&dir);
        sup.install_self_ref();

        let handshake = drive_handshake(Arc::clone(&mock), vec![tool("demo:echo")]);
        sup.ensure_running("com.example.demo")
            .await
            .expect("握手应成功");
        handshake.await.expect("握手应答者不应 panic");

        // 参数校验在转发之前，所以这条请求根本到不了调用链，无需假插件应答。
        let err = expect_error(reverse_call(&mock, HOST_CALL_TOOL, json!({"arguments":{}})).await);
        assert_eq!(err.code, JsonRpcError::CODE_INVALID_PARAMS, "得到 {err:?}");
    }

    #[tokio::test]
    async fn 未安装自引用时反向调用应报宿主不可用() {
        let dir = tmp_dir("e2e-host-noref");
        let (sup, mock) = e2e_fixture(&dir);
        // 故意不调用 install_self_ref。

        let handshake = drive_handshake(Arc::clone(&mock), vec![tool("demo:echo")]);
        sup.ensure_running("com.example.demo")
            .await
            .expect("正向握手不依赖宿主回调，应照常成功");
        handshake.await.expect("握手应答者不应 panic");

        // 没有 self_ref 就没有 HostHandler，反向请求只能报内部错误。
        // 这也反证了 spawn_and_handshake 里那次注入不是可有可无的装饰。
        let err = expect_error(reverse_call(&mock, HOST_LIST_TOOLS, json!({})).await);
        assert_eq!(err.code, JsonRpcError::CODE_INTERNAL_ERROR, "得到 {err:?}");
    }

    // ─────────── 启用 / 禁用 ───────────

    /// 禁用后再 `ensure_running` 应该立刻报错，而不是去启动进程。
    ///
    /// 这是「禁用」语义的核心保证：只要进了禁用集，任何按需路径都不该把插件
    /// 拉起来——否则禁用的视觉反馈就跟实际行为不一致。
    #[tokio::test]
    async fn 禁用后ensure_running应拒绝启动() {
        let dir = tmp_dir("disabled-ensure");
        let factory = RecordingFactory::new(vec!["demo:echo"]);
        let sup = Supervisor::new(
            simple_registry(),
            checker_with(&dir, PromptDecision::AllowAlways),
            RecordingFactory::into_dyn(Arc::clone(&factory)),
        );
        sup.set_plugin_enabled("com.example.demo", false).await;

        let err = sup
            .ensure_running("com.example.demo")
            .await
            .unwrap_err();
        assert!(
            matches!(err, InvokeError::PluginDisabled { .. }),
            "得到 {err:?}"
        );
        // 工厂没被调用 = 没有任何进程被拉起。
        assert_eq!(factory.create_count(), 0, "禁用插件不应拉起进程");
    }

    /// 启动时拉起 startup 插件，禁用的应被跳过。
    #[tokio::test]
    async fn start_eager_plugins应跳过禁用项() {
        use crate::protocol::manifest::Lifecycle;
        let dir = tmp_dir("disabled-eager");
        let startup = Lifecycle {
            mode: crate::protocol::manifest::LifecycleMode::Startup,
            ..Lifecycle::default()
        };
        let reg = registry_of(vec![manifest_of(
            "com.example.startup",
            vec!["demo:echo"],
            vec!["network:http"],
            startup,
        )]);
        let factory = RecordingFactory::new(vec!["demo:echo"]);
        let sup = Supervisor::new(
            reg,
            checker_with(&dir, PromptDecision::AllowAlways),
            RecordingFactory::into_dyn(Arc::clone(&factory)),
        );
        sup.set_plugin_enabled("com.example.startup", false).await;

        let failures = sup.start_eager_plugins().await;
        assert!(failures.is_empty(), "禁用项不应出现在失败列表里：{failures:?}");
        assert_eq!(
            factory.create_count(),
            0,
            "禁用插件不该被工厂创建"
        );
    }

    /// 禁用时若插件已在跑，进程会被回收（这是 set_plugin_enabled 的副作用）。
    #[tokio::test]
    async fn 禁用正在运行的插件应回收进程() {
        let dir = tmp_dir("disabled-running");
        let factory = RecordingFactory::new(vec!["demo:echo"]);
        let sup = Supervisor::new(
            simple_registry(),
            checker_with(&dir, PromptDecision::AllowAlways),
            RecordingFactory::into_dyn(Arc::clone(&factory)),
        );
        sup.ensure_running("com.example.demo")
            .await
            .expect("启用态应能启动");
        assert_eq!(factory.create_count(), 1);

        let changed = sup.set_plugin_enabled("com.example.demo", false).await;
        assert!(changed, "从启用到禁用应报告状态变化");
        // 实例应已从表里清掉。
        assert!(
            sup.live_plugin_ids().await.is_empty(),
            "禁用后实例表应为空"
        );
    }

    /// list_tools 应过滤掉禁用插件的工具——这是 `host/listTools` 与
    /// MCP `tools/list` 的共同约束，文档和 B3 之前已承诺但未兑现。
    #[tokio::test]
    async fn list_tools应过滤禁用插件() {
        let dir = tmp_dir("disabled-tools");
        let sup = Supervisor::new(
            simple_registry(),
            checker_with(&dir, PromptDecision::AllowAlways),
            RecordingFactory::new(vec!["demo:echo"]),
        );
        let before: Vec<_> = sup
            .list_tools()
            .await
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        assert!(
            before.contains(&"com.example.demo".to_string()),
            "启用时应有 demo 工具：{before:?}"
        );

        sup.set_plugin_enabled("com.example.demo", false).await;
        let after: Vec<_> = sup
            .list_tools()
            .await
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        assert!(
            !after.contains(&"com.example.demo".to_string()),
            "禁用后 demo 工具应消失：{after:?}"
        );
    }
}
