# InTools 插件宿主设计文档

日期：2026-09-02
状态：已确认（待实施）

## 1. 项目定位

InTools 是一款跨平台桌面插件宿主。宿主本身只负责加载、调度、权限管控和对外通信，所有具体功能由插件提供。AI 编排能力本身也是一个插件；MCP 网关是宿主内置的对外接口。

设计目标，按优先级排序：

1. 插件宿主好用、稳定、易于第三方开发插件
2. AI 能通过自然语言编排所有已加载插件
3. MCP 网关让第三方 AI 工具（Claude Desktop、Cursor 等）复用本机插件能力
4. 体积小、启动快、内存占用低

非目标：不做插件市场、不做 OS 级沙箱、不在第一版建设插件生态。

## 2. 技术选型

| 层 | 选择 | 理由 |
|---|---|---|
| 内核 | Rust | 体积小、无运行时、内存安全 |
| 窗口/UI 外壳 | Tauri（系统 WebView） | 安装包 5-15MB，不打包浏览器内核 |
| 前端 | 原生 HTML/CSS/JS | 不引入框架，避免构建复杂度与体积膨胀 |
| 插件形态 | 独立子进程，语言不限 | 自由度最高，能直接调用系统 API |
| 插件通信 | JSON-RPC 2.0 over stdio（逐行 JSON） | 有标准规范，任何语言易于实现 |
| MCP 传输 | Streamable HTTP，绑定 127.0.0.1 | MCP 当前推荐的远程传输方式 |

架构选型时对比过事件总线架构和 gRPC 方案。事件总线增加路由与去重复杂度且与"按需唤醒"语义不契合；gRPC 要求插件作者掌握 protobuf 并携带 stub，与"快速开发插件"目标冲突。因此采用请求-响应式的 JSON-RPC。

## 3. 总体架构

```
┌─────────────────────────────────────────────────┐
│  Tauri 窗口 / 系统托盘（极薄 UI）                  │
│  ┌─────────────────────────────────────────────┐│
│  │  Rust 内核                                   ││
│  │  ├─ registry    插件注册表                    ││
│  │  ├─ runtime     子进程运行时 + 调度            ││
│  │  ├─ permission  权限校验                      ││
│  │  ├─ mcp         对外 MCP Server               ││
│  │  └─ config      配置持久化                    ││
│  └────────────┬────────────────────────────────┘│
└───────────────┼──────────────────────────────────┘
                │ stdio JSON-RPC 2.0
    ┌───────────┼───────────┬──────────────┐
    ▼           ▼           ▼              ▼
 ┌────────┐ ┌──────────┐ ┌──────────┐ ┌──────────┐
 │OCR 插件 │ │AI 编排插件│ │文件搜索  │ │更多插件… │
 │(任意语言)│ │(Python)  │ │(任意语言) │ │          │
 └────────┘ └──────────┘ └──────────┘ └──────────┘
```

内核不包含任何具体业务功能，也不包含任何平台特定的系统能力实现。

## 4. 插件包结构

插件是插件目录下的一个子目录：

```
~/.intools/plugins/
├── ocr-plugin/
│   ├── manifest.toml   # 必选
│   ├── main.py         # 入口，语言不限
│   └── icon.png        # 可选
└── file-search/
    ├── manifest.toml
    └── main.py
```

### 4.1 manifest.toml

```toml
[plugin]
id = "com.example.ocr"
name = "OCR 识别"
version = "1.0.0"
author = "Example"
description = "屏幕截图并识别文字"

[exec]
command = "python"
args = ["-u", "main.py"]
# 亦可为 command = "node", args = ["main.js"]
# 或编译好的二进制 command = "./ocr-server"

[[tools]]
name = "ocr:recognize"
description = "识别图片中的文字，返回文本内容"
[tools.input_schema]
type = "object"
required = ["image_path"]
[tools.input_schema.properties.image_path]
type = "string"
description = "图片文件的绝对路径"
[tools.input_schema.properties.lang]
type = "string"
description = "识别语言，默认 zh-CN"

[capabilities]
permissions = ["screen:capture", "file:read"]

[lifecycle]
mode = "on-demand"        # on-demand | background | startup
idle_timeout_sec = 300
restart_policy = "on-failure"   # never | on-failure | always
request_timeout_sec = 30
```

