//! 权限层：调用方身份与三级危险度授权判定。
//!
//! 权限声明的粒度是**插件级**：`manifest.toml` 的 `[capabilities] permissions`
//! 适用于该插件的所有工具，宿主不区分单个工具需要哪些权限。
//!
//! 持久化格式（`~/.intools/permissions.json`）：
//!
//! ```json
//! {
//!   "com.example.ocr": {
//!     "screen:capture": { "granted": true, "scope": "always", "at": "2026-09-02T10:00:00Z" },
//!     "file:read": { "granted": true, "scope": "always", "paths": ["~/Pictures"] }
//!   }
//! }
//! ```
//!
//! 外层键是插件 id，内层键是权限字符串（`Permission` 的 `Display` 形式）。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::config::{self, JsonIoError, PathError};
use crate::protocol::manifest::{DangerLevel, Permission};

// ─────────────────── 调用方身份 ───────────────────

/// 调用方身份。UI、AI 编排插件、MCP 客户端三条路径共用同一个调用通道，
/// 差别只体现在这里——权限判定与审计日志都按身份区分。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallerIdentity {
    /// 宿主界面发起，用户在场，可以弹窗询问。
    Ui,
    /// 另一个插件通过反向 RPC 发起（如 AI 编排插件）。`depth` 是当前调用链深度。
    Plugin { id: String, depth: u8 },
    /// 第三方 AI 工具经 MCP 网关发起，用户不在场，不能弹窗。
    Mcp { client_name: String },
}

impl CallerIdentity {
    /// 当前调用链深度。UI 与 MCP 是链条起点，深度为 0。
    pub fn depth(&self) -> u8 {
        match self {
            CallerIdentity::Ui | CallerIdentity::Mcp { .. } => 0,
            CallerIdentity::Plugin { depth, .. } => *depth,
        }
    }

    /// 是否可以向用户弹窗询问授权。MCP 调用时用户不在场，只能依赖既有授权记录。
    pub fn can_prompt(&self) -> bool {
        matches!(self, CallerIdentity::Ui | CallerIdentity::Plugin { .. })
    }

    /// 审计日志用的稳定标识。
    pub fn label(&self) -> String {
        match self {
            CallerIdentity::Ui => "ui".to_string(),
            CallerIdentity::Plugin { id, depth } => format!("plugin:{id}@{depth}"),
            CallerIdentity::Mcp { client_name } => format!("mcp:{client_name}"),
        }
    }
}

// ─────────────────── 授权记录 ───────────────────

/// 授权的有效范围。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GrantScope {
    /// 永久记住，落盘。
    Always,
    /// 仅本次会话有效，进程退出即失效，**不落盘**。
    Session,
}

/// 单条授权记录，对应 permissions.json 里内层的一个值。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrantRecord {
    pub granted: bool,
    pub scope: GrantScope,
    pub at: String,
    /// 路径类权限的限定范围，如 `file:read` 只允许 `~/Pictures`。为空时不序列化。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
}

impl GrantRecord {
    /// 永久授权/拒绝。
    pub fn always(granted: bool) -> Self {
        Self {
            granted,
            scope: GrantScope::Always,
            at: now_iso8601(),
            paths: Vec::new(),
        }
    }

    /// 仅本次会话授权/拒绝。
    pub fn session(granted: bool) -> Self {
        Self {
            granted,
            scope: GrantScope::Session,
            at: now_iso8601(),
            paths: Vec::new(),
        }
    }

    pub fn with_paths(mut self, paths: Vec<String>) -> Self {
        self.paths = paths;
        self
    }
}

/// 磁盘上的整体结构：plugin id → 权限字符串 → 记录。
type GrantMap = BTreeMap<String, BTreeMap<String, GrantRecord>>;

// ─────────────────── 存储 ───────────────────

