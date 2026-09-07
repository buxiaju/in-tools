# InTools 架构文档

> 深入了解 InTools 的系统架构、设计理念和实现细节。

## 📋 目录

- [设计目标](#设计目标)
- [核心理念](#核心理念)
- [总体架构](#总体架构)
- [核心模块](#核心模块)
- [数据流](#数据流)
- [安全模型](#安全模型)
- [扩展机制](#扩展机制)

## 设计目标

InTools 是一款跨平台桌面插件宿主。宿主本身只负责加载、调度、权限管控和对外通信，所有具体功能由插件提供。

设计目标，按优先级排序：

1. **插件宿主好用、稳定、易于第三方开发插件**
2. **AI 能通过自然语言编排所有已加载插件**
3. **MCP 网关让第三方 AI 工具（Claude Desktop、Cursor 等）复用本机插件能力**
4. **体积小、启动快、内存占用低**

## 核心理念

### 一切皆插件

> 文件操作是插件，系统控制是插件，网络工具是插件，AI 编排也是插件。

**优势：**
- 灵活扩展：任何编程语言、任何能力都可以加入
- 安全隔离：插件是独立进程
- 易于维护：宿主不包含业务逻辑

**代价：**
- 需要协议层抽象
- 插件开发门槛
- 性能开销

## 总体架构

```
┌─────────────────────────────────────────────────────────┐
│  Tauri 窗口 / 系统托盘（极薄 UI）                       │
│  ┌────────────────────────────────────────────────────┐ │
│  │  Rust 内核                                         │ │
│  │  ├─ registry    插件注册表                         │ │
│  │  ├─ runtime     子进程运行时 + 调度                 │ │
│  │  ├─ permission  权限校验                            │ │
│  │  ├─ mcp         对外 MCP Server                    │ │
│  │  └─ config      配置持久化                          │ │
│  └────────────────────────────────────────────────────┘ │
└─────────────────────────────────────────────────────────┘
                ↓ stdio JSON-RPC 2.0
    ┌───────────┼───────────┬──────────────┐
    ▼           ▼           ▼              ▼
 ┌────────┐ ┌──────────┐ ┌──────────┐ ┌──────────┐
 │OCR 插件 │ │AI 编排插件│ │文件搜索  │ │更多插件… │
 │(任意语言)│ │(Python)  │ │(任意语言) │ │          │
 └────────┘ └──────────┘ └──────────┘ └──────────┘
```

## 核心模块

### 1. 协议层 (`protocol/`)

**职责：** 定义 JSON-RPC 2.0 消息格式、manifest 格式

**关键类型：**
- `JsonRpcRequest` / `JsonRpcResponse` / `JsonRpcNotification` / `JsonRpcError`
- `Manifest`：插件元数据
- `Permission`：权限声明
- `ToolDescriptor`：工具描述

**特点：**
- 纯数据，无 IO
- 完全可测试
- 协议版本协商

### 2. 注册表层 (`registry/`)

**职责：** 扫描插件目录，维护工具名到插件的反向索引

**关键类型：**
- `Registry`：插件注册表
- `ToolConflict`：工具冲突记录
- `LoadedPlugin`：已加载的插件

**特性：**
- 工具名冲突处理（先到先得）
- 插件分类（system / test / user）
- 动态发现

### 3. 运行时层 (`runtime/`)

**职责：** 插件进程的生命周期管理

**关键类型：**
- `PluginInstance`：单个插件实例状态机
- `Supervisor`：全局调度器
- `Transport`：传输抽象

**状态机：**
```
STOPPED → STARTING → IDLE ⇄ BUSY → STOPPING → STOPPED
              ↓                  ↓
            ERROR            (空闲超时)
```

### 4. 权限层 (`permission/`)

**职责：** 权限校验和授权记录

**三级危险度：**
- **低危**：自动放行（如 file:read, clipboard:read）
- **中危**：首次询问后记住（如 file:write, clipboard:write）
- **高危**：每会话首次询问（如 process:spawn, shell:exec）

**网络隔离：**
- 域名/IP/端口/协议白名单/黑名单
- 实时校验网络访问

### 5. MCP 网关 (`mcp/`)

**职责：** 提供 MCP（Model Context Protocol）服务

**特性：**
- Streamable HTTP 传输
- Bearer Token 鉴权
- 高危插件不暴露
- 完整审计日志

### 6. UI 层 (`src/`)

**职责：** 提供用户界面

**技术栈：**
- 原生 HTML/CSS/JS
- 无前端框架
- Tauri 桥接

## 数据流

### 用户调用工具

```
用户点击
    ↓
Tauri Command (commands.rs)
    ↓
ToolInvoker::call_tool
    ↓
权限校验
    ↓
网络访问检查（如需要）
    ↓
实例按需唤醒
    ↓
JSON-RPC 转发
    ↓
插件执行
    ↓
返回结果
```

### AI 编排调用

```
用户输入
    ↓
ai-orchestrator 插件
    ↓
host/listTools (获取工具列表)
    ↓
调用 LLM API
    ↓
LLM 决定调用工具
    ↓
host/callTool (调用工具)
    ↓
权限校验
    ↓
插件执行
    ↓
返回结果给 LLM
    ↓
生成最终回复
```

## 安全模型

### 多层防护

1. **协议层**：JSON-RPC 2.0 严格格式校验
2. **权限层**：三级危险度 + 网络隔离 + 资源限制
3. **运行时层**：进程隔离 + 资源监控
4. **审计层**：完整调用链追踪

### 调用方身份

```rust
pub enum CallerIdentity {
    Ui,                              // 用户界面
    Plugin { id: String, depth: u8 }, // 其他插件
    Mcp { client_name: String },      // MCP 客户端
}
```

不同身份有不同的权限和操作限制。

### 高危插件规则

凡 manifest 中声明了 `input:control` 或 `process:spawn` 的插件，其**全部**工具一律不对 MCP 暴露，即使用户勾选亦由 bridge 拦截。

### 网络隔离规则

- **默认允许**：manifest 声明的 network 权限
- **白名单优先**：如果设置了白名单，只允许白名单内的访问
- **黑名单优先**：黑名单优先级高于白名单

## 扩展机制

### 插件开发

支持的插件开发方式：

1. **Python SDK**：装饰器注册，零样板
2. **Node.js SDK**：装饰器注册，异步支持
3. **任意语言**：只要能读写 stdin/stdout

### 工具扩展

每个插件可以声明多个工具：

```toml
[[tools]]
name = "my:tool"
description = "我的工具"
[tools.input_schema]
type = "object"
properties = {}
```

### UI 扩展（第三期）

插件可以请求宿主弹出特定 UI：

```python
@plugin.ui_request("overlay", schema, "callback_method")
def show_overlay():
    pass
```

### 第三方集成

- **MCP 网关**：Claude Desktop、Cursor 等
- **AI 编排**：DeepSeek、OpenAI、Ollama 等
- **插件市场**：GitHub Pages + JSON API

## 性能考量

### 启动优化

- on-demand 模式：插件按需启动
- 空闲回收：自动停止空闲插件
- 工具缓存：避免重复查询

### 资源优化

- 进程隔离：崩溃不影响宿主
- 内存限制：防止恶意插件
- CPU 限制：避免资源争抢

## 未来演进

- **跨平台**：macOS、Linux 支持
- **插件签名**：安全验证
- **插件市场**：社区生态
- **OS 沙箱**：Windows AppContainer、macOS Sandbox

## 参考

- [设计文档](superpowers/specs/2026-09-02-intools-plugin-host-design.md)
- [实施计划](superpowers/specs/2026-09-02-intools-implementation-plan.md)
- [插件开发指南](plugin-development.md)
