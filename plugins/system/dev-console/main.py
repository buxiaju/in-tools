"""开发者控制台插件。

通过反向 RPC 暴露宿主的完整工具链，让 AI 对话成为插件调试界面。
支持：列出插件/工具、调用任意工具、读写配置、校验 manifest。
"""

import json
import os
import sys

# Python 3.11+ 内置 tomllib（二进制读），旧版本回退到 toml 包
try:
    import tomllib as _toml_reader  # 3.11+
    def _load_toml(text):
        return _toml_reader.loads(text)
except ImportError:
    try:
        import toml as _toml_reader  # pip install toml
        def _load_toml(text):
            return _toml_reader.loads(text)
    except ImportError:
        _toml_reader = None
        def _load_toml(text):
            raise ImportError("需要 Python 3.11+（内置 tomllib）或 pip install toml")

PROTOCOL_VERSION = "1.0"

TOOLS = [
    {
        "name": "dev:plugins",
        "description": "列出所有已加载插件及其运行状态、工具数、权限声明",
        "input_schema": {"type": "object", "properties": {}},
    },
    {
        "name": "dev:tools",
        "description": "列出所有可用工具及其 JSON Schema（跨插件）",
        "input_schema": {"type": "object", "properties": {}},
    },
    {
        "name": "dev:call",
        "description": "直接调用任意工具，用于开发调试",
        "input_schema": {
            "type": "object",
            "required": ["tool"],
            "properties": {
                "tool": {"type": "string", "description": "工具全名，如 hello:echo"},
                "arguments": {"type": "object", "description": "工具参数"},
            },
        },
    },
    {
        "name": "dev:config",
        "description": "读取或写入指定插件的私有配置",
        "input_schema": {
            "type": "object",
            "required": ["action"],
            "properties": {
                "action": {"type": "string", "enum": ["get", "set"], "description": "get 读取或 set 写入"},
                "value": {"type": "object", "description": "set 时要写入的 JSON 值"},
            },
        },
    },
    {
        "name": "dev:validate",
        "description": "校验 manifest.toml 文件，报告格式、校验、权限问题",
        "input_schema": {
            "type": "object",
            "required": ["path"],
            "properties": {
                "path": {"type": "string", "description": "manifest.toml 的绝对路径或相对于插件目录的路径"},
            },
        },
    },
    {
        "name": "dev:search",
        "description": "搜索插件目录，按关键词匹配插件名、工具名或描述",
        "input_schema": {
            "type": "object",
            "required": ["query"],
            "properties": {
                "query": {"type": "string", "description": "搜索关键词"},
            },
        },
    },
]

CODE_METHOD_NOT_FOUND = -32601
CODE_INVALID_PARAMS = -32602
CODE_INTERNAL_ERROR = -32603

_next_id = 5000


def next_id():
    global _next_id
    _next_id += 1
    return _next_id


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
    write_message({"jsonrpc": "2.0", "id": request_id, "error": {"code": code, "message": message}})


# ─────────────────── 反向 RPC ───────────────────


def host_call(method, params):
    """发送 host/* 请求并等待响应。"""
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


# ─────────────────── 工具实现 ───────────────────


def do_plugins():
    """列出所有插件（通过反向 RPC）。"""
    tools_result, err = host_call("host/listTools", {})
    if err:
        return None, err

    # listTools 返回的是工具列表，我们需要反推出插件信息
    plugin_map = {}
    for t in (tools_result or []):
        pid = t.get("plugin_id", "unknown")
        if pid not in plugin_map:
            plugin_map[pid] = {"id": pid, "tools": []}
        plugin_map[pid]["tools"].append({
            "name": t["name"],
            "description": t.get("description", ""),
        })

    return {"count": len(plugin_map), "plugins": list(plugin_map.values())}, None


def do_tools():
    """列出所有工具（通过反向 RPC）。"""
    tools_result, err = host_call("host/listTools", {})
    if err:
        return None, err
    return {"count": len(tools_result or []), "tools": tools_result or []}, None


def do_call(tool, arguments):
    """调用指定工具（通过反向 RPC）。"""
    result, err = host_call("host/callTool", {"tool": tool, "arguments": arguments or {}})
    if err:
        return None, err
    return {"tool": tool, "result": result}, None


def do_config_get():
    """读取本插件配置。"""
    result, err = host_call("host/getConfig", {})
    if err:
        return None, err
    return {"config": result}, None


def do_config_set(value):
    """写入本插件配置。"""
    result, err = host_call("host/setConfig", {"value": value})
    if err:
        return None, err
    return {"ok": True, "config": result}, None


