"""文件操作插件。

覆盖文件系统的基本操作：读取、写入、列出目录、创建目录、
删除、复制、移动、获取文件信息。仅使用 Python 标准库。
"""

import os
import shutil
import sys
import time
import importlib.util

_sdk_path = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "python-sdk", "intools.py")
_spec = importlib.util.spec_from_file_location("intools", _sdk_path)
_sdk = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_sdk)

Plugin = _sdk.Plugin
PluginError = _sdk.PluginError
ErrorCode = _sdk.ErrorCode

plugin = Plugin()


def safe_stat(path):
    """获取文件/目录信息。"""
    st = os.stat(path)
    size = st.st_size
    if size > 1024 * 1024:
        size_str = f"{size / 1024 / 1024:.1f} MB"
    elif size > 1024:
        size_str = f"{size / 1024:.1f} KB"
    else:
        size_str = f"{size} B"
    return {
        "path": path,
        "name": os.path.basename(path),
        "type": "directory" if os.path.isdir(path) else "file",
        "size": size,
        "size_human": size_str,
        "modified": time.strftime("%Y-%m-%d %H:%M:%S", time.localtime(st.st_mtime)),
        "created": time.strftime("%Y-%m-%d %H:%M:%S", time.localtime(st.st_ctime)),
        "readonly": not os.access(path, os.W_OK),
    }


@plugin.tool(
    "file:read",
    "读取文本文件内容",
    {
        "type": "object",
        "required": ["path"],
        "properties": {
            "path": {"type": "string", "description": "文件路径"},
            "encoding": {"type": "string", "description": "编码，默认 utf-8"},
            "max_size": {"type": "integer", "description": "最大读取字节数，默认 1MB"},
        },
    },
)
def do_read(args, ctx):
    path = args["path"]
    if not os.path.isfile(path):
        raise PluginError(ErrorCode.INVALID_PARAMS, f"文件不存在：{path}")
    encoding = args.get("encoding", "utf-8")
    max_size = args.get("max_size", 1024 * 1024)
    size = os.path.getsize(path)
    if size > max_size:
        raise PluginError(ErrorCode.INVALID_PARAMS, f"文件过大（{size} 字节），超过限制 {max_size} 字节")
    try:
        with open(path, "r", encoding=encoding) as f:
            content = f.read()
    except UnicodeDecodeError:
        raise PluginError(ErrorCode.INVALID_PARAMS, f"无法以 {encoding} 编码读取，请尝试其他编码")
    return {"content": content, "length": len(content), "path": path}


@plugin.tool(
    "file:write",
    "写入文本文件（覆盖或追加）",
    {
        "type": "object",
        "required": ["path", "content"],
        "properties": {
            "path": {"type": "string", "description": "文件路径"},
            "content": {"type": "string", "description": "要写入的内容"},
            "mode": {"type": "string", "description": "write（覆盖）或 append（追加），默认 write"},
        },
    },
)
def do_write(args, ctx):
    path, content = args["path"], args["content"]
    mode = args.get("mode", "write")
    if mode not in ("write", "append"):
        raise PluginError(ErrorCode.INVALID_PARAMS, f"mode 必须是 write 或 append")
    try:
        os.makedirs(os.path.dirname(path) or ".", exist_ok=True)
        with open(path, "w" if mode == "write" else "a", encoding="utf-8") as f:
            f.write(content)
    except OSError as e:
        raise PluginError(ErrorCode.INTERNAL, f"写入失败：{e}")
    return {"ok": True, "path": path, "bytes_written": len(content.encode("utf-8"))}


