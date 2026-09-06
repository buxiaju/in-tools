"""插件生成器。

用户在 AI 对话中描述需求，AI 调用本插件自动生成完整插件项目。
支持 Python / Node.js / Go 三种语言模板。

工作流程：
1. 用户说"帮我做一个剪贴板管理插件"
2. AI 编排器理解意图，调用 plugin:generate 工具
3. 本插件生成 manifest.toml + 入口文件 + README.md
4. 用户在插件页刷新即可看到新插件
"""

import json
import os
import re
import sys

PROTOCOL_VERSION = "1.0"

TOOLS = [
    {
        "name": "plugin:generate",
        "description": "根据描述生成完整插件项目（manifest.toml + 入口文件 + README.md）",
        "input_schema": {
            "type": "object",
            "required": ["name", "description", "tools"],
            "properties": {
                "name": {
                    "type": "string",
                    "description": "插件名称（英文小写连字符，如 weather-query）",
                },
                "description": {
                    "type": "string",
                    "description": "插件的中文描述",
                },
                "language": {
                    "type": "string",
                    "enum": ["python", "node", "go"],
                    "description": "编程语言，默认 python",
                },
                "tools": {
                    "type": "array",
                    "description": "工具列表",
                    "items": {
                        "type": "object",
                        "required": ["name", "description"],
                        "properties": {
                            "name": {"type": "string", "description": "工具名，如 weather:query"},
                            "description": {"type": "string", "description": "工具描述"},
                            "params": {
                                "type": "object",
                                "description": "参数 schema，如 {required: ['city'], properties: {city: {type: 'string'}}}",
                            },
                        },
                    },
                },
                "permissions": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "所需权限列表，如 ['network:http']",
                },
                "lifecycle": {
                    "type": "string",
                    "enum": ["on-demand", "background", "startup"],
                    "description": "生命周期模式，默认 on-demand",
                },
            },
        },
    },
    {
        "name": "plugin:list-templates",
        "description": "列出可用的插件语言模板及其特点",
        "input_schema": {"type": "object", "properties": {}},
    },
]

CODE_METHOD_NOT_FOUND = -32601
CODE_INVALID_PARAMS = -32602
CODE_INTERNAL_ERROR = -32603

_next_id = 7000


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
    rid = next_id()
    write_message({"jsonrpc": "2.0", "id": rid, "method": method, "params": params})
    while True:
        line = sys.stdin.readline()
        if not line:
            return None, (-32603, "连接断开")
        try:
            message = json.loads(line.strip())
        except ValueError:
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


# ─────────────────── 名称处理 ───────────────────


def sanitize_name(name):
    """将用户输入转为合法插件目录名。"""
    name = name.lower().strip()
    name = re.sub(r"[^a-z0-9-]", "-", name)
    name = re.sub(r"-+", "-", name).strip("-")
    if not name:
        name = "my-plugin"
    return name


def name_to_id(name):
    """插件名转反向域名 ID。"""
    return "com.example." + name.replace("-", "")


def name_to_prefix(name):
    """插件名转工具前缀。取第一段：weather-query → weather。"""
    return name.split("-")[0]


# ─────────────────── 文件生成 ───────────────────


