# 网页抓取插件

抓取网页内容、提取文本、解析 HTML。使用 Python 标准库实现，无需安装第三方包。

## 工具

### `web:fetch`

获取网页内容。

**参数：**
- `url` (必需)：网页 URL
- `encoding` (可选)：字符编码，默认自动检测

**返回：**
- `status`：HTTP 状态码
- `url`：最终 URL（可能经过重定向）
- `content_type`：内容类型
- `encoding`：字符编码
- `body`：网页内容
- `length`：内容长度

**示例：**
```json
{
  "url": "https://example.com"
}
```

### `web:extract-text`

从 HTML 中提取纯文本。

**参数：**
- `html` (必需)：HTML 内容

**返回：**
- `text`：提取的纯文本
- `text`：文本长度

**示例：**
```json
{
  "html": "<html><body><h1>Hello</h1><p>World</p></body></html>"
}
```

### `web:extract-links`

从 HTML 中提取所有链接。

**参数：**
- `html` (必需)：HTML 内容
- `base_url` (可选)：基础 URL（用于解析相对链接）

**返回：**
- `links`：链接列表
- `count`：链接数量

**示例：**
```json
{
  "html": "<html><body><a href=\"/page1\">Page 1</a><a href=\"https://example.com/page2\">Page 2</a></body></html>",
  "base_url": "https://example.com"
}
```

## 使用场景

1. **网页内容抓取**：获取网页的完整 HTML 内容
2. **文本提取**：从 HTML 中提取纯文本，去除标签和脚本
3. **链接提取**：从网页中提取所有链接，用于爬虫或分析
4. **网页分析**：分析网页结构、内容分布

## 权限

- `network:http`：网络 HTTP 访问权限

## 技术实现

- 使用 Python 标准库 `urllib.request` 进行 HTTP 请求
- 使用 `html.parser.HTMLParser` 解析 HTML
- 自动检测字符编码
- 支持相对链接解析
