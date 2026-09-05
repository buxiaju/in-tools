//! 插件目录扫描与发现。
//!
//! 规则（对齐实施计划 Phase 2）：
//! - 插件根目录（默认 `~/.intools/plugins`）下每一个**子目录**视为一个插件候选；
//!   根目录里的普通文件/隐藏文件直接跳过。
//! - 每个子目录必须存在 `manifest.toml`：缺失记为 `LoadOutcome::MissingManifest`。
//! - manifest 解析失败记为 `LoadOutcome::ManifestError`。
//! - 单个插件出错不影响其他插件加载；最终通过 `scan_plugins_root` 统一返回一个包含
//!   每个目录加载结果的向量，调用方（Registry）据此构建表。
//! - 插件目录名与 `plugin.id` 不要求一致——id 只以 manifest 中的反向域名字段为准。
//!   （允许目录名是人可读短名，避免迁移时必须改文件夹。）

#![allow(dead_code)]

use crate::protocol::manifest::{load_from_str, Manifest, ManifestError};
use std::{
    fs, io,
    path::{Path, PathBuf},
};
use thiserror::Error;

/// 单个插件目录的加载结果。
#[derive(Debug, Clone)]
pub struct LoadedPlugin {
    /// 插件目录在文件系统中的绝对（或扫描时提供的）路径。
    pub plugin_dir: PathBuf,
    /// 解析/校验后的 manifest。
    pub manifest: Manifest,
}

/// 单个目录加载失败的原因。
#[derive(Debug, Error)]
pub enum LoadError {
    #[error("缺少 manifest.toml（目录 {}）", .path.display())]
    MissingManifest { path: PathBuf },

    #[error("读取 manifest 失败（{}）：{source}", .path.display())]
    Manifest {
        path: PathBuf,
        #[source]
        source: ManifestError,
    },

    #[error("IO 错误（{}）：{source}", .path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// 一次扫描返回的聚合条目。
pub type ScanEntry = Result<LoadedPlugin, LoadError>;

/// 扫描插件根目录；目录不存在时返回空向量（方便用户手动把目录建好之前先启动宿主看
/// 空列表，而不是启动就报错）。
pub fn scan_plugins_root(root: &Path) -> Result<Vec<ScanEntry>, LoadError> {
    if !root.exists() {
        return Ok(vec![]);
    }
    let read_dir = fs::read_dir(root).map_err(|e| LoadError::Io {
        path: root.to_path_buf(),
        source: e,
    })?;

    let mut results: Vec<ScanEntry> = Vec::new();
    for entry in read_dir {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                // 单个条目读失败只记录错误，不中止其余。
                results.push(Err(LoadError::Io {
                    path: root.to_path_buf(),
                    source: e,
                }));
                continue;
            }
        };
        let path = entry.path();
        let meta = match entry.file_type() {
            Ok(ft) => ft.is_dir(),
            Err(e) => {
                results.push(Err(LoadError::Io {
                    path: path.clone(),
                    source: e,
                }));
                continue;
            }
        };
        if !meta {
            continue; // 根目录里的普通文件跳过。
        }
        results.push(load_plugin_dir(&path));
    }
    // 让结果在调用方看来更可预测：按插件目录名的字典序排序。
    // 错误条目的稳定顺序通过 Err 先于 Ok，再按路径比较。
    results.sort_by(|a, b| {
        use std::cmp::Ordering;
        let pa = match a {
            Ok(ok) => &ok.plugin_dir,
            Err(e) => e_path(e),
        };
        let pb = match b {
            Ok(ok) => &ok.plugin_dir,
            Err(e) => e_path(e),
        };
        match (a.is_err(), b.is_err()) {
            (true, false) => Ordering::Less,
            (false, true) => Ordering::Greater,
            _ => pa.cmp(pb),
        }
    });
    Ok(results)
}

fn e_path(e: &LoadError) -> &Path {
    match e {
        LoadError::MissingManifest { path } => path,
        LoadError::Manifest { path, .. } => path,
        LoadError::Io { path, .. } => path,
    }
}

/// 从一个具体目录加载插件 manifest（公开以便 registry 在"热更新单个插件"时直接用）。
pub fn load_plugin_dir(plugin_dir: &Path) -> ScanEntry {
    let manifest_path = plugin_dir.join("manifest.toml");
    if !manifest_path.exists() {
        return Err(LoadError::MissingManifest {
            path: plugin_dir.to_path_buf(),
        });
    }
    let text = match fs::read_to_string(&manifest_path) {
        Ok(t) => t,
        Err(e) => {
            return Err(LoadError::Io {
                path: manifest_path,
                source: e,
            });
        }
    };
    let manifest = load_from_str(&text).map_err(|me| LoadError::Manifest {
        path: manifest_path.clone(),
        source: me,
    })?;
    Ok(LoadedPlugin {
        plugin_dir: plugin_dir.to_path_buf(),
        manifest,
    })
}

