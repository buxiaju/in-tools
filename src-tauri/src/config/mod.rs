//! 配置层：`~/.intools/` 下的路径解析与 JSON 原子落盘。
//!
//! # 目录约定（Windows 下 `~` = `%USERPROFILE%`）
//!
//! ```text
//! ~/.intools/
//! ├── plugins/                 # 插件根目录（由 config::paths::plugins_root 提供）
//! ├── cache/
//! │   └── tools.json           # 运行时 tools/list 缓存（Phase 5）
//! ├── permissions.json         # 授权记录（Phase 5）
//! ├── mcp-exposure.json        # MCP 暴露白名单（Phase 5 定义、Phase 7 写、Phase 8 读）
//! ├── host-config.json         # 宿主全局配置（MCP 开关、日志级别、Token 等占位）
//! ├── shortcuts.json           # 用户对插件快捷键的覆盖（优先于 manifest 默认值）
//! ├── plugin-settings/
//! │   └── <plugin-id>.json     # 每插件用户设置，宿主按 manifest [[settings]] 渲染
//! ├── plugin-configs/
//! │   └── <plugin-id>.json     # 每插件配置，由 host/getConfig|setConfig 读写（Phase 6）
//! └── logs/
//!     ├── <plugin-id>.log      # 插件 stderr 转存（Phase 4）
//!     └── mcp-audit.log        # MCP 审计（Phase 8）
//! ```
//!
//! # 原子写入策略
//! 写入时先 `write <file>.tmp` → `fs::rename` 覆盖目标文件。
//! 这样进程崩溃在任何时刻都只留下两种结果：要么旧文件完整存在，
//! 要么新文件完整落盘；不会出现"写了一半 JSON 变成截断文本"的不可恢复损坏。
//! Windows 下 `std::fs::rename` 在同一卷上是原子且可覆盖目标（已在非独占时有效）。

// Phase 2 产物尚未被 runtime/supervisor 引用。
#![allow(dead_code)]

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value as JsonValue;
use std::{
    fs, io,
    path::{Path, PathBuf},
};
use thiserror::Error;

// ─────────────────── 路径 ───────────────────

/// 解析出宿主数据根目录，默认 `~/.intools`。失败时返回错误——因为后续所有模块
/// 都依赖这个目录，拿不到就无法启动。
pub fn data_root() -> Result<PathBuf, PathError> {
    let home = dirs::home_dir().ok_or(PathError::NoHomeDir)?;
    Ok(home.join(".intools"))
}

/// 所有常用路径的集中入口；保证与上面注释表保持同一种来源。
pub mod paths {
    use super::*;

    pub fn plugins_root() -> Result<PathBuf, PathError> {
        Ok(data_root()?.join("plugins"))
    }

    pub fn cache_dir() -> Result<PathBuf, PathError> {
        Ok(data_root()?.join("cache"))
    }

    pub fn cache_tools_json() -> Result<PathBuf, PathError> {
        Ok(cache_dir()?.join("tools.json"))
    }

    pub fn permissions_json() -> Result<PathBuf, PathError> {
        Ok(data_root()?.join("permissions.json"))
    }

    pub fn mcp_exposure_json() -> Result<PathBuf, PathError> {
        Ok(data_root()?.join("mcp-exposure.json"))
    }

    pub fn host_config_json() -> Result<PathBuf, PathError> {
        Ok(data_root()?.join("host-config.json"))
    }

    pub fn logs_dir() -> Result<PathBuf, PathError> {
        Ok(data_root()?.join("logs"))
    }

    /// 校验 plugin_id 可安全用作文件名。
    ///
    /// 禁止路径分隔符与 `.`/`..`，避免插件用 `../permissions` 之类的 id
    /// 越权覆写宿主自己的文件。凡是把 plugin_id 拼进路径的地方都必须先过这里。
    pub(super) fn check_plugin_id(plugin_id: &str) -> Result<(), PathError> {
        if plugin_id.is_empty()
            || plugin_id.contains('/')
            || plugin_id.contains('\\')
            || plugin_id == "."
            || plugin_id == ".."
        {
            return Err(PathError::InvalidPluginId(plugin_id.to_string()));
        }
        Ok(())
    }

    pub fn plugin_stderr_log(plugin_id: &str) -> Result<PathBuf, PathError> {
        check_plugin_id(plugin_id)?;
        Ok(logs_dir()?.join(format!("{plugin_id}.log")))
    }

    /// 每插件配置的存放目录。
    ///
    /// 与宿主自身的 `host-config.json` 分开：插件配置由插件经
    /// `host/getConfig` / `host/setConfig` 自行读写，宿主不解释其内容，
    /// 因此不能和宿主配置混在同一个文件里。
    pub fn plugin_configs_dir() -> Result<PathBuf, PathError> {
        Ok(data_root()?.join("plugin-configs"))
    }

