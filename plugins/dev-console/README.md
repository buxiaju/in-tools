# 开发者控制台

通过 AI 对话界面调试和管理插件，无需离开 InTools。

## 工具

| 工具 | 说明 |
|------|------|
| `dev:plugins` | 列出所有已加载插件及其状态 |
| `dev:tools` | 列出所有可用工具及 JSON Schema |
| `dev:call` | 直接调用任意工具（调试用） |
| `dev:config` | 读写本插件私有配置 |
| `dev:validate` | 校验 manifest.toml 并报告问题 |
| `dev:search` | 按关键词搜索插件和工具 |

## 使用示例

在 AI 对话中：

- "列出所有插件" → 调用 `dev:plugins`
- "hello:echo 这个工具有什么参数" → AI 会先调 `dev:tools` 查 schema
- "帮我调用 hello:echo，参数 text=你好" → AI 调 `dev:call`
- "校验一下 file-hash 插件的 manifest" → AI 调 `dev:validate`
- "有哪些跟文件相关的工具" → AI 调 `dev:search`

## 技术亮点

- **反向 RPC**：通过 `host/listTools`、`host/callTool` 访问宿主能力
- **manifest 校验**：解析 TOML 并检查必填字段、工具名冲突、高危权限
- **零依赖**：仅使用 Python 标准库 + 系统自带 toml 模块