def generate_plugin(params):
    """生成完整插件项目。"""
    name = sanitize_name(params["name"])
    desc = params["description"]
    lang = params.get("language", "python")
    tools = params.get("tools", [])
    permissions = params.get("permissions", [])
    lifecycle = params.get("lifecycle", "on-demand")

    plugin_id = name_to_id(name)
    prefix = name_to_prefix(name)

    # 校验工具名
    for t in tools:
        tname = t.get("name", "")
        if not tname:
            return None, (CODE_INVALID_PARAMS, "工具名不能为空")

    # 确保工具名有前缀
    normalized_tools = []
    for t in tools:
        tname = t["name"]
        if ":" not in tname:
            tname = f"{prefix}:{tname}"
        normalized_tools.append({
            "name": tname,
            "description": t["description"],
            "params": t.get("params", {}),
        })

    # 计算插件目录
    plugins_dir = _find_plugins_dir()
    plugin_dir = os.path.join(plugins_dir, name)

    if os.path.exists(plugin_dir):
        return None, (CODE_INVALID_PARAMS, f"插件目录已存在: {plugin_dir}")

    os.makedirs(plugin_dir, exist_ok=True)

    # 生成文件
    files = []

    # manifest.toml
    manifest = _gen_manifest(plugin_id, name, desc, lang, normalized_tools, permissions, lifecycle)
    manifest_path = os.path.join(plugin_dir, "manifest.toml")
    _write_file(manifest_path, manifest)
    files.append("manifest.toml")

    # 入口文件
    if lang == "python":
        code = _gen_python(name, prefix, normalized_tools)
        _write_file(os.path.join(plugin_dir, "main.py"), code)
        files.append("main.py")
    elif lang == "node":
        code = _gen_node(name, prefix, normalized_tools)
        _write_file(os.path.join(plugin_dir, "main.js"), code)
        files.append("main.js")
    elif lang == "go":
        code = _gen_go(name, prefix, normalized_tools)
        _write_file(os.path.join(plugin_dir, "main.go"), code)
        files.append("main.go")

    # README.md
    readme = _gen_readme(name, desc, normalized_tools, lang)
    _write_file(os.path.join(plugin_dir, "README.md"), readme)
    files.append("README.md")

    return {
        "success": True,
        "plugin_name": name,
        "plugin_id": plugin_id,
        "directory": plugin_dir,
        "language": lang,
        "tools_count": len(normalized_tools),
        "files": files,
        "next_steps": [
            f"插件已生成到 {plugin_dir}",
            "在插件页点击「刷新」即可看到新插件",
            f"如需自定义逻辑，编辑 {files[1]}",
        ],
    }, None


def _find_plugins_dir():
    """找到用户插件目录（user/ 子目录）。"""
    # 优先用环境变量
    env_dir = os.environ.get("INTOOLS_PLUGINS_DIR")
    if env_dir and os.path.isdir(env_dir):
        return os.path.join(env_dir, "user")
    # 默认 ~/.intools/plugins/user
    home = os.path.expanduser("~")
    return os.path.join(home, ".intools", "plugins", "user")


def _write_file(path, content):
    with open(path, "w", encoding="utf-8") as f:
        f.write(content)


# ─────────────────── manifest 模板 ───────────────────


def _gen_manifest(plugin_id, name, desc, lang, tools, permissions, lifecycle):
    cmd_map = {"python": ("python", '["-u", "main.py"]'), "node": ("node", '["main.js"]'), "go": (f"{name}.exe", "[]")}
    cmd, args = cmd_map.get(lang, cmd_map["python"])

    tools_toml = ""
    for t in tools:
        params = t.get("params", {})
        required = params.get("required", [])
        props = params.get("properties", {})

        tools_toml += f'\n[[tools]]\nname = "{t["name"]}"\ndescription = "{t["description"]}"\n'
        tools_toml += '[tools.input_schema]\ntype = "object"\n'
        if required:
            req_str = ", ".join(f'"{r}"' for r in required)
            tools_toml += f"required = [{req_str}]\n"
        for pname, pschema in props.items():
            ptype = pschema.get("type", "string")
            pdesc = pschema.get("description", "")
            tools_toml += f'[tools.input_schema.properties.{pname}]\ntype = "{ptype}"\n'
            if pdesc:
                tools_toml += f'description = "{pdesc}"\n'

    perms_str = ", ".join(f'"{p}"' for p in permissions) if permissions else ""

    return f'''[plugin]
id = "{plugin_id}"
name = "{desc}"
version = "1.0.0"
author = ""
description = "{desc}"

[exec]
command = "{cmd}"
args = {args}
{tools_toml}
[capabilities]
permissions = [{perms_str}]

[docs]
usage_file = "README.md"

[lifecycle]
mode = "{lifecycle}"
idle_timeout_sec = 300
restart_policy = "on-failure"
'''


# ─────────────────── Python 模板 ───────────────────