工具的 `input_schema` 是标准 JSON Schema，可被 LLM function calling 和 MCP 直接复用。

### 4.2 工具描述的两级设计

on-demand 插件平时处于停止状态，但 AI 与 MCP 客户端随时可能查询可用工具。若为此启动所有插件，将违背按需运行目标。因此采用两级描述：

- **静态层**：manifest 中的 `[[tools]]`，宿主无需启动插件即可读取
- **运行时层**：插件启动后宿主调用一次 `tools/list`，允许插件动态增减工具

运行时结果缓存至 `~/.intools/cache/tools.json`。冷启动时 `tools/list` 查询直接由缓存回答，不唤醒任何插件；仅在实际 `tools/call` 时才唤醒目标插件。

## 5. 通信协议

宿主与插件通过 stdin/stdout 交换 JSON-RPC 2.0 消息，每条消息一行，以 `\n` 结尾。

### 5.1 宿主 → 插件

```json
{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}
{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"ocr:recognize","arguments":{"image_path":"/tmp/shot.png"}}}
{"jsonrpc":"2.0","id":3,"method":"plugin/shutdown","params":{}}
```

### 5.2 插件 → 宿主（响应）

```json
{"jsonrpc":"2.0","id":2,"result":{"text":"你好世界","confidence":0.98}}
{"jsonrpc":"2.0","id":2,"error":{"code":-32000,"message":"图片文件不存在"}}
```

### 5.3 插件 → 宿主（反向 RPC）

插件可主动向宿主发起请求，这是 AI 编排插件的基础：

| 方法 | 用途 |
|---|---|
| `host/listTools` | 获取当前所有可用工具及其 Schema |
| `host/callTool` | 调用其他插件的工具 |
| `host/getConfig` | 读取本插件的配置项 |
| `host/setConfig` | 写入本插件的配置项 |
| `host/notify` | 向 UI 推送消息 |

宿主处理 `host/callTool` 时会重新执行完整权限校验，调用者身份为发起插件，因此 AI 编排插件不构成权限绕过通道。同时施加调用链深度限制（默认最大 5 层），防止插件间递归调用。

### 5.4 插件 → 宿主（通知）

```json
{"jsonrpc":"2.0","method":"notify/progress","params":{"task_id":"abc","progress":50}}
{"jsonrpc":"2.0","method":"notify/stream","params":{"session_id":"s1","delta":"正在识别"}}
{"jsonrpc":"2.0","method":"notify/toolCall","params":{"session_id":"s1","tool":"ocr:recognize","status":"running"}}
{"jsonrpc":"2.0","method":"notify/streamEnd","params":{"session_id":"s1"}}
```

宿主将这些通知通过 Tauri 事件系统转发给前端，使 AI 的工具调用过程对用户可见。

### 5.5 启动握手

```
宿主                         插件
  │                           │
  ├── spawn subprocess ──────►│
  │◄── plugin/hello ─────────┤   {method:"plugin/hello", params:{protocol_version, tools}}
  ├── plugin/ready ──────────►│   {method:"plugin/ready", params:{config, plugin_dir}}
  │◄── result ───────────────┤
  │   ◄── 正常 RPC 通信 ────► │
```

握手 10 秒内未收到 `plugin/hello` 视为启动失败，插件标记为 ERROR 并在 UI 显示原因。

`protocol_version` 采用 `MAJOR.MINOR` 形式，第一版为 `1.0`。版本协商规则：MAJOR 不一致时拒绝加载并在 UI 提示需升级插件或宿主；MAJOR 一致而插件 MINOR 高于宿主时正常加载，插件应自行降级使用宿主已支持的方法。

权限声明的粒度为**插件级**：`[capabilities] permissions` 中的权限适用于该插件的所有工具，宿主不区分单个工具需要哪些权限。此选择使 manifest 更简洁，代价是权限范围偏粗，插件作者应通过拆分插件来收窄权限面。

### 5.6 生命周期状态机

```
STOPPED ──→ STARTING ──→ IDLE ⇄ BUSY ──→ STOPPING ──→ STOPPED
               │           │
            (失败)     (空闲超时)
               ▼           ▼
             ERROR      STOPPING
```

- `on-demand`：无请求时 STOPPED，有请求时唤醒；空闲超时后自动停止
- `background`：常驻 IDLE，适合文件监听等场景
- `startup`：宿主启动时自动拉起，适合 AI 编排插件

