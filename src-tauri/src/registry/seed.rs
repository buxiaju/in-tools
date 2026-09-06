//! 内置插件播种：把随安装包分发的插件拷进用户插件目录。
//!
//! 背景：`~/.intools/plugins` 在全新机器上并不存在，而 [`discovery::scan_plugins_root`]
//! 对「目录不存在」是静默返回空列表的，于是打包安装后的第一次启动只会显示
//! 「插件目录为空」——宿主没坏，但看起来什么都没有。
//!
//! 播种规则刻意收得很紧：**只在插件根目录整体不存在时播种一次**。
//! `commands::uninstall_plugin` 是物理 `remove_dir_all`，如果这里改成「逐个补齐缺失的」，
//! 用户卸载掉的内置插件会在下次启动时复活，卸载就成了假动作。
//!
//! 代价是后续版本新增的内置插件不会自动出现在老用户机器上。这是有意的取舍：
//! 宁可少给，不可违背用户已经表达过的删除意图。
//!
//! [`discovery::scan_plugins_root`]: crate::registry::discovery::scan_plugins_root

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use thiserror::Error;

/// 一次播种尝试的结果。调用方据此决定日志措辞——「跳过」是常态，不该报警。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SeedOutcome {
    /// 已播种，携带拷贝成功的插件目录数量。
    Seeded { plugins: usize },
    /// 未播种，携带原因（用于日志，不是错误）。
    Skipped(SkipReason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// 插件根目录已存在——用户已经用过了，不再插手。
    TargetExists,
    /// 找不到随包分发的插件源目录（典型场景：源码运行且未经 tauri-build 拷贝资源）。
    NoBundledSource,
}

impl std::fmt::Display for SkipReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TargetExists => write!(f, "插件目录已存在"),
            Self::NoBundledSource => write!(f, "未找到随包内置插件"),
        }
    }
}

