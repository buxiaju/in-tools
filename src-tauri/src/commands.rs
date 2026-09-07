//! Tauri command 层：前端与内核之间的唯一桥梁。
//!
//! 三条贯穿本模块的约定：
//!
//! 1. **工具调用一律走 [`ToolInvoker::call_tool`] 并署名 [`CallerIdentity::Ui`]**。
//!    界面不得绕过它直连 `PluginInstance`，否则权限校验与审计就被跳过了。
//! 2. **错误统一压成 `String`**。Tauri 要求 `Err` 可序列化，而内核错误
//!    （`InvokeError` / `PermissionError`）刻意没派生 serde；`Display` 实现已经
//!    是给人看的中文，直接用它。
//! 3. **跨 IPC 的类型另建扁平 DTO**，不给内核类型硬加 `Serialize`。内核类型的
//!    形状应当服务于内核逻辑，而不是被前端的展示需求牵着走。

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value as JsonValue};
use tauri::State;

use intools::config::{self, paths, CloseBehavior, HostConfig, LogLevel, McpExposure, UserShortcuts};
use intools::mcp::audit::{read_audit_log, McpAuditRecord};
use intools::mcp::server;
use intools::permission::{CallerIdentity, PromptDecision};
use intools::registry::{discovery, import, Registry};
use intools::runtime::instance::InstanceState;
use intools::runtime::supervisor::{Supervisor, ToolInvoker};
use intools::shortcut;

use crate::gateway::{self, McpGateway};
use crate::hotkey::ShortcutManager;
use crate::ui::{SharedPrompter, UiPrompter};

/// 界面侧共享状态。
///
/// `Supervisor` 的泛型参数被钉死成 [`SharedPrompter`]（即 `Arc<UiPrompter>`）：
/// 生产环境只有这一种询问方式，而 `State<T>` 要求具体类型。用 `Arc` 作泛型参数
/// 是为了让 checker 与本模块共享同一个询问器——前者发起询问、后者回填答复。
/// 测试用的 `StubPrompter` 走内核自己的单测，不经此处。
pub struct AppState {
    pub supervisor: Arc<Supervisor<SharedPrompter>>,
    pub prompter: Arc<UiPrompter>,
    /// MCP 网关句柄。设置页的开关通过它真正启停监听。
    pub gateway: Arc<McpGateway>,
    /// 全局热键管理器。改键命令改完文件后靠它立即重注册，无需重启。
    pub shortcuts: Arc<ShortcutManager>,
}

impl AppState {
    pub fn new(
        supervisor: Arc<Supervisor<SharedPrompter>>,
        prompter: Arc<UiPrompter>,
        gateway: Arc<McpGateway>,
        shortcuts: Arc<ShortcutManager>,
    ) -> Self {
        Self {
            supervisor,
            prompter,
            gateway,
            shortcuts,
        }
    }
}

/// command 的统一返回类型。
type CmdResult<T> = Result<T, String>;

// ─────────────────── DTO ───────────────────

/// 插件列表页的一行。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginView {
    pub id: String,
    pub name: String,
    pub version: String,
    pub author: String,
    pub description: String,
    /// 生命周期模式，形如 `on-demand` / `startup`。
    pub lifecycle: String,
    /// 运行状态，形如 `stopped` / `idle` / `error`。
    pub state: String,
    /// 最近一次错误，供列表里显示失败原因。
    pub last_error: Option<String>,
    pub inflight: u64,
    pub restart_attempts: u32,
    /// manifest 声明的权限，已转成 `category:action[:scope]` 字符串。
    pub permissions: Vec<String>,
    /// 该插件提供的工具名。
    pub tools: Vec<String>,
    /// 是否带使用说明（manifest 有 `[docs]` 段）。
    ///
    /// 说明与快捷键都是**可选**的，前端据这两个标记决定要不要渲染对应按钮，
    /// 而不是点进去才发现是空的。
    pub has_docs: bool,
    /// 是否声明了快捷键（manifest 有 `[shortcut]` 段）。
    pub has_shortcut: bool,
    /// 是否有设置项（manifest 有 `[[settings]]`）。
    pub has_settings: bool,
    /// 是否已启用（未被用户禁用）。禁用 = 持久化 + 不响应按需唤醒 + 不出现在工具列表。
    /// 跟运行状态（`state`）正交：禁用插件的 `state` 仍是 `stopped`。
    pub enabled: bool,
    /// 工具结果展示声明，供前端按 schema 渲染结构化结果。
    pub result_display: Vec<intools::protocol::manifest::ResultDisplay>,
    /// 插件分类：system / test / user。
    pub category: String,
}

/// 工具列表项，供对话页做工具选择与手动调用。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolView {
    pub name: String,
    pub description: String,
    pub plugin_id: String,
    pub input_schema: JsonValue,
    /// 是否已向 MCP 客户端暴露。
    pub exposed: bool,
}

/// 权限页的一行：某插件的一条落盘授权。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GrantView {
    pub plugin_id: String,
    /// 权限字符串。
    pub permission: String,
    pub granted: bool,
    /// 授权时间（ISO 8601）。
    pub at: String,
    pub paths: Vec<String>,
}

/// 一次成功导入的结果，供界面提示「装的是哪个插件、落在哪」。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImportResultView {
    pub plugin_id: String,
    pub plugin_name: String,
    /// 落盘目录，出问题时便于让用户直接去看。
    pub installed_dir: String,
}

/// 「重载插件」的结果，供前端向用户汇报变动。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReloadResultView {
    /// 本次新出现的插件 id。
    pub added: Vec<String>,
    /// 本次消失的插件 id（目录被删或 manifest 变得无法解析）。
    pub removed: Vec<String>,
    /// 重载后的插件总数。
    pub total: usize,
    /// 扫描时发现的 manifest 解析/IO 错误（逐条文字描述）。
    pub load_failures: Vec<String>,
    /// 跨插件工具名冲突（逐条文字描述）。
    pub conflicts: Vec<String>,
}

/// 单个 MCP 客户端的配置片段。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpClientSnippet {
    /// 稳定标识，前端下拉框的 value。
    pub id: String,
    /// 客户端显示名。
    pub label: String,
    /// 该客户端的配置文件位置，用户得知道往哪儿粘。
    pub config_path: String,
    /// 使用要点：桥接依赖、生效方式等。
    pub note: String,
    /// 可直接粘贴的 JSON 片段。
    pub snippet: String,
}

/// 设置页读写的宿主配置。
///
/// 不直接复用 [`HostConfig`]：`plugins_dir` 是 `Option<PathBuf>`，跨 IPC 后前端拿到
/// 的路径分隔符形态不好处理；这里统一转成字符串，空串表示「用默认目录」。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SettingsView {
    /// 插件目录覆盖值，空串表示未设置。
    pub plugins_dir: String,
    /// 当前实际生效的插件目录，只读，供界面显示默认值。
    pub effective_plugins_dir: String,
    pub mcp_enabled: bool,
    /// MCP Bearer Token，未启用过则为空串。
    pub mcp_token: String,
    /// 网关**当前是否真在监听**。与 `mcp_enabled` 分开是有意的：配置说「开着」
    /// 而端口被占导致没起来时，两者会不一致，界面必须能把这种撕裂显示出来。
    pub mcp_running: bool,
    /// 客户端应连接的地址。运行中取实际监听地址（而非配置值），未运行时取默认地址。
    pub mcp_endpoint: String,
    /// 各客户端可直接粘贴的配置片段。
    ///
    /// 由后端生成而不是前端拼：端点形状与鉴权头是服务端契约的一部分，
    /// 放前端等于把同一份真相抄两遍，改协议时必漏。
    ///
    /// 是**列表**而非单份：客户端配置 schema 并不通用，见 [`mcp_client_snippets`]。
    pub mcp_snippets: Vec<McpClientSnippet>,
    /// 日志级别，形如 `info`。
    pub log_level: String,
    /// 关闭按钮行为：`minimize_to_tray`（默认）/ `exit`。
    pub close_behavior: String,
}