## 6. 内核模块划分

```
src-tauri/src/
├── main.rs              # Tauri 入口，装配各模块
├── protocol/            # 协议层（纯数据，无 IO）
│   ├── message.rs       # JSON-RPC 消息序列化与解析
│   └── manifest.rs      # manifest.toml 解析与校验
├── registry/            # 插件注册表
│   ├── mod.rs           # manifest 加载、工具名 → 插件 ID 索引
│   └── discovery.rs     # 目录扫描与热更新
├── runtime/             # 插件运行时（唯一接触子进程的模块）
│   ├── process.rs       # 子进程 spawn / kill / stdio
│   ├── transport.rs     # 逐行 JSON 编解码 + 请求-响应配对
│   ├── instance.rs      # 单实例状态机
│   └── supervisor.rs    # 全局调度：唤醒、回收、重启
├── permission/mod.rs    # 权限校验与授权记录
├── mcp/                 # 对外 MCP Server
│   ├── server.rs        # Streamable HTTP 端点
│   └── bridge.rs        # 插件工具 → MCP tool 映射
├── config/mod.rs        # 配置持久化
└── commands.rs          # Tauri command，供前端调用
```

模块边界的核心约束：`runtime` 是唯一接触子进程的模块；`protocol` 是纯函数式的，可完全离线测试；`registry` 只处理 manifest 数据结构，不感知进程。

### 6.1 核心抽象

```rust
#[async_trait]
pub trait Transport: Send + Sync {
    async fn send(&mut self, msg: OutgoingMessage) -> Result<(), TransportError>;
    async fn recv(&mut self) -> Result<IncomingMessage, TransportError>;
}

pub struct StdioTransport { /* 真实子进程 */ }
pub struct MockTransport { /* 内存管道，测试用 */ }
```

```rust
pub trait ToolInvoker: Send + Sync {
    async fn call_tool(
        &self,
        tool_name: &str,
        args: serde_json::Value,
        caller: CallerIdentity,
    ) -> Result<serde_json::Value, InvokeError>;

    async fn list_tools(&self, caller: CallerIdentity) -> Vec<ToolDescriptor>;
}

pub enum CallerIdentity {
    Ui,
    Plugin { id: String, depth: u8 },
    Mcp { client_name: String },
}
```

`Supervisor` 实现 `ToolInvoker`。UI、AI 编排插件、外部 MCP 客户端三条调用路径全部经由此接口，差别仅在 `CallerIdentity`。任何路径都无法绕过权限校验。

### 6.2 统一调用链路

```
   UI 点击          AI 编排插件         外部 MCP 客户端
      │            host/callTool          POST /mcp
      └─────────────────┼────────────────────┘
                        ▼
               ToolInvoker::call_tool
               ┌────────────────────┐
               │ 1. 工具名解析        │
               │ 2. 权限校验（按身份） │
               │ 3. 调用深度检查      │
               │ 4. 实例按需唤醒      │
               │ 5. RPC 转发 + 超时   │
               │ 6. 审计日志          │
               └────────────────────┘
                        ▼
                   插件子进程
```

## 7. 权限模型

采用声明 + 首次使用时授权的模式，而非安装时一次性授予。

| 等级 | 示例权限 | 处理方式 |
|---|---|---|
| 低危 | `file:read`（限定目录）、`network:http` | manifest 声明即可，不询问 |
| 中危 | `screen:capture`、`clipboard:write`、`file:write` | 首次调用时询问，可记住选择 |
| 高危 | `input:control`、`process:spawn`、`file:write:*` | 每会话首次调用询问，可选本次允许 / 永久允许 |

授权记录持久化至 `~/.intools/permissions.json`：

```json
{
  "com.example.ocr": {
    "screen:capture": { "granted": true, "scope": "always", "at": "2026-09-02T10:00:00Z" },
    "file:read": { "granted": true, "scope": "always", "paths": ["~/Pictures"] }
  }
}
```

### 7.1 安全模型的边界

必须明确说明本模型的限制：在子进程架构下，插件进程拥有宿主用户的完整系统权限，宿主无法在操作系统层面阻止恶意插件直接访问文件或网络。因此本权限模型实际提供的保障是：

