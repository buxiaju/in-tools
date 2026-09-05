# 插件 UI + 后台常驻 + 全局快捷键 + 每插件设置 — 设计文档

> **日期**: 2026-09-04
> **状态**: 第一期已实现，第二、三期待定

## 1. 需求拆分

用户原始需求包含五个独立子系统：

| # | 子系统               | 难度 | 独立性   |
| - | ----------------- | -- | ----- |
| A | 托盘常驻（关窗口不退出）      | 低  | 可独立交付 |
| B | 全局快捷键（热键触发插件工具）   | 中  | 依赖 A  |
| C | 插件自带交互界面（如框选覆盖层）  | 高  | 协议级改造 |
| D | 独立调用路径（绕开 AI/MCP） | 低  | 部分已有  |
| E | 每插件设置页（参数可配置）     | 中  | 可独立交付 |

### 三期排期

```
第一期  A 托盘常驻 + B 全局快捷键 + D 单插件调用入口
        └─ 成果：快捷键按下 → 直接跑 screenshot:capture → 存盘 + 通知
           不需要动协议，不需要插件界面，是完整可用的闭环

第二期  E 每插件设置（manifest 声明 schema + 宿主渲染表单 + 持久化）
        └─ 成果：设置页能配「截图存哪」，且这个值能传进插件

第三期  C 插件自带交互界面（框选覆盖层等）
        └─ 协议扩展，安全模型要重新论证，单独立项
```

## 2. 第一期设计（已实现）

### 2.1 范围

- **A 托盘常驻**：窗口关闭按钮 → 隐藏而非退出；托盘图标 + 菜单（显示窗口 / 退出）

- **B 全局快捷键**：插件 manifest 声明 `[shortcut]` 表，宿主启动时自动注册热键

- **D 独立调用**：热键按下 → `Supervisor::call_tool` → 插件执行 → 日志

### 2.2 Manifest 扩展

新增可选 `[shortcut]` 表：

```toml
[shortcut]
key = "Ctrl+Shift+S"          # 快捷键串，由 tauri-plugin-global-shortcut 解析
tool = "screenshot:capture"   # 按下时调用的工具名，须在 [[tools]] 中声明
```

校验规则：

- `key` 与 `tool` 均不可为空（仅空白字符也算空）

- `tool` 必须在本插件的 `[[tools]]` 列表中声明

- 不写 `[shortcut]` 表时 `shortcut` 字段为 `None`，插件行为不受影响

### 2.3 托盘实现

**依赖**：`tauri` 加 `tray-icon` + `image-png` feature（核心内置，无额外插件）

**托盘菜单**：

- "显示窗口" → `window.show()` + `window.set_focus()`

- "退出" → `app.exit(0)`，触发 `RunEvent::Exit` 回收子进程

**窗口关闭 → 隐藏**：

```rust
window.on_window_event(move |event| {
    if let WindowEvent::CloseRequested { api, .. } = event {
        api.prevent_close();
        let _ = wc.hide();
    }
});
```

### 2.4 全局快捷键实现

**依赖**：`tauri-plugin-global-shortcut` v2

**注册流程**：

1. `assemble()` 在 registry 被 Supervisor 消费前，提取所有插件的 `shortcut` 绑定
2. `setup()` 中遍历绑定，调用 `app.global_shortcut().on_shortcut(key, handler)`
3. handler 仅响应 `ShortcutState::Pressed`，spawn 异步任务调用 `supervisor.call_tool(tool, {}, CallerIdentity::Ui)`
4. 注册失败（热键被占用）只记日志，不阻断启动

**安全考量**：

- 热键调用使用 `CallerIdentity::Ui` 身份，与界面手动调用工具一致

- 权限校验照常进行：若插件需 `screen:capture` 权限且用户未授权，会弹窗询问

- 审计日志照常记录

### 2.5 Capabilities 扩展

`capabilities/default.json` 新增权限：

- `core:tray:default` — 托盘图标

- `core:menu:default` — 托盘菜单

- `core:app:allow-default-window-icon` — 使用应用图标作为托盘图标

- `global-shortcut:default` — 全局快捷键

### 2.6 变更清单

| 文件                                        | 变更                                                                       |
| ----------------------------------------- | ------------------------------------------------------------------------ |
| `src-tauri/Cargo.toml`                    | tauri 加 `tray-icon`+`image-png` feature；加 `tauri-plugin-global-shortcut` |
| `src-tauri/capabilities/default.json`     | 加 4 条权限                                                                  |
| `src-tauri/src/protocol/manifest.rs`      | 加 `Shortcut` 结构 + `shortcut: Option<Shortcut>` 字段 + 校验 + 5 个测试           |
| `src-tauri/src/main.rs`                   | 托盘 `setup_tray()` + 窗口关闭隐藏 + 快捷键注册 + `assemble()` 返回绑定                   |
| `src-tauri/src/runtime/supervisor.rs`     | 测试辅助 `manifest_of` 加 `shortcut: None`                                    |
| `plugins/screenshot-plugin/manifest.toml` | 加 `[shortcut]` 表                                                         |

