//! 插件 `manifest.toml` 的结构、解析与校验。
//!
//! manifest 分五个表：`[plugin]`（元信息）、`[exec]`（启动命令与参数）、
//! `[[tools]]`（静态工具清单）、`[capabilities]`（权限声明）、`[lifecycle]`
//! （生命周期策略）。结构严格对应设计文档 §4.1 的示例。
//!
//! 解析与校验分开：
//! - [`Manifest::parse_toml`] 从 TOML 文本解析出结构，做最基本的反序列化校验（如
//!   枚举值是否在范围内）。
//! - [`Manifest::validate`] 进一步做语义校验：插件 ID 反向域名格式、版本号语义化、
//!   工具名非空且插件内不重复、`input_schema` 为合法 JSON Schema 对象等。
//!   这些错误无法由 TOML 反序列化本身表达。
//! - [`load_from_str`] 一步完成「解析 + 校验」，是后续 registry 加载插件的入口。

// Phase 1 产物尚未被 registry/runtime 引用，整模块允许"未使用"。
#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use thiserror::Error;

// ─────────────────── 顶层结构 ───────────────────

/// 加载 manifest 的便捷入口：解析 TOML 字符串 → 校验。
pub fn load_from_str(toml_text: &str) -> Result<Manifest, ManifestError> {
    parse_toml(toml_text)?.validate()
}

/// 仅解析（不做语义校验），公开以便调用方做容错展示。
pub fn parse_toml(toml_text: &str) -> Result<Manifest, ManifestError> {
    toml::from_str(toml_text).map_err(ManifestError::Toml)
}

/// 完整的 manifest 结构，对应 `manifest.toml` 的全部字段。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub plugin: PluginInfo,
    pub exec: ExecInfo,
    #[serde(default)]
    pub tools: Vec<ToolDescriptor>,
    #[serde(default)]
    pub capabilities: Capabilities,
    #[serde(default)]
    pub lifecycle: Lifecycle,
}

impl Manifest {
    /// 语义校验，成功返回 `Ok(self)`，便于链式调用。
    pub fn validate(mut self) -> Result<Manifest, ManifestError> {
        // 1. 插件 ID 反向域名
        validate_plugin_id(&self.plugin.id)?;

        // 2. version 字段语义化格式
        validate_semver(&self.plugin.version)?;

        // 3. command 与 args[0] 均非空
        if self.exec.command.trim().is_empty() {
            return Err(ManifestError::Validation("exec.command 不能为空".to_string()));
        }
        if self
            .exec
            .args
            .first()
            .map(|a| a.is_empty())
            .unwrap_or(false)
        {
            return Err(ManifestError::Validation(
                "exec.args[0] 若存在则不能为空串".to_string(),
            ));
        }

        // 4. 工具名非空且插件内不重复；逐一校验 input_schema
        let mut seen = std::collections::HashSet::new();
        for tool in &mut self.tools {
            if tool.name.trim().is_empty() {
                return Err(ManifestError::Validation(
                    "tools.name 不能为空".to_string(),
                ));
            }
            if !seen.insert(tool.name.clone()) {
                return Err(ManifestError::DuplicateToolName(tool.name.clone()));
            }
            validate_input_schema(&tool.input_schema, &tool.name)?;
        }

        // 5. lifecycle 数值合理性：timeout 不能为 0
        if let Some(s) = self.lifecycle.idle_timeout_sec {
            if s == 0 {
                return Err(ManifestError::Validation(
                    "lifecycle.idle_timeout_sec 不能为 0（若想禁用请留空或改为 background 模式）"
                        .to_string(),
                ));
            }
        }
        if let Some(s) = self.lifecycle.request_timeout_sec {
            if s == 0 {
                return Err(ManifestError::Validation(
                    "lifecycle.request_timeout_sec 不能为 0".to_string(),
                ));
            }
        }

        Ok(self)
    }
}