    pub fn plugin_config_json(plugin_id: &str) -> Result<PathBuf, PathError> {
        plugin_config_json_in(&plugin_configs_dir()?, plugin_id)
    }

    /// 与 [`plugin_config_json`] 相同，但把存放目录作为参数传入。
    ///
    /// 存在的意义是测试：单测不能往真实的 `~/.intools` 里写文件，但消毒逻辑
    /// 必须与生产路径共用同一份实现，否则「测试里安全、生产上越权」。
    pub fn plugin_config_json_in(dir: &Path, plugin_id: &str) -> Result<PathBuf, PathError> {
        check_plugin_id(plugin_id)?;
        Ok(dir.join(format!("{plugin_id}.json")))
    }

    pub fn mcp_audit_log() -> Result<PathBuf, PathError> {
        Ok(logs_dir()?.join("mcp-audit.log"))
    }

    /// 随安装包分发的文档目录。
    ///
    /// `tauri.conf.json` 的 `bundle.resources` 把仓库 `docs/` 整体拷到
    /// 可执行文件旁的 `docs/`，安装/开发期同一路径——开发期 `tauri-build` 已经
    /// 无条件把它拷进 `target/<profile>/docs`，所以读不到再退一步找 exe_dir/docs。
    pub fn bundled_docs_dir() -> Result<PathBuf, PathError> {
        let exe = std::env::current_exe()
            .map_err(|err| PathError::NoExeDir(err.to_string()))?;
        Ok(exe
            .parent()
            .ok_or_else(|| {
                PathError::NoExeDir("可执行文件路径没有父目录，无法定位 docs/".to_string())
            })?
            .join("docs"))
    }

    pub fn bundled_doc(name: &str) -> Result<PathBuf, PathError> {
        // 防止路径穿越——只允许平面文件名，不允许 `..` 或目录跳转。
        if name.is_empty()
            || name.contains('/')
            || name.contains('\\')
            || name.contains("..")
        {
            return Err(PathError::InvalidPluginId(format!(
                "非法的文档路径 `{name}`"
            )));
        }
        Ok(bundled_docs_dir()?.join(name))
    }

    /// 每插件用户设置的存放目录。
    ///
    /// 与 `plugin-configs/` 不同：后者是插件自己经协议读写的配置，
    /// 这里是宿主管理的、面向用户设置页的参数值。
    pub fn plugin_settings_dir() -> Result<PathBuf, PathError> {
        Ok(data_root()?.join("plugin-settings"))
    }

    pub fn plugin_settings_json(plugin_id: &str) -> Result<PathBuf, PathError> {
        check_plugin_id(plugin_id)?;
        Ok(plugin_settings_dir()?.join(format!("{plugin_id}.json")))
    }

    /// 用户对插件快捷键的覆盖表。
    ///
    /// 单独一个文件而不是塞进 `plugin-settings/<id>.json`：快捷键是**跨插件**的
    /// 全局资源，注册时必须一次性看到全部绑定才能判重。分散在每插件文件里，
    /// 就得先遍历目录再合并，多一层无谓的易错逻辑。
    pub fn shortcuts_json() -> Result<PathBuf, PathError> {
        Ok(data_root()?.join("shortcuts.json"))
    }
}

/// 确保目录存在；不存在则创建（含父目录）。
pub fn ensure_dir(path: &Path) -> Result<(), PathError> {
    if !path.exists() {
        fs::create_dir_all(path).map_err(|e| PathError::Io {
            path: path.to_path_buf(),
            source: e,
            op: "create_dir_all",
        })?;
    }
    Ok(())
}

// ─────────────────── JSON 读写 ───────────────────

/// 读 JSON 文件。文件不存在时返回 `Ok(None)`，方便调用方做"默认值"处理。
pub fn read_json<T: DeserializeOwned>(path: &Path) -> Result<Option<T>, JsonIoError> {
    if !path.exists() {
        return Ok(None);
    }
    let bytes = fs::read(path).map_err(|e| JsonIoError::Io {
        path: path.to_path_buf(),
        source: e,
        op: "read",
    })?;
    // 空文件常见于「前一次 rename 前 crash 留下 0 字节 tmp 没清」这类情况。
    if bytes.is_empty() {
        return Ok(None);
    }
    // Windows 上记事本、PowerShell 的 `Set-Content -Encoding UTF8` 都会写 UTF-8 BOM，
    // 而 serde_json 会把它当成非法字符直接报错。用户手改一次配置就整体解析失败，
    // 这是目标平台上的常见操作，不该让它变成故障。
    let bytes = bytes
        .strip_prefix(&[0xEF, 0xBB, 0xBF])
        .unwrap_or(bytes.as_slice());
    let value = serde_json::from_slice(bytes).map_err(|e| JsonIoError::Parse {
        path: path.to_path_buf(),
        source: e,
    })?;
    Ok(Some(value))
}

