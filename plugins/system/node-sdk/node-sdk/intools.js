/**
 * InTools Node.js 插件 SDK v2。
 *
 * 新增：
 * - 反向 RPC：plugin.callTool / plugin.listTools / plugin.getConfig / plugin.setConfig
 * - 进度通知：plugin.notify(method, params) 推送 notify/progress 等
 * - 异步工具 handler 完整支持
 *
 * 用法：
 *   const { Plugin } = require("../node-sdk/intools");
 *   const plugin = new Plugin("1.0");
 *
 *   plugin.tool("my:task", "执行耗时任务", { ... }, async (args, ctx) => {
 *     ctx.notify("notify/progress", { percent: 0 });
 *     const tools = await ctx.listTools();       // 反向 RPC：列工具
 *     const r = await ctx.callTool("other:foo", { x: 1 }); // 跨插件调用
 *     ctx.notify("notify/progress", { percent: 100 });
 *     return { result: r };
 *   });
 *
 *   plugin.start();
 */

"use strict";

const readline = require("readline");

// ── JSON-RPC 错误码（与宿主约定） ────────────────────────────────────
const ErrorCode = {
  PARSE: -32700,
  INVALID_REQUEST: -32600,
  METHOD_NOT_FOUND: -32601,
  INVALID_PARAMS: -32602,
  INTERNAL: -32603,
  PLUGIN: -32000,
  CALL_DEPTH: -32001,
  PERMISSION_DENIED: -32002,
  TOOL_NOT_FOUND: -32003,
  TIMEOUT: -32004,
};

// ── 工具调用上下文 ────────────────────────────────────────────────────

/**
 * 工具调用上下文，作为 handler 的第二个参数传入。
 * 提供反向 RPC 和进度通知能力。
 */
class ToolContext {
  /** @param {Plugin} plugin */
  constructor(plugin) {
    this._plugin = plugin;
  }

  /**
   * 列出全部可用工具（跨插件）。
   * @returns {Promise<Array<{name: string, description: string, plugin_id: string}>>}
   */
  listTools() {
    return this._plugin._hostCall("host/listTools", {});
  }

  /**
   * 调用其他插件的工具。
   * @param {string} toolName - 工具全名，如 "ocr:recognize"
   * @param {object} args - 工具参数
   * @returns {Promise<any>}
   */
  callTool(toolName, args = {}) {
    return this._plugin._hostCall("host/callTool", { tool: toolName, arguments: args });
  }

  /**
   * 读取本插件的配置。
   * @returns {Promise<object>}
   */
  getConfig() {
    return this._plugin._hostCall("host/getConfig", {});
  }

  /**
   * 写入本插件的配置。
   * @param {object} value - 要写入的 JSON 值
   * @returns {Promise<any>}
   */
  setConfig(value) {
    return this._plugin._hostCall("host/setConfig", { value });
  }

  /**
   * 向宿主推送通知（经 Tauri 事件转发到前端）。
   * @param {string} method - 通知方法名，如 "notify/progress"
   * @param {object} params - 通知参数
   */
  notify(method, params = {}) {
    this._plugin._notify(method, params);
  }

  /**
   * 推送进度通知（notify/progress 的快捷方式）。
   * @param {object} info - 进度信息，如 { task_id: "abc", percent: 50, message: "处理中" }
   */
  progress(info) {
    this.notify("notify/progress", info);
  }

  /**
   * 推送流式文本片段（notify/stream 的快捷方式）。
   * @param {string} text - 文本片段
   * @param {string} [taskId] - 任务 ID（可选）
   */
  stream(text, taskId) {
    this.notify("notify/stream", { text, ...(taskId ? { task_id: taskId } : {}) });
  }

  /**
   * 推送工具调用开始通知（notify/toolCall 的快捷方式）。
   * @param {string} tool - 工具名
   * @param {object} args - 参数
   */
  toolCallStart(tool, args) {
    this.notify("notify/toolCall", { tool, arguments: args, phase: "start" });
  }

  /**
   * 推送工具调用完成通知。
   * @param {string} tool - 工具名
   * @param {any} result - 结果
   */
  toolCallEnd(tool, result) {
    this.notify("notify/toolCall", { tool, result, phase: "end" });
  }
}

// ── 插件主体 ──────────────────────────────────────────────────────────

class Plugin {
  /**
   * @param {string} protocolVersion - 协议版本，通常 "1.0"
   */
  constructor(protocolVersion = "1.0") {
    this._protocolVersion = protocolVersion;
    /** @type {Map<string, { description: string, inputSchema: object, handler: Function }>} */
    this._tools = new Map();
    /** @type {Map<string, Function>} */
    this._notifications = new Map();
    this._pluginDir = null;
    this._config = null;
    this._closed = false;

    // 反向 RPC 的请求/响应配对。
    this._nextRpcId = 1000;
    /** @type {Map<number, { resolve: Function, reject: Function }>} */
    this._pendingRpc = new Map();

    // stdin 行缓冲，供反向 RPC 等待响应时复用。
    this._lineQueue = [];
    this._lineWaiters = [];
  }

