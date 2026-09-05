//! 快捷键绑定解析：把「manifest 默认值」与「用户覆盖」合成一张最终绑定表，
//! 并在合成过程中检测跨插件按键冲突。
//!
//! 这个模块刻意做成**纯函数**（不碰文件、不碰 Tauri、不碰进程）：输入是插件
//! manifest 列表加上一份 `UserShortcuts`，输出是绑定表加问题清单。理由有两条：
//!
//! 1. 快捷键的真正难点全在这层规则里（三种优先级、空串语义、冲突裁决），
//!    做成纯函数才能用单元测试把每条规则钉死，不必真去注册一个全局热键。
//! 2. `main.rs` 里负责注册的那段代码因此退化成「遍历 + 调 API」，
//!    它是 Tauri 依赖最重、最难测的地方，逻辑越薄越好。
//!
//! 按键串的语法校验也放在这里。用户是通过按键捕获输入的，理论上不会产生
//! 非法组合，但 `~/.intools/shortcuts.json` 是明文文件、允许手改，所以宿主
//! 必须假定它可能是任意垃圾内容。

#![allow(dead_code)]

use crate::config::UserShortcuts;
use crate::protocol::manifest::Manifest;

// ─────────────────── 按键串归一化 ───────────────────

/// 归一化后的修饰键顺序。固定顺序才能让「Shift+Ctrl+S」与「Ctrl+Shift+S」
/// 被识别为同一个键，否则两个插件用不同书写顺序绑同一组合键，冲突检测会漏判。
const MODIFIER_ORDER: [&str; 4] = ["Ctrl", "Alt", "Shift", "Super"];

/// 允许的具名主键。白名单而非黑名单：宁可少支持几个冷门键，也不要把
/// 无法被 `tauri-plugin-global-shortcut` 解析的字符串放过去——那种错误
/// 只会在运行时注册失败，用户看到的是「设置保存了但热键没反应」。
const NAMED_KEYS: [&str; 25] = [
    "Space",
    "Enter",
    "Tab",
    "Escape",
    "Backspace",
    "Delete",
    "Insert",
    "Home",
    "End",
    "PageUp",
    "PageDown",
    "Up",
    "Down",
    "Left",
    "Right",
    "Comma",
    "Period",
    "Slash",
    "Semicolon",
    "Quote",
    "Backquote",
    "Minus",
    "Equal",
    "BracketLeft",
    "BracketRight",
];

/// 按键串不合法的原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyError {
    /// 空串。调用方通常应先判空再走归一化，走到这里说明逻辑漏了一层。
    Empty,
    /// 一个修饰键都没有。纯单键全局热键会劫持整个系统的该按键，不允许。
    NoModifier,
    /// 没有主键，或有多个主键（如 `Ctrl+A+B`）。
    MainKeyCount(usize),
    /// 主键不在白名单内。
    UnknownKey(String),
    /// 同一个修饰键写了两次（如 `Ctrl+Ctrl+S`）。
    DuplicateModifier(String),
}

impl std::fmt::Display for KeyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(f, "按键不能为空"),
            Self::NoModifier => write!(f, "至少需要一个修饰键（Ctrl / Alt / Shift / Super）"),
            Self::MainKeyCount(n) => {
                write!(f, "必须且只能有一个主键，当前有 {n} 个")
            }
            Self::UnknownKey(k) => write!(f, "不支持的按键 `{k}`"),
            Self::DuplicateModifier(m) => write!(f, "修饰键 `{m}` 重复"),
        }
    }
}

impl std::error::Error for KeyError {}

/// 把一段修饰键别名折叠成规范名。返回 `None` 表示它不是修饰键。
///
/// 别名表覆盖各平台的常见写法：用户可能从 macOS 教程里抄来 `Cmd+Shift+S`，
/// 也可能手写 `Control`。统一折叠成 Windows 语义，免得同一组合键存出两种字面量。
fn canonical_modifier(part: &str) -> Option<&'static str> {
    match part.to_ascii_lowercase().as_str() {
        "ctrl" | "control" | "commandorcontrol" | "cmdorctrl" => Some("Ctrl"),
        "alt" | "option" => Some("Alt"),
        "shift" => Some("Shift"),
        "super" | "meta" | "cmd" | "command" | "win" => Some("Super"),
        _ => None,
    }
}