/// 把 [`InstanceState`] 转成前端用的小写字符串。
///
/// `InstanceState` 没派生 serde（它是纯内核类型），手写映射比给它加 derive 更克制。
fn state_label(state: Option<InstanceState>) -> String {
    match state {
        // 实例表里没有记录 == 从未启动过，与显式 Stopped 对前端是同一件事。
        None | Some(InstanceState::Stopped) => "stopped",
        Some(InstanceState::Starting) => "starting",
        Some(InstanceState::Idle) => "idle",
        Some(InstanceState::Busy) => "busy",
        Some(InstanceState::Stopping) => "stopping",
        Some(InstanceState::Error) => "error",
    }
    .to_string()
}

/// [`LogLevel`] ↔ 字符串。走 serde 以免这里和 `#[serde(rename_all)]` 的约定跑偏。
fn log_level_label(level: LogLevel) -> String {
    serde_json::to_value(level)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| "info".to_string())
}

fn parse_log_level(raw: &str) -> CmdResult<LogLevel> {
    serde_json::from_value(JsonValue::String(raw.to_string()))
        .map_err(|_| format!("无法识别的日志级别 `{raw}`"))
}

/// [`CloseBehavior`] ↔ 字符串，与 `log_level_label` 同款套路。
fn close_behavior_label(behavior: CloseBehavior) -> String {
    serde_json::to_value(behavior)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| "minimize_to_tray".to_string())
}

fn parse_close_behavior(raw: &str) -> CmdResult<CloseBehavior> {
    serde_json::from_value(JsonValue::String(raw.to_string()))
        .map_err(|_| format!("无法识别的关闭按钮行为 `{raw}`"))
}

// ─────────────────── 插件 ───────────────────

/// 列出全部已加载插件及其运行状态。
#[tauri::command]
pub async fn list_plugins(state: State<'_, AppState>) -> CmdResult<Vec<PluginView>> {
    let supervisor = &state.supervisor;
    let mut views = Vec::new();

    // 注册表的读守卫不是 `Send`，握着它走进下面的 `.await` 会被编译器拒掉——
    // 这个限制是刻意的：查状态期间若发生重载，后半个循环用的就是过期数据。
    // 先整体克隆一份快照（`RegisteredPlugin: Clone`），循环体照原样写。
    let plugins: Vec<_> = supervisor
        .registry()
        .list_plugins_sorted()
        .into_iter()
        .cloned()
        .collect();

    for plugin in plugins {
        let id = plugin.id().to_string();
        let status = supervisor.instance_status(&id).await;
        let manifest = &plugin.manifest;

        views.push(PluginView {
            id: id.clone(),
            name: manifest.plugin.name.clone(),
            version: manifest.plugin.version.clone(),
            author: manifest.plugin.author.clone(),
            description: manifest.plugin.description.clone(),
            lifecycle: serde_json::to_value(manifest.lifecycle.mode)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_else(|| "on-demand".to_string()),
            state: state_label(status.as_ref().map(|s| s.state)),
            last_error: status.as_ref().and_then(|s| s.last_error.clone()),
            inflight: status.as_ref().map_or(0, |s| s.inflight),
            restart_attempts: status.as_ref().map_or(0, |s| s.restart_attempts),
            permissions: manifest
                .capabilities
                .permissions
                .iter()
                .map(|p| p.to_string())
                .collect(),
            tools: plugin.tools.iter().map(|t| t.name.clone()).collect(),
            has_docs: manifest.docs.is_some(),
            has_shortcut: manifest.shortcut.is_some(),
            has_settings: !manifest.settings.is_empty(),
            enabled: !supervisor.is_disabled(&id),
            result_display: manifest.result_display.clone(),
            category: plugin.category.as_str().to_string(),
        });
    }

    Ok(views)
}

/// 手动启动插件。已在运行时为幂等空操作。
#[tauri::command]
pub async fn start_plugin(plugin_id: String, state: State<'_, AppState>) -> CmdResult<()> {
    state
        .supervisor
        .start_plugin(&plugin_id)
        .await
        .map_err(|e| e.to_string())
}

/// 手动停止插件。未在运行时同样返回成功。
#[tauri::command]
pub async fn stop_plugin(plugin_id: String, state: State<'_, AppState>) -> CmdResult<()> {
    state
        .supervisor
        .stop_plugin(&plugin_id)
        .await
        .map_err(|e| e.to_string())
}

/// 设置插件的启用 / 禁用态。
///
/// 禁用是持久化的——会被写回 `HostConfig.disabled_plugins`，重启后仍生效。
/// 与「停止」的区别：停止只回收当前进程，下一次按需调用还会被拉起来；
/// 禁用则拦截 `ensure_running` 与 `start_eager_plugins`，并过滤掉工具列表。
/// 启用是反方向，不会主动拉起，按需唤醒路径自会处理。
#[tauri::command]
pub async fn set_plugin_enabled(
    plugin_id: String,
    enabled: bool,
    state: State<'_, AppState>,
) -> CmdResult<bool> {
    let changed = state.supervisor.set_plugin_enabled(&plugin_id, enabled).await;

    // 持久化到 HostConfig。失败也要提示——不能让 UI 显示已禁用，实际重启后
    // 又被拉起来。
    let mut cfg = HostConfig::load().map_err(|e| e.to_string())?;
    let list = &mut cfg.disabled_plugins;
    let had = list.iter().any(|id| id == &plugin_id);
    if enabled && had {
        list.retain(|id| id != &plugin_id);
    } else if !enabled && !had {
        list.push(plugin_id.clone());
    } else {
        // 没变化就跳过写盘——避免每次点都动一次配置文件。
        return Ok(changed);
    }
    cfg.save().map_err(|e| e.to_string())?;
    // MCP 工具表是请求时重建的（见 `gateway.rs::supervisor_table_source`），
    // 不需要主动作废缓存——下一次 `tools/list` 自然会读最新的 disabled 集。
    Ok(changed)
}

/// 卸载插件：停进程 → 删目录 → 撤授权 → 清 MCP 暴露。
///
/// 内存注册表现在虽是 `RwLock`（可热替换），但本命令不自动触发重载——用户可
/// 手动点「重载插件」让内存与磁盘同步。保持分离是因为重载会整体重建注册表，
/// 对一个只删了一个插件的场景太重。
///
/// 顺序是刻意的：先停进程，否则 Windows 上正在运行的 `python main.py` 会锁住目录；
/// 授权与暴露记录放在删目录之后清，即使清理失败也不会留下「目录还在但权限没了」
/// 这种更难解释的中间态。
#[tauri::command]
pub async fn uninstall_plugin(plugin_id: String, state: State<'_, AppState>) -> CmdResult<()> {
    let plugin_dir = state
        .supervisor
        .registry()
        .get_plugin(&plugin_id)
        .map(|p| p.plugin_dir.clone())
        .ok_or_else(|| format!("插件 `{plugin_id}` 不存在"))?;

    state
        .supervisor
        .stop_plugin(&plugin_id)
        .await
        .map_err(|e| e.to_string())?;

    std::fs::remove_dir_all(&plugin_dir)
        .map_err(|e| format!("删除插件目录 {} 失败：{e}", plugin_dir.display()))?;

    state
        .supervisor
        .revoke_plugin_grants(&plugin_id)
        .await
        .map_err(|e| e.to_string())?;

    // 白名单里残留的条目会让 Phase 8 的网关尝试解析一个已不存在的工具，顺手清掉。
    let mut exposure = McpExposure::load().map_err(|e| e.to_string())?;
    let prefix = format!("{plugin_id}:");
    let before = exposure.exposed.len();
    exposure.exposed.retain(|t| !t.starts_with(&prefix));
    if exposure.exposed.len() != before {
        exposure.save().map_err(|e| e.to_string())?;
    }

    Ok(())
}