/// 原子写 JSON：写到同目录下的 `.tmp` 再 rename 覆盖。
///
/// - 写目录不存在时自动创建父目录
/// - JSON 序列化失败不产生任何文件
/// - rename 失败会留下 `.tmp` 文件，不污染原文件
pub fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), JsonIoError> {
    if let Some(parent) = path.parent() {
        ensure_dir(parent).map_err(JsonIoError::from_path)?;
    }
    let tmp = match path.file_name() {
        Some(name) => {
            let mut tmp_name = name.to_os_string();
            tmp_name.push(".tmp");
            path.with_file_name(tmp_name)
        }
        None => {
            return Err(JsonIoError::InvalidPath {
                path: path.to_path_buf(),
                reason: "目标路径无文件名",
            });
        }
    };

    let bytes = serde_json::to_vec_pretty(value).map_err(|e| JsonIoError::Serialize {
        path: path.to_path_buf(),
        source: e,
    })?;
    fs::write(&tmp, bytes).map_err(|e| JsonIoError::Io {
        path: tmp.clone(),
        source: e,
        op: "write(tmp)",
    })?;
    fs::rename(&tmp, path).map_err(|e| JsonIoError::Io {
        path: path.to_path_buf(),
        source: e,
        op: "rename tmp over target",
    })?;
    Ok(())
}

// ─────────────────── 宿主全局配置 ───────────────────

/// 日志级别，对应 `tracing` 的五档。设置页的下拉框直接映射到这里。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Error,
    Warn,
    #[default]
    Info,
    Debug,
    Trace,
}

impl LogLevel {
    /// 转成 `EnvFilter` 认得的字符串。
    pub fn as_filter_str(self) -> &'static str {
        match self {
            LogLevel::Error => "error",
            LogLevel::Warn => "warn",
            LogLevel::Info => "info",
            LogLevel::Debug => "debug",
            LogLevel::Trace => "trace",
        }
    }
}

/// `~/.intools/host-config.json` 的内容：宿主全局配置。
///
/// 所有字段都带 `#[serde(default)]`，这样旧版本写下的、缺字段的配置文件
/// 仍能读起来——否则加一个新设置项就会让用户的既有配置整体解析失败。
/// 关闭按钮的行为。`MinimizeToTray` 是默认——宿主是「常驻工具」定位，
/// 误点关闭不该杀掉后台插件。
///
/// 注意区分两种「退出」：
/// - 关闭按钮 = 用户误点的概率高，默认隐藏到托盘；
/// - 托盘菜单的「退出」= 用户主动操作，照常走 `app.exit(0)`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloseBehavior {
    /// 隐藏到托盘，进程不退出。
    #[default]
    MinimizeToTray,
    /// 真正退出宿主与全部插件子进程。
    Exit,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct HostConfig {
    /// 插件根目录。`None` 表示用默认的 `~/.intools/plugins`。
    pub plugins_dir: Option<PathBuf>,
    /// MCP 网关总开关。默认关——设计要求「默认零暴露」。
    pub mcp_enabled: bool,
    /// MCP 的 Bearer token，首次开启时生成。
    pub mcp_token: Option<String>,
    /// 日志级别。
    pub log_level: LogLevel,
    /// 被用户禁用的插件 id 列表。禁用的插件不会被 `start_eager_plugins`
    /// 自动拉起、不会被 `ensure_running` 按需唤醒、其工具不会出现在
    /// `host/listTools` 与 MCP `tools/list` 中。
    pub disabled_plugins: Vec<String>,
    /// 主窗口关闭按钮的行为。默认 `minimize_to_tray`，与「宿主是常驻后台工具」
    /// 的产品定位一致。
    pub close_behavior: CloseBehavior,
    /// 插件市场总开关。默认关。
    pub marketplace_enabled: bool,
    /// 插件市场 URL。默认使用 GitHub Pages。
    pub marketplace_url: String,
}

impl HostConfig {
    /// 从 `~/.intools/host-config.json` 读取。文件不存在时得到默认配置。
    pub fn load() -> Result<Self, JsonIoError> {
        let path = paths::host_config_json()?;
        Ok(read_json(&path)?.unwrap_or_default())
    }

    /// 从指定路径读取。测试用，避免碰到真实家目录。
    pub fn load_at(path: &Path) -> Result<Self, JsonIoError> {
        Ok(read_json(path)?.unwrap_or_default())
    }

    pub fn save(&self) -> Result<(), JsonIoError> {
        let path = paths::host_config_json()?;
        write_json(&path, self)
    }

    pub fn save_at(&self, path: &Path) -> Result<(), JsonIoError> {
        write_json(path, self)
    }

    /// 解析出实际生效的插件根目录。
    pub fn effective_plugins_dir(&self) -> Result<PathBuf, PathError> {
        match &self.plugins_dir {
            Some(dir) => Ok(dir.clone()),
            None => paths::plugins_root(),
        }
    }

