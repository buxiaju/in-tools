//! 插件注册表：`plugin_id → Manifest` 正向映射，`工具名 → plugin_id` 反向索引，
//! 以及跨插件工具名冲突处理（"先到先得 + 后加载者该工具被拒绝并记录冲突原因"）。
//!
//! Registry 只处理 manifest 数据结构，不感知进程，是后续 `runtime`、`mcp`、`permission`
//! 做"工具名解析"的单一入口。

#![allow(dead_code)]

use crate::protocol::manifest::{Manifest, ToolDescriptor};
use crate::registry::discovery::{LoadError, LoadedPlugin, ScanEntry};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

pub mod discovery;
pub mod import;
pub mod seed;

/// 已成功加载、并且通过注册表"工具去重"规则的插件记录。
#[derive(Debug, Clone)]
pub struct RegisteredPlugin {
    pub plugin_dir: PathBuf,
    pub manifest: Manifest,
    /// manifest 静态声明的工具清单。跨插件工具名冲突时，某些工具可能已被移除，
    /// 所以这里不再是 manifest 原样，而是经过"冲突清洗后"的最终版本。
    pub tools: Vec<ToolDescriptor>,
}

impl RegisteredPlugin {
    pub fn id(&self) -> &str {
        &self.manifest.plugin.id
    }
}

/// 跨插件工具名冲突记录：后加载的 `tool_name` 被先加载的 `owner_plugin` 占据。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolConflict {
    pub tool_name: String,
    pub owner_plugin: String,
    pub rejected_from: String,
    pub reason: String,
}

/// 注册表。
#[derive(Debug, Default)]
pub struct Registry {
    /// 按 plugin_id 字典序组织已成功注册的插件。
    plugins: BTreeMap<String, RegisteredPlugin>,
    /// 工具名 → plugin_id 反向索引。MCP/AI 编排层 resolve_tool 都走这里。
    tool_owner: BTreeMap<String, String>,
    /// 加载过程中产生的冲突条目（不会因为加载完成就消失，方便 UI 在"插件详情"页面
    /// 展示"你的工具 foo:bar 没被暴露，因为另一个插件先声明了同名工具"）。
    conflicts: Vec<ToolConflict>,
    /// discovery 阶段未能加载的条目：缺失 manifest、校验失败、IO 错误 等。
    load_failures: Vec<LoadError>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    /// 从一次 discovery 扫描结果构建注册表。
    ///
    /// 规则：
    /// - 遇到 scan 中的 `Err` → 记入 `load_failures`。
    /// - 遇到重复的 plugin.id（理论上不会发生但防御性处理）→ 后加载者整体拒绝，
    ///   记录冲突。
    /// - 逐个工具比对：同名工具已被先加载插件占据 → 该工具被丢弃，记入 conflicts；
    ///   其余工具保留，写入反向索引。
    pub fn from_scan(entries: Vec<ScanEntry>) -> Self {
        let mut reg = Self::new();
        // 按 discovery 已经给出的稳定顺序依次插入，先到先得。
        for entry in entries {
            match entry {
                Ok(loaded) => reg.insert_loaded(loaded),
                Err(e) => reg.load_failures.push(e),
            }
        }
        reg
    }

    /// 从插件根目录一步构建：先扫描再构造。
    pub fn scan_and_build(root: &Path) -> Result<Self, LoadError> {
        let entries = discovery::scan_plugins_root(root)?;
        Ok(Self::from_scan(entries))
    }