/// 把一段主键折叠成规范名。返回 `None` 表示不在白名单内。
fn canonical_main_key(part: &str) -> Option<String> {
    // 单个字母 / 数字：统一大写。
    if part.len() == 1 {
        let c = part.chars().next().unwrap();
        if c.is_ascii_alphanumeric() {
            return Some(c.to_ascii_uppercase().to_string());
        }
    }
    // 功能键 F1..F24。注意不能只判首字母是 F，否则 `Foo` 会被放过。
    let lower = part.to_ascii_lowercase();
    if let Some(digits) = lower.strip_prefix('f') {
        if !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()) {
            if let Ok(n) = digits.parse::<u32>() {
                if (1..=24).contains(&n) {
                    return Some(format!("F{n}"));
                }
            }
        }
    }
    // 具名键：忽略大小写匹配白名单，返回白名单里的规范拼写。
    NAMED_KEYS
        .iter()
        .find(|k| k.eq_ignore_ascii_case(part))
        .map(|k| (*k).to_string())
}

/// 校验并归一化一个按键串，例如 `"shift+ctrl+s"` → `"Ctrl+Shift+S"`。
///
/// 归一化的产物同时是**冲突检测的比较基准**和**写入 shortcuts.json 的字面量**，
/// 所以必须幂等：`normalize_key(normalize_key(x)) == normalize_key(x)`。
pub fn normalize_key(raw: &str) -> Result<String, KeyError> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(KeyError::Empty);
    }

    let mut modifiers: Vec<&'static str> = Vec::new();
    let mut main_keys: Vec<String> = Vec::new();

    for part in raw.split('+') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some(m) = canonical_modifier(part) {
            if modifiers.contains(&m) {
                return Err(KeyError::DuplicateModifier(m.to_string()));
            }
            modifiers.push(m);
        } else if let Some(k) = canonical_main_key(part) {
            main_keys.push(k);
        } else {
            return Err(KeyError::UnknownKey(part.to_string()));
        }
    }

    if main_keys.len() != 1 {
        return Err(KeyError::MainKeyCount(main_keys.len()));
    }
    if modifiers.is_empty() {
        return Err(KeyError::NoModifier);
    }

    // 按固定顺序输出，而不是按用户输入顺序。
    let mut out: Vec<&str> = MODIFIER_ORDER
        .iter()
        .filter(|m| modifiers.contains(m))
        .copied()
        .collect();
    out.push(&main_keys[0]);
    Ok(out.join("+"))
}

// ─────────────────── 绑定与问题 ───────────────────

/// 一条最终生效的绑定来自哪里。UI 据此显示「默认」还是「已自定义」。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindingSource {
    /// 来自 manifest 的 `[shortcut] key`。
    Manifest,
    /// 来自 `~/.intools/shortcuts.json` 的用户覆盖。
    User,
}

impl BindingSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Manifest => "manifest",
            Self::User => "user",
        }
    }
}

/// 一条真正要去注册的绑定。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedBinding {
    pub plugin_id: String,
    /// 归一化后的按键串，可直接交给 `global_shortcut().on_shortcut()`。
    pub key: String,
    /// 要调用的工具名，永远来自 manifest。
    pub tool: String,
    /// 交互界面标识（如 `region-select`），永远来自 manifest。
    pub ui: Option<String>,
    pub source: BindingSource,
}

