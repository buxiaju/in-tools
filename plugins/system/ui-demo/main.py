"""UI Demo 插件：演示宿主的声明式 UI 系统。"""
import json
import os
import sys
import time
import platform

PROTOCOL_VERSION = "1.0"

TOOLS = [
    {
        "name": "demo:status",
        "description": "返回系统状态信息",
        "input_schema": {"type": "object"},
    },
    {
        "name": "demo:list",
        "description": "返回当前目录下的文件列表",
        "input_schema": {"type": "object"},
    },
    {
        "name": "demo:report",
        "description": "返回一份 Markdown 格式的系统报告",
        "input_schema": {"type": "object"},
    },
]

start_time = time.time()


def log(msg):
    print(msg, file=sys.stderr, flush=True)


def write_message(payload):
    sys.stdout.write(json.dumps(payload, ensure_ascii=False) + "\n")
    sys.stdout.flush()


def notify(method, params):
    write_message({"jsonrpc": "2.0", "method": method, "params": params})


def reply_result(request_id, result):
    write_message({"jsonrpc": "2.0", "id": request_id, "result": result})


def reply_error(request_id, code, message):
    write_message({"jsonrpc": "2.0", "id": request_id, "error": {"code": code, "message": message}})


def handle_status():
    uptime = int(time.time() - start_time)
    hours, remainder = divmod(uptime, 3600)
    minutes, seconds = divmod(remainder, 60)
    return {
        "platform": platform.system(),
        "python_version": platform.python_version(),
        "uptime": f"{hours}h {minutes}m {seconds}s",
        "memory_mb": round(os.getpid() % 1000 + 10, 1),
    }


def handle_list():
    items = []
    for name in sorted(os.listdir("."))[:10]:
        try:
            size = os.path.getsize(name)
            mtime = time.strftime("%Y-%m-%d %H:%M", time.localtime(os.path.getmtime(name)))
        except OSError:
            size = 0
            mtime = "—"
        if size > 1024 * 1024:
            size_str = f"{size / 1024 / 1024:.1f} MB"
        elif size > 1024:
            size_str = f"{size / 1024:.1f} KB"
        else:
            size_str = f"{size} B"
        items.append({"name": name, "size": size_str, "modified": mtime})
    return items


def handle_report():
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


def call_tool(params):
    name = params.get("name")
    if name == "demo:status":
        return handle_status(), None
    if name == "demo:list":
        return handle_list(), None
    if name == "demo:report":
        return handle_report(), None
    return None, (-32601, f"未知工具: {name}")


def handle_request(request_id, method, params):
    if method == "plugin/ready":
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
    reply_error(request_id, -32601, f"未知方法: {method}")
    return False


def main():
    sys.stdin.reconfigure(encoding="utf-8")
    sys.stdout.reconfigure(encoding="utf-8")
    sys.stderr.reconfigure(encoding="utf-8")

    notify("plugin/hello", {"protocol_version": PROTOCOL_VERSION, "tools": TOOLS})

    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            message = json.loads(line)
        except ValueError:
            log(f"丢弃无法解析的输入行：{line[:200]}")
            continue
        request_id = message.get("id")
        method = message.get("method")
        if method is None:
            continue
        if request_id is None:
            log(f"忽略通知：{method}")
            continue
        if handle_request(request_id, method, message.get("params") or {}):
            break

    log("插件退出")


if __name__ == "__main__":
    main()
