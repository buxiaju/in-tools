"""窗口信息插件（Python SDK 版）。

通过 Windows API 获取当前活动窗口或枚举所有顶层窗口的信息。
仅依赖 Python 标准库 + ctypes。
"""

import ctypes
import os
import sys
import importlib.util
from ctypes import wintypes

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
psapi = ctypes.windll.psapi

try:
    user32.SetProcessDPIAware()
except Exception:
    pass


class RECT(ctypes.Structure):
    _fields_ = [
        ("left", ctypes.c_long), ("top", ctypes.c_long),
        ("right", ctypes.c_long), ("bottom", ctypes.c_long),
    ]


def get_window_text(hwnd):
    length = user32.GetWindowTextLengthW(hwnd)
    if length <= 0:
        return ""
    buf = ctypes.create_unicode_buffer(length + 1)
    user32.GetWindowTextW(hwnd, buf, length + 1)
    return buf.value


def get_class_name(hwnd):
    buf = ctypes.create_unicode_buffer(256)
    user32.GetClassNameW(hwnd, buf, 256)
    return buf.value


def get_window_process_id(hwnd):
    pid = ctypes.c_ulong()
    user32.GetWindowThreadProcessId(hwnd, ctypes.byref(pid))
    return pid.value


def get_process_name(pid):
    try:
        handle = kernel32.OpenProcess(0x0400 | 0x0010, False, pid)
        if not handle:
            return None
        try:
            buf = ctypes.create_unicode_buffer(260)
            size = ctypes.c_ulong(260)
            if psapi.GetModuleBaseNameW(handle, None, buf, size):
                return buf.value
        finally:
            kernel32.CloseHandle(handle)
    except Exception:
        pass
    return None


def get_window_rect(hwnd):
    rect = RECT()
    if user32.GetWindowRect(hwnd, ctypes.byref(rect)):
        return {"x": rect.left, "y": rect.top, "width": rect.right - rect.left, "height": rect.bottom - rect.top}
    return None


def get_window_info(hwnd):
    pid = get_window_process_id(hwnd)
    return {
        "hwnd": hwnd, "title": get_window_text(hwnd), "class_name": get_class_name(hwnd),
        "process_id": pid, "process_name": get_process_name(pid),
        "visible": bool(user32.IsWindowVisible(hwnd)),
        "minimized": bool(user32.IsIconic(hwnd)),
        "rect": get_window_rect(hwnd),
    }


def enum_windows(include_invisible=False, include_minimized=False, max_results=50):
    results = []
    stopped = {"value": False}
    WNDENUMPROC = ctypes.WINFUNCTYPE(ctypes.c_bool, wintypes.HWND, wintypes.LPARAM)

    def callback(hwnd, lparam):
        if stopped["value"]:
            return False
        if not include_invisible and not user32.IsWindowVisible(hwnd):
            return True
        if not include_minimized and user32.IsIconic(hwnd):
            return True
        title = get_window_text(hwnd)
        if not title and not include_invisible:
            return True
        results.append(get_window_info(hwnd))
        if len(results) >= max_results:
            stopped["value"] = True
            return False
        return True

    user32.EnumWindows(WNDENUMPROC(callback), 0)
    return results


# ── 工具 ──────────────────────────────────────────────────────────

@plugin.tool("window:active", "获取当前前台活动窗口的详细信息", {"type": "object", "properties": {}})
def active_window(args, ctx):
    hwnd = user32.GetForegroundWindow()
    if not hwnd:
        raise PluginError(ErrorCode.INTERNAL, "无法获取前台窗口")
    return get_window_info(hwnd)


@plugin.tool(
    "window:list",
    "列出当前所有可见的顶层窗口",
    {
        "type": "object",
        "properties": {
            "include_minimized": {"type": "boolean", "description": "是否包含最小化窗口"},
            "max_results": {"type": "integer", "description": "最多返回的窗口数，默认 50"},
        },
    },
)
def list_windows(args, ctx):
    include_minimized = args.get("include_minimized", False)
    max_results = args.get("max_results", 50)
    if not isinstance(max_results, int) or max_results < 1:
        max_results = 50
    max_results = min(max_results, 200)
    try:
        windows = enum_windows(include_minimized=include_minimized, max_results=max_results)
    except Exception as e:
        raise PluginError(ErrorCode.INTERNAL, f"枚举窗口失败：{e}")
    return {"windows": windows, "count": len(windows)}


@plugin.tool(
    "window:find",
    "按标题关键词搜索窗口",
    {
        "type": "object",
        "required": ["title_keyword"],
        "properties": {
            "title_keyword": {"type": "string", "description": "窗口标题关键词（不区分大小写）"},
            "max_results": {"type": "integer", "description": "最多返回的结果数，默认 20"},
        },
    },
)
def find_window(args, ctx):
    keyword = args.get("title_keyword")
    if not isinstance(keyword, str) or not keyword:
        raise PluginError(ErrorCode.INVALID_PARAMS, "参数 title_keyword 缺失或为空")
    max_results = min(args.get("max_results", 20), 100)
    keyword_lower = keyword.lower()
    try:
        all_windows = enum_windows(include_minimized=True, max_results=200)
    except Exception as e:
        raise PluginError(ErrorCode.INTERNAL, f"枚举窗口失败：{e}")
    matches = [w for w in all_windows if keyword_lower in w["title"].lower()][:max_results]
    return {"matches": matches, "count": len(matches), "keyword": keyword}


plugin.start()
