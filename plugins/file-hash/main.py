"""文件哈希插件（Python SDK 版）。

计算文件的 MD5/SHA1/SHA256 哈希值，以及目录的递归哈希校验。
仅使用 Python 标准库。
"""

import hashlib
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


@plugin.tool(
    "hash:file",
    "计算单个文件的 MD5/SHA1/SHA256 哈希值",
    {
        "type": "object",
        "required": ["path"],
        "properties": {
            "path": {"type": "string", "description": "文件的绝对路径"},
        },
    },
)
def hash_file(args, ctx):
    path = args["path"]
    if not os.path.isfile(path):
        raise PluginError(ErrorCode.INVALID_PARAMS, f"文件不存在：{path}")

    md5 = hashlib.md5()
    sha1 = hashlib.sha1()
    sha256 = hashlib.sha256()
    size = 0

    try:
        with open(path, "rb") as f:
            while True:
                chunk = f.read(8192)
                if not chunk:
                    break
                md5.update(chunk)
                sha1.update(chunk)
                sha256.update(chunk)
                size += len(chunk)
    except OSError as e:
        raise PluginError(ErrorCode.INTERNAL, f"读取文件失败：{e}")

    return {
        "path": path,
        "filename": os.path.basename(path),
        "size": size,
        "md5": md5.hexdigest(),
        "sha1": sha1.hexdigest(),
        "sha256": sha256.hexdigest(),
    }


@plugin.tool(
    "hash:dir",
    "递归计算目录下所有文件的 SHA256",
    {
        "type": "object",
        "required": ["path"],
        "properties": {
            "path": {"type": "string", "description": "目录的绝对路径"},
        },
    },
)
def hash_dir(args, ctx):
    path = args["path"]
    if not os.path.isdir(path):
        raise PluginError(ErrorCode.INVALID_PARAMS, f"目录不存在：{path}")

    files = []
    total_size = 0

    try:
        for root, dirs, filenames in os.walk(path):
            for name in filenames:
                full = os.path.join(root, name)
                try:
                    h = hashlib.sha256()
                    fsize = 0
                    with open(full, "rb") as f:
                        while True:
                            chunk = f.read(8192)
                            if not chunk:
                                break
                            h.update(chunk)
                            fsize += len(chunk)
                    rel = os.path.relpath(full, path)
                    files.append({"path": rel, "size": fsize, "sha256": h.hexdigest()})
                    total_size += fsize
                except OSError:
                    files.append({"path": os.path.relpath(full, path), "size": 0, "sha256": "error"})
    except OSError as e:
        raise PluginError(ErrorCode.INTERNAL, f"遍历目录失败：{e}")

    return {
        "directory": path,
        "file_count": len(files),
        "total_size": total_size,
        "files": files,
    }


plugin.start()
