"""剪贴板工具插件（Python SDK 版）。

通过 Windows API 读写系统剪贴板的文本内容，支持读取、写入、追加、清空和检测。
仅依赖 Python 标准库 + ctypes。
"""

import ctypes
import os
import sys
import importlib.util

_sdk_path = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "python-sdk", "intools.py")
_spec = importlib.util.spec_from_file_location("intools", _sdk_path)
_sdk = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_sdk)

Plugin = _sdk.Plugin
PluginError = _sdk.PluginError
ErrorCode = _sdk.ErrorCode

plugin = Plugin()

# ── Windows API ────────────────────────────────────────────────────

user32 = ctypes.windll.user32
kernel32 = ctypes.windll.kernel32

CF_UNICODETEXT = 13
GMEM_MOVEABLE = 0x0002


def open_clipboard():
    for _ in range(10):
        if user32.OpenClipboard(0):
            return
        import time
        time.sleep(0.05)
    raise RuntimeError("无法打开剪贴板，可能被其他程序占用")


def read_text():
    open_clipboard()
    try:
        handle = user32.GetClipboardData(CF_UNICODETEXT)
        if not handle:
            return ""
        ptr = kernel32.GlobalLock(handle)
        if not ptr:
            return ""
        try:
            return ctypes.wstring_at(ptr)
        finally:
            kernel32.GlobalUnlock(handle)
    finally:
        user32.CloseClipboard()


def write_text(text):
    open_clipboard()
    try:
        user32.EmptyClipboard()
        data = ctypes.create_unicode_buffer(text)
        size = ctypes.sizeof(data)
        hmem = kernel32.GlobalAlloc(GMEM_MOVEABLE, size)
        if not hmem:
            raise RuntimeError("GlobalAlloc 失败")
        ptr = kernel32.GlobalLock(hmem)
        if not ptr:
            kernel32.GlobalFree(hmem)
            raise RuntimeError("GlobalLock 失败")
        ctypes.memmove(ptr, data, size)
        kernel32.GlobalUnlock(hmem)
        if not user32.SetClipboardData(CF_UNICODETEXT, hmem):
            kernel32.GlobalFree(hmem)
            raise RuntimeError("SetClipboardData 失败")
    finally:
        user32.CloseClipboard()


def clear_clipboard():
    open_clipboard()
    try:
        user32.EmptyClipboard()
    finally:
        user32.CloseClipboard()


# ── 工具 ──────────────────────────────────────────────────────────

@plugin.tool("clipboard:read_text", "读取系统剪贴板中的纯文本内容", {"type": "object", "properties": {}})
def do_read(args, ctx):
    try:
        text = read_text()
    except Exception as e:
        raise PluginError(ErrorCode.INTERNAL, f"读取剪贴板失败：{e}")
    return {"text": text, "length": len(text), "has_text": bool(text)}


@plugin.tool(
    "clipboard:write_text",
    "将指定文本写入系统剪贴板",
    {"type": "object", "required": ["text"], "properties": {"text": {"type": "string", "description": "要写入的文本"}}},
)
def do_write(args, ctx):
    text = args.get("text")
    if not isinstance(text, str):
        raise PluginError(ErrorCode.INVALID_PARAMS, "参数 text 必须是字符串")
    try:
        write_text(text)
    except Exception as e:
        raise PluginError(ErrorCode.INTERNAL, f"写入剪贴板失败：{e}")
    return {"ok": True, "length": len(text)}


@plugin.tool(
    "clipboard:append_text",
    "将文本追加到剪贴板现有内容之后",
    {
        "type": "object",
        "required": ["text"],
        "properties": {
            "text": {"type": "string", "description": "要追加的文本"},
            "separator": {"type": "string", "description": "分隔符，默认换行符"},
        },
    },
)
def do_append(args, ctx):
    text = args.get("text")
    if not isinstance(text, str):
        raise PluginError(ErrorCode.INVALID_PARAMS, "参数 text 必须是字符串")
    separator = args.get("separator", "\n")
    try:
        existing = read_text()
        new_text = existing + separator + text if existing else text
        write_text(new_text)
    except Exception as e:
        raise PluginError(ErrorCode.INTERNAL, f"追加剪贴板失败：{e}")
    return {"ok": True, "text": new_text, "length": len(new_text)}


@plugin.tool("clipboard:clear", "清空系统剪贴板", {"type": "object", "properties": {}})
def do_clear(args, ctx):
    try:
        clear_clipboard()
    except Exception as e:
        raise PluginError(ErrorCode.INTERNAL, f"清空剪贴板失败：{e}")
    return {"ok": True}


@plugin.tool("clipboard:has_text", "检查剪贴板中当前是否包含文本内容", {"type": "object", "properties": {}})
def do_has_text(args, ctx):
    return {"has_text": bool(user32.IsClipboardFormatAvailable(CF_UNICODETEXT))}


plugin.start()
