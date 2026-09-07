# API 文档

> InTools 内部 API 和 Tauri Commands 参考。

## 📋 目录

- [Tauri Commands](#tauri-commands)
- [插件协议](#插件协议)
- [反向 RPC](#反向-rpc)
- [MCP 网关](#mcp-网关)
- [插件市场 API](#插件市场-api)

## Tauri Commands

Tauri Commands 是前端与 Rust 内核之间的桥梁。所有命令都在 `src-tauri/src/commands.rs` 中定义。

### 插件管理

#### `list_plugins`

列出所有已加载的插件。

**参数：** 无

**返回：** `Vec<PluginView>`

```rust
pub struct PluginView {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: String,
    pub state: String,           // "running" | "stopped" | "error"
    pub tools: Vec<ToolView>,
    pub has_shortcut: bool,
    pub settings_fields: Vec<SettingFieldView>,
}
```

#### `start_plugin`

启动一个插件。

**参数：**
- `plugin_id: String` - 插件 ID

**返回：** `Result<()>`

#### `stop_plugin`

停止一个插件。

**参数：**
- `plugin_id: String` - 插件 ID

**返回：** `Result<()>`

#### `set_plugin_enabled`

启用/禁用一个插件。

**参数：**
- `plugin_id: String` - 插件 ID
- `enabled: bool` - 是否启用

**返回：** `Result<bool>` - 是否真的发生了变化

#### `uninstall_plugin`

卸载一个插件（删除其目录）。

**参数：**
- `plugin_id: String` - 插件 ID

**返回：** `Result<()>`

### 插件包管理

#### `import_plugin_package`

从 zip 文件导入插件包。

**参数：**
- `zip_bytes: Vec<u8>` - zip 文件字节

**返回：** `Result<ImportResultView>`

```rust
pub struct ImportResultView {
    pub plugin_id: String,
    pub plugin_name: String,
    pub installed_dir: String,
}
```

#### `reload_plugins`

重新扫描插件目录。

**参数：** 无

**返回：** `Result<ReloadResultView>`

```rust
pub struct ReloadResultView {
    pub added: Vec<String>,     // 新增的插件 ID
    pub removed: Vec<String>,   // 移除的插件 ID
    pub total: usize,           // 总数
}
```

### 工具调用

#### `list_tools`

列出所有可用的工具。

**参数：** 无

**返回：** `Vec<ToolView>`

```rust
pub struct ToolView {
    pub name: String,
    pub description: String,
    pub plugin_id: String,
    pub input_schema: serde_json::Value,
}
```

#### `call_tool`

调用一个工具。

**参数：**
- `tool_name: String` - 工具名称
- `args: serde_json::Value` - 工具参数

**返回：** `Result<serde_json::Value>`

#### `respond_permission_prompt`

回复权限询问。

**参数：**
- `id: String` - 询问 ID
- `decision: String` - 决定（"allow_always" | "allow_session" | "deny_always" | "deny_once"）

**返回：** `Result<bool>`

### 权限管理

#### `list_grants`

列出所有落盘的授权记录。

**参数：** 无

**返回：** `Vec<GrantView>`

#### `revoke_grants`

撤销某插件的全部授权。

**参数：**
- `plugin_id: String` - 插件 ID

**返回：** `Result<()>`

#### `set_tool_exposed`

设置某工具对 MCP 客户端的暴露。

**参数：**
- `plugin_id: String` - 插件 ID
- `tool_name: String` - 工具名称
- `exposed: bool` - 是否暴露

**返回：** `Result<bool>`

### 设置

#### `get_settings`

获取宿主配置。

**参数：** 无

**返回：** `Result<SettingsView>`

```rust
pub struct SettingsView {
    pub plugins_dir: Option<String>,
    pub mcp_enabled: bool,
    pub mcp_token: Option<String>,
    pub log_level: String,
    pub close_behavior: String,
    pub marketplace_enabled: bool,
    pub marketplace_url: String,
}
```

#### `save_settings`

保存宿主配置。

**参数：**
- `plugins_dir: String` - 插件目录
- `log_level: String` - 日志级别
- `close_behavior: String` - 关闭行为

**返回：** `Result<SettingsView>`

#### `set_mcp_enabled`

启用/禁用 MCP 网关。

**参数：**
- `enabled: bool` - 是否启用

**返回：** `Result<SettingsView>`

### AI 配置

#### `get_ai_config`

获取 AI 编排配置。

**参数：** 无

**返回：** `Result<AiConfigView>`

#### `set_ai_config`

保存 AI 编排配置。

**参数：**
- `base_url: String` - API base URL
- `api_key: String` - API 密钥
- `model: String` - 模型名

**返回：** `Result<()>`

#### `test_ai_connection`

测试 AI API 连接。

**参数：**
- `base_url: String` - API base URL
- `api_key: String` - API 密钥
- `model: String` - 模型名

**返回：** `Result<String>`

### 插件市场

#### `get_marketplace_config`

获取插件市场配置。

**参数：** 无

**返回：** `Result<MarketplaceConfigView>`

#### `set_marketplace_config`

设置插件市场配置。

**参数：**
- `enabled: bool` - 是否启用
- `url: String` - 市场 URL

**返回：** `Result<()>`

#### `fetch_marketplace_plugins`

从插件市场获取插件列表。

**参数：**
- `marketplace_url: String` - 市场 URL

**返回：** `Result<Vec<MarketplacePluginView>>`

### 插件设置

#### `get_plugin_settings`

获取插件设置字段定义和当前值。

**参数：**
- `plugin_id: String` - 插件 ID

**返回：** `Result<PluginSettingsView>`

```rust
pub struct PluginSettingsView {
    pub fields: Vec<SettingFieldView>,
    pub values: serde_json::Value,
}

pub struct SettingFieldView {
    pub key: String,
    pub label: String,
    pub field_type: String,      // "string" | "number" | "boolean" | "select"
    pub default: serde_json::Value,
    pub options: Option<Vec<String>>,
    pub value: serde_json::Value,
}
```

#### `save_plugin_settings`

保存插件设置值。

**参数：**
- `plugin_id: String` - 插件 ID
- `values: serde_json::Value` - 设置值

**返回：** `Result<()>`

### 快捷键

#### `get_plugin_shortcut`

获取插件的快捷键配置。

**参数：**
- `plugin_id: String` - 插件 ID

**返回：** `Result<ShortcutView>`

#### `set_plugin_shortcut`

设置插件的快捷键。

**参数：**
- `plugin_id: String` - 插件 ID
- `key: String` - 快捷键
- `enabled: bool` - 是否启用

**返回：** `Result<()>`

#### `list_shortcut_bindings`

列出所有快捷键绑定。

**参数：** 无

**返回：** `Result<Vec<ShortcutBindingView>>`

### UI 命令

#### `get_pixel_color`

获取指定屏幕坐标的像素颜色（Windows 专用）。

**参数：**
- `x: i32` - x 坐标
- `y: i32` - y 坐标

**返回：** `Result<PixelColorView>`

#### `get_clipboard_history`

获取剪贴板历史。

**参数：** 无

**返回：** `Result<Vec<ClipboardEntry>>`

#### `close_overlay`

关闭覆盖层窗口。

**参数：** 无

**返回：** `Result<()>`

## 插件协议

InTools 插件通过 JSON-RPC 2.0 over stdio 与宿主通信。每条消息一行，以 `\n` 结尾。

### 握手流程

```
宿主                         插件
  │                           │
  ├── spawn subprocess ──────►│
  │◄── plugin/hello ─────────┤   {method:"plugin/hello", params:{protocol_version, tools}}
  ├── plugin/ready ──────────►│   {method:"plugin/ready", params:{config, plugin_dir}}
  │◄── result ───────────────┤
  │   ◄── 正常 RPC 通信 ────► │
```

### 宿主 → 插件

```json
{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}
{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"ocr:recognize","arguments":{"image_path":"/tmp/shot.png"}}}
{"jsonrpc":"2.0","id":3,"method":"plugin/shutdown","params":{}}
```

### 插件 → 宿主

```json
{"jsonrpc":"2.0","id":2,"result":{"text":"你好世界","confidence":0.98}}
{"jsonrpc":"2.0","id":2,"error":{"code":-32000,"message":"图片文件不存在"}}
```

## 反向 RPC

### 方法列表

| 方法 | 用途 |
|------|------|
| `host/listTools` | 获取当前所有可用工具及其 Schema |
| `host/callTool` | 调用其他插件的工具 |
| `host/getConfig` | 读取本插件的配置项 |
| `host/setConfig` | 写入本插件的配置项 |
| `host/notify` | 向 UI 推送消息 |
| `host/uiRequest` | 请求宿主弹出特定 UI |

### 调用示例

```python
# 调用其他工具
result, error = host_call("host/callTool", {
    "name": "ocr:recognize",
    "arguments": {"image_path": "/tmp/shot.png"}
})

# 发送通知
notify("notify/stream", {"delta": "正在识别..."})
notify("notify/toolCall", {"tool": "ocr:recognize", "status": "running"})
```

## MCP 网关

### 端点

```
POST http://127.0.0.1:7801/mcp
```

### 鉴权

```
Authorization: Bearer <token>
```

### 方法

- `initialize`：初始化连接
- `tools/list`：列出可用工具
- `tools/call`：调用工具

### 工具名转换

- `ocr:recognize` → `ocr_recognize`（冒号转下划线）
- 反向解析：建立正向映射表 `mcp_name → 原始工具名`

## 插件市场 API

### 端点

市场使用静态 JSON API：

- `GET /plugins.json` - 插件列表索引
- `GET /versions.json` - 版本信息
- `GET /search.json` - 搜索索引

### 响应格式

```json
{
  "version": "1.0.0",
  "generated_at": "2026-09-06T00:00:00Z",
  "total": 32,
  "plugins": [
    {
      "id": "com.intools.ai-orchestrator",
      "name": "AI 编排",
      "version": "0.2.0",
      "description": "...",
      "author": "InTools",
      "category": "system",
      "tags": ["ai", "network"],
      "homepage": "https://...",
      "license": "MIT",
      "created_at": "2026-01-01T00:00:00Z",
      "updated_at": "2026-09-06T00:00:00Z",
      "file_size": 1024,
      "download_count": 0,
      "rating": 0,
      "rating_count": 0
    }
  ]
}
```

## 错误码

| 错误码 | 含义 |
|--------|------|
| `-32700` | Parse error（解析错误） |
| `-32600` | Invalid Request（无效请求） |
| `-32601` | Method not found（方法未找到） |
| `-32602` | Invalid params（参数无效） |
| `-32603` | Internal error（内部错误） |
| `-32000` 至 `-32099` | 服务器自定义错误 |

## 参考

- [设计文档](superpowers/specs/2026-09-02-intools-plugin-host-design.md)
- [插件开发指南](plugin-development.md)
- [架构文档](architecture.md)
- [安全文档](security.md)
