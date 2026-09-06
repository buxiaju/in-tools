//! 插件 `manifest.toml` 的结构、解析与校验。
//!
//! manifest 的表：`[plugin]`（元信息）、`[exec]`（启动命令与参数）、
//! `[[tools]]`（静态工具清单）、`[capabilities]`（权限声明）、`[lifecycle]`
//! （生命周期策略）。前五个表严格对应设计文档 §4.1 的示例。
//!
//! 另有三个可选表，用于「插件自带交互入口与文档」：
//! - `[shortcut]`：全局快捷键绑定。**可有可无**——插件作者决定是否适合热键调用。
//! - `[[settings]]`：用户可配置参数，宿主据此渲染设置表单。
//! - `[docs]`：说明文档文件名，宿主在插件卡片上提供「说明」入口。
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
    /// 可选的全局快捷键绑定。声明后宿主启动时自动注册，按下即调用指定工具。
    #[serde(default)]
    pub shortcut: Option<Shortcut>,
    /// 用户可配置参数列表。宿主据此渲染设置页表单，值持久化到
    /// `~/.intools/plugin-settings/<plugin_id>.json`。
    #[serde(default)]
    pub settings: Vec<SettingField>,
    /// 可选的说明文档声明。声明后宿主在插件卡片上显示「说明」按钮。
    #[serde(default)]
    pub docs: Option<Docs>,
    /// 工具结果的结构化展示声明。
    #[serde(default)]
    pub result_display: Vec<ResultDisplay>,
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
            return Err(ManifestError::Validation(
                "exec.command 不能为空".to_string(),
            ));
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
                return Err(ManifestError::Validation("tools.name 不能为空".to_string()));
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

        // 6. 快捷键绑定（可选）：tool 非空且须在本插件 tools 列表中。
        //    key 允许为空串 —— 表示「本插件适合热键调用，但作者不预设按键，
        //    交由用户在设置页自行绑定」。空白先归一成空串，免得后面到处 trim。
        if let Some(sc) = &mut self.shortcut {
            sc.key = sc.key.trim().to_string();
        }
        if let Some(sc) = &self.shortcut {
            if sc.tool.trim().is_empty() {
                return Err(ManifestError::Validation(
                    "shortcut.tool 不能为空".to_string(),
                ));
            }
            let tool_exists = self.tools.iter().any(|t| t.name == sc.tool);
            if !tool_exists {
                return Err(ManifestError::Validation(format!(
                    "shortcut.tool `{}` 未在本插件的 [[tools]] 中声明", sc.tool
                )));
            }
        }

        // 7. 设置字段：key 非空且不重复；Select 必须有 options
        let mut seen_keys = std::collections::HashSet::new();
        for (i, sf) in self.settings.iter().enumerate() {
            if sf.key.trim().is_empty() {
                return Err(ManifestError::Validation(format!(
                    "settings[{i}].key 不能为空"
                )));
            }
            if !seen_keys.insert(sf.key.clone()) {
                return Err(ManifestError::Validation(format!(
                    "settings[{i}].key `{}` 与前面的字段重复", sf.key
                )));
            }
            if sf.label.trim().is_empty() {
                return Err(ManifestError::Validation(format!(
                    "settings[{i}].label 不能为空（key=`{}`）", sf.key
                )));
            }
            if sf.field_type == SettingType::Select && sf.options.is_empty() {
                return Err(ManifestError::Validation(format!(
                    "settings[{i}] type=select 但 options 为空（key=`{}`）", sf.key
                )));
            }
        }

        // 8. 说明文档（可选）：usage_file 必须是插件目录内的纯文件名。
        //    这个值来自插件作者（不可信输入），会被宿主拼到 plugin_dir 上读文件，
        //    所以必须挡住路径穿越：不允许分隔符，也不允许 . / .. 这种特殊名。
        if let Some(docs) = &self.docs {
            let f = docs.usage_file.trim();
            if f.is_empty() {
                return Err(ManifestError::Validation(
                    "docs.usage_file 不能为空".to_string(),
                ));
            }
            if f.contains('/') || f.contains('\\') || f == "." || f == ".." || f.contains("..") {
                return Err(ManifestError::Validation(format!(
                    "docs.usage_file `{f}` 必须是插件目录下的纯文件名（不允许路径分隔符或 ..）"
                )));
            }
        }

        // 9. result_display 校验
        let tool_names: std::collections::HashSet<&str> =
            self.tools.iter().map(|t| t.name.as_str()).collect();
        for rd in &self.result_display {
            if !tool_names.contains(rd.tool.as_str()) {
                return Err(ManifestError::Validation(format!(
                    "result_display.tool '{}' 未匹配任何已声明的工具",
                    rd.tool
                )));
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
    /// 工具级权限声明（可选）。
    ///
    /// 如果声明了，工具调用时会检查这些权限，而不是使用插件级权限。
    /// 这允许更细粒度的权限控制。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub permissions: Vec<Permission>,
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
    /// 资源限制配置。
    #[serde(default, skip_serializing_if = "ResourceLimits::is_default")]
    pub resource_limits: ResourceLimits,
}

impl Default for Lifecycle {
    fn default() -> Self {
        Self {
            mode: LifecycleMode::OnDemand,
            idle_timeout_sec: Some(300),
            restart_policy: RestartPolicy::OnFailure,
            request_timeout_sec: Some(30),
            resource_limits: ResourceLimits::default(),
        }
    }
}

/// 资源限制配置。
///
/// 用于限制插件进程的资源使用，防止恶意或失控的插件消耗过多系统资源。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ResourceLimits {
    /// 最大内存使用量（MB）。0 表示不限制。
    pub max_memory_mb: u32,
    /// 最大 CPU 使用率（百分比，0-100）。0 表示不限制。
    pub max_cpu_percent: u32,
    /// 最大磁盘使用量（MB）。0 表示不限制。
    pub max_disk_mb: u32,
    /// 最大网络带宽（KB/s）。0 表示不限制。
    pub max_network_kbps: u32,
}

