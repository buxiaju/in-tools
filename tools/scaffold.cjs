#!/usr/bin/env node
/**
 * InTools 插件脚手架。
 *
 * 用法：
 *   node tools/scaffold.cjs <plugin-name> [语言] [描述]
 *
 * 示例：
 *   node tools/scaffold.cjs my-tool python "我的工具"
 *   node tools/scaffold.cjs fast-calc go "高性能计算工具"
 *   node tools/scaffold.cjs web-scraper node "网页抓取工具"
 *
 * 会在 plugins/<plugin-name>/ 下生成完整可运行的插件项目。
 */

"use strict";

const fs = require("fs");
const path = require("path");

const LANGS = ["python", "node", "go"];
const TEMPLATES = {
  python: { ext: "py", cmd: "python", args: '["-u", "main.py"]' },
  node:   { ext: "js", cmd: "node",   args: '["main.js"]' },
  go:     { ext: "go", cmd: "my-tool.exe", args: "[]" },
};

// ── 参数解析 ────────────────────────────────────────────────────────

const args = process.argv.slice(2);

if (args.length === 0 || args[0] === "--help" || args[0] === "-h") {
  console.log(`
InTools 插件脚手架

用法: node tools/scaffold.cjs <plugin-name> [语言] [描述]

语言: python (默认), node, go

示例:
  node tools/scaffold.cjs my-tool python "我的工具"
  node tools/scaffold.cjs fast-calc go "高性能计算工具"
  node tools/scaffold.cjs web-scraper node "网页抓取工具"
`);
  process.exit(0);
}

const pluginName = args[0];
const lang = (args[1] || "python").toLowerCase();
const description = args[2] || `${pluginName} 插件`;

if (!LANGS.includes(lang)) {
  console.error(`不支持的语言: ${lang}，可选: ${LANGS.join(", ")}`);
  process.exit(1);
}

// 验证名称：只允许小写字母、数字、连字符
if (!/^[a-z][a-z0-9-]*$/.test(pluginName)) {
  console.error("插件名称只能包含小写字母、数字和连字符，且必须以字母开头");
  process.exit(1);
}

const pluginId = `com.example.${pluginName.replace(/-/g, "")}`;
// 工具名前缀取第一段：demo-hello → demo，file-hash → hash
const toolPrefix = pluginName.split("-")[0];
const pluginDir = path.join(__dirname, "..", "plugins", pluginName);

if (fs.existsSync(pluginDir)) {
  console.error(`目录已存在: ${pluginDir}`);
  process.exit(1);
}

fs.mkdirSync(pluginDir, { recursive: true });
console.log(`创建插件目录: ${pluginDir}`);

// ── manifest.toml ──────────────────────────────────────────────────

const tmpl = TEMPLATES[lang];

const manifest = `[plugin]
id = "${pluginId}"
name = "${description}"
version = "1.0.0"
author = ""
description = "${description}"

[exec]
command = "${tmpl.cmd}"
args = ${tmpl.args}

[[tools]]
name = "${toolPrefix}:echo"
description = "原样返回传入的文本"
[tools.input_schema]
type = "object"
required = ["text"]
[tools.input_schema.properties.text]
type = "string"
description = "要回显的文本"

[[tools]]
name = "${toolPrefix}:info"
description = "返回插件版本与运行环境信息"
[tools.input_schema]
type = "object"

[capabilities]
permissions = []

[[result_display]]
tool = "${toolPrefix}:info"
type = "kv"
fields = [
  { key = "plugin", label = "插件" },
  { key = "version", label = "版本" },
  { key = "runtime", label = "运行时" },
]

[docs]
usage_file = "README.md"

[lifecycle]
mode = "on-demand"
idle_timeout_sec = 300
restart_policy = "on-failure"
`;

writeFile("manifest.toml", manifest);

// ── 入口文件 ────────────────────────────────────────────────────────

if (lang === "python") {
  writeFile("main.py", pythonTemplate());
} else if (lang === "node") {
  writeFile("main.js", nodeTemplate());
} else if (lang === "go") {
  writeFile("main.go", goTemplate());
}

// ── README ──────────────────────────────────────────────────────────

writeFile("README.md", `# ${description}

## 工具

| 工具 | 说明 |
|------|------|
| \`${toolPrefix}:echo\` | 原样返回传入的文本 |
| \`${toolPrefix}:info\` | 返回插件版本与运行环境信息 |

## 开发

${lang === "go" ? "```bash\ngo build -o my-tool.exe main.go\n```" : "直接修改入口文件，重启后生效。"}
`);

console.log(`\n插件「${description}」已生成！`);
console.log(`\n目录: plugins/${pluginName}/`);
console.log(`语言: ${lang}`);
console.log(`\n启动 InTools 后在插件页即可看到。`);

// ── 辅助函数 ────────────────────────────────────────────────────────

function writeFile(name, content) {
  const fullPath = path.join(pluginDir, name);
  fs.writeFileSync(fullPath, content, "utf-8");
  console.log(`  ${name}`);
}

// ── 模板 ────────────────────────────────────────────────────────────

function pythonTemplate() {
  return `"""${description}。"""

import json
import sys

PROTOCOL_VERSION = "1.0"

TOOLS = [
    {
        "name": "${toolPrefix}:echo",
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
        "name": "${toolPrefix}:info",
        "description": "返回插件版本与运行环境信息",
        "input_schema": {"type": "object", "properties": {}},
    },
]

CODE_METHOD_NOT_FOUND = -32601
CODE_INVALID_PARAMS = -32602


def log(message):
    print(message, file=sys.stderr, flush=True)


def write_message(payload):
    sys.stdout.write(json.dumps(payload, ensure_ascii=False) + "\\n")
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

    if name == "${toolPrefix}:echo":
        text = arguments.get("text")
        if not isinstance(text, str):
            return None, (CODE_INVALID_PARAMS, "参数 text 缺失或不是字符串")
        return {"text": text}, None

    if name == "${toolPrefix}:info":
        import platform
        return {
            "plugin": "${pluginName}",
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
`;
}