/// 导入 zip 形态的插件包。
///
/// 现有 id 取自**磁盘实时扫描**而非 `state.supervisor.registry()`：内存注册表是启动
/// 时的快照，既不包含本次会话里刚导入的插件（连导两次同一个包就会都放过去，重启后
/// 后者被静默丢弃），也仍包含刚被卸载的插件（明明目录已删，却拦着不让重装）。
/// 磁盘状态才是「重启后会加载成什么」的真相。
///
/// 扫描与解压都是阻塞 IO，整段丢进 `spawn_blocking`，避免几 MB 的包卡住异步运行时。
#[tauri::command]
pub async fn import_plugin_package(zip_bytes: Vec<u8>) -> CmdResult<ImportResultView> {
    let plugins_root = HostConfig::load()
        .map_err(|e| e.to_string())?
        .effective_plugins_dir()
        .map_err(|e| e.to_string())?;
    // 用户导入的插件放到 user/ 子目录
    let user_dir = plugins_root.join("user");

    let outcome = tauri::async_runtime::spawn_blocking(move || {
        // 扫描失败（目录不存在）不该拦住导入：那正是「一个插件都没装」的情形，
        // 空 id 列表即可。扫描全部三个分类目录以检查 id 冲突。
        let mut existing_ids = Vec::new();
        for subdir in &["system", "test", "user"] {
            let dir = plugins_root.join(subdir);
            if let Ok(entries) = discovery::scan_plugins_root(&dir) {
                for entry in entries {
                    if let Ok(p) = entry {
                        existing_ids.push(p.manifest.plugin.id);
                    }
                }
            }
        }

        import::import_from_bytes(&user_dir, &zip_bytes, &existing_ids)
    })
    .await
    .map_err(|e| format!("导入任务未能完成：{e}"))?
    .map_err(|e| e.to_string())?;

    Ok(ImportResultView {
        plugin_id: outcome.plugin_id,
        plugin_name: outcome.plugin_name,
        installed_dir: outcome.installed_dir.display().to_string(),
    })
}

/// 重载插件：重新扫描插件目录，整体替换内存注册表。
///
/// 不需要重启宿主即可识别 manifest 变化（新增、删除、改工具声明）。扫描在
/// `spawn_blocking` 里做（阻塞 IO），替换在 [`Supervisor::replace_registry`]
/// 里做（会停掉消失插件的进程、清掉它们的工具缓存），最后刷新快捷键绑定。
#[tauri::command]
pub async fn reload_plugins(state: State<'_, AppState>) -> CmdResult<ReloadResultView> {
    let plugins_root = HostConfig::load()
        .map_err(|e| e.to_string())?
        .effective_plugins_dir()
        .map_err(|e| e.to_string())?;

    let next = tauri::async_runtime::spawn_blocking(move || {
        Registry::scan_and_build_categorized(&plugins_root)
    })
    .await
    .map_err(|e| format!("重载任务未能完成：{e}"))?
    .map_err(|e| e.to_string())?;

    let diff = state.supervisor.replace_registry(next).await;

    // 从替换后的注册表里取出冲突与加载失败信息，交给前端展示。
    let (load_failures, conflicts) = {
        let reg = state.supervisor.registry();
        (
            reg.load_failures().iter().map(|e| e.to_string()).collect(),
            reg.conflicts()
                .iter()
                .map(|c| {
                    format!(
                        "`{}` 的工具 `{}` 被拒：{}（已被 `{}` 占据）",
                        c.rejected_from, c.tool_name, c.reason, c.owner_plugin
                    )
                })
                .collect(),
        )
    };

    // 插件清单变了，快捷键绑定也要跟着刷新。
    let _ = state.shortcuts.reload();

    Ok(ReloadResultView {
        added: diff.added,
        removed: diff.removed,
        total: diff.total,
        load_failures,
        conflicts,
    })
}

// ─────────────────── 工具 ───────────────────

/// 列出全部工具。走缓存，不会唤醒任何插件。
#[tauri::command]
pub async fn list_tools(state: State<'_, AppState>) -> CmdResult<Vec<ToolView>> {
    // 暴露清单读失败不该让工具列表整体不可用，退化成「都未暴露」即可。
    let exposure = McpExposure::load().unwrap_or_default();

    Ok(state
        .supervisor
        .list_tools()
        .await
        .into_iter()
        .map(|(plugin_id, tool)| ToolView {
            exposed: exposure.is_exposed(&format!("{plugin_id}:{}", tool.name)),
            name: tool.name,
            description: tool.description,
            plugin_id,
            input_schema: tool.input_schema,
        })
        .collect())
}

/// 调用工具。
///
/// 署名 [`CallerIdentity::Ui`]：界面是可询问的调用方，遇到未授权的中/高危权限
/// 会触发弹窗（经 [`UiPrompter`]），而不是直接失败。
///
/// 设置合并：先加载该插件的用户设置（`~/.intools/plugin-settings/<id>.json`），
/// 作为默认参数；用户显式传入的 `args` 覆盖同名键。
#[tauri::command]
pub async fn call_tool(
    tool_name: String,
    args: JsonValue,
    state: State<'_, AppState>,
) -> CmdResult<JsonValue> {
    tracing::info!(tool = %tool_name, "界面发起工具调用");

    // 合并插件设置：设置值作默认，显式 args 覆盖。
    let merged = merge_plugin_settings(&tool_name, args, &state.supervisor);

    let result = state
        .supervisor
        .call_tool(&tool_name, merged, CallerIdentity::Ui)
        .await
        .map_err(|e| e.to_string());
    match &result {
        Ok(_) => tracing::info!(tool = %tool_name, "工具调用成功"),
        Err(e) => tracing::info!(tool = %tool_name, error = %e, "工具调用失败"),
    }
    result
}

/// 将插件用户设置合并进工具参数。设置值作默认，`explicit_args` 覆盖同名键。
fn merge_plugin_settings(
    tool_name: &str,
    explicit_args: JsonValue,
    supervisor: &Supervisor<SharedPrompter>,
) -> JsonValue {
    let Some(plugin_id) = supervisor.plugin_id_for_tool(tool_name) else {
        return explicit_args;
    };

    let stored = match config::load_plugin_settings(&plugin_id) {
        Ok(map) => map,
        Err(e) => {
            tracing::warn!(plugin_id = %plugin_id, error = %e, "加载插件设置失败，使用原始参数");
            return explicit_args;
        }
    };

    if stored.is_empty() {
        return explicit_args;
    }

    // 合并：stored 作默认，explicit 覆盖。
    let mut merged = stored;
    if let Some(obj) = explicit_args.as_object() {
        for (k, v) in obj {
            merged.insert(k.clone(), v.clone());
        }
    } else if !explicit_args.is_null() {
        // explicit_args 不是对象也不是 null：原样返回，不合并。
        return explicit_args;
    }
    JsonValue::Object(merged)
}

// ─────────────────── 权限 ───────────────────

/// 前端弹窗的答复落点。
///
/// 返回 `false` 表示这条询问已不存在（重复答复或调用方已放弃），前端可忽略。
#[tauri::command]
pub fn respond_permission_prompt(
    id: String,
    decision: PromptDecision,
    state: State<'_, AppState>,
) -> bool {
    state.prompter.resolve(&id, decision)
}

/// 列出全部落盘授权。只含永久授权——会话授权随进程消失，列出来会让用户误以为可撤销。
#[tauri::command]
pub async fn list_grants(state: State<'_, AppState>) -> CmdResult<Vec<GrantView>> {
    Ok(state
        .supervisor
        .list_persisted_grants()
        .await
        .into_iter()
        .flat_map(|pg| {
            let plugin_id = pg.plugin_id;
            pg.grants
                .into_iter()
                .map(move |(permission, record)| GrantView {
                    plugin_id: plugin_id.clone(),
                    permission,
                    granted: record.granted,
                    at: record.at,
                    paths: record.paths,
                })
        })
        .collect())
}

