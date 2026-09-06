"""屏幕文字识别插件（Python SDK 版）。

截取屏幕区域，通过 Windows 内置 OCR 引擎识别文字。
截图使用 GDI，OCR 通过 PowerShell 调用 WinRT API，无需安装第三方 Python 包。
"""

import ctypes
import json
import os
import subprocess
import sys
import tempfile
import time
import importlib.util

_sdk_path = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "python-sdk", "intools.py")
_spec = importlib.util.spec_from_file_location("intools", _sdk_path)
_sdk = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_sdk)

Plugin = _sdk.Plugin
PluginError = _sdk.PluginError
ErrorCode = _sdk.ErrorCode

plugin = Plugin()

# ── Windows API for screenshot ─────────────────────────────────────

user32 = ctypes.windll.user32
gdi32 = ctypes.windll.gdi32
kernel32 = ctypes.windll.kernel32

try:
    user32.SetProcessDPIAware()
except Exception:
    pass

SRCCOPY = 0x00CC0020
DIB_RGB_COLORS = 0
BI_RGB = 0


class BITMAPINFOHEADER(ctypes.Structure):
    _fields_ = [
        ("biSize", ctypes.c_uint32), ("biWidth", ctypes.c_long), ("biHeight", ctypes.c_long),
        ("biPlanes", ctypes.c_uint16), ("biBitCount", ctypes.c_uint16), ("biCompression", ctypes.c_uint32),
        ("biSizeImage", ctypes.c_uint32), ("biXPelsPerMeter", ctypes.c_long),
        ("biYPelsPerMeter", ctypes.c_long), ("biClrUsed", ctypes.c_uint32), ("biClrImportant", ctypes.c_uint32),
    ]


class BITMAPINFO(ctypes.Structure):
    _fields_ = [("bmiHeader", BITMAPINFOHEADER), ("bmiColors", ctypes.c_ulong * 3)]


