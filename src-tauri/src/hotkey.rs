//! 全局热键的 Tauri 适配层：把 [`intools::shortcut`] 算出来的绑定真正注册到系统。
//!
//! 与 lib 侧 `shortcut` 模块的分工是刻意的：
//!
//! - `intools::shortcut`（纯函数）负责**该绑什么键**——归一化、冲突裁决、来源判定，
//!   全部可在单测里跑，不需要真的占用一个系统热键。
//! - 本模块负责**怎么绑上去**——持有 `AppHandle`、调 `unregister_all` / `on_shortcut`、
//!   按下时派发工具调用。这一层没法单测（要真实事件循环），所以刻意压到最薄。
//!
//! 之所以要有一个常驻的 [`ShortcutManager`] 而不是像先前那样在 `setup` 里一次性
//! for 循环注册完：用户在设置页改键后要求**立即生效**。而 `global-shortcut` 插件
//! 只提供「注册」与「全部注销」，没有「替换单个」，因此每次变更都得
//! 「全部注销 → 按最新结果全部重注册」。这个动作要能从命令线程反复触发，
//! 就必须有个地方存住 `AppHandle` 和 `Supervisor`，这就是本结构体存在的理由。

use std::sync::{Arc, Mutex, OnceLock};

use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

use intools::config::UserShortcuts;
use intools::permission::CallerIdentity;
use intools::runtime::supervisor::{Supervisor, ToolInvoker};
use intools::shortcut::{self, ResolveOutcome, ResolvedBinding, ShortcutIssue};

use crate::ui::SharedPrompter;

/// 一次「重新注册全部热键」的结果。
///
/// 注意 `issues` 混合了两类失败：绑定解析阶段的（键写错、插件间撞键）和系统注册
/// 阶段的（键被别的程序占了）。对用户来说这两类的表现完全一样——「这个键没生效」，
/// 所以合并成一个列表，由 `reason` 说明具体原因。
#[derive(Debug, Clone, Default)]
pub struct ApplyReport {
    /// 真正注册成功、当前生效的绑定。
    pub registered: Vec<ResolvedBinding>,
    /// 未能生效的绑定及原因。
    pub issues: Vec<ShortcutIssue>,
}

/// 全局热键管理器。
///
/// `AppHandle` 与 `Supervisor` 都用 [`OnceLock`] 留白：本结构体在 `tauri::Builder`
/// 之前就得造好（要塞进 `AppState`），而 `AppHandle` 只在 `setup` 回调里才存在。
/// 这与 [`crate::ui::UiPrompter`] 是同一套路子。
///
/// 不派生 `Debug`：`Supervisor` 没实现它，而热键管理器也没有值得打印的状态——
/// 需要看绑定时用 [`Self::active_bindings`]。
#[derive(Default)]
pub struct ShortcutManager {
    app: OnceLock<AppHandle>,
    supervisor: OnceLock<Arc<Supervisor<SharedPrompter>>>,
    /// 当前生效的绑定快照，供界面回显「谁占着哪个键」。
    active: Mutex<Vec<ResolvedBinding>>,
}

impl ShortcutManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// 注入运行期依赖。在 Tauri 的 `setup` 回调里调用；重复调用只有首次生效。
    pub fn install(&self, app: AppHandle, supervisor: Arc<Supervisor<SharedPrompter>>) {
        let _ = self.app.set(app);
        let _ = self.supervisor.set(supervisor);
    }

    /// 当前生效的绑定快照。
    pub fn active_bindings(&self) -> Vec<ResolvedBinding> {
        self.active
            .lock()
            .expect("active bindings 锁不应被 poison")
            .clone()
    }

    /// 重新读取 manifest 与用户覆盖，全量重注册热键。
    ///
    /// 幂等：连续调用两次的最终状态与调用一次相同。改键、装插件、卸插件后都该调它。
    ///
    /// 读 `shortcuts.json` 失败时按「无用户覆盖」处理而非直接返回错误：
    /// 这个文件是用户可手改的，写坏了一个逗号不该让**所有**热键（包括插件自带默认键）
    /// 一起失效——那种表现很难让人联想到是自己改坏了 JSON。
    pub fn reload(&self) -> ApplyReport {
        let Some(supervisor) = self.supervisor.get() else {
            tracing::warn!("热键管理器尚未 install，跳过本次重注册");
            return ApplyReport::default();
        };

        let user = match UserShortcuts::load() {
            Ok(user) => user,
            Err(err) => {
                tracing::warn!(error = %err, "读取 shortcuts.json 失败，本次按无用户覆盖处理");
                UserShortcuts::default()
            }
        };

        let manifests = supervisor.list_plugins_with_manifests();
        // 同 `list_shortcut_bindings`：owned 数据借一层视图喂给纯函数。
        let view: Vec<_> = manifests.iter().map(|(id, m)| (id.as_str(), m)).collect();
        let outcome = shortcut::resolve_bindings(&view, &user);
        self.apply(outcome)
    }

    /// 把解析结果落到系统上：先全部注销，再逐条注册。
    ///
    /// 「先全部注销」是必须的——插件的 `unregister_all` 会清空它自己的 handler 表，
    /// 否则改键后旧键仍然活着，用户会看到「新键能用、旧键也还能用」。
    fn apply(&self, outcome: ResolveOutcome) -> ApplyReport {
        let Some(app) = self.app.get() else {
            tracing::warn!("AppHandle 尚未注入，跳过本次热键注册");
            return ApplyReport::default();
        };

        // 整个「注销 + 注册」序列持锁，避免两个并发的 reload 交错执行
        // 导致注册表与 `active` 快照对不上。
        let mut active = self.active.lock().expect("active bindings 锁不应被 poison");

        let gs = app.global_shortcut();
        if let Err(err) = gs.unregister_all() {
            // 注销失败通常意味着系统侧状态已经不可信，但仍继续尝试注册：
            // 最坏结果是旧键残留，比一个热键都没有要好。
            tracing::warn!(error = %err, "注销旧热键失败，继续尝试注册新热键");
        }

        let mut report = ApplyReport {
            registered: Vec::new(),
            issues: outcome.issues,
        };

        for binding in outcome.bindings {
            let sup = Arc::clone(
                self.supervisor
                    .get()
                    .expect("supervisor 与 app 同时 install"),
            );
            let tool = binding.tool.clone();
            let ui = binding.ui.clone();
            let handle = app.clone();

            let result = gs.on_shortcut(binding.key.as_str(), move |_app, _sc, event| {
                // 只认按下。不过滤的话一次敲击会触发两回（按下 + 抬起）。
                if event.state() != ShortcutState::Pressed {
                    return;
                }
                dispatch(&handle, &sup, &tool, ui.as_deref());
            });

            match result {
                Ok(()) => {
                    tracing::info!(
                        key = %binding.key,
                        tool = %binding.tool,
                        plugin_id = %binding.plugin_id,
                        source = binding.source.as_str(),
                        "已注册全局快捷键"
                    );
                    report.registered.push(binding);
                }
                Err(err) => {
                    // 走到这里说明键本身合法（已过 normalize_key）但系统拒绝，
                    // 绝大多数情况是被别的常驻程序抢先占用了。
                    tracing::warn!(
                        key = %binding.key,
                        plugin_id = %binding.plugin_id,
                        error = %err,
                        "注册全局快捷键失败"
                    );
                    report.issues.push(ShortcutIssue {
                        plugin_id: binding.plugin_id,
                        key: binding.key,
                        owner_plugin: None,
                        reason: format!("系统拒绝注册该快捷键，可能已被其他程序占用（{err}）"),
                    });
                }
            }
        }

        *active = report.registered.clone();
        report
    }
}

