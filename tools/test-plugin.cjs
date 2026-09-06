#!/usr/bin/env node
/**
 * InTools 插件测试工具。
 *
 * 模拟宿主行为，向插件发送 JSON-RPC 消息并验证响应。
 * 可用于 CI、开发调试、以及验证插件是否符合协议。
 *
 * 用法：
 *   node tools/test-plugin.cjs plugins/hello-plugin
 *   node tools/test-plugin.cjs plugins/hello-plugin --call hello:echo --args '{"text":"hi"}'
 *   node tools/test-plugin.cjs plugins/system-info --list-tools
 *
 * 测试项：
 * 1. manifest.toml 存在且可解析
 * 2. 插件进程可启动
 * 3. 插件在 10 秒内发出 plugin/hello
 * 4. 插件响应 plugin/ready
 * 5. tools/list 返回的工具与 manifest 一致
 * 6. （可选）调用指定工具并验证返回
 */

"use strict";

const fs = require("fs");
const path = require("path");
const { spawn } = require("child_process");
const readline = require("readline");

// ── 参数解析 ────────────────────────────────────────────────────────

const args = process.argv.slice(2);

if (args.length === 0 || args[0] === "--help" || args[0] === "-h") {
  console.log(`
InTools 插件测试工具

用法: node tools/test-plugin.cjs <插件目录> [选项]

选项:
  --list-tools      测试 tools/list 并打印工具清单
  --call <工具名>   调用指定工具
  --args <JSON>     工具参数（配合 --call）
  --manifest-only   只校验 manifest，不启动进程
  --timeout <ms>    握手超时（默认 10000ms）

示例:
  node tools/test-plugin.cjs plugins/hello-plugin
  node tools/test-plugin.cjs plugins/hello-plugin --call hello:echo --args '{"text":"hi"}'
  node tools/test-plugin.cjs plugins/system-info --list-tools
`);
  process.exit(0);
}

const pluginDir = path.resolve(args[0]);
let doListTools = args.includes("--list-tools");
let callTool = null;
let callArgs = {};
let manifestOnly = args.includes("--manifest-only");
let timeout = 10000;

for (let i = 0; i < args.length; i++) {
  if (args[i] === "--call" && args[i + 1]) callTool = args[++i];
  if (args[i] === "--args" && args[i + 1]) callArgs = JSON.parse(args[++i]);
  if (args[i] === "--timeout" && args[i + 1]) timeout = parseInt(args[++i], 10);
}

const results = [];
let exitCode = 0;

function ok(msg) { results.push({ status: "PASS", msg }); }
function fail(msg) { results.push({ status: "FAIL", msg }); exitCode = 1; }
function info(msg) { results.push({ status: "INFO", msg }); }

// ── 步骤 1：校验 manifest ──────────────────────────────────────────

const manifestPath = path.join(pluginDir, "manifest.toml");
if (!fs.existsSync(manifestPath)) {
  fail(`manifest.toml 不存在: ${manifestPath}`);
  printResults();
  process.exit(1);
}

let manifest;
try {
  manifest = parseManifest(fs.readFileSync(manifestPath, "utf-8"));
  ok("manifest.toml 解析成功");
} catch (e) {
  fail(`manifest.toml 解析失败: ${e.message}`);
  printResults();
  process.exit(1);
}

// 校验必填字段
const plugin = manifest.plugin || {};
const exec = manifest.exec || {};
if (!plugin.id) fail("[plugin].id 缺失"); else ok(`plugin.id: ${plugin.id}`);
if (!plugin.name) fail("[plugin].name 缺失"); else ok(`plugin.name: ${plugin.name}`);
if (!plugin.version) fail("[plugin].version 缺失"); else ok(`plugin.version: ${plugin.version}`);
if (!exec.command) fail("[exec].command 缺失"); else ok(`exec.command: ${exec.command}`);

