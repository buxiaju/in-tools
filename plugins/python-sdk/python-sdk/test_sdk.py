"""SDK 快速验证脚本。用法：echo '{"jsonrpc":"2.0","method":"plugin/ready","id":1,"params":{}}' | python test_sdk.py"""
import sys, os, importlib.util
# 直接从文件加载，绕过目录名含连字符无法 import 的问题
_spec = importlib.util.spec_from_file_location("intools", os.path.join(os.path.dirname(os.path.abspath(__file__)), "intools.py"))
_mod = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_mod)
Plugin = _mod.Plugin

p = Plugin()

@p.tool("test:echo", "回显文本", {"type": "object", "required": ["text"], "properties": {"text": {"type": "string"}}})
def echo(args, ctx):
    return {"text": args["text"]}

@p.tool("test:info", "返回版本", {"type": "object", "properties": {}})
def info(args, ctx):
    return {"version": "1.0.0", "runtime": f"Python {sys.version.split()[0]}"}

p.start()
