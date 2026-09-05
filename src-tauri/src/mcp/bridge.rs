//! MCP 桥接层：内部工具视图到 MCP 工具视图的转换与过滤。
//!
//! 三道关卡，顺序固定（先便宜后昂贵、先粗后细）：
//!
//! 1. **高危插件拦截**：插件只要声明了任一 [`DangerLevel::High`] 权限，其**全部**
//!    工具都不暴露，即使白名单勾了也不暴露。权限模型是插件级粗粒度的，所以拦截
//!    也只能是插件级的。
//! 2. **白名单过滤**：默认零暴露。键格式必须与 `commands.rs` 的 `list_tools` /
//!    `set_tool_exposed` 完全一致，即 `format!("{plugin_id}:{tool_name}")`。
//! 3. **工具名映射**：MCP 规范只允许 `[A-Za-z0-9_-]`，而内部工具名惯例带冒号
//!    （`ocr:recognize`）。这里做**正向映射表**：暴露时生成 `mcp_name → 原始名`
//!    字典，`tools/call` 查表。绝不做反向字符串还原——`a_b` 究竟来自 `a:b` 还是
//!    `a.b` 还是本来就叫 `a_b`，字符串层面无法区分。
//!
//! 映射冲突时先到先得：后者不予暴露并记入 [`McpToolTable::conflicts`]。

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Value as JsonValue};

use crate::config::McpExposure;
use crate::protocol::manifest::DangerLevel;
use crate::registry::{RegisteredPlugin, Registry};

/// 一个已通过全部过滤、可对外暴露的工具。
#[derive(Debug, Clone, PartialEq)]
pub struct McpToolEntry {
    /// 对外的 MCP 工具名，满足 `^[A-Za-z0-9_-]+$`。
    pub mcp_name: String,
    /// 内部原始工具名，例如 `ocr:recognize`。调用内核时用这个。
    pub original_name: String,
    /// 归属插件 ID。
    pub plugin_id: String,
    pub description: String,
    /// Schema 原样透出，不做任何改写。
    pub input_schema: JsonValue,
}

/// 一次映射名冲突。后者被排除。
#[derive(Debug, Clone, PartialEq)]
pub struct McpNameConflict {
    /// 冲突的 MCP 名。
    pub mcp_name: String,
    /// 先占住这个名字的原始工具名。
    pub occupied_by: String,
    /// 因为撞名而被排除的原始工具名。
    pub rejected: String,
    pub rejected_plugin: String,
}

/// 暴露给 MCP 客户端的工具全表。
///
/// 这是一个**快照**：白名单或注册表变化后需要重新 [`build`](Self::build)。
/// 服务端每次 `tools/list` 都重建，避免 UI 改了勾选而网关还在用旧表。
#[derive(Debug, Clone, Default)]
pub struct McpToolTable {
    /// 按 `mcp_name` 字典序排列，保证 `tools/list` 输出稳定。
    entries: Vec<McpToolEntry>,
    /// `mcp_name` → `entries` 下标。
    index: BTreeMap<String, usize>,
    conflicts: Vec<McpNameConflict>,
    /// 因高危权限被整体拦截的插件 ID。
    blocked_plugins: BTreeSet<String>,
}

