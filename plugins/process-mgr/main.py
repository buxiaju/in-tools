"""进程管理插件。

列出/搜索/终止进程，启动程序，执行 shell 命令。
仅使用 Python 标准库 + ctypes（Windows API）。
"""

import ctypes
import os
import subprocess
import sys
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


def _wmic_query(fields, where=""):
    """执行 WMIC 查询并解析 CSV 输出。"""
    cmd = f"wmic process get {fields} /format:csv"
    if where:
        cmd = f"wmic process where \"{where}\" get {fields} /format:csv"
    try:
        result = subprocess.run(cmd, shell=True, capture_output=True, text=True, encoding="utf-8", errors="replace", timeout=10)
        lines = [l.strip() for l in result.stdout.strip().split("\n") if l.strip()]
        if len(lines) < 2:
            return []
        headers = lines[0].split(",")
        rows = []
        for line in lines[1:]:
            cols = line.split(",")
            if len(cols) >= len(headers):
                rows.append(dict(zip(headers, cols)))
        return rows
    except Exception:
        return []


@plugin.tool(
    "proc:list",
    "列出当前运行的进程",
    {
        "type": "object",
        "properties": {
            "keyword": {"type": "string", "description": "按进程名过滤（不区分大小写）"},
            "max_results": {"type": "integer", "description": "最多返回条数，默认 50"},
        },
    },
)
def do_list(args, ctx):
    keyword = (args.get("keyword") or "").lower()
    max_results = min(args.get("max_results", 50), 200)
    rows = _wmic_query("ProcessId,Name,ExecutablePath,WorkingSetSize")
    procs = []
    for r in rows:
        name = r.get("Name", "")
        if keyword and keyword not in name.lower():
            continue
        try:
            pid = int(r.get("ProcessId", 0))
            mem_kb = int(r.get("WorkingSetSize", 0)) // 1024
        except ValueError:
            continue
        procs.append({
            "pid": pid,
            "name": name,
            "path": r.get("ExecutablePath", ""),
            "memory_kb": mem_kb,
            "memory_mb": round(mem_kb / 1024, 1),
        })
        if len(procs) >= max_results:
            break
    return {"count": len(procs), "processes": procs}


@plugin.tool(
    "proc:kill",
    "终止指定进程",
    {
        "type": "object",
        "required": ["pid"],
        "properties": {
            "pid": {"type": "integer", "description": "进程 ID"},
            "force": {"type": "boolean", "description": "是否强制终止，默认 true"},
        },
    },
)
def do_kill(args, ctx):
    pid = args["pid"]
    force = args.get("force", True)
    flag = "/F" if force else ""
    try:
        result = subprocess.run(f"taskkill {flag} /PID {pid}", shell=True, capture_output=True, text=True, timeout=5)
        if result.returncode != 0:
            raise PluginError(ErrorCode.INTERNAL, f"终止进程失败：{result.stderr.strip()}")
    except subprocess.TimeoutExpired:
        raise PluginError(ErrorCode.INTERNAL, "终止进程超时")
    return {"ok": True, "pid": pid}


@plugin.tool(
    "proc:start",
    "启动程序或打开文件/文件夹/网址",
    {
        "type": "object",
        "required": ["target"],
        "properties": {
            "target": {"type": "string", "description": "程序路径、文件路径、文件夹或网址"},
            "args": {"type": "string", "description": "命令行参数"},
            "working_dir": {"type": "string", "description": "工作目录"},
        },
    },
)
def do_start(args, ctx):
    target = args["target"]
    cmd_args = args.get("args", "")
    working_dir = args.get("working_dir")
    try:
        if target.startswith(("http://", "https://")):
            os.startfile(target)
        elif os.path.isdir(target):
            os.startfile(target)
        elif os.path.isfile(target):
            os.startfile(target)
        else:
            cmd = f"{target} {cmd_args}".strip()
            subprocess.Popen(cmd, shell=True, cwd=working_dir)
    except Exception as e:
        raise PluginError(ErrorCode.INTERNAL, f"启动失败：{e}")
    return {"ok": True, "target": target}


@plugin.tool(
    "proc:exec",
    "执行 shell 命令并返回输出",
    {
        "type": "object",
        "required": ["command"],
        "properties": {
            "command": {"type": "string", "description": "要执行的命令"},
            "timeout": {"type": "integer", "description": "超时秒数，默认 30"},
            "working_dir": {"type": "string", "description": "工作目录"},
        },
    },
)
def do_exec(args, ctx):
    command = args["command"]
    timeout = min(args.get("timeout", 30), 120)
    working_dir = args.get("working_dir")
    try:
        result = subprocess.run(
            command, shell=True, capture_output=True, text=True,
            encoding="utf-8", errors="replace", timeout=timeout, cwd=working_dir,
        )
        return {
            "stdout": result.stdout[:50000],
            "stderr": result.stderr[:10000],
            "returncode": result.returncode,
            "ok": result.returncode == 0,
        }
    except subprocess.TimeoutExpired:
        raise PluginError(ErrorCode.INTERNAL, f"命令执行超时（{timeout}秒）")


plugin.start()
