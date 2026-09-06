"""InTools 最小示范插件（Python SDK 版）。

使用 Python SDK 后，只需关注工具逻辑，协议握手、消息编解码全由 SDK 接管。
"""

import os
import sys
import importlib.util

# 加载 Python SDK（目录名含连字符，无法直接 import）
_sdk_path = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "python-sdk", "intools.py")
_spec = importlib.util.spec_from_file_location("intools", _sdk_path)
_sdk = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_sdk)

Plugin = _sdk.Plugin
PluginError = _sdk.PluginError
ErrorCode = _sdk.ErrorCode

plugin = Plugin()


@plugin.tool(
    "hello:echo",
    "原样返回传入的文本，用于验证宿主与插件之间的调用链路",
    {
        "type": "object",
        "required": ["text"],
        "properties": {"text": {"type": "string", "description": "要回显的文本"}},
    },
)
def echo(args, ctx):
    text = args.get("text")
    if not isinstance(text, str):
        raise PluginError(ErrorCode.INVALID_PARAMS, "参数 text 缺失或不是字符串")
    return {"text": text}


@plugin.tool(
    "hello:crash",
    "立即以非零码退出，用于验证宿主的崩溃处理与待处理请求失败逻辑",
    {"type": "object", "properties": {}},
)
def crash(args, ctx):
    # os._exit 绕过 atexit 与缓冲区刷写，模拟插件真正的意外死亡。
    # 用 sys.exit 会走正常清理流程，测不出「进程猝死时待处理请求如何收场」。
    os._exit(3)


plugin.start()