impl McpToolTable {
    /// 依据当前注册表与白名单构建映射表。
    pub fn build(registry: &Registry, exposure: &McpExposure) -> Self {
        let mut blocked_plugins = BTreeSet::new();
        for plugin in registry.list_plugins_sorted() {
            if plugin_is_high_risk(plugin) {
                blocked_plugins.insert(plugin.id().to_string());
            }
        }

        let mut entries: Vec<McpToolEntry> = Vec::new();
        let mut index: BTreeMap<String, usize> = BTreeMap::new();
        let mut conflicts = Vec::new();

        // list_all_tools 已按原始工具名字典序返回，先到先得因此是确定性的。
        for (plugin_id, tool) in registry.list_all_tools() {
            if blocked_plugins.contains(&plugin_id) {
                tracing::debug!(
                    plugin_id = %plugin_id,
                    tool = %tool.name,
                    "高危插件，MCP 层不暴露"
                );
                continue;
            }
            let qualified = format!("{plugin_id}:{}", tool.name);
            if !exposure.is_exposed(&qualified) {
                continue;
            }

            let mcp_name = to_mcp_name(&tool.name);
            if let Some(&existing) = index.get(&mcp_name) {
                let occupied_by = entries[existing].original_name.clone();
                tracing::warn!(
                    mcp_name = %mcp_name,
                    occupied_by = %occupied_by,
                    rejected = %tool.name,
                    rejected_plugin = %plugin_id,
                    "MCP 工具名映射冲突，后者不予暴露"
                );
                conflicts.push(McpNameConflict {
                    mcp_name,
                    occupied_by,
                    rejected: tool.name.clone(),
                    rejected_plugin: plugin_id.clone(),
                });
                continue;
            }

            index.insert(mcp_name.clone(), entries.len());
            entries.push(McpToolEntry {
                mcp_name,
                original_name: tool.name.clone(),
                plugin_id: plugin_id.clone(),
                description: tool.description.clone(),
                input_schema: tool.input_schema.clone(),
            });
        }

        // entries 按原始名有序，但映射会改变相对顺序，重排一次以 mcp_name 为准。
        entries.sort_by(|a, b| a.mcp_name.cmp(&b.mcp_name));
        index = entries
            .iter()
            .enumerate()
            .map(|(i, e)| (e.mcp_name.clone(), i))
            .collect();

        Self {
            entries,
            index,
            conflicts,
            blocked_plugins,
        }
    }

    /// 直接用已经算好的条目建表，负责排序与建索引。
    ///
    /// 给不需要真实注册表的场景用（服务端单测、以后可能的缓存回放）。
    /// 走这条路**不做**高危拦截与白名单过滤——那两道闸在 [`Self::build`] 里，
    /// 所以生产代码一律用 `build`。
    pub fn from_entries(mut entries: Vec<McpToolEntry>) -> Self {
        entries.sort_by(|a, b| a.mcp_name.cmp(&b.mcp_name));
        entries.dedup_by(|a, b| a.mcp_name == b.mcp_name);
        let index = entries
            .iter()
            .enumerate()
            .map(|(i, e)| (e.mcp_name.clone(), i))
            .collect();
        Self {
            entries,
            index,
            conflicts: Vec::new(),
            blocked_plugins: BTreeSet::new(),
        }
    }

