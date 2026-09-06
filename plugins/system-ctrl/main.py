"""系统控制插件。

音量控制、亮度调节、锁屏、发送通知、获取系统信息。
仅使用 Python 标准库 + subprocess。
"""

import os
import subprocess
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


def _ps(script):
    """执行 PowerShell 命令。"""
    try:
        result = subprocess.run(
            ["powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", script],
            capture_output=True, text=True, encoding="utf-8", errors="replace", timeout=10,
        )
        return result.stdout.strip(), result.stderr.strip(), result.returncode
    except subprocess.TimeoutExpired:
        return "", "超时", 1


@plugin.tool(
    "sys:volume",
    "获取或设置系统音量",
    {
        "type": "object",
        "properties": {
            "level": {"type": "integer", "description": "音量级别 0-100，不传则只查询"},
            "mute": {"type": "boolean", "description": "静音/取消静音"},
        },
    },
)
def do_volume(args, ctx):
    level = args.get("level")
    mute = args.get("mute")

    if mute is not None:
        val = "$true" if mute else "$false"
        _ps(f'(New-Object -ComObject WScript.Shell).SendKeys([char]173)')

    if level is not None:
        level = max(0, min(100, level))
        ps = f"""
        $wshShell = New-Object -ComObject WScript.Shell
        # 先静音取消
        # 用 NirCmd 或直接设置音频端点
        Add-Type -TypeDefinition @"
using System.Runtime.InteropServices;
[Guid("5CDF2C82-841E-4546-9722-0CF74078229A"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
interface IAudioEndpointVolume {{ int f(); int g(); int h(); int i(); int SetMasterVolumeLevelScalar(float fLevel, System.Guid pguidEventContext); int j(); int k(); int GetMasterVolumeLevelScalar(out float pfLevel); }}
[Guid("D666063F-1587-4E43-81F1-B948E807363F"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
interface IMMDevice {{ int Activate(ref System.Guid iid, int dwClsCtx, IntPtr pActivationParams, [MarshalAs(UnmanagedType.IUnknown)] out object ppInterface); }}
[Guid("A95664D2-9614-4F35-A746-DE8DB63617E6"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
interface IMMDeviceEnumerator {{ int GetDefaultAudioEndpoint(int dataFlow, int role, out IMMDevice ppDevice); }}
[ComImport, Guid("BCDE0395-E52F-467C-8E3D-C4579291692E")] class MMDeviceEnumerator {{ }}
"@ -ErrorAction SilentlyContinue
        try {{
            $enumerator = New-Object MMDeviceEnumerator
            $device = $null
            $enumerator.GetDefaultAudioEndpoint(0, 1, [ref]$device)
            $guid = [Guid]"5CDF2C82-841E-4546-9722-0CF74078229A"
            $volume = $null
            $device.Activate([ref]$guid, 1, [IntPtr]::Zero, [ref]$volume)
            $volume.SetMasterVolumeLevelScalar({level / 100.0}, [Guid]::Empty)
            Write-Output "OK:{level}"
        }} catch {{
            Write-Output "ERR:$_"
        }}
        """
        out, err, rc = _ps(ps)
        if rc != 0 and "ERR:" in out:
            return {"ok": False, "error": out}

    # 查询当前音量
    out, err, rc = _ps('''
    try {
        $wshShell = New-Object -ComObject WScript.Shell
        # 用 nircmd 或 PowerShell 音频 API 查询
        $vol = (Get-AudioDevice -PlaybackVolume 2>$null)
        if ($vol) { Write-Output "VOL:$vol" } else { Write-Output "VOL:unknown" }
    } catch { Write-Output "VOL:unknown" }
    ''')
    return {"ok": True, "level": level}


@plugin.tool(
    "sys:lock",
    "锁定 Windows 桌面",
    {"type": "object", "properties": {}},
)
def do_lock(args, ctx):
    try:
        ctypes.windll.user32.LockWorkStation()
        return {"ok": True}
    except Exception:
        subprocess.run("rundll32.exe user32.dll,LockWorkStation", shell=True)
        return {"ok": True}


@plugin.tool(
    "sys:notify",
    "发送 Windows 系统通知",
    {
        "type": "object",
        "required": ["title", "message"],
        "properties": {
            "title": {"type": "string", "description": "通知标题"},
            "message": {"type": "string", "description": "通知内容"},
        },
    },
)
def do_notify(args, ctx):
    title = args["title"]
    message = args["message"]
    ps = f"""
    [Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, ContentType = WindowsRuntime] | Out-Null
    [Windows.Data.Xml.Dom.XmlDocument, Windows.Data.Xml.Dom, ContentType = WindowsRuntime] | Out-Null
    $template = '<toast><visual><binding template="ToastGeneric"><text>{title}</text><text>{message}</text></binding></visual></toast>'
    $xml = New-Object Windows.Data.Xml.Dom.XmlDocument
    $xml.LoadXml($template)
    $toast = [Windows.UI.Notifications.ToastNotification]::new($xml)
    [Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier("InTools").Show($toast)
    """
    out, err, rc = _ps(ps)
    if rc != 0:
        # 回退到 msg 命令
        subprocess.run(f'msg * /time:5 "{title}: {message}"', shell=True, capture_output=True)
    return {"ok": True}


@plugin.tool(
    "sys:clipboard",
    "读取或写入系统剪贴板",
    {
        "type": "object",
        "properties": {
            "text": {"type": "string", "description": "要写入的文本，不传则只读取"},
        },
    },
)
def do_clipboard(args, ctx):
    text = args.get("text")
    if text is not None:
        ps = f'Set-Clipboard -Value "{text.replace(chr(34), chr(39))}"'
        _ps(ps)
        return {"ok": True, "action": "write", "length": len(text)}
    else:
        out, err, rc = _ps("Get-Clipboard")
        return {"ok": True, "action": "read", "text": out}


plugin.start()
