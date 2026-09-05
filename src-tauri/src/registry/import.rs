//! 插件包导入：把用户挑选的 `.zip` 解压进插件根目录。
//!
//! 与 [`seed`] 的关系：两者都是「往插件根目录里放一个插件目录」，也都用
//! 「暂存目录 + 整体改名」落盘，但触发条件与失败语义完全不同——播种是启动时的
//! 一次性静默行为，导入是用户主动发起、必须给出明确成败反馈的操作。
//!
//! # 为什么必须在落盘前检查 plugin.id
//!
//! [`Registry::insert_loaded`] 对重复 `plugin.id` 的处理是**整体拒绝后加载者**，
//! 只在冲突列表里留一条记录。如果导入时不查重，用户会看到「导入成功」，重启后
//! 插件却不见踪影——文件明明躺在磁盘上，界面上什么都没有。这是最难自查的失败
//! 形态，所以查重放在写盘之前，宁可让用户先去卸载旧版本。
//!
//! # 生效时机
//!
//! 落盘即完成，但 `Supervisor` 持有的是 `Registry` 的值而非共享引用，运行期没有
//! 重扫入口，因此新插件要重启后才出现。调用方有义务把这一点告诉用户。
//!
//! [`seed`]: crate::registry::seed
//! [`Registry::insert_loaded`]: crate::registry::Registry::insert_loaded

use std::fs;
use std::io;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use thiserror::Error;

use crate::registry::discovery::{self, LoadError};

/// 解压规模上限。
///
/// 做成结构体而不是直写常量，是为了让单测能用「2 个条目 / 64 字节」这种小数字去
/// 撞边界。否则验证 200 MB 上限就得真的造一个 200 MB 的压缩包，慢且没多验证到
/// 什么——真正需要验证的是判定逻辑，不是那个具体数字。
#[derive(Debug, Clone, Copy)]
struct Limits {
    /// 解压后允许的未压缩总字节数。
    ///
    /// 防 zip bomb：一个几十 KB 的压缩包可以膨胀成几十 GB。插件是「几个脚本 +
    /// 少量资源」，200 MB 已经宽裕到不会挡住任何正常插件，同时把炸弹拦在磁盘
    /// 写满之前。
    max_total_bytes: u64,
    /// 允许的条目数上限。
    ///
    /// 单独限制条目数是因为字节数管不住「几十万个空文件」这种形态——它总大小
    /// 为 0，却能把创建文件的系统调用拖到几分钟。
    max_entries: usize,
}

impl Limits {
    const DEFAULT: Self = Self {
        max_total_bytes: 200 * 1024 * 1024,
        max_entries: 5_000,
    };
}

/// 暂存目录名前缀。
///
/// 前导点没有隐藏效果（[`discovery::scan_plugins_root`] 并不跳过点目录），留着是
/// 为了在文件管理器里一眼看出「这不是插件」。真正避免它被当成插件的手段是
/// [`sweep_stale_staging`]：每次导入前清掉所有残留。
const STAGING_PREFIX: &str = ".import-staging-";

/// 导入成功的结果，用于组织给用户看的提示语。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportOutcome {
    pub plugin_id: String,
    pub plugin_name: String,
    /// 最终落地的插件目录（可能带 `-2` 之类的去重后缀）。
    pub installed_dir: PathBuf,
}

