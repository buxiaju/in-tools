# 工作流插件

串联多个插件的工具，编排自动化工作流。演示反向 RPC（跨插件调用）与进度通知。

## 工具

| 工具 | 说明 |
|------|------|
| `workflow:list` | 列出宿主中所有可用工具（跨插件） |
| `workflow:run` | 执行内置工作流（如 `health-check`），逐步调用并推送进度 |
| `workflow:chain` | 用户自定义工具链，按顺序执行，前一步结果可传入下一步 |

## 内置工作流

- **health-check**：调用 `system:info` → `system:cpu` → `system:disk`，生成系统健康报告

## 自定义工具链示例

```json
{
  "steps": [
    { "tool": "system:info", "label": "获取系统信息" },
    { "tool": "hash:file", "args": { "path": "$prev" }, "label": "计算哈希" }
  ]
}
```

`$prev` 是占位符，会被替换为上一步的结果。

## 技术亮点

- **反向 RPC**：通过 `ctx.listTools()` 和 `ctx.callTool()` 调用其他插件
- **进度通知**：通过 `ctx.progress()` 推送实时进度到前端
- **流式通知**：通过 `ctx.stream()` 推送文本片段
