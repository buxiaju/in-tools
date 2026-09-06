"""网页抓取插件（Python SDK 版）。

抓取网页内容、提取文本、解析 HTML。
使用标准库实现，无需安装第三方包。
"""

import importlib.util
import os
import re
import urllib.request
import urllib.error
from html.parser import HTMLParser

# 加载 SDK
_sdk_path = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "python-sdk", "intools.py")
_spec = importlib.util.spec_from_file_location("intools", _sdk_path)
_sdk = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_sdk)

Plugin = _sdk.Plugin
PluginError = _sdk.PluginError
ErrorCode = _sdk.ErrorCode

plugin = Plugin()


# ── HTML 解析器 ──────────────────────────────────────────────────

class TextExtractor(HTMLParser):
    """从 HTML 中提取纯文本。"""

    def __init__(self):
        super().__init__()
        self.text_parts = []
        self.skip_tags = {"script", "style", "noscript"}
        self.current_skip = False

    def handle_starttag(self, tag, attrs):
        if tag in self.skip_tags:
            self.current_skip = True

    def handle_endtag(self, tag):
        if tag in self.skip_tags:
            self.current_skip = False

    def handle_data(self, data):
        if not self.current_skip:
            text = data.strip()
            if text:
                self.text_parts.append(text)

    def get_text(self):
        return "\n".join(self.text_parts)


class LinkExtractor(HTMLParser):
    """从 HTML 中提取所有链接。"""

    def __init__(self, base_url=""):
        super().__init__()
        self.links = []
        self.base_url = base_url

    def handle_starttag(self, tag, attrs):
        if tag == "a":
            for name, value in attrs:
                if name == "href" and value:
                    # 处理相对链接
                    if value.startswith(("http://", "https://")):
                        self.links.append(value)
                    elif value.startswith("//"):
                        self.links.append("https:" + value)
                    elif value.startswith("/") and self.base_url:
                        # 绝对路径
                        base = self.base_url.rstrip("/")
                        self.links.append(base + value)
                    elif self.base_url:
                        # 相对路径
                        base = self.base_url.rstrip("/")
                        if not base.endswith("/"):
                            base = base.rsplit("/", 1)[0]
                        self.links.append(base + "/" + value)
                    else:
                        self.links.append(value)

    def get_links(self):
        return self.links


# ── 工具 ──────────────────────────────────────────────────────────

@plugin.tool(
    "web:fetch",
    "获取网页内容",
    {
        "type": "object",
        "required": ["url"],
        "properties": {
            "url": {"type": "string", "description": "网页 URL"},
            "encoding": {"type": "string", "description": "字符编码，默认自动检测"},
        },
    },
)
def do_fetch(args, ctx):
    url = args["url"]
    encoding = args.get("encoding")

    try:
        req = urllib.request.Request(url, headers={
            "User-Agent": "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36",
            "Accept": "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
            "Accept-Language": "zh-CN,zh;q=0.9,en;q=0.8",
        })
        with urllib.request.urlopen(req, timeout=30) as resp:
            # 获取编码
            content_type = resp.headers.get("Content-Type", "")
            if not encoding:
                # 从 Content-Type 中提取编码
                match = re.search(r"charset=([^\s;]+)", content_type, re.IGNORECASE)
                if match:
                    encoding = match.group(1)
                else:
                    encoding = "utf-8"

            body = resp.read().decode(encoding, errors="replace")
            return {
                "status": resp.status,
                "url": resp.url,
                "content_type": content_type,
                "encoding": encoding,
                "body": body,
                "length": len(body),
            }
    except urllib.error.HTTPError as e:
        return {"status": e.code, "error": str(e)}
    except Exception as e:
        raise PluginError(ErrorCode.INTERNAL, f"网页抓取失败：{e}")


@plugin.tool(
    "web:extract-text",
    "从 HTML 中提取纯文本",
    {
        "type": "object",
        "required": ["html"],
        "properties": {
            "html": {"type": "string", "description": "HTML 内容"},
        },
    },
)
def do_extract_text(args, ctx):
    html = args["html"]

    try:
        extractor = TextExtractor()
        extractor.feed(html)
        text = extractor.get_text()
        return {
            "text": text,
            "length": len(text),
        }
    except Exception as e:
        raise PluginError(ErrorCode.INTERNAL, f"文本提取失败：{e}")


@plugin.tool(
    "web:extract-links",
    "从 HTML 中提取所有链接",
    {
        "type": "object",
        "required": ["html"],
        "properties": {
            "html": {"type": "string", "description": "HTML 内容"},
            "base_url": {"type": "string", "description": "基础 URL（用于解析相对链接）"},
        },
    },
)
def do_extract_links(args, ctx):
    html = args["html"]
    base_url = args.get("base_url", "")

    try:
        extractor = LinkExtractor(base_url)
        extractor.feed(html)
        links = extractor.get_links()
        return {
            "links": links,
            "count": len(links),
        }
    except Exception as e:
        raise PluginError(ErrorCode.INTERNAL, f"链接提取失败：{e}")


plugin.start()
