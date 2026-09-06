/**
 * 系统信息插件（Node.js）。
 *
 * 利用 Node.js 原生 os 模块暴露 CPU、内存、磁盘、网络接口等系统信息，
 * 展示「任何语言皆可做插件」的平台能力。
 *
 * 依赖：仅 Node.js 标准库（os / fs / child_process），无第三方包。
 */

"use strict";

const os = require("os");
const fs = require("fs");
const { Plugin, PluginError, ErrorCode } = require("../node-sdk/intools");

const plugin = new Plugin("1.0");

// ── 工具 1：system:info ────────────────────────────────────────────────

plugin.tool(
  "system:info",
  "返回操作系统、CPU、内存等基础系统信息",
  { type: "object", properties: {} },
  () => {
    const cpus = os.cpus();
    const totalMem = os.totalmem();
    const freeMem = os.freemem();
    const uptime = os.uptime();

    const hours = Math.floor(uptime / 3600);
    const minutes = Math.floor((uptime % 3600) / 60);

    return {
      hostname: os.hostname(),
      platform: os.platform(),
      arch: os.arch(),
      release: os.release(),
      node_version: process.version,
      cpu_model: cpus[0]?.model || "unknown",
      cpu_cores: cpus.length,
      total_memory_mb: Math.round(totalMem / 1024 / 1024),
      free_memory_mb: Math.round(freeMem / 1024 / 1024),
      memory_usage_percent: Math.round(((totalMem - freeMem) / totalMem) * 100),
      uptime: `${hours}h ${minutes}m`,
    };
  }
);

// ── 工具 2：system:cpu ─────────────────────────────────────────────────

plugin.tool(
  "system:cpu",
  "返回各 CPU 核心的型号、速度与使用率",
  { type: "object", properties: {} },
  () => {
    const cpus = os.cpus();
    return cpus.map((cpu, i) => {
      const total = Object.values(cpu.times).reduce((a, b) => a + b, 0);
      const idle = cpu.times.idle;
      return {
        core: i,
        model: cpu.model.trim(),
        speed_mhz: cpu.speed,
        usage_percent: Math.round(((total - idle) / total) * 100),
      };
    });
  }
);

// ── 工具 3：system:network ─────────────────────────────────────────────

plugin.tool(
  "system:network",
  "列出所有网络接口及其 IPv4/IPv6 地址",
  { type: "object", properties: {} },
  () => {
    const interfaces = os.networkInterfaces();
    const result = [];
    for (const [name, addrs] of Object.entries(interfaces)) {
      for (const addr of addrs) {
        // 跳过 IPv6 内部地址和 IPv4 回环，对用户没信息量。
        if (addr.internal && addr.family === "IPv6") continue;
        result.push({
          interface: name,
          address: addr.address,
          family: addr.family,
          mac: addr.mac,
          internal: addr.internal,
        });
      }
    }
    return result;
  }
);

// ── 工具 4：system:disk ────────────────────────────────────────────────

plugin.tool(
  "system:disk",
  "查询磁盘使用情况（仅 Windows）",
  { type: "object", properties: {} },
  () => {
    if (os.platform() !== "win32") {
      throw new PluginError(ErrorCode.PLUGIN, "磁盘查询仅支持 Windows");
    }

    const { execSync } = require("child_process");
    try {
      // WMIC 是 Windows 原生命令，不需要额外安装。
      const raw = execSync(
        "wmic logicaldisk where DriveType=3 get DeviceID,Size,FreeSpace,VolumeName /format:csv",
        { encoding: "utf-8", timeout: 5000 }
      );
      const lines = raw.trim().split("\n").filter(Boolean);
      // 第一行是表头，跳过
      return lines.slice(1).map((line) => {
        const cols = line.trim().split(",");
        const id = cols[1];
        const free = parseInt(cols[2], 10);
        const size = parseInt(cols[3], 10);
        return {
          drive: id,
          volume: cols[4] || "",
          total_gb: isNaN(size) ? "N/A" : (size / 1024 / 1024 / 1024).toFixed(1),
          free_gb: isNaN(free) ? "N/A" : (free / 1024 / 1024 / 1024).toFixed(1),
          usage_percent:
            isNaN(size) || isNaN(free) || size === 0
              ? "N/A"
              : Math.round(((size - free) / size) * 100),
        };
      });
    } catch (e) {
      throw new PluginError(ErrorCode.INTERNAL, `WMIC 调用失败: ${e.message}`);
    }
  }
);

// ── 工具 5：system:env ─────────────────────────────────────────────────

plugin.tool(
  "system:env",
  "列出环境变量（可通过 prefix 过滤前缀，如 PATH / NODE_）",
  {
    type: "object",
    properties: {
      prefix: {
        type: "string",
        description: "只返回以此开头的环境变量名（大小写不敏感），留空返回全部",
      },
    },
  },
  (args) => {
    const prefix = (args.prefix || "").toUpperCase();
    const entries = Object.entries(process.env)
      .filter(([key]) => !prefix || key.toUpperCase().startsWith(prefix))
      .sort(([a], [b]) => a.localeCompare(b))
      .map(([key, value]) => ({
        key,
        value: value.length > 200 ? value.slice(0, 200) + "…" : value,
      }));
    return { count: entries.length, entries };
  }
);

// ── 启动 ──────────────────────────────────────────────────────────────

plugin.start();
