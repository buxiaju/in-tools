"""屏幕截图插件（Python SDK 版）。

使用 Win32 API（ctypes）截取主屏幕，保存为 PNG 文件并返回路径。
仅依赖 Python 标准库（ctypes + zlib + struct），不需要安装 Pillow 等第三方包。
"""

import ctypes
import os
import struct
import sys
import time
import zlib
import importlib.util

_sdk_path = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "python-sdk", "intools.py")
_spec = importlib.util.spec_from_file_location("intools", _sdk_path)
_sdk = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_sdk)

Plugin = _sdk.Plugin
PluginError = _sdk.PluginError
ErrorCode = _sdk.ErrorCode

plugin = Plugin()

# ── Win32 常量与初始化 ──────────────────────────────────────────────

SRCCOPY = 0x00CC0020
SM_CXSCREEN = 0
SM_CYSCREEN = 1

_user32 = ctypes.windll.user32
_gdi32 = ctypes.windll.gdi32
_user32.SetProcessDPIAware()


class BITMAPINFOHEADER(ctypes.Structure):
    _fields_ = [
        ("biSize", ctypes.c_uint32), ("biWidth", ctypes.c_int32), ("biHeight", ctypes.c_int32),
        ("biPlanes", ctypes.c_uint16), ("biBitCount", ctypes.c_uint16), ("biCompression", ctypes.c_uint32),
        ("biSizeImage", ctypes.c_uint32), ("biXPelsPerMeter", ctypes.c_int32),
        ("biYPelsPerMeter", ctypes.c_int32), ("biClrUsed", ctypes.c_uint32), ("biClrImportant", ctypes.c_uint32),
    ]


# ── PNG 编码 ────────────────────────────────────────────────────────

def _png_chunk(chunk_type, data):
    body = chunk_type + data
    return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body) & 0xFFFFFFFF)


def write_png(path, width, height, rgb_data):
    stride = width * 3
    filtered = bytearray(height * (stride + 1))
    for y in range(height):
        pos = y * (stride + 1)
        filtered[pos] = 0
        filtered[pos + 1 : pos + 1 + stride] = rgb_data[y * stride : (y + 1) * stride]
    compressed = zlib.compress(bytes(filtered), 6)
    with open(path, "wb") as f:
        f.write(b"\x89PNG\r\n\x1a\n")
        f.write(_png_chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0)))
        f.write(_png_chunk(b"IDAT", compressed))
        f.write(_png_chunk(b"IEND", b""))


# ── 屏幕截图 ────────────────────────────────────────────────────────

def capture_screen(region=None):
    screen_w = _user32.GetSystemMetrics(SM_CXSCREEN)
    screen_h = _user32.GetSystemMetrics(SM_CYSCREEN)
    if screen_w == 0 or screen_h == 0:
        raise RuntimeError("无法获取屏幕分辨率")

    if region:
        cap_x, cap_y = int(region.get("x", 0)), int(region.get("y", 0))
        width, height = int(region.get("width", 0)), int(region.get("height", 0))
        if width <= 0 or height <= 0:
            raise RuntimeError("区域尺寸无效")
        if cap_x < 0:
            width += cap_x; cap_x = 0
        if cap_y < 0:
            height += cap_y; cap_y = 0
        width, height = min(width, screen_w - cap_x), min(height, screen_h - cap_y)
        if width <= 0 or height <= 0:
            raise RuntimeError("区域超出屏幕范围")
    else:
        cap_x, cap_y, width, height = 0, 0, screen_w, screen_h

    hdc_screen = _user32.GetDC(0)
    try:
        hdc_mem = _gdi32.CreateCompatibleDC(hdc_screen)
        bmp = _gdi32.CreateCompatibleBitmap(hdc_screen, width, height)
        old = _gdi32.SelectObject(hdc_mem, bmp)
        _gdi32.BitBlt(hdc_mem, 0, 0, width, height, hdc_screen, cap_x, cap_y, SRCCOPY)

        bih = BITMAPINFOHEADER()
        bih.biSize = ctypes.sizeof(BITMAPINFOHEADER)
        bih.biWidth, bih.biHeight = width, -height
        bih.biPlanes, bih.biBitCount, bih.biCompression = 1, 32, 0

        buf = ctypes.create_string_buffer(width * height * 4)
        if _gdi32.GetDIBits(hdc_mem, bmp, 0, height, buf, ctypes.byref(bih), 0) == 0:
            raise RuntimeError("GetDIBits 失败")

        _gdi32.SelectObject(hdc_mem, old)
        _gdi32.DeleteObject(bmp)
        _gdi32.DeleteDC(hdc_mem)
    finally:
        _user32.ReleaseDC(0, hdc_screen)

    raw = buf.raw
    count = width * height
    rgb = bytearray(count * 3)
    for i in range(count):
        s = i * 4
        d = i * 3
        rgb[d], rgb[d + 1], rgb[d + 2] = raw[s + 2], raw[s + 1], raw[s]
    return width, height, bytes(rgb)


# ── 工具 ────────────────────────────────────────────────────────────

@plugin.tool(
    "screenshot:capture",
    "截取当前主屏幕画面，保存为 PNG 文件，返回文件路径与图片尺寸",
    {
        "type": "object",
        "properties": {
            "output_dir": {"type": "string", "description": "截图保存目录，默认为 ~/Pictures"},
            "region": {"type": "object", "description": "截图区域 {x, y, width, height}，不传则截取全屏"},
        },
    },
)
def do_capture(args, ctx):
    output_dir = args.get("output_dir") or os.path.join(os.path.expanduser("~"), "Pictures")
    output_dir = os.path.expanduser(output_dir)
    try:
        os.makedirs(output_dir, exist_ok=True)
    except OSError as e:
        raise PluginError(ErrorCode.INVALID_PARAMS, f"无法创建输出目录 {output_dir}：{e}")

    region = args.get("region")
    if region is not None and not isinstance(region, dict):
        raise PluginError(ErrorCode.INVALID_PARAMS, "region 必须是包含 x/y/width/height 的对象")

    try:
        w, h, rgb = capture_screen(region)
    except RuntimeError as e:
        raise PluginError(ErrorCode.INTERNAL, f"截图失败：{e}")

    filename = time.strftime("screenshot_%Y%m%d_%H%M%S.png")
    filepath = os.path.join(output_dir, filename)
    try:
        write_png(filepath, w, h, rgb)
    except OSError as e:
        raise PluginError(ErrorCode.INTERNAL, f"写入 PNG 失败：{e}")

    return {"path": filepath, "width": w, "height": h, "filename": filename}


plugin.start()
