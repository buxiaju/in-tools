

# InTools

InTools 是一个基于插件机制的桌面工具宿主程序，通过统一的协议层将各类工具（插件）的能力汇聚在一起，提供简洁的操作界面与权限管控。

## 核心特性

- **插件化架构**：任何可执行程序或脚本（如 Python、Node.js、二进制）均可作为插件接入
- **统一通信协议**：基于 JSON-RPC 的进程间通信，插件与宿主之间的交互清晰可预测
- **权限模型**：细粒度的权限控制，插件需声明所需权限，用户可选择授予或拒绝
- **全局快捷键**：支持为插件配置全局热键，脱离窗口焦点也可触发
- **AI 编排集成**：内置 AI 编排插件，可调用 LLM API 组合调用多种工具
- **MCP 网关**：提供 MCP（Machine Control Protocol）兼容接口，可被其他 MCP 客户端调用
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
│   ├── ai-orchestrator/   # AI 编排插件
│   ├── caller-plugin/     # 演示插件间调用
│   ├── file-search/       # 文件搜索工具
│   ├── hello-plugin/      # 入门示范插件
│   ├── responder-plugin/  # 响应式插件
│   └── screenshot-plugin/ # 屏幕截图工具
├── src/                   # 前端资源
│   ├── index.html         # 主界面
│   ├── main.js           # 前端逻辑
│   ├── style.css         # 样式
│   └── overlay.*         # 截图覆盖层
├── docs/                  # 文档
│   ├── plugin-development.md  # 插件开发指南
│   └── superpowers/specs/     # 设计文档
└── tools/                 # 构建工具（NSIS 等）
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
4. 工具视图：查看所有可用工具并直接调用
5. 权限视图：管理已授予的权限
6. 设置视图：配置日志级别、AI API 等

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
id = "my-plugin"
name = "我的插件"
version = "1.0.0"

[command]
type = "python"
args = ["main.py"]

[[tools]]
name = "hello"
description = "打招呼"
input_schema = { type = "object", properties = { name = { type = "string" } } }

[permissions]
declare = ["file.read"]

[shortcut]
key = "Ctrl+Shift+H"
description = "快速打招呼"
```

### 协议通信

插件与宿主通过标准输入输出（stdin/stdout）进行 JSON-RPC 通信：

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

- **hello-plugin**：最简插件模板，演示基本结构
- **file-search**：文件搜索工具，演示参数处理与路径安全
- **screenshot-plugin**：屏幕截图，演示系统级 API 调用
- **ai-orchestrator**：AI 编排，演示多工具组合与 LLM 调用

## 配置说明

### 主配置

`~/.config/in-tools/config.json`：

```json
{
    "plugins_dir": "~/.local/share/in-tools/plugins",
    "log_level": "info",
    "close_behavior": "minimize",
    "mcp_enabled": false
}
```

### 快捷键配置

`~/.config/in-tools/shortcuts.json`：

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

| 权限 | 说明 | 危险等级 |
|-----|------|---------|
| `file.read` | 读取文件系统 | 中 |
| `file.write` | 写入文件系统 | 高 |
| `process.spawn` | 启动外部进程 | 高 |
| `network` | 网络访问 | 高 |
| `ui.show` | 显示窗口 | 低 |

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

- Rust 1.70+
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
cargo test --workspace
```

## 协议与兼容性

当前版本协议版本：**1.0**

主版本号不兼容时，插件将无法加载。次要版本差异向后兼容。

## 许可证

本项目采用 [MIT License](./LICENSE) 开源协议。

## 贡献

欢迎提交 Issue 与 Pull Request。开发插件时，请先阅读[插件开发手册](./docs/plugin-development.md)了解详细规范。