    /// 新增单个已加载插件；主要用于 `from_scan`，也公开以便热更新单个目录后直接合入。
    pub fn insert_loaded(&mut self, loaded: LoadedPlugin) {
        let plugin_id = loaded.manifest.plugin.id.clone();

        // plugin.id 去重。
        if self.plugins.contains_key(&plugin_id) {
            self.conflicts.push(ToolConflict {
                tool_name: "*".to_string(),
                owner_plugin: plugin_id.clone(),
                rejected_from: plugin_id.clone(),
                reason: "重复的 plugin.id，后加载者整体被拒绝".to_string(),
            });
            return;
        }

        let LoadedPlugin {
            plugin_dir,
            manifest,
        } = loaded;

        let mut kept: Vec<ToolDescriptor> = Vec::with_capacity(manifest.tools.len());
        for tool in &manifest.tools {
            if let Some(owner) = self.tool_owner.get(&tool.name).cloned() {
                self.conflicts.push(ToolConflict {
                    tool_name: tool.name.clone(),
                    owner_plugin: owner,
                    rejected_from: plugin_id.clone(),
                    reason: "已被先加载的同工具名插件占用（先到先得）".to_string(),
                });
            } else {
                self.tool_owner.insert(tool.name.clone(), plugin_id.clone());
                kept.push(tool.clone());
            }
        }

        self.plugins.insert(
            plugin_id,
            RegisteredPlugin {
                plugin_dir,
                manifest,
                tools: kept,
            },
        );
    }

    // ── 查询接口（对应实施计划 §Phase 2）─────────

    pub fn get_plugin(&self, id: &str) -> Option<&RegisteredPlugin> {
        self.plugins.get(id)
    }

    /// 解析工具名到其归属插件；工具不存在时返回 `None`（后续调用层会把它包装成
    /// `CODE_TOOL_NOT_FOUND` JSON-RPC 错误）。
    pub fn resolve_tool(&self, tool_name: &str) -> Option<&RegisteredPlugin> {
        let pid = self.tool_owner.get(tool_name)?;
        self.plugins.get(pid)
    }

    /// 合并所有已注册插件的工具清单（静态层），供 `Supervisor::list_tools` 与
    /// `host/listTools` 直接使用，按字典序稳定返回。
    pub fn list_all_tools(&self) -> Vec<(String, ToolDescriptor)> {
        let mut out: Vec<(String, ToolDescriptor)> = self
            .plugins
            .values()
            .flat_map(|p| p.tools.iter().cloned().map(|t| (p.id().to_string(), t)))
            .collect();
        out.sort_by(|a, b| a.1.name.cmp(&b.1.name));
        out
    }

    // ── 只读访问器（供 UI 展示）──────────────────────

    pub fn list_plugins_sorted(&self) -> Vec<&RegisteredPlugin> {
        self.plugins.values().collect()
    }

    pub fn conflicts(&self) -> &[ToolConflict] {
        &self.conflicts
    }

    pub fn load_failures(&self) -> &[LoadError] {
        &self.load_failures
    }

    /// 已注册插件数（不含失败、不含只因为 tool 冲突被丢弃的条目）。
    pub fn len(&self) -> usize {
        self.plugins.len()
    }

    pub fn is_empty(&self) -> bool {
        self.plugins.is_empty()
    }

    /// 返回所有已注册 plugin_id 的集合；方便做"热更新前后差异比较"。
    pub fn plugin_ids(&self) -> BTreeSet<&str> {
        self.plugins.keys().map(String::as_str).collect()
    }
}