def _gen_python(name, prefix, tools):
    tool_defs = ""
    for t in tools:
        params = t.get("params", {})
        required = params.get("required", [])
        props = params.get("properties", {})

        schema_props = {}
        for pname, pschema in props.items():
            schema_props[pname] = {"type": pschema.get("type", "string"), "description": pschema.get("description", "")}

        tool_defs += f'''
    {{
        "name": "{t['name']}",
        "description": "{t['description']}",
        "input_schema": {{
            "type": "object",
            "required": {json.dumps(required)},
            "properties": {json.dumps(schema_props, ensure_ascii=False)},
        }},
    }},'''

    handlers = ""
    for t in tools:
        tname = t["name"]
        handler_name = tname.replace(":", "_").replace("-", "_")
        params = t.get("params", {})
        props = params.get("properties", {})
        required = params.get("required", [])

        # 生成参数提取代码
        arg_lines = ""
        for pname, pschema in props.items():
            ptype = pschema.get("type", "string")
            if ptype == "string":
                arg_lines += f'    {pname} = arguments.get("{pname}", "")\n'
            elif ptype == "integer" or ptype == "number":
                arg_lines += f'    {pname} = arguments.get("{pname}", 0)\n'
            elif ptype == "boolean":
                arg_lines += f'    {pname} = arguments.get("{pname}", False)\n'
            else:
                arg_lines += f'    {pname} = arguments.get("{pname}")\n'

        # 必填参数检查
        check_lines = ""
        for r in required:
            check_lines += f'    if not {r}:\n        return None, (CODE_INVALID_PARAMS, "参数 {r} 缺失")\n'

        handlers += f'''

def handle_{handler_name}(arguments):
{arg_lines}{check_lines}    # TODO: 实现 {t['description']}
    return {{"message": "待实现"}}, None
'''

    call_cases = ""
    for t in tools:
        tname = t["name"]
        handler_name = tname.replace(":", "_").replace("-", "_")
        call_cases += f'''
    if name == "{tname}":
        return handle_{handler_name}(arguments)
'''

    return f'''"""{name} 插件。"""

import json
import sys

PROTOCOL_VERSION = "1.0"

TOOLS = [{tool_defs}
]

CODE_METHOD_NOT_FOUND = -32601
CODE_INVALID_PARAMS = -32602
CODE_INTERNAL_ERROR = -32603


def log(message):
    print(message, file=sys.stderr, flush=True)


def write_message(payload):
    sys.stdout.write(json.dumps(payload, ensure_ascii=False) + "\\n")
    sys.stdout.flush()


def notify(method, params):
    write_message({{"jsonrpc": "2.0", "method": method, "params": params}})


def reply_result(request_id, result):
    write_message({{"jsonrpc": "2.0", "id": request_id, "result": result}})


def reply_error(request_id, code, message):
    write_message({{"jsonrpc": "2.0", "id": request_id, "error": {{"code": code, "message": message}}}})

{handlers}
def call_tool(params):
    name = params.get("name")
    arguments = params.get("arguments") or {{}}
{call_cases}
    return None, (CODE_METHOD_NOT_FOUND, f"未知工具: {{name}}")


def handle_request(request_id, method, params):
    if method == "plugin/ready":
        log(f"握手完成，插件目录：{{params.get('plugin_dir')}}")
        reply_result(request_id, {{"ok": True}})
        return False
    if method == "tools/list":
        reply_result(request_id, {{"tools": TOOLS}})
        return False
    if method == "tools/call":
        result, error = call_tool(params)
        if error is None:
            reply_result(request_id, result)
        else:
            reply_error(request_id, error[0], error[1])
        return False
    if method == "plugin/shutdown":
        reply_result(request_id, {{"ok": True}})
        return True
    reply_error(request_id, CODE_METHOD_NOT_FOUND, f"未知方法: {{method}}")
    return False


def main():
    sys.stdin.reconfigure(encoding="utf-8")
    sys.stdout.reconfigure(encoding="utf-8")
    sys.stderr.reconfigure(encoding="utf-8")
    notify("plugin/hello", {{"protocol_version": PROTOCOL_VERSION, "tools": TOOLS}})
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            message = json.loads(line)
        except ValueError:
            log(f"丢弃无法解析的输入行: {{line[:200]}}")
            continue
        request_id = message.get("id")
        method = message.get("method")
        if method is None:
            continue
        if request_id is None:
            continue
        if handle_request(request_id, method, message.get("params") or {{}}):
            break
    log("插件退出")


if __name__ == "__main__":
    main()
'''