1. **透明化**：用户清楚知晓插件声明要做什么
2. **管控宿主提供的能力**：插件经 `host/*` 反向 RPC 获得的能力受严格约束
3. **可审计**：所有工具调用记入日志，可回溯

真正的隔离需要 OS 级机制（Windows AppContainer、macOS Sandbox、Linux namespaces），列为后续增强项，不在第一版范围内。如实记录此限制优于宣称具备沙箱能力。

## 8. 故障处理

| 场景 | 处理 |
|---|---|
| 插件进程崩溃 | 将所有待处理请求标记失败并返回错误；按 `restart_policy` 决定是否重启，指数退避 1s/2s/4s/8s/16s，最多 5 次 |
| 请求超时 | 默认 30 秒（manifest 可覆盖），超时返回错误但不终止进程 |
| 启动握手超时 | 10 秒未收到 `plugin/hello` 判定失败，标记 ERROR |
| 非法 JSON 输出 | 丢弃该行并记入日志，不影响后续消息解析 |
| 插件 stderr | 全量捕获至 `~/.intools/logs/<plugin-id>.log`，便于插件作者调试 |
| 调用链过深 | 超过 5 层返回错误，防止插件间递归 |

## 9. AI 编排插件

AI 编排是一个普通插件，不提供具体能力，而是消费其他插件的能力。

```
用户输入："把屏幕上的文字识别出来，然后翻译成英文"
    ▼
ai-orchestrator 插件
  1. host/listTools     → 获取所有可用工具及 Schema
  2. 转换为 LLM function calling 格式
  3. 调用 LLM（OpenAI 兼容 API / DeepSeek / Ollama）
  4. LLM 决定调用 ocr:screenshot
  5. host/callTool      → 宿主执行，返回图片路径
  6. LLM 决定调用 ocr:recognize
  7. host/callTool      → 返回识别文本
  8. LLM 决定调用 translate:text
  9. host/callTool      → 返回英文
 10. 经 notify/stream 将最终回复推送至 UI
```

将 LLM 逻辑置于插件而非内核，带来四点收益：

1. 更换模型、调整 prompt、引入 ReAct 等策略无需重新编译内核
2. 用户可同时安装多个编排插件（如一个云端模型、一个本地 Ollama），互不干扰
3. 内核无需引入 HTTP 客户端、tokenizer、SSE 解析等依赖，体积保持最小
4. 插件以 Python 实现，可直接使用成熟 LLM SDK

内核为 AI 提供的唯一支持是 `host/listTools` 与 `host/callTool`，二者本身即为通用能力。

## 10. MCP 网关

MCP 协议同样基于 JSON-RPC 2.0，核心方法亦为 `tools/list` 与 `tools/call`。本项目的插件协议刻意对齐该命名，因此网关基本是透传。

```
┌──────────────────┐   MCP over        ┌────────────────────┐
│  Claude Desktop  │   Streamable HTTP │  mcp/server (axum) │
│  Cursor / Cline  │◄─────────────────►│         ▼          │
│  其他 MCP 客户端  │  127.0.0.1:7801   │  mcp/bridge        │
└──────────────────┘   /mcp             │         ▼          │
                                        │  ToolInvoker       │
                                        └────────────────────┘
```

bridge 的职责：

1. **命名转换**：`ocr:recognize` → `ocr_recognize`（MCP 工具名不允许冒号）
2. **Schema 复用**：manifest 中的 `input_schema` 原样输出
3. **身份标记**：`CallerIdentity::Mcp { client_name }`，走同一套权限校验
4. **白名单过滤**：仅暴露用户显式勾选的插件

### 10.1 MCP 安全约束

对外开放端口是本项目最大风险点，因此：

- **默认关闭**：MCP Server 默认不启动，需用户在设置中显式开启
- **仅本机监听**：硬编码绑定 `127.0.0.1`，不提供改为 `0.0.0.0` 的选项
- **Bearer Token 鉴权**：开启时生成随机 token，客户端须在 `Authorization` 头携带；UI 提供可复制的客户端配置片段
- **默认零暴露**：新装插件不会自动出现在 MCP 工具列表，须用户逐个勾选
- **高危权限不外放**：由于权限为插件级声明，凡 manifest 中声明了 `input:control` 或 `process:spawn` 的插件，其**全部**工具一律不对 MCP 暴露，即使用户勾选亦由 bridge 拦截。外部 AI 直接控制键鼠风险过高，第一版一律禁止。此规则的代价是：一个既能截屏又能模拟键鼠的插件将完全无法用于 MCP，插件作者若希望部分能力可被外部 AI 使用，应将高危能力拆分为独立插件
- **完整审计**：所有 MCP 调用记录调用方、工具名、参数摘要、时间戳，写入 `~/.intools/logs/mcp-audit.log`