// ─────────────────── 单元测试 ───────────────────
#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::discovery::load_plugin_dir;
    use std::fs;
    use tempfile::tempdir;

    fn write_manifest(dir: &Path, toml_text: &str) {
        fs::write(dir.join("manifest.toml"), toml_text.as_bytes()).unwrap();
    }

    // 构造一个带 n_tools 个工具的 toml，工具名为 `{prefix}:t{N}`。
    fn plugin_toml(id: &str, prefix: &str, n_tools: usize) -> String {
        let mut tools_toml = String::new();
        for i in 0..n_tools {
            tools_toml.push_str(&format!(
                r#"
[[tools]]
name = "{prefix}:t{i}"
description = "{prefix} tool {i}"
[tools.input_schema]
type = "object"
"#
            ));
        }
        format!(
            r#"
[plugin]
id = "{id}"
name = "{id}"
version = "1.0.0"

[exec]
command = "python"
{tools_toml}
"#
        )
    }

    fn make_plugin(root: &Path, dir_name: &str, toml: &str) -> PathBuf {
        let dir = root.join(dir_name);
        fs::create_dir_all(&dir).unwrap();
        write_manifest(&dir, toml);
        dir
    }

    // ── 空 / 不存在 ─────────────────────────────────

    #[test]
    fn registry_empty_root_builds_clean() {
        let tmp = tempdir().unwrap();
        let reg = Registry::scan_and_build(&tmp.path().join("empty")).unwrap();
        assert!(reg.is_empty());
        assert_eq!(reg.list_all_tools().len(), 0);
        assert!(reg.conflicts().is_empty());
        assert!(reg.load_failures().is_empty());
    }

    // ── 正常注册 1-2 插件 ───────────────────────────

    #[test]
    fn registry_one_plugin_three_tools() {
        let tmp = tempdir().unwrap();
        make_plugin(tmp.path(), "p", &plugin_toml("com.example.one", "one", 3));
        let reg = Registry::scan_and_build(tmp.path()).unwrap();
        assert_eq!(reg.len(), 1);
        let plugin = reg.get_plugin("com.example.one").unwrap();
        assert_eq!(plugin.tools.len(), 3);
        assert_eq!(reg.resolve_tool("one:t1").unwrap().id(), "com.example.one");
        let all = reg.list_all_tools();
        assert_eq!(all.len(), 3);
        let names: Vec<_> = all.iter().map(|(_, t)| t.name.clone()).collect();
        assert_eq!(names, ["one:t0", "one:t1", "one:t2"]);
    }

    #[test]
    fn registry_two_plugins_tools_merged() {
        let tmp = tempdir().unwrap();
        make_plugin(tmp.path(), "a-dir", &plugin_toml("com.example.a", "a", 2));
        make_plugin(tmp.path(), "b-dir", &plugin_toml("com.example.b", "b", 2));
        let reg = Registry::scan_and_build(tmp.path()).unwrap();
        assert_eq!(reg.len(), 2);
        let all_names: Vec<_> = reg
            .list_all_tools()
            .into_iter()
            .map(|(_, t)| t.name)
            .collect();
        assert_eq!(all_names, ["a:t0", "a:t1", "b:t0", "b:t1"]);
    }

    // ── 跨插件工具名冲突：先到先得 ──────────────────

    #[test]
    fn cross_plugin_tool_conflict_first_owner_wins() {
        let tmp = tempdir().unwrap();
        // 两个插件都声明 tool "shared:echo"。目录字典序 alpha < beta，alpha 先到。
        let alpha = make_plugin(
            tmp.path(),
            "alpha",
            &plugin_toml("com.example.alpha", "shared", 1), // shared:t0
        );
        // beta 额外再声明一次 shared:t0。
        let beta_dir = tmp.path().join("beta");
        fs::create_dir_all(&beta_dir).unwrap();
        write_manifest(
            &beta_dir,
            r#"
[plugin]
id = "com.example.beta"
name = "Beta"
version = "1.0.0"

[exec]
command = "python"

[[tools]]
name = "shared:t0"
[tools.input_schema]
type = "object"

[[tools]]
name = "beta:own"
[tools.input_schema]
type = "object"
"#,
        );

        let reg = Registry::scan_and_build(tmp.path()).unwrap();
        // shared:t0 解析到 alpha（先到）。
        let owner = reg.resolve_tool("shared:t0").unwrap();
        assert_eq!(owner.id(), "com.example.alpha");
        // alpha 保留 1 个工具。
        assert_eq!(reg.get_plugin("com.example.alpha").unwrap().tools.len(), 1);
        // beta 丢掉 shared:t0，但保留 beta:own，所以 1 个。
        let beta = reg.get_plugin("com.example.beta").unwrap();
        assert_eq!(
            beta.tools
                .iter()
                .map(|t| t.name.as_str())
                .collect::<Vec<_>>(),
            ["beta:own"]
        );
        // 冲突表应有一条。
        assert_eq!(reg.conflicts().len(), 1);
        let c = &reg.conflicts()[0];
        assert_eq!(c.tool_name, "shared:t0");
        assert_eq!(c.owner_plugin, "com.example.alpha");
        assert_eq!(c.rejected_from, "com.example.beta");
        // 解析 beta:own 存在。
        assert_eq!(
            reg.resolve_tool("beta:own").unwrap().id(),
            "com.example.beta"
        );
        // UI 需要显示失败的目录路径（通过原始 LoadedPlugin 路径保存在 RegisteredPlugin）。
        assert_eq!(
            reg.get_plugin("com.example.alpha").unwrap().plugin_dir,
            alpha
        );
    }

    // ── 重复 plugin.id：后加载者整体拒绝 ──────────

    #[test]
    fn duplicate_plugin_id_rejects_second_entirely() {
        let tmp = tempdir().unwrap();
        make_plugin(
            tmp.path(),
            "first-dir",
            &plugin_toml("com.example.same", "a", 2),
        );
        make_plugin(
            tmp.path(),
            "z-dir", // 字典序 last，保证 first 先加载
            &plugin_toml("com.example.same", "z", 3),
        );
        let reg = Registry::scan_and_build(tmp.path()).unwrap();
        // 只有一个插件（先到者保持）。
        assert_eq!(reg.len(), 1);
        let only = reg.get_plugin("com.example.same").unwrap();
        assert_eq!(only.tools.len(), 2); // "a" 家的工具
                                         // 它的目录名应为 first-dir。
        assert!(only.plugin_dir.ends_with("first-dir"));
        // 冲突 1 条（* 表示整体拒绝）。
        assert_eq!(reg.conflicts().len(), 1);
        assert_eq!(reg.conflicts()[0].tool_name, "*");
    }

    // ── 失败条目收集但不阻断成功插件 ─────────────────

    #[test]
    fn load_failures_collected_along_successes() {
        let tmp = tempdir().unwrap();
        make_plugin(tmp.path(), "ok", &plugin_toml("com.example.ok", "ok", 1));
        let missing = tmp.path().join("missing-manifest");
        fs::create_dir_all(&missing).unwrap(); // 没有 manifest.toml
        let reg = Registry::scan_and_build(tmp.path()).unwrap();
        assert_eq!(reg.len(), 1);
        assert_eq!(reg.load_failures().len(), 1);
        assert!(matches!(
            reg.load_failures()[0],
            super::discovery::LoadError::MissingManifest { .. }
        ));
        // 成功插件工具仍然可解析。
        assert_eq!(reg.resolve_tool("ok:t0").unwrap().id(), "com.example.ok");
    }

    // ── insert_loaded 可用于热加载单独目录 ─────────

    #[test]
    fn insert_loaded_after_new_dir_created() {
        let tmp = tempdir().unwrap();
        make_plugin(tmp.path(), "a", &plugin_toml("com.example.a", "a", 1));
        let mut reg = Registry::scan_and_build(tmp.path()).unwrap();
        assert_eq!(reg.len(), 1);

        // 用户新建一个插件目录，手动 load 再 insert。
        let new = tmp.path().join("b");
        fs::create_dir_all(&new).unwrap();
        write_manifest(&new, &plugin_toml("com.example.b", "b", 1));
        let loaded = load_plugin_dir(&new).unwrap();
        reg.insert_loaded(loaded);

        assert_eq!(reg.len(), 2);
        assert_eq!(reg.resolve_tool("b:t0").unwrap().id(), "com.example.b");
        let names: Vec<_> = reg
            .list_all_tools()
            .into_iter()
            .map(|(_, t)| t.name)
            .collect();
        assert_eq!(names, ["a:t0", "b:t0"]);
    }

    // ── list_all_tools 稳定字典序（跨插件同名已被 conflict 去除）───────

    #[test]
    fn list_all_tools_stable_sort_regardless_of_insertion_order() {
        // 用"直接构造 RegisteredPlugin"会比较麻烦，换个思路：扫描顺序是按目录名
        // 字典序，先到先得。我们造 2 个插件，工具名分散，断言最终 list 的顺序。
        let tmp = tempdir().unwrap();
        let toml = |id: &str, names: &[&str]| -> String {
            let mut tools = String::new();
            for n in names {
                tools.push_str(&format!(
                    r#"
[[tools]]
name = "{n}"
description = "d"
input_schema = {{type = "object"}}
"#
                ));
            }
            format!(
                r#"
[plugin]
id = "{id}"
name = "{id}"
version = "1.0.0"

[exec]
command = "python"
{tools}
"#
            )
        };
        // 验证 toml 文本含 [[tools]] 且 n_tools 正确
        let sample = toml("com.example.z", &["tool:c", "tool:a"]);
        assert!(sample.contains("[[tools]]"));
        assert_eq!(sample.matches("name = \"tool:").count(), 2);
        let parsed: Manifest = crate::protocol::manifest::load_from_str(&sample).unwrap();
        assert_eq!(
            parsed.tools.len(),
            2,
            "预验证失败：sample.tools={:?}",
            parsed.tools
        );

        make_plugin(
            tmp.path(),
            "z",
            &toml("com.example.z", &["tool:c", "tool:a"]),
        );
        make_plugin(tmp.path(), "a", &toml("com.example.a2", &["tool:b"]));
        let reg = Registry::scan_and_build(tmp.path()).unwrap();
        let names: Vec<_> = reg
            .list_all_tools()
            .into_iter()
            .map(|(_, t)| t.name)
            .collect();
        assert_eq!(names, ["tool:a", "tool:b", "tool:c"]);
    }

    // ── list_plugins_sorted 字典序（BTreeMap 自动保证）────

    #[test]
    fn plugins_iterated_in_id_lexicographic_order() {
        let tmp = tempdir().unwrap();
        // 目录顺序与 id 顺序相反，验证按 id 排而不是目录排。
        make_plugin(
            tmp.path(),
            "z-last-dir",
            &plugin_toml("com.example.first", "a", 1),
        );
        make_plugin(
            tmp.path(),
            "a-first-dir",
            &plugin_toml("com.example.second", "b", 1),
        );
        let reg = Registry::scan_and_build(tmp.path()).unwrap();
        let ids: Vec<_> = reg
            .list_plugins_sorted()
            .iter()
            .map(|p| p.id().to_string())
            .collect();
        // BTreeMap<String, _> 键字典序
        assert_eq!(ids, ["com.example.first", "com.example.second"]);
    }

    // ── resolve_tool 对未知工具返回 None ──────────

    #[test]
    fn resolve_unknown_tool_returns_none() {
        let tmp = tempdir().unwrap();
        make_plugin(tmp.path(), "p", &plugin_toml("com.example.x", "x", 1));
        let reg = Registry::scan_and_build(tmp.path()).unwrap();
        assert!(reg.resolve_tool("nonexistent:foo").is_none());
        assert!(reg.get_plugin("does.not.exist").is_none());
    }

    // ── plugin_ids 集合工具 ───────────────────────

    #[test]
    fn plugin_ids_btreeset_matches() {
        let tmp = tempdir().unwrap();
        make_plugin(tmp.path(), "p1", &plugin_toml("com.example.p1", "p1", 0));
        make_plugin(tmp.path(), "p2", &plugin_toml("com.example.p2", "p2", 0));
        let reg = Registry::scan_and_build(tmp.path()).unwrap();
        let ids = reg.plugin_ids();
        assert!(ids.contains("com.example.p1"));
        assert!(ids.contains("com.example.p2"));
        assert_eq!(ids.len(), 2);
    }
}