/// 撤销某插件的全部授权。下次调用会重新询问。
#[tauri::command]
pub async fn revoke_grants(plugin_id: String, state: State<'_, AppState>) -> CmdResult<()> {
    state
        .supervisor
        .revoke_plugin_grants(&plugin_id)
        .await
        .map_err(|e| e.to_string())
}

// ─────────────────── MCP 暴露 ───────────────────

/// 勾选/取消某工具对 MCP 客户端的暴露。返回是否真的发生了变化。
#[tauri::command]
pub fn set_tool_exposed(plugin_id: String, tool_name: String, exposed: bool) -> CmdResult<bool> {
    let mut exposure = McpExposure::load().map_err(|e| e.to_string())?;
    let changed = exposure.set_exposed(&format!("{plugin_id}:{tool_name}"), exposed);
    // 没变化就不写盘，省一次原子 rename。
    if changed {
        exposure.save().map_err(|e| e.to_string())?;
    }
    Ok(changed)
}

// ─────────────────── 设置 ───────────────────

/// 读取宿主配置。
#[tauri::command]
pub async fn get_settings(state: State<'_, AppState>) -> CmdResult<SettingsView> {
    settings_view(&state.gateway).await
}

/// 组装设置视图。抽成自由函数是因为几个 command 都要在改完配置后回读一次。
async fn settings_view(gateway: &McpGateway) -> CmdResult<SettingsView> {
    let config = HostConfig::load().map_err(|e| e.to_string())?;
    let token = config.mcp_token.clone().unwrap_or_default();
    // 运行中优先报实际监听地址：端口 0 或将来允许改端口时，配置值会撒谎。
    let endpoint = match gateway.local_addr().await {
        Some(addr) => format!("http://{addr}/mcp"),
        None => format!("http://{}/mcp", server::default_addr()),
    };

    Ok(SettingsView {
        plugins_dir: config
            .plugins_dir
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_default(),
        effective_plugins_dir: config
            .effective_plugins_dir()
            .map_err(|e| e.to_string())?
            .display()
            .to_string(),
        mcp_enabled: config.mcp_enabled,
        mcp_snippets: mcp_client_snippets(&endpoint, &token),
        mcp_token: token,
        mcp_running: gateway.is_running().await,
        mcp_endpoint: endpoint,
        log_level: log_level_label(config.log_level),
        close_behavior: close_behavior_label(config.close_behavior),
    })
}

/// Token 未生成时片段里的占位符。直接吐空 `Bearer ` 只会让用户收到 401，
/// 却看不出是自己没开网关。
const TOKEN_PLACEHOLDER: &str = "<开启网关后生成>";

/// 桥接用的 mcp-remote 版本。
///
/// 钉死大版本而非用 `latest`：这是给用户抄进配置文件长期留存的，上游一旦发布
/// 破坏性改动，用户那份配置会在某天早上突然失效，且毫无线索指向 npx。
/// 不写 `^0.8.3` 是因为 Windows 下 npx 经 cmd.exe 启动，`^` 会被当转义符吃掉。
const MCP_REMOTE_SPEC: &str = "mcp-remote@0.8.3";

/// 生成各客户端的配置片段。
///
/// **不能只给一份**。曾经统一输出 `{"type":"http","url":…,"headers":…}`，那是
/// Claude Code / Cursor / VS Code 的形态；而 `claude_desktop_config.json` 的校验
/// schema 只接受 stdio 形态（必须有 `command`）。用户照抄进 Claude Desktop 的后果
/// 不是「不生效」这么轻——它解析失败后会在下次保存时重写该文件，把整个
/// `mcpServers` 块连同用户原有的正常条目一起丢掉。
///
/// 所以按客户端分形态：能直连 HTTP 的直连，Claude Desktop 走 stdio 桥接。
fn mcp_client_snippets(endpoint: &str, token: &str) -> Vec<McpClientSnippet> {
    vec![
        McpClientSnippet {
            id: "claude-desktop".to_string(),
            label: "Claude Desktop".to_string(),
            config_path: "%APPDATA%\\Claude\\claude_desktop_config.json".to_string(),
            note: format!(
                "Claude Desktop 只认 stdio 形态，须用 mcp-remote 桥接（需已安装 Node.js）。\
                 首次启动会由 npx 下载 {MCP_REMOTE_SPEC}。粘贴后完全退出并重启客户端。"
            ),
            snippet: bridge_snippet(endpoint, token),
        },
        McpClientSnippet {
            id: "claude-code".to_string(),
            label: "Claude Code（CLI）".to_string(),
            config_path: "~/.claude.json".to_string(),
            note: "原生支持 HTTP，直连即可，不需要桥接进程。".to_string(),
            snippet: http_snippet(endpoint, token),
        },
        McpClientSnippet {
            id: "cursor".to_string(),
            label: "Cursor / VS Code".to_string(),
            config_path: "Cursor：.cursor/mcp.json；VS Code：.vscode/mcp.json（键名为 servers）"
                .to_string(),
            note: "原生支持 HTTP，直连即可。VS Code 的外层键是 servers，不是 mcpServers。"
                .to_string(),
            snippet: http_snippet(endpoint, token),
        },
    ]
}

/// 原生支持 Streamable HTTP 的客户端形态。
fn http_snippet(endpoint: &str, token: &str) -> String {
    to_pretty(serde_json::json!({
        "mcpServers": {
            "intools": {
                "type": "http",
                "url": endpoint,
                "headers": { "Authorization": bearer(token) },
            }
        }
    }))
}

/// Claude Desktop 用的 stdio 桥接形态。
///
/// Token 走 `env` 而非直接拼进 `args`：Windows 上 Claude Desktop 启动 npx 时不会
/// 转义 `args` 里的空格，`"Authorization: Bearer xxx"` 会被拆成两段，请求头就废了。
/// 写成 `Authorization:${AUTH_HEADER}`（冒号后无空格）可以绕开。
fn bridge_snippet(endpoint: &str, token: &str) -> String {
    to_pretty(serde_json::json!({
        "mcpServers": {
            "intools": {
                "command": "npx",
                // -y 免掉首次安装的交互确认，否则客户端只会看到进程卡住。
                // --allow-http 是因为端点是本机明文 HTTP，桥接默认只放行 HTTPS。
                "args": [
                    "-y",
                    MCP_REMOTE_SPEC,
                    endpoint,
                    "--allow-http",
                    "--header",
                    "Authorization:${AUTH_HEADER}",
                ],
                "env": { "AUTH_HEADER": bearer(token) },
            }
        }
    }))
}

fn bearer(token: &str) -> String {
    format!(
        "Bearer {}",
        if token.is_empty() {
            TOKEN_PLACEHOLDER
        } else {
            token
        }
    )
}

/// 片段是给人复制的，紧凑格式没法读。序列化失败在这里不可能发生
/// （全是字符串字面量），退化成空串即可。
fn to_pretty(value: serde_json::Value) -> String {
    serde_json::to_string_pretty(&value).unwrap_or_default()
}

/// 写入插件目录与日志级别。
///
/// MCP 开关不走这里——它要生成/保留 Token，语义比「改个字段」重，
/// 单独用 [`set_mcp_enabled`]。
#[tauri::command]
pub async fn save_settings(
    plugins_dir: String,
    log_level: String,
    close_behavior: String,
    state: State<'_, AppState>,
) -> CmdResult<SettingsView> {
    let mut config = HostConfig::load().map_err(|e| e.to_string())?;
    let trimmed = plugins_dir.trim();
    config.plugins_dir = if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.into())
    };
    config.log_level = parse_log_level(&log_level)?;
    config.close_behavior = parse_close_behavior(&close_behavior)?;
    config.save().map_err(|e| e.to_string())?;
    // 关窗行为是「下一次关窗时生效」——本次运行已经注册的关闭监听
    // 不会再换，告知用户重启或下一次启动自然生效足够。
    settings_view(&state.gateway).await
}