    pub fn tools(&self) -> &[McpToolEntry] {
        &self.entries
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// 查表把 MCP 名解析回原始工具名。**这是唯一的反解途径。**
    pub fn resolve(&self, mcp_name: &str) -> Option<&McpToolEntry> {
        self.index.get(mcp_name).map(|&i| &self.entries[i])
    }

    pub fn conflicts(&self) -> &[McpNameConflict] {
        &self.conflicts
    }

    pub fn blocked_plugins(&self) -> &BTreeSet<String> {
        &self.blocked_plugins
    }

    /// 渲染成 MCP `tools/list` 响应里的 `tools` 数组。
    pub fn to_mcp_tools_json(&self) -> Vec<JsonValue> {
        self.entries
            .iter()
            .map(|e| {
                json!({
                    "name": e.mcp_name,
                    "description": e.description,
                    "inputSchema": e.input_schema,
                })
            })
            .collect()
    }
}

/// 内部工具名 → MCP 工具名。
///
/// MCP 规范要求工具名只含 `[A-Za-z0-9_-]`，而内部惯例是 `前缀:动作`。
/// 所有不合规字符统一替换为 `_`：`ocr:recognize` → `ocr_recognize`。
///
/// 这个函数是**有损**的，因此绝不能反向使用，反解一律走 [`McpToolTable::resolve`]。
pub fn to_mcp_name(original: &str) -> String {
    original
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// 插件是否声明了高危权限。
///
/// 用 [`DangerLevel::High`] 判定而非字面量匹配 `input:control` / `process:spawn`，
/// 这样 `file:write:*` 这类同样高危的权限也会被一并覆盖，且未来新增高危权限时
/// 只需改 `danger_level()` 一处。
pub fn plugin_is_high_risk(plugin: &RegisteredPlugin) -> bool {
    plugin
        .manifest
        .capabilities
        .permissions
        .iter()
        .any(|p| p.danger_level() == DangerLevel::High)
}

// ─────────────────── 单元测试 ───────────────────
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;
    use tempfile::tempdir;

    fn write_plugin(root: &Path, dir_name: &str, toml_text: &str) {
        let dir = root.join(dir_name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("manifest.toml"), toml_text.as_bytes()).unwrap();
    }

    /// 普通插件：无权限声明，两个工具 `{prefix}:read` / `{prefix}:write`。
    fn plain_toml(id: &str, prefix: &str) -> String {
        format!(
            r#"
[plugin]
id = "{id}"
name = "{id}"
version = "1.0.0"

[exec]
command = "python"

[[tools]]
name = "{prefix}:read"
description = "读"
[tools.input_schema]
type = "object"

[[tools]]
name = "{prefix}:write"
description = "写"
[tools.input_schema]
type = "object"
"#
        )
    }

    fn exposure(items: &[&str]) -> McpExposure {
        McpExposure {
            exposed: items.iter().map(|s| s.to_string()).collect(),
        }
    }

    // ── 名字映射 ─────────────────────────────────

    #[test]
    fn 冒号被替换为下划线() {
        assert_eq!(to_mcp_name("ocr:recognize"), "ocr_recognize");
    }

    #[test]
    fn 合规字符原样保留() {
        assert_eq!(to_mcp_name("plain-name_1"), "plain-name_1");
    }

    #[test]
    fn 点号与空格也被规整() {
        assert_eq!(to_mcp_name("a.b c:d"), "a_b_c_d");
    }

    // ── 白名单 ───────────────────────────────────

    #[test]
    fn 默认零暴露() {
        let tmp = tempdir().unwrap();
        write_plugin(tmp.path(), "a", &plain_toml("com.example.a", "a"));
        let reg = Registry::scan_and_build(tmp.path()).unwrap();

        let table = McpToolTable::build(&reg, &McpExposure::default());
        assert!(
            table.is_empty(),
            "白名单为空时不应暴露任何工具，实际 {} 个",
            table.len()
        );
    }

    #[test]
    fn 勾选后出现在列表() {
        let tmp = tempdir().unwrap();
        write_plugin(tmp.path(), "a", &plain_toml("com.example.a", "a"));
        let reg = Registry::scan_and_build(tmp.path()).unwrap();

        let table = McpToolTable::build(&reg, &exposure(&["com.example.a:a:read"]));
        assert_eq!(table.len(), 1, "只勾了一个工具");
        assert_eq!(table.tools()[0].mcp_name, "a_read");
        assert_eq!(table.tools()[0].original_name, "a:read");
        assert_eq!(table.tools()[0].plugin_id, "com.example.a");
    }

    #[test]
    fn 未勾选的同插件工具不出现() {
        let tmp = tempdir().unwrap();
        write_plugin(tmp.path(), "a", &plain_toml("com.example.a", "a"));
        let reg = Registry::scan_and_build(tmp.path()).unwrap();

        let table = McpToolTable::build(&reg, &exposure(&["com.example.a:a:read"]));
        let names: Vec<_> = table.tools().iter().map(|t| t.mcp_name.as_str()).collect();
        assert_eq!(names, ["a_read"], "a:write 未勾选，不应出现");
    }

    // ── 正向映射表反解 ───────────────────────────

    #[test]
    fn 映射表能把mcp名解析回原始名() {
        let tmp = tempdir().unwrap();
        write_plugin(tmp.path(), "a", &plain_toml("com.example.a", "ocr"));
        let reg = Registry::scan_and_build(tmp.path()).unwrap();

        let table = McpToolTable::build(&reg, &exposure(&["com.example.a:ocr:read"]));
        let entry = table.resolve("ocr_read").expect("应能查到 ocr_read");
        assert_eq!(entry.original_name, "ocr:read");
        assert_eq!(entry.plugin_id, "com.example.a");
        assert!(table.resolve("不存在的名字").is_none());
    }

    // ── 映射冲突 ─────────────────────────────────

    #[test]
    fn 映射冲突时后者被排除并记录() {
        let tmp = tempdir().unwrap();
        // `dup:x` 与 `dup.x` 都会映射到 `dup_x`。放在同一插件里避免注册表层先行去重。
        write_plugin(
            tmp.path(),
            "a",
            r#"
[plugin]
id = "com.example.a"
name = "A"
version = "1.0.0"

[exec]
command = "python"

[[tools]]
name = "dup.x"
[tools.input_schema]
type = "object"

[[tools]]
name = "dup:x"
[tools.input_schema]
type = "object"
"#,
        );
        let reg = Registry::scan_and_build(tmp.path()).unwrap();
        let table = McpToolTable::build(
            &reg,
            &exposure(&["com.example.a:dup.x", "com.example.a:dup:x"]),
        );

        assert_eq!(table.len(), 1, "撞名的两个工具只应暴露一个");
        assert_eq!(table.conflicts().len(), 1, "应记录一条冲突");
        let c = &table.conflicts()[0];
        assert_eq!(c.mcp_name, "dup_x");
        // list_all_tools 按原始名排序，"dup.x" < "dup:x"（'.' = 0x2E < ':' = 0x3A）。
        assert_eq!(c.occupied_by, "dup.x", "先到者占名");
        assert_eq!(c.rejected, "dup:x", "后者被排除");
        assert_eq!(
            table.resolve("dup_x").unwrap().original_name,
            "dup.x",
            "查表应解析到先到者"
        );
    }

    // ── 高危拦截 ─────────────────────────────────

    fn high_risk_toml(id: &str, permission: &str) -> String {
        format!(
            r#"
[plugin]
id = "{id}"
name = "{id}"
version = "1.0.0"

[exec]
command = "python"

[[tools]]
name = "risky:go"
[tools.input_schema]
type = "object"

[capabilities]
permissions = ["{permission}"]
"#
        )
    }

    #[test]
    fn 高危插件即使勾选也不暴露() {
        for permission in ["input:control", "process:spawn"] {
            let tmp = tempdir().unwrap();
            write_plugin(
                tmp.path(),
                "r",
                &high_risk_toml("com.example.risky", permission),
            );
            let reg = Registry::scan_and_build(tmp.path()).unwrap();
            assert_eq!(reg.len(), 1, "前置条件：插件应加载成功（{permission}）");
            let table = McpToolTable::build(&reg, &exposure(&["com.example.risky:risky:go"]));

            assert!(table.is_empty(), "声明 {permission} 的插件即使勾选也不应暴露");
            assert!(
                table.blocked_plugins().contains("com.example.risky"),
                "应记入被拦截插件"
            );
            assert!(table.resolve("risky_go").is_none());
        }
    }

    #[test]
    fn 高危插件的全部工具一并被拦截() {
        let tmp = tempdir().unwrap();
        write_plugin(
            tmp.path(),
            "r",
            r#"
[plugin]
id = "com.example.risky"
name = "Risky"
version = "1.0.0"

[exec]
command = "python"

[[tools]]
name = "safe:ping"
[tools.input_schema]
type = "object"

[[tools]]
name = "risky:go"
[tools.input_schema]
type = "object"

[capabilities]
permissions = ["input:control"]
"#,
        );
        let reg = Registry::scan_and_build(tmp.path()).unwrap();
        let table = McpToolTable::build(
            &reg,
            &exposure(&["com.example.risky:safe:ping", "com.example.risky:risky:go"]),
        );
        assert!(table.is_empty(), "权限是插件级的，无害工具也一并拦截");
    }

    #[test]
    fn 通配符写盘同样算高危() {
        let tmp = tempdir().unwrap();
        write_plugin(
            tmp.path(),
            "w",
            r#"
[plugin]
id = "com.example.w"
name = "W"
version = "1.0.0"

[exec]
command = "python"

[[tools]]
name = "w:save"
[tools.input_schema]
type = "object"

[capabilities]
permissions = ["file:write:*"]
"#,
        );
        let reg = Registry::scan_and_build(tmp.path()).unwrap();
        let table = McpToolTable::build(&reg, &exposure(&["com.example.w:w:save"]));
        assert_eq!(reg.len(), 1, "前置条件：插件应加载成功");
        assert!(table.is_empty(), "file:write:* 是 High，应被拦截");
    }

    #[test]
    fn 低危权限不影响暴露() {
        let tmp = tempdir().unwrap();
        write_plugin(
            tmp.path(),
            "n",
            r#"
[plugin]
id = "com.example.n"
name = "N"
version = "1.0.0"

[exec]
command = "python"

[[tools]]
name = "n:read"
[tools.input_schema]
type = "object"

[capabilities]
permissions = ["file:read:~/docs"]
"#,
        );
        let reg = Registry::scan_and_build(tmp.path()).unwrap();
        let table = McpToolTable::build(&reg, &exposure(&["com.example.n:n:read"]));
        assert_eq!(table.len(), 1, "file:read 不是高危，应正常暴露");
        assert!(table.blocked_plugins().is_empty());
    }

    // ── 输出格式 ─────────────────────────────────

    #[test]
    fn schema原样透出() {
        let tmp = tempdir().unwrap();
        write_plugin(
            tmp.path(),
            "s",
            r#"
[plugin]
id = "com.example.s"
name = "S"
version = "1.0.0"

[exec]
command = "python"

[[tools]]
name = "s:go"
description = "描述"
[tools.input_schema]
type = "object"
required = ["path"]
[tools.input_schema.properties.path]
type = "string"
"#,
        );
        let reg = Registry::scan_and_build(tmp.path()).unwrap();
        let table = McpToolTable::build(&reg, &exposure(&["com.example.s:s:go"]));
        let rendered = table.to_mcp_tools_json();

        assert_eq!(rendered.len(), 1);
        assert_eq!(rendered[0]["name"], "s_go");
        assert_eq!(rendered[0]["description"], "描述");
        assert_eq!(rendered[0]["inputSchema"]["type"], "object");
        assert_eq!(
            rendered[0]["inputSchema"]["properties"]["path"]["type"],
            "string",
            "嵌套 schema 应原样保留"
        );
    }

    #[test]
    fn 输出按mcp名稳定排序() {
        let tmp = tempdir().unwrap();
        write_plugin(tmp.path(), "a", &plain_toml("com.example.a", "zeta"));
        write_plugin(tmp.path(), "b", &plain_toml("com.example.b", "alpha"));
        let reg = Registry::scan_and_build(tmp.path()).unwrap();
        let table = McpToolTable::build(
            &reg,
            &exposure(&[
                "com.example.a:zeta:read",
                "com.example.b:alpha:read",
                "com.example.b:alpha:write",
            ]),
        );
        let names: Vec<_> = table.tools().iter().map(|t| t.mcp_name.as_str()).collect();
        assert_eq!(names, ["alpha_read", "alpha_write", "zeta_read"]);
    }

    #[test]
    fn 白名单里的幽灵条目被忽略() {
        let tmp = tempdir().unwrap();
        write_plugin(tmp.path(), "a", &plain_toml("com.example.a", "a"));
        let reg = Registry::scan_and_build(tmp.path()).unwrap();
        // 插件已卸载但白名单残留。
        let table = McpToolTable::build(&reg, &exposure(&["com.gone.plugin:gone:tool"]));
        assert!(table.is_empty(), "不存在的工具不应凭空出现在表里");
    }
}
