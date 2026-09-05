"""Caller 测试插件。

能主动发起 host/* 反向 RPC 请求。核心难点在于：当本插件通过 host/callTool
调用其他插件时，宿主可能在同一通道上回发 tools/call 请求（如递归调用自身）。
因此 host_call 不能简单阻塞等待——它必须在等待响应期间继续处理宿主发来的
请求，形成递归事件循环。
"""

import json
import sys

PROTOCOL_VERSION = "1.0"

TOOLS = [
    {
        "name": "caller:call",
        "description": "经 host/callTool 调用指定工具",
        "input_schema": {
            "type": "object",
            "required": ["tool"],
            "properties": {
                "tool": {"type": "string"},
                "arguments": {"type": "object"},
            },
        },
    },
    {
        "name": "caller:list",
        "description": "调用 host/listTools",
        "input_schema": {"type": "object"},
    },
    {
        "name": "caller:config",
        "description": "先 host/setConfig 再 host/getConfig",
        "input_schema": {
            "type": "object",
            "properties": {"value": {}},
        },
    },
    {
        "name": "caller:notify",
        "description": "调用 host/notify",
        "input_schema": {
            "type": "object",
            "properties": {"method": {"type": "string"}},
        },
    },
    {
        "name": "caller:unknown",
        "description": "调用不存在的 host/unknown 方法",
        "input_schema": {"type": "object"},
    },
    {
        "name": "caller:recurse",
        "description": "经 host/callTool 调用自身",
        "input_schema": {"type": "object"},
    },
]

CODE_INVALID_PARAMS = -32602

_next_id = 100


def next_id():
    global _next_id
    val = _next_id
    _next_id += 1
    return val


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


def host_call(method, params):
    """发送 host/* 请求并等待响应。

    等待期间若收到宿主发来的 tools/call 请求（如递归调用），就地处理它——
    处理过程可能再次调用 host_call，形成与调用链深度一致的 Python 调用栈。
    收到自己的响应后立即返回 (result, None) 或 (None, (code, message))。
    """
    rid = next_id()
    write_message({"jsonrpc": "2.0", "id": rid, "method": method, "params": params})

    while True:
        line = sys.stdin.readline()
        if not line:
            return None, (-32603, "连接断开")

        try:
            message = json.loads(line.strip())
        except ValueError:
            log("丢弃无法解析的行：{}".format(line[:200]))
            continue

        msg_id = message.get("id")
        msg_method = message.get("method")

        if msg_method is not None:
            # 宿主发来的请求（如递归 tools/call），就地处理。
            handle_request(msg_id, msg_method, message.get("params") or {})
            continue

        # 响应消息
        if msg_id == rid:
            if "error" in message:
                err = message["error"]
                return None, (err["code"], err["message"])
            return message.get("result"), None

        # 不是自己的响应，忽略（单线程下不会发生，防御性处理）。
        log("忽略无关响应：id={}".format(msg_id))


def handle_request(request_id, method, params):
    """处理宿主发来的请求。返回 True 表示应当退出主循环。"""
    if method == "plugin/ready":
        reply_result(request_id, {"ok": True})
        return False

    if method == "tools/list":
        reply_result(request_id, {"tools": TOOLS})
        return False

    if method == "tools/call":
        name = params.get("name")
        arguments = params.get("arguments") or {}

        if name == "caller:call":
            tool = arguments.get("tool")
            if not isinstance(tool, str):
                reply_error(request_id, CODE_INVALID_PARAMS, "参数 tool 缺失")
                return False
            call_args = arguments.get("arguments") or {}
            result, error = host_call(
                "host/callTool", {"name": tool, "arguments": call_args}
            )
            if error is not None:
                # 把 host 的错误码包在 result 里返回，而非用 reply_error——
                # 否则宿主会把它包成 InstanceError::Plugin → CODE_PLUGIN_ERROR，
                # 原始错误码（如 -32002 权限被拒）就丢失了。
                reply_result(
                    request_id, {"host_error": {"code": error[0], "message": error[1]}}
                )
            else:
                reply_result(request_id, result)
            return False

        if name == "caller:list":
            result, error = host_call("host/listTools", {})
            if error is not None:
                reply_error(request_id, error[0], error[1])
            else:
                reply_result(request_id, result)
            return False

        if name == "caller:config":
            value = arguments.get("value", {"mode": "test"})
            _, err = host_call("host/setConfig", value)
            if err is not None:
                reply_error(request_id, err[0], err[1])
                return False
            result, err = host_call("host/getConfig", {})
            if err is not None:
                reply_error(request_id, err[0], err[1])
            else:
                reply_result(request_id, result)
            return False

        if name == "caller:notify":
            notify_method = arguments.get("method", "progress")
            _, err = host_call(
                "host/notify", {"method": notify_method, "params": {"pct": 50}}
            )
            if err is not None:
                reply_error(request_id, err[0], err[1])
            else:
                reply_result(request_id, {"ok": True})
            return False

        if name == "caller:unknown":
            _, err = host_call("host/unknownMethod", {})
            if err is not None:
                reply_result(
                    request_id, {"host_error": {"code": err[0], "message": err[1]}}
                )
            else:
                reply_result(request_id, {"unexpected": "success"})
            return False

        if name == "caller:recurse":
            result, error = host_call(
                "host/callTool", {"name": "caller:recurse", "arguments": {}}
            )
            if error is not None:
                reply_result(
                    request_id, {"host_error": {"code": error[0], "message": error[1]}}
                )
            else:
                # 递归调用成功返回了（理论上不会发生，深度上限会先触发）
                reply_result(request_id, result or {"ok": True})
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
            # 响应消息由 host_call 自行消费，主循环不应看到它。
            continue
        if request_id is None:
            continue
        if handle_request(request_id, method, message.get("params") or {}):
            break

    log("caller 退出")


if __name__ == "__main__":
    main()