function nodeTemplate() {
  return `/**
 * ${description}。
 *
 * 使用 InTools Node.js SDK，零协议样板代码。
 */

"use strict";

const { Plugin, PluginError, ErrorCode } = require("../node-sdk/intools");

const plugin = new Plugin("1.0");

plugin.tool(
  "${toolPrefix}:echo",
  "原样返回传入的文本",
  {
    type: "object",
    required: ["text"],
    properties: { text: { type: "string", description: "要回显的文本" } },
  },
  (args) => {
    if (typeof args.text !== "string") {
      throw new PluginError(ErrorCode.INVALID_PARAMS, "参数 text 缺失或不是字符串");
    }
    return { text: args.text };
  }
);

plugin.tool(
  "${toolPrefix}:info",
  "返回插件版本与运行环境信息",
  { type: "object", properties: {} },
  () => ({
    plugin: "${pluginName}",
    version: "1.0.0",
    runtime: \`Node.js \${process.version}\`,
  })
);

plugin.start();
`;
}

function goTemplate() {
  return `// ${description}
//
// 编译：go build -o my-tool.exe main.go
package main

import (
	"bufio"
	"encoding/json"
	"fmt"
	"os"
	"runtime"
	"strings"
)

const protocolVersion = "1.0"

var tools = []map[string]interface{}{
	{
		"name":        "${toolPrefix}:echo",
		"description": "原样返回传入的文本",
		"input_schema": map[string]interface{}{
			"type":     "object",
			"required": []string{"text"},
			"properties": map[string]interface{}{
				"text": map[string]interface{}{
					"type":        "string",
					"description": "要回显的文本",
				},
			},
		},
	},
	{
		"name":        "${toolPrefix}:info",
		"description": "返回插件版本与运行环境信息",
		"input_schema": map[string]interface{}{
			"type":       "object",
			"properties": map[string]interface{}{},
		},
	},
}

type jsonrpcMsg struct {
	JSONRPC string      \`json:"jsonrpc"\`
	ID      interface{} \`json:"id,omitempty"\`
	Method  string      \`json:"method,omitempty"\`
	Params  interface{} \`json:"params,omitempty"\`
	Result  interface{} \`json:"result,omitempty"\`
	Error   interface{} \`json:"error,omitempty"\`
}

func writeMsg(msg jsonrpcMsg) {
	b, _ := json.Marshal(msg)
	fmt.Println(string(b))
}

func log(msg string) {
	fmt.Fprintln(os.Stderr, "[InTools] "+msg)
}

func notify(method string, params interface{}) {
	writeMsg(jsonrpcMsg{JSONRPC: "2.0", Method: method, Params: params})
}

func replyResult(id interface{}, result interface{}) {
	writeMsg(jsonrpcMsg{JSONRPC: "2.0", ID: id, Result: result})
}

func replyError(id interface{}, code int, message string) {
	writeMsg(jsonrpcMsg{
		JSONRPC: "2.0",
		ID:      id,
		Error:   map[string]interface{}{"code": code, "message": message},
	})
}

func handleRequest(id interface{}, method string, params map[string]interface{}) {
	switch method {
	case "plugin/ready":
		log(fmt.Sprintf("握手完成，插件目录：%v", params["plugin_dir"]))
		replyResult(id, map[string]interface{}{"ok": true})
	case "tools/list":
		replyResult(id, map[string]interface{}{"tools": tools})
	case "tools/call":
		name, _ := params["name"].(string)
		args, _ := params["arguments"].(map[string]interface{})
		if args == nil {
			args = map[string]interface{}{}
		}
		switch name {
		case "${toolPrefix}:echo":
			text, _ := args["text"].(string)
			if text == "" {
				replyError(id, -32602, "参数 text 缺失")
				return
			}
			replyResult(id, map[string]interface{}{"text": text})
		case "${toolPrefix}:info":
			replyResult(id, map[string]interface{}{
				"plugin":  "${pluginName}",
				"version": "1.0.0",
				"runtime": fmt.Sprintf("Go %s", runtime.Version()),
			})
		default:
			replyError(id, -32601, "未知工具: "+name)
		}
	case "plugin/shutdown":
		replyResult(id, map[string]interface{}{"ok": true})
		os.Exit(0)
	default:
		replyError(id, -32601, "未知方法: "+method)
	}
}

func main() {
	notify("plugin/hello", map[string]interface{}{
		"protocol_version": protocolVersion,
		"tools":            tools,
	})

	scanner := bufio.NewScanner(os.Stdin)
	scanner.Buffer(make([]byte, 0, 64*1024), 256*1024)

	for scanner.Scan() {
		line := strings.TrimSpace(scanner.Text())
		if line == "" {
			continue
		}
		var msg jsonrpcMsg
		if err := json.Unmarshal([]byte(line), &msg); err != nil {
			log(fmt.Sprintf("丢弃无法解析的输入行: %s", line[:min(len(line), 200)]))
			continue
		}
		if msg.ID == nil || msg.Method == "" {
			continue
		}
		params, _ := msg.Params.(map[string]interface{})
		if params == nil {
			params = map[string]interface{}{}
		}
		handleRequest(msg.ID, msg.Method, params)
	}
	log("插件退出")
}
`;
}
