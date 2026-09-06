"""Responder 测试插件。

协议与 hello-plugin 相同：逐行 JSON-RPC 2.0，stdin 收 stdout 发。
唯一区别是声明了 screen:capture 权限——这样调用方在 DenyAlways 下
会被宿主权限层拦截，用于验证「插件经 host/callTool 调用时不绕开权限」。
"""

import json
import sys

PROTOCOL_VERSION = "1.0"

TOOLS = [
    {
        "name": "responder:echo",
        "description": "原样返回传入文本",
        "input_schema": {
            "type": "object",
            "required": ["text"],
            "properties": {
                "text": {"type": "string", "description": "要回显的文本"},
            },
        },
    },
]

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


def handle_request(request_id, method, params):
    if method == "plugin/ready":
        reply_result(request_id, {"ok": True})
        return False

    if method == "tools/list":
        reply_result(request_id, {"tools": TOOLS})
        return False

    if method == "tools/call":
        name = params.get("name")
        arguments = params.get("arguments") or {}
        if name == "responder:echo":
            text = arguments.get("text")
            if not isinstance(text, str):
                reply_error(request_id, CODE_INVALID_PARAMS, "参数 text 缺失或不是字符串")
                return False
            reply_result(request_id, {"text": text})
            return False
        reply_error(request_id, -32601, "未知工具：{}".format(name))
        return False

    if method == "plugin/shutdown":
        reply_result(request_id, {"ok": True})
        return True

    reply_error(request_id, -32601, "未知方法：{}".format(method))
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
            log("丢弃无法解析的输入行：{}".format(line[:200]))
            continue

        request_id = message.get("id")
        method = message.get("method")
        if method is None:
            continue
        if request_id is None:
            continue
        if handle_request(request_id, method, message.get("params") or {}):
            break

    log("responder 退出")


if __name__ == "__main__":
    main()