impl Default for ResourceLimits {
    fn default() -> Self {
        Self {
            max_memory_mb: 0,
            max_cpu_percent: 0,
            max_disk_mb: 0,
            max_network_kbps: 0,
        }
    }
}

impl ResourceLimits {
    /// 检查是否为默认配置（所有限制都为 0）。
    pub fn is_default(&self) -> bool {
        self.max_memory_mb == 0
            && self.max_cpu_percent == 0
            && self.max_disk_mb == 0
            && self.max_network_kbps == 0
    }

    /// 检查是否有任何资源限制。
    pub fn has_limits(&self) -> bool {
        !self.is_default()
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

// ─────────────────── 快捷键绑定 ───────────────────

/// 插件可选的全局快捷键绑定。
///
/// manifest 中写为 `[shortcut]` 表：
/// ```toml
/// [shortcut]
/// key = "Ctrl+Shift+S"
/// tool = "screenshot:capture"
/// ```
///
/// 宿主启动时扫描所有已注册插件的 `shortcut`，用 `tauri-plugin-global-shortcut`
/// 注册热键。按下时直接走 `Supervisor::call_tool` 调用指定工具，绕过 AI/MCP 路径。
/// 这是「独立调用」的最短路径——不需要打开主窗口、不需要 AI 介入。
///
/// **整个表都是可选的**：不写 `[shortcut]` 意味着这个插件只通过 AI/MCP 调用，
/// 设置页也不会出现快捷键区块。写了但 `key = ""`，则表示「支持热键但不预设按键」，
/// 设置页会显示一个空的待绑定输入框，等用户自己录入。
///
/// manifest 里的 `key` 只是**默认值**。用户在设置页的改动存到
/// `~/.intools/shortcuts.json`，优先级高于此处，且不会回写插件目录。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Shortcut {
    /// 快捷键串，格式由 `tauri-plugin-global-shortcut` 定义，如 `Ctrl+Shift+S`。
    /// 允许为空串：表示插件支持热键但不预设按键，交由用户绑定。
    #[serde(default)]
    pub key: String,
    /// 按下时调用的工具名，必须在本插件的 `[[tools]]` 中声明。
    pub tool: String,
    /// 可选的交互界面类型。目前支持 `"region-select"`：按下快捷键后先弹出
    /// 全屏透明覆盖层，用户拖拽框选区域，确认后再用选区坐标调用 `tool`。
    #[serde(default)]
    pub ui: Option<String>,
}

// ─────────────────── 说明文档 ───────────────────

/// 插件自带的使用说明声明。
///
/// manifest 中写为 `[docs]` 表：
/// ```toml
/// [docs]
/// usage_file = "README.md"
/// ```
///
/// `usage_file` 是**插件目录下的纯文件名**（不是路径），宿主用
/// `plugin_dir.join(usage_file)` 读取后在弹窗里渲染。内容按 Markdown 子集展示，
/// 且全程走 `textContent`，因此插件作者无法通过 README 注入脚本。
///
/// 与 `[shortcut]` 一样整表可选：不写就没有「说明」按钮。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Docs {
    /// 说明文档文件名，相对插件根目录，例如 `README.md`。
    pub usage_file: String,
}

// ─────────────────── 设置字段 ───────────────────

/// 设置字段类型。宿主据此渲染对应的表单控件。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SettingType {
    /// 单行文本框。
    String,
    /// 数字输入框。
    Number,
    /// 开关。
    Boolean,
    /// 下拉选择框，须提供 `options`。
    Select,
    /// 颜色选择器，前端渲染为 color input。
    Color,
    /// 文件路径选择，前端渲染为带浏览按钮的文本框。
    Path,
    /// 密码输入，前端渲染为遮罩文本框。
    Password,
}

/// 插件可配置参数的字段描述。
///
/// manifest 中写为 `[[settings]]` 数组，每项描述一个可在设置页调整的参数：
/// ```toml
/// [[settings]]
/// key = "output_dir"
/// label = "截图保存目录"
/// type = "string"
/// default = "~/Pictures"
///
/// [[settings]]
/// key = "image_format"
/// label = "图片格式"
/// type = "select"
/// default = "png"
/// options = ["png", "jpg"]
/// ```
///
/// 宿主在 `call_tool` 时将设置值合并进工具参数：用户显式传参优先于设置默认值。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SettingField {
    /// 参数键名，传给工具时作为 args 的键。
    pub key: String,
    /// 人类可读标签，显示在设置页表单上。
    pub label: String,
    /// 字段类型。
    #[serde(rename = "type")]
    pub field_type: SettingType,
    /// 默认值。未在设置文件中覆盖时使用。
    #[serde(default)]
    pub default: JsonValue,
    /// 当 `field_type = Select` 时的可选项列表。
    #[serde(default)]
    pub options: Vec<String>,
    /// 字段分组标题。相同 group 的字段会归入同一个视觉区域。
    #[serde(default)]
    pub group: Option<String>,
    /// 字段下方的说明文字。
    #[serde(default)]
    pub description: Option<String>,
    /// 输入框占位提示。
    #[serde(default)]
    pub placeholder: Option<String>,
    /// number 类型的最小值。
    #[serde(default)]
    pub min: Option<f64>,
    /// number 类型的最大值。
    #[serde(default)]
    pub max: Option<f64>,
    /// number 类型的步长。
    #[serde(default)]
    pub step: Option<f64>,
}

