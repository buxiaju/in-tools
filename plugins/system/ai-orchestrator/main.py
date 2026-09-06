"""AI 编排插件。

对接 OpenAI 兼容 API（DeepSeek、Ollama 等），自动调用其他插件的工具完成任务。
仅使用 Python 标准库（urllib）。

工作流程：
1. 收到 ai:chat 调用 → 读配置（base_url / api_key / model）
2. 调 host/listTools 获取可用工具，转为 OpenAI function calling 格式
3. 向 LLM 发送用户消息 + 工具列表
4. 若 LLM 返回 tool_calls，逐个调 host/callTool 执行
5. 将工具结果喂回 LLM，重复直到 LLM 给出最终文本或达到 10 轮上限
6. 全程经 notify/stream、notify/toolCall、notify/streamEnd 推送进度
"""

import json
import sys
import urllib.request
import urllib.error

PROTOCOL_VERSION = "1.0"

MAX_TOOL_ROUNDS = 10
REQUEST_TIMEOUT = 120  # 秒，与 manifest 的 request_timeout_sec 对齐

AI_CHAT_TOOL = {
    "name": "ai:chat",
    "description": "向 AI 发送消息，AI 会自动选择并调用可用工具来完成任务",
    "input_schema": {
        "type": "object",
        "required": ["message"],
        "properties": {
            "message": {"type": "string", "description": "用户的自然语言消息"}
        },
    },
}

AI_CHAT_WITH_HISTORY_TOOL = {
    "name": "ai:chat-with-history",
    "description": "向 AI 发送消息，支持多轮对话历史",
    "input_schema": {
        "type": "object",
        "required": ["message"],
        "properties": {
            "message": {"type": "string", "description": "用户的自然语言消息"},
            "history": {
                "type": "array",
                "description": "对话历史（可选）",
                "items": {
                    "type": "object",
                    "properties": {
                        "role": {"type": "string", "enum": ["user", "assistant", "tool"]},
                        "content": {"type": "string"},
                    },
                },
            },
            "system_prompt": {"type": "string", "description": "自定义系统提示词（可选）"},
        },
    },
}

TOOLS = [AI_CHAT_TOOL, AI_CHAT_WITH_HISTORY_TOOL]

CODE_METHOD_NOT_FOUND = -32601
CODE_INVALID_PARAMS = -32602
CODE_INTERNAL_ERROR = -32603

_next_id = 1000


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


# ─────────────────── 反向 RPC ───────────────────