/// 一条被跳过的绑定及原因。
///
/// 不用 `Result` 收集是因为「有问题」并不阻断其余绑定：一个插件按键写错，
/// 不该导致另外五个插件的热键全部失效。宿主要做的是尽量注册 + 把问题摊给用户看。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShortcutIssue {
    pub plugin_id: String,
    /// 出问题的按键串（原始值，未归一化，方便用户对照自己写的内容）。
    pub key: String,
    /// 按键冲突时，占用该按键的插件 ID；按键非法时为 `None`。
    pub owner_plugin: Option<String>,
    pub reason: String,
}

/// 解析结果。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResolveOutcome {
    pub bindings: Vec<ResolvedBinding>,
    pub issues: Vec<ShortcutIssue>,
}

/// 把 manifest 声明与用户覆盖合成最终绑定表。
///
/// `plugins` 需按 `plugin_id` 字典序传入（`Registry::list_plugins_sorted` 天然满足）。
/// 顺序决定冲突裁决：同一按键被多个插件抢时**字典序在前者得**。用字典序而非
/// 加载顺序，是为了让结果可复现——加载顺序会随文件系统枚举顺序变化，
/// 用户会看到「同样两个插件，重启一次热键就换人了」这种鬼故事。
///
/// 优先级规则（逐插件独立判定）：
/// 1. 用户覆盖存在且 `enabled == false` → 整条跳过，不注册也不算冲突。
/// 2. 用户覆盖的 `key` 非空 → 用它，来源标 `User`。
/// 3. 否则回落 manifest 的 `key`；若 manifest 的 key 也是空串 →
///    这是「支持热键但未绑定」，静默跳过，**不算问题**。
pub fn resolve_bindings(plugins: &[(&str, &Manifest)], user: &UserShortcuts) -> ResolveOutcome {
    let mut out = ResolveOutcome::default();
    // 归一化按键 → 已占用它的 plugin_id。
    let mut taken: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();

    for (plugin_id, manifest) in plugins {
        let Some(sc) = &manifest.shortcut else {
            continue;
        };

        let override_entry = user.get(plugin_id);
        if let Some(o) = override_entry {
            if !o.enabled {
                continue;
            }
        }

        let (raw_key, source) = match override_entry {
            Some(o) if !o.key.trim().is_empty() => (o.key.trim().to_string(), BindingSource::User),
            _ => (sc.key.trim().to_string(), BindingSource::Manifest),
        };

        // 「支持热键但没绑」是正常状态，不是问题。
        if raw_key.is_empty() {
            continue;
        }

        let key = match normalize_key(&raw_key) {
            Ok(k) => k,
            Err(e) => {
                out.issues.push(ShortcutIssue {
                    plugin_id: (*plugin_id).to_string(),
                    key: raw_key,
                    owner_plugin: None,
                    reason: format!("按键格式不合法：{e}"),
                });
                continue;
            }
        };

        if let Some(owner) = taken.get(&key) {
            out.issues.push(ShortcutIssue {
                plugin_id: (*plugin_id).to_string(),
                key: raw_key,
                owner_plugin: Some(owner.clone()),
                reason: format!("按键 `{key}` 已被插件 `{owner}` 占用（按插件 ID 字典序先到先得）"),
            });
            continue;
        }

        taken.insert(key.clone(), (*plugin_id).to_string());
        out.bindings.push(ResolvedBinding {
            plugin_id: (*plugin_id).to_string(),
            key,
            tool: sc.tool.clone(),
            ui: sc.ui.clone(),
            source,
        });
    }

    out
}

/// 在已有绑定表里查找某按键的占用者，忽略 `exclude_plugin` 自己。
///
/// 给「保存快捷键」命令做前置检查用：用户按下一个组合键时，宿主要能立刻回答
/// 「这个键被谁占了」，而不是等保存完再从 issues 里翻。`exclude_plugin` 是为了
/// 让用户把自己的键改成自己原来的值时不报冲突。
pub fn find_key_owner<'a>(
    bindings: &'a [ResolvedBinding],
    key: &str,
    exclude_plugin: &str,
) -> Option<&'a ResolvedBinding> {
    let normalized = normalize_key(key).ok()?;
    bindings
        .iter()
        .find(|b| b.key == normalized && b.plugin_id != exclude_plugin)
}