// ─────────────────── 结果展示 ───────────────────

/// 工具结果的结构化展示声明。
///
/// 插件在 manifest 中声明工具结果的展示方式，宿主据此将 JSON 结果渲染为
/// 可读的 UI 组件，而非原始 JSON 文本。声明写在 `[[result_display]]` 段。
///
/// ```toml
/// [[result_display]]
/// tool = "color:pick"
/// type = "kv"
/// fields = [
///   { key = "hex", label = "HEX" },
///   { key = "rgb_string", label = "RGB" },
/// ]
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResultDisplay {
    /// 绑定的工具名，须与 `[[tools]]` 中的某个 name 匹配。
    pub tool: String,
    /// 展示类型。
    #[serde(rename = "type")]
    pub display_type: ResultDisplayType,
    /// kv 模式：要展示的字段列表。
    #[serde(default)]
    pub fields: Vec<ResultField>,
    /// table 模式：列定义。
    #[serde(default)]
    pub columns: Vec<ResultColumn>,
    /// markdown 模式：指定包含 markdown 文本的字段名。
    #[serde(default)]
    pub content_key: Option<String>,
    /// 所有模式通用：结果根对象中包含状态信息的字段名。
    #[serde(default)]
    pub status_key: Option<String>,
}

/// 结果展示类型。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ResultDisplayType {
    /// 键值对展示，每个 field 显示为一行 label: value。
    Kv,
    /// 表格展示，适合列表数据。
    Table,
    /// Markdown 文本渲染。
    Markdown,
    /// 原始 JSON 展示（降级）。
    Raw,
}

/// kv 模式下的字段声明。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResultField {
    /// JSON 结果中的键名。
    pub key: String,
    /// 人类可读标签。
    pub label: String,
}

/// table 模式下的列声明。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResultColumn {
    /// JSON 结果数组元素中的键名。
    pub key: String,
    /// 列标题。
    pub label: String,
}

// ─────────────────── 权限 ───────────────────

/// 权限的「方面」——把系统的所有面分成 16 个桶，每个桶下的 [`PermissionAction`]
/// 是该面允许的具体动作。新增类别只在这两个枚举里加，不动其他代码。
///
/// 字符串映射规则是单一来源：`as_str()` / `parse_str()`。所以「`process:spawn`
/// 在新类别表里实际叫什么」靠这两个方法与 `PermissionCategory::parse_str` 锁死，
/// 改动一处即知全部影响面。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PermissionCategory {
    /// 文件系统读写
    File,
    /// 网络访问（HTTP / WebSocket / DNS / 原始 socket）
    Network,
    /// 起新进程
    Process,
    /// 执行 shell 命令（与 `process:spawn` 不同——后者是裸起二进制，
    /// 这里专指通过 cmd / PowerShell / bash 间接执行）
    Shell,
    /// 屏幕截图 / 录屏
    Screen,
    /// 模拟键鼠输入
    Input,
    /// 读写剪贴板
    Clipboard,
    /// 麦克风采集 / 扬声器播放
    Audio,
    /// 系统级操作（关机 / 重启 / 注销 / 锁屏）
    System,
    /// 操纵其他窗口（最小化 / 关闭 / 置顶 / 移动）
    Window,
    /// 启动其他桌面应用（带 GUI 的可执行文件）
    App,
    /// Windows 注册表读写
    Registry,
    /// 读取 / 修改凭据存储
    Credential,
    /// 访问密钥 / 证书（系统或用户级）
    Crypto,
    /// 发送系统通知
    Notification,
    /// 摄像头 / 蓝牙 / 串口等通用硬件访问
    Hardware,
    /// 安装 / 卸载 / 自启动等持久化行为
    Persistence,
    /// 计划任务 / cron 表达式注册
    Schedule,
    /// 读取 / 修改进程环境变量
    Environment,
}

impl PermissionCategory {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Network => "network",
            Self::Process => "process",
            Self::Shell => "shell",
            Self::Screen => "screen",
            Self::Input => "input",
            Self::Clipboard => "clipboard",
            Self::Audio => "audio",
            Self::System => "system",
            Self::Window => "window",
            Self::App => "app",
            Self::Registry => "registry",
            Self::Credential => "credential",
            Self::Crypto => "crypto",
            Self::Notification => "notification",
            Self::Hardware => "hardware",
            Self::Persistence => "persistence",
            Self::Schedule => "schedule",
            Self::Environment => "environment",
        }
    }

    pub fn parse_str(s: &str) -> Option<Self> {
        Some(match s {
            "file" => Self::File,
            "network" => Self::Network,
            "process" => Self::Process,
            "shell" => Self::Shell,
            "screen" => Self::Screen,
            "input" => Self::Input,
            "clipboard" => Self::Clipboard,
            "audio" => Self::Audio,
            "system" => Self::System,
            "window" => Self::Window,
            "app" => Self::App,
            "registry" => Self::Registry,
            "credential" => Self::Credential,
            "crypto" => Self::Crypto,
            "notification" => Self::Notification,
            "hardware" => Self::Hardware,
            "persistence" => Self::Persistence,
            "schedule" => Self::Schedule,
            "environment" => Self::Environment,
            _ => return None,
        })
    }

    /// 人类可读的中文名。前端授权弹窗里用它显示「权限面」。
    pub fn label(self) -> &'static str {
        match self {
            Self::File => "文件",
            Self::Network => "网络",
            Self::Process => "进程",
            Self::Shell => "Shell",
            Self::Screen => "屏幕",
            Self::Input => "输入",
            Self::Clipboard => "剪贴板",
            Self::Audio => "音频",
            Self::System => "系统",
            Self::Window => "窗口",
            Self::App => "应用",
            Self::Registry => "注册表",
            Self::Credential => "凭据",
            Self::Crypto => "密钥",
            Self::Notification => "通知",
            Self::Hardware => "硬件",
            Self::Persistence => "持久化",
            Self::Schedule => "计划任务",
            Self::Environment => "环境变量",
        }
    }
}

