# 进程管理

列出、搜索、终止进程，启动程序，执行 shell 命令。

## 工具

| 工具 | 说明 |
|------|------|
| `proc:list` | 列出当前运行的进程，支持按名称过滤 |
| `proc:kill` | 终止指定 PID 的进程 |
| `proc:start` | 启动程序、打开文件/文件夹/网址 |
| `proc:exec` | 执行 shell 命令并返回输出 |

## 权限

需要 `process:spawn` 和 `shell:exec` 权限，不对 MCP 暴露。