# ─────────────────── Node.js 模板 ───────────────────


def _gen_node(name, prefix, tools):
    tool_defs = ""
    for t in tools:
        tname = t["name"]
        tdesc = t["description"]
        params = t.get("params", {})
        required = params.get("required", [])
        props = params.get("properties", {})

        # 构建 JSON Schema
        schema = {"type": "object", "properties": {}}
        if required:
            schema["required"] = required
        for pname, pschema in props.items():
            schema["properties"][pname] = {"type": pschema.get("type", "string"), "description": pschema.get("description", "")}

        schema_json = json.dumps(schema, ensure_ascii=False, indent=4)

        # 参数提取
        arg_lines = ""
        for pname in props:
            arg_lines += f'    const {pname} = args.{pname} || "";\n'

        # 必填检查
        check_lines = ""
        for r in required:
            check_lines += f'    if (!{r}) throw new PluginError(ErrorCode.INVALID_PARAMS, "参数 {r} 缺失");\n'

        tool_defs += f'''
plugin.tool(
  "{tname}",
  "{tdesc}",
  {schema_json},
  (args, ctx) => {{
{arg_lines}{check_lines}    // TODO: 实现 {tdesc}
    return {{ message: "待实现" }};
  }}
);
'''

    return f'''/**
 * {name} 插件。
 *
 * 使用 InTools Node.js SDK。
 */

"use strict";

const {{ Plugin, PluginError, ErrorCode }} = require("../node-sdk/intools");

const plugin = new Plugin("1.0");
{tool_defs}
plugin.start();
'''


# ─────────────────── Go 模板 ───────────────────


