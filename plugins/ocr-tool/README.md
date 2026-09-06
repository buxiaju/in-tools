# 屏幕文字识别 (OCR)

截取屏幕区域，使用 Windows 内置 OCR 引擎识别其中的文字。

## 用法

### 快捷键取词（推荐）

1. 按下 `Ctrl+Alt+O`
2. 屏幕变暗，拖拽框选想要识别的文字区域（或直接点击表示全屏）
3. 识别结果自动复制到剪贴板

### 在对话中使用

- 「识别一下屏幕上的文字」
- 「把这个区域的文字 OCR 出来」
- 「用英文识别屏幕上的文字」

## 工具参数

| 参数 | 类型 | 说明 |
| --- | --- | --- |
| `region` | object | 截图区域 `{x, y, width, height}`，不传则全屏 |
| `language` | string | 识别语言：`zh-Hans`、`zh-Hant`、`en`、`ja`、`ko` |
| `copy_to_clipboard` | boolean | 是否复制结果到剪贴板，默认 true |
| `save_image` | boolean | 是否保存截图文件，默认 false |

## 返回格式

```json
{
  "ok": true,
  "text": "识别出的完整文字",
  "lines": ["第一行", "第二行"],
  "language": "zh-Hans",
  "word_count": 10,
  "region": {"x": 100, "y": 200, "width": 800, "height": 600},
  "copied_to_clipboard": true,
  "image_path": null
}
```

## 权限说明

- `screen:capture`：截取屏幕画面
- `clipboard:write`：将识别结果复制到剪贴板

## 设置项

在插件卡片的「设置」里可以配置：

- **识别语言**：默认简体中文，可切换为英文、日文等
- **自动复制识别结果到剪贴板**：默认开启

## 系统要求

- Windows 10 1809 及以上（内置 Windows.Media.Ocr）
- 需要安装对应语言的 OCR 语言包（系统设置 → 时间和语言 → 语言 → 添加语言，勾选"文本到语音"和"光学字符识别"）

## 限制

- 仅支持 Windows
- 识别质量取决于截图清晰度和字体，建议框选时尽量只包含文字区域
- 手写体、艺术字、倾斜文字识别率较低
- 快捷键默认 `Ctrl+Alt+O`，可在插件卡片的「快捷键」里修改