    /// 开启 MCP，并在尚无 token 时生成一个。返回当前生效的 token。
    ///
    /// token 只在这里生成：设计要求「首次开启时生成」，此后保持稳定，
    /// 否则每次开关都会让已配置好的 MCP 客户端失效。
    pub fn enable_mcp(&mut self) -> &str {
        self.mcp_enabled = true;
        self.mcp_token
            .get_or_insert_with(|| uuid::Uuid::new_v4().simple().to_string())
    }

    /// 关闭 MCP。保留 token，便于再次开启时客户端无需改配置。
    pub fn disable_mcp(&mut self) {
        self.mcp_enabled = false;
    }
}

// ─────────────────── MCP 暴露白名单 ───────────────────

/// `~/.intools/mcp-exposure.json` 的内容：`{"exposed": ["plugin.id:tool"]}`。
///
/// Phase 7 的权限页写，Phase 8 的网关读。默认零暴露。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct McpExposure {
    /// 已暴露的工具全名，形如 `plugin.id:tool`。
    pub exposed: Vec<String>,
}

impl McpExposure {
    pub fn load() -> Result<Self, JsonIoError> {
        let path = paths::mcp_exposure_json()?;
        Ok(read_json(&path)?.unwrap_or_default())
    }

    pub fn load_at(path: &Path) -> Result<Self, JsonIoError> {
        Ok(read_json(path)?.unwrap_or_default())
    }

    pub fn save(&self) -> Result<(), JsonIoError> {
        let path = paths::mcp_exposure_json()?;
        write_json(&path, self)
    }

    pub fn save_at(&self, path: &Path) -> Result<(), JsonIoError> {
        write_json(path, self)
    }

    pub fn is_exposed(&self, qualified_tool: &str) -> bool {
        self.exposed.iter().any(|t| t == qualified_tool)
    }

    /// 勾选/取消勾选一个工具。返回是否发生了变化。
    ///
    /// 内部保持排序去重：白名单会被人工审阅，稳定顺序让 diff 可读。
    pub fn set_exposed(&mut self, qualified_tool: &str, exposed: bool) -> bool {
        let present = self.is_exposed(qualified_tool);
        if present == exposed {
            return false;
        }
        if exposed {
            self.exposed.push(qualified_tool.to_string());
            self.exposed.sort();
            self.exposed.dedup();
        } else {
            self.exposed.retain(|t| t != qualified_tool);
        }
        true
    }
}

// ─────────────────── 每插件用户设置 ───────────────────

/// 加载插件的用户设置。文件不存在时返回空对象。
pub fn load_plugin_settings(plugin_id: &str) -> Result<serde_json::Map<String, JsonValue>, JsonIoError> {
    let path = paths::plugin_settings_json(plugin_id)?;
    match read_json::<serde_json::Map<String, JsonValue>>(&path)? {
        Some(map) => Ok(map),
        None => Ok(serde_json::Map::new()),
    }
}

/// 保存插件的用户设置（原子写入）。
pub fn save_plugin_settings(
    plugin_id: &str,
    settings: &serde_json::Map<String, JsonValue>,
) -> Result<(), JsonIoError> {
    let path = paths::plugin_settings_json(plugin_id)?;
    write_json(&path, settings)
}

// ─────────────────── 用户快捷键覆盖 ───────────────────

/// 单个插件的快捷键用户覆盖。
///
/// 与 manifest 的 `[shortcut]` 是「覆盖」而非「替换」关系：只覆盖 `key` 与
/// `enabled`，`tool` 与 `ui` 永远来自 manifest。理由是后两者属于插件的实现契约
/// （调哪个工具、要不要框选界面），用户改了只会调坏；能改的只有「按哪个键」
/// 和「要不要启用」。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserShortcut {
    /// 用户指定的按键串。空串表示「不指定，沿用 manifest 默认值」。
    #[serde(default)]
    pub key: String,
    /// 是否启用。`false` 时即使有按键也不注册。
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

impl Default for UserShortcut {
    fn default() -> Self {
        Self {
            key: String::new(),
            enabled: true,
        }
    }
}

/// `~/.intools/shortcuts.json` 的内容：`{"bindings": {"<plugin-id>": {...}}}`。
///
/// 用 `BTreeMap` 而非 `HashMap`：文件会被用户直接查看甚至手改，字典序输出
/// 让 diff 稳定可读。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct UserShortcuts {
    pub bindings: std::collections::BTreeMap<String, UserShortcut>,
}

impl UserShortcuts {
    pub fn load() -> Result<Self, JsonIoError> {
        let path = paths::shortcuts_json()?;
        Ok(read_json(&path)?.unwrap_or_default())
    }