/// 授权记录存储。
///
/// **路径由外部注入**：`config::data_root()` 直接读用户主目录且没有环境变量覆盖口子，
/// 若在此内部硬编码调用，单测就会污染真实的 `~/.intools/permissions.json`。
/// 生产代码走 [`PermissionStore::open_default`]，测试走 [`PermissionStore::open_at`]。
#[derive(Debug)]
pub struct PermissionStore {
    path: PathBuf,
    /// 落盘部分，只含 `GrantScope::Always`。
    persisted: GrantMap,
    /// 会话部分，仅存活于内存。
    session: GrantMap,
}

impl PermissionStore {
    /// 从指定路径加载。文件不存在时得到空存储，首次 `record` 时才创建文件。
    pub fn open_at(path: impl Into<PathBuf>) -> Result<Self, PermissionError> {
        let path = path.into();
        let persisted: GrantMap = config::read_json(&path)?.unwrap_or_default();
        Ok(Self {
            path,
            persisted,
            session: GrantMap::new(),
        })
    }

    /// 从默认位置 `~/.intools/permissions.json` 加载。
    pub fn open_default() -> Result<Self, PermissionError> {
        Self::open_at(config::paths::permissions_json()?)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 查授权记录。会话记录优先于落盘记录——高危权限「本次允许」应当覆盖旧的永久拒绝。
    pub fn lookup(&self, plugin_id: &str, permission: &Permission) -> Option<&GrantRecord> {
        let key = permission.to_string();
        self.session
            .get(plugin_id)
            .and_then(|m| m.get(&key))
            .or_else(|| self.persisted.get(plugin_id).and_then(|m| m.get(&key)))
    }

    /// 写入一条授权记录。`Always` 立即落盘，`Session` 只进内存。
    pub fn record(
        &mut self,
        plugin_id: &str,
        permission: &Permission,
        record: GrantRecord,
    ) -> Result<(), PermissionError> {
        let key = permission.to_string();
        match record.scope {
            GrantScope::Always => {
                // 同一权限的会话记录已被永久决定取代，清掉以免继续遮蔽。
                if let Some(m) = self.session.get_mut(plugin_id) {
                    m.remove(&key);
                }
                self.persisted
                    .entry(plugin_id.to_string())
                    .or_default()
                    .insert(key, record);
                self.flush()?;
            }
            GrantScope::Session => {
                self.session
                    .entry(plugin_id.to_string())
                    .or_default()
                    .insert(key, record);
            }
        }
        Ok(())
    }

    /// 撤销某插件的全部授权（落盘与会话一并清除）。
    pub fn revoke_plugin(&mut self, plugin_id: &str) -> Result<(), PermissionError> {
        self.session.remove(plugin_id);
        if self.persisted.remove(plugin_id).is_some() {
            self.flush()?;
        }
        Ok(())
    }

    /// 清空会话授权。高危权限「每会话首次调用询问」依赖这里。
    pub fn clear_session(&mut self) {
        self.session.clear();
    }

    /// 列出某插件的落盘授权，供设置界面展示。
    pub fn persisted_grants(&self, plugin_id: &str) -> Option<&BTreeMap<String, GrantRecord>> {
        self.persisted.get(plugin_id)
    }

    fn flush(&self) -> Result<(), PermissionError> {
        config::write_json(&self.path, &self.persisted)?;
        Ok(())
    }
}

// ─────────────────── 询问抽象 ───────────────────

/// 一次授权询问的上下文。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptRequest {
    pub plugin_id: String,
    pub permission: Permission,
    pub danger: DangerLevel,
    pub caller: CallerIdentity,
    /// 触发此次询问的工具名，形如 `ocr:recognize`。
    ///
    /// `None` 表示询问与具体工具无关（例如某个插件启动期的全局检查）。
    /// 前端弹窗会把 `None` 渲染成「N/A」而非空白，避免让人怀疑是不是数据丢了。
    pub tool: Option<String>,
    /// 工具调用的参数摘要——给用户展示「为什么这次危险」。
    ///
    /// 由调用方在拼参数前手写一段可读摘要，比如写文件时给出路径和大小，
    /// 而不是把整个 JSON 对象糊在弹窗里。
    /// `None` 同上。
    pub args_summary: Option<String>,
}

