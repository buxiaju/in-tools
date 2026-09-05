"""文件搜索插件。

按文件名关键词搜索指定目录，仅使用 Python 标准库。
协议与 hello-plugin 一致：逐行 JSON-RPC 2.0，stdin 收、stdout 发。
"""

import json
import os
import sys

PROTOCOL_VERSION = "1.0"

TOOLS = [
    {
        "name": "search:files",
        "description": "按文件名关键词搜索指定目录下的文件，返回匹配的文件路径列表",
        "input_schema": {
            "type": "object",
            "required": ["keyword"],
            "properties": {
                "keyword": {
                    "type": "string",
                    "description": "文件名中需要包含的关键词（不区分大小写）",
                },
                "directory": {
                    "type": "string",
                    "description": "搜索的根目录，默认为用户主目录",
                },
                "max_results": {
                    "type": "integer",
                    "description": "最多返回的结果数，默认 50",
                    "minimum": 1,
                    "maximum": 500,
                },
            },
        },
    },
]

CODE_METHOD_NOT_FOUND = -32601
CODE_INVALID_PARAMS = -32602
CODE_INTERNAL_ERROR = -32603


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


def safe_path(path):
    """将路径安全转为可 JSON 序列化的字符串。

    Unix 上 os.walk 可能返回含 surrogateescape 的文件名（非 UTF-8 字节），
    json.dumps(ensure_ascii=False) 遇到孤立代理码会抛异常。这里提前用
    utf-8/replace 编解码把不可表示的字节替换为 U+FFFD，保证不崩。
    """
    try:
        path.encode("utf-8")
        return path
    except UnicodeEncodeError:
        return path.encode("utf-8", errors="replace").decode("utf-8")


def search_files(keyword_lower, directory, max_results):
    """递归搜索 directory 下文件名含 keyword 的文件，返回结果列表。

    参数 keyword_lower 已是小写，直接与 filename.lower() 比较。
    返回 (results, truncated)。
    """
    results = []
    truncated = False

    for root, dirs, files in os.walk(directory, followlinks=False):
        for filename in files:
            if keyword_lower in filename.lower():
                full_path = safe_path(os.path.join(root, filename))
                results.append({"path": full_path, "name": safe_path(filename)})
                if len(results) >= max_results:
                    truncated = True
                    return results, truncated

    return results, truncated


def call_tool(params):
    name = params.get("name")
    arguments = params.get("arguments") or {}

    if name != "search:files":
        return None, (CODE_METHOD_NOT_FOUND, "未知工具：{}".format(name))

    keyword = arguments.get("keyword")
    if not isinstance(keyword, str) or not keyword:
        return None, (CODE_INVALID_PARAMS, "参数 keyword 缺失或为空")

    directory = arguments.get("directory")
    if not directory:
        directory = os.path.expanduser("~")
    directory = os.path.expanduser(directory)

    if not os.path.isdir(directory):
        return None, (CODE_INVALID_PARAMS, "目录不存在或不是目录：{}".format(directory))

    max_results = arguments.get("max_results", 50)
    if not isinstance(max_results, int) or max_results < 1:
        max_results = 50
    max_results = min(max_results, 500)

    try:
        results, truncated = search_files(keyword.lower(), directory, max_results)
    except OSError as e:
        return None, (CODE_INTERNAL_ERROR, "搜索过程中出错：{}".format(e))

    return {"matches": results, "count": len(results), "truncated": truncated}, None


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

    log("插件退出")


if __name__ == "__main__":
    main()
