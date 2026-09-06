
# InTools

InTools 是一个基于「一切皆插件」理念的桌面工具宿主程序（v0.4.0），通过统一的协议层将各类工具的能力汇聚在一起，提供简洁的操作界面与权限管控。

## 核心理念

> **一切皆插件。** 文件操作是插件，系统控制是插件，网络工具是插件，AI 编排也是插件。用户可以通过 AI 对话一句话生成新的插件，任何编程语言（Python、Node.js、Go、Rust…）都能接入。

## 核心特性

- **插件化架构**：任何可执行程序或脚本（Python、Node.js、Go、Rust…）均可作为插件接入
- **统一通信协议**：JSON-RPC 2.0 over stdin/stdout，跨语言一致
- **多语言 SDK**：Python SDK + Node.js SDK，零协议样板代码写插件
- **AI 自然语言**：内置 AI 编排，对话中自动调用插件完成任务
- **一句话生成插件**：对话中描述需求，AI 自动调用插件生成器创建完整项目
- **三级分类**：系统插件 / 测试插件 / 用户插件，隔离管理
- **权限模型**：细粒度权限控制（三级危险度），用户可选授予/拒绝
- **全局快捷键**：为插件配置全局热键，脱离窗口焦点也可触发
- **MCP 网关**：启用后可通过 HTTP 端点被 Claude Desktop 等 MCP 客户端调用
- **跨平台支持**：基于 Tauri 构建，支持 Windows、macOS、Linux

## 系统插件

InTools 内置 17 个系统插件，覆盖电脑操作的完整能力：

### 核心工具

| 插件 | 语言 | 工具 | 能力 |
|------|------|------|------|
| **file-ops** | Python | file:read/write/list/mkdir/delete/copy/move/info | 文件系统操作 |
| **file-search** | Python | search:files | 文件搜索 |
| **clipboard-tool** | Python | clipboard:read/append/clear/has/write | 剪贴板历史 |
| **color-picker** | Python | color:pick/at | 屏幕取色 |
| **screenshot-plugin** | Python | screenshot:capture | 屏幕截图 |
| **ocr-tool** | Python | ocr:recognize | OCR 文字识别 |
| **window-info** | Python | window:active/list/find | 窗口信息 |
| **process-mgr** | Python | proc:list/kill/start/exec | 进程管理 |
| **system-ctrl** | Python | sys:volume/lock/notify/clipboard | 系统控制 |
| **net-tools** | Python | net:ping/dns/check-port/http/ip | 网络工具 |
| **app-launcher** | Python | app:open/search/explorer | 应用启动器 |
| **ai-orchestrator** | Python | ai:chat | AI 自然语言对话 |
| **file-hash** | Python | hash:file/dir | 文件哈希校验 |

### 开发者工具

| 插件 | 语言 | 工具 | 能力 |
|------|------|------|------|
| **dev-console** | Python | dev:plugins/tools/call/validate/search | 对话中调试插件 |
| **plugin-builder** | Python | plugin:generate/list-templates | 一句话生成插件 |
| **system-info** | Node.js | system:info/cpu/network/disk/env | 系统信息（跨语言示例） |
| **workflow-runner** | Node.js | workflow:list/run/chain | 跨插件工作流编排 |

### 测试插件

| 插件 | 用途 |
|------|------|
| **caller-plugin** | 反向 RPC 集成测试 |
| **responder-plugin** | 权限拦截测试 |

## 目录结构

```
InTools/
├── src-tauri/              # Tauri 宿主程序（Rust）
│   ├── src/
│   │   ├── commands.rs     # 前端交互命令（含 AI 连接测试）
│   │   ├── config/         # 配置管理（原子写入）
│   │   ├── gateway.rs      # MCP 网关
│   │   ├── hotkey.rs       # 全局快捷键
│   │   ├── mcp/           # MCP 协议实现
│   │   ├── permission/    # 权限管理（三级危险度）
│   │   ├── protocol/      # 插件通信协议（JSON-RPC 2.0）
│   │   ├── registry/      # 插件注册表（system/test/user 三分类）
│   │   ├── runtime/       # 运行时管理（子进程生命周期）
│   │   ├── shortcut/      # 快捷键解析
│   │   └── ui.rs          # UI 适配层
│   └── tests/             # 集成测试
├── plugins/                # 插件源码
│   ├── system/            # 系统插件（随安装包分发）
│   │   ├── file-ops/      # 文件操作
│   │   ├── clipboard-tool/# 剪贴板
│   │   ├── screenshot-plugin/ # 截图
│   │   ├── ocr-tool/      # OCR
│   │   └── ...            # 其他系统插件
│   ├── test/              # 测试插件
│   ├── python-sdk/        # Python 插件 SDK
│   ├── node-sdk/          # Node.js 插件 SDK
│   └── tools/             # 开发者工具
│       ├── scaffold.cjs   # 插件脚手架
│       ├── validate.cjs   # manifest 校验器
│       └── test-plugin.cjs # 插件协议测试
├── src/                   # 前端资源
│   ├── index.html         # 主界面
│   ├── main.js            # 前端逻辑
│   ├── style.css          # 样式
│   └── overlay.js         # 截图/取色/剪贴板覆盖层
└── docs/                  # 文档
    └── plugin-development.md  # 插件开发手册
```

