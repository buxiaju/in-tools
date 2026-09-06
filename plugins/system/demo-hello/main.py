"""示范插件。"""

import json
import sys

PROTOCOL_VERSION = "1.0"

TOOLS = [
    {
        "name": "demo:hello:echo",
        "description": "原样返回传入的文本",
        "input_schema": {
            "type": "object",
            "required": ["text"],
            "properties": {
                "text": {"type": "string", "description": "要回显的文本"}
            },
        },
    },
    {
        "name": "demo:hello:info",
        "description": "返回插件版本与运行环境信息",
        "input_schema": {"type": "object", "properties": {}},
    },
]

CODE_METHOD_NOT_FOUND = -32601
CODE_INVALID_PARAMS = -32602


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

    if name == "demo:hello:echo":
        text = arguments.get("text")
        if not isinstance(text, str):
            return None, (CODE_INVALID_PARAMS, "参数 text 缺失或不是字符串")
        return {"text": text}, None

    if name == "demo:hello:info":
        import platform
        return {
            "plugin": "demo-hello",
            "version": "1.0.0",
            "runtime": f"Python {platform.python_version()}",
        }, None

    return None, (CODE_METHOD_NOT_FOUND, f"未知工具: {name}")


def handle_request(request_id, method, params):
    if method == "plugin/ready":
        log(f"握手完成，插件目录：{params.get('plugin_dir')}")
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

    reply_error(request_id, CODE_METHOD_NOT_FOUND, f"未知方法: {method}")
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
            log(f"丢弃无法解析的输入行: {line[:200]}")
            continue

        request_id = message.get("id")
        method = message.get("method")

        if method is None:
            continue

        if request_id is None:
            log(f"忽略通知: {method}")
            continue

        if handle_request(request_id, method, message.get("params") or {}):
            break

    log("插件退出")


if __name__ == "__main__":
    main()
