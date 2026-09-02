# InTools 实施计划

日期：2026-09-02
对应设计：`2026-09-02-intools-plugin-host-design.md`

## 计划原则

1. **依赖倒序推进**：先纯逻辑（无 IO）、再单进程 IO、再多进程协作、最后 UI 与对外接口
2. **每阶段可独立验证**：每个阶段结束都有明确的验证命令与通过标准，不允许出现"写完但跑不起来"的中间态
3. **测试先于依赖它的阶段**：`Transport` trait 与 `MockTransport` 在真实子进程之前完成，使后续状态机逻辑可离线测试
4. **示范插件最后做**：协议稳定后再写插件，避免协议变更导致插件反复重写

## 前置条件

本机工具链现状（已核查）：

| 工具 | 状态 |
|---|---|
| Node.js | v24.19.0 ✅ |
| npm | 11.17.0 ✅ |
| Python | 3.13.15 ✅ |
| Rust / Cargo | **未安装 ❌** |

**Phase 0 必须先解决 Rust 工具链**。Windows 下 Tauri 另需：

- MSVC 构建工具（Visual Studio Build Tools，含 "Desktop development with C++"）
- WebView2 Runtime（Windows 10/11 通常已预装）

## 阶段划分

### Phase 0：环境与项目骨架

**目标**：`cargo test` 与 `cargo tauri dev` 均能跑通空项目。

任务：
1. 安装 Rust 稳定版工具链（rustup）
2. 确认 MSVC 构建工具可用
3. 创建 Tauri 2.x 项目骨架，前端为原生 HTML/CSS/JS（不使用框架模板）
4. 建立目录结构：`src-tauri/src/{protocol,registry,runtime,permission,mcp,config}/`
5. 添加依赖：`serde`、`serde_json`、`toml`、`tokio`、`async-trait`、`thiserror`、`tracing`（`axum` 在 Phase 8 需要时再加，避免过早引入 HTTP 栈）
6. 配置 `.gitignore`（`target/`、`node_modules/`、`dist/`）

**验证**：`cargo test`（0 个测试通过）、`cargo clippy -- -D warnings` 无告警、`cargo tauri dev` 能弹出空窗口。

---

### Phase 1：协议层（纯逻辑，无 IO）

**目标**：JSON-RPC 消息与 manifest 的解析、校验、序列化全部可用且被测试覆盖。

任务：
1. `protocol/message.rs`
   - `JsonRpcRequest` / `JsonRpcResponse` / `JsonRpcNotification` / `JsonRpcError` 类型
   - `IncomingMessage` / `OutgoingMessage` 枚举（区分请求、响应、通知）
   - 逐行编解码：从字节缓冲切分完整行并解析，处理粘包与半包
   - 非法 JSON 行返回可识别错误而非 panic
2. `protocol/manifest.rs`
   - `Manifest` 结构：`plugin` / `exec` / `tools` / `capabilities` / `lifecycle`
   - `LifecycleMode`（on-demand / background / startup）、`RestartPolicy` 枚举
   - `Permission` 类型与三级危险度分类（低危 / 中危 / 高危）
   - 校验规则：必填字段、`id` 格式（反向域名）、`version` 语义化版本、工具名非空且插件内不重复、`input_schema` 为合法 JSON Schema 对象
   - `protocol_version` 协商函数：MAJOR 不一致则拒绝

**验证**：`cargo test protocol::` 全绿。必须覆盖：正常解析、缺必填字段、非法版本号、插件内工具名重复、非法 JSON 容错、粘包（一次读入多行）、半包（一行被截断）、MAJOR 版本不匹配。

---

### Phase 2：插件注册表

**目标**：扫描插件目录，建立工具名到插件的索引。

任务：
1. `registry/discovery.rs`：扫描插件根目录，读取每个子目录的 `manifest.toml`，解析失败的插件记为 ERROR 但不影响其他插件加载
2. `registry/mod.rs`
   - `Registry`：`plugin_id → Manifest` 映射
   - 工具名 → `plugin_id` 反向索引
   - **跨插件工具名冲突处理**：两个插件声明同名工具时，后加载者的该工具被拒绝并记录冲突原因（保持先到先得，避免行为不确定）
   - 查询接口：`get_plugin`、`resolve_tool`、`list_all_tools`