/// 开关 MCP 网关。首次开启时生成 Token，之后开关都复用同一个，
/// 避免已配置好的客户端因为一次误关而失效。
///
/// 配置先落盘再启停，顺序不能反：启动失败时 Token 已经写进配置，用户重试拿到的
/// 还是同一个，不必回去改客户端。但**启动失败必须把 `mcp_enabled` 回滚**，
/// 否则界面开关显示「已开启」而实际没有监听，是最难排查的一类状态撕裂。
#[tauri::command]
pub async fn set_mcp_enabled(enabled: bool, state: State<'_, AppState>) -> CmdResult<SettingsView> {
    let mut config = HostConfig::load().map_err(|e| e.to_string())?;

    if enabled {
        let token = config.enable_mcp().to_string();
        config.save().map_err(|e| e.to_string())?;

        if let Err(err) = start_gateway(
            Arc::clone(&state.gateway),
            Arc::clone(&state.supervisor),
            token,
        )
        .await
        {
            config.disable_mcp();
            // 回滚都失败就只能让错误叠加上报：此时配置与实际状态确实不一致，
            // 瞒下来只会让用户更迷惑。
            if let Err(save_err) = config.save() {
                return Err(format!("{err}（回滚配置亦失败：{save_err}）"));
            }
            return Err(err.to_string());
        }
    } else {
        config.disable_mcp();
        config.save().map_err(|e| e.to_string())?;
        state.gateway.stop().await;
    }

    settings_view(&state.gateway).await
}

/// 用给定 Supervisor 启动网关。抽出来是因为 `main.rs` 的开机自启也要走同一条路径。
///
/// 收 `Arc` 而不是 `&AppState`：开机自启发生在 `tauri::async_runtime::spawn` 里，
/// 那里拿不到 `State` 的借用。
pub async fn start_gateway(
    gateway: Arc<McpGateway>,
    supervisor: Arc<Supervisor<SharedPrompter>>,
    token: String,
) -> Result<std::net::SocketAddr, server::ServerError> {
    gateway
        .start(
            server::default_addr(),
            token,
            Arc::clone(&supervisor) as Arc<dyn ToolInvoker>,
            gateway::supervisor_table_source(supervisor),
        )
        .await
}

// ─────────────────── 每插件设置 ───────────────────

/// 设置页返回给前端的视图：字段 schema + 当前值。
#[derive(Debug, Clone, Serialize)]
pub struct PluginSettingsView {
    /// manifest 声明的字段列表（含类型、标签、默认值、选项）。
    pub fields: Vec<SettingFieldView>,
    /// 当前持久化的设置值（可能只覆盖部分字段，未覆盖的用 default 补）。
    pub values: serde_json::Map<String, JsonValue>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SettingFieldView {
    pub key: String,
    pub label: String,
    pub field_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<JsonValue>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step: Option<f64>,
}

/// 获取插件的设置 schema + 当前值。
///
/// 插件不存在或无设置字段时返回空 fields，前端据此隐藏设置按钮。
#[tauri::command]
pub async fn get_plugin_settings(
    plugin_id: String,
    state: State<'_, AppState>,
) -> CmdResult<PluginSettingsView> {
    use intools::protocol::manifest::SettingType;

    // 从 supervisor 的 registry 中找到该插件的 manifest。
    let manifests = state.supervisor.list_plugins_with_manifests();
    let has_plugin = manifests.iter().any(|(id, _)| *id == plugin_id);
    if !has_plugin {
        return Err(format!("插件 `{plugin_id}` 不存在"));
    }

    let fields: Vec<SettingFieldView> = manifests
        .iter()
        .find(|(id, _)| *id == plugin_id)
        .map(|(_, m)| {
            m.settings
                .iter()
                .map(|sf| SettingFieldView {
                    key: sf.key.clone(),
                    label: sf.label.clone(),
                    field_type: match sf.field_type {
                        SettingType::String => "string".into(),
                        SettingType::Number => "number".into(),
                        SettingType::Boolean => "boolean".into(),
                        SettingType::Select => "select".into(),
                        SettingType::Color => "color".into(),
                        SettingType::Path => "path".into(),
                        SettingType::Password => "password".into(),
                    },
                    default: if sf.default.is_null() { None } else { Some(sf.default.clone()) },
                    options: sf.options.clone(),
                    group: sf.group.clone(),
                    description: sf.description.clone(),
                    placeholder: sf.placeholder.clone(),
                    min: sf.min,
                    max: sf.max,
                    step: sf.step,
                })
                .collect()
        })
        .unwrap_or_default();

    let values = config::load_plugin_settings(&plugin_id).unwrap_or_default();

    Ok(PluginSettingsView { fields, values })
}

/// 保存插件设置值。
#[tauri::command]
pub async fn save_plugin_settings(
    plugin_id: String,
    values: serde_json::Map<String, JsonValue>,
) -> CmdResult<()> {
    config::save_plugin_settings(&plugin_id, &values)
        .map_err(|e| e.to_string())
}

// ─────────────────── 快捷键 ───────────────────

/// 单个插件的快捷键现状，供改键弹窗回填。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginShortcutView {
    pub plugin_id: String,
    /// manifest 里预设的按键。可能是空串——插件支持热键但不预设。
    pub default_key: String,
    /// 当前**生效**的按键。空串表示未绑定。
    pub key: String,
    /// `"manifest"` 或 `"user"`，用来在界面上标注「已自定义」。
    pub source: String,
    pub enabled: bool,
    /// 按下后调用的工具名，只读。
    pub tool: String,
    /// 交互界面类型，只读。
    pub ui: Option<String>,
}

/// 全局快捷键总览里的一行。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShortcutBindingView {
    pub plugin_id: String,
    pub plugin_name: String,
    pub key: String,
    pub tool: String,
    pub source: String,
    /// 是否真的注册到系统了。解析通过但被操作系统拒绝时为 `false`。
    pub active: bool,
}

/// 一条未能生效的绑定及原因。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShortcutIssueView {
    pub plugin_id: String,
    pub key: String,
    /// 冲突时指向占用者插件 id；按键非法时为 `null`。
    pub owner_plugin: Option<String>,
    pub reason: String,
}

/// 快捷键总览。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShortcutOverviewView {
    pub bindings: Vec<ShortcutBindingView>,
    pub issues: Vec<ShortcutIssueView>,
}

fn issue_view(issue: &shortcut::ShortcutIssue) -> ShortcutIssueView {
    ShortcutIssueView {
        plugin_id: issue.plugin_id.clone(),
        key: issue.key.clone(),
        owner_plugin: issue.owner_plugin.clone(),
        reason: issue.reason.clone(),
    }
}

/// 读取某插件的快捷键现状。
///
/// 插件没声明 `[shortcut]` 时返回错误而非空值：前端本就靠 `has_shortcut`
/// 决定是否显示入口，走到这里说明调用方状态已经不对了，静默返回空反而掩盖问题。
#[tauri::command]
pub async fn get_plugin_shortcut(
    plugin_id: String,
    state: State<'_, AppState>,
) -> CmdResult<PluginShortcutView> {
    let manifest = state
        .supervisor
        .manifest_of(&plugin_id)
        .ok_or_else(|| format!("插件 `{plugin_id}` 不存在"))?;
    let sc = manifest
        .shortcut
        .as_ref()
        .ok_or_else(|| format!("插件 `{plugin_id}` 未声明快捷键"))?;

    let user = UserShortcuts::load().unwrap_or_default();
    let override_ = user.get(&plugin_id);
    let custom = override_.map(|o| o.key.trim()).unwrap_or_default();
    let enabled = override_.is_none_or(|o| o.enabled);

    let (key, source) = if custom.is_empty() {
        (sc.key.clone(), shortcut::BindingSource::Manifest)
    } else {
        (custom.to_string(), shortcut::BindingSource::User)
    };

    Ok(PluginShortcutView {
        plugin_id,
        default_key: sc.key.clone(),
        key,
        source: source.as_str().to_string(),
        enabled,
        tool: sc.tool.clone(),
        ui: sc.ui.clone(),
    })
}