/// 用户对一次询问的答复。
///
/// 带 serde 是因为它要从前端弹窗经 Tauri command 传回来；
/// `kebab-case` 与 manifest 里其他枚举的约定保持一致。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PromptDecision {
    /// 拒绝，且记住（永久拒绝）。
    DenyAlways,
    /// 本次拒绝，下次还问。
    DenyOnce,
    /// 仅本次会话允许。
    AllowSession,
    /// 永久允许。
    AllowAlways,
}

/// 授权询问通道。生产实现走 Tauri 弹窗，测试实现给固定答复。
///
/// 设计成 trait 而非直接调 UI，是为了让权限判定逻辑能脱离 GUI 单测。
#[async_trait::async_trait]
pub trait PermissionPrompter: Send + Sync {
    async fn ask(&self, request: &PromptRequest) -> PromptDecision;
}

/// 让 `Arc<P>` 自身也是一个询问器。
///
/// [`PermissionChecker`] 按值持有 prompter，但真实的询问器（如界面弹窗）往往还要
/// 被 command 层共享以回填用户的答复。有了这层转发，同一个 `Arc` 既能交给
/// checker、又能留在手上，无需把内部状态再包一层 `Arc<Mutex<..>>`。
#[async_trait::async_trait]
impl<P: PermissionPrompter + ?Sized> PermissionPrompter for std::sync::Arc<P> {
    async fn ask(&self, request: &PromptRequest) -> PromptDecision {
        (**self).ask(request).await
    }
}

/// 总是给同一答复的询问器，供测试与无人值守场景使用。
#[derive(Debug, Clone, Copy)]
pub struct FixedPrompter(pub PromptDecision);

#[async_trait::async_trait]
impl PermissionPrompter for FixedPrompter {
    async fn ask(&self, _request: &PromptRequest) -> PromptDecision {
        self.0
    }
}

// ─────────────────── 三级判定 ───────────────────

/// 权限判定器：按 [`DangerLevel`] 决定「放行 / 查记录 / 询问」。
///
/// 分级规则本身来自 [`Permission::danger_level`]（Phase 1 已实现），此处不重复定义。
pub struct PermissionChecker<P: PermissionPrompter> {
    store: PermissionStore,
    prompter: P,
}

impl<P: PermissionPrompter> PermissionChecker<P> {
    pub fn new(store: PermissionStore, prompter: P) -> Self {
        Self { store, prompter }
    }

    pub fn store(&self) -> &PermissionStore {
        &self.store
    }

    pub fn store_mut(&mut self) -> &mut PermissionStore {
        &mut self.store
    }

    /// 校验插件声明的全部权限。任一项被拒即整体失败。
    ///
    /// 权限声明是插件级的，因此这里不接收工具名——宿主不区分单个工具需要哪些权限。
    /// `tool` 与 `args_summary` 仅用于弹窗上下文展示，不参与校验逻辑。
    pub async fn check_all(
        &mut self,
        plugin_id: &str,
        declared: &[Permission],
        caller: &CallerIdentity,
        tool: Option<&str>,
        args_summary: Option<&str>,
    ) -> Result<(), PermissionError> {
        for permission in declared {
            self.check_one(plugin_id, permission, caller, tool, args_summary).await?;
        }
        Ok(())
    }

