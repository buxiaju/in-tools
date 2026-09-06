#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! 宿主进程入口：装配内核、接线 Tauri、管理生命周期。
//!
//! 本文件是唯一知道「内核」与「界面」两侧如何对接的地方。内核自身对 Tauri
//! 一无所知，全靠这里把 [`ui::UiPrompter`] / [`ui::TauriNotificationSink`]
//! 注入到 trait 缝隙里。

// `commands` / `ui` / `hotkey` 挂在 bin 而非 lib：内核（lib）刻意不依赖 tauri，只通过
// `PermissionPrompter` / `NotificationSink` 等 trait 留缝，从而保持 headless 可测。
// 实测把依赖 tauri 的模块加进 lib 后，`cargo test --lib` 产出的测试二进制会在
// 进程启动阶段就以 0xC0000139（ENTRYPOINT_NOT_FOUND）失败，连纯内核测试一起拖垮；
// 而 `cargo test --bins` 正常。因此 Tauri 适配层一律放在 bin 侧。
mod clipboard;
mod commands;
mod gateway;
mod hotkey;
mod ui;

use std::sync::Arc;

use tauri::menu::{IsMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{Manager, RunEvent, WindowEvent};

use intools::config::{CloseBehavior, HostConfig};
use intools::permission::{PermissionChecker, PermissionStore};
use intools::registry::{seed, Registry};
use intools::runtime::supervisor::{
    AuditSink, NotificationSink, ProcessTransportFactory, Supervisor,
};

use crate::commands::AppState;
use crate::gateway::McpGateway;
use crate::hotkey::ShortcutManager;
use crate::ui::{SharedPrompter, TauriNotificationSink, UiPrompter};

fn main() {
    // 配置要在日志之前读：日志级别本身就存在配置里。这一步失败只能退回默认配置，
    // 因为此时还没有日志可打，也还没有窗口可弹提示。
    let (config, config_error) = match HostConfig::load() {
        Ok(config) => (config, None),
        Err(err) => (HostConfig::default(), Some(err)),
    };
    init_tracing(&config);
    // 但「退回默认配置」这件事必须说出来：否则用户手改配置写坏了一个字符，
    // 表现只是插件目录莫名其妙变回默认值，没有任何线索可查。
    if let Some(err) = config_error {
        tracing::error!(error = %err, "读取宿主配置失败，本次运行使用默认配置");
    }

    let (supervisor, prompter, sink) = match assemble(&config) {
        Ok(parts) => parts,
        Err(err) => {
            // 装配失败通常是家目录不可写或权限库损坏，属于无法降级的硬故障。
            tracing::error!(error = %err, "初始化失败，无法继续");
            panic!("初始化 InTools 失败：{err}");
        }
    };

    let gateway = Arc::new(McpGateway::new());
    // 热键管理器要同时进 `AppState`（供改键命令触发重注册）和 `setup`（注入 AppHandle），
    // 所以在这里造好并共享，而不是让任何一方独占。
    let shortcuts = Arc::new(ShortcutManager::new());

    let startup_supervisor = Arc::clone(&supervisor);
    let exit_supervisor = Arc::clone(&supervisor);
    let startup_gateway = Arc::clone(&gateway);
    let exit_gateway = Arc::clone(&gateway);
    let setup_shortcuts = Arc::clone(&shortcuts);
    // 开机自启只认「上次关机时是开着的」这一个信号。Token 缺失说明配置被手改坏了，
    // 此时不能凭空生成——那会让已配置的客户端集体失效且毫无提示。
    let startup_token = config.mcp_enabled.then(|| config.mcp_token.clone()).flatten();
    // 关窗行为用 copy 而不是 clone——枚举就两个值，stack 上就够了。
    let startup_close_behavior = config.close_behavior;

    tauri::Builder::default()
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .manage(AppState::new(
            Arc::clone(&supervisor),
            Arc::clone(&prompter),
            Arc::clone(&gateway),
            Arc::clone(&shortcuts),
        ))
        .invoke_handler(tauri::generate_handler![
            commands::list_plugins,
            commands::start_plugin,
            commands::stop_plugin,
            commands::set_plugin_enabled,
            commands::uninstall_plugin,
            commands::import_plugin_package,
            commands::reload_plugins,
            commands::list_tools,
            commands::call_tool,
            commands::respond_permission_prompt,
            commands::list_grants,
            commands::revoke_grants,
            commands::set_tool_exposed,
            commands::get_settings,
            commands::save_settings,
            commands::set_mcp_enabled,
            commands::get_ai_config,
            commands::set_ai_config,
            commands::test_ai_connection,
            commands::get_plugin_settings,
            commands::save_plugin_settings,
            commands::get_plugin_shortcut,
            commands::set_plugin_shortcut,
            commands::list_shortcut_bindings,
            commands::get_plugin_docs,
            commands::get_plugin_dev_doc,
            commands::list_mcp_audit,
            commands::get_pixel_color,
            commands::get_clipboard_history,
            commands::copy_clipboard_entry,
            commands::close_overlay,
            commands::call_plugin_callback,
            commands::get_marketplace_config,
            commands::set_marketplace_config,
            commands::fetch_marketplace_plugins,
        ])
        .setup(move |app| {
            // `AppHandle` 只在这里才存在，而 `PermissionChecker` 在它之前就得造好，
            // 所以两个适配器都用 `OnceLock` 留白、到这一步才补上。
            prompter.install(app.handle().clone());
            sink.install(app.handle().clone());

            // 会话授权（`AllowSession`）的语义是「本次运行内有效」，
            // 因此必须在启动时清空，否则上次运行的残留会跳过本该有的询问。
            let sup = Arc::clone(&startup_supervisor);
            tauri::async_runtime::spawn(async move {
                sup.clear_session_grants().await;

                for (plugin_id, err) in sup.start_eager_plugins().await {
                    // 单个 eager 插件起不来不该影响宿主与其他插件，记录后继续。
                    tracing::warn!(plugin_id = %plugin_id, error = %err, "eager 插件启动失败");
                }
            });

            // MCP 网关按上次的开关状态自启。失败（多半是端口被占）只记日志：
            // 宿主本体与界面不该被一个可选的对外端口拖住。
            if let Some(token) = startup_token {
                let gw = Arc::clone(&startup_gateway);
                let sup = Arc::clone(&startup_supervisor);
                tauri::async_runtime::spawn(async move {
                    match commands::start_gateway(gw, sup, token).await {
                        Ok(addr) => tracing::info!(%addr, "MCP 网关已自动开启"),
                        Err(err) => tracing::error!(error = %err, "MCP 网关自动开启失败"),
                    }
                });
            }

            // 系统托盘：关窗口不退出，常驻后台。
            setup_tray(app, &startup_supervisor)?;

            // 剪贴板历史监控：后台轮询系统剪贴板变化。
            clipboard::init();
            clipboard::start_monitoring();

            // 窗口关闭按钮 → 按配置决定隐藏到托盘还是真退出。
            //
            // 区分两种「退出」很关键：
            // - 关闭按钮 = 误点概率高，宿主定位是「常驻后台」，默认隐藏；
            // - 托盘菜单的「退出」= 用户主动行为，照常 `app.exit(0)`。
            if let Some(window) = app.get_webview_window("main") {
                let wc = window.clone();
                window.on_window_event(move |event| {
                    if let WindowEvent::CloseRequested { api, .. } = event {
                        if startup_close_behavior == CloseBehavior::Exit {
                            // 不 prevent_close，让 Tauri 走默认关闭路径，
                            // 会触发 RunEvent::Exit，回收子进程。
                            tracing::info!("窗口关闭 = 退出宿主（按配置）");
                            return;
                        }
                        api.prevent_close();
                        let _ = wc.hide();
                        tracing::info!("窗口已隐藏到托盘（点击关闭按钮）");
                    }
                });
            }

            // 全局快捷键交给 `ShortcutManager`：这里只做首次注册，后续用户改键时
            // 由命令层再调 `reload()`。注册失败不阻断启动——热键是增强项，
            // 少一个键不该让整个宿主起不来，issues 会在设置页里显示给用户。
            setup_shortcuts.install(app.handle().clone(), Arc::clone(&startup_supervisor));
            let report = setup_shortcuts.reload();
            for issue in &report.issues {
                tracing::warn!(
                    plugin_id = %issue.plugin_id,
                    key = %issue.key,
                    reason = %issue.reason,
                    "快捷键未生效"
                );
            }
            tracing::info!(
                registered = report.registered.len(),
                issues = report.issues.len(),
                "全局快捷键注册完成"
            );

            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("构建 InTools 失败")
        .run(move |_app, event| {
            // 退出前必须回收子进程：插件是独立进程，宿主直接结束只会留下孤儿。
            if let RunEvent::Exit = event {
                let sup = Arc::clone(&exit_supervisor);
                let gw = Arc::clone(&exit_gateway);
                tauri::async_runtime::block_on(async move {
                    // 先停网关再停插件：反过来的话，关停途中进来的 MCP 请求会打到
                    // 正在收尾的实例上，徒增一堆无意义的错误日志。
                    gw.stop().await;
                    sup.shutdown_all().await;
                });
            }
        });
}

/// 按配置初始化日志。
///
/// 环境变量 `INTOOLS_LOG` 优先于配置文件——排查问题时不该被迫先改配置再重启。
fn init_tracing(config: &HostConfig) {
    let filter = tracing_subscriber::EnvFilter::try_from_env("INTOOLS_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(config.log_level.as_filter_str()));

    tracing_subscriber::fmt().with_env_filter(filter).init();
}

/// 创建系统托盘图标与菜单。
///
/// 菜单三项：
/// - 「显示窗口」/「退出」——基础动作；
/// - 快捷键工具子菜单（分隔线下）——每个拥有全局快捷键的插件一项，点一下
///   直接触发 [`hotkey::dispatch`]，绕开按键，避免「我明明按了快捷键却没反应」
///   时用户只能重启宿主。
fn setup_tray(
    app: &tauri::App,
    supervisor: &Arc<Supervisor<SharedPrompter>>,
) -> Result<(), Box<dyn std::error::Error>> {
    use intools::config::UserShortcuts;

    let show_i = MenuItem::with_id(app, "show", "显示窗口", true, None::<&str>)?;
    let quit_i = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
    let sep = tauri::menu::PredefinedMenuItem::separator(app)?;
    let shortcuts_header = MenuItem::with_id(
        app,
        "shortcuts-header",
        "快捷键工具（点击直接触发）",
        false,
        None::<&str>,
    )?;

    // 收集当前生效的快捷键 → 菜单项。复用 `shortcut::resolve_bindings` 的解析
    // 逻辑——和真正的热键注册走的是同一份清单，避免两边漂移。
    let manifests = supervisor.list_plugins_with_manifests();
    let view: Vec<_> = manifests.iter().map(|(id, m)| (id.as_str(), m)).collect();
    let user = UserShortcuts::load().unwrap_or_default();
    let outcome = intools::shortcut::resolve_bindings(&view, &user);

    // 准备菜单项集合，闭包（`on_menu_event`）要用，所以一次性算好。
    let mut items: Vec<Box<dyn IsMenuItem<tauri::Wry>>> = vec![
        Box::new(show_i),
        Box::new(sep),
        Box::new(shortcuts_header),
    ];
    for binding in &outcome.bindings {
        let plugin_name = manifests
            .iter()
            .find(|(id, _)| id == &binding.plugin_id)
            .map_or(binding.plugin_id.clone(), |(_, m)| m.plugin.name.clone());
        let label = format!("{}（{}）", plugin_name, binding.key);
        let item = MenuItem::with_id(
            app,
            format!("shortcut:{}:{}", binding.plugin_id, binding.tool),
            label,
            true,
            None::<&str>,
        )?;
        items.push(Box::new(item));
    }
    items.push(Box::new(PredefinedMenuItem::separator(app)?));
    items.push(Box::new(quit_i));

    let refs: Vec<&dyn IsMenuItem<tauri::Wry>> = items.iter().map(|b| &**b).collect();
    let menu = Menu::with_items(app, &refs)?;

    // 菜单事件分发。`shortcut:` 前缀的 id 走 `dispatch`，其他维持原行为。
    let sup = Arc::clone(supervisor);
    let _tray = TrayIconBuilder::with_id("main-tray")
        .icon(app.default_window_icon().unwrap().clone())
        .tooltip("InTools 插件宿主")
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(move |app, event| match event.id().as_ref() {
            "show" => {
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.show();
                    let _ = window.set_focus();
                }
            }
            "quit" => {
                app.exit(0);
            }
            id if id.starts_with("shortcut:") => {
                // id 形如 `shortcut:{plugin_id}:{tool}`。把后半段撕开喂给 dispatch。
                let rest = &id["shortcut:".len()..];
                if let Some((plugin_id, tool)) = rest.split_once(':') {
                    let ui = outcome
                        .bindings
                        .iter()
                        .find(|b| b.plugin_id == plugin_id && b.tool == tool)
                        .and_then(|b| b.ui.clone());
                    hotkey::dispatch(app, &sup, tool, ui.as_deref());
                }
            }
            _ => {}
        })
        .build(app)?;
    Ok(())
}

/// [`assemble`] 的产物：内核 + 两个 Tauri 适配器。
///
/// 抽成别名只为把 `assemble` 的签名压回可读范围，没有额外语义。
type Assembled = (
    Arc<Supervisor<SharedPrompter>>,
    Arc<UiPrompter>,
    Arc<TauriNotificationSink>,
);

/// 组装内核三件套与两个 Tauri 适配器。
///
/// 返回的 `Supervisor` 已完成 `install_self_ref`，可以处理反向 RPC。
///
/// 注意这里**不**再提取快捷键绑定：绑定要随「用户改键」变化，因此改为由
/// [`ShortcutManager::reload`] 每次从 `Supervisor` 现场读取 manifest，
/// 而不是在启动时抄一份快照——快照一旦被 registry 消费就再也刷不新了。
fn assemble(config: &HostConfig) -> Result<Assembled, Box<dyn std::error::Error>> {
    let plugins_root = config.effective_plugins_dir()?;

    // 全新安装时 `~/.intools/plugins` 并不存在，而扫描器对此是静默返回空列表的，
    // 结果就是「装完打开什么都没有」。播种失败不该拦住启动：大不了回到空列表，
    // 用户仍可自己往目录里放插件。
    //
    // 播种目标改为 `plugins/system/`：系统插件与用户插件隔离存放。
    match seed::seed_builtin_plugins(&plugins_root) {

        Ok(seed::SeedOutcome::Seeded { plugins }) => {
            tracing::info!(
                plugins,
                dir = %plugins_root.display(),
                "已播种内置插件"
            );
        }
        Ok(seed::SeedOutcome::Skipped(reason)) => {
            tracing::debug!(reason = %reason, "跳过内置插件播种");
        }
        Err(err) => {
            tracing::warn!(
                dir = %plugins_root.display(),
                error = %err,
                "内置插件播种失败，继续以现有插件目录启动"
            );
        }
    }

    // 插件目录扫不动（不存在、无权限）不该让宿主起不来：退化成空注册表后，
    // 用户仍能进设置页改目录，比直接崩掉可用得多。
    //
    // 扫描三个分类目录：system → test → user。同 id 冲突时先到先得（system 优先）。
    let registry = match Registry::scan_and_build_categorized(&plugins_root) {
        Ok(registry) => registry,
        Err(err) => {
            tracing::warn!(
                dir = %plugins_root.display(),
                error = %err,
                "插件目录扫描失败，以空注册表启动"
            );
            Registry::default()
        }
    };

    for failure in registry.load_failures() {
        tracing::warn!(error = %failure, "插件加载失败，已跳过");
    }
    for conflict in registry.conflicts() {
        tracing::warn!(
            tool = %conflict.tool_name,
            owner = %conflict.owner_plugin,
            rejected = %conflict.rejected_from,
            "工具名冲突，后者被拒绝"
        );
    }
    tracing::info!(
        plugins = registry.plugin_ids().len(),
        dir = %plugins_root.display(),
        "插件注册表就绪"
    );

    let prompter = Arc::new(UiPrompter::new());
    let sink = Arc::new(TauriNotificationSink::new());

    // `PermissionChecker` 按值持有 prompter，而 command 层还要用它 `resolve`
    // 前端的答复，所以传 `Arc` 的克隆而不是所有权。
    let store = PermissionStore::open_default()?;
    let checker = PermissionChecker::new(store, Arc::clone(&prompter));

    // 审计 sink 是**装饰器**：`McpAuditSink` 把 `mcp:` 开头的调用额外落盘，
    // 其余照常转发给内层的 tracing sink。`with_audit_sink` 是替换语义，
    // 所以必须这样套，否则 UI 与插件调用的审计会被一起吞掉。
    // 拿不到日志目录（家目录不可写）时退化成不落盘，不阻断启动。
    let audit: Option<Arc<dyn AuditSink>> = match intools::mcp::audit::McpAuditSink::new() {
        Ok(sink) => Some(Arc::new(sink)),
        Err(err) => {
            tracing::warn!(error = %err, "MCP 审计日志不可用，本次运行只记 tracing");
            None
        }
    };

    let mut supervisor = Supervisor::new(registry, checker, Arc::new(ProcessTransportFactory))
        .with_notification_sink(Arc::clone(&sink) as Arc<dyn NotificationSink>)
        .with_disabled_plugins(config.disabled_plugins.clone());
    if let Some(audit) = audit {
        supervisor = supervisor.with_audit_sink(audit);
    }
    let supervisor = Arc::new(supervisor);
    supervisor.install_self_ref();

    Ok((supervisor, prompter, sink))
}