/// 改键。返回 `Some(原因)` 表示已保存但未能注册到系统。
///
/// 三步：归一化 → 冲突检查 → 落盘并立即重注册。冲突检查放在落盘**之前**，
/// 让用户在弹窗里当场看到「已被 XX 占用」，而不是保存成功后才发现按键失灵。
///
/// `key` 传空串且 `enabled` 为真即「恢复默认」——[`UserShortcuts::set`] 会删掉
/// 该条覆盖，绑定重新回落到 manifest。
#[tauri::command]
pub async fn set_plugin_shortcut(
    plugin_id: String,
    key: String,
    enabled: bool,
    state: State<'_, AppState>,
) -> CmdResult<Option<String>> {
    if state.supervisor.manifest_of(&plugin_id).is_none() {
        return Err(format!("插件 `{plugin_id}` 不存在"));
    }

    let trimmed = key.trim();
    let normalized = if trimmed.is_empty() {
        String::new()
    } else {
        shortcut::normalize_key(trimmed).map_err(|e| e.to_string())?
    };

    if !normalized.is_empty() && enabled {
        let active = state.shortcuts.active_bindings();
        if let Some(owner) = shortcut::find_key_owner(&active, &normalized, &plugin_id) {
            let name = state
                .supervisor
                .manifest_of(&owner.plugin_id)
                .map_or_else(|| owner.plugin_id.clone(), |m| m.plugin.name.clone());
            return Err(format!("`{normalized}` 已被「{name}」占用"));
        }
    }

    let mut user = UserShortcuts::load().map_err(|e| e.to_string())?;
    user.set(&plugin_id, &normalized, enabled);
    user.save().map_err(|e| e.to_string())?;

    let report = state.shortcuts.reload();
    tracing::info!(
        plugin_id = %plugin_id,
        key = %normalized,
        enabled,
        registered = report.registered.len(),
        "快捷键已更新"
    );

    // 只把与本插件相关的失败回给调用方；其他插件的问题在总览里看。
    Ok(report
        .issues
        .iter()
        .find(|i| i.plugin_id == plugin_id)
        .map(|i| i.reason.clone()))
}

/// 快捷键总览：全部声明了热键的插件当前绑定到哪个键、有没有出问题。
///
/// 这里用 [`shortcut::resolve_bindings`] 重算一遍而不是直接返回
/// [`ShortcutManager::active_bindings`]：后者只有成功注册的那些，看不到冲突
/// 与非法按键。重算是纯函数，不碰操作系统，所以刷新页面不会打断已生效的热键。
#[tauri::command]
pub async fn list_shortcut_bindings(
    state: State<'_, AppState>,
) -> CmdResult<ShortcutOverviewView> {
    let manifests = state.supervisor.list_plugins_with_manifests();
    let user = UserShortcuts::load().unwrap_or_default();
    // `resolve_bindings` 收 `&[(&str, &Manifest)]`，而 supervisor 现在只能给出
    // owned 数据（注册表在锁后面，借用出不来）。建一层借用视图比改那个函数的
    // 签名划算得多——它的签名被自己的一整批单测钉着。
    let view: Vec<_> = manifests
        .iter()
        .map(|(id, m)| (id.as_str(), m))
        .collect();
    let outcome = shortcut::resolve_bindings(&view, &user);
    let active = state.shortcuts.active_bindings();

    let bindings = outcome
        .bindings
        .iter()
        .map(|b| ShortcutBindingView {
            plugin_name: manifests
                .iter()
                .find(|(id, _)| id == &b.plugin_id)
                .map_or_else(|| b.plugin_id.clone(), |(_, m)| m.plugin.name.clone()),
            plugin_id: b.plugin_id.clone(),
            key: b.key.clone(),
            tool: b.tool.clone(),
            source: b.source.as_str().to_string(),
            active: active
                .iter()
                .any(|a| a.plugin_id == b.plugin_id && a.key == b.key),
        })
        .collect();

    Ok(ShortcutOverviewView {
        bindings,
        issues: outcome.issues.iter().map(issue_view).collect(),
    })
}

// ─────────────────── 使用说明 ───────────────────

/// 插件自带的说明文档内容。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginDocsView {
    pub plugin_name: String,
    /// 文件名，展示在弹窗标题里，方便用户知道读的是哪个文件。
    pub filename: String,
    /// 原始 Markdown 文本。渲染在前端做——宿主不替插件解释它的文档格式。
    pub content: String,
}

/// 读取插件说明文档。
///
/// `usage_file` 来自插件作者，属于不可信输入。manifest 校验阶段已经拒掉了
/// 含路径分隔符与 `.` / `..` 的值，这里再拼一次目录即可，不会逃出插件根目录。
#[tauri::command]
pub async fn get_plugin_docs(
    plugin_id: String,
    state: State<'_, AppState>,
) -> CmdResult<PluginDocsView> {
    let manifest = state
        .supervisor
        .manifest_of(&plugin_id)
        .ok_or_else(|| format!("插件 `{plugin_id}` 不存在"))?;
    let docs = manifest
        .docs
        .as_ref()
        .ok_or_else(|| format!("插件 `{plugin_id}` 未提供使用说明"))?;
    let dir = state
        .supervisor
        .plugin_dir(&plugin_id)
        .ok_or_else(|| format!("插件 `{plugin_id}` 目录不可用"))?;

    let path = dir.join(&docs.usage_file);
    let content = std::fs::read_to_string(&path)
        .map_err(|e| format!("读取 {} 失败：{e}", docs.usage_file))?;

    Ok(PluginDocsView {
        plugin_name: manifest.plugin.name.clone(),
        filename: docs.usage_file.clone(),
        content,
    })
}

/// 读取随安装包分发的插件开发手册（Markdown）。
///
/// 文件路径固定：`docs/plugin-development.md`，由 `tauri.conf.json` 的
/// `bundle.resources` 整体拷贝到可执行文件旁。文档改了只需重打包，
/// 不需要改代码。
#[tauri::command]
pub async fn get_plugin_dev_doc() -> CmdResult<String> {
    let path = paths::bundled_doc("plugin-development.md").map_err(|e| e.to_string())?;
    std::fs::read_to_string(&path).map_err(|e| format!("读取 `{}` 失败：{e}", path.display()))
}

// ─────────────────── MCP 审计 ───────────────────

/// 列出最近的 MCP 审计记录，最新的排前面。
///
/// 给权限页用：让用户能直观看到「谁、什么时候、通过 MCP 调了哪个工具、结果怎样」。
///
/// 不分页——日志文件通常很小（MCP 调用频率本身就不高），全量读一次
/// 比维护一个分页 token 简单得多。
#[tauri::command]
pub async fn list_mcp_audit(limit: Option<usize>) -> CmdResult<Vec<McpAuditEntryView>> {
    let path = paths::mcp_audit_log().map_err(|e| e.to_string())?;
    let mut records = read_audit_log(&path).map_err(|e| e.to_string())?;
    // 时间倒序，让用户第一眼看到的是「刚刚发生了什么」。
    records.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    if let Some(n) = limit {
        records.truncate(n);
    }
    Ok(records.into_iter().map(McpAuditEntryView::from).collect())
}

