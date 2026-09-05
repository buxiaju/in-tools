"""屏幕截图插件。

使用 Win32 API（ctypes）截取主屏幕，保存为 PNG 文件并返回路径。
仅依赖 Python 标准库（ctypes + zlib + struct），不需要安装 Pillow 等第三方包。
协议与 hello-plugin 一致：逐行 JSON-RPC 2.0，stdin 收、stdout 发。
stdout 只允许出现协议消息，任何调试输出都必须走 stderr。
"""

import json
import os
import struct
import sys
import time
import zlib
import ctypes

PROTOCOL_VERSION = "1.0"

# ── Win32 常量与初始化 ──

SRCCOPY = 0x00CC0020
SM_CXSCREEN = 0
SM_CYSCREEN = 1

_is_windows = sys.platform.startswith("win")
if _is_windows:
    _user32 = ctypes.windll.user32
    _gdi32 = ctypes.windll.gdi32
    # 让进程感知 DPI，否则在高分屏上截到的图会被系统缩放。
    _user32.SetProcessDPIAware()


class BITMAPINFOHEADER(ctypes.Structure):
    _fields_ = [
        ("biSize", ctypes.c_uint32),
        ("biWidth", ctypes.c_int32),
        ("biHeight", ctypes.c_int32),
        ("biPlanes", ctypes.c_uint16),
        ("biBitCount", ctypes.c_uint16),
        ("biCompression", ctypes.c_uint32),
        ("biSizeImage", ctypes.c_uint32),
        ("biXPelsPerMeter", ctypes.c_int32),
        ("biYPelsPerMeter", ctypes.c_int32),
        ("biClrUsed", ctypes.c_uint32),
        ("biClrImportant", ctypes.c_uint32),
    ]


# ── PNG 编码 ──


def _png_chunk(chunk_type, data):
    """构造一个 PNG chunk：长度 + 类型 + 数据 + CRC32。"""
    body = chunk_type + data
    return (
        struct.pack(">I", len(data))
        + body
        + struct.pack(">I", zlib.crc32(body) & 0xFFFFFFFF)
    )


def write_png(path, width, height, rgb_data):
    """把 RGB 像素数据写成 PNG 文件。

    rgb_data 为 width*height*3 字节，行序从上到下，每像素按 R G B 排列。
    PNG 格式：签名 + IHDR + IDAT + IEND，每个 chunk 带 CRC32。
    """
    # 每行前加一个过滤字节（0 = None），再整体 zlib 压缩。
    stride = width * 3
    filtered = bytearray(height * (stride + 1))
    for y in range(height):
        pos = y * (stride + 1)
        filtered[pos] = 0
        filtered[pos + 1 : pos + 1 + stride] = rgb_data[
            y * stride : (y + 1) * stride
        ]
    compressed = zlib.compress(bytes(filtered), 6)

    with open(path, "wb") as f:
        f.write(b"\x89PNG\r\n\x1a\n")
        f.write(
            _png_chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0))
        )
        f.write(_png_chunk(b"IDAT", compressed))
        f.write(_png_chunk(b"IEND", b""))


# ── 屏幕截图 ──


def capture_screen(region=None):
    """截取主屏幕，返回 (width, height, rgb_bytes)。

    用 BitBlt 把屏幕 DC 拷到内存位图，再用 GetDIBits 取出像素。
    Win32 32 位位图返回 BGRA，这里转成 RGB 给 PNG 编码器。

    若提供 region = {"x", "y", "width", "height"}，则只截取该区域。
    """
    if not _is_windows:
        raise RuntimeError("屏幕截图插件仅支持 Windows")

    screen_w = _user32.GetSystemMetrics(SM_CXSCREEN)
    screen_h = _user32.GetSystemMetrics(SM_CYSCREEN)
    if screen_w == 0 or screen_h == 0:
        raise RuntimeError("无法获取屏幕分辨率")

    if region:
        # 区域截图：截取指定的矩形区域
        try:
            cap_x = int(region.get("x", 0))
            cap_y = int(region.get("y", 0))
            width = int(region.get("width", 0))
            height = int(region.get("height", 0))
        except (TypeError, ValueError):
            raise RuntimeError("区域参数必须为数字：x/y/width/height")
        if width <= 0 or height <= 0:
            raise RuntimeError("区域尺寸无效：width 和 height 必须为正数")
        # 裁剪到屏幕范围内
        if cap_x < 0:
            width += cap_x
            cap_x = 0
        if cap_y < 0:
            height += cap_y
            cap_y = 0
        width = min(width, screen_w - cap_x)
        height = min(height, screen_h - cap_y)
        if width <= 0 or height <= 0:
            raise RuntimeError("区域超出屏幕范围")
    else:
        cap_x, cap_y = 0, 0
        width = screen_w
        height = screen_h

    hdc_screen = _user32.GetDC(0)
    try:
        hdc_mem = _gdi32.CreateCompatibleDC(hdc_screen)
        bmp = _gdi32.CreateCompatibleBitmap(hdc_screen, width, height)
        old = _gdi32.SelectObject(hdc_mem, bmp)

        _gdi32.BitBlt(hdc_mem, 0, 0, width, height, hdc_screen, cap_x, cap_y, SRCCOPY)

        bih = BITMAPINFOHEADER()
        bih.biSize = ctypes.sizeof(BITMAPINFOHEADER)
        bih.biWidth = width
        bih.biHeight = -height  # 负值 = 从上到下，与 PNG 行序一致
        bih.biPlanes = 1
        bih.biBitCount = 32
        bih.biCompression = 0  # BI_RGB

        buf_size = width * height * 4
        buf = ctypes.create_string_buffer(buf_size)
        rows = _gdi32.GetDIBits(hdc_mem, bmp, 0, height, buf, ctypes.byref(bih), 0)
        if rows == 0:
            raise RuntimeError("GetDIBits 失败，无法读取像素数据")

        # 清理 GDI 资源
        _gdi32.SelectObject(hdc_mem, old)
        _gdi32.DeleteObject(bmp)
        _gdi32.DeleteDC(hdc_mem)
    finally:
        _user32.ReleaseDC(0, hdc_screen)

    # BGRA → RGB：Win32 32 位位图每像素 4 字节按 B G R A 排列，
    # PNG 需要 R G B。逐像素转换是纯 Python 循环，1080p 约 200 万像素，
    # 实测耗时 1–2 秒，对截图场景可接受。
    raw = buf.raw
    count = width * height
    rgb = bytearray(count * 3)
    for i in range(count):
        s = i * 4
        d = i * 3
        rgb[d] = raw[s + 2]      # R
        rgb[d + 1] = raw[s + 1]  # G
        rgb[d + 2] = raw[s]      # B
    return width, height, bytes(rgb)