  /**
   * 注册一个工具。
   *
   * handler 签名：(args, ctx) => result
   *   - args: 工具参数对象
   *   - ctx: ToolContext，提供反向 RPC 和进度通知
   *   - 返回值或 Promise 的 resolve 值作为工具结果
   *   - 抛出 PluginError 即返回错误响应
   *
   * @param {string} name
   * @param {string} description
   * @param {object} inputSchema - JSON Schema
   * @param {Function} handler - (args, ctx) => result | Promise<result>
   * @returns {this}
   */
  tool(name, description, inputSchema, handler) {
    this._tools.set(name, { description, inputSchema: inputSchema || {}, handler });
    return this;
  }

  /**
   * 注册通知处理器。
   * @param {string} method
   * @param {Function} handler - (params) => void
   * @returns {this}
   */
  onNotification(method, handler) {
    this._notifications.set(method, handler);
    return this;
  }

  /** 插件目录（plugin/ready 时由宿主传入）。 */
  get pluginDir() { return this._pluginDir; }

  /** 插件配置（plugin/ready 时由宿主传入）。 */
  get config() { return this._config; }

  /**
   * 启动插件主循环。
   * 阻塞直到收到 plugin/shutdown 或 stdin 关闭。
   */
  start() {
    this._send({
      jsonrpc: "2.0",
      method: "plugin/hello",
      params: {
        protocol_version: this._protocolVersion,
        tools: this._toolDescriptors(),
      },
    });

    this._readLoop();
  }

  // ── 反向 RPC（内部） ──────────────────────────────────────────────

  /**
   * 发送 host/* 请求并等待响应。
   * 等待期间若收到宿主发来的请求，就地处理后再继续等。
   */
  _hostCall(method, params) {
    return new Promise((resolve, reject) => {
      const id = this._nextRpcId++;
      this._pendingRpc.set(id, { resolve, reject });
      this._send({ jsonrpc: "2.0", id, method, params });
      this._drainUntilResponse(id);
    });
  }

  /**
   * 读 stdin 直到收到指定 id 的响应。
   * 期间遇到的请求就地处理。
   */
  _drainUntilResponse(targetId) {
    const drain = () => {
      this._readLine().then((line) => {
        if (line === null) {
          // stdin EOF
          const pending = this._pendingRpc.get(targetId);
          if (pending) {
            this._pendingRpc.delete(targetId);
            pending.reject(new PluginError(ErrorCode.INTERNAL, "连接断开"));
          }
          return;
        }

        let msg;
        try { msg = JSON.parse(line); } catch { drain(); return; }

        // 是响应？
        if (msg.id !== undefined && !msg.method) {
          const waiter = this._pendingRpc.get(msg.id);
          if (waiter) {
            this._pendingRpc.delete(msg.id);
            if (msg.error) {
              waiter.reject(new PluginError(msg.error.code, msg.error.message));
            } else {
              waiter.resolve(msg.result);
            }
            if (msg.id === targetId) return; // 目标响应到了
          }
          drain();
          return;
        }

        // 是请求？就地处理
        if (msg.method && msg.id !== undefined) {
          this._handleRequest(msg.id, msg.method, msg.params || {});
          drain();
          return;
        }

        // 是通知？转发
        if (msg.method) {
          const handler = this._notifications.get(msg.method);
          if (handler) try { handler(msg.params || {}); } catch (e) { this._log(`通知处理异常: ${e}`); }
        }

        drain();
      });
    };
    drain();
  }

  // ── 行读取（队列 + 等待器模式） ─────────────────────────────────

  /**
   * 异步读一行 stdin。如果队列里有缓存行，直接返回；否则挂起等待。
   * 返回 null 表示 EOF。
   */
  _readLine() {
    if (this._lineQueue.length > 0) {
      return Promise.resolve(this._lineQueue.shift());
    }
    return new Promise((resolve) => {
      this._lineWaiters.push(resolve);
    });
  }

  /**
   * 投递一行到队列。如果有等待中的消费者，直接唤醒。
   */
  _feedLine(line) {
    if (this._lineWaiters.length > 0) {
      const waiter = this._lineWaiters.shift();
      waiter(line);
    } else {
      this._lineQueue.push(line);
    }
  }

  // ── 主循环 ──────────────────────────────────────────────────────

  _readLoop() {
    const rl = readline.createInterface({ input: process.stdin, terminal: false });

    rl.on("line", (line) => {
      if (this._closed) return;
      const trimmed = line.trim();
      if (!trimmed) return;
      this._feedLine(trimmed);
      this._processNextLine();
    });

    rl.on("close", () => {
      this._log("stdin 已关闭，插件退出");
      // 唤醒所有等待者
      for (const w of this._lineWaiters) w(null);
      this._lineWaiters = [];
      this._close();
    });
  }

