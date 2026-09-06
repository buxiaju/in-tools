# AI 编排

这是整个对话功能的大脑。你在对话框里输入的内容由它转发给 AI，AI 需要动手时，它负责去调用其他插件的工具。

## 先配置 API

设置页 →「AI API 配置」，填三项：

| 字段 | 说明 |
| --- | --- |
| `base_url` | OpenAI 兼容接口地址，例如 `https://api.deepseek.com/v1` |
| `api_key` | 你的密钥 |
| `model` | 模型名，留空按 `deepseek-chat` 处理 |

没填 `base_url` 或 `api_key`，调用会直接返回提示，不会去发请求。

## 工具

### `ai:chat`

向 AI 发送消息，AI 会自动选择并调用可用工具来完成任务。

**参数：**
- `message` (必需)：用户的自然语言消息

**示例：**
```json
{
  "message": "帮我找找桌面上有没有叫报告的文件"
}
```

### `ai:chat-with-history`

向 AI 发送消息，支持多轮对话历史。

**参数：**
- `message` (必需)：用户的自然语言消息
- `history` (可选)：对话历史数组
- `system_prompt` (可选)：自定义系统提示词

**示例：**
```json
{
  "message": "继续刚才的对话",
  "history": [
    {"role": "user", "content": "你好"},
    {"role": "assistant", "content": "你好！有什么可以帮助你的吗？"}
  ],
  "system_prompt": "你是一个专业的助手"
}
```

## 它是怎么干活的

1. 收到你的消息
2. 调 `host/listTools` 拿到当前所有已启用插件的工具，转成 OpenAI function calling 格式
3. 把消息和工具列表一起发给 AI
4. AI 要调工具，就经 `host/callTool` 逐个执行，把结果回灌给 AI
5. 循环到 AI 给出最终答复

所以**你新装一个插件，不用改这里的任何代码**，它下一轮就能被 AI 看见并调用。

## 权限

声明了 `network:http`（低危）。它只往你配的 `base_url` 发请求。

`api_key` 存在 `~/.intools/plugin-configs/com.intools.ai-orchestrator.json`，明文。这台机器上能读你用户目录的程序都能看到它。

## 运行方式

`mode = "startup"`，随 InTools 启动常驻，不空闲回收——每次对话都要用，反复拉起进程不划算。`request_timeout_sec = 120`，因为一次对话可能包含多轮工具调用。

## 排查

- **一直转圈然后超时**：多半是 `base_url` 不通，或者模型响应慢过了 120 秒。
- **AI 说不知道有什么工具**：确认目标插件在插件页是启用状态，禁用的插件不会进 `host/listTools`。
- **工具调用被拒**：看是不是那个插件的权限被你在权限页拒过，去删掉授权记录重试。

## 一个值得注意的实现细节

它的配置是用 `host/getConfig` 读的，走 `plugin-configs/` 这套**插件私有存储**——这跟 manifest 里 `[[settings]]` 声明的那套配置**不是同一个地方**。`[[settings]]` 的值由宿主合并进工具调用参数，`host/getConfig` 永远读不到。写插件时别搞混。
