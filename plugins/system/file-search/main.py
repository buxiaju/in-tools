"""文件搜索插件（Python SDK 版）。

按文件名关键词搜索指定目录，仅使用 Python 标准库。
"""

import os
import sys
import importlib.util

_sdk_path = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "python-sdk", "intools.py")
_spec = importlib.util.spec_from_file_location("intools", _sdk_path)
_sdk = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_sdk)

Plugin = _sdk.Plugin
PluginError = _sdk.PluginError
ErrorCode = _sdk.ErrorCode

plugin = Plugin()


def safe_path(path):
    """将路径安全转为可 JSON 序列化的字符串。"""
    try:
        path.encode("utf-8")
        return path
    except UnicodeEncodeError:
        return path.encode("utf-8", errors="replace").decode("utf-8")


def search_files(keyword_lower, directory, max_results):
    """递归搜索文件名含 keyword 的文件。"""
    results = []
    truncated = False
    for root, dirs, files in os.walk(directory, followlinks=False):
        for filename in files:
            if keyword_lower in filename.lower():
                full_path = safe_path(os.path.join(root, filename))
                results.append({"path": full_path, "name": safe_path(filename)})
                if len(results) >= max_results:
                    return results, True
    return results, truncated


@plugin.tool(
    "search:files",
    "按文件名关键词搜索指定目录下的文件，返回匹配的文件路径列表",
    {
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
            },
        },
    },
)
def do_search(args, ctx):
    keyword = args.get("keyword")
    if not isinstance(keyword, str) or not keyword:
        raise PluginError(ErrorCode.INVALID_PARAMS, "参数 keyword 缺失或为空")

    directory = args.get("directory") or os.path.expanduser("~")
    directory = os.path.expanduser(directory)
    if not os.path.isdir(directory):
        raise PluginError(ErrorCode.INVALID_PARAMS, f"目录不存在或不是目录：{directory}")

    max_results = args.get("max_results", 50)
    if not isinstance(max_results, int) or max_results < 1:
        max_results = 50
    max_results = min(max_results, 500)

    try:
        results, truncated = search_files(keyword.lower(), directory, max_results)
    except OSError as e:
        raise PluginError(ErrorCode.INTERNAL, f"搜索过程中出错：{e}")

    return {"matches": results, "count": len(results), "truncated": truncated}


plugin.start()