/// 在某一「方面」下能做的具体动作。
///
/// 新增动作先评估两件事：
/// 1. 它属于哪个 [`PermissionCategory`]？
/// 2. 它的危险度是多少？见 [`PermissionAction::default_danger`]。
///
/// 「网络」类别下有一组具体的协议动作（[`PermissionAction::Http`] /
/// [`PermissionAction::WebSocket`] / [`PermissionAction::Dns`] /
/// [`PermissionAction::Socket`]），它们与通用动作 [`PermissionAction::Send`] /
/// [`PermissionAction::Receive`] 二选一：要么用通用动作表达「能收发字节」，
/// 要么用具体协议表达「按 HTTP / WebSocket 等协议访问」。同时声明两条等价，
/// 校验层会拒掉冗余声明（见 [`validate_pair`]）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PermissionAction {
    Read,
    Write,
    Capture,
    Record,
    Control,
    Spawn,
    Exec,
    Send,
    Receive,
    Manage,
    Modify,
    Install,
    Uninstall,
    /// `network:http` —— HTTP/HTTPS 请求
    Http,
    /// `network:websocket` —— WebSocket 全双工
    WebSocket,
    /// `network:dns` —— DNS 查询
    Dns,
    /// `network:socket` —— 原始 TCP / UDP socket
    Socket,
}

impl PermissionAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::Capture => "capture",
            Self::Record => "record",
            Self::Control => "control",
            Self::Spawn => "spawn",
            Self::Exec => "exec",
            Self::Send => "send",
            Self::Receive => "receive",
            Self::Manage => "manage",
            Self::Modify => "modify",
            Self::Install => "install",
            Self::Uninstall => "uninstall",
            Self::Http => "http",
            Self::WebSocket => "websocket",
            Self::Dns => "dns",
            Self::Socket => "socket",
        }
    }

    pub fn parse_str(s: &str) -> Option<Self> {
        Some(match s {
            "read" => Self::Read,
            "write" => Self::Write,
            "capture" => Self::Capture,
            "record" => Self::Record,
            "control" => Self::Control,
            "spawn" => Self::Spawn,
            "exec" => Self::Exec,
            "send" => Self::Send,
            "receive" => Self::Receive,
            "manage" => Self::Manage,
            "modify" => Self::Modify,
            "install" => Self::Install,
            "uninstall" => Self::Uninstall,
            "http" => Self::Http,
            "websocket" => Self::WebSocket,
            "dns" => Self::Dns,
            "socket" => Self::Socket,
            _ => return None,
        })
    }
}

/// 单个权限声明，形如 `screen:capture`、`file:read`、`file:read:/foo`。
///
/// 内部格式：`CATEGORY:ACTION[:SCOPE]`。示例：
/// - `file:read`（分类 `file`，动作 `read`，无范围）
/// - `file:read:~/Pictures`（分类 `file`，动作 `read`，范围 `~/Pictures`）
/// - `process:spawn`（高危，无范围）
///
/// 解析时会校验 `category` 与 `action` 是否在 [`PermissionCategory`] / [`PermissionAction`]
/// 的枚举表内——未知类别一律拒绝，避免「写下 `xyz:read` 也能通过校验」
/// 这种开口。范围 `scope` 是自由字符串（路径、域名、`*`），但具体含义
/// 留给插件自己的运行时去解释。
///
/// 按分类 + 动作映射到三级危险度，见 [`Permission::danger_level`]。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Permission {
    pub category: String,
    pub action: String,
    pub scope: Option<String>,
}

/// 权限危险度。带 serde 是为了让权限弹窗按危险度渲染不同的警示样式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
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
        // 类别 + 动作必须在白名单内。`PermissionCategory::parse_str` 返回 `None`
        // 时附带一行「已知类别有……」提示，比「unknown category」这种半截报错更有用。
        let category = PermissionCategory::parse_str(cat).ok_or_else(|| {
            format!(
                "权限 `{s}` 的类别 `{cat}` 不在白名单（已知类别：{}）",
                known_categories()
            )
        })?;
        let action = PermissionAction::parse_str(act).ok_or_else(|| {
            format!(
                "权限 `{s}` 的动作 `{act}` 不在白名单（已知动作：{}）",
                known_actions()
            )
        })?;
        // 类别与动作的合理性自检（拒掉「`clipboard:spawn`」这种荒诞组合），
        // 避免以后误打一个字符串就能通过校验。
        if let Err(msg) = validate_pair(category, action) {
            return Err(format!("权限 `{s}` 组合不合法：{msg}"));
        }
        Ok(Permission {
            category: category.as_str().to_string(),
            action: action.as_str().to_string(),
            scope: scope.map(|s| s.to_string()),
        })
    }

    /// 按设计文档 §7 的三级危险度分类。
    ///
    /// 规则集中在一处（[`PermissionAction::default_danger`] + `scope_elevates`
    /// 提升规则），新增类别时**只**改那张表 + `validate_pair` 这两处即可，
    /// 不用动本函数。
    pub fn danger_level(&self) -> DangerLevel {
        let Some(category) = PermissionCategory::parse_str(&self.category) else {
            // 未知类别按低危处理——`parse` 已经把这条路径堵死，
            // 这里是兜底（譬如从老配置文件手动 JSON 注入的脏数据）。
            return DangerLevel::Low;
        };
        let Some(action) = PermissionAction::parse_str(&self.action) else {
            return DangerLevel::Low;
        };
        let base = action.default_danger();
        let elevated_by_scope = scope_elevates(category, action, self.scope.as_deref());
        let elevated_by_read = matches!(action, PermissionAction::Read)
            && read_danger_elevates(category);
        if elevated_by_scope || elevated_by_read {
            base.elevate()
        } else {
            base
        }
    }
}