const tools = Array.isArray(manifest.tools) ? manifest.tools : [];
ok(`声明了 ${tools.length} 个工具`);

if (manifestOnly) {
  printResults();
  process.exit(exitCode);
}

// ── 步骤 2：启动插件进程 ───────────────────────────────────────────

const command = exec.command;
const cmdArgs = Array.isArray(exec.args) ? exec.args : [];

info(`启动: ${command} ${cmdArgs.join(" ")}`);

const child = spawn(command, cmdArgs, {
  cwd: pluginDir,
  stdio: ["pipe", "pipe", "pipe"],
  shell: process.platform === "win32",
});

let stdoutLines = [];
let stderrLines = [];
let helloReceived = false;
let readySent = false;
let handshakeComplete = false;
let toolsFromPlugin = [];

const rl = readline.createInterface({ input: child.stdout });

rl.on("line", (line) => {
  const trimmed = line.trim();
  if (!trimmed) return;

  let msg;
  try { msg = JSON.parse(trimmed); } catch {
    info(`stdout 非 JSON: ${trimmed.slice(0, 100)}`);
    return;
  }

  stdoutLines.push(msg);

  // 握手第一步：收到 plugin/hello
  if (msg.method === "plugin/hello" && msg.id === undefined) {
    helloReceived = true;
    const pv = msg.params?.protocol_version;
    toolsFromPlugin = msg.params?.tools || [];
    ok(`收到 plugin/hello (protocol_version=${pv}, ${toolsFromPlugin.length} 工具)`);

    // 检查工具一致性
    const manifestNames = new Set(tools.map(t => t.name));
    const pluginNames = new Set(toolsFromPlugin.map(t => t.name));
    for (const name of manifestNames) {
      if (!pluginNames.has(name)) {
        fail(`工具 '${name}' 在 manifest 中声明但插件未上报`);
      }
    }
    for (const name of pluginNames) {
      if (!manifestNames.has(name)) {
        info(`工具 '${name}' 插件上报但 manifest 未声明（可能是运行时动态工具）`);
      }
    }

    // 发送 plugin/ready
    const ready = JSON.stringify({
      jsonrpc: "2.0",
      id: 1,
      method: "plugin/ready",
      params: { config: {}, plugin_dir: pluginDir },
    });
    child.stdin.write(ready + "\n");
    readySent = true;
    return;
  }

  // 响应
  if (msg.id !== undefined && !msg.method) {
    if (msg.id === 1) {
      if (msg.error) {
        fail(`plugin/ready 返回错误: ${msg.error.message}`);
      } else {
        handshakeComplete = true;
        ok("握手完成 (plugin/ready → ok)");
      }

      // 握手完成后，按需发后续请求
      if (doListTools) {
        const req = JSON.stringify({
          jsonrpc: "2.0", id: 2, method: "tools/list", params: {},
        });
        child.stdin.write(req + "\n");
      } else if (callTool) {
        const req = JSON.stringify({
          jsonrpc: "2.0", id: 2, method: "tools/call",
          params: { name: callTool, arguments: callArgs },
        });
        child.stdin.write(req + "\n");
      } else {
        // 没有后续请求，发送 shutdown
        const shutdown = JSON.stringify({
          jsonrpc: "2.0", id: 99, method: "plugin/shutdown", params: {},
        });
        child.stdin.write(shutdown + "\n");
      }
      return;
    }

    // 后续响应
    if (msg.error) {
      fail(`请求 ${msg.id} 返回错误: [${msg.error.code}] ${msg.error.message}`);
    } else {
      const result = msg.result;
      if (doListTools && result?.tools) {
        ok(`tools/list 返回 ${result.tools.length} 个工具:`);
        for (const t of result.tools) {
          info(`  ${t.name}: ${t.description || "(无描述)"}`);
        }
      } else if (callTool) {
        ok(`工具调用成功 (${callTool}):`);
        info(`  ${JSON.stringify(result, null, 2)}`);
      }
    }

    // 发 shutdown
    const shutdown = JSON.stringify({
      jsonrpc: "2.0", id: 99, method: "plugin/shutdown", params: {},
    });
    child.stdin.write(shutdown + "\n");
  }
});

