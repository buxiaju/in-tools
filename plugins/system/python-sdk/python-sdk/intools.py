"""InTools Python 插件 SDK。

用法：
    from intools import Plugin, PluginError, ErrorCode

    plugin = Plugin()

    @plugin.tool("my:echo", "原样返回文本", {
        "type": "object",
        "required": ["text"],
        "properties": {"text": {"type": "string"}},
    })
    def echo(args, ctx):
        return {"text": args["text"]}

    plugin.start()

插件只需关注工具逻辑，协议握手、消息编解码、错误处理全由 SDK 接管。
stdout 只输出协议帧，所有调试日志走 stderr（由宿主转存到日志文件）。

反向 RPC（跨插件调用）：
    @plugin.tool("my:task", "调用其他插件", {})
    def task(args, ctx):
        tools = ctx.list_tools()              # 列出所有可用工具
        result = ctx.call_tool("ocr:recognize", {"path": "a.png"})  # 跨插件调用
        cfg = ctx.get_config()                # 读取本插件配置
        ctx.set_config({"last": 123})         # 写入本插件配置
        ctx.progress({"percent": 50})         # 进度通知
        ctx.stream("文本片段")                 # 流式输出
        return {"done": True}
"""

import json
import sys

PROTOCOL_VERSION = "1.0"

# ── JSON-RPC 错误码 ─────────────────────────────────────────────────

class ErrorCode:
    PARSE = -32700
    INVALID_REQUEST = -32600
    METHOD_NOT_FOUND = -32601
    INVALID_PARAMS = -32602
    INTERNAL = -32603
    PLUGIN = -32000
    CALL_DEPTH = -32001
    PERMISSION_DENIED = -32002
    TOOL_NOT_FOUND = -32003
    TIMEOUT = -32004


class PluginError(Exception):
    """插件业务错误，抛出后转成 JSON-RPC error 响应。"""

    def __init__(self, code, message):
        super().__init__(message)
        self.code = code


# ── 工具调用上下文 ───────────────────────────────────────────────────

class ToolContext:
    """工具 handler 的第二个参数，提供反向 RPC 和进度通知。"""

    def __init__(self, plugin):
        self._plugin = plugin

    def list_tools(self):
        """列出所有可用工具（跨插件）。"""
        return self._plugin._host_call("host/listTools", {})

    def call_tool(self, tool_name, args=None):
        """调用其他插件的工具。"""
        return self._plugin._host_call("host/callTool", {
            "tool": tool_name,
            "arguments": args or {},
        })

    def get_config(self):
        """读取本插件配置。"""
        return self._plugin._host_call("host/getConfig", {})

    def set_config(self, value):
        """写入本插件配置。"""
        return self._plugin._host_call("host/setConfig", {"value": value})

    def notify(self, method, params=None):
        """向宿主推送通知。"""
        self._plugin._notify(method, params or {})

    def progress(self, info):
        """推送进度通知（notify/progress 快捷方式）。"""
        self.notify("notify/progress", info)

    def stream(self, text, task_id=None):
        """推送流式文本片段。"""
        params = {"text": text}
        if task_id:
            params["task_id"] = task_id
        self.notify("notify/stream", params)

    def tool_call_start(self, tool, args=None):
        """推送工具调用开始通知。"""
        self.notify("notify/toolCall", {"tool": tool, "arguments": args or {}, "phase": "start"})

    def tool_call_end(self, tool, result=None):
        """推送工具调用完成通知。"""
        self.notify("notify/toolCall", {"tool": tool, "result": result, "phase": "end"})


# ── 插件主体 ─────────────────────────────────────────────────────────

