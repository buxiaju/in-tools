# 窗口信息

获取当前活动窗口或枚举所有顶层窗口的信息，包括标题、类名、进程名、位置和大小。

## 工具列表

| 工具              | 说明              |
| --------------- | --------------- |
| `window:active` | 获取当前前台活动窗口的详细信息 |
| `window:list`   | 列出所有可见的顶层窗口     |
| `window:find`   | 按标题关键词搜索窗口      |

## 用法

### 快捷键

按 `Ctrl+Shift+W` 快速获取当前活动窗口信息。

### 在对话中使用

- 「当前窗口是什么」

- 「列出所有打开的窗口」

- 「找一下标题包含 Chrome 的窗口」

## 返回格式

### window:active

```json
{
  "hwnd": 123456,
  "title": "InTools",
  "class_name": "Chrome_WidgetWin_1",
  "process_id": 9220,
  "process_name": "intools.exe",
  "visible": true,
  "minimized": false,
  "rect": {"x": 0, "y": 0, "width": 1920, "height": 1080}
}
```

### window:list

```json
{
  "windows": [...],
  "count": 10
}
```

### window:find

```json
{
  "matches": [...],
  "count": 2,
  "keyword": "Chrome"
}
```

## 限制

- 仅支持 Windows

- 默认只列出可见且非最小化的有标题窗口

- 窗口坐标为物理像素（已启用 DPI 感知）

- 快捷键默认 `Ctrl+Shift+W`，可在插件卡片的「快捷键」里修改