// ─────────────────── 单元测试 ───────────────────
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn write_manifest(dir: &Path, toml_text: &str) {
        fs::write(dir.join("manifest.toml"), toml_text.as_bytes()).unwrap();
    }

    fn sample_toml(id: &str) -> String {
        format!(
            r#"
[plugin]
id = "{id}"
name = "N-{id}"
version = "1.0.0"

[exec]
command = "python"
"#
        )
    }

    // ── 空目录 / 不存在 ───────────────────────────

    #[test]
    fn scan_missing_root_returns_empty() {
        let tmp = tempdir().unwrap();
        let nope = tmp.path().join("does-not-exist");
        let res = scan_plugins_root(&nope).unwrap();
        assert!(res.is_empty());
    }

    #[test]
    fn scan_empty_root_returns_empty() {
        let tmp = tempdir().unwrap();
        let res = scan_plugins_root(tmp.path()).unwrap();
        assert!(res.is_empty());
    }

    // ── 正常加载 ───────────────────────────────────

    #[test]
    fn scan_loads_two_plugins() {
        let tmp = tempdir().unwrap();
        let a = tmp.path().join("plugin-a");
        let b = tmp.path().join("plugin-b");
        fs::create_dir_all(&a).unwrap();
        fs::create_dir_all(&b).unwrap();
        write_manifest(&a, &sample_toml("com.example.a"));
        write_manifest(&b, &sample_toml("com.example.b"));

        let entries = scan_plugins_root(tmp.path()).unwrap();
        assert_eq!(entries.len(), 2);
        let oks: Vec<_> = entries.iter().filter_map(|e| e.as_ref().ok()).collect();
        assert_eq!(oks.len(), 2);
        let ids: Vec<_> = oks.iter().map(|o| o.manifest.plugin.id.as_str()).collect();
        // 字典序 plugin-a < plugin-b
        assert_eq!(ids, ["com.example.a", "com.example.b"]);
        assert!(oks[0].plugin_dir.ends_with("plugin-a"));
        assert!(oks[1].plugin_dir.ends_with("plugin-b"));
    }

    // ── 容错：单个坏插件不影响其他 ──────────────────

    #[test]
    fn scan_handles_missing_manifest_for_one_plugin() {
        let tmp = tempdir().unwrap();
        let good = tmp.path().join("good");
        let bad = tmp.path().join("bad");
        fs::create_dir_all(&good).unwrap();
        fs::create_dir_all(&bad).unwrap();
        write_manifest(&good, &sample_toml("com.example.good"));
        // bad 没有 manifest.toml

        let entries = scan_plugins_root(tmp.path()).unwrap();
        // 排序后 Err 在前、Ok 在后；bad < good，所以顺序是 bad(err) → good(ok)
        assert_eq!(entries.len(), 2);
        assert!(
            entries[0].is_err(),
            "期望第 0 项是错误，实际 {:?}",
            entries[0]
        );
        let err = entries[0].as_ref().unwrap_err();
        assert!(
            matches!(err, LoadError::MissingManifest { .. }),
            "err={:?}",
            err
        );
        assert_eq!(
            entries[1].as_ref().unwrap().manifest.plugin.id,
            "com.example.good"
        );
    }

    #[test]
    fn scan_handles_invalid_manifest_content() {
        let tmp = tempdir().unwrap();
        let good = tmp.path().join("good");
        let malformed = tmp.path().join("malformed");
        fs::create_dir_all(&good).unwrap();
        fs::create_dir_all(&malformed).unwrap();
        write_manifest(&good, &sample_toml("com.example.good"));
        // manifest.toml 存在但内容不是合法 manifest。
        write_manifest(&malformed, "not [a] valid = \n toml ;;;;");

        let entries = scan_plugins_root(tmp.path()).unwrap();
        assert_eq!(entries.len(), 2);
        let malformed_err = entries.iter().find_map(|e| e.as_ref().err()).unwrap();
        // 失败原因被归类为 Manifest。
        let LoadError::Manifest { path, .. } = malformed_err else {
            panic!("期望 Manifest 错误，实际 {:?}", malformed_err);
        };
        assert_eq!(*path, malformed.join("manifest.toml"));
        let good_entry = entries.iter().find_map(|e| e.as_ref().ok()).unwrap();
        assert_eq!(good_entry.manifest.plugin.id, "com.example.good");
    }

    #[test]
    fn scan_handles_manifest_validation_errors() {
        // plugin.id 只有两段，无法通过反向域名校验。
        let tmp = tempdir().unwrap();
        let short = tmp.path().join("short");
        fs::create_dir_all(&short).unwrap();
        write_manifest(
            &short,
            r#"
[plugin]
id = "a.b"
name = "X"
version = "1.0.0"

[exec]
command = "python"
"#,
        );
        let entries = scan_plugins_root(tmp.path()).unwrap();
        assert_eq!(entries.len(), 1);
        let LoadError::Manifest { source: me, .. } = entries[0].as_ref().unwrap_err() else {
            panic!("期望 Manifest Error，实际 {:?}", entries[0]);
        };
        let ManifestError::Validation(txt) = me else {
            panic!("期望 Validation，实际 {:?}", me);
        };
        assert!(txt.contains("反向域名"));
    }

    // ── 根目录中的普通文件被跳过 ───────────────────

    #[test]
    fn scan_ignores_files_at_root_level() {
        let tmp = tempdir().unwrap();
        let good = tmp.path().join("plugin-a");
        fs::create_dir_all(&good).unwrap();
        write_manifest(&good, &sample_toml("com.example.a"));
        fs::write(tmp.path().join("README.md"), b"readme").unwrap();
        fs::write(tmp.path().join("index.txt"), b"123").unwrap();

        let entries = scan_plugins_root(tmp.path()).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].as_ref().unwrap().manifest.plugin.id,
            "com.example.a"
        );
    }

    // ── 热加载单个目录 load_plugin_dir ────────────

    #[test]
    fn load_plugin_dir_ok_and_missing() {
        let tmp = tempdir().unwrap();
        let p = tmp.path().join("p");
        fs::create_dir_all(&p).unwrap();
        // 无 manifest → MissingManifest
        let err = load_plugin_dir(&p).unwrap_err();
        assert!(matches!(err, LoadError::MissingManifest { .. }));
        // 加上 manifest
        write_manifest(&p, &sample_toml("z.z.z"));
        let loaded = load_plugin_dir(&p).unwrap();
        assert_eq!(loaded.manifest.plugin.id, "z.z.z");
    }
}
