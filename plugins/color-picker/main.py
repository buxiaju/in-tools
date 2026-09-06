"""屏幕取色器插件（Python SDK 版）。

通过 Windows API 获取鼠标当前位置或指定坐标的屏幕像素颜色，
返回 HEX / RGB / HSL 格式，可选复制到剪贴板。
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
gdi32 = ctypes.windll.gdi32
kernel32 = ctypes.windll.kernel32

try:
    user32.SetProcessDPIAware()
except Exception:
    pass


class POINT(ctypes.Structure):
    _fields_ = [("x", ctypes.c_long), ("y", ctypes.c_long)]


def get_cursor_pos():
    pt = POINT()
    user32.GetCursorPos(ctypes.byref(pt))
    return pt.x, pt.y


def get_pixel(x, y):
    hdc = user32.GetDC(0)
    if not hdc:
        raise RuntimeError("GetDC 失败")
    try:
        color = gdi32.GetPixel(hdc, x, y)
        if color == 0xFFFFFFFF:
            color = gdi32.GetPixel(hdc, x, y)
        r = color & 0xFF
        g = (color >> 8) & 0xFF
        b = (color >> 16) & 0xFF
        return r, g, b
    finally:
        user32.ReleaseDC(0, hdc)


def rgb_to_hex(r, g, b):
    return "#{:02X}{:02X}{:02X}".format(r, g, b)


def rgb_to_hsl(r, g, b):
    rf, gf, bf = r / 255.0, g / 255.0, b / 255.0
    mx, mn = max(rf, gf, bf), min(rf, gf, bf)
    l = (mx + mn) / 2.0
    if mx == mn:
        h = s = 0.0
    else:
        d = mx - mn
        s = d / (2.0 - mx - mn) if l > 0.5 else d / (mx + mn)
        if mx == rf:
            h = (gf - bf) / d + (6.0 if gf < bf else 0.0)
        elif mx == gf:
            h = (bf - rf) / d + 2.0
        else:
            h = (rf - gf) / d + 4.0
        h /= 6.0
    return round(h * 360), round(s * 100), round(l * 100)


def set_clipboard_text(text):
    CF_UNICODETEXT, GMEM_MOVEABLE = 13, 0x0002
    if not user32.OpenClipboard(0):
        return False
    try:
        user32.EmptyClipboard()
        data = ctypes.create_unicode_buffer(text)
        hmem = kernel32.GlobalAlloc(GMEM_MOVEABLE, ctypes.sizeof(data))
        if not hmem:
            return False
        ptr = kernel32.GlobalLock(hmem)
        if not ptr:
            kernel32.GlobalFree(hmem)
            return False
        ctypes.memmove(ptr, data, ctypes.sizeof(data))
        kernel32.GlobalUnlock(hmem)
        if not user32.SetClipboardData(CF_UNICODETEXT, hmem):
            kernel32.GlobalFree(hmem)
            return False
        return True
    finally:
        user32.CloseClipboard()


def build_color_result(r, g, b, x, y):
    h, s, l = rgb_to_hsl(r, g, b)
    return {
        "hex": rgb_to_hex(r, g, b),
        "rgb": {"r": r, "g": g, "b": b},
        "rgb_string": f"rgb({r}, {g}, {b})",
        "hsl": {"h": h, "s": s, "l": l},
        "hsl_string": f"hsl({h}, {s}%, {l}%)",
        "position": {"x": x, "y": y},
    }


# ── 工具 ──────────────────────────────────────────────────────────

@plugin.tool(
    "color:pick",
    "获取鼠标当前所在位置的屏幕像素颜色，返回 HEX、RGB、HSL 及坐标",
    {
        "type": "object",
        "properties": {
            "copy_to_clipboard": {"type": "boolean", "description": "是否将 HEX 复制到剪贴板，默认 true"},
            "hex": {"type": "string", "description": "覆盖层传入的 HEX 值"},
            "r": {"type": "integer", "description": "覆盖层传入的 R 分量"},
            "g": {"type": "integer", "description": "覆盖层传入的 G 分量"},
            "b": {"type": "integer", "description": "覆盖层传入的 B 分量"},
            "x": {"type": "integer", "description": "覆盖层传入的 X 坐标"},
            "y": {"type": "integer", "description": "覆盖层传入的 Y 坐标"},
        },
    },
)
def pick_color(args, ctx):
    has_overlay = isinstance(args.get("r"), int) and isinstance(args.get("g"), int) and isinstance(args.get("b"), int)

    if has_overlay:
        r, g, b = args["r"] & 0xFF, args["g"] & 0xFF, args["b"] & 0xFF
        x, y = args.get("x", 0), args.get("y", 0)
    else:
        x, y = get_cursor_pos()
        try:
            r, g, b = get_pixel(x, y)
        except Exception as e:
            raise PluginError(ErrorCode.INTERNAL, f"取色失败：{e}")

    result = build_color_result(r, g, b, x, y)

    if args.get("copy_to_clipboard", True):
        result["copied_to_clipboard"] = set_clipboard_text(result["hex"])

    return result


@plugin.tool(
    "color:at",
    "获取屏幕上指定坐标处的像素颜色",
    {
        "type": "object",
        "required": ["x", "y"],
        "properties": {
            "x": {"type": "integer", "description": "屏幕物理像素 X 坐标"},
            "y": {"type": "integer", "description": "屏幕物理像素 Y 坐标"},
        },
    },
)
def color_at(args, ctx):
    x, y = args.get("x"), args.get("y")
    if not isinstance(x, int) or not isinstance(y, int) or x < 0 or y < 0:
        raise PluginError(ErrorCode.INVALID_PARAMS, "参数 x、y 必须是非负整数")
    try:
        r, g, b = get_pixel(x, y)
    except Exception as e:
        raise PluginError(ErrorCode.INTERNAL, f"取色失败：{e}")
    return build_color_result(r, g, b, x, y)


plugin.start()