    async fn check_one(
        &mut self,
        plugin_id: &str,
        permission: &Permission,
        caller: &CallerIdentity,
        tool: Option<&str>,
        args_summary: Option<&str>,
    ) -> Result<(), PermissionError> {
        let danger = permission.danger_level();

        // 低危：manifest 声明即视为已授权，不询问、不落盘。
        if danger == DangerLevel::Low {
            return Ok(());
        }

        // 设计文档 §10：声明了高危权限的插件一律不对 MCP 暴露，
        // 即使用户在暴露白名单里勾选也要在此拦截。这是不可被授权覆盖的硬规则。
        if danger == DangerLevel::High && matches!(caller, CallerIdentity::Mcp { .. }) {
            return Err(PermissionError::Denied {
                plugin_id: plugin_id.to_string(),
                permission: permission.to_string(),
                reason: DenyReason::HighRiskNotExposedToMcp,
            });
        }

        // 已有决定（会话记录优先）就直接采纳。
        // 高危的「每会话首次询问」由 `Session` 记录在启动时被清空来实现。
        if let Some(record) = self.store.lookup(plugin_id, permission) {
            return if record.granted {
                Ok(())
            } else {
                Err(PermissionError::Denied {
                    plugin_id: plugin_id.to_string(),
                    permission: permission.to_string(),
                    reason: DenyReason::PreviouslyDenied,
                })
            };
        }

        // 无记录，需要询问。MCP 调用时用户不在场，无从询问，只能拒绝。
        if !caller.can_prompt() {
            return Err(PermissionError::Denied {
                plugin_id: plugin_id.to_string(),
                permission: permission.to_string(),
                reason: DenyReason::NoRecordAndCannotPrompt,
            });
        }

        let request = PromptRequest {
            plugin_id: plugin_id.to_string(),
            permission: permission.clone(),
            danger,
            caller: caller.clone(),
            tool: tool.map(str::to_string),
            args_summary: args_summary.map(str::to_string),
        };
        let decision = self.prompter.ask(&request).await;

        match decision {
            PromptDecision::AllowAlways => {
                self.store
                    .record(plugin_id, permission, GrantRecord::always(true))?;
                Ok(())
            }
            PromptDecision::AllowSession => {
                self.store
                    .record(plugin_id, permission, GrantRecord::session(true))?;
                Ok(())
            }
            PromptDecision::DenyAlways => {
                self.store
                    .record(plugin_id, permission, GrantRecord::always(false))?;
                Err(PermissionError::Denied {
                    plugin_id: plugin_id.to_string(),
                    permission: permission.to_string(),
                    reason: DenyReason::UserRejected,
                })
            }
            PromptDecision::DenyOnce => Err(PermissionError::Denied {
                plugin_id: plugin_id.to_string(),
                permission: permission.to_string(),
                reason: DenyReason::UserRejected,
            }),
        }
    }
}

/// 拒绝原因，用于审计日志与界面提示。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DenyReason {
    /// 用户当场拒绝。
    UserRejected,
    /// 命中此前保存的拒绝记录。
    PreviouslyDenied,
    /// 无授权记录且当前调用方无法弹窗询问（MCP）。
    NoRecordAndCannotPrompt,
    /// 高危权限插件不对 MCP 暴露。
    HighRiskNotExposedToMcp,
}

impl std::fmt::Display for DenyReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            DenyReason::UserRejected => "用户拒绝授权",
            DenyReason::PreviouslyDenied => "此前已拒绝该权限",
            DenyReason::NoRecordAndCannotPrompt => "无授权记录且当前调用方不能询问用户",
            DenyReason::HighRiskNotExposedToMcp => "声明高危权限的插件不对 MCP 暴露",
        };
        f.write_str(s)
    }
}

// ─────────────────── 时间戳 ───────────────────

/// 生成 `2026-09-02T10:00:00Z` 形式的时间戳。
///
/// 不引入 chrono：宿主的体积预算很紧，而这里只需要单向的「epoch 秒 → UTC 字符串」，
/// 用 Howard Hinnant 的 civil-from-days 算法二十行就够。
fn now_iso8601() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    format_iso8601(secs)
}

fn format_iso8601(epoch_secs: i64) -> String {
    let days = epoch_secs.div_euclid(86_400);
    let rem = epoch_secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    let (hh, mm, ss) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

/// 由「1970-01-01 起的天数」还原公历日期。
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 }.div_euclid(146_097);
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

// ─────────────────── 错误 ───────────────────

#[derive(Debug, thiserror::Error)]
pub enum PermissionError {
    #[error("插件 `{plugin_id}` 的权限 `{permission}` 未获授权：{reason}")]
    Denied {
        plugin_id: String,
        permission: String,
        reason: DenyReason,
    },

