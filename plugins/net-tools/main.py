"""网络工具插件。

Ping、DNS 查询、端口检测、HTTP 请求、本机 IP 获取。
仅使用 Python 标准库。
"""

import os
import socket
import subprocess
import sys
import urllib.request
import urllib.error
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
    "net:ping",
    "Ping 指定主机",
    {
        "type": "object",
        "required": ["host"],
        "properties": {
            "host": {"type": "string", "description": "主机名或 IP"},
            "count": {"type": "integer", "description": "Ping 次数，默认 4"},
        },
    },
)
def do_ping(args, ctx):
    host = args["host"]
    count = args.get("count", 4)
    try:
        result = subprocess.run(
            ["ping", "-n", str(count), host],
            capture_output=True, text=True, encoding="utf-8", errors="replace", timeout=30,
        )
        return {"host": host, "output": result.stdout.strip(), "success": result.returncode == 0}
    except subprocess.TimeoutExpired:
        raise PluginError(ErrorCode.INTERNAL, "Ping 超时")


@plugin.tool(
    "net:dns",
    "查询域名的 DNS 记录",
    {
        "type": "object",
        "required": ["domain"],
        "properties": {"domain": {"type": "string", "description": "要查询的域名"}},
    },
)
def do_dns(args, ctx):
    domain = args["domain"]
    try:
        results = socket.getaddrinfo(domain, None)
        ips = list(set(r[4][0] for r in results))
        return {"domain": domain, "ips": ips, "count": len(ips)}
    except socket.gaierror as e:
        raise PluginError(ErrorCode.INTERNAL, f"DNS 查询失败：{e}")


@plugin.tool(
    "net:check-port",
    "检测指定主机的端口是否开放",
    {
        "type": "object",
        "required": ["host", "port"],
        "properties": {
            "host": {"type": "string", "description": "主机名或 IP"},
            "port": {"type": "integer", "description": "端口号"},
            "timeout": {"type": "integer", "description": "超时秒数，默认 5"},
        },
    },
)
def do_check_port(args, ctx):
    host, port = args["host"], args["port"]
    timeout = args.get("timeout", 5)
    try:
        sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        sock.settimeout(timeout)
        result = sock.connect_ex((host, port))
        sock.close()
        return {"host": host, "port": port, "open": result == 0}
    except Exception as e:
        return {"host": host, "port": port, "open": False, "error": str(e)}


@plugin.tool(
    "net:http",
    "发送 HTTP GET 请求",
    {
        "type": "object",
        "required": ["url"],
        "properties": {
            "url": {"type": "string", "description": "请求 URL"},
            "timeout": {"type": "integer", "description": "超时秒数，默认 15"},
        },
    },
)
def do_http(args, ctx):
    url = args["url"]
    timeout = args.get("timeout", 15)
    try:
        req = urllib.request.Request(url, headers={"User-Agent": "InTools/1.0"})
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            body = resp.read().decode("utf-8", errors="replace")
            return {
                "status": resp.status,
                "headers": dict(resp.headers),
                "body": body[:50000],
                "length": len(body),
            }
    except urllib.error.HTTPError as e:
        return {"status": e.code, "error": str(e)}
    except Exception as e:
        raise PluginError(ErrorCode.INTERNAL, f"HTTP 请求失败：{e}")


@plugin.tool(
    "net:ip",
    "获取本机 IP 地址信息",
    {"type": "object", "properties": {}},
)
def do_ip(args, ctx):
    hostname = socket.gethostname()
    local_ip = socket.gethostbyname(hostname)
    # 获取外部 IP
    external_ip = None
    try:
        req = urllib.request.Request("https://api.ipify.org", headers={"User-Agent": "InTools/1.0"})
        with urllib.request.urlopen(req, timeout=5) as resp:
            external_ip = resp.read().decode("utf-8").strip()
    except Exception:
        pass
    return {"hostname": hostname, "local_ip": local_ip, "external_ip": external_ip}


plugin.start()