def capture_screen(x, y, width, height):
    hdc_screen = user32.GetDC(0)
    if not hdc_screen:
        raise RuntimeError("GetDC 失败")
    hdc_mem = gdi32.CreateCompatibleDC(hdc_screen)
    hbitmap = gdi32.CreateCompatibleBitmap(hdc_screen, width, height)
    gdi32.SelectObject(hdc_mem, hbitmap)
    gdi32.BitBlt(hdc_mem, 0, 0, width, height, hdc_screen, x, y, SRCCOPY)

    bmi = BITMAPINFO()
    bmi.bmiHeader.biSize = ctypes.sizeof(BITMAPINFOHEADER)
    bmi.bmiHeader.biWidth = width
    bmi.bmiHeader.biHeight = -height
    bmi.bmiHeader.biPlanes = 1
    bmi.bmiHeader.biBitCount = 24
    bmi.bmiHeader.biCompression = BI_RGB

    row_size = ((width * 3 + 3) // 4) * 4
    image_size = row_size * height
    buf = (ctypes.c_ubyte * image_size)()
    gdi32.GetDIBits(hdc_mem, hbitmap, 0, height, buf, ctypes.byref(bmi), DIB_RGB_COLORS)

    file_header_size = 14
    info_header_size = ctypes.sizeof(BITMAPINFOHEADER)
    pixel_offset = file_header_size + info_header_size
    bmp_bytes = bytearray()
    bmp_bytes.extend(b"BM")
    bmp_bytes.extend((pixel_offset + image_size).to_bytes(4, "little"))
    bmp_bytes.extend(b"\x00\x00\x00\x00")
    bmp_bytes.extend(pixel_offset.to_bytes(4, "little"))
    bmp_bytes.extend(bytes(bmi.bmiHeader))
    bmp_bytes.extend(bytes(buf))

    gdi32.DeleteObject(hbitmap)
    gdi32.DeleteDC(hdc_mem)
    user32.ReleaseDC(0, hdc_screen)
    return bytes(bmp_bytes)


def get_screen_size():
    return user32.GetSystemMetrics(0), user32.GetSystemMetrics(1)


# ── OCR via PowerShell ─────────────────────────────────────────────

OCR_PS_SCRIPT = r"""
param([string]$ImagePath, [string]$Language = "")

Add-Type -TypeDefinition @"
using System;
using System.Threading.Tasks;
using Windows.Media.Ocr;
using Windows.Graphics.Imaging;
using Windows.Storage;
using Windows.Globalization;

public class OcrHelper {
    public static string Recognize(string imagePath, string language) {
        try {
            return RecognizeAsync(imagePath, language).GetAwaiter().GetResult();
        } catch (Exception e) {
            return "{\"error\":\"" + e.Message.Replace("\"", "\\\"") + "\"}";
        }
    }

    static async Task<string> RecognizeAsync(string imagePath, string language) {
        var file = await StorageFile.GetFileFromPathAsync(imagePath);
        var stream = await file.OpenAsync(FileAccessMode.Read);
        var decoder = await BitmapDecoder.CreateAsync(stream);
        var bitmap = await decoder.GetSoftwareBitmapAsync();

        OcrEngine engine;
        if (!string.IsNullOrEmpty(language)) {
            var lang = new Language(language);
            engine = OcrEngine.TryCreateFromLanguage(lang);
            if (engine == null) engine = OcrEngine.TryCreateFromUserProfileLanguages();
        } else {
            engine = OcrEngine.TryCreateFromUserProfileLanguages();
        }
        if (engine == null) return "{\"error\":\"OCR engine unavailable\"}";

        var result = await engine.RecognizeAsync(bitmap);
        var lines = new System.Collections.Generic.List<string>();
        foreach (var line in result.Lines) lines.Add(line.Text);

        var json = new System.Text.StringBuilder();
        json.Append("{\"text\":\"");
        json.Append(result.Text.Replace("\\", "\\\\").Replace("\"", "\\\""));
        json.Append("\",\"lines\":[");
        for (int i = 0; i < lines.Count; i++) {
            if (i > 0) json.Append(",");
            json.Append("\"");
            json.Append(lines[i].Replace("\\", "\\\\").Replace("\"", "\\\""));
            json.Append("\"");
        }
        json.Append("],\"language\":\"");
        json.Append(engine.RecognizerLanguage.LanguageTag);
        json.Append("\",\"word_count\":");
        json.Append(result.Text.Split(new char[]{' '}, StringSplitOptions.RemoveEmptyEntries).Length);
        json.Append("}");
        stream.Dispose();
        return json.ToString();
    }
}
"@

$result = [OcrHelper]::Recognize($ImagePath, $Language)
Write-Output $result
"""


def run_ocr(image_path, language=""):
    """通过 C# 内联代码调用 Windows OCR 识别图片文字。"""
    script_path = os.path.join(tempfile.gettempdir(), f"intools_ocr_{os.getpid()}.ps1")
    try:
        with open(script_path, "w", encoding="utf-8") as f:
            f.write(OCR_PS_SCRIPT)
        cmd = ["powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", script_path, "-ImagePath", image_path]
        if language:
            cmd.extend(["-Language", language])
        result = subprocess.run(cmd, capture_output=True, text=True, encoding="utf-8", timeout=30)
        if result.returncode != 0:
            raise RuntimeError(result.stderr.strip() or "OCR failed")
        output = result.stdout.strip()
        idx = output.find("{")
        if idx >= 0:
            output = output[idx:]
        return json.loads(output)
    except subprocess.TimeoutExpired:
        raise RuntimeError("OCR timeout")
    except json.JSONDecodeError as e:
        raise RuntimeError(f"OCR output parse error: {e}")
    finally:
        try:
            os.unlink(script_path)
        except OSError:
            pass


def run_ocr(image_path, language=""):
    script_path = os.path.join(tempfile.gettempdir(), f"intools_ocr_{os.getpid()}.ps1")
    try:
        with open(script_path, "w", encoding="utf-8") as f:
            f.write(OCR_PS_SCRIPT)
        cmd = ["powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", script_path, "-ImagePath", image_path]
        if language:
            cmd.extend(["-Language", language])
        result = subprocess.run(cmd, capture_output=True, text=True, encoding="utf-8", timeout=30)
        if result.returncode != 0:
            raise RuntimeError(result.stderr.strip() or "OCR 执行失败")
        output = result.stdout.strip()
        idx = output.find("{")
        if idx >= 0:
            output = output[idx:]
        return json.loads(output)
    except subprocess.TimeoutExpired:
        raise RuntimeError("OCR 识别超时（超过 30 秒）")
    except json.JSONDecodeError as e:
        raise RuntimeError(f"OCR 输出解析失败：{e}")
    finally:
        try:
            os.unlink(script_path)
        except OSError:
            pass


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


# ── 工具 ──────────────────────────────────────────────────────────

@plugin.tool(
    "ocr:recognize",
    "截取屏幕指定区域并识别其中的文字",
    {
        "type": "object",
        "properties": {
            "region": {"type": "object", "description": "截图区域 {x, y, width, height}，不传则截取全屏"},
            "language": {"type": "string", "description": "识别语言，如 zh-Hans、en"},
            "copy_to_clipboard": {"type": "boolean", "description": "是否将识别结果复制到剪贴板，默认 true"},
            "save_image": {"type": "boolean", "description": "是否保存截图文件，默认 false"},
        },
    },
)
def do_ocr(args, ctx):
    region = args.get("region")
    if region:
        x, y = int(region.get("x", 0)), int(region.get("y", 0))
        width, height = int(region.get("width", 0)), int(region.get("height", 0))
        if width <= 0 or height <= 0:
            raise PluginError(ErrorCode.INVALID_PARAMS, "区域 width 和 height 必须为正数")
    else:
        x, y = 0, 0
        width, height = get_screen_size()

    language = args.get("language", "") or ""
    copy_to_clipboard = args.get("copy_to_clipboard", True)
    save_image = args.get("save_image", False)

    try:
        bmp_data = capture_screen(x, y, width, height)
    except Exception as e:
        raise PluginError(ErrorCode.INTERNAL, f"截图失败：{e}")

    tmp_dir = tempfile.gettempdir()
    timestamp = int(time.time() * 1000)
    image_path = os.path.join(tmp_dir, f"intools_ocr_{timestamp}.bmp")
    try:
        with open(image_path, "wb") as f:
            f.write(bmp_data)

        try:
            ocr_result = run_ocr(image_path, language)
        except Exception as e:
            return {
                "ok": False, "error": str(e), "image_path": image_path,
                "region": {"x": x, "y": y, "width": width, "height": height},
                "hint": "OCR 识别失败，截图已保存。可尝试安装对应语言的 OCR 语言包。",
            }

        text = ocr_result.get("text", "")
        copied = set_clipboard_text(text) if copy_to_clipboard and text else False

        if not save_image:
            try:
                os.unlink(image_path)
                image_path = None
            except OSError:
                pass

        return {
            "ok": True, "text": text, "lines": ocr_result.get("lines", []),
            "language": ocr_result.get("language", ""), "word_count": ocr_result.get("word_count", 0),
            "region": {"x": x, "y": y, "width": width, "height": height},
            "copied_to_clipboard": copied, "image_path": image_path,
        }
    finally:
        if image_path and os.path.exists(image_path) and not save_image:
            try:
                os.unlink(image_path)
            except OSError:
                pass


plugin.start()
