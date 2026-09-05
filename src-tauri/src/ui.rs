//! 内核 trait 到 Tauri 的适配层。
//!
//! 内核（`permission` / `runtime`）刻意不依赖 `tauri`，所有面向界面的出口都是
//! trait：[`PermissionPrompter`]、[`NotificationSink`]。本模块提供它们的生产实现，
//! 把「事件推给前端」这件事集中在一处。
//!
//! 这里要解决一个时序矛盾：`PermissionChecker` 在构造 `Supervisor` 时就需要一个
//! prompter，而 `AppHandle` 要等 Tauri 的 `setup` 回调才拿得到。解法与
//! `Supervisor::install_self_ref` 一致——先建好空壳，`setup` 里再注入
//! `AppHandle`（[`UiPrompter::install`]、[`TauriNotificationSink::install`]）。

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};
use tokio::sync::oneshot;

use intools::permission::{PermissionPrompter, PromptDecision, PromptRequest};
use intools::protocol::manifest::DangerLevel;
use intools::runtime::supervisor::{NotificationSink, PluginNotification};

/// 权限询问事件名。前端监听它弹窗。
pub const EVENT_PERMISSION_PROMPT: &str = "intools://permission-prompt";

/// 插件通知事件名。`notify/stream` 等经由它渲染到对话页。
pub const EVENT_PLUGIN_NOTIFICATION: &str = "intools://plugin-notification";

// ─────────────────── 权限询问 ───────────────────

/// 推给前端的一次授权询问。
///
/// 不直接复用 [`PromptRequest`]：那是内核类型，`Permission` 与 `CallerIdentity`
/// 都是结构化枚举，前端只需要能直接显示的扁平字符串。多写一个 DTO
/// 换来内核不必为了 IPC 而扭曲自己的形状。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptPayload {
    /// 本次询问的唯一 id，前端答复时必须带回。
    pub id: String,
    pub plugin_id: String,
    /// 权限字符串，形如 `screen:capture`。
    pub permission: String,
    pub danger: DangerLevel,
    /// 调用方标识，形如 `ui` / `plugin:com.example.ai@1`。
    pub caller: String,
    /// 触发此次询问的工具名（如 `ocr:recognize`）；与具体工具无关时为 `None`。
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub tool: Option<String>,
    /// 参数的人类可读摘要（如 `{"path":"a.png","size":42}`）。
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub args_summary: Option<String>,
}

/// 用户对询问的答复通道：id → oneshot sender。
type PendingPrompts = Mutex<HashMap<String, oneshot::Sender<PromptDecision>>>;

/// 走 Tauri 弹窗的 [`PermissionPrompter`] 实现。
///
/// 一次询问的完整流程：
/// 1. `ask` 生成 id，登记一个 oneshot，emit 事件；
/// 2. 前端弹窗，用户点选，调用 `respond_permission_prompt` command；
/// 3. command 转调 [`UiPrompter::resolve`]，唤醒第 1 步的 await。
///
/// 若 `AppHandle` 尚未注入或前端窗口已关闭，一律按 [`PromptDecision::DenyOnce`]
/// 处理——**拿不到用户确认时必须拒绝**，绝不能默认放行。选 `DenyOnce`
/// 而不是 `DenyAlways`，是因为「界面没起来」不代表用户想永久拒绝。
#[derive(Debug, Default)]
pub struct UiPrompter {
    app: OnceLock<AppHandle>,
    pending: PendingPrompts,
}

impl UiPrompter {
    pub fn new() -> Self {
        Self::default()
    }

    /// 注入 `AppHandle`。在 Tauri 的 `setup` 回调里调用；重复调用只有首次生效。
    pub fn install(&self, app: AppHandle) {
        let _ = self.app.set(app);
    }

    /// 前端答复的落点。返回 `false` 表示该 id 不存在（超时或重复答复），调用方可忽略。
    pub fn resolve(&self, id: &str, decision: PromptDecision) -> bool {
        let sender = self
            .pending
            .lock()
            .expect("pending prompts 锁不应被 poison")
            .remove(id);
        match sender {
            // send 失败说明等待方已经不在了（比如调用超时被丢弃），不算错误。
            Some(tx) => tx.send(decision).is_ok(),
            None => false,
        }
    }

    /// 当前待答复的询问数。仅测试用，故加 cfg 门控，避免正式构建里的死代码警告。
    #[cfg(test)]
    pub fn pending_count(&self) -> usize {
        self.pending
            .lock()
            .expect("pending prompts 锁不应被 poison")
            .len()
    }
}

