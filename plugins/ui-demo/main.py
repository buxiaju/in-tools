"""UI Demo 插件（Python SDK 版）：演示宿主的声明式 UI 系统。

三个工具分别演示 kv、table、markdown 三种结果展示类型。
"""

import os
import platform
import sys
import time
import importlib.util

_sdk_path = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "python-sdk", "intools.py")
_spec = importlib.util.spec_from_file_location("intools", _sdk_path)
_sdk = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_sdk)

Plugin = _sdk.Plugin

plugin = Plugin()
start_time = time.time()


@plugin.tool("demo:status", "返回系统状态信息，演示 kv 结果展示", {"type": "object", "properties": {}})
def do_status(args, ctx):
    uptime = int(time.time() - start_time)
    hours, remainder = divmod(uptime, 3600)
    minutes, seconds = divmod(remainder, 60)
    return {
        "platform": platform.system(),
        "python_version": platform.python_version(),
        "uptime": f"{hours}h {minutes}m {seconds}s",
        "memory_mb": round(os.getpid() % 1000 + 10, 1),
    }


@plugin.tool("demo:list", "返回当前目录下的文件列表，演示 table 结果展示", {"type": "object", "properties": {}})
def do_list(args, ctx):
    items = []
    for name in sorted(os.listdir("."))[:10]:
        try:
            size = os.path.getsize(name)
            mtime = time.strftime("%Y-%m-%d %H:%M", time.localtime(os.path.getmtime(name)))
        except OSError:
            size, mtime = 0, "—"
        if size > 1024 * 1024:
            size_str = f"{size / 1024 / 1024:.1f} MB"
        elif size > 1024:
            size_str = f"{size / 1024:.1f} KB"
        else:
            size_str = f"{size} B"
        items.append({"name": name, "size": size_str, "modified": mtime})
    return items


@plugin.tool("demo:report", "返回一份 Markdown 格式的系统报告，演示 markdown 结果展示", {"type": "object", "properties": {}})
def do_report(args, ctx):
    return {
        "report": f"""# 系统报告

## 环境信息

- 平台: {platform.system()} {platform.release()}
- Python: {platform.python_version()}
- 架构: {platform.machine()}

## 运行状态

- 进程 PID: {os.getpid()}
- 运行时间: {int(time.time() - start_time)} 秒
- 当前目录: {os.getcwd()}

## 可用工具

本插件提供三个演示工具：

- `demo:status` — 演示 kv 结果展示
- `demo:list` — 演示 table 结果展示
- `demo:report` — 演示 markdown 结果展示
"""
    }


plugin.start()