/// 前端用的 MCP 审计条目。结构与 `McpAuditRecord` 一一对应，但放在 commands 层
/// 而不是 mcp 层，让前端 DTO 与后端内部结构解耦——审计字段未来若调整
/// （比如加调用链 ID）不必惊动 IPC 类型。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpAuditEntryView {
    pub timestamp: String,
    pub caller: String,
    pub tool: String,
    pub plugin_id: String,
    pub args_summary: String,
    pub duration_ms: u64,
    pub outcome: String,
}

impl From<McpAuditRecord> for McpAuditEntryView {
    fn from(r: McpAuditRecord) -> Self {
        Self {
            timestamp: r.timestamp,
            caller: r.caller,
            tool: r.tool,
            plugin_id: r.plugin_id,
            args_summary: r.args_summary,
            duration_ms: r.duration_ms,
            outcome: r.outcome,
        }
    }
}

// ─────────────────── AI 配置 ───────────────────

/// AI 编排插件的配置，存于 `~/.intools/plugin-configs/<id>.json`。
///
/// 不直接复用插件自己的 `host/getConfig`（那要走子进程往返）：设置页在插件
/// 未启动时也要能读写配置，所以宿主直接操作文件。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AiConfigView {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
}

/// AI 编排插件的固定 id，与 manifest.toml 中的 `[plugin] id` 一致。
const AI_ORCHESTRATOR_ID: &str = "com.intools.ai-orchestrator";

#[tauri::command]
pub async fn get_ai_config() -> CmdResult<AiConfigView> {
    let path = config::paths::plugin_config_json(AI_ORCHESTRATOR_ID)
        .map_err(|e| e.to_string())?;
    let value: Option<JsonValue> = config::read_json(&path).map_err(|e| e.to_string())?;
    let value = value.unwrap_or_else(|| json!({}));
    Ok(AiConfigView {
        base_url: value
            .get("base_url")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        api_key: value
            .get("api_key")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        model: value
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or("deepseek-chat")
            .to_string(),
    })
}

#[tauri::command]
pub async fn set_ai_config(
    base_url: String,
    api_key: String,
    model: String,
) -> CmdResult<AiConfigView> {
    let path = config::paths::plugin_config_json(AI_ORCHESTRATOR_ID)
        .map_err(|e| e.to_string())?;
    let value = json!({
        "base_url": base_url,
        "api_key": api_key,
        "model": model,
    });
    config::write_json(&path, &value).map_err(|e| e.to_string())?;
    Ok(AiConfigView {
        base_url,
        api_key,
        model,
    })
}

/// 测试 AI API 连接：发送一个最小请求验证 base_url + api_key 是否可用。
#[tauri::command]
pub async fn test_ai_connection(
    base_url: String,
    api_key: String,
    model: String,
) -> CmdResult<serde_json::Value> {
    let base_url = base_url.trim_end_matches('/');
    let chat_url = format!("{}/chat/completions", base_url);

    let body = json!({
        "model": if model.is_empty() { "deepseek-chat" } else { &model },
        "messages": [{"role": "user", "content": "hi"}],
        "max_tokens": 1,
    });

    let client = reqwest::Client::new();
    let resp = client
        .post(&chat_url)
        .header("Content-Type", "application/json")
        .header(
            "Authorization",
            format!("Bearer {}", if api_key.is_empty() { "none" } else { &api_key }),
        )
        .json(&body)
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await
        .map_err(|e| format!("连接失败：{}", e))?;

    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();

    if status.is_success() {
        // 解析模型名用于展示
        let parsed: Option<serde_json::Value> = serde_json::from_str(&text).ok();
        let model_used = parsed
            .as_ref()
            .and_then(|v| v.get("model"))
            .and_then(|m| m.as_str())
            .unwrap_or(&model);
        Ok(json!({
            "ok": true,
            "message": "连接成功",
            "model": model_used,
            "status": status.as_u16(),
        }))
    } else {
        // 尝试解析错误信息
        let error_msg = parsed_error(&text, status.as_u16());
        Ok(json!({
            "ok": false,
            "message": error_msg,
            "status": status.as_u16(),
        }))
    }
}

fn parsed_error(text: &str, status: u16) -> String {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(text) {
        if let Some(msg) = v.get("error").and_then(|e| e.get("message")).and_then(|m| m.as_str()) {
            return format!("HTTP {} — {}", status, msg);
        }
    }
    format!("HTTP {}", status)
}

// ─────────────────── 屏幕取色 ───────────────────

/// 取色器面板用的像素颜色 DTO。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PixelColorView {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub hex: String,
}

/// 获取屏幕指定坐标处的像素颜色，供取色器覆盖层实时调用。
///
/// 坐标是物理像素（覆盖层已乘过 devicePixelRatio）。走 Windows GDI 的
/// GetDC/GetPixel，不启动插件进程，延迟在亚毫秒级——鼠标移动时高频调用
/// 也不会卡。非 Windows 平台返回错误。
#[cfg(windows)]
#[tauri::command]
pub fn get_pixel_color(x: i32, y: i32) -> CmdResult<PixelColorView> {
    use windows_sys::Win32::Graphics::Gdi::{GetDC, GetPixel, ReleaseDC};

    let hdc = unsafe { GetDC(std::ptr::null_mut()) };
    if hdc.is_null() {
        return Err("GetDC 失败".to_string());
    }
    // COLORREF 是 0x00BBGGRR，GetPixel 失败时返回 CLR_INVALID (0xFFFFFFFF)。
    let color = unsafe { GetPixel(hdc, x, y) };
    unsafe { ReleaseDC(std::ptr::null_mut(), hdc) };

    if color == 0xFFFFFFFF {
        return Err("GetPixel 失败（坐标可能超出屏幕范围）".to_string());
    }

    let r = (color & 0xFF) as u8;
    let g = ((color >> 8) & 0xFF) as u8;
    let b = ((color >> 16) & 0xFF) as u8;
    let hex = format!("#{:02X}{:02X}{:02X}", r, g, b);

    Ok(PixelColorView { r, g, b, hex })
}

#[cfg(not(windows))]
#[tauri::command]
pub fn get_pixel_color(_x: i32, _y: i32) -> CmdResult<PixelColorView> {
    Err("屏幕取色仅支持 Windows".to_string())
}

// ─── 剪贴板历史 ───

#[tauri::command]
pub fn get_clipboard_history() -> CmdResult<Vec<crate::clipboard::ClipboardEntry>> {
    Ok(crate::clipboard::get_history())
}

#[tauri::command]
pub fn copy_clipboard_entry(text: String) -> CmdResult<()> {
    crate::clipboard::copy_entry_to_clipboard(&text)
}