  /**
   * 如果主循环空闲（没有正在进行的反向 RPC），处理队列里的下一行。
   * 如果有反向 RPC 在等，_drainUntilResponse 会自己消费行。
   */
  _processNextLine() {
    if (this._pendingRpc.size > 0) return; // 有反向 RPC 在等，不抢行

    this._readLine().then((line) => {
      if (line === null || this._closed) return;

      let msg;
      try { msg = JSON.parse(line); } catch {
        this._log(`丢弃无法解析的输入行: ${line.slice(0, 200)}`);
        return;
      }

      if (msg.method && msg.id !== undefined) {
        this._handleRequest(msg.id, msg.method, msg.params || {});
      } else if (msg.method) {
        const handler = this._notifications.get(msg.method);
        if (handler) try { handler(msg.params || {}); } catch (e) { this._log(`通知处理异常: ${e}`); }
      } else if (msg.id !== undefined) {
        const waiter = this._pendingRpc.get(msg.id);
        if (waiter) {
          this._pendingRpc.delete(msg.id);
          if (msg.error) waiter.reject(new PluginError(msg.error.code, msg.error.message));
          else waiter.resolve(msg.result);
        }
      }

      // 继续处理下一行
      if (this._pendingRpc.size === 0) this._processNextLine();
    });
  }

  // ── 请求处理 ──────────────────────────────────────────────────────

  _handleRequest(id, method, params) {
    try {
      const result = this._dispatch(method, params);
      if (result && typeof result.then === "function") {
        result.then((r) => this._replyResult(id, r)).catch((e) => this._replyError(id, e));
        return;
      }
      this._replyResult(id, result);
    } catch (e) {
      this._replyError(id, e);
    }
  }

  _dispatch(method, params) {
    if (method === "plugin/ready") {
      this._pluginDir = params.plugin_dir || null;
      this._config = params.config || {};
      this._log(`握手完成，插件目录：${this._pluginDir}`);
      return { ok: true };
    }

    if (method === "plugin/shutdown") {
      this._log("收到 shutdown 信号");
      setImmediate(() => this._close());
      return { ok: true };
    }

    if (method === "tools/list") {
      return { tools: this._toolDescriptors() };
    }

    if (method === "tools/call") {
      return this._callTool(params);
    }

    // 通知型方法
    const handler = this._notifications.get(method);
    if (handler) {
      handler(params);
      return { ok: true };
    }

    throw new PluginError(ErrorCode.METHOD_NOT_FOUND, `未知方法: ${method}`);
  }

  _callTool(params) {
    const name = params.name;
    const args = params.arguments || {};
    const entry = this._tools.get(name);
    if (!entry) throw new PluginError(ErrorCode.TOOL_NOT_FOUND, `未知工具: ${name}`);

    const ctx = new ToolContext(this);
    const result = entry.handler(args, ctx);
    if (result && typeof result.then === "function") return result;
    return result;
  }

  // ── 消息发送 ──────────────────────────────────────────────────────

  _notify(method, params) {
    this._send({ jsonrpc: "2.0", method, params });
  }

  _replyResult(id, result) {
    this._send({ jsonrpc: "2.0", id, result: result === undefined ? {} : result });
  }

  _replyError(id, e) {
    if (e instanceof PluginError) {
      this._send({ jsonrpc: "2.0", id, error: { code: e.code, message: e.message } });
    } else {
      this._log(`未处理异常: ${e.stack || e}`);
      this._send({ jsonrpc: "2.0", id, error: { code: ErrorCode.INTERNAL, message: String(e) } });
    }
  }

  _send(payload) {
    if (this._closed) return;
    process.stdout.write(JSON.stringify(payload) + "\n");
  }

  _toolDescriptors() {
    return [...this._tools.entries()].map(([name, t]) => ({
      name,
      description: t.description,
      input_schema: t.inputSchema,
    }));
  }

  _log(msg) {
    process.stderr.write(`[InTools] ${msg}\n`);
  }

  _close() {
    if (this._closed) return;
    this._closed = true;
    // 拒绝所有等待中的反向 RPC
    for (const [id, { reject }] of this._pendingRpc) {
      reject(new PluginError(ErrorCode.INTERNAL, "插件正在关闭"));
    }
    this._pendingRpc.clear();
    process.exit(0);
  }
}

/**
 * 插件业务错误。抛出后转成 JSON-RPC error 响应。
 */
class PluginError extends Error {
  constructor(code, message) {
    super(message);
    this.code = code;
    this.name = "PluginError";
  }
}

module.exports = { Plugin, PluginError, ErrorCode };