3. `config/mod.rs`：全局配置与插件配置的 JSON 读写，路径 `~/.intools/`（Windows 为 `%USERPROFILE%\.intools\`）

**验证**：`cargo test registry:: config::` 全绿。用临时目录构造测试夹具，覆盖：正常扫描多插件、单个 manifest 损坏不影响其余、跨插件工具名冲突、空目录、目录不存在。

---

### Phase 3：Transport 抽象与实例状态机（仍不启动真实进程）

**目标**：插件生命周期状态机完全可离线测试。

任务：
1. `runtime/transport.rs`
   - `Transport` trait（`send` / `recv`）
   - `MockTransport`：内存队列实现，可预置插件应答、可注入连接断开
   - 请求-响应配对：递增请求 ID、待响应表、按 ID 唤醒等待方
2. `runtime/instance.rs`
   - 状态枚举：`Stopped` / `Starting` / `Idle` / `Busy` / `Stopping` / `Error`
   - 握手流程：等待 `plugin/hello` → 版本协商 → 发送 `plugin/ready` → 进入 Idle
   - 握手超时（10s）→ Error
   - 请求超时（默认 30s，manifest 可覆盖）→ 返回错误但不改变实例状态
   - 空闲计时：Idle 持续超过 `idle_timeout_sec` 触发停止（仅 on-demand）
   - 连接断开：所有待响应请求以错误结束

**验证**：`cargo test runtime::instance` 全绿，使用 `tokio::time::pause()` 控制虚拟时钟，测试瞬间完成。覆盖：握手成功、握手超时、请求正常往返、请求超时、传输中断导致待响应请求全部失败、空闲超时触发停止、background 模式不因空闲而停止。

---

### Phase 4：真实子进程与首个示范插件

**目标**：宿主能真正拉起一个插件进程并完成一次调用。

任务：
1. `runtime/process.rs`
   - 子进程 spawn，`cwd` 设为插件目录，接管 stdin/stdout/stderr
   - `StdioTransport`：实现 `Transport`
   - stderr 独立任务全量转存 `~/.intools/logs/<plugin-id>.log`
   - 优雅关闭：先发 `plugin/shutdown`，超时未退出则强杀
   - **Windows 特别处理**：确保子进程随宿主退出而终止（避免残留进程），使用 Job Object 或退出时显式清理
2. `plugins/hello-plugin/`：Python 实现的最小插件（含 `manifest.toml` + `main.py`），实现握手、`tools/list`、一个 `hello:echo` 工具
3. 集成测试：真实启动 `hello-plugin`，完成握手 → 调用 → 关闭

**验证**：`cargo test --test integration_process`。覆盖：spawn 成功并握手、调用 `hello:echo` 返回预期结果、优雅关闭、可执行文件不存在时报错清晰、进程崩溃（插件按指令非零退出）后待响应请求失败。

---

### Phase 5：权限层与统一调用入口

**目标**：`ToolInvoker` 成为唯一调用通道，权限校验无法绕过。

任务：
1. `permission/mod.rs`
   - `CallerIdentity` 枚举（`Ui` / `Plugin{id,depth}` / `Mcp{client_name}`）
   - 三级判定：低危直接通过；中危首次询问后可记住；高危每会话首次询问
   - 授权记录持久化 `~/.intools/permissions.json`
   - 授权询问抽象为 trait（`PermissionPrompter`），测试实现可预设"总是同意 / 总是拒绝"，UI 实现走 Tauri 弹窗
2. `runtime/supervisor.rs`
   - 管理全部插件实例，实现 `ToolInvoker`
   - `call_tool` 六步：工具名解析 → 权限校验 → 调用深度检查（>5 拒绝）→ 按需唤醒 → RPC 转发+超时 → 审计日志
   - `list_tools`：优先读缓存 `~/.intools/cache/tools.json`，不唤醒插件
   - **缓存写入时机**：插件每次握手成功后调用其 `tools/list`，用运行时返回覆盖该插件在缓存中的条目；插件被卸载时删除其条目。缓存缺失时回退到 manifest 静态声明
   - 崩溃重启：按 `restart_policy`，指数退避 1/2/4/8/16 秒，上限 5 次
   - `startup` 模式插件在初始化时拉起，`background` 模式常驻
3. 审计日志：所有调用记录调用方、工具、参数摘要、耗时、结果状态

**验证**：`cargo test permission:: runtime::supervisor`。覆盖：三级权限各自的判定路径、拒绝时调用失败、授权记录持久化往返、调用深度超限被拒、缓存命中时不唤醒插件、崩溃后按策略重启且退避递增、`never` 策略不重启。

---

### Phase 6：反向 RPC

**目标**：插件能调用其他插件，且权限不被绕过。

任务：
1. 在实例消息循环中处理插件发来的请求：`host/listTools`、`host/callTool`、`host/getConfig`、`host/setConfig`、`host/notify`
2. `host/callTool` 转交 `Supervisor::call_tool`，`CallerIdentity` 为 `Plugin{id, depth+1}`
3. `notify/*` 通知经 Tauri 事件系统转发前端（此阶段先转发到日志，Phase 7 接 UI）
4. 集成测试用两个测试插件：A 通过 `host/callTool` 调用 B

**验证**：`cargo test --test integration_reverse_rpc`。覆盖：A 成功调用 B、A 调用无权限工具被拒、递归调用在第 6 层被拒、`host/getConfig`/`setConfig` 往返、插件请求未知 `host/*` 方法返回标准 JSON-RPC "method not found"。

---

### Phase 7：前端界面

**目标**：四个界面可用，UI 操作全部经由 `ToolInvoker`。

任务：
1. `commands.rs`：Tauri command —— 列插件、启停插件、列工具、调用工具、读写配置、权限查询与撤销、MCP 开关
2. 前端四个页面（原生 HTML/CSS/JS，单页 + 简单路由）
   - 插件列表：状态、启停、卸载
   - AI 对话：输入框、流式回复区、工具调用过程可视化
   - 权限管理：已授权项查看与撤销、MCP 暴露勾选
   - 设置：插件目录、MCP 开关与 Token、日志级别
3. 权限询问弹窗：实现 `PermissionPrompter`
4. `notify/stream` 等通知经 Tauri event 推送前端渲染

**验证**：`cargo tauri dev` 手动走查——`hello-plugin` 可见且状态正确、能手动调用 `hello:echo` 看到结果、触发中危权限时弹窗出现且拒绝后调用失败、撤销授权后再次调用重新询问。

---

### Phase 8：MCP 网关

**目标**：外部 MCP 客户端能列出并调用被授权暴露的工具。

任务：
1. `mcp/bridge.rs`
   - 工具名转换 `ocr:recognize` → `ocr_recognize`。**反向解析不做字符串还原**，而是建立正向映射表（暴露工具时生成 `mcp_name → 原始工具名` 字典），`tools/call` 时查表，从根本上消除歧义；若两个原始名映射到同一 `mcp_name`，后者不予暴露并记录冲突
   - Schema 原样透出
   - 白名单过滤（默认零暴露）。存储格式在 Phase 5 的 `config` 中定义为 `~/.intools/mcp-exposure.json`，形如 `{"exposed": ["plugin.id:tool"]}`，Phase 7 UI 写、Phase 8 网关读
   - 高危插件拦截：声明 `input:control` 或 `process:spawn` 的插件其全部工具不暴露
2. `mcp/server.rs`
   - axum 起 Streamable HTTP 端点 `/mcp`，硬编码绑定 `127.0.0.1:7801`
   - MCP `initialize` / `tools/list` / `tools/call` 方法
   - Bearer Token 鉴权，首次开启生成随机 token
   - 审计日志 `~/.intools/logs/mcp-audit.log`
3. 设置页展示可复制的客户端配置片段
4. 端口占用时向上报错并在 UI 提示

**验证**：`cargo test mcp::` + `cargo test --test integration_mcp`。覆盖：无 token 返回 401、错误 token 返回 401、正确 token 可 `tools/list`、未勾选插件不出现在列表、勾选后出现、`tools/call` 能真实调用插件、高危插件即使勾选也不暴露、映射表能把 `mcp_name` 正确解析回原始工具名、映射冲突时后者被排除。手动验证：Claude Desktop 配置后能看到并调用工具。

---

### Phase 9：file-search 插件

**目标**：交付第一个真实有用的能力，验证协议对第三方作者足够易用。

任务：
1. `plugins/file-search/`（Python，仅用标准库以免依赖问题）
   - `search:files` 工具：按文件名关键词搜索指定目录
   - 参数：`keyword`、`directory`（默认用户主目录）、`max_results`（默认 50）
   - 声明权限 `file:read`
   - `on-demand` 模式，空闲 300 秒回收
2. 处理跨平台路径与非 UTF-8 文件名
3. 编写插件开发文档 `docs/plugin-development.md`：协议说明、manifest 字段表、Python 骨架、调试方法

**验证**：从 UI 手动调用搜索桌面文件返回正确结果；经 MCP 调用同样成功；插件在空闲后自动停止并在下次调用时被唤醒。

---

### Phase 10：AI 编排插件与端到端验收

**目标**：达成设计文档的验收标准。

任务：
1. `plugins/ai-orchestrator/`（Python）
   - `startup` 模式常驻
   - 启动时 `host/listTools` 获取工具并转为 OpenAI function calling 格式
   - 对接 OpenAI 兼容 API，base_url 与 api_key 经 `host/getConfig` 读取（支持 DeepSeek、Ollama）
   - 多轮工具调用循环，上限 10 轮防失控
   - 经 `notify/stream` / `notify/toolCall` / `notify/streamEnd` 推送过程与结果
   - `ai:chat` 工具作为入口
   - 错误处理：API 不可达、密钥无效、工具调用失败均返回用户可读提示而非崩溃
2. 前端 AI 对话页接通
3. 端到端验收

**验证（设计文档验收标准）**：
- 对话框输入"帮我找找桌面上有没有叫报告的文件"，AI 自动调用 `file-search` 并给出结果，UI 显示工具调用过程
- Claude Desktop 连接 MCP 后可直接使用 `file_search` 工具
- 全量 `cargo test` 通过、`cargo clippy -- -D warnings` 无告警
- 构建产物体积检查：`cargo tauri build` 后确认安装包在预期量级（目标 5-15MB）

---

## 阶段依赖图

```
Phase 0 环境骨架
   │
Phase 1 protocol（纯逻辑）
   │
   ├─────────────┐
   ▼             ▼
Phase 2       Phase 3
registry      transport+instance（MockTransport）
   │             │
   └──────┬──────┘
          ▼
     Phase 4 真实子进程 + hello-plugin
          ▼
     Phase 5 permission + Supervisor(ToolInvoker)
          ▼
     Phase 6 反向 RPC
          │
   ┌──────┴──────┐
   ▼             ▼
Phase 7 UI    Phase 8 MCP
   └──────┬──────┘
          ▼
     Phase 9 file-search
          ▼
     Phase 10 ai-orchestrator + 端到端验收
```

## 风险与应对

| 风险 | 影响 | 应对 |
|---|---|---|
| Rust 工具链与 MSVC 安装受阻 | 阻塞全部阶段 | Phase 0 优先解决；若 MSVC 不可用可尝试 GNU 工具链，但 Tauri 官方推荐 MSVC |
| Windows 子进程残留 | 宿主退出后插件进程仍在 | Phase 4 明确用 Job Object 处理，并加集成测试断言 |
| Tauri 2.x API 与文档版本漂移 | 前端集成返工 | Phase 0 固定 Tauri 版本号，不使用 `latest` |
| MCP 规范演进 | 网关兼容性 | bridge 与 server 分离，协议细节集中在 server 层便于替换 |
| Python 3.13 第三方库兼容 | 插件无法运行 | 示范插件优先使用标准库；`ai-orchestrator` 仅需 HTTP 客户端，可用标准库 `urllib` 兜底 |
| 工具名跨插件冲突 | 调用指向不确定 | Phase 2 已定"先到先得 + 记录冲突"策略 |

## 不在本计划内

以下项在设计文档中已明确推后，本计划不涉及：插件市场与在线安装、OS 级沙箱、OCR/音乐/视频/网页查询插件、插件签名、插件间事件总线、多语言 SDK。
