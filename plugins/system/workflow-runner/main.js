/**
 * 工作流插件（Node.js）。
 *
 * 演示「插件编排插件」：通过反向 RPC 调用其他插件的工具，串联成工作流。
 *
 * 能力：
 * - workflow:list  — 列出所有可用工具（反向 RPC demo）
 * -workflow:run    — 执行预定义工作流，逐步调用工具并推送进度
 * - workflow:chain — 用户自定义工具链，按顺序执行
 */

"use strict";

const { Plugin, PluginError, ErrorCode } = require("../node-sdk/intools");
const plugin = new Plugin("1.0");

// ── 工具 1：列出所有可用工具 ─────────────────────────────────────────

plugin.tool(
  "workflow:list",
  "列出宿主中所有可用的工具（跨插件）",
  { type: "object", properties: {} },
  async (_args, ctx) => {
    const tools = await ctx.listTools();
    return {
      total: tools.length,
      tools: tools.map((t) => ({
        name: t.name,
        plugin: t.plugin_id,
        description: t.description,
      })),
    };
  }
);

// ── 工具 2：执行预定义工作流 ─────────────────────────────────────────

/**
 * 内置工作流：系统健康检查。
 * 调用 system:info + system:cpu + system:disk，汇总成报告。
 */
const BUILTIN_WORKFLOWS = {
  "health-check": {
    name: "系统健康检查",
    description: "收集系统信息、CPU、磁盘使用情况，生成健康报告",
    steps: [
      { tool: "system:info", args: {}, label: "获取系统信息" },
      { tool: "system:cpu", args: {}, label: "获取 CPU 状态" },
      { tool: "system:disk", args: {}, label: "获取磁盘状态" },
    ],
  },
};

plugin.tool(
  "workflow:run",
  "执行内置工作流，逐步调用工具并推送进度",
  {
    type: "object",
    required: ["workflow"],
    properties: {
      workflow: {
        type: "string",
        description: "工作流名称，如 health-check",
      },
    },
  },
  async (args, ctx) => {
    const wf = BUILTIN_WORKFLOWS[args.workflow];
    if (!wf) {
      throw new PluginError(
        ErrorCode.INVALID_PARAMS,
        `未知工作流: ${args.workflow}，可用: ${Object.keys(BUILTIN_WORKFLOWS).join(", ")}`
      );
    }

    ctx.progress({ message: `开始执行「${wf.name}」`, percent: 0 });

    const results = {};
    for (let i = 0; i < wf.steps.length; i++) {
      const step = wf.steps[i];
      const percent = Math.round(((i + 1) / wf.steps.length) * 100);

      ctx.toolCallStart(step.tool, step.args);
      ctx.progress({ message: step.label, percent });

      try {
        const result = await ctx.callTool(step.tool, step.args);
        results[step.tool] = result;
        ctx.toolCallEnd(step.tool, { ok: true });
      } catch (e) {
        results[step.tool] = { error: e.message || String(e) };
        ctx.toolCallEnd(step.tool, { error: e.message });
      }
    }

    ctx.progress({ message: "工作流完成", percent: 100 });

    return {
      workflow: args.workflow,
      name: wf.name,
      steps_completed: wf.steps.length,
      results,
    };
  }
);

// ── 工具 3：自定义工具链 ─────────────────────────────────────────────

plugin.tool(
  "workflow:chain",
  "按顺序执行用户指定的工具链，前一步的结果可传入下一步",
  {
    type: "object",
    required: ["steps"],
    properties: {
      steps: {
        type: "array",
        description: "工具步骤列表",
        items: {
          type: "object",
          required: ["tool"],
          properties: {
            tool: { type: "string", description: "工具名" },
            args: { type: "object", description: "工具参数" },
            label: { type: "string", description: "步骤说明" },
          },
        },
      },
    },
  },
  async (args, ctx) => {
    const steps = args.steps;
    if (!Array.isArray(steps) || steps.length === 0) {
      throw new PluginError(ErrorCode.INVALID_PARAMS, "steps 不能为空");
    }

    ctx.progress({ message: `开始执行 ${steps.length} 步工具链`, percent: 0 });

    const results = [];
    let lastResult = null;

    for (let i = 0; i < steps.length; i++) {
      const step = steps[i];
      const label = step.label || step.tool;
      const percent = Math.round(((i + 1) / steps.length) * 100);

      // 支持 $prev 占位符：args 里的 "$prev" 会被替换为上一步的结果
      const stepArgs = substitutePrev(step.args || {}, lastResult);

      ctx.toolCallStart(step.tool, stepArgs);
      ctx.progress({ message: `步骤 ${i + 1}/${steps.length}: ${label}`, percent });

      try {
        lastResult = await ctx.callTool(step.tool, stepArgs);
        results.push({ step: i + 1, tool: step.tool, status: "ok", result: lastResult });
        ctx.toolCallEnd(step.tool, { ok: true });
      } catch (e) {
        const err = e.message || String(e);
        results.push({ step: i + 1, tool: step.tool, status: "error", error: err });
        ctx.toolCallEnd(step.tool, { error: err });
        // 工具链遇错即停
        ctx.progress({ message: `步骤 ${i + 1} 失败: ${err}`, percent });
        break;
      }
    }

    ctx.progress({ message: "工具链完成", percent: 100 });

    return {
      total_steps: steps.length,
      completed: results.length,
      results,
    };
  }
);

/**
 * 递归替换 args 中的 "$prev" 占位符为上一步结果。
 * 支持嵌套对象和数组。
 */
function substitutePrev(obj, prev) {
  if (obj === "$prev") return prev;
  if (Array.isArray(obj)) return obj.map((item) => substitutePrev(item, prev));
  if (obj && typeof obj === "object") {
    const result = {};
    for (const [k, v] of Object.entries(obj)) {
      result[k] = substitutePrev(v, prev);
    }
    return result;
  }
  return obj;
}

plugin.start();