def host_call(method, params):
    """发送 host/* 请求并等待响应。

    等待期间若收到宿主发来的请求，就地处理——与 caller-plugin 同模式。
    返回 (result, error)，两者恰有一个非 None。
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
            handle_request(msg_id, msg_method, message.get("params") or {})
            continue

        if msg_id == rid:
            if "error" in message:
                err = message["error"]
                return None, (err["code"], err["message"])
            return message.get("result"), None

        log("忽略无关响应：id={}".format(msg_id))


# ─────────────────── HTTP ───────────────────


def http_post(url, api_key, body):
    """向 LLM API 发送 POST 请求，返回解析后的 JSON 响应。

    错误时抛 RuntimeError，消息可直接展示给用户。
    """
    data = json.dumps(body, ensure_ascii=False).encode("utf-8")
    req = urllib.request.Request(
        url,
        data=data,
        headers={
            "Content-Type": "application/json",
            "Authorization": "Bearer {}".format(api_key),
        },
        method="POST",
    )
    try:
        with urllib.request.urlopen(req, timeout=REQUEST_TIMEOUT) as resp:
            raw = resp.read().decode("utf-8")
            return json.loads(raw)
    except urllib.error.HTTPError as e:
        body_text = ""
        try:
            body_text = e.read().decode("utf-8", errors="replace")[:500]
        except Exception:
            pass
        if e.code == 401:
            raise RuntimeError("API 密钥无效（HTTP 401），请在设置页检查 AI 配置")
        raise RuntimeError("API 返回 HTTP {}：{}".format(e.code, body_text))
    except urllib.error.URLError as e:
        raise RuntimeError("无法连接 API（{}），请检查 base_url 和网络".format(e.reason))


# ─────────────────── 工具转换 ───────────────────


def to_function_name(tool_name):
    """把工具名转为合法的 OpenAI function name（仅字母数字下划线连字符）。"""
    return tool_name.replace(":", "_")


def tools_to_functions(tools):
    """把 host/listTools 返回的工具列表转为 OpenAI function calling 格式。

    排除 ai:chat 自身，防止 LLM 递归调用。
    """
    functions = []
    for tool in tools:
        name = tool.get("name", "")
        if name == "ai:chat":
            continue
        functions.append(
            {
                "type": "function",
                "function": {
                    "name": to_function_name(name),
                    "description": tool.get("description", ""),
                    "parameters": tool.get("input_schema", {"type": "object"}),
                },
            }
        )
    return functions


def function_name_to_tool(func_name, tools):
    """把 function name 反解回原始工具名。"""
    for tool in tools:
        if to_function_name(tool.get("name", "")) == func_name:
            return tool["name"]
    return None


# ─────────────────── 核心对话循环 ───────────────────


def run_chat(user_message, config, tools, history=None, system_prompt=None):
    """执行一次 AI 对话，含多轮工具调用。返回 (response_text, error)。"""
    base_url = config.get("base_url", "").rstrip("/")
    api_key = config.get("api_key", "")
    model = config.get("model", "deepseek-chat")

    if not base_url:
        return None, "未配置 base_url，请在设置页配置 AI API"
    if not api_key:
        return None, "未配置 api_key，请在设置页配置 AI API"

    functions = tools_to_functions(tools)
    chat_url = "{}/chat/completions".format(base_url)

    # 构建消息列表
    messages = []

    # 添加系统提示词
    if system_prompt:
        messages.append({"role": "system", "content": system_prompt})
    else:
        messages.append({
            "role": "system",
            "content": (
                "你是一个智能助手，可以通过调用工具来帮助用户完成任务。"
                "请根据用户的需求选择合适的工具。如果不需要工具，直接回答即可。"
                "工具调用结果会用中文返回，请基于结果给出自然、简洁的回答。"
            ),
        })

    # 添加对话历史
    if history:
        for msg in history:
            if isinstance(msg, dict) and "role" in msg and "content" in msg:
                messages.append(msg)

    # 添加当前用户消息
    messages.append({"role": "user", "content": user_message})

    for round_num in range(MAX_TOOL_ROUNDS):
        body = {
            "model": model,
            "messages": messages,
            "tools": functions if functions else None,
        }
        # tools 为空时不发送 tools 字段，避免某些 API 报错
        if not functions:
            body.pop("tools", None)

        try:
            resp = http_post(chat_url, api_key, body)
        except RuntimeError as e:
            return None, str(e)

        choices = resp.get("choices", [])
        if not choices:
            return None, "API 返回了空响应"

        msg = choices[0].get("message", {})
        content = msg.get("content") or ""
        tool_calls = msg.get("tool_calls") or []

        # 追加 assistant 消息（含 tool_calls 信息）
        messages.append(msg)

        if not tool_calls:
            # 没有工具调用，这是最终回答
            notify("notify/stream", {"delta": content})
            notify("notify/streamEnd", {})
            return content, None

        # 有工具调用，逐个执行
        for tc in tool_calls:
            func = tc.get("function", {})
            func_name = func.get("name", "")
            raw_args = func.get("arguments", "{}")

            try:
                args = json.loads(raw_args) if isinstance(raw_args, str) else raw_args
            except (json.JSONDecodeError, ValueError):
                args = {}

            original_name = function_name_to_tool(func_name, tools)
            if original_name is None:
                tool_result = {"error": "未知工具：{}".format(func_name)}
            else:
                notify(
                    "notify/toolCall",
                    {"tool": original_name, "status": "running", "arguments": args},
                )

                result, error = host_call(
                    "host/callTool",
                    {"name": original_name, "arguments": args},
                )

                if error is not None:
                    tool_result = {"error": error[1]}
                    notify(
                        "notify/toolCall",
                        {"tool": original_name, "status": "error", "error": error[1]},
                    )
                else:
                    tool_result = result
                    notify(
                        "notify/toolCall",
                        {"tool": original_name, "status": "done", "result": result},
                    )

            messages.append(
                {
                    "role": "tool",
                    "tool_call_id": tc.get("id", ""),
                    "content": json.dumps(tool_result, ensure_ascii=False),
                }
            )

    # 达到上限
    summary = "已达到工具调用轮数上限（{}轮），以下是最新的进展。".format(MAX_TOOL_ROUNDS)
    notify("notify/stream", {"delta": summary})
    notify("notify/streamEnd", {})
    return summary, None


# ─────────────────── 工具调用分发 ───────────────────


def call_tool(params):
    name = params.get("name")
    arguments = params.get("arguments") or {}

    if name not in ("ai:chat", "ai:chat-with-history"):
        return None, (CODE_METHOD_NOT_FOUND, "未知工具：{}".format(name))

    message = arguments.get("message")
    if not isinstance(message, str) or not message:
        return None, (CODE_INVALID_PARAMS, "参数 message 缺失或不是字符串")

    # 读配置
    config, error = host_call("host/getConfig", {})
    if error is not None:
        return None, (CODE_INTERNAL_ERROR, "读取配置失败：{}".format(error[1]))

    if not config:
        config = {}

    # 获取工具列表
    tools_resp, error = host_call("host/listTools", {})
    if error is not None:
        return None, (CODE_INTERNAL_ERROR, "获取工具列表失败：{}".format(error[1]))

    tools = tools_resp.get("tools", []) if tools_resp else []

    # 获取历史和系统提示词（仅对 ai:chat-with-history 有效）
    history = arguments.get("history") if name == "ai:chat-with-history" else None
    system_prompt = arguments.get("system_prompt") if name == "ai:chat-with-history" else None

    # 运行对话循环
    response, error = run_chat(message, config, tools, history, system_prompt)
    if error is not None:
        return None, (CODE_INTERNAL_ERROR, error)

    return {"response": response}, None


# ─────────────────── 请求处理 ───────────────────


def handle_request(request_id, method, params):
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
            log("忽略通知：{}".format(method))
            continue

        if handle_request(request_id, method, message.get("params") or {}):
            break

    log("ai-orchestrator 退出")


if __name__ == "__main__":
    main()
