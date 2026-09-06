
# InTools

InTools 是一个基于插件机制的桌面工具宿主程序（v0.4.0），通过统一的协议层将各类工具（插件）的能力汇聚在一起，提供简洁的操作界面与权限管控。

## 核心特性

- **插件化架构**：任何可执行程序或脚本（如 Python、Node.js、二进制）均可作为插件接入
- **统一通信协议**：基于 JSON-RPC 2.0 的进程间通信，插件与宿主之间的交互清晰可预测
- **权限模型**：细粒度的权限控制，插件需声明所需权限，用户可选择授予或拒绝
- **全局快捷键**：支持为插件配置全局热键，脱离窗口焦点也可触发
- **AI 编排集成**：内置 AI 编排插件，可调用 LLM API 组合调用多种工具
- **插件导入**：支持从 zip 包导入第三方插件
- **MCP 网关**：启用后可通过 HTTP 端点被其他 MCP 客户端调用，支持 Bearer Token 认证
- **跨平台支持**：基于 Tauri 构建，支持 Windows、macOS、Linux

## 目录结构

```
in-tools/
├── src-tauri/              # Tauri 宿主程序（Rust）
│   ├── src/
│   │   ├── commands.rs     # 前端交互命令
│   │   ├── config/         # 配置管理
│   │   ├── gateway.rs      # MCP 网关
│   │   ├── hotkey.rs       # 全局快捷键
│   │   ├── mcp/           # MCP 协议实现
│   │   ├── permission/    # 权限管理
│   │   ├── protocol/      # 插件通信协议
│   │   ├── registry/      # 插件注册表
│   │   ├── runtime/       # 运行时管理
│   │   ├── shortcut/      # 快捷键解析
│   │   └── ui.rs          # UI 相关
│   └── tests/             # 集成测试
├── plugins/                # 示范插件（Python）
│   ├── ai-orchestrator/   # AI 编排插件（Python）
│   ├── caller-plugin/     # 反向 RPC 集成测试桩（Python）
│   ├── clipboard-tool/    # 剪贴板工具（Python，含剪贴板历史）
│   ├── color-picker/      # 取色器（Python）
│   ├── file-hash/         # 文件哈希工具（Go，编译型插件示例）
│   ├── file-search/       # 文件搜索工具（Python）
│   ├── hello-plugin/      # 入门示范插件（Python）
│   ├── node-sdk/          # Node.js 插件 SDK
│   ├── ocr-tool/          # OCR 文字识别（Python）
│   ├── responder-plugin/  # 权限拦截测试桩（Python）
│   ├── screenshot-plugin/ # 屏幕截图工具（Python）
│   ├── system-info/       # 系统信息工具（Node.js，跨语言插件示例）
│   ├── ui-demo/           # 插件 UI 声明系统演示（Python）
│   └── window-info/       # 窗口信息查询（Python）
├── src/                   # 前端资源
│   ├── index.html         # 主界面
│   ├── main.js            # 前端逻辑
│   ├── style.css          # 样式
│   └── overlay.js         # 截图覆盖层
├── docs/                  # 文档
│   ├── plugin-development.md  # 插件开发指南
│   └── superpowers/specs/     # 设计文档
```

## 快速开始

### 安装