    pub fn load_at(path: &Path) -> Result<Self, JsonIoError> {
        Ok(read_json(path)?.unwrap_or_default())
    }

    pub fn save(&self) -> Result<(), JsonIoError> {
        let path = paths::shortcuts_json()?;
        write_json(&path, self)
    }

    pub fn save_at(&self, path: &Path) -> Result<(), JsonIoError> {
        write_json(path, self)
    }

    pub fn get(&self, plugin_id: &str) -> Option<&UserShortcut> {
        self.bindings.get(plugin_id)
    }

    /// 写入某插件的覆盖。
    ///
    /// `key` 为空且 `enabled` 为真时直接删除条目，而不是存一条空覆盖：
    /// 「没有覆盖」和「覆盖成默认」在语义上是同一件事，让文件里不留垃圾条目。
    /// 这样用户点「恢复默认」后 shortcuts.json 会真正回到干净状态。
    pub fn set(&mut self, plugin_id: &str, key: &str, enabled: bool) {
        let key = key.trim();
        if key.is_empty() && enabled {
            self.bindings.remove(plugin_id);
            return;
        }
        self.bindings.insert(
            plugin_id.to_string(),
            UserShortcut {
                key: key.to_string(),
                enabled,
            },
        );
    }

    pub fn remove(&mut self, plugin_id: &str) -> bool {
        self.bindings.remove(plugin_id).is_some()
    }
}

// ─────────────────── 错误类型 ───────────────────

#[derive(Debug, Error)]
pub enum PathError {
    #[error("无法定位用户主目录（HOME / USERPROFILE 未设置）")]
    NoHomeDir,

    #[error("无法定位可执行文件目录：{0}")]
    NoExeDir(String),

    #[error("非法插件 ID：`{0}`（禁止包含路径分隔符）")]
    InvalidPluginId(String),

    #[error("IO 错误（{op}）在 {}：{source}", path.display())]
    Io {
        path: PathBuf,
        op: &'static str,
        #[source]
        source: io::Error,
    },
}