class Plugin:
    """InTools 插件。"""

    def __init__(self, protocol_version=PROTOCOL_VERSION):
        self._protocol_version = protocol_version
        self._tools = {}       # name -> (schema, handler)
        self._plugin_dir = None
        self._config = None
        self._closed = False
        self._next_rpc_id = 1000

        # stdin 编码钉死 UTF-8
        if hasattr(sys.stdin, "reconfigure"):
            sys.stdin.reconfigure(encoding="utf-8")
        if hasattr(sys.stdout, "reconfigure"):
            sys.stdout.reconfigure(encoding="utf-8")
        if hasattr(sys.stderr, "reconfigure"):
            sys.stderr.reconfigure(encoding="utf-8")

    def tool(self, name, description, input_schema=None):
        """装饰器：注册一个工具。

        handler 签名：(args: dict, ctx: ToolContext) -> dict
        """
        def decorator(fn):
            self._tools[name] = {
                "description": description,
                "input_schema": input_schema or {"type": "object", "properties": {}},
                "handler": fn,
            }
            return fn
        return decorator

    def register(self, name, description, input_schema, handler):
        """非装饰器方式注册工具。"""
        self._tools[name] = {
            "description": description,
            "input_schema": input_schema or {"type": "object", "properties": {}},
            "handler": handler,
        }

    @property
    def plugin_dir(self):
        return self._plugin_dir

    @property
    def config(self):
        return self._config

    def start(self):
        """启动插件主循环。阻塞直到收到 shutdown 或 stdin 关闭。"""
        self._send({
            "jsonrpc": "2.0",
            "method": "plugin/hello",
            "params": {
                "protocol_version": self._protocol_version,
                "tools": self._tool_descriptors(),
            },
        })

        for line in sys.stdin:
            if self._closed:
                break
            line = line.strip()
            if not line:
                continue
            self._handle_line(line)

        self._log("插件退出")

    # ── 反向 RPC ─────────────────────────────────────────────────────

    def _host_call(self, method, params):
        """发送 host/* 请求并等待响应。等响应期间就地处理宿主发来的请求。"""
        rid = self._next_rpc_id
        self._next_rpc_id += 1
        self._send({"jsonrpc": "2.0", "id": rid, "method": method, "params": params})

        while True:
            line = sys.stdin.readline()
            if not line:
                raise PluginError(ErrorCode.INTERNAL, "连接断开")

            try:
                message = json.loads(line.strip())
            except ValueError:
                self._log(f"丢弃无法解析的行: {line[:200]}")
                continue

            msg_id = message.get("id")
            msg_method = message.get("method")

            # 是请求？就地处理
            if msg_method is not None and msg_id is not None:
                self._handle_request(msg_id, msg_method, message.get("params") or {})
                continue

            # 是目标响应？
            if msg_id == rid:
                if "error" in message:
                    err = message["error"]
                    raise PluginError(err["code"], err["message"])
                return message.get("result")

            # 是通知？
            if msg_method:
                continue

            self._log(f"忽略无关响应: id={msg_id}")

    # ── 消息处理 ─────────────────────────────────────────────────────

    def _handle_line(self, line):
        try:
            msg = json.loads(line)
        except ValueError:
            self._log(f"丢弃无法解析的输入行: {line[:200]}")
            return

        # 通知（无 id）
        if msg.get("method") and msg.get("id") is None:
            return

        # 响应（无 method）
        if not msg.get("method"):
            return

        # 请求
        self._handle_request(msg.get("id"), msg["method"], msg.get("params") or {})

    def _handle_request(self, req_id, method, params):
        try:
            result = self._dispatch(method, params)
            self._reply_result(req_id, result)
        except PluginError as e:
            self._reply_error(req_id, e.code, str(e))
        except Exception as e:
            self._log(f"未处理异常: {e}")
            self._reply_error(req_id, ErrorCode.INTERNAL, str(e))

    def _dispatch(self, method, params):
        if method == "plugin/ready":
            self._plugin_dir = params.get("plugin_dir")
            self._config = params.get("config", {})
            self._log(f"握手完成，插件目录: {self._plugin_dir}")
            return {"ok": True}

        if method == "plugin/shutdown":
            self._log("收到 shutdown 信号")
            self._closed = True
            return {"ok": True}

        if method == "tools/list":
            return {"tools": self._tool_descriptors()}

        if method == "tools/call":
            name = params.get("name")
            args = params.get("arguments", {})
            entry = self._tools.get(name)
            if not entry:
                raise PluginError(ErrorCode.TOOL_NOT_FOUND, f"未知工具: {name}")
            ctx = ToolContext(self)
            return entry["handler"](args, ctx)

        raise PluginError(ErrorCode.METHOD_NOT_FOUND, f"未知方法: {method}")

    # ── 消息发送 ─────────────────────────────────────────────────────

    def _notify(self, method, params):
        self._send({"jsonrpc": "2.0", "method": method, "params": params})

    def _reply_result(self, req_id, result):
        self._send({"jsonrpc": "2.0", "id": req_id, "result": result or {}})

    def _reply_error(self, req_id, code, message):
        self._send({"jsonrpc": "2.0", "id": req_id, "error": {"code": code, "message": message}})

    def _send(self, payload):
        if self._closed:
            return
        sys.stdout.write(json.dumps(payload, ensure_ascii=False) + "\n")
        sys.stdout.flush()

    def _tool_descriptors(self):
        return [
            {
                "name": name,
                "description": info["description"],
                "input_schema": info["input_schema"],
            }
            for name, info in self._tools.items()
        ]

    def _log(self, message):
        print(f"[InTools] {message}", file=sys.stderr, flush=True)
