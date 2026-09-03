"""InTools 最小示范插件。

协议是逐行 JSON-RPC 2.0：stdin 收，stdout 发，一行一条消息。
stdout 只允许出现协议消息，任何调试输出都必须走 stderr——
宿主会把 stdout 的每一行都当协议帧解析，一句 print 就能污染整条链路。
"""

import json
import os
import sys

PROTOCOL_VERSION = "1.0"

TOOLS = [
    {
        "name": "hello:echo",
        "description": "原样返回传入的文本，用于验证宿主与插件之间的调用链路",
        "input_schema": {
            "type": "object",
            "required": ["text"],
            "properties": {
                "text": {"type": "string", "description": "要回显的文本"}
            },
        },
    },
    {
        "name": "hello:crash",
        "description": "立即以非零码退出，用于验证宿主的崩溃处理与待处理请求失败逻辑",
        "input_schema": {"type": "object", "properties": {}},
    },
]

CODE_METHOD_NOT_FOUND = -32601
CODE_INVALID_PARAMS = -32602


def log(message):
    """写 stderr。宿主会把 stderr 全量转存到日志文件，供插件作者排查。"""
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
        {
            "jsonrpc": "2.0",
            "id": request_id,
            "error": {"code": code, "message": message},
        }
    )


def call_tool(params):
    """执行一次工具调用，返回 (result, error)，两者恰有一个非 None。"""
    name = params.get("name")
    arguments = params.get("arguments") or {}

    if name == "hello:echo":
        text = arguments.get("text")
        if not isinstance(text, str):
            return None, (CODE_INVALID_PARAMS, "参数 text 缺失或不是字符串")
        return {"text": text}, None

    if name == "hello:crash":
        log("收到 hello:crash，即将以退出码 3 终止")
        # os._exit 绕过 atexit 与缓冲区刷写，模拟插件真正的意外死亡。
        # 用 sys.exit 会走正常清理流程，测不出「进程猝死时待处理请求如何收场」。
        os._exit(3)

    return None, (CODE_METHOD_NOT_FOUND, "未知工具：{}".format(name))


def handle_request(request_id, method, params):
    """处理一条带 id 的请求。返回 True 表示应当退出主循环。"""
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
    # 协议规定 UTF-8。Windows 上 Python 面对管道时默认用系统代码页（常见 GBK），
    # 写中文会直接抛 UnicodeEncodeError，且只在部分机器上复现。钉死编码消除这种环境依赖。
    sys.stdin.reconfigure(encoding="utf-8")
    sys.stdout.reconfigure(encoding="utf-8")
    sys.stderr.reconfigure(encoding="utf-8")

    # 握手第一步由插件主动发起：宿主 spawn 完成后就在等这条通知。
    notify("plugin/hello", {"protocol_version": PROTOCOL_VERSION, "tools": TOOLS})

    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue

        try:
            message = json.loads(line)
        except ValueError:
            # 无法解析就拿不到 id，也就无从回错误响应，只能记日志后跳过。
            log("丢弃无法解析的输入行：{}".format(line[:200]))
            continue

        request_id = message.get("id")
        method = message.get("method")

        if method is None:
            # 没有 method 的是响应（宿主对反向 RPC 的回复），本插件不发反向 RPC，忽略。
            continue

        if request_id is None:
            log("忽略通知：{}".format(method))
            continue

        if handle_request(request_id, method, message.get("params") or {}):
            break

    # 走到这里有两种可能：收到 plugin/shutdown，或 stdin 被关闭送来 EOF。
    # 后者是宿主丢弃 stdin 句柄的结果，同样意味着该退出了。
    log("插件退出")


if __name__ == "__main__":
    main()