def do_validate(path):
    """校验 manifest.toml 文件。"""
    # 解析路径
    if not os.path.isabs(path):
        # 尝试相对插件目录
        plugin_dir = os.environ.get("PLUGIN_DIR", "")
        if plugin_dir:
            path = os.path.join(plugin_dir, path)

    if not os.path.exists(path):
        return None, (CODE_INVALID_PARAMS, f"文件不存在: {path}")

    issues = []
    warnings = []

    try:
        with open(path, "r", encoding="utf-8") as f:
            raw = f.read()
    except Exception as e:
        return None, (CODE_INTERNAL_ERROR, f"读取文件失败: {e}")

    # 1. TOML 解析
    try:
        data = _load_toml(raw)
    except ImportError as e:
        return None, (CODE_INTERNAL_ERROR, str(e))
    except Exception as e:
        issues.append(f"TOML 语法错误: {e}")
        return {"valid": False, "issues": issues, "warnings": warnings}, None

    # 2. 必需段检查
    if "plugin" not in data:
        issues.append("缺少 [plugin] 段")
    else:
        p = data["plugin"]
        for field in ["id", "name", "version"]:
            if field not in p:
                issues.append(f"[plugin] 缺少 {field}")

        pid = p.get("id", "")
        if pid and ("." not in pid or len(pid.split(".")) < 2):
            warnings.append(f"插件 ID '{pid}' 不是反向域名格式（如 com.example.tool）")

        ver = p.get("version", "")
        if ver and ver.count(".") < 2:
            warnings.append(f"版本号 '{ver}' 不是语义化三段格式（如 1.0.0）")

    if "exec" not in data:
        issues.append("缺少 [exec] 段")
    else:
        e = data["exec"]
        if not e.get("command"):
            issues.append("[exec].command 不能为空")

    # 3. 工具检查
    tools = data.get("tools", [])
    if not tools:
        warnings.append("未声明任何工具，插件将不提供任何功能")

    tool_names = set()
    for i, t in enumerate(tools):
        tname = t.get("name", "")
        if not tname:
            issues.append(f"tools[{i}].name 不能为空")
        elif tname in tool_names:
            issues.append(f"工具名 '{tname}' 重复")
        else:
            tool_names.add(tname)

    # 4. 权限检查
    caps = data.get("capabilities", {})
    perms = caps.get("permissions", [])
    high_risk = {"input:control", "process:spawn", "shell:exec", "system:manage", "persistence:install"}
    for p in perms:
        if p in high_risk:
            warnings.append(f"声明了高危权限 '{p}'，该插件将不对 MCP 暴露")

    # 5. 快捷键检查
    sc = data.get("shortcut")
    if sc:
        if sc.get("tool") and sc["tool"] not in tool_names:
            issues.append(f"shortcut.tool '{sc['tool']}' 未在 [[tools]] 中声明")

    # 6. 设置字段检查
    settings = data.get("settings", [])
    keys = set()
    for i, sf in enumerate(settings):
        skey = sf.get("key", "")
        if not skey:
            issues.append(f"settings[{i}].key 不能为空")
        elif skey in keys:
            issues.append(f"settings[{i}].key '{skey}' 重复")
        else:
            keys.add(skey)

    valid = len(issues) == 0
    return {
        "valid": valid,
        "issues": issues,
        "warnings": warnings,
        "summary": {
            "plugin_id": data.get("plugin", {}).get("id", "N/A"),
            "tools_count": len(tools),
            "permissions": perms,
            "has_shortcut": sc is not None,
            "has_docs": "docs" in data,
            "has_settings": len(settings) > 0,
        },
    }, None


def do_search(query):
    """搜索工具。"""
    tools_result, err = host_call("host/listTools", {})
    if err:
        return None, err

    q = query.lower()
    matched = []
    for t in (tools_result or []):
        name = t.get("name", "")
        desc = t.get("description", "")
        pid = t.get("plugin_id", "")
        if q in name.lower() or q in desc.lower() or q in pid.lower():
            matched.append(t)

    return {"query": query, "count": len(matched), "matches": matched}, None


# ─────────────────── 请求路由 ───────────────────


def call_tool(params):
    name = params.get("name")
    arguments = params.get("arguments") or {}

    if name == "dev:plugins":
        return do_plugins()

    if name == "dev:tools":
        return do_tools()

    if name == "dev:call":
        tool = arguments.get("tool")
        if not tool:
            return None, (CODE_INVALID_PARAMS, "参数 tool 缺失")
        return do_call(tool, arguments.get("arguments"))

    if name == "dev:config":
        action = arguments.get("action")
        if action == "get":
            return do_config_get()
        elif action == "set":
            value = arguments.get("value")
            if not isinstance(value, dict):
                return None, (CODE_INVALID_PARAMS, "set 时 value 必须是对象")
            return do_config_set(value)
        else:
            return None, (CODE_INVALID_PARAMS, f"未知操作: {action}，支持 get/set")

    if name == "dev:validate":
        path = arguments.get("path")
        if not path:
            return None, (CODE_INVALID_PARAMS, "参数 path 缺失")
        return do_validate(path)

    if name == "dev:search":
        query = arguments.get("query")
        if not query:
            return None, (CODE_INVALID_PARAMS, "参数 query 缺失")
        return do_search(query)

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
