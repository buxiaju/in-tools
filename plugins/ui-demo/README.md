# UI Demo 插件

演示宿主的声明式 UI 系统。展示三种结果展示类型和增强的设置表单。

## 工具

| 工具 | 结果展示 | 说明 |
| --- | --- | --- |
| `demo:status` | kv | 系统状态信息 |
| `demo:list` | table | 当前目录文件列表 |
| `demo:report` | markdown | Markdown 格式系统报告 |

## 设置项演示

本插件的设置表单展示了：
- **分组**：设置项按「外观」「显示」「API 配置」「输出」分组
- **颜色选择器**：主题色设置
- **数字输入**：支持 min/max/step 约束
- **密码输入**：API Key 遮罩显示
- **路径输入**：输出目录选择
- **字段描述**：每个字段都有说明文字

## 使用方式

在对话中可以这样说：

- 「查看系统状态」→ 触发 `demo:status`
- 「列出当前目录文件」→ 触发 `demo:list`
- 「生成系统报告」→ 触发 `demo:report`

## manifest 中的 result_display 声明

本插件的 `manifest.toml` 演示了三种结果展示声明：

- **kv**：`demo:status` 的结果以键值对形式展示（平台、Python 版本、运行时间等）
- **table**：`demo:list` 的结果以表格形式展示（文件名、大小、修改时间）
- **markdown**：`demo:report` 的结果以 Markdown 格式渲染

在自己的插件中使用 `[[result_display]]` 段即可让工具结果自动以结构化 UI 展示，而非原始 JSON。