/// 热键按下后的实际动作。
///
/// 两条分支的差别在于「参数从哪来」：
/// - 声明了 `ui` 的插件，参数要由用户在覆盖层里现场给（如框选区域坐标），
///   所以这里只负责把界面打开，真正的 `call_tool` 由覆盖层的 JS 发起；
/// - 没声明 `ui` 的插件，参数只能取自它自己的设置项，直接调用即可。
pub(crate) fn dispatch(
    handle: &AppHandle,
    sup: &Arc<Supervisor<SharedPrompter>>,
    tool: &str,
    ui: Option<&str>,
) {
    if ui == Some("region-select") {
        match show_region_select_overlay(handle, tool) {
            Ok(()) => tracing::info!(tool = %tool, "已打开框选覆盖层"),
            Err(err) => tracing::warn!(tool = %tool, error = %err, "打开框选覆盖层失败"),
        }
        return;
    }

    if let Some(other) = ui {
        // manifest 校验只保证 `ui` 是个字符串，不保证宿主认得。认不出来时降级为
        // 直接调用而不是罢工：插件的核心功能仍可用，只是少了交互界面。
        tracing::warn!(tool = %tool, ui = %other, "未知的 ui 类型，降级为直接调用");
    }

    let tool = tool.to_string();
    let sup = Arc::clone(sup);
    tauri::async_runtime::spawn(async move {
        tracing::info!(tool = %tool, "快捷键触发工具调用");
        // 快捷键路径没有界面可填参数，只能把该插件的设置项整体当参数传进去。
        let args = match sup.plugin_id_for_tool(&tool) {
            Some(pid) => match intools::config::load_plugin_settings(&pid) {
                Ok(map) if !map.is_empty() => serde_json::Value::Object(map),
                _ => serde_json::json!({}),
            },
            None => serde_json::json!({}),
        };
        match sup.call_tool(&tool, args, CallerIdentity::Ui).await {
            Ok(_) => tracing::info!(tool = %tool, "快捷键工具调用成功"),
            Err(err) => tracing::warn!(tool = %tool, error = %err, "快捷键工具调用失败"),
        }
    });
}

/// 创建全屏透明覆盖层，供用户拖拽框选截图区域。
///
/// 覆盖层加载 `overlay.html?tool=<tool_name>`，JS 端处理鼠标拖拽选区，
/// 选定后调用 `call_tool` 命令（携带 region 坐标），然后自行关闭窗口。
/// 若覆盖层已存在（用户连按快捷键），先关闭旧的再创建新的。
fn show_region_select_overlay(
    handle: &AppHandle,
    tool: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(existing) = handle.get_webview_window("overlay") {
        let _ = existing.close();
    }

    // 工具名含 `:`，直接拼进 query 会被部分平台的 URL 解析器截断，故先转义。
    let url = format!("overlay.html?tool={}", tool.replace(':', "%3A"));
    let _overlay = WebviewWindowBuilder::new(handle, "overlay", WebviewUrl::App(url.into()))
        .title("截图选区")
        .decorations(false)
        .fullscreen(true)
        .always_on_top(true)
        .transparent(true)
        .skip_taskbar(true)
        .build()?;

    Ok(())
}
