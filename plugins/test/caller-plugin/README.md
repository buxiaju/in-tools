# caller-plugin

反向 RPC 集成测试的调用方插件。通过 `host/*` 回调宿主，验证插件间调用链路的正确性。

## 工具

| 工具 | 说明 |
| --- | --- |
| `caller:call` | 经 `host/callTool` 调用指定工具 |
| `caller:list` | 调用 `host/listTools` 列出全部已启用工具 |
| `caller:config` | 先 `host/setConfig` 再 `host/getConfig`，验证配置读写往返 |
| `caller:notify` | 调用 `host/notify` 向 UI 推送通知 |
| `caller:unknown` | 调用不存在的 `host/unknown` 方法，验证错误处理 |
| `caller:recurse` | 经 `host/callTool` 调用自身，测试递归调用深度上限 |

## 用途

本插件不提供用户功能，仅供集成测试使用。测试覆盖：

- 正常反向 RPC 调用（`host/callTool`、`host/listTools`）
- 插件私有配置读写（`host/getConfig`、`host/setConfig`）
- 通知推送（`host/notify`）
- 未知方法的错误响应
- 递归调用深度上限（5 层）

## 无权限声明

`permissions = []`——不申请任何权限，测试环境零配置即可运行。