### 2.7 测试

- manifest 层：5 个新测试（解析成功、默认 None、key 空、tool 空、tool 不在 tools 列表）

- 编译：`cargo build` 通过

- clippy：`cargo clippy --all-targets -- -D warnings` 0 warning

- 全量测试：320 passed, 0 failed（275 lib + 16 + 12 + 11(+1 ignored) + 6）

### 2.8 已知限制

1. 快捷键当前由 manifest 硬编码，不支持用户自定义（第二期 E 的设置页会解决）
2. 托盘左键单击当前弹出菜单（而非切换窗口显隐），后续可加 `on_tray_icon_event` 定制
3. 快捷键调用工具的参数固定为 `{}`（空对象），不支持传参（第二期解决）
4. 热键注册失败只记日志，前端无感知（后续可 emit 事件让界面提示）

## 3. 第二期设计（已实现）

### 3.1 每插件设置页

- manifest 新增 `[[settings]]` 数组，每项声明 `key`、`label`、`type`、`default`、`options`

- 宿主在插件列表每个卡片上渲染「设置」按钮，点击弹出模态窗

- 模态窗根据字段类型自动生成表单控件：string → 文本框、number → 数字框、boolean → 开关、select → 下拉框

- 设置值持久化到 `~/.intools/plugin-settings/<plugin_id>.json`

- 工具调用时（`call_tool` 和快捷键两条路径），宿主将设置值合并进 `args`：设置值作默认，显式传参覆盖同名键

### 3.2 Manifest 扩展

```toml
[[settings]]
key = "output_dir"
label = "截图保存目录"
type = "string"
default = "~/Pictures"

[[settings]]
key = "image_format"
label = "图片格式"
type = "select"
default = "png"
options = ["png", "jpg"]
```

校验规则：

- `key` 非空且不重复

- `label` 非空

- `type = "select"` 时 `options` 不可为空

- 不写 `[[settings]]` 时设置列表为空，插件行为不受影响

### 3.3 设置合并机制

`call_tool` 和快捷键两条路径均在调用 `Supervisor::call_tool` 前合并设置：

1. `Supervisor::plugin_id_for_tool(tool_name)` 解析工具名 → 插件 ID
2. `config::load_plugin_settings(plugin_id)` 读取持久化设置值
3. 以设置值为基础，用户显式 args 覆盖同名键
4. 合并后的 JSON 对象传给 `Supervisor::call_tool`

MCP 路径不合并设置：MCP 调用方是 AI，应自行提供参数。

### 3.4 变更清单

| 文件                                        | 变更                                                                                               |
| ----------------------------------------- | ------------------------------------------------------------------------------------------------ |
| `src-tauri/src/protocol/manifest.rs`      | 加 `SettingField` + `SettingType` + `settings: Vec<SettingField>` + 校验 + 6 个测试                    |
| `src-tauri/src/config/mod.rs`             | 加 `plugin_settings_dir()` + `plugin_settings_json()` + `load/save_plugin_settings()`             |
| `src-tauri/src/runtime/supervisor.rs`     | 加 `plugin_id_for_tool()` + `list_plugins_with_manifests()` + 导入 `Manifest`                       |
| `src-tauri/src/commands.rs`               | 加 `get_plugin_settings` + `save_plugin_settings` 命令 + `merge_plugin_settings()` + 修改 `call_tool` |
| `src-tauri/src/main.rs`                   | 快捷键调用合并设置 + 注册新命令                                                                                |
| `src/index.html`                          | 加插件设置模态窗                                                                                         |
| `src/main.js`                             | 加 `openPluginSettings()` + `savePluginSettingsModal()` + 插件卡片「设置」按钮                              |
| `plugins/screenshot-plugin/manifest.toml` | 加 `[[settings]]` 两个字段                                                                            |

### 3.5 质量验证

- `cargo build` — 通过

- `cargo clippy --all-targets -- -D warnings` — 0 warning

- `cargo test` — 326 passed, 0 failed（281 lib + 16 + 12 + 11(+1 ignored) + 6）

### 3.6 已知限制

1. 设置合并仅对 UI 调用和快捷键生效，MCP 路径不合并（设计决策）
2. 设置值不做类型校验（前端渲染时按类型生成控件，但后端存储时不过验）
3. 无「恢复默认」按钮（前端可加）
4. 设置页未区分「有设置」和「无设置」的插件——所有插件都显示「设置」按钮

## 4. 第三期设计（待定）

### 4.1 插件自带交互界面

- 协议扩展：`plugin/ui:request` 方法，插件可请求宿主弹出特定 UI

- 安全模型：CSP 限制下，插件 UI 不加载任意 HTML/JS，而是通过声明式 schema 让宿主渲染

- screenshot 框选覆盖层：全屏无边框置顶窗口，鼠标拖拽框选，返回区域坐标

- 这是最复杂的部分，需要重新论证协议和安全边界