def _gen_go(name, prefix, tools):
    tools_json = json.dumps([
        {
            "name": t["name"],
            "description": t["description"],
            "input_schema": {
                "type": "object",
                "required": t.get("params", {}).get("required", []),
                "properties": {
                    pname: {"type": pschema.get("type", "string"), "description": pschema.get("description", "")}
                    for pname, pschema in t.get("params", {}).get("properties", {}).items()
                },
            },
        }
        for t in tools
    ], ensure_ascii=False, indent="\t")

    # 生成 case 分支
    cases = ""
    for t in tools:
        tname = t["name"]
        tdesc = t["description"]
        params = t.get("params", {})
        required = params.get("required", [])
        props = params.get("properties", {})

        arg_lines = ""
        for pname in props:
            arg_lines += f'\t\t\t{pname}, _ := args["{pname}"].(string)\n'

        check_lines = ""
        for r in required:
            check_lines += f'\t\t\tif {r} == "" {{\n\t\t\t\treplyError(id, -32602, "参数 {r} 缺失")\n\t\t\t\treturn\n\t\t\t}}\n'

        cases += f'''\t\tcase "{tname}":
{arg_lines}{check_lines}\t\t\t// TODO: 实现 {tdesc}
\t\t\treplyResult(id, map[string]interface{{}}{{"message": "待实现"}})
'''

    return f'''// {name} 插件
//
// 编译：go build -o {name}.exe main.go
package main

import (
\t"bufio"
\t"encoding/json"
\t"fmt"
\t"os"
\t"runtime"
\t"strings"
)

const protocolVersion = "1.0"

var tools = {tools_json}

type jsonrpcMsg struct {{
\tJSONRPC string      `json:"jsonrpc"`
\tID      interface{{}} `json:"id,omitempty"`
\tMethod  string      `json:"method,omitempty"`
\tParams  interface{{}} `json:"params,omitempty"`
\tResult  interface{{}} `json:"result,omitempty"`
\tError   interface{{}} `json:"error,omitempty"`
}}

func writeMsg(msg jsonrpcMsg) {{
\tb, _ := json.Marshal(msg)
\tfmt.Println(string(b))
}}

func log(msg string) {{
\tfmt.Fprintln(os.Stderr, "[InTools] "+msg)
}}

func notify(method string, params interface{{}}) {{
\twriteMsg(jsonrpcMsg{{JSONRPC: "2.0", Method: method, Params: params}})
}}

func replyResult(id interface{{}}, result interface{{}}) {{
\twriteMsg(jsonrpcMsg{{JSONRPC: "2.0", ID: id, Result: result}})
}}

func replyError(id interface{{}}, code int, message string) {{
\twriteMsg(jsonrpcMsg{{
\t\tJSONRPC: "2.0",
\t\tID:      id,
\t\tError:   map[string]interface{{}}{{"code": code, "message": message}},
\t}})
}}

func handleRequest(id interface{{}}, method string, params map[string]interface{{}}) {{
\tswitch method {{
\tcase "plugin/ready":
\t\tlog(fmt.Sprintf("握手完成，插件目录：%v", params["plugin_dir"]))
\t\treplyResult(id, map[string]interface{{}}{{"ok": true}})
\tcase "tools/list":
\t\treplyResult(id, map[string]interface{{}}{{"tools": tools}})
\tcase "tools/call":
\t\tname, _ := params["name"].(string)
\t\targs, _ := params["arguments"].(map[string]interface{{}})
\t\tif args == nil {{
\t\t\targs = map[string]interface{{}}{{}}
\t\t}}
\t\tswitch name {{
{cases}\t\tdefault:
\t\t\treplyError(id, -32601, "未知工具: "+name)
\t\t}}
\tcase "plugin/shutdown":
\t\treplyResult(id, map[string]interface{{}}{{"ok": true}})
\t\tos.Exit(0)
\tdefault:
\t\treplyError(id, -32601, "未知方法: "+method)
\t}}
}}

func main() {{
\tnotify("plugin/hello", map[string]interface{{}}{{
\t\t"protocol_version": protocolVersion,
\t\t"tools":            tools,
\t}})
\tscanner := bufio.NewScanner(os.Stdin)
\tscanner.Buffer(make([]byte, 0, 64*1024), 256*1024)
\tfor scanner.Scan() {{
\t\tline := strings.TrimSpace(scanner.Text())
\t\tif line == "" {{
\t\t\tcontinue
\t\t}}
\t\tvar msg jsonrpcMsg
\t\tif err := json.Unmarshal([]byte(line), &msg); err != nil {{
\t\t\tlog(fmt.Sprintf("丢弃无法解析的输入行: %s", line[:min(len(line), 200)]))
\t\t\tcontinue
\t\t}}
\t\tif msg.ID == nil || msg.Method == "" {{
\t\t\tcontinue
\t\t}}
\t\tparams, _ := msg.Params.(map[string]interface{{}})
\t\tif params == nil {{
\t\t\tparams = map[string]interface{{}}{{}}
\t\t}}
\t\thandleRequest(msg.ID, msg.Method, params)
\t}}
\tlog("插件退出")
}}

// 保留 runtime 引用（模板需要）
var _ = runtime.Version()
'''


# ─────────────────── README 模板 ───────────────────


def _gen_readme(name, desc, tools, lang):
    tool_table = ""
    for t in tools:
        tool_table += f'| `{t["name"]}` | {t["description"]} |\n'

    lang_note = {"python": "Python", "node": "Node.js", "go": "Go（需编译）"}.get(lang, lang)

    return f'''# {desc}

## 语言

{lang_note}

## 工具

| 工具 | 说明 |
|------|------|
{tool_table}
## 开发

编辑入口文件后重启 InTools 即可生效。
'''


# ─────────────────── 请求路由 ───────────────────


def call_tool(params):
    name = params.get("name")
    arguments = params.get("arguments") or {}

    if name == "plugin:generate":
        return generate_plugin(arguments)

    if name == "plugin:list-templates":
        return {
            "templates": [
                {"language": "python", "command": "python", "description": "最广泛，无需编译，适合快速原型"},
                {"language": "node", "command": "node", "description": "JavaScript/TypeScript，有 SDK 支持"},
                {"language": "go", "command": "编译后 .exe", "description": "高性能二进制，适合系统工具"},
            ]
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
            continue
        if handle_request(request_id, method, message.get("params") or {}):
            break
    log("插件退出")


if __name__ == "__main__":
    main()