impl DangerLevel {
    /// 危险度向上提一级。`High` 已是顶，不再变。
    fn elevate(self) -> Self {
        match self {
            Self::Low => Self::Medium,
            Self::Medium | Self::High => Self::High,
        }
    }
}

impl PermissionAction {
    /// 该动作在「无范围限定」时的默认危险度。新增动作时这是必填项。
    ///
    /// 评分原则：
    /// - Low：可观察到的副作用能被用户撤回、或只在用户主动发起的场景出现。
    /// - Medium：能改用户能看到的东西（屏幕、剪贴板、文件、窗口），
    ///   但范围有限（不覆盖整个机器）。
    /// - High：能接管机器行为本身（执行、起进程、改系统状态、捕获输入）。
    pub fn default_danger(self) -> DangerLevel {
        match self {
            Self::Read => DangerLevel::Low,
            Self::Receive => DangerLevel::Low,
            Self::Send => DangerLevel::Low,
            Self::Http => DangerLevel::Low,
            Self::WebSocket => DangerLevel::Low,
            Self::Dns => DangerLevel::Low,

            Self::Write => DangerLevel::Medium,
            Self::Capture => DangerLevel::Medium,
            Self::Record => DangerLevel::Medium,
            Self::Modify => DangerLevel::Medium,
            Self::Manage => DangerLevel::Medium,

            Self::Control => DangerLevel::High,
            Self::Spawn => DangerLevel::High,
            Self::Exec => DangerLevel::High,
            Self::Install => DangerLevel::High,
            Self::Uninstall => DangerLevel::High,
            // 裸 TCP/UDP socket 等同任意网络目标——提一档让用户看到。
            Self::Socket => DangerLevel::High,
        }
    }
}

/// 「读」类动作的细调：哪些 `Read` 因为泄漏后果严重而需要升档。
///
/// 集中一处维护，避免散在 `danger_level` 默认值里加 if 分支——每加一条
/// 都要在这写明「为什么」，而不是埋进表达式。
pub fn read_danger_elevates(category: PermissionCategory) -> bool {
    use PermissionCategory::*;
    // 剪贴板：可能含密码 / 2FA / 私钥片段。
    // 凭据 / 密钥：本身就是敏感凭据。
    matches!(category, Clipboard | Credential | Crypto | Registry)
}

/// scope 把动作的危险度往上一级推。
///
/// 「`file:write:/foo`」是用户指定的目录——危险度 Medium 已经合适；
/// 「`file:write:*`」覆盖任意目录——这一条必须升级到 High，否则用户
/// 看到一个 Low / Medium 的「写文件」就放行了，实际上能写整个磁盘。
///
/// 网络协议动作的 `*` 升档机制不同：默认 Low 的 `Http` / `WebSocket` / `Dns`
/// 在「任意主机」时升到 Medium（值得弹窗），但不必到 High——HTTP / DNS 是
/// 良性协议，与 `Shell:exec` 任意命令不同。高危动作（`Socket`、`Spawn`、`Exec`）
/// 的 `*` 直接升到 High。
fn scope_elevates(category: PermissionCategory, action: PermissionAction, scope: Option<&str>) -> bool {
    if scope != Some("*") {
        return false;
    }
    use PermissionAction::*;
    use PermissionCategory::*;
    match (category, action) {
        // 文件读写覆盖任意路径——升到 High。
        (File, Write | Read) => true,
        // 起任意进程 / 任意 shell 命令——High。
        (Process, Spawn) | (Shell, Exec) => true,
        // 任意主机的网络协议值得升档（Low → Medium），让用户看一眼。
        // - 裸 socket 默认就是 High（等同任意网络目标），不再加这层升档。
        (Network, Http | WebSocket | Dns) => true,
        (Network, Socket) => true,
        _ => false,
    }
}

/// 校验「类别 + 动作」的组合是否说得通。拒掉 `clipboard:spawn` 这类荒诞对。
///
/// 判定原则：每个类别只允许它「能合理承担」的动作。`PermissionAction` 是
/// 跨类别共享的动作词表，所以必须在这里显式约束。
fn validate_pair(
    category: PermissionCategory,
    action: PermissionAction,
) -> Result<(), &'static str> {
    use PermissionAction::*;
    use PermissionCategory::*;
    let ok = match (category, action) {
        (File, Read | Write) => true,
        (Network, Read | Write | Send | Receive | Http | WebSocket | Dns | Socket) => true,
        (Process, Spawn) => true,
        (Shell, Exec) => true,
        (Screen, Capture | Record) => true,
        (Input, Control) => true,
        (Clipboard, Read | Write) => true,
        (Audio, Capture | Record | Send | Receive) => true,
        (System, Manage) => true,
        (Window, Manage | Modify) => true,
        (App, Spawn) => true,
        (Registry, Read | Write | Modify) => true,
        (Credential, Read | Write) => true,
        (Crypto, Read | Write) => true,
        (Notification, Send) => true,
        (Hardware, Read | Write | Control) => true,
        (Persistence, Install | Uninstall | Modify) => true,
        (Schedule, Manage) => true,
        (Environment, Read | Write) => true,
        _ => false,
    };
    if ok {
        Ok(())
    } else {
        Err("类别与动作的搭配不在白名单内")
    }
}