# ── 工具定义 ──

TOOLS = [
    {
        "name": "screenshot:capture",
        "description": "截取当前主屏幕画面，保存为 PNG 文件，返回文件路径与图片尺寸",
        "input_schema": {
            "type": "object",
            "properties": {
                "output_dir": {
                    "type": "string",
                    "description": "截图保存目录，默认为用户主目录下的 Pictures",
                },
                "region": {
                    "type": "object",
                    "description": "截图区域 {x, y, width, height}，不传则截取全屏",
                },
            },
        },
    }
]

CODE_METHOD_NOT_FOUND = -32601
CODE_INVALID_PARAMS = -32602
CODE_INTERNAL_ERROR = -32603


# ── 协议层（与其它插件同构）──


def log(message):
    print(message, file=sys.stderr, flush=True)


def write_message(payload):
    sys.stdout.write(json.dumps(payload, ensure_ascii=False) + "\n")
    sys.stdout.flush()


def notify(method, params):
    write_message({"jsonrpc": "2.0", "method": method, "params": params})


def reply_result(request_id, result):
    write_message({"jsonrpc": "2.0", "id": request_id, "result": result})


def reply_error(request_id, code, message):
    write_message(
        {"jsonrpc": "2.0", "id": request_id, "error": {"code": code, "message": message}}
    )


def call_tool(params):
    name = params.get("name")
    arguments = params.get("arguments") or {}

    if name != "screenshot:capture":
        return None, (CODE_METHOD_NOT_FOUND, "未知工具：{}".format(name))

    output_dir = arguments.get("output_dir")
    if not output_dir:
        output_dir = os.path.join(os.path.expanduser("~"), "Pictures")
    output_dir = os.path.expanduser(output_dir)

    try:
        os.makedirs(output_dir, exist_ok=True)
    except OSError as e:
        return None, (CODE_INVALID_PARAMS, "无法创建输出目录 {}：{}".format(output_dir, e))

    # 可选的区域参数：region = {"x", "y", "width", "height"}
    region = arguments.get("region")
    if region is not None and not isinstance(region, dict):
        return None, (CODE_INVALID_PARAMS, "region 必须是包含 x/y/width/height 的对象")

    try:
        w, h, rgb = capture_screen(region)
    except RuntimeError as e:
        return None, (CODE_INTERNAL_ERROR, "截图失败：{}".format(e))
    except Exception as e:
        return None, (CODE_INTERNAL_ERROR, "截图过程异常：{}".format(e))

    filename = time.strftime("screenshot_%Y%m%d_%H%M%S.png")
    filepath = os.path.join(output_dir, filename)

    try:
        write_png(filepath, w, h, rgb)
    except OSError as e:
        return None, (CODE_INTERNAL_ERROR, "写入 PNG 失败：{}".format(e))

    return {"path": filepath, "width": w, "height": h, "filename": filename}, None


def handle_request(request_id, method, params):
    if method == "plugin/ready":
        log("握手完成，插件目录：{}".format(params.get("plugin_dir")))
        reply_result(request_id, {"ok": True})
        return False

    if method == "tools/list":
        reply_result(request_id, {"tools": TOOLS})
        return False

    if method == "tools/call":
        result, error = call_tool(params)
        if error is None:
            reply_result(request_id, result)
        else:
            reply_error(request_id, error[0], error[1])
        return False

    if method == "plugin/shutdown":
        reply_result(request_id, {"ok": True})
        return True

    reply_error(request_id, CODE_METHOD_NOT_FOUND, "未知方法：{}".format(method))
    return False


def main():
    # 协议规定 UTF-8。Windows 上 Python 面对管道时默认用系统代码页，
    # 钉死编码消除环境依赖。
    sys.stdin.reconfigure(encoding="utf-8")
    sys.stdout.reconfigure(encoding="utf-8")
    sys.stderr.reconfigure(encoding="utf-8")

    # 握手第一步由插件主动发起。
    notify("plugin/hello", {"protocol_version": PROTOCOL_VERSION, "tools": TOOLS})

    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue

        try:
            message = json.loads(line)
        except ValueError:
            log("丢弃无法解析的输入行：{}".format(line[:200]))
            continue

        request_id = message.get("id")
        method = message.get("method")

        if method is None:
            continue

        if request_id is None:
            log("忽略通知：{}".format(method))
            continue

        if handle_request(request_id, method, message.get("params") or {}):
            break

    log("插件退出")


if __name__ == "__main__":
    main()