#[async_trait::async_trait]
impl PermissionPrompter for UiPrompter {
    async fn ask(&self, request: &PromptRequest) -> PromptDecision {
        let Some(app) = self.app.get() else {
            tracing::warn!(
                plugin = %request.plugin_id,
                permission = %request.permission,
                "界面未就绪，无法询问授权，按拒绝处理"
            );
            return PromptDecision::DenyOnce;
        };

        let id = uuid::Uuid::new_v4().simple().to_string();
        let payload = PromptPayload {
            id: id.clone(),
            plugin_id: request.plugin_id.clone(),
            permission: request.permission.to_string(),
            danger: request.danger,
            caller: request.caller.label(),
            tool: request.tool.clone(),
            args_summary: request.args_summary.clone(),
        };

        let (tx, rx) = oneshot::channel();
        self.pending
            .lock()
            .expect("pending prompts 锁不应被 poison")
            .insert(id.clone(), tx);

        if let Err(e) = app.emit(EVENT_PERMISSION_PROMPT, &payload) {
            // emit 失败要把登记项清掉，否则 pending 表会永久泄漏一条。
            self.pending
                .lock()
                .expect("pending prompts 锁不应被 poison")
                .remove(&id);
            tracing::warn!(error = %e, "推送授权询问失败，按拒绝处理");
            return PromptDecision::DenyOnce;
        }

        // 这行日志是排查「弹窗没出现」的唯一抓手：有它说明事件已送达前端，
        // 问题在界面侧；没有它说明根本没走到询问这一步。
        tracing::info!(
            prompt_id = %id,
            plugin = %request.plugin_id,
            permission = %request.permission,
            "已推送授权询问，等待用户答复"
        );

        // rx 出错只可能是 sender 被 drop（窗口关闭等），同样按拒绝处理。
        match rx.await {
            Ok(decision) => {
                tracing::info!(prompt_id = %id, ?decision, "收到授权答复");
                decision
            }
            Err(_) => {
                self.pending
                    .lock()
                    .expect("pending prompts 锁不应被 poison")
                    .remove(&id);
                tracing::warn!(plugin = %request.plugin_id, "授权询问未获答复，按拒绝处理");
                PromptDecision::DenyOnce
            }
        }
    }
}

// ─────────────────── 插件通知 ───────────────────

/// 把插件通知 emit 到前端的 [`NotificationSink`] 实现。
///
/// 与 [`UiPrompter`] 同样支持延迟注入 `AppHandle`；未注入时退化为写日志，
/// 行为与 Phase 6 的 `TracingNotificationSink` 一致。
#[derive(Debug, Default)]
pub struct TauriNotificationSink {
    app: OnceLock<AppHandle>,
}

impl TauriNotificationSink {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn install(&self, app: AppHandle) {
        let _ = self.app.set(app);
    }
}

impl NotificationSink for TauriNotificationSink {
    fn emit(&self, notification: PluginNotification) {
        let Some(app) = self.app.get() else {
            tracing::info!(
                plugin = %notification.plugin_id,
                method = %notification.method,
                "界面未就绪，插件通知仅记日志"
            );
            return;
        };
        if let Err(e) = app.emit(EVENT_PLUGIN_NOTIFICATION, &notification) {
            tracing::warn!(error = %e, "推送插件通知失败");
        }
    }
}

/// [`UiPrompter`] 与 [`TauriNotificationSink`] 都要在 `setup` 里注入 `AppHandle`，
/// 打包成一个别名省得 `main.rs` 到处写 `Arc<...>`。
pub type SharedPrompter = Arc<UiPrompter>;

// ─────────────────── 单元测试 ───────────────────
#[cfg(test)]
mod tests {
    use super::*;
    use intools::permission::CallerIdentity;
    use intools::protocol::manifest::Permission;

    fn request() -> PromptRequest {
        PromptRequest {
            plugin_id: "com.example.demo".to_string(),
            permission: Permission::parse("screen:capture").unwrap(),
            danger: DangerLevel::High,
            caller: CallerIdentity::Ui,
            tool: Some("screenshot:capture".to_string()),
            args_summary: Some(r#"{"region":"full"}"#.to_string()),
        }
    }

    #[tokio::test]
    async fn ask_without_app_handle_denies_once() {
        // 界面没起来时绝不能默认放行；同时也不该永久拒绝。
        let prompter = UiPrompter::new();
        assert_eq!(prompter.ask(&request()).await, PromptDecision::DenyOnce);
        // 失败路径不应留下待答复项。
        assert_eq!(prompter.pending_count(), 0);
    }

    #[test]
    fn resolve_unknown_id_returns_false() {
        let prompter = UiPrompter::new();
        assert!(!prompter.resolve("no-such-id", PromptDecision::AllowAlways));
    }

    #[test]
    fn prompt_payload_serializes_flat_strings() {
        let payload = PromptPayload {
            id: "abc".to_string(),
            plugin_id: "com.example.demo".to_string(),
            permission: "file:read:~/Pictures".to_string(),
            danger: DangerLevel::Medium,
            caller: "plugin:com.example.ai@1".to_string(),
            tool: Some("fs:read".to_string()),
            args_summary: Some(r#"{"path":"a.png"}"#.to_string()),
        };
        let json = serde_json::to_value(&payload).unwrap();
        assert_eq!(json["permission"], "file:read:~/Pictures");
        assert_eq!(json["danger"], "medium");
        assert_eq!(json["caller"], "plugin:com.example.ai@1");
    }

    #[test]
    fn prompt_decision_round_trips_kebab_case() {
        let json = serde_json::to_string(&PromptDecision::AllowSession).unwrap();
        assert_eq!(json, "\"allow-session\"");
        let back: PromptDecision = serde_json::from_str("\"deny-always\"").unwrap();
        assert_eq!(back, PromptDecision::DenyAlways);
    }
}