## 快速开始

### 安装

从 [Gitee releases](https://gitee.com/buxiaju/in-tools/releases) 下载安装包，或从源码编译：

```bash
cd src-tauri
cargo tauri build
```

### 使用

1. 启动 InTools，系统插件自动加载
2. 在 AI 对话中输入自然语言，AI 自动调用工具完成任务
3. 试试："帮我找找桌面上有没有叫报告的文件"
4. 或直接在插件页管理插件的启停、设置、快捷键

## 多语言插件开发

### 三步写一个 Python 插件

```python
# 1. 导入 SDK
import sys, os, importlib.util
spec = importlib.util.spec_from_file_location("intools",
    os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "python-sdk", "intools.py"))
mod = importlib.util.module_from_spec(spec); spec.loader.exec_module(mod)
Plugin = mod.Plugin; plugin = Plugin()

# 2. 注册工具
@plugin.tool("my:echo", "回显文本", {"type": "object", "properties": {"text": {"type": "string"}}})
def echo(args, ctx):
    return {"text": args["text"]}

# 3. 启动
plugin.start()
```

### 三步写一个 Node.js 插件

```js
const { Plugin } = require("../node-sdk/intools");
const plugin = new Plugin("1.0");
plugin.tool("my:echo", "回显文本", { type: "object", properties: { text: { type: "string" } } },
  (args) => ({ text: args.text }));
plugin.start();
```

### 支持的语言

| 语言 | SDK | 特点 |
|------|-----|------|
| Python | 内置 SDK（`python-sdk/intools.py`） | 装饰器注册，零样板 |
| Node.js | 内置 SDK（`node-sdk/intools.js`） | 装饰器注册，异步支持 |
| Go | 无 SDK（直接用标准库） | 编译型二进制，高性能 |
| Rust | 无 SDK（直接用 serde_json） | 编译型二进制 |
| 任意语言 | 只要能读写 stdin/stdout | JSON-RPC 2.0 协议 |

### 生成新插件

```bash
# 用脚手架工具生成插件项目
node tools/scaffold.cjs my-tool python "我的工具"

# 校验 manifest
node tools/validate.cjs plugins/my-tool/manifest.toml

# 测试插件协议
node tools/test-plugin.cjs plugins/my-tool --list-tools
```

## AI 对话

AI 编排插件（`ai-orchestrator`）支持 OpenAI 兼容 API（DeepSeek、Ollama、OpenAI 等）。

### 配置

在设置页填入 API Base URL、API Key、模型名，点击「测试连接」验证。

### 使用示例

- "帮我找找桌面上有没有叫报告的文件" → 调用 file-search
- "截取当前屏幕" → 调用 screenshot-plugin
- "帮我做一个天气查询插件" → 调用 plugin-builder
- "列出所有插件" → 调用 dev-console

## 权限模型

插件需在 `manifest.toml` 中声明所需权限。权限分三级：

| 危险度 | 处理方式 | 示例 |
|--------|---------|------|
| 低危 | 自动放行 | file:read, clipboard:read |
| 中危 | 查记录 / 弹窗询问 | file:write, clipboard:write |
| 高危 | 弹窗询问，不对 MCP 暴露 | process:spawn, shell:exec |

## MCP 网关

启用后，本地 MCP 客户端（Claude Desktop、Cursor 等）可直接调用 InTools 的插件工具。

```
设置 → MCP 网关 → 开启
↓
Claude Desktop 配置 InTools 端点
↓
在 Claude 中使用 InTools 工具
```

## 文档

- [插件开发手册](./docs/plugin-development.md) — 完整的插件开发指南
- [设计文档](./docs/superpowers/specs/) — 架构设计与实施计划

## 许可证

本项目采用 [MIT License](./LICENSE) 开源协议。