#[derive(Debug, Error)]
pub enum ImportError {
    #[error("这不是一个有效的 zip 文件")]
    Corrupt(#[source] zip::result::ZipError),

    // 上限值随错误一起带出来，而不是写死在文案里：文案写死就得跟常量两头对齐，
    // 改了一处忘另一处就会骗用户。
    #[error("插件包解压后体积超出上限（{} MiB），已中止", max_bytes / 1024 / 1024)]
    TooLarge { max_bytes: u64 },

    #[error("插件包内条目过多（上限 {max_entries} 个），已中止")]
    TooManyEntries { max_entries: usize },

    #[error("插件包内含有不安全的路径：{entry}")]
    UnsafePath { entry: String },

    #[error("插件包里找不到 manifest.toml（它应当在压缩包根目录，或根目录下唯一的一层子目录里）")]
    ManifestMissing,

    #[error("插件包的 manifest.toml 无效：{0}")]
    ManifestInvalid(String),

    #[error("插件 `{id}`（{name}）已安装，请先卸载后再导入。")]
    IdConflict { id: String, name: String },

    #[error("IO 错误（{op}）在 {}：{source}", path.display())]
    Io {
        path: PathBuf,
        op: &'static str,
        #[source]
        source: io::Error,
    },
}

fn io_err<'a>(path: &'a Path, op: &'static str) -> impl FnOnce(io::Error) -> ImportError + 'a {
    move |source| ImportError::Io {
        path: path.to_path_buf(),
        source,
        op,
    }
}

/// 把 zip 字节导入 `plugins_root`。
///
/// `existing_ids` 由调用方从注册表取——本模块不碰 Tauri 状态，好让整条解压与校验
/// 链路可以纯函数式地单测。
pub fn import_from_bytes(
    plugins_root: &Path,
    zip_bytes: &[u8],
    existing_ids: &[String],
) -> Result<ImportOutcome, ImportError> {
    import_with_limits(plugins_root, zip_bytes, existing_ids, Limits::DEFAULT)
}

/// [`import_from_bytes`] 的内部形态，上限可注入。
///
/// 单独留这一层只为单测：把 200 MiB 换成几十字节就能验证超限分支，不必真的造一个
/// 巨大的压缩包。
fn import_with_limits(
    plugins_root: &Path,
    zip_bytes: &[u8],
    existing_ids: &[String],
    limits: Limits,
) -> Result<ImportOutcome, ImportError> {
    fs::create_dir_all(plugins_root).map_err(io_err(plugins_root, "create_dir_all"))?;
    sweep_stale_staging(plugins_root);

    let staging = plugins_root.join(format!(
        "{STAGING_PREFIX}{}",
        uuid::Uuid::new_v4().simple()
    ));

    let outcome = import_into_staging(plugins_root, &staging, zip_bytes, existing_ids, limits);

    // 成功路径也要清：单层包装目录的情形下被改名走的是 staging 的子目录，
    // staging 自己会剩一个空壳。失败路径则要保证不留半个插件目录。
    let _ = fs::remove_dir_all(&staging);

    outcome
}

fn import_into_staging(
    plugins_root: &Path,
    staging: &Path,
    zip_bytes: &[u8],
    existing_ids: &[String],
    limits: Limits,
) -> Result<ImportOutcome, ImportError> {
    fs::create_dir_all(staging).map_err(io_err(staging, "create_dir_all"))?;
    extract_all(zip_bytes, staging, limits)?;

    let source_dir = locate_plugin_dir(staging)?;
    let loaded = discovery::load_plugin_dir(&source_dir).map_err(|e| match e {
        LoadError::MissingManifest { .. } => ImportError::ManifestMissing,
        LoadError::Manifest { source, .. } => ImportError::ManifestInvalid(source.to_string()),
        LoadError::Io { path, source } => ImportError::Io {
            path,
            op: "read manifest",
            source,
        },
    })?;

    let plugin_id = loaded.manifest.plugin.id.clone();
    let plugin_name = loaded.manifest.plugin.name.clone();

    if existing_ids.iter().any(|id| id == &plugin_id) {
        return Err(ImportError::IdConflict {
            id: plugin_id,
            name: plugin_name,
        });
    }

    let target = allocate_target_dir(plugins_root, &plugin_id);
    fs::rename(&source_dir, &target).map_err(io_err(&target, "rename"))?;

    Ok(ImportOutcome {
        plugin_id,
        plugin_name,
        installed_dir: target,
    })
}

/// 清理上一次导入崩溃时留下的暂存目录。
///
/// 不这么做的话，残留目录会被 [`discovery::scan_plugins_root`] 当成插件目录扫到，
/// 在插件列表里变成一条「缺少 manifest.toml」的幽灵错误，而用户无从知道它是什么。
/// 失败一律忽略：清不掉（比如被杀毒软件占用）不该阻断本次导入。
fn sweep_stale_staging(plugins_root: &Path) {
    let Ok(entries) = fs::read_dir(plugins_root) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if name.starts_with(STAGING_PREFIX) {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}

/// 逐条解压到 `dest`，同时施加条目数、体积与路径安全限制。
///
/// 不用 `ZipArchive::extract` 是为了能把「超限」和「路径不安全」分成两种可诉说的
/// 错误，而不是笼统地报一句解压失败。
fn extract_all(zip_bytes: &[u8], dest: &Path, limits: Limits) -> Result<(), ImportError> {
    let cursor = io::Cursor::new(zip_bytes);
    let mut archive = zip::ZipArchive::new(cursor).map_err(ImportError::Corrupt)?;

    if archive.len() > limits.max_entries {
        return Err(ImportError::TooManyEntries {
            max_entries: limits.max_entries,
        });
    }

    // 声明的未压缩大小来自压缩包头部，是攻击者可以随口写的数字。先按它做一次
    // 便宜的拒绝（不写任何磁盘），真正的约束靠下面的 `budget` 按实际写入量兜底。
    let declared: u64 = (0..archive.len())
        .filter_map(|i| archive.by_index(i).ok().map(|f| f.size()))
        .sum();
    if declared > limits.max_total_bytes {
        return Err(ImportError::TooLarge {
            max_bytes: limits.max_total_bytes,
        });
    }

    let mut budget = limits.max_total_bytes;

    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(ImportError::Corrupt)?;
        let raw_name = entry.name().to_string();

        // 符号链接指向的位置不受 zip 条目路径检查约束，落盘后可以把后续写入
        // 引到插件目录之外。插件不需要它，直接拒绝，不做「跳过」——静默跳过会
        // 让用户拿到一个残缺却看似成功的插件。
        if entry.is_symlink() {
            return Err(ImportError::UnsafePath { entry: raw_name });
        }

        // `enclosed_name` 挡掉绝对路径、NUL 字节和逃出根目录的 `..`。
        let rel = entry
            .enclosed_name()
            .ok_or_else(|| ImportError::UnsafePath {
                entry: raw_name.clone(),
            })?;
        if rel.components().any(|c| !matches!(c, Component::Normal(_))) {
            return Err(ImportError::UnsafePath { entry: raw_name });
        }

        let out = dest.join(&rel);

        if entry.is_dir() {
            fs::create_dir_all(&out).map_err(io_err(&out, "create_dir_all"))?;
            continue;
        }

        if let Some(parent) = out.parent() {
            fs::create_dir_all(parent).map_err(io_err(parent, "create_dir_all"))?;
        }

        let mut file = fs::File::create(&out).map_err(io_err(&out, "create"))?;
        // 多读 1 字节：拷满预算说明还有后续内容，即声明大小撒了谎。
        let written = io::copy(&mut entry.by_ref().take(budget + 1), &mut file)
            .map_err(io_err(&out, "copy"))?;
        if written > budget {
            return Err(ImportError::TooLarge {
                max_bytes: limits.max_total_bytes,
            });
        }
        budget -= written;
    }

    Ok(())
}

/// 找出 manifest.toml 真正所在的目录。
///
/// 容忍两种布局：压缩包根目录直接是插件目录，或者根目录下只有一层包装目录。
/// 后者是 Windows 资源管理器「压缩到 ZIP 文件」的默认产物——用户右键一个插件
/// 文件夹得到的就是包装形态，不接受它等于把最常见的打包方式判为错误。
///
/// 更深的嵌套不猜：那通常意味着用户压错了对象（比如整个 `plugins` 目录）。
fn locate_plugin_dir(staging: &Path) -> Result<PathBuf, ImportError> {
    if staging.join("manifest.toml").is_file() {
        return Ok(staging.to_path_buf());
    }

    let mut dirs = Vec::new();
    for entry in fs::read_dir(staging).map_err(io_err(staging, "read_dir"))? {
        let entry = entry.map_err(io_err(staging, "read_dir"))?;
        if entry.path().is_dir() {
            dirs.push(entry.path());
        }
    }

    if let [only] = dirs.as_slice() {
        if only.join("manifest.toml").is_file() {
            return Ok(only.clone());
        }
    }

    Err(ImportError::ManifestMissing)
}

/// 为插件挑一个未被占用的目录名。
///
/// 名字取 `plugin.id` 的末段（`com.intools.filesearch` → `filesearch`）：目录名与
/// 插件身份无关（见 [`discovery`] 模块说明），取末段纯粹是为了让用户在文件管理器
/// 里认得出来。重名时退到 `-2`、`-3`。
fn allocate_target_dir(plugins_root: &Path, plugin_id: &str) -> PathBuf {
    let base = dir_name_for(plugin_id);

    let first = plugins_root.join(&base);
    if !first.exists() {
        return first;
    }
    for n in 2u32.. {
        let candidate = plugins_root.join(format!("{base}-{n}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    unreachable!("2.. 是无界区间")
}

/// Windows 设备名，作为文件名会被内核直接拒绝。
const WIN_RESERVED: &[&str] = &[
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

fn dir_name_for(plugin_id: &str) -> String {
    let last = plugin_id.rsplit('.').next().unwrap_or(plugin_id);

    // manifest 校验只保证 id 各段是 `[A-Za-z0-9_-]`，没排除 `com.foo.nul` 这种末段
    // 撞上设备名的写法。退回完整 id：它带点号，一定不是设备名。
    if last.is_empty() || WIN_RESERVED.contains(&last.to_ascii_lowercase().as_str()) {
        return plugin_id.to_string();
    }
    last.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use zip::write::SimpleFileOptions;

    /// zip 条目：目录条目靠名字以 `/` 结尾表达，与真实压缩包一致。
    enum Entry<'a> {
        File(&'a str, &'a str),
        Symlink(&'a str, &'a str),
    }

    /// 在内存里拼一个 zip。
    ///
    /// 不用磁盘上的样本文件：那样一来「这个包里到底有什么」就得跳到另一个文件去看，
    /// 而每个用例关心的恰恰正是条目布局本身。
    fn make_zip(entries: &[Entry<'_>]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(io::Cursor::new(Vec::new()));
        let opts = SimpleFileOptions::default();
        for entry in entries {
            match entry {
                Entry::File(name, body) => {
                    writer.start_file(*name, opts).unwrap();
                    io::Write::write_all(&mut writer, body.as_bytes()).unwrap();
                }
                Entry::Symlink(name, target) => {
                    writer.add_symlink(*name, *target, opts).unwrap();
                }
            }
        }
        writer.finish().unwrap().into_inner()
    }

    fn manifest_toml(id: &str, name: &str) -> String {
        format!(
            r#"
[plugin]
id = "{id}"
name = "{name}"
version = "1.0.0"

[exec]
command = "python"
"#
        )
    }

    /// 插件根目录下的可见条目名，用于断言「没留下残渣」。
    fn entries_of(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    fn plugins_root(tmp: &tempfile::TempDir) -> PathBuf {
        tmp.path().join("plugins")
    }

    // ── 正常布局 ───────────────────────────────────

    #[test]
    fn imports_flat_package() {
        let tmp = tempdir().unwrap();
        let root = plugins_root(&tmp);
        let manifest = manifest_toml("com.example.hello", "问候");
        let zip = make_zip(&[
            Entry::File("manifest.toml", &manifest),
            Entry::File("main.py", "print('hi')"),
        ]);

        let out = import_from_bytes(&root, &zip, &[]).unwrap();

        assert_eq!(out.plugin_id, "com.example.hello");
        assert_eq!(out.plugin_name, "问候");
        // 目录名取 id 末段，而非压缩包里的任何名字。
        assert_eq!(out.installed_dir, root.join("hello"));
        assert!(out.installed_dir.join("manifest.toml").is_file());
        assert!(out.installed_dir.join("main.py").is_file());
        // 暂存目录必须清干净，否则会在插件列表里冒出一条幽灵错误。
        assert_eq!(entries_of(&root), ["hello"]);
    }

    #[test]
    fn imports_package_with_single_wrapper_dir() {
        let tmp = tempdir().unwrap();
        let root = plugins_root(&tmp);
        let manifest = manifest_toml("com.example.hello", "问候");
        // Windows 资源管理器「压缩到 ZIP 文件」产出的就是这种带一层目录的形态。
        let zip = make_zip(&[
            Entry::File("hello-plugin/manifest.toml", &manifest),
            Entry::File("hello-plugin/main.py", "print('hi')"),
        ]);

        let out = import_from_bytes(&root, &zip, &[]).unwrap();

        assert_eq!(out.plugin_id, "com.example.hello");
        assert_eq!(out.installed_dir, root.join("hello"));
        assert!(out.installed_dir.join("main.py").is_file());
        assert_eq!(entries_of(&root), ["hello"]);
    }

    /// PowerShell 的 `Compress-Archive` 把条目名写成 `hello-plugin\manifest.toml`。
    /// 这违反 ZIP 规范（APPNOTE 要求用正斜杠），但它是 Windows 上开箱即用的打包
    /// 命令，用户拿它打的包必须能导入。`enclosed_name` 用 Windows 路径语义解析，
    /// 反斜杠同样算分隔符，所以这里能通——写成测试是为了锁住这个依赖，
    /// 免得将来换成按正斜杠自行切分时静默退化。
    #[test]
    fn backslash_separators_are_accepted() {
        let tmp = tempdir().unwrap();
        let root = plugins_root(&tmp);
        let manifest = manifest_toml("com.example.hello", "问候");
        let zip = make_zip(&[
            Entry::File(r"hello-plugin\manifest.toml", &manifest),
            Entry::File(r"hello-plugin\main.py", "print('hi')"),
        ]);

        let out = import_from_bytes(&root, &zip, &[]).unwrap();

        assert_eq!(out.installed_dir, root.join("hello"));
        assert!(out.installed_dir.join("main.py").is_file());
        assert_eq!(entries_of(&root), ["hello"]);
    }

    #[test]
    fn nested_too_deep_is_manifest_missing() {
        let tmp = tempdir().unwrap();
        let root = plugins_root(&tmp);
        let manifest = manifest_toml("com.example.hello", "问候");
        // 用户压错了对象（把 `plugins` 整个目录压进去）——不猜，直接报找不到。
        let zip = make_zip(&[Entry::File("plugins/hello/manifest.toml", &manifest)]);

        let err = import_from_bytes(&root, &zip, &[]).unwrap_err();

        assert!(matches!(err, ImportError::ManifestMissing));
        assert!(entries_of(&root).is_empty());
    }

    #[test]
    fn two_sibling_dirs_is_manifest_missing() {
        let tmp = tempdir().unwrap();
        let root = plugins_root(&tmp);
        let manifest = manifest_toml("com.example.hello", "问候");
        // 有两个候选就不存在「唯一的包装目录」，不能替用户挑一个。
        let zip = make_zip(&[
            Entry::File("a/manifest.toml", &manifest),
            Entry::File("b/manifest.toml", &manifest),
        ]);

        let err = import_from_bytes(&root, &zip, &[]).unwrap_err();

        assert!(matches!(err, ImportError::ManifestMissing));
    }

    // ── manifest 问题 ──────────────────────────────

    #[test]
    fn missing_manifest_is_reported() {
        let tmp = tempdir().unwrap();
        let root = plugins_root(&tmp);
        let zip = make_zip(&[Entry::File("main.py", "print('hi')")]);

        let err = import_from_bytes(&root, &zip, &[]).unwrap_err();

        assert!(matches!(err, ImportError::ManifestMissing));
        assert!(entries_of(&root).is_empty());
    }

    #[test]
    fn invalid_manifest_carries_reason() {
        let tmp = tempdir().unwrap();
        let root = plugins_root(&tmp);
        // id 不是反向域名（少于三段），manifest 校验会拒绝。
        let zip = make_zip(&[Entry::File(
            "manifest.toml",
            &manifest_toml("hello", "问候"),
        )]);

        let err = import_from_bytes(&root, &zip, &[]).unwrap_err();

        // 具体措辞由 manifest 校验决定，这里只要求原因被透传出来而非吞掉。
        let ImportError::ManifestInvalid(reason) = &err else {
            panic!("期望 ManifestInvalid，实际 {err:?}");
        };
        assert!(!reason.is_empty());
        assert!(entries_of(&root).is_empty());
    }

    // ── id 冲突 ────────────────────────────────────

    #[test]
    fn id_conflict_rejects_and_leaves_nothing() {
        let tmp = tempdir().unwrap();
        let root = plugins_root(&tmp);
        let manifest = manifest_toml("com.example.hello", "问候");
        let zip = make_zip(&[Entry::File("manifest.toml", &manifest)]);

        let existing = vec!["com.example.hello".to_string()];
        let err = import_from_bytes(&root, &zip, &existing).unwrap_err();

        match &err {
            ImportError::IdConflict { id, name } => {
                assert_eq!(id, "com.example.hello");
                assert_eq!(name, "问候");
            }
            other => panic!("期望 IdConflict，实际 {other:?}"),
        }
        // 关键：拒绝之后磁盘上不能留下任何东西，否则下次导入会撞上残留。
        assert!(entries_of(&root).is_empty());
    }

    #[test]
    fn occupied_dir_name_gets_suffix() {
        let tmp = tempdir().unwrap();
        let root = plugins_root(&tmp);
        fs::create_dir_all(root.join("hello")).unwrap();

        let manifest = manifest_toml("com.example.hello", "问候");
        let zip = make_zip(&[Entry::File("manifest.toml", &manifest)]);

        // 目录名撞车但 id 不冲突：换个名字装下去，不该失败。
        let out = import_from_bytes(&root, &zip, &[]).unwrap();

        assert_eq!(out.installed_dir, root.join("hello-2"));
        assert_eq!(entries_of(&root), ["hello", "hello-2"]);
    }

    // ── 恶意与畸形输入 ─────────────────────────────

    #[test]
    fn non_zip_bytes_are_corrupt() {
        let tmp = tempdir().unwrap();
        let root = plugins_root(&tmp);

        // 把 .txt 改名成 .zip 是很常见的误操作。
        let err = import_from_bytes(&root, b"this is definitely not a zip", &[]).unwrap_err();

        assert!(matches!(err, ImportError::Corrupt(_)));
        assert!(entries_of(&root).is_empty());
    }

    #[test]
    fn parent_dir_traversal_is_rejected() {
        let tmp = tempdir().unwrap();
        let root = plugins_root(&tmp);
        let manifest = manifest_toml("com.example.hello", "问候");
        let zip = make_zip(&[
            Entry::File("manifest.toml", &manifest),
            Entry::File("../escaped.txt", "pwned"),
        ]);

        let err = import_from_bytes(&root, &zip, &[]).unwrap_err();

        match &err {
            ImportError::UnsafePath { entry } => assert!(entry.contains("escaped.txt")),
            other => panic!("期望 UnsafePath，实际 {other:?}"),
        }
        // 逃逸目标落在插件根目录之外，所以这里额外确认它没被写出去。
        assert!(!tmp.path().join("escaped.txt").exists());
        assert!(entries_of(&root).is_empty());
    }

    #[test]
    fn inner_parent_dir_that_stays_inside_is_allowed() {
        let tmp = tempdir().unwrap();
        let root = plugins_root(&tmp);
        let manifest = manifest_toml("com.example.hello", "问候");
        // `a/../manifest.toml` 归一化后仍在根内，不该误伤。
        let zip = make_zip(&[Entry::File("a/../manifest.toml", &manifest)]);

        let out = import_from_bytes(&root, &zip, &[]).unwrap();

        assert_eq!(out.installed_dir, root.join("hello"));
    }

    #[test]
    fn symlink_entry_is_rejected() {
        let tmp = tempdir().unwrap();
        let root = plugins_root(&tmp);
        let manifest = manifest_toml("com.example.hello", "问候");
        let zip = make_zip(&[
            Entry::File("manifest.toml", &manifest),
            Entry::Symlink("link", "../../secrets"),
        ]);

        let err = import_from_bytes(&root, &zip, &[]).unwrap_err();

        match &err {
            ImportError::UnsafePath { entry } => assert_eq!(entry, "link"),
            other => panic!("期望 UnsafePath，实际 {other:?}"),
        }
        assert!(entries_of(&root).is_empty());
    }

    #[test]
    fn oversize_package_is_rejected() {
        let tmp = tempdir().unwrap();
        let root = plugins_root(&tmp);
        let manifest = manifest_toml("com.example.hello", "问候");
        let zip = make_zip(&[
            Entry::File("manifest.toml", &manifest),
            Entry::File("big.bin", &"x".repeat(4096)),
        ]);

        let limits = Limits {
            max_total_bytes: 64,
            max_entries: 100,
        };
        let err = import_with_limits(&root, &zip, &[], limits).unwrap_err();

        match &err {
            ImportError::TooLarge { max_bytes } => assert_eq!(*max_bytes, 64),
            other => panic!("期望 TooLarge，实际 {other:?}"),
        }
        assert!(entries_of(&root).is_empty());
    }

    #[test]
    fn too_many_entries_is_rejected() {
        let tmp = tempdir().unwrap();
        let root = plugins_root(&tmp);
        // 条目数上限单独存在，是因为体积上限管不住「一大堆空文件」。
        let zip = make_zip(&[
            Entry::File("a.txt", ""),
            Entry::File("b.txt", ""),
            Entry::File("c.txt", ""),
        ]);

        let limits = Limits {
            max_total_bytes: 1024 * 1024,
            max_entries: 2,
        };
        let err = import_with_limits(&root, &zip, &[], limits).unwrap_err();

        match &err {
            ImportError::TooManyEntries { max_entries } => assert_eq!(*max_entries, 2),
            other => panic!("期望 TooManyEntries，实际 {other:?}"),
        }
    }

    // ── 残留清理 ───────────────────────────────────

    #[test]
    fn stale_staging_dir_is_swept() {
        let tmp = tempdir().unwrap();
        let root = plugins_root(&tmp);
        // 模拟上一次导入中途崩溃留下的暂存目录。
        let stale = root.join(format!("{STAGING_PREFIX}deadbeef"));
        fs::create_dir_all(&stale).unwrap();
        fs::write(stale.join("junk.txt"), b"junk").unwrap();

        let manifest = manifest_toml("com.example.hello", "问候");
        let zip = make_zip(&[Entry::File("manifest.toml", &manifest)]);
        import_from_bytes(&root, &zip, &[]).unwrap();

        // 残留必须消失：否则它会被扫成一条「缺少 manifest.toml」的幽灵插件。
        assert_eq!(entries_of(&root), ["hello"]);
    }

    // ── 目录名推导 ─────────────────────────────────

    #[test]
    fn dir_name_uses_last_segment() {
        assert_eq!(dir_name_for("com.intools.filesearch"), "filesearch");
    }

    #[test]
    fn dir_name_avoids_windows_device_names() {
        // 末段撞上设备名时退回完整 id：`nul` 作为目录名会被内核直接拒绝。
        assert_eq!(dir_name_for("com.example.nul"), "com.example.nul");
        assert_eq!(dir_name_for("com.example.COM1"), "com.example.COM1");
        // 普通名字不受影响。
        assert_eq!(dir_name_for("com.example.console"), "console");
    }
}