// ─────────────────── 单元测试 ───────────────────
#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::manifest::load_from_str;

    // ── 按键归一化 ──────────────────────────────

    #[test]
    fn normalize_orders_modifiers_canonically() {
        assert_eq!(normalize_key("Shift+Ctrl+S").unwrap(), "Ctrl+Shift+S");
        assert_eq!(normalize_key("Alt+Ctrl+A").unwrap(), "Ctrl+Alt+A");
        assert_eq!(
            normalize_key("Shift+Super+Alt+Ctrl+Z").unwrap(),
            "Ctrl+Alt+Shift+Super+Z"
        );
    }

    #[test]
    fn normalize_is_idempotent() {
        let once = normalize_key("shift+ctrl+s").unwrap();
        let twice = normalize_key(&once).unwrap();
        assert_eq!(once, twice);
    }

    #[test]
    fn normalize_folds_modifier_aliases() {
        assert_eq!(normalize_key("Control+S").unwrap(), "Ctrl+S");
        assert_eq!(normalize_key("CommandOrControl+S").unwrap(), "Ctrl+S");
        assert_eq!(normalize_key("Option+S").unwrap(), "Alt+S");
        assert_eq!(normalize_key("Cmd+S").unwrap(), "Super+S");
        assert_eq!(normalize_key("Win+S").unwrap(), "Super+S");
        assert_eq!(normalize_key("Meta+S").unwrap(), "Super+S");
    }

    #[test]
    fn normalize_uppercases_letters_and_keeps_digits() {
        assert_eq!(normalize_key("ctrl+s").unwrap(), "Ctrl+S");
        assert_eq!(normalize_key("Ctrl+7").unwrap(), "Ctrl+7");
    }

    #[test]
    fn normalize_accepts_function_keys() {
        assert_eq!(normalize_key("Ctrl+f1").unwrap(), "Ctrl+F1");
        assert_eq!(normalize_key("Alt+F12").unwrap(), "Alt+F12");
        assert_eq!(normalize_key("Alt+F24").unwrap(), "Alt+F24");
        // F25 不存在，别被 strip_prefix('f') 蒙过去。
        assert!(normalize_key("Alt+F25").is_err());
        // 以 f 开头但不是功能键。
        assert!(normalize_key("Alt+Foo").is_err());
    }

    #[test]
    fn normalize_accepts_named_keys_case_insensitively() {
        assert_eq!(normalize_key("Ctrl+space").unwrap(), "Ctrl+Space");
        assert_eq!(normalize_key("Alt+PAGEUP").unwrap(), "Alt+PageUp");
        assert_eq!(normalize_key("Ctrl+Shift+escape").unwrap(), "Ctrl+Shift+Escape");
    }

    #[test]
    fn normalize_rejects_empty() {
        assert_eq!(normalize_key("").unwrap_err(), KeyError::Empty);
        assert_eq!(normalize_key("   ").unwrap_err(), KeyError::Empty);
    }

    #[test]
    fn normalize_rejects_modifier_only_key() {
        // 只有修饰键，没有主键。
        assert_eq!(
            normalize_key("Ctrl+Shift").unwrap_err(),
            KeyError::MainKeyCount(0)
        );
    }

    #[test]
    fn normalize_rejects_bare_key_without_modifier() {
        // 纯单键会劫持全系统该按键，必须拒。
        assert_eq!(normalize_key("S").unwrap_err(), KeyError::NoModifier);
        assert_eq!(normalize_key("F1").unwrap_err(), KeyError::NoModifier);
    }

    #[test]
    fn normalize_rejects_multiple_main_keys() {
        assert_eq!(
            normalize_key("Ctrl+A+B").unwrap_err(),
            KeyError::MainKeyCount(2)
        );
    }

    #[test]
    fn normalize_rejects_duplicate_modifier() {
        assert_eq!(
            normalize_key("Ctrl+Ctrl+S").unwrap_err(),
            KeyError::DuplicateModifier("Ctrl".to_string())
        );
        // 别名重复也要认出来。
        assert_eq!(
            normalize_key("Ctrl+Control+S").unwrap_err(),
            KeyError::DuplicateModifier("Ctrl".to_string())
        );
    }

    #[test]
    fn normalize_rejects_unknown_key() {
        assert_eq!(
            normalize_key("Ctrl+Banana").unwrap_err(),
            KeyError::UnknownKey("Banana".to_string())
        );
    }

    #[test]
    fn normalize_tolerates_spaces_and_stray_plus() {
        assert_eq!(normalize_key(" Ctrl + Shift + S ").unwrap(), "Ctrl+Shift+S");
        assert_eq!(normalize_key("Ctrl++S").unwrap(), "Ctrl+S");
    }

    // ── 绑定解析 ────────────────────────────────

    /// 造一个带 `[shortcut]` 的最小 manifest。`key` 传空串即「支持但未绑定」。
    fn manifest_with_shortcut(id: &str, key: &str, ui: Option<&str>) -> Manifest {
        let ui_line = match ui {
            Some(u) => format!("ui = \"{u}\"\n"),
            None => String::new(),
        };
        let toml = format!(
            r#"
[plugin]
id = "{id}"
name = "Test"
version = "1.0.0"

[exec]
command = "python"
args = ["main.py"]

[[tools]]
name = "t:run"
description = "d"

[shortcut]
key = "{key}"
tool = "t:run"
{ui_line}
[capabilities]
permissions = []
"#
        );
        load_from_str(&toml).expect("测试 manifest 应当合法")
    }

    /// 造一个不带 `[shortcut]` 的 manifest。
    fn manifest_without_shortcut(id: &str) -> Manifest {
        let toml = format!(
            r#"
[plugin]
id = "{id}"
name = "Test"
version = "1.0.0"

[exec]
command = "python"
args = ["main.py"]

[[tools]]
name = "t:run"
description = "d"

[capabilities]
permissions = []
"#
        );
        load_from_str(&toml).expect("测试 manifest 应当合法")
    }

    #[test]
    fn plugin_without_shortcut_table_is_skipped() {
        let m = manifest_without_shortcut("com.test.a");
        let out = resolve_bindings(&[("com.test.a", &m)], &UserShortcuts::default());
        assert!(out.bindings.is_empty());
        assert!(out.issues.is_empty());
    }

    #[test]
    fn manifest_default_key_is_used_when_no_override() {
        let m = manifest_with_shortcut("com.test.a", "Ctrl+Shift+S", None);
        let out = resolve_bindings(&[("com.test.a", &m)], &UserShortcuts::default());
        assert_eq!(out.bindings.len(), 1);
        let b = &out.bindings[0];
        assert_eq!(b.key, "Ctrl+Shift+S");
        assert_eq!(b.tool, "t:run");
        assert_eq!(b.source, BindingSource::Manifest);
        assert!(out.issues.is_empty());
    }

    #[test]
    fn manifest_key_is_normalized_on_resolve() {
        // 插件作者写的顺序不规范，也要能注册，且归一化后参与冲突比较。
        let m = manifest_with_shortcut("com.test.a", "shift+ctrl+s", None);
        let out = resolve_bindings(&[("com.test.a", &m)], &UserShortcuts::default());
        assert_eq!(out.bindings[0].key, "Ctrl+Shift+S");
    }

    #[test]
    fn empty_manifest_key_without_override_is_silently_unbound() {
        // 「支持热键但作者不预设」——既不注册，也不该报问题。
        let m = manifest_with_shortcut("com.test.a", "", None);
        let out = resolve_bindings(&[("com.test.a", &m)], &UserShortcuts::default());
        assert!(out.bindings.is_empty());
        assert!(out.issues.is_empty());
    }

    #[test]
    fn user_override_wins_over_manifest_default() {
        let m = manifest_with_shortcut("com.test.a", "Ctrl+Shift+S", None);
        let mut user = UserShortcuts::default();
        user.set("com.test.a", "Alt+F9", true);
        let out = resolve_bindings(&[("com.test.a", &m)], &user);
        assert_eq!(out.bindings.len(), 1);
        assert_eq!(out.bindings[0].key, "Alt+F9");
        assert_eq!(out.bindings[0].source, BindingSource::User);
    }

    #[test]
    fn user_override_binds_a_previously_unbound_plugin() {
        let m = manifest_with_shortcut("com.test.a", "", None);
        let mut user = UserShortcuts::default();
        user.set("com.test.a", "Ctrl+Alt+K", true);
        let out = resolve_bindings(&[("com.test.a", &m)], &user);
        assert_eq!(out.bindings.len(), 1);
        assert_eq!(out.bindings[0].key, "Ctrl+Alt+K");
        assert_eq!(out.bindings[0].source, BindingSource::User);
    }

    #[test]
    fn disabled_override_skips_registration_entirely() {
        let m = manifest_with_shortcut("com.test.a", "Ctrl+Shift+S", None);
        let mut user = UserShortcuts::default();
        user.set("com.test.a", "Ctrl+Shift+S", false);
        let out = resolve_bindings(&[("com.test.a", &m)], &user);
        assert!(out.bindings.is_empty());
        // 用户主动关掉的东西不是「问题」。
        assert!(out.issues.is_empty());
    }

    #[test]
    fn disabled_override_with_empty_key_also_skips() {
        // 用户只是「停用」，没换键：仍应跳过 manifest 默认值。
        let m = manifest_with_shortcut("com.test.a", "Ctrl+Shift+S", None);
        let mut user = UserShortcuts::default();
        user.set("com.test.a", "", false);
        let out = resolve_bindings(&[("com.test.a", &m)], &user);
        assert!(out.bindings.is_empty());
        assert!(out.issues.is_empty());
    }

    #[test]
    fn ui_and_tool_always_come_from_manifest() {
        // 用户覆盖只能改键，不能改调哪个工具、要不要框选界面。
        let m = manifest_with_shortcut("com.test.a", "Ctrl+Shift+S", Some("region-select"));
        let mut user = UserShortcuts::default();
        user.set("com.test.a", "Alt+F9", true);
        let out = resolve_bindings(&[("com.test.a", &m)], &user);
        let b = &out.bindings[0];
        assert_eq!(b.tool, "t:run");
        assert_eq!(b.ui.as_deref(), Some("region-select"));
    }

    #[test]
    fn invalid_user_key_is_reported_and_does_not_fall_back() {
        // 非法覆盖不静默回落到 manifest 默认值：否则用户看到的是
        // 「我改了键，热键却还是旧的」，比直接报错更难排查。
        let m = manifest_with_shortcut("com.test.a", "Ctrl+Shift+S", None);
        let mut user = UserShortcuts::default();
        user.set("com.test.a", "Banana", true);
        let out = resolve_bindings(&[("com.test.a", &m)], &user);
        assert!(out.bindings.is_empty());
        assert_eq!(out.issues.len(), 1);
        assert_eq!(out.issues[0].plugin_id, "com.test.a");
        assert_eq!(out.issues[0].key, "Banana");
        assert!(out.issues[0].owner_plugin.is_none());
    }

    #[test]
    fn invalid_key_in_one_plugin_does_not_break_others() {
        let bad = manifest_with_shortcut("com.test.a", "", None);
        let good = manifest_with_shortcut("com.test.b", "Ctrl+Shift+G", None);
        let mut user = UserShortcuts::default();
        user.set("com.test.a", "Ctrl+Ctrl+X", true);
        let out = resolve_bindings(&[("com.test.a", &bad), ("com.test.b", &good)], &user);
        assert_eq!(out.bindings.len(), 1);
        assert_eq!(out.bindings[0].plugin_id, "com.test.b");
        assert_eq!(out.issues.len(), 1);
    }

    #[test]
    fn duplicate_key_first_in_lexicographic_order_wins() {
        let a = manifest_with_shortcut("com.test.a", "Ctrl+Shift+S", None);
        let b = manifest_with_shortcut("com.test.b", "Ctrl+Shift+S", None);
        let out = resolve_bindings(
            &[("com.test.a", &a), ("com.test.b", &b)],
            &UserShortcuts::default(),
        );
        assert_eq!(out.bindings.len(), 1);
        assert_eq!(out.bindings[0].plugin_id, "com.test.a");
        assert_eq!(out.issues.len(), 1);
        assert_eq!(out.issues[0].plugin_id, "com.test.b");
        assert_eq!(out.issues[0].owner_plugin.as_deref(), Some("com.test.a"));
    }

    #[test]
    fn conflict_detected_across_different_spellings() {
        // 归一化的价值：两种写法本质同键，不能都注册成功。
        let a = manifest_with_shortcut("com.test.a", "Ctrl+Shift+S", None);
        let b = manifest_with_shortcut("com.test.b", "shift+control+s", None);
        let out = resolve_bindings(
            &[("com.test.a", &a), ("com.test.b", &b)],
            &UserShortcuts::default(),
        );
        assert_eq!(out.bindings.len(), 1);
        assert_eq!(out.issues.len(), 1);
    }

    #[test]
    fn user_override_can_resolve_a_manifest_conflict() {
        // 两插件默认撞键，用户给后者换个键，两个就都能用了。
        let a = manifest_with_shortcut("com.test.a", "Ctrl+Shift+S", None);
        let b = manifest_with_shortcut("com.test.b", "Ctrl+Shift+S", None);
        let mut user = UserShortcuts::default();
        user.set("com.test.b", "Ctrl+Shift+B", true);
        let out = resolve_bindings(&[("com.test.a", &a), ("com.test.b", &b)], &user);
        assert_eq!(out.bindings.len(), 2);
        assert!(out.issues.is_empty());
    }

    #[test]
    fn override_on_unrelated_plugin_id_is_ignored() {
        // shortcuts.json 里残留已卸载插件的条目不该影响现存插件。
        let m = manifest_with_shortcut("com.test.a", "Ctrl+Shift+S", None);
        let mut user = UserShortcuts::default();
        user.set("com.test.ghost", "Alt+F9", true);
        let out = resolve_bindings(&[("com.test.a", &m)], &user);
        assert_eq!(out.bindings.len(), 1);
        assert_eq!(out.bindings[0].key, "Ctrl+Shift+S");
        assert!(out.issues.is_empty());
    }

    // ── 占用查询 ────────────────────────────────

    #[test]
    fn find_key_owner_locates_conflicting_plugin() {
        let a = manifest_with_shortcut("com.test.a", "Ctrl+Shift+S", None);
        let out = resolve_bindings(&[("com.test.a", &a)], &UserShortcuts::default());
        let owner = find_key_owner(&out.bindings, "shift+ctrl+s", "com.test.b").unwrap();
        assert_eq!(owner.plugin_id, "com.test.a");
    }

    #[test]
    fn find_key_owner_excludes_self() {
        let a = manifest_with_shortcut("com.test.a", "Ctrl+Shift+S", None);
        let out = resolve_bindings(&[("com.test.a", &a)], &UserShortcuts::default());
        // 用户把自己的键「改成」原值，不该报冲突。
        assert!(find_key_owner(&out.bindings, "Ctrl+Shift+S", "com.test.a").is_none());
    }

    #[test]
    fn find_key_owner_returns_none_for_free_or_invalid_key() {
        let a = manifest_with_shortcut("com.test.a", "Ctrl+Shift+S", None);
        let out = resolve_bindings(&[("com.test.a", &a)], &UserShortcuts::default());
        assert!(find_key_owner(&out.bindings, "Alt+F9", "com.test.b").is_none());
        assert!(find_key_owner(&out.bindings, "Banana", "com.test.b").is_none());
    }
}