#[derive(Debug, Error)]
pub enum SeedError {
    #[error("无法定位当前可执行文件所在目录：{0}")]
    NoExeDir(#[source] io::Error),

    #[error("IO 错误（{op}）在 {}：{source}", path.display())]
    Io {
        path: PathBuf,
        op: &'static str,
        #[source]
        source: io::Error,
    },
}

fn io_err<'a>(path: &'a Path, op: &'static str) -> impl FnOnce(io::Error) -> SeedError + 'a {
    move |source| SeedError::Io {
        path: path.to_path_buf(),
        source,
        op,
    }
}

/// 随安装包分发的插件目录。
///
/// Windows 上 Tauri 的 `resource_dir()` 等价于「可执行文件所在目录」，而播种发生在
/// `tauri::Builder` 之前、还没有 `AppHandle` 可用，所以这里直接从 `current_exe`
/// 推导，避免为了拿一个路径把整段装配顺序倒过来。
///
/// 开发期同样成立：`tauri-build` 无条件把 `bundle.resources` 拷进 `target/<profile>/`。
pub fn bundled_plugins_dir() -> Result<PathBuf, SeedError> {
    let exe = std::env::current_exe().map_err(SeedError::NoExeDir)?;
    let dir = exe
        .parent()
        .ok_or_else(|| SeedError::NoExeDir(io::Error::other("可执行文件路径没有父目录")))?;
    Ok(dir.join("plugins"))
}

/// 生产入口：从随包插件目录播种到 `target_root/system/`。
///
/// 系统插件与用户插件隔离：`target_root/system/` 存放随安装包分发的插件，
/// `target_root/user/` 存放用户自行安装的插件。
pub fn seed_builtin_plugins(target_root: &Path) -> Result<SeedOutcome, SeedError> {
    let system_dir = target_root.join("system");
    let result = seed_from(&bundled_plugins_dir()?, &system_dir);
    // 播种成功或已存在时，确保 user 目录也存在（供用户安装插件用）。
    if result.is_ok() {
        let user_dir = target_root.join("user");
        if !user_dir.exists() {
            let _ = fs::create_dir_all(&user_dir);
        }
    }
    result
}

/// 播种实现本体，源目录作为参数传入。
///
/// 拆出来是为了测试：单测不能依赖真实的可执行文件布局，但拷贝与「已存在则跳过」
/// 的判定必须与生产路径共用同一份实现。
pub fn seed_from(source: &Path, target_root: &Path) -> Result<SeedOutcome, SeedError> {
    if target_root.exists() {
        return Ok(SeedOutcome::Skipped(SkipReason::TargetExists));
    }
    if !source.is_dir() {
        return Ok(SeedOutcome::Skipped(SkipReason::NoBundledSource));
    }

    // 先拷进同级暂存目录再整体改名：中途失败（磁盘满、被杀进程）时 `target_root`
    // 依然不存在，下次启动会重来一遍。直接往 `target_root` 里拷则会留下半个目录，
    // 而「已存在就跳过」的规则会让这个残缺状态永久固化。
    let parent = target_root.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(parent).map_err(io_err(parent, "create_dir_all"))?;

    let staging = staging_dir(target_root);
    if staging.exists() {
        fs::remove_dir_all(&staging).map_err(io_err(&staging, "remove_dir_all"))?;
    }

    let plugins = match copy_plugin_dirs(source, &staging) {
        Ok(n) => n,
        Err(err) => {
            let _ = fs::remove_dir_all(&staging);
            return Err(err);
        }
    };

    match fs::rename(&staging, target_root) {
        Ok(()) => Ok(SeedOutcome::Seeded { plugins }),
        Err(err) => {
            let _ = fs::remove_dir_all(&staging);
            // 另一个实例抢先建好了目录：这不是故障，按「已存在」处理。
            if target_root.exists() {
                Ok(SeedOutcome::Skipped(SkipReason::TargetExists))
            } else {
                Err(SeedError::Io {
                    path: target_root.to_path_buf(),
                    source: err,
                    op: "rename",
                })
            }
        }
    }
}

fn staging_dir(target_root: &Path) -> PathBuf {
    let name = target_root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("plugins");
    let parent = target_root.parent().unwrap_or(Path::new("."));
    parent.join(format!(".{name}.seeding-{}", std::process::id()))
}

/// 把 `source` 下的每个子目录整体拷进 `dest`，返回拷贝的插件目录数。
///
/// 只认子目录：与 [`discovery::scan_plugins_root`] 的口径保持一致，源目录根部的
/// 散落文件（README 之类）不该被当成插件带过去。
///
/// [`discovery::scan_plugins_root`]: crate::registry::discovery::scan_plugins_root
fn copy_plugin_dirs(source: &Path, dest: &Path) -> Result<usize, SeedError> {
    fs::create_dir_all(dest).map_err(io_err(dest, "create_dir_all"))?;

    let mut count = 0usize;
    for entry in fs::read_dir(source).map_err(io_err(source, "read_dir"))? {
        let entry = entry.map_err(io_err(source, "read_dir"))?;
        let path = entry.path();
        let is_dir = entry
            .file_type()
            .map_err(io_err(&path, "file_type"))?
            .is_dir();
        if !is_dir {
            continue;
        }
        copy_dir_recursive(&path, &dest.join(entry.file_name()))?;
        count += 1;
    }
    Ok(count)
}

fn copy_dir_recursive(source: &Path, dest: &Path) -> Result<(), SeedError> {
    fs::create_dir_all(dest).map_err(io_err(dest, "create_dir_all"))?;
    for entry in fs::read_dir(source).map_err(io_err(source, "read_dir"))? {
        let entry = entry.map_err(io_err(source, "read_dir"))?;
        let from = entry.path();
        let to = dest.join(entry.file_name());
        let file_type = entry.file_type().map_err(io_err(&from, "file_type"))?;
        if file_type.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else {
            // 符号链接按 `fs::copy` 的默认行为跟随并复制内容——插件目录里出现
            // 符号链接本身就非预期，复制内容比原样搬运链接更不容易出意外。
            fs::copy(&from, &to).map_err(io_err(&from, "copy"))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// 造一个「随包插件目录」：两个合法插件 + 一个根部散落文件。
    fn make_source(root: &Path) {
        let alpha = root.join("alpha");
        fs::create_dir_all(alpha.join("sub")).unwrap();
        fs::write(alpha.join("manifest.toml"), "id = \"alpha\"").unwrap();
        fs::write(alpha.join("main.py"), "print('alpha')").unwrap();
        fs::write(alpha.join("sub").join("helper.py"), "# helper").unwrap();

        let beta = root.join("beta");
        fs::create_dir_all(&beta).unwrap();
        fs::write(beta.join("manifest.toml"), "id = \"beta\"").unwrap();

        fs::write(root.join("README.md"), "not a plugin").unwrap();
    }

    #[test]
    fn bundled_plugins_dir_is_sibling_of_exe() {
        // 只断言路径形状，不断言存在：`cargo test` 下 current_exe() 位于
        // target/<profile>/deps/，而资源被拷到 target/<profile>/，两者不同级。
        // 存在性由打包后的实机验证覆盖。
        let dir = bundled_plugins_dir().unwrap();
        let exe = std::env::current_exe().unwrap();
        assert_eq!(dir, exe.parent().unwrap().join("plugins"));
    }

    #[test]
    fn seeds_when_target_missing() {
        let tmp = TempDir::new().unwrap();
        let source = tmp.path().join("bundled");
        fs::create_dir_all(&source).unwrap();
        make_source(&source);
        let target = tmp.path().join("home").join("plugins");

        let outcome = seed_from(&source, &target).unwrap();

        assert_eq!(outcome, SeedOutcome::Seeded { plugins: 2 });
        assert!(target.join("alpha").join("manifest.toml").is_file());
        assert!(target.join("alpha").join("sub").join("helper.py").is_file());
        assert!(target.join("beta").join("manifest.toml").is_file());
    }

    #[test]
    fn skips_root_level_files() {
        let tmp = TempDir::new().unwrap();
        let source = tmp.path().join("bundled");
        fs::create_dir_all(&source).unwrap();
        make_source(&source);
        let target = tmp.path().join("plugins");

        seed_from(&source, &target).unwrap();

        assert!(!target.join("README.md").exists());
    }

    #[test]
    fn skips_when_target_exists_even_if_empty() {
        // 关键回归：用户把内置插件全卸载后目录会剩个空壳，此时绝不能重新播种。
        let tmp = TempDir::new().unwrap();
        let source = tmp.path().join("bundled");
        fs::create_dir_all(&source).unwrap();
        make_source(&source);
        let target = tmp.path().join("plugins");
        fs::create_dir_all(&target).unwrap();

        let outcome = seed_from(&source, &target).unwrap();

        assert_eq!(outcome, SeedOutcome::Skipped(SkipReason::TargetExists));
        assert!(!target.join("alpha").exists());
    }

    #[test]
    fn skips_when_source_missing() {
        let tmp = TempDir::new().unwrap();
        let target = tmp.path().join("plugins");

        let outcome = seed_from(&tmp.path().join("nope"), &target).unwrap();

        assert_eq!(outcome, SeedOutcome::Skipped(SkipReason::NoBundledSource));
        assert!(!target.exists());
    }

    #[test]
    fn creates_missing_parent_dirs() {
        let tmp = TempDir::new().unwrap();
        let source = tmp.path().join("bundled");
        fs::create_dir_all(&source).unwrap();
        make_source(&source);
        let target = tmp.path().join("a").join("b").join("plugins");

        seed_from(&source, &target).unwrap();

        assert!(target.join("alpha").is_dir());
    }

    #[test]
    fn leaves_no_staging_dir_behind() {
        let tmp = TempDir::new().unwrap();
        let source = tmp.path().join("bundled");
        fs::create_dir_all(&source).unwrap();
        make_source(&source);
        let target = tmp.path().join("plugins");

        seed_from(&source, &target).unwrap();

        let leftovers: Vec<_> = fs::read_dir(tmp.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains("seeding"))
            .collect();
        assert!(leftovers.is_empty(), "残留暂存目录：{leftovers:?}");
    }

    #[test]
    fn seeded_layout_is_scannable() {
        // 播种的产物必须能被真正的扫描器认出来，否则拷贝得再对也没用。
        let tmp = TempDir::new().unwrap();
        let source = tmp.path().join("bundled");
        let plugin = source.join("demo");
        fs::create_dir_all(&plugin).unwrap();
        fs::write(
            plugin.join("manifest.toml"),
            r#"
[plugin]
id = "com.intools.demo"
name = "Demo"
version = "0.1.0"

[exec]
command = "python"
args = ["-u", "main.py"]

[[tools]]
name = "demo:ping"
description = "ping"
[tools.input_schema]
type = "object"
"#,
        )
        .unwrap();
        fs::write(plugin.join("main.py"), "").unwrap();
        let target = tmp.path().join("plugins");

        seed_from(&source, &target).unwrap();

        let entries = crate::registry::discovery::scan_plugins_root(&target).unwrap();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].is_ok(), "扫描失败：{:?}", entries[0]);
    }
}
