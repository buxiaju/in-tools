#!/usr/bin/env node
/**
 * InTools manifest.toml 校验工具。
 *
 * 用法：
 *   node tools/validate.cjs plugins/my-plugin/manifest.toml
 *   node tools/validate.cjs plugins/          # 校验目录下所有 manifest
 *
 * 报告格式、权限、工具名冲突等问题，返回非零退出码表示有 error 级问题。
 */

"use strict";

const fs = require("fs");
const path = require("path");

// ── 简易 TOML 解析（只够处理 manifest 结构） ─────────────────────────

/**
 * 极简 TOML 解析器，支持 InTools manifest 用到的所有语法。
 * 完整 TOML 请用 toml npm 包；这里避免引入外部依赖。
 */
function parseToml(text) {
  const result = {};
  let currentSection = result;
  let currentPath = [];

  for (let lineNum = 1; lineNum <= text.split("\n").length; lineNum++) {
    const raw = text.split("\n")[lineNum - 1];
    const line = raw.replace(/#.*$/, "").trim();
    if (!line) continue;

    // 表头 [section] 或 [[array]]
    const arrayMatch = line.match(/^\[\[(.+)\]\]$/);
    const sectionMatch = line.match(/^\[(.+)\]$/);

    if (arrayMatch) {
      const key = arrayMatch[1].trim();
      currentPath = [key];
      if (!result[key]) result[key] = [];
      const item = {};
      result[key].push(item);
      currentSection = item;
      continue;
    }

    if (sectionMatch) {
      const pathStr = sectionMatch[1].trim();
      currentPath = pathStr.split(".").map(s => s.trim());
      let obj = result;
      for (let i = 0; i < currentPath.length - 1; i++) {
        if (!obj[currentPath[i]]) obj[currentPath[i]] = {};
        obj = obj[currentPath[i]];
      }
      const lastKey = currentPath[currentPath.length - 1];
      if (!obj[lastKey]) obj[lastKey] = {};
      currentSection = obj[lastKey];
      continue;
    }

    // 键值对
    const eqIdx = line.indexOf("=");
    if (eqIdx === -1) continue;

    const key = line.slice(0, eqIdx).trim();
    let valueStr = line.slice(eqIdx + 1).trim();

    // 去掉行内注释（但要尊重字符串内的 #）
    const commentIdx = findCommentOutsideString(valueStr);
    if (commentIdx !== -1) valueStr = valueStr.slice(0, commentIdx).trim();

    try {
      currentSection[key] = parseTomlValue(valueStr);
    } catch (e) {
      // 解析失败，保留原始字符串
      currentSection[key] = valueStr;
    }
  }

  return result;
}

function findCommentOutsideString(s) {
  let inString = false;
  let quoteChar = "";
  for (let i = 0; i < s.length; i++) {
    if (!inString) {
      if (s[i] === '"' || s[i] === "'") {
        inString = true;
        quoteChar = s[i];
      } else if (s[i] === "#") {
        return i;
      }
    } else {
      if (s[i] === quoteChar) inString = false;
    }
  }
  return -1;
}

function parseTomlValue(s) {
  if (s === "true") return true;
  if (s === "false") return false;
  if (/^-?\d+$/.test(s)) return parseInt(s, 10);
  if (/^-?\d+\.\d+$/.test(s)) return parseFloat(s);

  // 字符串
  if ((s.startsWith('"') && s.endsWith('"')) || (s.startsWith("'") && s.endsWith("'"))) {
    return s.slice(1, -1);
  }

  // 数组
  if (s.startsWith("[") && s.endsWith("]")) {
    const inner = s.slice(1, -1).trim();
    if (!inner) return [];
    // 简单按逗号分割（不处理嵌套）
    return inner.split(",").map(item => parseTomlValue(item.trim()));
  }

  // 内联表
  if (s.startsWith("{") && s.endsWith("}")) {
    const inner = s.slice(1, -1).trim();
    if (!inner) return {};
    const obj = {};
    for (const pair of splitInlinePairs(inner)) {
      const eqIdx = pair.indexOf("=");
      if (eqIdx === -1) continue;
      const k = pair.slice(0, eqIdx).trim();
      const v = pair.slice(eqIdx + 1).trim();
      obj[k] = parseTomlValue(v);
    }
    return obj;
  }

  return s;
}

function splitInlinePairs(s) {
  const pairs = [];
  let depth = 0;
  let current = "";
  for (const ch of s) {
    if (ch === "{" || ch === "[") depth++;
    if (ch === "}" || ch === "]") depth--;
    if (ch === "," && depth === 0) {
      pairs.push(current.trim());
      current = "";
    } else {
      current += ch;
    }
  }
  if (current.trim()) pairs.push(current.trim());
  return pairs;
}

// ── 校验逻辑 ────────────────────────────────────────────────────────

function validateManifest(filePath) {
  const errors = [];
  const warnings = [];

  let content;
  try {
    content = fs.readFileSync(filePath, "utf-8");
  } catch (e) {
    return { file: filePath, errors: [`无法读取: ${e.message}`], warnings: [], summary: null };
  }

  let data;
  try {
    data = parseToml(content);
  } catch (e) {
    return { file: filePath, errors: [`TOML 解析失败: ${e.message}`], warnings: [], summary: null };
  }

  // ── [plugin] ──
  if (!data.plugin) {
    errors.push("缺少 [plugin] 段");
  } else {
    const p = data.plugin;
    if (!p.id) errors.push("[plugin] 缺少 id");
    if (!p.name) errors.push("[plugin] 缺少 name");
    if (!p.version) errors.push("[plugin] 缺少 version");
    else if (p.version.split(".").length < 3) warnings.push(`版本号 '${p.version}' 不是三段语义化格式`);
    if (p.id && (p.id.includes("/") || p.id.includes("\\"))) errors.push(`插件 ID '${p.id}' 包含路径分隔符`);
    if (p.id && p.id.split(".").length < 2) warnings.push(`插件 ID '${p.id}' 不是反向域名格式`);
  }

  // ── [exec] ──
  if (!data.exec) {
    errors.push("缺少 [exec] 段");
  } else {
    if (!data.exec.command) errors.push("[exec].command 不能为空");
    const args = data.exec.args;
    if (Array.isArray(args) && args.length > 0 && !args[0]) {
      errors.push("[exec].args[0] 不能为空串");
    }
  }

  // ── [[tools]] ──
  const tools = Array.isArray(data.tools) ? data.tools : [];
  const toolNames = new Set();
  for (let i = 0; i < tools.length; i++) {
    const t = tools[i];
    const tname = t.name || "";
    if (!tname) {
      errors.push(`tools[${i}].name 为空`);
    } else if (toolNames.has(tname)) {
      errors.push(`工具名 '${tname}' 在插件内重复`);
    } else {
      toolNames.add(tname);
    }
  }

  // ── [capabilities] ──
  const perms = data.capabilities?.permissions || [];
  const HIGH_RISK = ["input:control", "process:spawn", "shell:exec", "system:manage", "persistence:install"];
  for (const p of perms) {
    if (HIGH_RISK.includes(p)) {
      warnings.push(`声明了高危权限 '${p}'，该插件不对 MCP 暴露`);
    }
  }

  // ── [shortcut] ──
  if (data.shortcut) {
    const sc = data.shortcut;
    if (sc.tool && !toolNames.has(sc.tool)) {
      errors.push(`shortcut.tool '${sc.tool}' 未在 [[tools]] 中声明`);
    }
  }

  // ── [[settings]] ──
  const settings = Array.isArray(data.settings) ? data.settings : [];
  const settingKeys = new Set();
  for (let i = 0; i < settings.length; i++) {
    const sf = settings[i];
    const key = sf.key || "";
    if (!key) {
      errors.push(`settings[${i}].key 为空`);
    } else if (settingKeys.has(key)) {
      errors.push(`settings[${i}].key '${key}' 重复`);
    } else {
      settingKeys.add(key);
    }
    if (sf.type === "select" && (!sf.options || sf.options.length === 0)) {
      errors.push(`settings[${i}] type=select 但 options 为空`);
    }
  }

  // ── [[result_display]] ──
  const displays = Array.isArray(data.result_display) ? data.result_display : [];
  for (const rd of displays) {
    if (rd.tool && !toolNames.has(rd.tool)) {
      warnings.push(`result_display.tool '${rd.tool}' 未匹配任何已声明的工具`);
    }
  }

  return {
    file: filePath,
    errors,
    warnings,
    summary: {
      plugin_id: data.plugin?.id || "N/A",
      plugin_name: data.plugin?.name || "N/A",
      version: data.plugin?.version || "N/A",
      tools_count: tools.length,
      permissions: perms,
      has_shortcut: !!data.shortcut,
      has_docs: !!data.docs,
      has_settings: settings.length > 0,
    },
  };
}

// ── 主程序 ──────────────────────────────────────────────────────────

const args = process.argv.slice(2);

if (args.length === 0) {
  console.log(`
InTools manifest.toml 校验工具

用法:
  node tools/validate.cjs <manifest.toml 路径>
  node tools/validate.cjs <插件目录>

示例:
  node tools/validate.cjs plugins/hello-plugin/manifest.toml
  node tools/validate.cjs plugins/
`);
  process.exit(0);
}

const target = args[0];
let results = [];

if (fs.existsSync(target) && fs.statSync(target).isDirectory()) {
  // 扫描目录下所有 manifest.toml
  const dirs = fs.readdirSync(target, { withFileTypes: true });
  for (const d of dirs) {
    if (d.isDirectory()) {
      const manifestPath = path.join(target, d.name, "manifest.toml");
      if (fs.existsSync(manifestPath)) {
        results.push(validateManifest(manifestPath));
      }
    }
  }
} else {
  results.push(validateManifest(target));
}

let totalErrors = 0;
let totalWarnings = 0;

for (const r of results) {
  totalErrors += r.errors.length;
  totalWarnings += r.warnings.length;

  const icon = r.errors.length > 0 ? "✗" : r.warnings.length > 0 ? "⚠" : "✓";
  console.log(`\n${icon} ${r.file}`);

  if (r.summary) {
    console.log(`  ${r.summary.plugin_name} (${r.summary.plugin_id}) v${r.summary.version}`);
    console.log(`  ${r.summary.tools_count} 工具 · ${r.summary.permissions.length} 权限`);
  }

  for (const e of r.errors) console.log(`  ERROR: ${e}`);
  for (const w of r.warnings) console.log(`  WARN:  ${w}`);
}

console.log(`\n${"─".repeat(40)}`);
console.log(`共 ${results.length} 个 manifest，${totalErrors} 个错误，${totalWarnings} 个警告`);

process.exit(totalErrors > 0 ? 1 : 0);