## 11. 前端界面

前端使用原生 HTML/CSS/JS，仅通过 Tauri command 与 event 同内核通信，不承载业务逻辑。

| 界面 | 职责 |
|---|---|
| 插件列表 | 显示已装插件、运行状态、启停开关、卸载 |
| AI 对话 | 输入框、流式回复区、工具调用过程可视化 |
| 权限管理 | 查看与撤销已授权项，勾选对 MCP 暴露的插件 |
| 设置 | 插件目录、MCP 开关与 Token、日志级别 |

UI 中的任何操作最终均落至 `ToolInvoker`，UI 不是特权路径。

## 12. 系统能力归属

跨平台系统能力（截屏、模拟键鼠、文件索引）全部由插件自行实现，内核不提供 `host/screenshot` 之类的系统接口。

- 收益：内核不引入任何平台特定依赖，无需处理三平台差异，体积与维护成本最小
- 代价：插件作者需自行处理跨平台。Python 生态下 `mss`、`pyautogui` 等库已可较好覆盖

该决策符合"微内核 + 一切皆插件"定位，是体积目标的前提。

## 13. 测试策略

`Transport` 被抽象为 trait，使约九成逻辑无需真实子进程即可测试。

**单元测试（不启动进程）**

- `protocol`：JSON-RPC 编解码、粘包与半包处理、非法 JSON 容错；manifest 解析与校验（缺字段、版本号非法、工具名冲突）
- `registry`：目录扫描、工具名到插件 ID 的索引、重复工具名冲突检测
- `permission`：三级权限判定矩阵、授权记录读写、不同 `CallerIdentity` 的判定差异
- `runtime/instance`：以 `MockTransport` 驱动状态机，覆盖握手成功、握手超时、进程崩溃、空闲回收的状态迁移
- `mcp/bridge`：工具名转换、Schema 映射、白名单过滤、高危权限拦截

**集成测试（启动真实子进程）**

- 以约 30 行的 Python echo 测试插件验证完整链路：spawn → 握手 → 调用 → 返回 → 空闲回收
- 崩溃恢复：测试插件收到特定指令后以非零码退出，验证待处理请求正确失败且按策略重启
- 反向 RPC：测试插件 A 经 `host/callTool` 调用测试插件 B，验证权限校验与调用深度限制生效
- MCP 端到端：启动 Server，以 HTTP 客户端执行 `tools/list` 与 `tools/call`，验证 Token 鉴权与白名单

## 14. MVP 范围

**内核**

- manifest 解析与校验
- 插件发现与注册表
- 子进程运行时与 JSON-RPC transport
- 三种生命周期模式、空闲回收、崩溃重启
- `ToolInvoker` 统一入口、权限校验、审计日志
- 反向 RPC：`host/listTools`、`host/callTool`、`host/getConfig`、`host/setConfig`、`host/notify`
- 工具 Schema 缓存
- MCP Server：Streamable HTTP、Token 鉴权、白名单
- 四个前端界面

**示范插件（Python 实现，同时充当协议文档）**

- `hello-plugin`：最小骨架，约 30 行，作为插件作者模板
- `ai-orchestrator`：AI 编排，对接 OpenAI 兼容 API（含 DeepSeek、Ollama）
- `file-search`：本地文件名搜索，验证一项真实可用能力

**明确推后**

- 插件市场与在线安装（第一版手动放置目录）
- OS 级沙箱隔离
- OCR、音乐、视频、网页查询插件
- 插件签名与来源验证
- 插件间事件总线（当前仅有请求-响应）
- 多语言 SDK（第一版仅提供 Python 参考实现与协议文档）

**验收标准**

安装 `ai-orchestrator` 与 `file-search` 后，在对话框输入"帮我找找桌面上有没有叫报告的文件"，AI 能自动调用 `file-search` 并给出结果；同时 Claude Desktop 连接 MCP 后可直接使用 `file_search` 工具。此条通过即视为平台成立。