#[tauri::command]
pub fn close_overlay(app: tauri::AppHandle) -> CmdResult<()> {
    use tauri::Manager;
    if let Some(w) = app.get_webview_window("overlay") {
        w.close().map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// 调用插件回调（第三期 UI 扩展）。
///
/// 当用户在前端 UI 中完成操作后，通过此命令回调插件。
/// 例如：截图框选完成后，将选区坐标发送给 screenshot 插件。
///
/// # 参数
/// - `plugin_id`: 目标插件 ID
/// - `method`: 回调方法名（插件在 `host/uiRequest` 中指定的 `callback_method`）
/// - `args`: 回调参数（如选区坐标、表单数据等）
#[tauri::command]
pub async fn call_plugin_callback(
    plugin_id: String,
    method: String,
    args: serde_json::Value,
    state: State<'_, AppState>,
) -> CmdResult<()> {
    // 通过 Supervisor 调用插件
    state
        .supervisor
        .call_plugin_method(&plugin_id, &method, args)
        .await
        .map_err(|e| e.to_string())?;

    Ok(())
}

/// 获取插件市场配置。
#[tauri::command]
pub async fn get_marketplace_config() -> CmdResult<MarketplaceConfigView> {
    let config = HostConfig::load().map_err(|e| e.to_string())?;
    Ok(MarketplaceConfigView {
        enabled: config.marketplace_enabled,
        url: config.marketplace_url,
    })
}

/// 设置插件市场配置。
#[tauri::command]
pub async fn set_marketplace_config(
    enabled: bool,
    url: String,
) -> CmdResult<()> {
    let mut config = HostConfig::load().map_err(|e| e.to_string())?;
    config.marketplace_enabled = enabled;
    config.marketplace_url = url;
    config.save().map_err(|e| e.to_string())?;
    Ok(())
}

/// 从插件市场获取插件列表。
#[tauri::command]
pub async fn fetch_marketplace_plugins(
    marketplace_url: String,
) -> CmdResult<Vec<MarketplacePluginView>> {
    // 这里简化处理，实际实现需要：
    // 1. 从 marketplace_url 获取 JSON 数据
    // 2. 解析插件列表
    // 3. 返回格式化的插件信息
    // 目前返回空列表作为占位实现
    let _ = marketplace_url;
    Ok(Vec::new())
}

/// 插件市场配置视图。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketplaceConfigView {
    pub enabled: bool,
    pub url: String,
}

/// 插件市场插件视图。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketplacePluginView {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: String,
    pub category: String,
    pub tags: Vec<String>,
    pub homepage: Option<String>,
    pub license: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub file_size: u64,
    pub download_count: u64,
    pub rating: f32,
    pub rating_count: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_label_treats_missing_instance_as_stopped() {
        // 实例表里没有记录与显式 Stopped，对前端应当是同一种展示。
        assert_eq!(state_label(None), "stopped");
        assert_eq!(state_label(Some(InstanceState::Stopped)), "stopped");
    }

    #[test]
    fn state_label_covers_all_states() {
        assert_eq!(state_label(Some(InstanceState::Starting)), "starting");
        assert_eq!(state_label(Some(InstanceState::Idle)), "idle");
        assert_eq!(state_label(Some(InstanceState::Busy)), "busy");
        assert_eq!(state_label(Some(InstanceState::Stopping)), "stopping");
        assert_eq!(state_label(Some(InstanceState::Error)), "error");
    }

    #[test]
    fn log_level_round_trips_through_labels() {
        for level in [
            LogLevel::Error,
            LogLevel::Warn,
            LogLevel::Info,
            LogLevel::Debug,
            LogLevel::Trace,
        ] {
            let label = log_level_label(level);
            assert_eq!(
                parse_log_level(&label).unwrap(),
                level,
                "级别 {label} 未能往返"
            );
        }
    }

    #[test]
    fn parse_log_level_rejects_unknown() {
        let err = parse_log_level("verbose").unwrap_err();
        assert!(err.contains("verbose"), "错误信息应带上原始值：{err}");
    }

    #[test]
    fn settings_view_serializes_empty_dir_as_empty_string() {
        // 前端靠空串判断「未覆盖，走默认目录」，不能变成 null。
        let view = SettingsView {
            plugins_dir: String::new(),
            effective_plugins_dir: "C:\\x".to_string(),
            mcp_enabled: false,
            mcp_token: String::new(),
            mcp_running: false,
            mcp_endpoint: "http://127.0.0.1:7801/mcp".to_string(),
            mcp_snippets: Vec::new(),
            log_level: "info".to_string(),
            close_behavior: "minimize_to_tray".to_string(),
        };
        let json = serde_json::to_value(&view).unwrap();
        assert_eq!(json["plugins_dir"], "");
        assert_eq!(json["mcp_token"], "");
    }

    /// 取出指定客户端的片段并解析。
    fn 解析片段(snippets: &[McpClientSnippet], id: &str) -> JsonValue {
        let found = snippets
            .iter()
            .find(|s| s.id == id)
            .unwrap_or_else(|| panic!("缺少客户端片段：{id}"));
        serde_json::from_str(&found.snippet).expect("片段必须是合法 JSON")
    }

    #[test]
    fn 每个客户端片段都是合法json且带上token() {
        let snippets = mcp_client_snippets("http://127.0.0.1:7801/mcp", "abc123");
        assert!(!snippets.is_empty(), "至少要给出一个客户端片段");
        for s in &snippets {
            let parsed: JsonValue =
                serde_json::from_str(&s.snippet).unwrap_or_else(|e| panic!("{} 片段非法：{e}", s.id));
            assert!(
                s.snippet.contains("abc123"),
                "{} 的片段里没带 Token，复制过去只会 401",
                s.id
            );
            assert!(
                parsed["mcpServers"]["intools"].is_object(),
                "{} 的片段缺少 mcpServers.intools",
                s.id
            );
            assert!(!s.config_path.is_empty(), "{} 没说明往哪儿粘", s.id);
        }
    }

    #[test]
    fn 直连客户端拿到http形态() {
        let snippets = mcp_client_snippets("http://127.0.0.1:7801/mcp", "abc123");
        for id in ["claude-code", "cursor"] {
            let server = &解析片段(&snippets, id)["mcpServers"]["intools"];
            assert_eq!(server["type"], "http", "{id} 应直连");
            assert_eq!(server["url"], "http://127.0.0.1:7801/mcp");
            assert_eq!(server["headers"]["Authorization"], "Bearer abc123");
        }
    }

    #[test]
    fn claude_desktop拿到stdio桥接且绝不含url字段() {
        // 这条是本模块最硬的约束：claude_desktop_config.json 的 schema 只接受 stdio
        // 形态。混进 url/type 会让它解析失败，并在下次保存时把整个 mcpServers 块
        // （连带用户原有的正常条目）一起重写掉——比「不生效」严重得多。
        let snippets = mcp_client_snippets("http://127.0.0.1:7801/mcp", "abc123");
        let server = &解析片段(&snippets, "claude-desktop")["mcpServers"]["intools"];

        assert_eq!(server["command"], "npx", "stdio 形态必须有 command");
        assert!(server["url"].is_null(), "不能带 url");
        assert!(server["type"].is_null(), "不能带 type");
        assert!(server["headers"].is_null(), "不能带 headers");

        let args: Vec<&str> = server["args"]
            .as_array()
            .expect("args 应是数组")
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert!(args.contains(&"http://127.0.0.1:7801/mcp"), "端点要传给桥接");
        assert!(args.contains(&"--allow-http"), "本机明文 HTTP 需显式放行");
        assert!(args.contains(&"-y"), "缺 -y 会卡在 npx 安装确认");

        // Token 必须走 env：Windows 上 args 里的空格不会被转义，
        // 直接拼 "Authorization: Bearer x" 会被拆坏。
        assert_eq!(server["env"]["AUTH_HEADER"], "Bearer abc123");
        assert!(
            args.iter().any(|a| *a == "Authorization:${AUTH_HEADER}"),
            "请求头应引用 env 且冒号后无空格：{args:?}"
        );
        assert!(
            !args.iter().any(|a| a.contains("abc123")),
            "Token 不该出现在 args 里：{args:?}"
        );
    }

    #[test]
    fn 桥接版本被钉死且不含会被cmd吃掉的转义符() {
        // Windows 下 npx 经 cmd.exe 启动，`^` 会被当转义符吞掉，
        // 写成 mcp-remote@^0.8.3 会解析成不存在的版本。
        assert!(MCP_REMOTE_SPEC.contains('@'), "必须钉死版本，不能用 latest");
        assert!(!MCP_REMOTE_SPEC.contains('^'), "不能带 ^：{MCP_REMOTE_SPEC}");
    }

    #[test]
    fn 未生成token时片段给出占位而不是空bearer() {
        // 直接吐 "Bearer " 的话，用户复制过去只会得到一个 401，不知道是自己没开网关。
        let snippets = mcp_client_snippets("http://127.0.0.1:7801/mcp", "");
        for s in &snippets {
            assert!(
                !s.snippet.contains("Bearer \""),
                "{} 出现了空 Bearer",
                s.id
            );
            assert!(
                s.snippet.contains("开启网关"),
                "{} 的占位应提示如何获得 Token",
                s.id
            );
        }
    }
}