#[derive(Debug, Error)]
pub enum JsonIoError {
    #[error("路径无效：{}（{reason}）", path.display())]
    InvalidPath { path: PathBuf, reason: &'static str },

    #[error("IO 错误（{op}）在 {}：{source}", path.display())]
    Io {
        path: PathBuf,
        op: &'static str,
        #[source]
        source: io::Error,
    },

    #[error("解析 JSON 失败，文件 {}：{source}", path.display())]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },

    #[error("序列化 JSON 失败（目标 {path}）：{source}")]
    Serialize {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },

    #[error(transparent)]
    Path(#[from] PathError),
}

impl JsonIoError {
    fn from_path(e: PathError) -> Self {
        JsonIoError::Path(e)
    }
}

// 方便 registry 等后续模块做 variant 判别 + 测试断言。
impl PartialEq for PathError {
    fn eq(&self, other: &Self) -> bool {
        use PathError::*;
        match (self, other) {
            (NoHomeDir, NoHomeDir) => true,
            (InvalidPluginId(a), InvalidPluginId(b)) => a == b,
            (
                Io {
                    path: pa, op: oa, ..
                },
                Io {
                    path: pb, op: ob, ..
                },
            ) => pa == pb && oa == ob,
            _ => false,
        }
    }
}

// ─────────────────── 单元测试 ───────────────────
#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};
    use tempfile::tempdir;

    // ── 路径逻辑 ────────────────────────────────

    #[test]
    fn data_root_returns_intools_in_home() {
        let root = data_root().unwrap();
        let home = dirs::home_dir().unwrap();
        assert_eq!(root, home.join(".intools"));
    }

    #[test]
    fn paths_all_start_with_data_root() {
        let root = data_root().unwrap();
        assert_eq!(paths::plugins_root().unwrap(), root.join("plugins"));
        assert_eq!(
            paths::cache_tools_json().unwrap(),
            root.join("cache/tools.json")
        );
        assert_eq!(
            paths::permissions_json().unwrap(),
            root.join("permissions.json")
        );
        assert_eq!(
            paths::mcp_exposure_json().unwrap(),
            root.join("mcp-exposure.json")
        );
        assert_eq!(
            paths::host_config_json().unwrap(),
            root.join("host-config.json")
        );
        assert_eq!(
            paths::plugin_configs_dir().unwrap(),
            root.join("plugin-configs")
        );
        assert_eq!(
            paths::plugin_config_json("com.example.ocr").unwrap(),
            root.join("plugin-configs/com.example.ocr.json")
        );
        assert_eq!(paths::logs_dir().unwrap(), root.join("logs"));
        assert_eq!(
            paths::mcp_audit_log().unwrap(),
            root.join("logs/mcp-audit.log")
        );
        assert_eq!(
            paths::plugin_settings_dir().unwrap(),
            root.join("plugin-settings")
        );
        assert_eq!(
            paths::plugin_settings_json("com.example.ocr").unwrap(),
            root.join("plugin-settings/com.example.ocr.json")
        );
        assert_eq!(paths::shortcuts_json().unwrap(), root.join("shortcuts.json"));
    }

    #[test]
    fn plugin_stderr_log_sanitizes_plugin_id() {
        let good = paths::plugin_stderr_log("com.example.ocr").unwrap();
        let logs = paths::logs_dir().unwrap();
        assert_eq!(good, logs.join("com.example.ocr.log"));

        // 各种路径穿越 / 分隔符 都要被拒。
        assert!(paths::plugin_stderr_log("").is_err());
        assert!(paths::plugin_stderr_log(".").is_err());
        assert!(paths::plugin_stderr_log("..").is_err());
        assert!(paths::plugin_stderr_log("a/b").is_err());
        assert!(paths::plugin_stderr_log("a\\b").is_err());
    }

    #[test]
    fn plugin_config_json_sanitizes_plugin_id() {
        // 插件配置路径同样拼接 plugin_id，若不消毒，插件可用 `../permissions`
        // 之类的 id 让宿主把授权文件当成自己的配置覆写掉。
        assert!(paths::plugin_config_json("").is_err());
        assert!(paths::plugin_config_json(".").is_err());
        assert!(paths::plugin_config_json("..").is_err());
        assert!(paths::plugin_config_json("../permissions").is_err());
        assert!(paths::plugin_config_json("a\\b").is_err());
    }

    #[test]
    fn ensure_dir_creates_parents() {
        let tmp = tempdir().unwrap();
        let nested = tmp.path().join("a").join("b").join("c");
        assert!(!nested.exists());
        ensure_dir(&nested).unwrap();
        assert!(nested.is_dir());
        // 多次调用幂等。
        ensure_dir(&nested).unwrap();
    }

    // ── JSON 读写 & 原子 ─────────────────────────

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    struct SampleCfg {
        name: String,
        count: u32,
        tags: Vec<String>,
    }

    fn sample() -> SampleCfg {
        SampleCfg {
            name: "hello".to_string(),
            count: 42,
            tags: vec!["a".into(), "b".into()],
        }
    }

    #[test]
    fn json_write_then_read_round_trip() {
        let tmp = tempdir().unwrap();
        let file = tmp.path().join("cfg.json");
        let s = sample();
        write_json(&file, &s).unwrap();
        let got: SampleCfg = read_json(&file).unwrap().unwrap();
        assert_eq!(got, s);
    }

    #[test]
    fn json_missing_returns_none() {
        let tmp = tempdir().unwrap();
        let file = tmp.path().join("nope.json");
        let got: Option<SampleCfg> = read_json(&file).unwrap();
        assert!(got.is_none());
    }

    #[test]
    fn json_empty_file_returns_none() {
        let tmp = tempdir().unwrap();
        let file = tmp.path().join("empty.json");
        fs::write(&file, b"").unwrap();
        let got: Option<SampleCfg> = read_json(&file).unwrap();
        assert!(got.is_none());
    }

    #[test]
    fn json_tolerates_utf8_bom() {
        // Windows 上用记事本或 PowerShell 存一次配置就会带上 BOM。
        // 这不是损坏的文件，必须照常读出来。
        let tmp = tempdir().unwrap();
        let file = tmp.path().join("bom.json");
        let s = sample();
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice(&serde_json::to_vec(&s).unwrap());
        fs::write(&file, &bytes).unwrap();

        let got: SampleCfg = read_json(&file).unwrap().unwrap();
        assert_eq!(got, s);
    }

    #[test]
    fn json_corrupt_returns_parse_error() {
        let tmp = tempdir().unwrap();
        let file = tmp.path().join("bad.json");
        fs::write(&file, b"{ not json }").unwrap();
        let err = read_json::<SampleCfg>(&file).unwrap_err();
        let JsonIoError::Parse { .. } = err else {
            panic!("期望 Parse 错误，实际 {:?}", err);
        };
    }

    #[test]
    fn json_write_creates_parent_dirs() {
        let tmp = tempdir().unwrap();
        let file = tmp.path().join("nested").join("deep").join("cfg.json");
        let s = sample();
        write_json(&file, &s).unwrap();
        let got: SampleCfg = read_json(&file).unwrap().unwrap();
        assert_eq!(got, s);
    }

    #[test]
    fn json_write_atomic_does_not_trash_original_on_serialize_failure() {
        // 验证"先写 tmp 再 rename"流程：如果序列化失败，原文件不应被碰。
        // serde 序列化总是能成功的（所有可 Serialize 都 fail 不了），
        // 所以我们用"写到一个无法写入的目录"这条路径测不了……
        // 换一条等价路径：路径没有文件名时报错，且不会产生同名文件/目录。
        let tmp = tempdir().unwrap();
        let dir = tmp.path().join("no_filename_here");
        fs::create_dir_all(&dir).unwrap();
        let s = sample();
        let err = write_json(Path::new(""), &s).unwrap_err();
        let JsonIoError::InvalidPath { .. } = err else {
            panic!("期望 InvalidPath，实际 {:?}", err);
        };
    }

    #[test]
    fn write_json_is_atomic_newer_value_replaces_old() {
        // 简单等价验证：连续两次 write，第二次成功后读出来一定是新值。
        let tmp = tempdir().unwrap();
        let file = tmp.path().join("step.json");
        let mut a = sample();
        a.count = 1;
        let mut b = sample();
        b.count = 2;
        write_json(&file, &a).unwrap();
        write_json(&file, &b).unwrap();
        let got: SampleCfg = read_json(&file).unwrap().unwrap();
        assert_eq!(got.count, 2);
        // tmp 文件不会残留（rename 成功后被移走了，所以同目录没有 .tmp）
        let entries: Vec<_> = fs::read_dir(tmp.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert!(
            !entries.iter().any(|n| n.ends_with(".tmp")),
            "残留 tmp 文件：{entries:?}"
        );
    }

    // ── HostConfig ──────────────────────────────

    #[test]
    fn host_config_missing_file_yields_default() {
        let tmp = tempdir().unwrap();
        let file = tmp.path().join("host-config.json");
        let cfg = HostConfig::load_at(&file).unwrap();
        assert_eq!(cfg, HostConfig::default());
        // 默认必须是「MCP 关、无 token、Info 级别、插件目录跟随默认」。
        assert!(!cfg.mcp_enabled);
        assert!(cfg.mcp_token.is_none());
        assert_eq!(cfg.log_level, LogLevel::Info);
        assert!(cfg.plugins_dir.is_none());
    }

    #[test]
    fn host_config_round_trip() {
        let tmp = tempdir().unwrap();
        let file = tmp.path().join("host-config.json");
        let mut cfg = HostConfig {
            plugins_dir: Some(PathBuf::from("D:/my-plugins")),
            log_level: LogLevel::Debug,
            ..HostConfig::default()
        };
        cfg.enable_mcp();
        cfg.save_at(&file).unwrap();

        let got = HostConfig::load_at(&file).unwrap();
        assert_eq!(got, cfg);
    }

    #[test]
    fn host_config_tolerates_missing_fields() {
        // 老版本写下的配置文件只有一个字段；加了新设置项后仍必须读得起来，
        // 缺的字段落到默认值，而不是让整个配置解析失败。
        let tmp = tempdir().unwrap();
        let file = tmp.path().join("host-config.json");
        fs::write(&file, br#"{"mcp_enabled": true}"#).unwrap();
        let cfg = HostConfig::load_at(&file).unwrap();
        assert!(cfg.mcp_enabled);
        assert_eq!(cfg.log_level, LogLevel::Info);
        assert!(cfg.mcp_token.is_none());
    }

    #[test]
    fn enable_mcp_generates_token_once_and_keeps_it() {
        let mut cfg = HostConfig::default();
        let first = cfg.enable_mcp().to_string();
        assert!(!first.is_empty());

        // 重复开启不换 token。
        assert_eq!(cfg.enable_mcp(), first);

        // 关掉再开也不换——否则已配置好的 MCP 客户端会失效。
        cfg.disable_mcp();
        assert!(!cfg.mcp_enabled);
        assert_eq!(cfg.mcp_token.as_deref(), Some(first.as_str()));
        assert_eq!(cfg.enable_mcp(), first);
        assert!(cfg.mcp_enabled);
    }

    #[test]
    fn effective_plugins_dir_prefers_override() {
        let mut cfg = HostConfig::default();
        assert_eq!(
            cfg.effective_plugins_dir().unwrap(),
            paths::plugins_root().unwrap()
        );
        cfg.plugins_dir = Some(PathBuf::from("D:/elsewhere"));
        assert_eq!(
            cfg.effective_plugins_dir().unwrap(),
            PathBuf::from("D:/elsewhere")
        );
    }

    #[test]
    fn log_level_serializes_lowercase() {
        let json = serde_json::to_string(&LogLevel::Warn).unwrap();
        assert_eq!(json, "\"warn\"");
        let back: LogLevel = serde_json::from_str("\"trace\"").unwrap();
        assert_eq!(back, LogLevel::Trace);
        assert_eq!(LogLevel::Debug.as_filter_str(), "debug");
    }

    // ── McpExposure ─────────────────────────────

    #[test]
    fn mcp_exposure_defaults_to_zero_exposure() {
        let tmp = tempdir().unwrap();
        let file = tmp.path().join("mcp-exposure.json");
        let ex = McpExposure::load_at(&file).unwrap();
        assert!(ex.exposed.is_empty());
        assert!(!ex.is_exposed("com.example.ocr:recognize"));
    }

    #[test]
    fn set_exposed_toggles_and_reports_change() {
        let mut ex = McpExposure::default();
        assert!(ex.set_exposed("b.plugin:tool", true));
        // 已经是该状态时返回 false，调用方据此跳过落盘。
        assert!(!ex.set_exposed("b.plugin:tool", true));
        assert!(ex.is_exposed("b.plugin:tool"));

        assert!(ex.set_exposed("a.plugin:tool", true));
        // 内部保持排序，便于人工审阅白名单 diff。
        assert_eq!(ex.exposed, vec!["a.plugin:tool", "b.plugin:tool"]);

        assert!(ex.set_exposed("b.plugin:tool", false));
        assert!(!ex.set_exposed("b.plugin:tool", false));
        assert_eq!(ex.exposed, vec!["a.plugin:tool"]);
    }

    #[test]
    fn mcp_exposure_round_trip() {
        let tmp = tempdir().unwrap();
        let file = tmp.path().join("mcp-exposure.json");
        let mut ex = McpExposure::default();
        ex.set_exposed("com.example.ocr:recognize", true);
        ex.save_at(&file).unwrap();

        let got = McpExposure::load_at(&file).unwrap();
        assert_eq!(got, ex);
        assert!(got.is_exposed("com.example.ocr:recognize"));
    }

    // ── 用户快捷键覆盖 ────────────────────────────

    #[test]
    fn user_shortcuts_missing_file_yields_default() {
        let tmp = tempdir().unwrap();
        let file = tmp.path().join("shortcuts.json");
        let s = UserShortcuts::load_at(&file).unwrap();
        assert!(s.bindings.is_empty());
    }

    #[test]
    fn user_shortcuts_set_inserts_entry() {
        let mut s = UserShortcuts::default();
        s.set("com.intools.screenshot", "Ctrl+Alt+S", true);
        let entry = s.get("com.intools.screenshot").unwrap();
        assert_eq!(entry.key, "Ctrl+Alt+S");
        assert!(entry.enabled);
    }

    #[test]
    fn user_shortcuts_set_empty_key_and_enabled_removes_entry() {
        let mut s = UserShortcuts::default();
        s.set("com.intools.screenshot", "Ctrl+Alt+S", true);
        assert!(s.get("com.intools.screenshot").is_some());

        s.set("com.intools.screenshot", "", true);
        assert!(s.get("com.intools.screenshot").is_none());
    }

    #[test]
    fn user_shortcuts_set_empty_key_disabled_keeps_entry() {
        let mut s = UserShortcuts::default();
        s.set("com.intools.screenshot", "", false);
        let entry = s.get("com.intools.screenshot").unwrap();
        assert_eq!(entry.key, "");
        assert!(!entry.enabled);
    }

    #[test]
    fn user_shortcuts_remove_returns_whether_removed() {
        let mut s = UserShortcuts::default();
        assert!(!s.remove("com.intools.screenshot"));
        s.set("com.intools.screenshot", "Ctrl+Shift+S", true);
        assert!(s.remove("com.intools.screenshot"));
        assert!(s.get("com.intools.screenshot").is_none());
    }

    #[test]
    fn user_shortcuts_round_trip() {
        let tmp = tempdir().unwrap();
        let file = tmp.path().join("shortcuts.json");
        let mut s = UserShortcuts::default();
        s.set("com.intools.screenshot", "Ctrl+Shift+S", true);
        s.set("com.intools.hello", "", false);
        s.save_at(&file).unwrap();

        let got = UserShortcuts::load_at(&file).unwrap();
        let shot = got.get("com.intools.screenshot").unwrap();
        assert_eq!(shot.key, "Ctrl+Shift+S");
        assert!(shot.enabled);
        let hello = got.get("com.intools.hello").unwrap();
        assert_eq!(hello.key, "");
        assert!(!hello.enabled);
    }

    #[test]
    fn user_shortcuts_serialized_order_is_lexicographic() {
        let tmp = tempdir().unwrap();
        let file = tmp.path().join("shortcuts.json");
        let mut s = UserShortcuts::default();
        s.set("z.plugin", "Ctrl+Z", true);
        s.set("a.plugin", "Ctrl+A", true);
        s.save_at(&file).unwrap();

        let raw = std::fs::read_to_string(&file).unwrap();
        let a_pos = raw.find("a.plugin").unwrap();
        let z_pos = raw.find("z.plugin").unwrap();
        assert!(a_pos < z_pos, "a.plugin 应在 z.plugin 之前");
    }

    #[test]
    fn user_shortcuts_set_trims_key() {
        let mut s = UserShortcuts::default();
        s.set("test.plugin", "  Ctrl+Shift+T  ", true);
        assert_eq!(s.get("test.plugin").unwrap().key, "Ctrl+Shift+T");
    }
}