    #[error("授权记录读写失败：{0}")]
    Storage(#[from] JsonIoError),

    #[error("定位授权文件失败：{0}")]
    Path(#[from] PathError),
}

impl PermissionError {
    /// 是否为「被拒绝」而非「基础设施故障」。审计日志据此区分记录级别。
    pub fn is_denied(&self) -> bool {
        matches!(self, PermissionError::Denied { .. })
    }
}

// ─────────────────── 测试 ───────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tempfile::TempDir;

    fn perm(s: &str) -> Permission {
        Permission::parse(s).expect("权限字符串应可解析")
    }

    /// 记录被询问次数的 prompter。用于验证「记住授权后不再重复询问」——
    /// 只看最终放行与否无法区分「命中记录」和「又问了一次又同意了」。
    struct CountingPrompter {
        decision: PromptDecision,
        count: AtomicUsize,
    }

    impl CountingPrompter {
        fn new(decision: PromptDecision) -> Self {
            Self {
                decision,
                count: AtomicUsize::new(0),
            }
        }

        fn count(&self) -> usize {
            self.count.load(Ordering::SeqCst)
        }
    }

    #[async_trait::async_trait]
    impl PermissionPrompter for &CountingPrompter {
        async fn ask(&self, _request: &PromptRequest) -> PromptDecision {
            self.count.fetch_add(1, Ordering::SeqCst);
            self.decision
        }
    }

    fn store_in(dir: &TempDir) -> PermissionStore {
        PermissionStore::open_at(dir.path().join("permissions.json")).expect("应能打开空存储")
    }

    // ── 危险度分级前置确认 ──

    #[test]
    fn 分级规则与设计文档一致() {
        assert_eq!(perm("network:http").danger_level(), DangerLevel::Low);
        assert_eq!(perm("file:read").danger_level(), DangerLevel::Low);
        assert_eq!(perm("screen:capture").danger_level(), DangerLevel::Medium);
        assert_eq!(perm("file:write").danger_level(), DangerLevel::Medium);
        assert_eq!(perm("input:control").danger_level(), DangerLevel::High);
        assert_eq!(perm("process:spawn").danger_level(), DangerLevel::High);
        assert_eq!(perm("file:write:*").danger_level(), DangerLevel::High);
    }

    // ── 三级判定路径 ──

    #[tokio::test]
    async fn 低危权限不询问直接放行() {
        let dir = TempDir::new().unwrap();
        let prompter = CountingPrompter::new(PromptDecision::DenyAlways);
        let mut checker = PermissionChecker::new(store_in(&dir), &prompter);

        checker
            .check_all("p.low", &[perm("network:http")], &CallerIdentity::Ui, None, None)
            .await
            .expect("低危权限应直接放行");

        // 即便 prompter 会拒绝，也不该被调用到。
        assert_eq!(prompter.count(), 0, "低危权限不应触发询问");
    }

    #[tokio::test]
    async fn 中危权限首次询问且永久允许后不再询问() {
        let dir = TempDir::new().unwrap();
        let prompter = CountingPrompter::new(PromptDecision::AllowAlways);
        let mut checker = PermissionChecker::new(store_in(&dir), &prompter);
        let p = perm("screen:capture");

        checker
            .check_all("p.mid", std::slice::from_ref(&p), &CallerIdentity::Ui, None, None)
            .await
            .unwrap();
        assert_eq!(prompter.count(), 1, "首次应询问");

        checker
            .check_all("p.mid", std::slice::from_ref(&p), &CallerIdentity::Ui, None, None)
            .await
            .unwrap();
        assert_eq!(prompter.count(), 1, "已永久授权后不应再次询问");
    }

    #[tokio::test]
    async fn 高危权限本次会话允许在清空会话后需重新询问() {
        let dir = TempDir::new().unwrap();
        let prompter = CountingPrompter::new(PromptDecision::AllowSession);
        let mut checker = PermissionChecker::new(store_in(&dir), &prompter);
        let p = perm("input:control");

        checker
            .check_all("p.high", std::slice::from_ref(&p), &CallerIdentity::Ui, None, None)
            .await
            .unwrap();
        checker
            .check_all("p.high", std::slice::from_ref(&p), &CallerIdentity::Ui, None, None)
            .await
            .unwrap();
        assert_eq!(prompter.count(), 1, "同一会话内只询问一次");

        // 模拟宿主重启：会话记录清空。
        checker.store_mut().clear_session();
        checker
            .check_all("p.high", std::slice::from_ref(&p), &CallerIdentity::Ui, None, None)
            .await
            .unwrap();
        assert_eq!(prompter.count(), 2, "新会话应重新询问");
    }

    // ── 拒绝路径 ──

    #[tokio::test]
    async fn 用户拒绝时调用失败() {
        let dir = TempDir::new().unwrap();
        let mut checker =
            PermissionChecker::new(store_in(&dir), FixedPrompter(PromptDecision::DenyOnce));

        let err = checker
            .check_all("p.deny", &[perm("screen:capture")], &CallerIdentity::Ui, None, None)
            .await
            .expect_err("用户拒绝后应返回错误");

        assert!(err.is_denied());
        match err {
            PermissionError::Denied { reason, .. } => {
                assert_eq!(reason, DenyReason::UserRejected)
            }
            other => panic!("预期 Denied，实际 {other:?}"),
        }
    }

    #[tokio::test]
    async fn 拒绝一次不落盘因此下次仍会询问() {
        let dir = TempDir::new().unwrap();
        let prompter = CountingPrompter::new(PromptDecision::DenyOnce);
        let mut checker = PermissionChecker::new(store_in(&dir), &prompter);
        let p = perm("clipboard:read");

        let _ = checker
            .check_all("p.once", std::slice::from_ref(&p), &CallerIdentity::Ui, None, None)
            .await;
        let _ = checker
            .check_all("p.once", std::slice::from_ref(&p), &CallerIdentity::Ui, None, None)
            .await;

        assert_eq!(prompter.count(), 2, "DenyOnce 不应被记住");
        assert!(
            checker.store().persisted_grants("p.once").is_none(),
            "DenyOnce 不应落盘"
        );
    }

    #[tokio::test]
    async fn 永久拒绝后不再询问且持续失败() {
        let dir = TempDir::new().unwrap();
        let prompter = CountingPrompter::new(PromptDecision::DenyAlways);
        let mut checker = PermissionChecker::new(store_in(&dir), &prompter);
        let p = perm("clipboard:write");

        let _ = checker
            .check_all("p.always", std::slice::from_ref(&p), &CallerIdentity::Ui, None, None)
            .await;
        let err = checker
            .check_all("p.always", std::slice::from_ref(&p), &CallerIdentity::Ui, None, None)
            .await
            .expect_err("应持续被拒");

        assert_eq!(prompter.count(), 1, "永久拒绝后不应重复询问");
        match err {
            PermissionError::Denied { reason, .. } => {
                assert_eq!(reason, DenyReason::PreviouslyDenied)
            }
            other => panic!("预期 PreviouslyDenied，实际 {other:?}"),
        }
    }

    #[tokio::test]
    async fn 声明多项权限时任一被拒则整体失败() {
        let dir = TempDir::new().unwrap();
        let mut checker =
            PermissionChecker::new(store_in(&dir), FixedPrompter(PromptDecision::DenyOnce));

        let err = checker
            .check_all(
                "p.multi",
                &[perm("network:http"), perm("screen:capture")],
                &CallerIdentity::Ui,
                None,
                None,
            )
            .await
            .expect_err("含被拒权限时应整体失败");

        assert!(err.is_denied());
    }

    // ── MCP 相关硬规则 ──

    #[tokio::test]
    async fn mcp调用方遇高危权限直接拒绝且不询问() {
        let dir = TempDir::new().unwrap();
        let prompter = CountingPrompter::new(PromptDecision::AllowAlways);
        let mut checker = PermissionChecker::new(store_in(&dir), &prompter);
        let p = perm("input:control");
        let caller = CallerIdentity::Mcp {
            client_name: "claude".into(),
        };

        // 即使此前已永久授权，MCP 路径仍必须拦截。
        checker
            .store_mut()
            .record("p.hi", &p, GrantRecord::always(true))
            .unwrap();

        let err = checker
            .check_all("p.hi", std::slice::from_ref(&p), &caller, None, None)
            .await
            .expect_err("高危权限不应对 MCP 暴露");

        match err {
            PermissionError::Denied { reason, .. } => {
                assert_eq!(reason, DenyReason::HighRiskNotExposedToMcp)
            }
            other => panic!("预期 HighRiskNotExposedToMcp，实际 {other:?}"),
        }
        assert_eq!(prompter.count(), 0, "MCP 路径不应弹窗");
    }

    #[tokio::test]
    async fn mcp调用方无中危记录时因无法询问而被拒() {
        let dir = TempDir::new().unwrap();
        let prompter = CountingPrompter::new(PromptDecision::AllowAlways);
        let mut checker = PermissionChecker::new(store_in(&dir), &prompter);
        let caller = CallerIdentity::Mcp {
            client_name: "claude".into(),
        };

        let err = checker
            .check_all("p.mcp", &[perm("screen:capture")], &caller, None, None)
            .await
            .expect_err("无记录且不能询问时应拒绝");

        match err {
            PermissionError::Denied { reason, .. } => {
                assert_eq!(reason, DenyReason::NoRecordAndCannotPrompt)
            }
            other => panic!("预期 NoRecordAndCannotPrompt，实际 {other:?}"),
        }
        assert_eq!(prompter.count(), 0);
    }

    #[tokio::test]
    async fn mcp调用方可复用已有的中危授权() {
        let dir = TempDir::new().unwrap();
        let mut checker =
            PermissionChecker::new(store_in(&dir), FixedPrompter(PromptDecision::DenyAlways));
        let p = perm("screen:capture");
        checker
            .store_mut()
            .record("p.ok", &p, GrantRecord::always(true))
            .unwrap();

        checker
            .check_all(
                "p.ok",
                std::slice::from_ref(&p),
                &CallerIdentity::Mcp {
                    client_name: "claude".into(),
                },
                None,
                None,
            )
            .await
            .expect("已授权的中危权限对 MCP 应放行");
    }

    // ── 持久化 ──

    #[test]
    fn 永久授权可跨实例往返() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("permissions.json");
        let p = perm("screen:capture");

        let mut store = PermissionStore::open_at(&path).unwrap();
        store
            .record("com.example.ocr", &p, GrantRecord::always(true))
            .unwrap();

        let reopened = PermissionStore::open_at(&path).unwrap();
        let record = reopened
            .lookup("com.example.ocr", &p)
            .expect("重开后应能读到记录");
        assert!(record.granted);
        assert_eq!(record.scope, GrantScope::Always);
    }

