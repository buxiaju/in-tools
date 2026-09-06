"""应用启动器插件。

打开文件、文件夹、网址，搜索已安装应用，管理启动项。
仅使用 Python 标准库。
"""

import os
import subprocess
import sys
import winreg
import importlib.util

_sdk_path = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "python-sdk", "intools.py")
_spec = importlib.util.spec_from_file_location("intools", _sdk_path)
_sdk = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_sdk)

Plugin = _sdk.Plugin
PluginError = _sdk.PluginError
ErrorCode = _sdk.ErrorCode

plugin = Plugin()


def _open_target(target):
    """打开文件/文件夹/网址。"""
    if target.startswith(("http://", "https://", "www.")):
        if not target.startswith(("http://", "https://")):
            target = "https://" + target
        os.startfile(target)
    elif os.path.exists(target):
        os.startfile(target)
    else:
        # 尝试作为程序名启动
        subprocess.Popen(target, shell=True)


def _search_installed_apps(keyword):
    """搜索已安装应用（从注册表和开始菜单）。"""
    apps = []
    keyword_lower = keyword.lower()
    seen = set()

    # 从注册表搜索
    for root_key in [winreg.HKEY_LOCAL_MACHINE, winreg.HKEY_CURRENT_USER]:
        for subkey in [
            r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall",
            r"SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall",
        ]:
            try:
                key = winreg.OpenKey(root_key, subkey)
                for i in range(winreg.QueryInfoKey(key)[0]):
                    try:
                        sub = winreg.OpenKey(key, winreg.EnumKey(key, i))
                        name = winreg.QueryValueEx(sub, "DisplayName")[0]
                        if keyword_lower in name.lower() and name not in seen:
                            seen.add(name)
                            location = ""
                            try:
                                location = winreg.QueryValueEx(sub, "InstallLocation")[0]
                            except FileNotFoundError:
                                pass
                            apps.append({"name": name, "location": location})
                    except (FileNotFoundError, OSError):
                        continue
                winreg.CloseKey(key)
            except OSError:
                continue

    return apps[:20]


@plugin.tool(
    "app:open",
    "打开文件、文件夹或网址",
    {
        "type": "object",
        "required": ["target"],
        "properties": {
            "target": {"type": "string", "description": "文件路径、文件夹路径或网址"},
        },
    },
)
def do_open(args, ctx):
    target = args["target"]
    try:
        _open_target(target)
    except Exception as e:
        raise PluginError(ErrorCode.INTERNAL, f"打开失败：{e}")
    return {"ok": True, "target": target}


@plugin.tool(
    "app:search",
    "搜索已安装的应用程序",
    {
        "type": "object",
        "required": ["keyword"],
        "properties": {
            "keyword": {"type": "string", "description": "应用名称关键词"},
        },
    },
)
def do_search(args, ctx):
    keyword = args["keyword"]
    apps = _search_installed_apps(keyword)
    return {"keyword": keyword, "count": len(apps), "apps": apps}


@plugin.tool(
    "app:explorer",
    "在文件资源管理器中打开并选中指定文件",
    {
        "type": "object",
        "required": ["path"],
        "properties": {
            "path": {"type": "string", "description": "文件或文件夹路径"},
        },
    },
)
def do_explorer(args, ctx):
    path = args["path"]
    if not os.path.exists(path):
        raise PluginError(ErrorCode.INVALID_PARAMS, f"路径不存在：{path}")
    try:
        subprocess.run(["explorer", "/select,", path], check=False)
    except Exception as e:
        raise PluginError(ErrorCode.INTERNAL, f"打开资源管理器失败：{e}")
    return {"ok": True, "path": path}


plugin.start()