child.stderr.on("data", (data) => {
  stderrLines.push(data.toString().trim());
});

child.on("error", (e) => {
  fail(`进程启动失败: ${e.message}`);
  printResults();
  process.exit(1);
});

// 超时保护
const timer = setTimeout(() => {
  if (!helloReceived) {
    fail(`握手超时（${timeout}ms 内未收到 plugin/hello）`);
  }
  child.kill();
  printResults();
  process.exit(exitCode);
}, timeout);

child.on("close", (code) => {
  clearTimeout(timer);
  if (helloReceived && handshakeComplete) {
    ok(`插件正常退出 (code=${code})`);
  } else if (helloReceived && !handshakeComplete) {
    fail(`插件在握手完成前退出 (code=${code})`);
  }
  printResults();
  process.exit(exitCode);
});

// ── 输出 ────────────────────────────────────────────────────────────

function printResults() {
  console.log(`\n${"═".repeat(50)}`);
  console.log(`插件测试: ${plugin.name || plugin.id || pluginDir}`);
  console.log(`${"═".repeat(50)}`);

  for (const r of results) {
    const icon = r.status === "PASS" ? "✓" : r.status === "FAIL" ? "✗" : "·";
    console.log(`  ${icon} ${r.msg}`);
  }

  const passed = results.filter(r => r.status === "PASS").length;
  const failed = results.filter(r => r.status === "FAIL").length;
  console.log(`\n${passed} 通过, ${failed} 失败`);
}

// ── 极简 TOML 解析 ─────────────────────────────────────────────────

function parseManifest(text) {
  const result = {};
  let current = result;
  const lines = text.split("\n");

  for (const raw of lines) {
    const line = raw.replace(/#.*$/, "").trim();
    if (!line) continue;

    const arrayMatch = line.match(/^\[\[(.+)\]\]$/);
    const sectionMatch = line.match(/^\[(.+)\]$/);

    if (arrayMatch) {
      const key = arrayMatch[1].trim();
      if (!result[key]) result[key] = [];
      const item = {};
      result[key].push(item);
      current = item;
      continue;
    }

    if (sectionMatch) {
      const pathParts = sectionMatch[1].split(".").map(s => s.trim());
      let obj = result;
      for (let i = 0; i < pathParts.length - 1; i++) {
        if (!obj[pathParts[i]]) obj[pathParts[i]] = {};
        obj = obj[pathParts[i]];
      }
      const lastKey = pathParts[pathParts.length - 1];
      if (!obj[lastKey]) obj[lastKey] = {};
      current = obj[lastKey];
      continue;
    }

    const eqIdx = line.indexOf("=");
    if (eqIdx === -1) continue;

    const key = line.slice(0, eqIdx).trim();
    let val = line.slice(eqIdx + 1).trim();

    // 简单值解析
    if (val === "true") val = true;
    else if (val === "false") val = false;
    else if (/^-?\d+$/.test(val)) val = parseInt(val, 10);
    else if ((val.startsWith('"') && val.endsWith('"')) || (val.startsWith("'") && val.endsWith("'"))) {
      val = val.slice(1, -1);
    }
    else if (val.startsWith("[") && val.endsWith("]")) {
      const inner = val.slice(1, -1).trim();
      if (!inner) { val = []; }
      else {
        val = inner.split(",").map(item => {
          item = item.trim();
          if ((item.startsWith('"') && item.endsWith('"')) || (item.startsWith("'") && item.endsWith("'"))) {
            return item.slice(1, -1);
          }
          return item;
        });
      }
    }

    current[key] = val;
  }

  return result;
}