// ─────────────────── 子结构 ───────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginInfo {
    /// 反向域名 ID，例如 `com.example.ocr`。
    pub id: String,
    /// 面向用户的可读名称。
    pub name: String,
    /// 语义化版本，例如 `1.0.0`。
    pub version: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecInfo {
    /// 启动可执行文件，例如 `python` 或 `./ocr-server`。
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolDescriptor {
    /// 插件内唯一工具名，常用形式 `前缀:动作`（如 `ocr:recognize`）。
    /// 冒号是合法字符，MCP 层再做映射。
    pub name: String,
    /// 人类可读描述，供 AI 与用户阅读。
    #[serde(default)]
    pub description: String,
    /// JSON Schema（Draft 兼容即可），描述工具参数结构。
    ///
    /// 这里不做深度语法合法性（比如 `properties` 必须是对象这种约束），
    /// 只校验它**能被解析为对象**（因为 LLM SDK 和 MCP 都需要对象）。
    #[serde(default = "default_schema")]
    pub input_schema: JsonValue,
}

fn default_schema() -> JsonValue {
    serde_json::json!({"type":"object","properties":{},"required":[]})
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Capabilities {
    #[serde(default)]
    pub permissions: Vec<Permission>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Lifecycle {
    pub mode: LifecycleMode,
    /// `on-demand` 模式下 Idle 多久自动停止（秒）。默认 300。
    #[serde(alias = "idle_timeout_sec")]
    pub idle_timeout_sec: Option<u32>,
    pub restart_policy: RestartPolicy,
    /// 单次请求超时（秒）。默认 30。
    pub request_timeout_sec: Option<u32>,
}

impl Default for Lifecycle {
    fn default() -> Self {
        Self {
            mode: LifecycleMode::OnDemand,
            idle_timeout_sec: Some(300),
            restart_policy: RestartPolicy::OnFailure,
            request_timeout_sec: Some(30),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LifecycleMode {
    OnDemand,
    Background,
    Startup,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RestartPolicy {
    Never,
    OnFailure,
    Always,
}

// ─────────────────── 权限 ───────────────────

/// 单个权限声明，形如 `screen:capture`、`file:read`、`file:read:/foo`。
///
/// 内部格式：`CATEGORY:ACTION[:SCOPE]`。示例：
/// - `file:read`（分类 `file`，动作 `read`，无范围）
/// - `file:read:~/Pictures`（分类 `file`，动作 `read`，范围 `~/Pictures`）
/// - `process:spawn`（高危，无范围）
///
/// 按分类 + 动作映射到三级危险度，见 [`Permission::danger_level`]。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Permission {
    pub category: String,
    pub action: String,
    pub scope: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DangerLevel {
    Low,
    Medium,
    High,
}

impl Permission {
    pub fn parse(s: &str) -> Result<Self, String> {
        if s.trim().is_empty() {
            return Err("权限字符串不能为空".to_string());
        }
        let mut parts = s.splitn(3, ':');
        let cat = parts.next().unwrap_or("");
        let act = parts.next().unwrap_or("");
        let scope = parts.next();
        if cat.is_empty() || act.is_empty() {
            return Err(format!(
                "权限 `{s}` 格式非法，要求 `category:action[:scope]`"
            ));
        }
        Ok(Permission {
            category: cat.to_string(),
            action: act.to_string(),
            scope: scope.map(|s| s.to_string()),
        })
    }

    /// 按设计文档 §7 的三级危险度分类。
    ///
    /// 规则：
    /// - **高危**：`input:control`（模拟键鼠）、`process:spawn`（起新进程）、
    ///   `file:write:*`（任意路径写，即 action 为 `write` 且 scope 为 `*`）。
    /// - **中危**：`screen:capture`、`clipboard:write`、`file:write`（非 `*` 范围）、
    ///   `clipboard:read`。
    /// - **低危**：其余（如 `file:read`、`network:http`）。
    /// - 由于权限模型为插件级粗粒度，单个高危权限即会让整个插件在 MCP 层被拦截。
    pub fn danger_level(&self) -> DangerLevel {
        match (self.category.as_str(), self.action.as_str(), self.scope.as_deref()) {
            ("input", "control", _) => DangerLevel::High,
            ("process", "spawn", _) => DangerLevel::High,
            ("file", "write", Some("*")) => DangerLevel::High,

            ("screen", "capture", _) => DangerLevel::Medium,
            ("clipboard", "write", _) => DangerLevel::Medium,
            ("clipboard", "read", _) => DangerLevel::Medium,
            ("file", "write", _) => DangerLevel::Medium,

            _ => DangerLevel::Low,
        }
    }
}

impl std::fmt::Display for Permission {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.scope {
            Some(s) => write!(f, "{}:{}:{}", self.category, self.action, s),
            None => write!(f, "{}:{}", self.category, self.action),
        }
    }
}

impl TryFrom<String> for Permission {
    type Error = String;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Permission::parse(&value)
    }
}

impl From<Permission> for String {
    fn from(p: Permission) -> Self {
        p.to_string()
    }
}

// ─────────────────── 协议版本协商 ───────────────────

/// `plugin/hello` 中的协议版本号（MAJOR.MINOR，第一版为 1.0）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProtocolVersion {
    pub major: u32,
    pub minor: u32,
}

impl ProtocolVersion {
    pub const V1_0: ProtocolVersion = ProtocolVersion { major: 1, minor: 0 };

    pub fn parse(s: &str) -> Result<Self, String> {
        let mut it = s.split('.');
        let major = it
            .next()
            .ok_or_else(|| format!("协议版本 `{s}` 格式非法，期望 MAJOR.MINOR"))?
            .parse::<u32>()
            .map_err(|_| format!("协议版本 major 非法：`{s}`"))?;
        let minor = it
            .next()
            .ok_or_else(|| format!("协议版本 `{s}` 缺少 MINOR"))?
            .parse::<u32>()
            .map_err(|_| format!("协议版本 minor 非法：`{s}`"))?;
        if it.next().is_some() {
            return Err(format!("协议版本段过多：`{s}`"));
        }
        Ok(Self { major, minor })
    }
}

impl std::fmt::Display for ProtocolVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

/// 版本协商结果。按设计文档 §5.5 规则：
/// - MAJOR 不一致 → `Err`，拒绝加载
/// - MAJOR 一致 → `Ok(true)` 若插件 MINOR 不高于宿主；`Ok(false)` 需插件自行降级
///   使用宿主已支持的方法（宿主仍然接受）。
pub fn negotiate_version(
    host: ProtocolVersion,
    plugin: ProtocolVersion,
) -> Result<bool, VersionMismatch> {
    if host.major != plugin.major {
        return Err(VersionMismatch { host, plugin });
    }
    Ok(plugin.minor <= host.minor)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("协议主版本不兼容：宿主 {host}，插件 {plugin}")]
pub struct VersionMismatch {
    pub host: ProtocolVersion,
    pub plugin: ProtocolVersion,
}

// ─────────────────── 校验工具函数 ───────────────────

fn validate_plugin_id(id: &str) -> Result<(), ManifestError> {
    // 反向域名：至少 3 段（a.b.c），每段非空，允许字母数字、下划线、连字符。
    let segs: Vec<&str> = id.split('.').collect();
    if segs.len() < 3 {
        return Err(ManifestError::Validation(format!(
            "plugin.id `{id}` 必须为反向域名格式（至少 3 段，如 com.example.ocr）"
        )));
    }
    for s in &segs {
        if s.is_empty() {
            return Err(ManifestError::Validation(format!(
                "plugin.id `{id}` 段不能为空"
            )));
        }
        let ok = s
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
        if !ok {
            return Err(ManifestError::Validation(format!(
                "plugin.id `{id}` 段 `{s}` 含非法字符（仅允许字母数字 _ -）"
            )));
        }
    }
    Ok(())
}

/// 只做宽松语义化校验：`MAJOR.MINOR.PATCH`，三段均为非负整数。足够第一版使用。
fn validate_semver(v: &str) -> Result<(), ManifestError> {
    let segs: Vec<&str> = v.split('.').collect();
    if segs.len() != 3 {
        return Err(ManifestError::Validation(format!(
            "plugin.version `{v}` 必须是语义化版本 MAJOR.MINOR.PATCH"
        )));
    }
    for s in &segs {
        if s.is_empty() || !s.chars().all(|c| c.is_ascii_digit()) {
            return Err(ManifestError::Validation(format!(
                "plugin.version `{v}` 段 `{s}` 非法（必须为数字）"
            )));
        }
    }
    Ok(())
}

fn validate_input_schema(schema: &JsonValue, tool_name: &str) -> Result<(), ManifestError> {
    let obj = schema.as_object().ok_or_else(|| {
        ManifestError::Validation(format!(
            "工具 `{tool_name}` 的 input_schema 必须是 JSON 对象"
        ))
    })?;
    match obj.get("type").and_then(|v| v.as_str()) {
        Some("object") => Ok(()),
        Some(other) => Err(ManifestError::Validation(format!(
            "工具 `{tool_name}` 的 input_schema.type 必须为 `object`，实际 `{other}`"
        ))),
        None => Err(ManifestError::Validation(format!(
            "工具 `{tool_name}` 的 input_schema 缺少 `type` 字段"
        ))),
    }
}

// ─────────────────── 错误类型 ───────────────────

#[derive(Debug, Error)]
pub enum ManifestError {
    #[error("TOML 解析失败：{0}")]
    Toml(#[from] toml::de::Error),

    #[error("manifest 校验失败：{0}")]
    Validation(String),

    #[error("插件内工具名重复：{0}")]
    DuplicateToolName(String),

    #[error("权限字符串非法：{0}")]
    BadPermission(String),

    #[error("协议版本不兼容：{0}")]
    BadVersion(String),
}

// registry 等后续模块需要对错误做 variant 判别，提供一个可读的 PartialEq 近似。
impl PartialEq for ManifestError {
    fn eq(&self, other: &Self) -> bool {
        use ManifestError::*;
        match (self, other) {
            (Toml(a), Toml(b)) => a.to_string() == b.to_string(),
            (Validation(a), Validation(b)) => a == b,
            (DuplicateToolName(a), DuplicateToolName(b)) => a == b,
            (BadPermission(a), BadPermission(b)) => a == b,
            (BadVersion(a), BadVersion(b)) => a == b,
            _ => false,
        }
    }
}

// ─────────────────── 单元测试 ───────────────────
#[cfg(test)]
mod tests {
    use super::*;

    // 设计文档 §4.1 示例。作为解析基准：
    const SAMPLE: &str = r#"
[plugin]
id = "com.example.ocr"
name = "OCR 识别"
version = "1.0.0"
author = "Example"
description = "屏幕截图并识别文字"

[exec]
command = "python"
args = ["-u", "main.py"]

[[tools]]
name = "ocr:recognize"
description = "识别图片中的文字，返回文本内容"
[tools.input_schema]
type = "object"
required = ["image_path"]
[tools.input_schema.properties.image_path]
type = "string"
description = "图片文件的绝对路径"
[tools.input_schema.properties.lang]
type = "string"
description = "识别语言，默认 zh-CN"

[capabilities]
permissions = ["screen:capture", "file:read"]

[lifecycle]
mode = "on-demand"
idle_timeout_sec = 300
restart_policy = "on-failure"
request_timeout_sec = 30
"#;

    #[test]
    fn parse_design_doc_sample_success() {
        let m = load_from_str(SAMPLE).unwrap();
        assert_eq!(m.plugin.id, "com.example.ocr");
        assert_eq!(m.plugin.version, "1.0.0");
        assert_eq!(m.exec.command, "python");
        assert_eq!(m.exec.args, ["-u", "main.py"]);
        assert_eq!(m.tools.len(), 1);
        assert_eq!(m.tools[0].name, "ocr:recognize");
        let schema = &m.tools[0].input_schema;
        assert_eq!(schema["type"], "object");
        let required = schema["required"].as_array().unwrap();
        assert_eq!(required.len(), 1);
        assert_eq!(required[0], "image_path");
        assert_eq!(
            schema["properties"]["image_path"]["description"],
            "图片文件的绝对路径"
        );
        assert_eq!(
            m.capabilities.permissions[0],
            Permission::parse("screen:capture").unwrap()
        );
        assert_eq!(
            m.capabilities.permissions[1],
            Permission::parse("file:read").unwrap()
        );
        assert_eq!(m.lifecycle.mode, LifecycleMode::OnDemand);
        assert_eq!(m.lifecycle.idle_timeout_sec, Some(300));
        assert_eq!(m.lifecycle.restart_policy, RestartPolicy::OnFailure);
        assert_eq!(m.lifecycle.request_timeout_sec, Some(30));
    }

    // ── 必填字段缺失 ───────────────────────────────

    #[test]
    fn missing_plugin_table_is_rejected() {
        let err = load_from_str(
            r#"
[exec]
command = "python"
"#,
        )
        .unwrap_err();
        // toml 反序列化缺字段报错
        let ManifestError::Toml(_) = err else {
            panic!("期望 TOML 错误，实际 {:?}", err);
        };
    }

    #[test]
    fn empty_exec_command_is_rejected() {
        let err = load_from_str(
            r#"
[plugin]
id = "com.example.x"
name = "X"
version = "0.1.0"

[exec]
command = "   "
"#,
        )
        .unwrap_err();
        assert!(format!("{}", err).contains("exec.command 不能为空"));
    }

    #[test]
    fn exec_arg_first_empty_is_rejected() {
        let err = load_from_str(
            r#"
[plugin]
id = "com.example.x"
name = "X"
version = "0.1.0"

[exec]
command = "python"
args = ["", "something"]
"#,
        )
        .unwrap_err();
        assert!(format!("{}", err).contains("exec.args[0]"));
    }

    // ── plugin.id 格式 ──────────────────────────────

    #[test]
    fn plugin_id_short_rejected() {
        // 只两段，不满足反向域名至少 3 段
        let err = load_from_str(
            r#"
[plugin]
id = "example.ocr"
name = "X"
version = "0.1.0"

[exec]
command = "python"
"#,
        )
        .unwrap_err();
        let ManifestError::Validation(txt) = err else {
            panic!();
        };
        assert!(txt.contains("反向域名"));
    }

    #[test]
    fn plugin_id_empty_segment_rejected() {
        let err = load_from_str(
            r#"
[plugin]
id = "com..ocr"
name = "X"
version = "0.1.0"

[exec]
command = "python"
"#,
        )
        .unwrap_err();
        let ManifestError::Validation(txt) = err else { panic!() };
        assert!(txt.contains("段不能为空"));
    }

    #[test]
    fn plugin_id_invalid_chars_rejected() {
        let err = load_from_str(
            r#"
[plugin]
id = "com.example.ocr 插件"
name = "X"
version = "0.1.0"

[exec]
command = "python"
"#,
        )
        .unwrap_err();
        let ManifestError::Validation(txt) = err else { panic!() };
        assert!(txt.contains("非法字符"));
    }

    // ── 语义化版本号 ────────────────────────────────

    #[test]
    fn version_non_semver_rejected() {
        let err = load_from_str(
            r#"
[plugin]
id = "com.example.ocr"
name = "O"
version = "latest"

[exec]
command = "python"
"#,
        )
        .unwrap_err();
        let ManifestError::Validation(txt) = err else { panic!() };
        assert!(txt.contains("语义化版本"));
    }

    #[test]
    fn version_only_two_segments_rejected() {
        let err = load_from_str(
            r#"
[plugin]
id = "com.example.ocr"
name = "O"
version = "1.0"

[exec]
command = "python"
"#,
        )
        .unwrap_err();
        let ManifestError::Validation(txt) = err else { panic!() };
        assert!(txt.contains("语义化版本"));
    }

    #[test]
    fn version_prerelease_loose_rejected() {
        // 宽松校验只接受三段纯数字；beta 版本含 - 前缀或多段会被拒。
        let err = load_from_str(
            r#"
[plugin]
id = "com.example.ocr"
name = "O"
version = "1.0.0-beta.1"

[exec]
command = "python"
"#,
        )
        .unwrap_err();
        let ManifestError::Validation(txt) = err else {
            panic!("期望 Validation，实际 {:?}", err);
        };
        // 要么段数不对（MAJOR.MINOR.PATCH 之外多了 beta.1 会触发 len!=3 检查），
        // 要么某段含非数字字符。任一命中均视为合法。
        let hits = txt.contains("必须为数字") || txt.contains("语义化版本");
        assert!(hits, "txt={:?}", txt);
    }

    // ── 工具名重复 / 为空 ───────────────────────────

    #[test]
    fn duplicate_tool_name_in_plugin_rejected() {
        let manifest_toml = r#"
[plugin]
id = "com.example.ocr"
name = "O"
version = "1.0.0"

[exec]
command = "python"

[[tools]]
name = "ocr:recognize"
[tools.input_schema]
type = "object"

[[tools]]
name = "ocr:recognize"
[tools.input_schema]
type = "object"
"#;
        let err = load_from_str(manifest_toml).unwrap_err();
        let ManifestError::DuplicateToolName(name) = err else {
            panic!("期望 DuplicateToolName，实际 {:?}", err);
        };
        assert_eq!(name, "ocr:recognize");
    }

    #[test]
    fn empty_tool_name_rejected() {
        let manifest_toml = r#"
[plugin]
id = "com.example.ocr"
name = "O"
version = "1.0.0"

[exec]
command = "python"

[[tools]]
name = "  "
[tools.input_schema]
type = "object"
"#;
        let err = load_from_str(manifest_toml).unwrap_err();
        let ManifestError::Validation(txt) = err else { panic!() };
        assert!(txt.contains("tools.name 不能为空"));
    }

    // ── input_schema 校验 ────────────────────────────

    #[test]
    fn input_schema_not_object_rejected() {
        let manifest_toml = r#"
[plugin]
id = "com.example.ocr"
name = "O"
version = "1.0.0"

[exec]
command = "python"

[[tools]]
name = "ocr:recognize"
input_schema = "not an object"
"#;
        let err = load_from_str(manifest_toml).unwrap_err();
        let ManifestError::Validation(txt) = err else { panic!() };
        assert!(txt.contains("input_schema 必须是 JSON 对象"));
    }

    #[test]
    fn input_schema_type_not_object_rejected() {
        let manifest_toml = r#"
[plugin]
id = "com.example.ocr"
name = "O"
version = "1.0.0"

[exec]
command = "python"

[[tools]]
name = "ocr:recognize"
[tools.input_schema]
type = "array"
"#;
        let err = load_from_str(manifest_toml).unwrap_err();
        let ManifestError::Validation(txt) = err else { panic!() };
        assert!(txt.contains("input_schema.type 必须为 `object`"));
    }

    #[test]
    fn input_schema_type_missing_rejected() {
        let manifest_toml = r#"
[plugin]
id = "com.example.ocr"
name = "O"
version = "1.0.0"

[exec]
command = "python"

[[tools]]
name = "ocr:recognize"
[tools.input_schema]
description = "oops no type"
"#;
        let err = load_from_str(manifest_toml).unwrap_err();
        let ManifestError::Validation(txt) = err else { panic!() };
        assert!(txt.contains("缺少 `type` 字段"));
    }

    // ── 生命周期数值 ─────────────────────────────────

    #[test]
    fn zero_idle_timeout_rejected() {
        let manifest_toml = r#"
[plugin]
id = "com.example.ocr"
name = "O"
version = "1.0.0"

[exec]
command = "python"

[lifecycle]
mode = "on-demand"
idle_timeout_sec = 0
"#;
        let err = load_from_str(manifest_toml).unwrap_err();
        let ManifestError::Validation(txt) = err else { panic!() };
        assert!(txt.contains("idle_timeout_sec 不能为 0"));
    }

    #[test]
    fn zero_request_timeout_rejected() {
        let manifest_toml = r#"
[plugin]
id = "com.example.ocr"
name = "O"
version = "1.0.0"

[exec]
command = "python"

[lifecycle]
mode = "on-demand"
request_timeout_sec = 0
"#;
        let err = load_from_str(manifest_toml).unwrap_err();
        let ManifestError::Validation(txt) = err else { panic!() };
        assert!(txt.contains("request_timeout_sec 不能为 0"));
    }

    #[test]
    fn default_lifecycle_applies() {
        // 不写 [lifecycle] 表：要能应用默认值（OnDemand / 300s / OnFailure / 30s）。
        let manifest_toml = r#"
[plugin]
id = "com.example.ocr"
name = "O"
version = "1.0.0"

[exec]
command = "python"
"#;
        let m = load_from_str(manifest_toml).unwrap();
        assert_eq!(m.lifecycle.mode, LifecycleMode::OnDemand);
        assert_eq!(m.lifecycle.idle_timeout_sec, Some(300));
        assert_eq!(m.lifecycle.restart_policy, RestartPolicy::OnFailure);
        assert_eq!(m.lifecycle.request_timeout_sec, Some(30));
    }

    #[test]
    fn enum_variants_kebab_case() {
        let manifest_toml = r#"
[plugin]
id = "com.example.ocr"
name = "O"
version = "1.0.0"

[exec]
command = "python"

[lifecycle]
mode = "startup"
restart_policy = "always"
"#;
        let m = load_from_str(manifest_toml).unwrap();
        assert_eq!(m.lifecycle.mode, LifecycleMode::Startup);
        assert_eq!(m.lifecycle.restart_policy, RestartPolicy::Always);
    }

    #[test]
    fn bad_enum_variant_rejected_as_toml_error() {
        let manifest_toml = r#"
[plugin]
id = "com.example.ocr"
name = "O"
version = "1.0.0"

[exec]
command = "python"

[lifecycle]
mode = "lazy"  # 不存在
"#;
        let err = load_from_str(manifest_toml).unwrap_err();
        let ManifestError::Toml(_) = err else {
            panic!("期望 TOML 枚举反序列化错误，实际 {:?}", err);
        };
    }

    // ── Permission parse / danger_level ──────────────

    #[test]
    fn permission_parse_ok() {
        let p = Permission::parse("file:read").unwrap();
        assert_eq!(p.category, "file");
        assert_eq!(p.action, "read");
        assert!(p.scope.is_none());
        assert_eq!(p.to_string(), "file:read");

        let p2 = Permission::parse("file:read:~/Pictures").unwrap();
        assert_eq!(p2.scope.as_deref(), Some("~/Pictures"));
        assert_eq!(p2.to_string(), "file:read:~/Pictures");
    }

    #[test]
    fn permission_parse_bad() {
        assert!(Permission::parse("").is_err());
        assert!(Permission::parse("file").is_err()); // 少 action
        assert!(Permission::parse(":read").is_err()); // 空 category
    }

    #[test]
    fn permission_danger_level_matrix() {
        // 高危
        assert_eq!(
            Permission::parse("input:control").unwrap().danger_level(),
            DangerLevel::High
        );
        assert_eq!(
            Permission::parse("process:spawn").unwrap().danger_level(),
            DangerLevel::High
        );
        assert_eq!(
            Permission::parse("file:write:*").unwrap().danger_level(),
            DangerLevel::High
        );
        // 中危
        assert_eq!(
            Permission::parse("screen:capture").unwrap().danger_level(),
            DangerLevel::Medium
        );
        assert_eq!(
            Permission::parse("clipboard:write").unwrap().danger_level(),
            DangerLevel::Medium
        );
        assert_eq!(
            Permission::parse("clipboard:read").unwrap().danger_level(),
            DangerLevel::Medium
        );
        assert_eq!(
            Permission::parse("file:write:~/a.txt").unwrap().danger_level(),
            DangerLevel::Medium
        );
        // 低危
        assert_eq!(
            Permission::parse("file:read").unwrap().danger_level(),
            DangerLevel::Low
        );
        assert_eq!(
            Permission::parse("network:http").unwrap().danger_level(),
            DangerLevel::Low
        );
    }

    #[test]
    fn permissions_serialized_via_toml_round_trip() {
        // manifest 中 permissions 是字符串数组；经 try_from(String) → Permission 能正常解析。
        // 上面的示例测试已经覆盖了正向，这里再测一个带 scope 的三部分权限。
        let manifest_toml = r#"
[plugin]
id = "com.example.ocr"
name = "O"
version = "1.0.0"

[exec]
command = "python"

[capabilities]
permissions = ["file:read:~/Pictures", "file:write:*"]
"#;
        let m = load_from_str(manifest_toml).unwrap();
        assert_eq!(
            m.capabilities.permissions[0],
            Permission::parse("file:read:~/Pictures").unwrap()
        );
        assert_eq!(
            m.capabilities.permissions[1],
            Permission::parse("file:write:*").unwrap()
        );
        // file:write:* 属于高危
        assert_eq!(
            m.capabilities.permissions[1].danger_level(),
            DangerLevel::High
        );
    }

    // ── 协议版本协商 ─────────────────────────────────

    #[test]
    fn protocol_version_parse() {
        let v = ProtocolVersion::parse("1.0").unwrap();
        assert_eq!(v, ProtocolVersion::V1_0);
        assert_eq!(v.to_string(), "1.0");
        assert!(ProtocolVersion::parse("1").is_err());
        assert!(ProtocolVersion::parse("1.0.0").is_err());
        assert!(ProtocolVersion::parse("x.0").is_err());
    }

    #[test]
    fn negotiate_major_mismatch_rejected() {
        let host = ProtocolVersion::V1_0;
        let plugin = ProtocolVersion { major: 2, minor: 0 };
        let err = negotiate_version(host, plugin).unwrap_err();
        assert_eq!(err.host, host);
        assert_eq!(err.plugin, plugin);
        assert!(format!("{}", err).contains("主版本不兼容"));
    }

    #[test]
    fn negotiate_same_major() {
        // 宿主 1.2 + 插件 1.0 → 插件不需降级（返回 true）
        let host = ProtocolVersion { major: 1, minor: 2 };
        assert!(negotiate_version(host, ProtocolVersion::V1_0).unwrap());
        // 宿主 1.0 + 插件 1.2 → 插件需降级但仍加载（返回 false）
        assert!(!negotiate_version(
            ProtocolVersion::V1_0,
            ProtocolVersion { major: 1, minor: 2 }
        )
        .unwrap());
    }

    // ── 空 tools / 空 permissions / 空 capabilities 正常 ──

    #[test]
    fn empty_tools_and_capabilities_allowed() {
        let manifest_toml = r#"
[plugin]
id = "com.example.ocr"
name = "O"
version = "1.0.0"

[exec]
command = "python"
"#;
        let m = load_from_str(manifest_toml).unwrap();
        assert!(m.tools.is_empty());
        assert!(m.capabilities.permissions.is_empty());
    }

    #[test]
    fn tools_default_input_schema_applied() {
        // [[tools]] 未显式写 input_schema 时，补默认的 object 空 schema。
        let manifest_toml = r#"
[plugin]
id = "com.example.ocr"
name = "O"
version = "1.0.0"

[exec]
command = "python"

[[tools]]
name = "tool:noargs"
"#;
        let m = load_from_str(manifest_toml).unwrap();
        assert_eq!(m.tools[0].input_schema["type"], "object");
        assert_eq!(m.tools[0].input_schema["required"].as_array().unwrap().len(), 0);
        assert_eq!(
            m.tools[0].input_schema["properties"]
                .as_object()
                .unwrap()
                .len(),
            0
        );
    }

    // ── parse_toml + validate 分离：validate 不污染原结构 ──

    #[test]
    fn validate_does_not_break_round_trip() {
        let m1 = parse_toml(SAMPLE).unwrap();
        let m2 = m1.validate().unwrap();
        // 经 validate 后字段不应被变更（对工具结构而言 validate 是 &mut，因此本测试能
        // 拦住未来有人在 validate 里改写字段的潜在事故）。
        let m1_again = parse_toml(SAMPLE).unwrap();
        assert_eq!(m2, m1_again);
    }
}