fn known_categories() -> String {
    let all: Vec<&str> = [
        PermissionCategory::File,
        PermissionCategory::Network,
        PermissionCategory::Process,
        PermissionCategory::Shell,
        PermissionCategory::Screen,
        PermissionCategory::Input,
        PermissionCategory::Clipboard,
        PermissionCategory::Audio,
        PermissionCategory::System,
        PermissionCategory::Window,
        PermissionCategory::App,
        PermissionCategory::Registry,
        PermissionCategory::Credential,
        PermissionCategory::Crypto,
        PermissionCategory::Notification,
        PermissionCategory::Hardware,
        PermissionCategory::Persistence,
        PermissionCategory::Schedule,
        PermissionCategory::Environment,
    ]
    .iter()
    .map(|c| c.as_str())
    .collect();
    all.join(", ")
}

fn known_actions() -> String {
    let all: Vec<&str> = [
        PermissionAction::Read,
        PermissionAction::Write,
        PermissionAction::Capture,
        PermissionAction::Record,
        PermissionAction::Control,
        PermissionAction::Spawn,
        PermissionAction::Exec,
        PermissionAction::Send,
        PermissionAction::Receive,
        PermissionAction::Manage,
        PermissionAction::Modify,
        PermissionAction::Install,
        PermissionAction::Uninstall,
        PermissionAction::Http,
        PermissionAction::WebSocket,
        PermissionAction::Dns,
        PermissionAction::Socket,
    ]
    .iter()
    .map(|a| a.as_str())
    .collect();
    all.join(", ")
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
        let ManifestError::Validation(txt) = err else {
            panic!()
        };
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
        let ManifestError::Validation(txt) = err else {
            panic!()
        };
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
        let ManifestError::Validation(txt) = err else {
            panic!()
        };
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
        let ManifestError::Validation(txt) = err else {
            panic!()
        };
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
        let ManifestError::Validation(txt) = err else {
            panic!()
        };
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
        let ManifestError::Validation(txt) = err else {
            panic!()
        };
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
        let ManifestError::Validation(txt) = err else {
            panic!()
        };
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
        let ManifestError::Validation(txt) = err else {
            panic!()
        };
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
        let ManifestError::Validation(txt) = err else {
            panic!()
        };
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
        let ManifestError::Validation(txt) = err else {
            panic!()
        };
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
            Permission::parse("file:write:~/a.txt")
                .unwrap()
                .danger_level(),
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

    /// 拒绝未知类别。`Permission::parse` 必须把 `xyz:read` 这种字符串挡掉，
    /// 否则下游危险度映射会把它当 Low 默默放行——「写下任何字符串就能过校验」
    /// 是权限模型最严重的开口。
    #[test]
    fn permission_unknown_category_rejected() {
        let err = Permission::parse("xyz:read").unwrap_err();
        assert!(err.contains("xyz"), "错误信息应包含未知类别：{err}");
        assert!(err.contains("已知类别"), "应提示已知类别：{err}");
    }

    /// 拒绝未知动作。同上原因，单纯 `file:fly` 应当被拒。
    #[test]
    fn permission_unknown_action_rejected() {
        let err = Permission::parse("file:fly").unwrap_err();
        assert!(err.contains("fly"), "错误信息应包含未知动作：{err}");
        assert!(err.contains("已知动作"), "应提示已知动作：{err}");
    }

    /// 类别与动作的组合不合理（譬如 `clipboard:spawn`）应当被拒。
    /// `PermissionAction` 是跨类别共享的词表，必须显式约束。
    #[test]
    fn permission_invalid_pair_rejected() {
        // 剪贴板没有 spawn 这种动作
        let err = Permission::parse("clipboard:spawn").unwrap_err();
        assert!(err.contains("组合不合法"), "应说明组合不合法：{err}");
        // 屏幕不可能 modify
        let err2 = Permission::parse("screen:install").unwrap_err();
        assert!(err2.contains("组合不合法"), "应说明组合不合法：{err2}");
    }

    /// 「scope = `*`」把权限升档——典型场景：任意文件写、任意网络目标。
    /// 这一条规则覆盖的是「**默认允许的子集**也是全集」的最危险情形。
    #[test]
    fn permission_scope_star_elevates_danger() {
        assert_eq!(
            Permission::parse("file:write:*").unwrap().danger_level(),
            DangerLevel::High
        );
        // 没 scope 的同类权限保持中危。
        assert_eq!(
            Permission::parse("file:write:~/Documents").unwrap().danger_level(),
            DangerLevel::Medium
        );
        // 网络目标的 `*` 升档：任意主机。HTTP 默认 Low，`*` 提到 Medium 即可——
        // 已是合法协议，不必升到 High。
        assert_eq!(
            Permission::parse("network:http:*").unwrap().danger_level(),
            DangerLevel::Medium
        );
        // 指定域名不升档。
        assert_eq!(
            Permission::parse("network:http:api.example.com")
                .unwrap()
                .danger_level(),
            DangerLevel::Low
        );
    }

    /// 「读」类动作在某些类别下需要升档，因为内容可能含敏感数据。
    /// 例如剪贴板读、凭据读、注册表读。
    #[test]
    fn permission_read_in_sensitive_category_elevates() {
        assert_eq!(
            Permission::parse("clipboard:read").unwrap().danger_level(),
            DangerLevel::Medium
        );
        assert_eq!(
            Permission::parse("credential:read").unwrap().danger_level(),
            DangerLevel::Medium
        );
        assert_eq!(
            Permission::parse("crypto:read").unwrap().danger_level(),
            DangerLevel::Medium
        );
        // 普通读仍然是低危。
        assert_eq!(
            Permission::parse("file:read:~/Documents")
                .unwrap()
                .danger_level(),
            DangerLevel::Low
        );
    }

    /// 网络协议动作各自有合理默认危险度。`Socket` 因等同任意网络目标而 High。
    #[test]
    fn permission_network_protocol_danger() {
        assert_eq!(
            Permission::parse("network:http").unwrap().danger_level(),
            DangerLevel::Low
        );
        assert_eq!(
            Permission::parse("network:websocket").unwrap().danger_level(),
            DangerLevel::Low
        );
        assert_eq!(
            Permission::parse("network:socket").unwrap().danger_level(),
            DangerLevel::High
        );
    }

    /// 每个新类别至少有一个常见的合理组合应当被接受，覆盖「全类别可达」语义。
    /// 漏掉任何一类都会让插件作者无法声明该权限，进而违反「覆盖各个方面」。
    #[test]
    fn permission_all_categories_have_one_valid_pair() {
        let cases = [
            "file:read",
            "network:http",
            "process:spawn",
            "shell:exec",
            "screen:capture",
            "input:control",
            "clipboard:write",
            "audio:capture",
            "system:manage",
            "window:manage",
            "app:spawn",
            "registry:read",
            "credential:read",
            "crypto:read",
            "notification:send",
            "hardware:read",
            "persistence:install",
            "schedule:manage",
            "environment:read",
        ];
        for c in cases {
            Permission::parse(c).unwrap_or_else(|e| panic!("`{c}` 应当可解析：{e}"));
        }
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
        assert_eq!(
            m.tools[0].input_schema["required"]
                .as_array()
                .unwrap()
                .len(),
            0
        );
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

    // ── shortcut 表 ──────────────────────────────────

    #[test]
    fn shortcut_parsed_correctly() {
        let m = load_from_str(
            r#"
[plugin]
id = "com.example.screenshot"
name = "Screenshot"
version = "0.1.0"

[exec]
command = "python"

[[tools]]
name = "screenshot:capture"
[tools.input_schema]
type = "object"

[shortcut]
key = "Ctrl+Shift+S"
tool = "screenshot:capture"
"#,
        )
        .unwrap();
        let sc = m.shortcut.unwrap();
        assert_eq!(sc.key, "Ctrl+Shift+S");
        assert_eq!(sc.tool, "screenshot:capture");
    }

    #[test]
    fn shortcut_absent_by_default() {
        // 不写 [shortcut] 表时 shortcut 为 None
        let m = load_from_str(
            r#"
[plugin]
id = "com.example.x"
name = "X"
version = "1.0.0"

[exec]
command = "python"
"#,
        )
        .unwrap();
        assert!(m.shortcut.is_none());
    }

    #[test]
    fn shortcut_empty_key_accepted_as_unbound() {
        // key 为空/纯空白 → 合法，语义是「支持热键但不预设按键」，且被归一成空串。
        let m = load_from_str(
            r#"
[plugin]
id = "com.example.x"
name = "X"
version = "1.0.0"

[exec]
command = "python"

[[tools]]
name = "tool:run"
[tools.input_schema]
type = "object"

[shortcut]
key = "  "
tool = "tool:run"
"#,
        )
        .unwrap();
        let sc = m.shortcut.unwrap();
        assert_eq!(sc.key, "");
        assert_eq!(sc.tool, "tool:run");
    }

    #[test]
    fn shortcut_key_omitted_entirely_accepted() {
        // 连 key 字段都不写也可以，serde default 给空串。
        let m = load_from_str(
            r#"
[plugin]
id = "com.example.x"
name = "X"
version = "1.0.0"

[exec]
command = "python"

[[tools]]
name = "tool:run"
[tools.input_schema]
type = "object"

[shortcut]
tool = "tool:run"
"#,
        )
        .unwrap();
        assert_eq!(m.shortcut.unwrap().key, "");
    }

    #[test]
    fn shortcut_key_trimmed() {
        let m = load_from_str(
            r#"
[plugin]
id = "com.example.x"
name = "X"
version = "1.0.0"

[exec]
command = "python"

[[tools]]
name = "tool:run"
[tools.input_schema]
type = "object"

[shortcut]
key = "  Ctrl+Alt+K  "
tool = "tool:run"
"#,
        )
        .unwrap();
        assert_eq!(m.shortcut.unwrap().key, "Ctrl+Alt+K");
    }

    #[test]
    fn shortcut_empty_tool_rejected() {
        let err = load_from_str(
            r#"
[plugin]
id = "com.example.x"
name = "X"
version = "1.0.0"

[exec]
command = "python"

[[tools]]
name = "tool:run"
[tools.input_schema]
type = "object"

[shortcut]
key = "Ctrl+Shift+S"
tool = ""
"#,
        )
        .unwrap_err();
        let ManifestError::Validation(txt) = err else {
            panic!("期望 Validation，实际 {:?}", err);
        };
        assert!(txt.contains("shortcut.tool 不能为空"));
    }

    #[test]
    fn shortcut_tool_not_in_tools_rejected() {
        let err = load_from_str(
            r#"
[plugin]
id = "com.example.x"
name = "X"
version = "1.0.0"

[exec]
command = "python"

[[tools]]
name = "tool:run"
[tools.input_schema]
type = "object"

[shortcut]
key = "Ctrl+Shift+S"
tool = "tool:nonexistent"
"#,
        )
        .unwrap_err();
        let ManifestError::Validation(txt) = err else {
            panic!("期望 Validation，实际 {:?}", err);
        };
        assert!(txt.contains("未在本插件的 [[tools]] 中声明"));
    }

    // ── settings 表 ──────────────────────────────

    #[test]
    fn settings_parsed_correctly() {
        let m = load_from_str(
            r#"
[plugin]
id = "com.example.screenshot"
name = "Screenshot"
version = "0.1.0"

[exec]
command = "python"

[[tools]]
name = "screenshot:capture"
[tools.input_schema]
type = "object"

[[settings]]
key = "output_dir"
label = "截图保存目录"
type = "string"
default = "~/Pictures"

[[settings]]
key = "format"
label = "图片格式"
type = "select"
default = "png"
options = ["png", "jpg"]

[[settings]]
key = "include_cursor"
label = "包含鼠标指针"
type = "boolean"
default = false
"#,
        )
        .unwrap();
        assert_eq!(m.settings.len(), 3);
        assert_eq!(m.settings[0].key, "output_dir");
        assert_eq!(m.settings[0].field_type, SettingType::String);
        assert_eq!(m.settings[1].field_type, SettingType::Select);
        assert_eq!(m.settings[1].options, vec!["png", "jpg"]);
        assert_eq!(m.settings[2].field_type, SettingType::Boolean);
    }

    #[test]
    fn settings_empty_by_default() {
        let m = load_from_str(
            r#"
[plugin]
id = "com.example.x"
name = "X"
version = "1.0.0"

[exec]
command = "python"
"#,
        )
        .unwrap();
        assert!(m.settings.is_empty());
    }

    #[test]
    fn settings_empty_key_rejected() {
        let err = load_from_str(
            r#"
[plugin]
id = "com.example.x"
name = "X"
version = "1.0.0"

[exec]
command = "python"

[[settings]]
key = "  "
label = "L"
type = "string"
"#,
        )
        .unwrap_err();
        let ManifestError::Validation(txt) = err else {
            panic!("期望 Validation，实际 {:?}", err);
        };
        assert!(txt.contains("settings[0].key 不能为空"));
    }

    #[test]
    fn settings_duplicate_key_rejected() {
        let err = load_from_str(
            r#"
[plugin]
id = "com.example.x"
name = "X"
version = "1.0.0"

[exec]
command = "python"

[[settings]]
key = "dir"
label = "A"
type = "string"

[[settings]]
key = "dir"
label = "B"
type = "string"
"#,
        )
        .unwrap_err();
        let ManifestError::Validation(txt) = err else {
            panic!("期望 Validation，实际 {:?}", err);
        };
        assert!(txt.contains("重复"));
    }

    #[test]
    fn settings_select_without_options_rejected() {
        let err = load_from_str(
            r#"
[plugin]
id = "com.example.x"
name = "X"
version = "1.0.0"

[exec]
command = "python"

[[settings]]
key = "fmt"
label = "格式"
type = "select"
default = "png"
"#,
        )
        .unwrap_err();
        let ManifestError::Validation(txt) = err else {
            panic!("期望 Validation，实际 {:?}", err);
        };
        assert!(txt.contains("options 为空"));
    }

    #[test]
    fn settings_empty_label_rejected() {
        let err = load_from_str(
            r#"
[plugin]
id = "com.example.x"
name = "X"
version = "1.0.0"

[exec]
command = "python"

[[settings]]
key = "dir"
label = "  "
type = "string"
"#,
        )
        .unwrap_err();
        let ManifestError::Validation(txt) = err else {
            panic!("期望 Validation，实际 {:?}", err);
        };
        assert!(txt.contains("label 不能为空"));
    }

    // ── docs 表 ──────────────────────────────────────

    #[test]
    fn docs_parsed_correctly() {
        let m = load_from_str(
            r#"
[plugin]
id = "com.example.x"
name = "X"
version = "0.1.0"

[exec]
command = "python"

[docs]
usage_file = "HELP.md"
"#,
        )
        .unwrap();
        assert_eq!(m.docs.unwrap().usage_file, "HELP.md");
    }

    #[test]
    fn docs_absent_by_default() {
        let m = load_from_str(
            r#"
[plugin]
id = "com.example.x"
name = "X"
version = "1.0.0"

[exec]
command = "python"
"#,
        )
        .unwrap();
        assert!(m.docs.is_none());
    }

    #[test]
    fn docs_empty_usage_file_rejected() {
        let err = load_from_str(
            r#"
[plugin]
id = "com.example.x"
name = "X"
version = "0.1.0"

[exec]
command = "python"

[docs]
usage_file = "  "
"#,
        )
        .unwrap_err();
        let ManifestError::Validation(txt) = err else {
            panic!("期望 Validation，实际 {:?}", err);
        };
        assert!(txt.contains("usage_file 不能为空"));
    }

    #[test]
    fn docs_usage_file_with_path_separator_rejected() {
        let err = load_from_str(
            r#"
[plugin]
id = "com.example.x"
name = "X"
version = "0.1.0"

[exec]
command = "python"

[docs]
usage_file = "subdir/README.md"
"#,
        )
        .unwrap_err();
        let ManifestError::Validation(txt) = err else {
            panic!("期望 Validation，实际 {:?}", err);
        };
        assert!(txt.contains("纯文件名"));
    }

    #[test]
    fn docs_usage_file_is_dot_rejected() {
        let err = load_from_str(
            r#"
[plugin]
id = "com.example.x"
name = "X"
version = "0.1.0"

[exec]
command = "python"

[docs]
usage_file = "."
"#,
        )
        .unwrap_err();
        let ManifestError::Validation(txt) = err else {
            panic!("期望 Validation，实际 {:?}", err);
        };
        assert!(txt.contains("纯文件名"));
    }

    #[test]
    fn docs_usage_file_contains_dotdot_rejected() {
        let err = load_from_str(
            r#"
[plugin]
id = "com.example.x"
name = "X"
version = "0.1.0"

[exec]
command = "python"

[docs]
usage_file = ".."
"#,
        )
        .unwrap_err();
        let ManifestError::Validation(txt) = err else {
            panic!("期望 Validation，实际 {:?}", err);
        };
        assert!(txt.contains("纯文件名"));
    }
}