从 [Gitee releases](https://gitee.com/buxiaju/in-tools/releases) 下载对应平台的安装包，或从源码编译：

```bash
cd src-tauri
cargo tauri build
```

### 使用

1. 启动 InTools，任务栏图标显示应用状态
2. 在左侧导航切换功能视图
3. 插件视图：管理插件的启用/禁用状态
4. AI 对话：与 AI 交互，AI 可自动调用插件工具
5. 权限视图：管理已授予的权限
6. 插件开发：查看开发文档
7. 设置视图：配置关闭行为、MCP 网关开关、日志级别、AI API 等

## 插件开发

### 插件结构

每个插件是一个包含 `manifest.toml` 描述文件的目录：

```
my-plugin/
├── manifest.toml        # 插件元数据与工具声明
├── main.py             # 入口脚本（或其他可执行文件）
└── README.md           # 使用说明（可选）
```

### manifest.toml 示例

```toml
[plugin]
id = "com.example.myplugin"
name = "我的插件"
version = "1.0.0"
author = "作者名"
description = "一句话描述"

[exec]
command = "python"
args = ["-u", "main.py"]

[[tools]]
name = "my:echo"
description = "原样返回传入的文本"
[tools.input_schema]
type = "object"
required = ["text"]
[tools.input_schema.properties.text]
type = "string"
description = "要回显的文本"

[capabilities]
permissions = ["file:read"]

[shortcut]
key = "Ctrl+Shift+H"
tool = "my:echo"
```

### 协议通信

插件与宿主通过标准输入输出（stdin/stdout）进行 JSON-RPC 2.0 通信：

```python
import sys
import json

def main():
    # 1. 主动握手
    print(json.dumps({"jsonrpc": "2.0", "method": "hello", "params": {"protocol_version": "1.0"}}))
    sys.stdout.flush()
    
    # 2. 主循环：读取请求并响应
    for line in sys.stdin:
        request = json.loads(line)
        # 处理 request["method"]，返回结果
```

详细协议规范请参考 [插件开发手册](./docs/plugin-development.md)。

### 现有插件参考

- **hello-plugin**（Python）：最简插件模板，演示基本结构
- **system-info**（Node.js）：系统信息工具，演示跨语言插件开发
- **file-hash**（Go）：文件哈希工具，演示编译型二进制即插即用
- **file-search**：文件搜索工具，演示参数处理与路径安全
- **screenshot-plugin**：屏幕截图，演示系统级 API 调用
- **ai-orchestrator**：AI 编排，演示多工具组合与 LLM 调用
- **clipboard-tool**：剪贴板读写工具
- **color-picker**：取色器，演示屏幕像素采样
- **ocr-tool**：OCR 文字识别工具
- **window-info**：窗口信息查询工具
- **ui-demo**：UI 声明系统演示，展示分组表单、颜色/密码/路径输入、kv/table/markdown 结果展示

### 多语言支持

InTools 的插件协议基于 JSON-RPC 2.0 over stdin/stdout，任何能读写管道的语言都可以做插件。仓库内置：

- **Python** 插件：hello-plugin、ai-orchestrator 等（手写协议）
- **Node.js** 插件：system-info（使用 `node-sdk/intools.js` SDK，零协议样板代码）
- **Go** 插件：file-hash（编译后单文件即插即用，无需运行时）

详见 [插件开发手册](./docs/plugin-development.md) 的多语言骨架章节。

## 配置说明

### 主配置

`~/.intools/host-config.json`：

```json
{
    "plugins_dir": null,
    "log_level": "info",
    "close_behavior": "minimize",
    "mcp_enabled": false,
    "mcp_token": ""
}
```

### 快捷键配置

`~/.intools/shortcuts.json`：

```json
{
    "hello-plugin": {
        "key": "Ctrl+Shift+H",
        "enabled": true
    }
}
```

## 权限模型

插件需在 `manifest.toml` 中声明所需权限：

| 类别 | 可用动作 | 说明 |
|-----|---------|------|
| file | read / write | 文件系统读写 |
| network | http / websocket / dns / socket / read / write / send / receive | 网络访问 |
| process | spawn | 启动子进程 |
| shell | exec | 通过 cmd / PowerShell / bash 执行命令 |
| screen | capture / record | 屏幕截图与录屏 |
| input | control | 模拟键鼠输入 |
| clipboard | read / write | 读写剪贴板 |
| audio | capture / record / send / receive | 麦克风与扬声器 |
| system | manage | 关机 / 重启 / 注销 / 锁屏 |
| window | manage / modify | 操纵其他窗口 |
| app | spawn | 启动其他桌面应用 |
| registry | read / write / modify | Windows 注册表读写 |
| credential | read / write | 凭据存储访问 |
| crypto | read / write | 密钥 / 证书访问 |
| notification | send | 发送系统通知 |
| hardware | read / write / control | 摄像头 / 蓝牙 / 串口等通用硬件 |
| persistence | install / uninstall / modify | 安装 / 卸载 / 自启动等持久化行为 |
| schedule | manage | 计划任务 / cron 表达式注册 |
| environment | read / write | 进程环境变量 |

用户首次使用需要高危权限的插件时会收到提示，可选择：
- **允许**：本次调用有效
- **始终允许**：写入持久化授权
- **拒绝**：本次调用被拦截

## 常见问题

**Q: 插件启动失败？**
A: 检查 `stderr` 日志（在设置中查看日志），确认 Python 环境正确、依赖已安装。

**Q: 快捷键不生效？**
A: 快捷键在插件启用后才会注册；检查是否有其他程序占用了该组合键。

**Q: 如何调试插件？**
A: 在插件 `main.py` 中使用 `print()` 输出日志到 `stderr`，或手动模拟协议交互。

## 构建与开发

### 环境依赖

- Rust 1.88+（zip crate MSRV 要求）
- Python 3.8+（用于开发 Python 插件）
- Node.js（可选，用于前端资源）

### 本地开发

```bash
# 克隆仓库
git clone https://gitee.com/buxiaju/in-tools.git
cd in-tools

# 启动开发服务器
cd src-tauri
cargo tauri dev
```

### 运行测试

```bash
cd src-tauri
cargo test --lib
```

## 协议与兼容性

当前插件宿主协议版本：**1.0**

主版本号不兼容时，插件将无法加载。次要版本差异向后兼容。

## 许可证

本项目采用 [MIT License](./LICENSE) 开源协议。

## 贡献

欢迎提交 Issue 与 Pull Request。开发插件时，请先阅读[插件开发手册](./docs/plugin-development.md)了解详细规范。
