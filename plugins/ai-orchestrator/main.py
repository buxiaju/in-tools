"""AI 编排插件（Python SDK 版）。

对接 OpenAI 兼容 API（DeepSeek、Ollama 等），自动调用其他插件的工具完成任务。
仅使用 Python 标准库（urllib）。

工作流程：
1. 收到 ai:chat 调用 → 读配置（base_url / api_key / model）
2. 调 ctx.list_tools() 获取可用工具，转为 OpenAI function calling 格式
3. 向 LLM 发送用户消息 + 工具列表
4. 若 LLM 返回 tool_calls，逐个调 ctx.call_tool() 执行
5. 将工具结果喂回 LLM，重复直到 LLM 给出最终文本或达到 10 轮上限
6. 全程经 ctx.notify() 推送进度
"""

import json
import os
import sys
import urllib.request
import urllib.error
import importlib.util

_sdk_path = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "python-sdk", "intools.py")
_spec = importlib.util.spec_from_file_location("intools", _sdk_path)
_sdk = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_sdk)

Plugin = _sdk.Plugin
PluginError = _sdk.PluginError
ErrorCode = _sdk.ErrorCode

plugin = Plugin()

MAX_TOOL_ROUNDS = 10
REQUEST_TIMEOUT = 120


# ── HTTP ──────────────────────────────────────────────────────────

def http_post(url, api_key, body):
    """向 LLM API 发送 POST 请求，返回解析后的 JSON 响应。"""
    data = json.dumps(body, ensure_ascii=False).encode("utf-8")
    req = urllib.request.Request(
        url,
        data=data,
        headers={"Content-Type": "application/json", "Authorization": f"Bearer {api_key}"},
        method="POST",
    )
    try:
        with urllib.request.urlopen(req, timeout=REQUEST_TIMEOUT) as resp:
            return json.loads(resp.read().decode("utf-8"))
    except urllib.error.HTTPError as e:
        body_text = ""
        try:
            body_text = e.read().decode("utf-8", errors="replace")[:500]
        except Exception:
            pass
        if e.code == 401:
            raise RuntimeError("API 密钥无效（HTTP 401），请在设置页检查 AI 配置")
        raise RuntimeError(f"API 返回 HTTP {e.code}：{body_text}")
    except urllib.error.URLError as e:
        raise RuntimeError(f"无法连接 API（{e.reason}），请检查 base_url 和网络")


# ── 工具转换 ──────────────────────────────────────────────────────

def to_function_name(tool_name):
    """工具名转 OpenAI function name（冒号→下划线）。"""
    return tool_name.replace(":", "_")


def tools_to_functions(tools):
    """host/listTools 结果转 OpenAI function calling 格式，排除 ai:chat 自身。"""
    functions = []
    for tool in tools:
        name = tool.get("name", "")
        if name == "ai:chat":
            continue
        functions.append({
            "type": "function",
            "function": {
                "name": to_function_name(name),
                "description": tool.get("description", ""),
                "parameters": tool.get("input_schema", {"type": "object"}),
            },
        })
    return functions


def function_name_to_tool(func_name, tools):
    """function name 反解回原始工具名。"""
    for tool in tools:
        if to_function_name(tool.get("name", "")) == func_name:
            return tool["name"]
    return None


# ── 核心对话循环 ──────────────────────────────────────────────────

def run_chat(user_message, config, tools, ctx):
    """执行一次 AI 对话，含多轮工具调用。返回 (response_text, error)。"""
    base_url = config.get("base_url", "").rstrip("/")
    api_key = config.get("api_key", "")
    model = config.get("model", "deepseek-chat")

    if not base_url:
        return None, "未配置 base_url，请在设置页配置 AI API"
    if not api_key:
        return None, "未配置 api_key，请在设置页配置 AI API"

    functions = tools_to_functions(tools)
    chat_url = f"{base_url}/chat/completions"

    messages = [
        {
            "role": "system",
            "content": (
                "你是一个智能助手，可以通过调用工具来帮助用户完成任务。"
                "请根据用户的需求选择合适的工具。如果不需要工具，直接回答即可。"
                "工具调用结果会用中文返回，请基于结果给出自然、简洁的回答。"
            ),
        },
        {"role": "user", "content": user_message},
    ]

    for round_num in range(MAX_TOOL_ROUNDS):
        body = {"model": model, "messages": messages}
        if functions:
            body["tools"] = functions

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

        messages.append(msg)

        if not tool_calls:
            ctx.notify("notify/stream", {"delta": content})
            ctx.notify("notify/streamEnd", {})
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
                tool_result = {"error": f"未知工具：{func_name}"}
            else:
                ctx.tool_call_start(original_name, args)
                try:
                    result = ctx.call_tool(original_name, args)
                    tool_result = result
                    ctx.tool_call_end(original_name, {"ok": True})
                except PluginError as e:
                    tool_result = {"error": str(e)}
                    ctx.tool_call_end(original_name, {"error": str(e)})

            messages.append({
                "role": "tool",
                "tool_call_id": tc.get("id", ""),
                "content": json.dumps(tool_result, ensure_ascii=False),
            })

    summary = f"已达到工具调用轮数上限（{MAX_TOOL_ROUNDS}轮），以下是最新的进展。"
    ctx.notify("notify/stream", {"delta": summary})
    ctx.notify("notify/streamEnd", {})
    return summary, None


# ── 工具 ──────────────────────────────────────────────────────────

@plugin.tool(
    "ai:chat",
    "向 AI 发送消息，AI 会自动选择并调用可用工具来完成任务",
    {
        "type": "object",
        "required": ["message"],
        "properties": {"message": {"type": "string", "description": "用户的自然语言消息"}},
    },
)
def do_chat(args, ctx):
    message = args.get("message")
    if not isinstance(message, str) or not message:
        raise PluginError(ErrorCode.INVALID_PARAMS, "参数 message 缺失或不是字符串")

    # 读配置
    try:
        config = ctx.get_config() or {}
    except PluginError:
        config = {}

    # 获取工具列表
    try:
        tools_resp = ctx.list_tools()
        tools = tools_resp.get("tools", []) if isinstance(tools_resp, dict) else tools_resp or []
    except PluginError:
        tools = []

    # 运行对话循环
    response, error = run_chat(message, config, tools, ctx)
    if error is not None:
        raise PluginError(ErrorCode.INTERNAL, error)

    return {"response": response}


plugin.start()
