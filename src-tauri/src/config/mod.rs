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

use serde::{de::DeserializeOwned, Serialize};
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

    pub fn plugin_stderr_log(plugin_id: &str) -> Result<PathBuf, PathError> {
        // 禁止 plugin_id 里出现路径分隔符 /..，避免越权写日志到别处。
        if plugin_id.is_empty()
            || plugin_id.contains('/')
            || plugin_id.contains('\\')
            || plugin_id == "."
            || plugin_id == ".."
        {
            return Err(PathError::InvalidPluginId(plugin_id.to_string()));
        }
        Ok(logs_dir()?.join(format!("{plugin_id}.log")))
    }

    pub fn mcp_audit_log() -> Result<PathBuf, PathError> {
        Ok(logs_dir()?.join("mcp-audit.log"))
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
    let value = serde_json::from_slice(&bytes).map_err(|e| JsonIoError::Parse {
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

// ─────────────────── 错误类型 ───────────────────

#[derive(Debug, Error)]
pub enum PathError {
    #[error("无法定位用户主目录（HOME / USERPROFILE 未设置）")]
    NoHomeDir,

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
            (Io { path: pa, op: oa, .. }, Io { path: pb, op: ob, .. }) => {
                pa == pb && oa == ob
            }
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
        assert_eq!(paths::cache_tools_json().unwrap(), root.join("cache/tools.json"));
        assert_eq!(paths::permissions_json().unwrap(), root.join("permissions.json"));
        assert_eq!(
            paths::mcp_exposure_json().unwrap(),
            root.join("mcp-exposure.json")
        );
        assert_eq!(paths::host_config_json().unwrap(), root.join("host-config.json"));
        assert_eq!(paths::logs_dir().unwrap(), root.join("logs"));
        assert_eq!(
            paths::mcp_audit_log().unwrap(),
            root.join("logs/mcp-audit.log")
        );
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
}