    #[test]
    fn 会话授权不落盘() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("permissions.json");
        let p = perm("input:control");

        let mut store = PermissionStore::open_at(&path).unwrap();
        store
            .record("p.sess", &p, GrantRecord::session(true))
            .unwrap();
        assert!(store.lookup("p.sess", &p).is_some(), "本会话内应可见");

        let reopened = PermissionStore::open_at(&path).unwrap();
        assert!(
            reopened.lookup("p.sess", &p).is_none(),
            "会话授权不应出现在新实例中"
        );
    }

    #[test]
    fn 会话记录优先于落盘记录() {
        let dir = TempDir::new().unwrap();
        let mut store = store_in(&dir);
        let p = perm("input:control");

        store.record("p.x", &p, GrantRecord::always(false)).unwrap();
        store.record("p.x", &p, GrantRecord::session(true)).unwrap();

        assert!(
            store.lookup("p.x", &p).unwrap().granted,
            "「本次允许」应覆盖旧的永久拒绝"
        );
    }

    #[test]
    fn 永久决定会清除同权限的会话记录() {
        let dir = TempDir::new().unwrap();
        let mut store = store_in(&dir);
        let p = perm("screen:capture");

        store.record("p.y", &p, GrantRecord::session(true)).unwrap();
        store.record("p.y", &p, GrantRecord::always(false)).unwrap();

        assert!(
            !store.lookup("p.y", &p).unwrap().granted,
            "新的永久拒绝不应被旧的会话允许遮蔽"
        );
    }

    #[test]
    fn 撤销插件授权会同时清除落盘与会话记录() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("permissions.json");
        let a = perm("screen:capture");
        let b = perm("input:control");

        let mut store = PermissionStore::open_at(&path).unwrap();
        store.record("p.z", &a, GrantRecord::always(true)).unwrap();
        store.record("p.z", &b, GrantRecord::session(true)).unwrap();
        store.revoke_plugin("p.z").unwrap();

        assert!(store.lookup("p.z", &a).is_none());
        assert!(store.lookup("p.z", &b).is_none());
        assert!(PermissionStore::open_at(&path)
            .unwrap()
            .lookup("p.z", &a)
            .is_none());
    }

    #[test]
    fn 落盘格式与设计文档schema一致() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("permissions.json");

        let mut store = PermissionStore::open_at(&path).unwrap();
        store
            .record(
                "com.example.ocr",
                &perm("screen:capture"),
                GrantRecord::always(true),
            )
            .unwrap();
        store
            .record(
                "com.example.ocr",
                &perm("file:read"),
                GrantRecord::always(true).with_paths(vec!["~/Pictures".into()]),
            )
            .unwrap();

        let raw = std::fs::read_to_string(&path).unwrap();
        let json: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let entry = &json["com.example.ocr"]["screen:capture"];

        assert_eq!(entry["granted"], serde_json::json!(true));
        assert_eq!(entry["scope"], serde_json::json!("always"));
        assert!(
            entry.get("paths").is_none(),
            "无路径限定时不应写出 paths 字段"
        );
        assert_eq!(
            json["com.example.ocr"]["file:read"]["paths"],
            serde_json::json!(["~/Pictures"])
        );
    }

    #[test]
    fn 读到损坏的授权文件应报错而非静默放行() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("permissions.json");
        std::fs::write(&path, "{ 这不是合法 JSON").unwrap();

        let err = PermissionStore::open_at(&path).expect_err("损坏文件应报错");
        assert!(!err.is_denied(), "解析失败属于基础设施故障而非拒绝");
    }

    // ── 调用方身份 ──

    #[test]
    fn 调用方身份的深度与询问能力() {
        assert_eq!(CallerIdentity::Ui.depth(), 0);
        assert!(CallerIdentity::Ui.can_prompt());

        let mcp = CallerIdentity::Mcp {
            client_name: "claude".into(),
        };
        assert_eq!(mcp.depth(), 0);
        assert!(!mcp.can_prompt(), "MCP 调用时用户不在场");

        let nested = CallerIdentity::Plugin {
            id: "ai.orchestrator".into(),
            depth: 3,
        };
        assert_eq!(nested.depth(), 3);
        assert!(nested.can_prompt());
        assert_eq!(nested.label(), "plugin:ai.orchestrator@3");
    }

    // ── 时间戳 ──

    #[test]
    fn 时间戳格式化正确() {
        assert_eq!(format_iso8601(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_iso8601(1_772_452_800), "2026-03-02T12:00:00Z");
        // 闰日，验证 civil-from-days 的闰年处理。
        assert_eq!(format_iso8601(1_709_208_000), "2024-02-29T12:00:00Z");
    }

    #[test]
    fn 生成的时间戳形状合法() {
        let ts = now_iso8601();
        assert_eq!(ts.len(), 20, "应为 YYYY-MM-DDTHH:MM:SSZ");
        assert!(ts.ends_with('Z'));
        assert_eq!(&ts[4..5], "-");
        assert_eq!(&ts[10..11], "T");
    }
}