@plugin.tool(
    "file:list",
    "列出目录内容",
    {
        "type": "object",
        "required": ["path"],
        "properties": {
            "path": {"type": "string", "description": "目录路径"},
            "pattern": {"type": "string", "description": "文件名过滤（通配符）"},
            "max_results": {"type": "integer", "description": "最多返回条目数，默认 100"},
        },
    },
)
def do_list(args, ctx):
    path = args["path"]
    if not os.path.isdir(path):
        raise PluginError(ErrorCode.INVALID_PARAMS, f"目录不存在：{path}")
    max_results = min(args.get("max_results", 100), 500)
    try:
        entries = []
        for name in os.listdir(path)[:max_results]:
            full = os.path.join(path, name)
            try:
                info = safe_stat(full)
                entries.append(info)
            except OSError:
                entries.append({"name": name, "type": "unknown", "error": "无法访问"})
        return {"path": path, "count": len(entries), "entries": entries}
    except OSError as e:
        raise PluginError(ErrorCode.INTERNAL, f"列出目录失败：{e}")


@plugin.tool(
    "file:mkdir",
    "创建目录（含父目录）",
    {
        "type": "object",
        "required": ["path"],
        "properties": {"path": {"type": "string", "description": "要创建的目录路径"}},
    },
)
def do_mkdir(args, ctx):
    path = args["path"]
    try:
        os.makedirs(path, exist_ok=True)
    except OSError as e:
        raise PluginError(ErrorCode.INTERNAL, f"创建目录失败：{e}")
    return {"ok": True, "path": path}


@plugin.tool(
    "file:delete",
    "删除文件或目录",
    {
        "type": "object",
        "required": ["path"],
        "properties": {
            "path": {"type": "string", "description": "要删除的路径"},
            "recursive": {"type": "boolean", "description": "是否递归删除目录，默认 false"},
        },
    },
)
def do_delete(args, ctx):
    path = args["path"]
    recursive = args.get("recursive", False)
    if not os.path.exists(path):
        raise PluginError(ErrorCode.INVALID_PARAMS, f"路径不存在：{path}")
    try:
        if os.path.isdir(path):
            if recursive:
                shutil.rmtree(path)
            else:
                os.rmdir(path)
        else:
            os.remove(path)
    except OSError as e:
        raise PluginError(ErrorCode.INTERNAL, f"删除失败：{e}")
    return {"ok": True, "deleted": path}


@plugin.tool(
    "file:copy",
    "复制文件或目录",
    {
        "type": "object",
        "required": ["src", "dst"],
        "properties": {
            "src": {"type": "string", "description": "源路径"},
            "dst": {"type": "string", "description": "目标路径"},
        },
    },
)
def do_copy(args, ctx):
    src, dst = args["src"], args["dst"]
    if not os.path.exists(src):
        raise PluginError(ErrorCode.INVALID_PARAMS, f"源路径不存在：{src}")
    try:
        if os.path.isdir(src):
            shutil.copytree(src, dst)
        else:
            os.makedirs(os.path.dirname(dst) or ".", exist_ok=True)
            shutil.copy2(src, dst)
    except OSError as e:
        raise PluginError(ErrorCode.INTERNAL, f"复制失败：{e}")
    return {"ok": True, "src": src, "dst": dst}


@plugin.tool(
    "file:move",
    "移动/重命名文件或目录",
    {
        "type": "object",
        "required": ["src", "dst"],
        "properties": {
            "src": {"type": "string", "description": "源路径"},
            "dst": {"type": "string", "description": "目标路径"},
        },
    },
)
def do_move(args, ctx):
    src, dst = args["src"], args["dst"]
    if not os.path.exists(src):
        raise PluginError(ErrorCode.INVALID_PARAMS, f"源路径不存在：{src}")
    try:
        os.makedirs(os.path.dirname(dst) or ".", exist_ok=True)
        shutil.move(src, dst)
    except OSError as e:
        raise PluginError(ErrorCode.INTERNAL, f"移动失败：{e}")
    return {"ok": True, "src": src, "dst": dst}


@plugin.tool(
    "file:info",
    "获取文件或目录的详细信息",
    {
        "type": "object",
        "required": ["path"],
        "properties": {"path": {"type": "string", "description": "文件或目录路径"}},
    },
)
def do_info(args, ctx):
    path = args["path"]
    if not os.path.exists(path):
        raise PluginError(ErrorCode.INVALID_PARAMS, f"路径不存在：{path}")
    return safe_stat(path)


plugin.start()
