// InTools 前端。原生 ES module，无打包器：靠 tauri.conf.json 的 withGlobalTauri
// 拿到 window.__TAURI__，靠 type="module" 的延迟执行保证 DOM 已就绪。
//
// 三条约定：
// 1. command 的**入参**经 #[tauri::command] 宏转成 camelCase（pluginId / toolName），
//    而**返回**的 DTO 没加 serde rename_all，键仍是 snake_case（plugin_id / last_error）。
//    两侧不对称是 Tauri 的既有行为，不是笔误，改任何一侧都会静默断链。
// 2. CSP 是 script-src 'self'，不允许行内 on* 属性，所有交互一律 addEventListener。
// 3. 一切工具调用都走 call_tool command，前端不持有任何特权路径。

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const EVENT_PERMISSION_PROMPT = "intools://permission-prompt";
const EVENT_PLUGIN_NOTIFICATION = "intools://plugin-notification";

const ROUTES = ["plugins", "chat", "permissions", "settings"];

/** 已卸载但仍在内存注册表里的插件 id。后端删了目录，列表要重启才会真的消失。 */
const uninstalled = new Set();

const $ = (id) => document.getElementById(id);

// ─────────────────── 通用工具 ───────────────────

/** 一律用 textContent 写入，不拼 innerHTML：插件名、错误串都来自插件自己，不可信。 */
function el(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined && text !== null) node.textContent = String(text);
  return node;
}

function clear(node) {
  node.replaceChildren();
}

function showEmpty(node, text) {
  node.replaceChildren(el("p", "empty", text));
}

let toastTimer = null;
function toast(message, isError = false) {
  const node = $("toast");
  node.textContent = message;
  node.classList.toggle("error", isError);
  node.hidden = false;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => {
    node.hidden = true;
  }, isError ? 5000 : 2500);
}

/**
 * 包裹一次 invoke：失败弹 toast 并返回 undefined。
 *
 * 后端已把所有错误拍平成 String，所以这里 String(e) 就够了；catch 里不再区分类型。
 */
async function call(cmd, args) {
  try {
    return await invoke(cmd, args);
  } catch (e) {
    toast(`${cmd} 失败：${e}`, true);
    return undefined;
  }
}

/** 按钮在异步期间置灰，避免连点导致重复启停。 */
async function withBusy(button, fn) {
  const previous = button.disabled;
  button.disabled = true;
  try {
    await fn();
  } finally {
    button.disabled = previous;
  }
}

// ─────────────────── 插件页 ───────────────────

const STATE_LABELS = {
  stopped: "已停止",
  starting: "启动中",
  idle: "空闲",
  busy: "忙碌",
  stopping: "停止中",
  error: "错误",
};

function pluginCard(p) {
  const card = el("div", "card");
  if (uninstalled.has(p.id)) card.classList.add("removed");

  const head = el("div", "card-head");
  const title = el("div", "card-title");
  title.append(el("strong", null, p.name), el("span", "mono", p.id));
  // badge 的状态修饰类与后端 state_label 的取值一一对应。
  title.append(el("span", `badge ${p.state}`, STATE_LABELS[p.state] ?? p.state));
  // 禁用是独立于运行态的持久化标志——让用户在列表里一眼看到，避免把
  // 「启动/停止按钮失效」误归咎于 bug。
  if (!p.enabled) {
    title.append(el("span", "badge disabled", "已禁用"));
  }
  head.append(title);

  const actions = el("div", "card-actions");
  if (uninstalled.has(p.id)) {
    actions.append(el("span", "field-hint", "已卸载，点重载即可移除"));
  } else {
    const running = p.state !== "stopped" && p.state !== "error";
    const toggle = el("button", "btn", running ? "停止" : "启动");
    toggle.addEventListener("click", () =>
      withBusy(toggle, async () => {
        const ok = await call(running ? "stop_plugin" : "start_plugin", {
          pluginId: p.id,
        });
        if (ok !== undefined) {
          toast(running ? `${p.name} 已停止` : `${p.name} 已启动`);
          await renderPlugins();
        }
      }),
    );

    // 「启用 / 禁用」是持久化标志，独立于「启动 / 停止」：
    // 停止只回收当前进程，下一次按需调用还会被拉起来；禁用则拦截
    // `ensure_running` 与 `start_eager_plugins`，并把工具从列表里隐掉。
    const enable = el(
      "button",
      `btn ${p.enabled ? "" : "muted"}`,
      p.enabled ? "禁用" : "启用",
    );
    enable.addEventListener("click", () =>
      withBusy(enable, async () => {
        const next = !p.enabled;
        const changed = await call("set_plugin_enabled", {
          pluginId: p.id,
          enabled: next,
        });
        if (changed !== undefined) {
          toast(next ? `${p.name} 已启用` : `${p.name} 已禁用（不再自动启动）`);
          await renderPlugins();
        }
      }),
    );

    const remove = el("button", "btn danger", "卸载");
    remove.addEventListener("click", () =>
      withBusy(remove, async () => {
        // 删目录不可逆，值得一次确认。
        if (!confirm(`卸载 ${p.name}？将删除插件目录并撤销其全部授权。`)) return;
        const ok = await call("uninstall_plugin", { pluginId: p.id });
        if (ok !== undefined) {
          uninstalled.add(p.id);
          toast(`${p.name} 已卸载，点「重载插件」即可从列表移除`);
          await renderPlugins();
        }
      }),
    );

    const cfg = el("button", "btn", "设置");
    cfg.addEventListener("click", () => openPluginSettings(p.id, p.name));

    actions.append(enable, toggle, cfg, remove);

    // 说明与快捷键都是插件作者的可选项：没声明就不放按钮，免得点开只看到报错。
    if (p.has_docs) {
      const docs = el("button", "btn", "使用说明");
      docs.addEventListener("click", () => withBusy(docs, () => openPluginDocs(p.id)));
      actions.insertBefore(docs, remove);
    }
    if (p.has_shortcut) {
      const keys = el("button", "btn", "快捷键");
      keys.addEventListener("click", () =>
        withBusy(keys, () => openShortcutEditor(p.id, p.name)),
      );
      actions.insertBefore(keys, remove);
    }
  }
  head.append(actions);
  card.append(head);

  if (p.description) card.append(el("p", "card-meta", p.description));

  const meta = [`v${p.version}`, p.author, p.lifecycle];
  if (p.inflight > 0) meta.push(`进行中 ${p.inflight}`);
  if (p.restart_attempts > 0) meta.push(`重启 ${p.restart_attempts} 次`);
  card.append(el("p", "card-meta", meta.filter(Boolean).join(" · ")));

  if (p.tools.length > 0) {
    card.append(el("p", "card-meta", `工具：${p.tools.join("、")}`));
  }
  if (p.permissions.length > 0) {
    card.append(el("p", "card-meta", `权限：${p.permissions.join("、")}`));
  }
  if (p.last_error) card.append(el("p", "card-error", p.last_error));

  return card;
}

async function renderPlugins() {
  const list = $("plugins-list");
  const plugins = await call("list_plugins");
  if (!plugins) return;
  if (plugins.length === 0) {
    showEmpty(list, "插件目录为空。在设置页确认插件目录后重启。");
    return;
  }
  list.replaceChildren(...plugins.map(pluginCard));
}

/**
 * 处理选中的 zip 包。
 *
 * 字节以 `Array.from(new Uint8Array(...))` 过 IPC：Tauri 的 command 参数走 JSON
 * 序列化，`Vec<u8>` 对应的就是数字数组。几 MB 的包这样传有可观开销，但导入是低频
 * 手动操作，换取的是「不额外引入任何插件或自定义协议」。
 */
async function importPackage(button) {
  const input = $("import-file");
  const file = input.files?.[0];
  // 用户在系统对话框里点了取消。
  if (!file) return;

  await withBusy(button, async () => {
    const bytes = Array.from(new Uint8Array(await file.arrayBuffer()));
    const result = await call("import_plugin_package", { zipBytes: bytes });
    if (result !== undefined) {
      toast(`已导入插件《${result.plugin_name}》，点「重载插件」即可生效`);
      await renderPlugins();
    }
  });

  // 不清空的话，连续导入同一个文件不会再触发 change 事件——选中值没变。
  input.value = "";
}

/**
 * 重载插件：重新扫描磁盘，整体替换内存注册表。
 *
 * 与导入不同，重载是即时生效的——新增的插件立刻出现在列表里，被删的插件
 * 进程会被回收。用户改了 manifest、手动删了插件目录、或想清掉已卸载插件
 * 的残留行时，点这个按钮即可，不用重启宿主。
 */
async function reloadPlugins() {
  const result = await call("reload_plugins");
  if (result === undefined) return;

  const parts = [`共 ${result.total} 个插件`];
  if (result.added.length > 0) parts.push(`新增 ${result.added.join("、")}`);
  if (result.removed.length > 0) parts.push(`移除 ${result.removed.join("、")}`);
  if (result.load_failures.length > 0) parts.push(`${result.load_failures.length} 个加载失败`);
  if (result.conflicts.length > 0) parts.push(`${result.conflicts.length} 个工具冲突`);
  toast(parts.join("，"));

  await renderPlugins();
}

// ─────────────────── 对话页 ───────────────────

function appendMsg(kind, head, body) {
  const log = $("chat-log");
  const msg = el("div", `msg ${kind}`);
  msg.append(el("div", "msg-head", head));
  if (body !== undefined) msg.append(el("div", "msg-body", body));
  log.append(msg);
  // 手动调用与流式通知都应把最新内容顶到眼前。
  log.scrollTop = log.scrollHeight;
}

function stringify(value) {
  return typeof value === "string" ? value : JSON.stringify(value, null, 2);
}

async function submitChat(event) {
  event.preventDefault();
  const input = $("chat-input");
  const message = input.value.trim();
  if (!message) return;

  appendMsg("call", "你", message);
  input.value = "";

  const button = $("chat-form").querySelector("button[type=submit]");
  await withBusy(button, async () => {
    try {
      const result = await invoke("call_tool", {
        toolName: "ai:chat",
        args: { message },
      });
      appendMsg("result", "AI", result.response || stringify(result));
    } catch (e) {
      appendMsg("error", "AI 失败", String(e));
    }
  });
}

// ─────────────────── 权限页 ───────────────────

function grantCard(g) {
  const card = el("div", "card");
  const head = el("div", "card-head");
  const title = el("div", "card-title");
  title.append(el("span", "mono", g.permission));
  title.append(el("span", `badge ${g.granted ? "idle" : "error"}`, g.granted ? "已允许" : "已拒绝"));
  head.append(title);

  const revoke = el("button", "btn danger", "撤销该插件全部授权");
  revoke.addEventListener("click", () =>
    withBusy(revoke, async () => {
      const ok = await call("revoke_grants", { pluginId: g.plugin_id });
      if (ok !== undefined) {
        toast("已撤销，下次调用会重新询问");
        await renderPermissions();
      }
    }),
  );
  const actions = el("div", "card-actions");
  actions.append(revoke);
  head.append(actions);
  card.append(head);

  card.append(el("p", "card-meta", `${g.plugin_id} · ${g.at}`));
  if (g.paths.length > 0) card.append(el("p", "card-meta", `路径：${g.paths.join("、")}`));
  return card;
}

function exposureCard(t) {
  const card = el("div", "card");
  const label = el("label", "field checkbox");
  const box = el("input");
  box.type = "checkbox";
  box.checked = t.exposed;
  box.addEventListener("change", async () => {
    box.disabled = true;
    try {
      const changed = await call("set_tool_exposed", {
        pluginId: t.plugin_id,
        toolName: t.name,
        exposed: box.checked,
      });
      if (changed === undefined) {
        // 写盘失败就把勾回滚，别让界面显示一个并不存在的状态。
        box.checked = !box.checked;
      }
    } finally {
      box.disabled = false;
    }
  });

  label.append(box, el("span", "mono", t.name));
  card.append(label);
  card.append(el("p", "card-meta", `${t.plugin_id}${t.description ? ` · ${t.description}` : ""}`));
  return card;
}

async function renderPermissions() {
  const [grants, tools, audit] = await Promise.all([
    call("list_grants"),
    call("list_tools"),
    call("list_mcp_audit", { limit: 50 }),
  ]);

  const grantsList = $("grants-list");
  if (grants) {
    if (grants.length === 0) {
      showEmpty(grantsList, "暂无永久授权。会话内的临时授权不在此列出。");
    } else {
      grantsList.replaceChildren(...grants.map(grantCard));
    }
  }

  const exposureList = $("exposure-list");
  if (tools) {
    if (tools.length === 0) {
      showEmpty(exposureList, "暂无可暴露的工具。");
    } else {
      exposureList.replaceChildren(...tools.map(exposureCard));
    }
  }

  const auditList = $("audit-list");
  if (audit) {
    if (audit.length === 0) {
      showEmpty(auditList, "暂无 MCP 调用记录。发生过调用后会按时间倒序展示。");
    } else {
      auditList.replaceChildren(...audit.map(auditCard));
    }
  }
}

/**
 * 把 MCP 调用记录渲染成一行。
 *
 * 「调用方」这一列去掉了 `mcp:` 前缀——这是实现细节，不该泄漏给用户。
 * 「耗时 / 结果」压在一格里，失败用 danger 修饰类标红——失败次数是用户
 * 排查异常客户端最常盯的指标，颜色比文字更易扫读。
 */
function auditCard(r) {
  const caller = r.caller.startsWith("mcp:") ? r.caller.slice(4) : r.caller;
  const ok = r.outcome === "ok";
  const card = el("div", `card audit ${ok ? "" : "error"}`);
  card.append(
    el("strong", null, caller),
    el("span", "mono", r.tool),
    el("span", "field-hint", new Date(r.timestamp).toLocaleString()),
    el("span", "field-hint", `${r.duration_ms} ms`),
    el("span", `badge ${ok ? "idle" : "high"}`, ok ? "成功" : r.outcome),
    el("span", "field-hint mono", r.args_summary || "（无参数）"),
  );
  return card;
}

// ─────────────────── 设置页 ───────────────────

function fillSettings(s) {
  $("settings-dir").value = s.plugins_dir;
  $("settings-dir-hint").textContent = `当前生效：${s.effective_plugins_dir}`;
  $("settings-log").value = s.log_level;
  $("settings-close").value = s.close_behavior;
  $("settings-mcp").checked = s.mcp_enabled;
  $("settings-token").value = s.mcp_token;
  $("settings-token").placeholder = s.mcp_token ? "" : "尚未生成";
  fillMcpStatus(s);
}

/**
 * 后端返回的各客户端配置片段。缓存在这里而不是每次切换都回后端拿：
 * 片段只随 Token / 端点变化，而这两者只在 fillSettings 时刷新。
 */
let mcpSnippets = [];

/**
 * 开关状态与实际监听状态是两回事，必须分别显示。配置写着「开启」而端口没起来
 * （多半被占）时，只看勾选框会以为一切正常，排查起来毫无线索。
 */
function fillMcpStatus(s) {
  const status = $("settings-mcp-status");
  if (s.mcp_running) {
    status.className = "field-hint ok";
    status.textContent = `监听中：${s.mcp_endpoint}`;
  } else if (s.mcp_enabled) {
    status.className = "field-hint warn";
    status.textContent = `配置为开启，但端口未在监听——${s.mcp_endpoint} 多半被其他程序占用，关掉它再重新开启。`;
  } else {
    status.className = "field-hint";
    status.textContent = "未开启。开启后本机的 MCP 客户端才能连上。";
  }

  fillMcpSnippets(s.mcp_snippets || [], !!s.mcp_token);
}

/**
 * 填充客户端下拉框。
 *
 * 每种客户端的配置 schema 并不通用——Claude Desktop 只认 stdio 形态，把 http
 * 形态粘进去会让它把整个 mcpServers 块重写掉。所以片段必须按客户端区分，
 * 不能只给一份让用户自己猜。
 */
function fillMcpSnippets(snippets, hasToken) {
  mcpSnippets = snippets;
  const select = $("settings-client");
  // 重建选项前记住当前选择，否则每次刷新设置页都会跳回第一项。
  const previous = select.value;
  select.textContent = "";
  for (const s of snippets) {
    const option = document.createElement("option");
    option.value = s.id;
    option.textContent = s.label;
    select.appendChild(option);
  }
  if (snippets.some((s) => s.id === previous)) select.value = previous;

  showMcpSnippet();
  // 没 Token 时片段里只有占位符，复制出去也是废的，直接禁用避免误导。
  $("settings-copy").disabled = !hasToken;
}

/** 把当前选中客户端的片段与说明显示出来。 */
function showMcpSnippet() {
  const current = mcpSnippets.find((s) => s.id === $("settings-client").value);
  $("settings-snippet").value = current ? current.snippet : "";
  $("settings-client-path").textContent = current ? current.config_path : "";
  $("settings-client-note").textContent = current ? current.note : "";
}

async function renderSettings() {
  const s = await call("get_settings");
  if (s) fillSettings(s);
  await renderAiConfig();
  await renderShortcuts();
}

async function submitSettings(event) {
  event.preventDefault();
  const button = $("settings-form").querySelector("button[type=submit]");
  await withBusy(button, async () => {
    const s = await call("save_settings", {
      pluginsDir: $("settings-dir").value,
      logLevel: $("settings-log").value,
      closeBehavior: $("settings-close").value,
    });
    if (s) {
      fillSettings(s);
      $("settings-status").textContent = "已保存，重启后生效";
    }
  });
}

async function toggleMcp() {
  const box = $("settings-mcp");
  box.disabled = true;
  try {
    const s = await call("set_mcp_enabled", { enabled: box.checked });
    if (s) {
      fillSettings(s);
    } else {
      // 后端已经回滚了配置，这里不能只翻勾选框就完事——得回读一次真实状态，
      // 否则「开关」与「是否在监听」两条信息又会各说各话。
      box.checked = !box.checked;
      await renderSettings();
    }
  } finally {
    box.disabled = false;
  }
}

async function copySnippet() {
  const button = $("settings-copy");
  await withBusy(button, async () => {
    const text = $("settings-snippet").value;
    try {
      await navigator.clipboard.writeText(text);
      toast("配置片段已复制");
    } catch (err) {
      // 剪贴板可能被 webview 权限挡下。此时把文本选中，用户按 Ctrl+C 仍能拿走。
      $("settings-snippet").select();
      toast(`无法自动复制（${err}），已选中文本，请手动复制`, true);
    }
  });
}

// ─────────────────── 插件设置弹窗 ───────────────────

let currentSettingsPluginId = null;

async function openPluginSettings(pluginId, pluginName) {
  currentSettingsPluginId = pluginId;
  $("ps-title").textContent = `${pluginName} 设置`;

  const data = await call("get_plugin_settings", { pluginId });
  if (!data) return;

  const form = $("ps-form");
  form.replaceChildren();

  if (!data.fields || data.fields.length === 0) {
    form.append(el("p", "empty", "该插件无可配置参数。"));
  } else {
    for (const f of data.fields) {
      const label = el("label", "field");
      label.append(el("span", "label", f.label));

      let input;
      const current = data.values[f.key] ?? f.default;

      if (f.field_type === "boolean") {
        input = el("input");
        input.type = "checkbox";
        input.checked = !!current;
        label.classList.add("checkbox");
        label.insertBefore(input, label.firstChild);
        $("ps-form").append(label);
        continue;
      } else if (f.field_type === "select") {
        input = el("select", "input");
        for (const opt of f.options) {
          const o = el("option", null, opt);
          o.value = opt;
          if (opt === current) o.selected = true;
          input.append(o);
        }
      } else if (f.field_type === "number") {
        input = el("input", "input");
        input.type = "number";
        input.value = current ?? "";
      } else {
        input = el("input", "input");
        input.type = "text";
        input.value = current ?? "";
      }
      input.dataset.key = f.key;
      input.dataset.type = f.field_type;
      label.append(input);
      form.append(label);
    }
  }

  $("plugin-settings-modal").hidden = false;
}

async function savePluginSettingsModal() {
  if (!currentSettingsPluginId) return;
  const form = $("ps-form");
  const values = {};

  for (const input of form.querySelectorAll("input, select")) {
    const key = input.dataset.key;
    if (!key) continue;
    const type = input.dataset.type;
    if (type === "boolean") {
      values[key] = input.checked;
    } else if (type === "number") {
      values[key] = input.value === "" ? null : Number(input.value);
    } else {
      values[key] = input.value;
    }
  }

  const ok = await call("save_plugin_settings", {
    pluginId: currentSettingsPluginId,
    values,
  });
  if (ok !== undefined) {
    toast("设置已保存");
    $("plugin-settings-modal").hidden = true;
  }
}

// ─────────────────── 快捷键 ───────────────────

/**
 * `KeyboardEvent.code` → 后端 `shortcut::normalize_key` 认得的键名。
 *
 * 用 `code` 而不是 `key`：`key` 会随布局和修饰键变化（按下 Shift+2 时 `key` 是
 * `@`），而全局热键注册的是物理键位，`code` 才和它对得上。
 */
const NAMED_CODES = new Set([
  "Space", "Enter", "Tab", "Backspace", "Delete", "Insert",
  "Home", "End", "PageUp", "PageDown",
  "Comma", "Period", "Slash", "Semicolon", "Quote", "Backquote",
  "Minus", "Equal", "BracketLeft", "BracketRight",
]);

function codeToKeyName(code) {
  if (code.startsWith("Key")) return code.slice(3);
  if (code.startsWith("Digit")) return code.slice(5);
  if (code.startsWith("Arrow")) return code.slice(5);
  if (/^F([1-9]|1[0-9]|2[0-4])$/.test(code)) return code;
  if (NAMED_CODES.has(code)) return code;
  return null;
}

/**
 * 拼出后端要求的组合键写法。
 *
 * 修饰键顺序必须是 Ctrl → Alt → Shift → Super：`global-hotkey` 的解析器不接受
 * 别的顺序，这个固定次序是硬要求而非风格偏好。
 */
function eventToShortcut(event) {
  const name = codeToKeyName(event.code);
  if (!name) return null;
  const parts = [];
  if (event.ctrlKey) parts.push("Ctrl");
  if (event.altKey) parts.push("Alt");
  if (event.shiftKey) parts.push("Shift");
  if (event.metaKey) parts.push("Super");
  // 裸键会立刻抢走全系统的这个按键，不给用户这个机会。
  if (parts.length === 0) return null;
  parts.push(name);
  return parts.join("+");
}

let shortcutDraft = null;
let capturing = false;

function setCaptureLabel(text) {
  $("sk-capture").textContent = text || "未绑定";
}

function showShortcutWarn(text) {
  const node = $("sk-warn");
  node.textContent = text ?? "";
  node.hidden = !text;
}

async function openShortcutEditor(pluginId, pluginName) {
  const data = await call("get_plugin_shortcut", { pluginId });
  if (!data) return;

  shortcutDraft = { pluginId, key: data.key, enabled: data.enabled };
  $("sk-title").textContent = `${pluginName} 快捷键`;
  setCaptureLabel(data.key);
  $("sk-enabled").checked = data.enabled;
  $("sk-default").textContent = data.default_key
    ? `插件预设：${data.default_key}（当前来自${data.source === "user" ? "你的设置" : "插件预设"}）`
    : "插件未给预设键，需要你指定一个。";
  showShortcutWarn(null);
  stopCapture();
  $("shortcut-modal").hidden = false;
}

function startCapture() {
  capturing = true;
  $("sk-capture").classList.add("capturing");
  setCaptureLabel("请按键…");
}

function stopCapture() {
  capturing = false;
  $("sk-capture").classList.remove("capturing");
}

function onCaptureKeydown(event) {
  if (!capturing) return;
  event.preventDefault();
  // 只按了修饰键，等它按完整。
  if (["Control", "Alt", "Shift", "Meta"].includes(event.key)) return;
  if (event.code === "Escape") {
    stopCapture();
    setCaptureLabel(shortcutDraft?.key);
    return;
  }
  const combo = eventToShortcut(event);
  if (!combo) {
    showShortcutWarn("这个组合不支持：至少要一个修饰键，主键限字母、数字、F 键或常见符号。");
    return;
  }
  shortcutDraft.key = combo;
  showShortcutWarn(null);
  stopCapture();
  setCaptureLabel(combo);
}

async function saveShortcut() {
  if (!shortcutDraft) return;
  const result = await call("set_plugin_shortcut", {
    pluginId: shortcutDraft.pluginId,
    key: shortcutDraft.key,
    enabled: $("sk-enabled").checked,
  });
  // 冲突和非法键位走 Err，call 已经弹过 toast，弹窗留着让用户改。
  if (result === undefined) return;
  $("shortcut-modal").hidden = true;
  // result 非 null 表示存下了但系统没接受，属于要让用户知道的半成功。
  toast(result ? `已保存，但未能注册：${result}` : "快捷键已生效", !!result);
  await renderShortcuts();
}

/** 清掉用户覆盖，回落到 manifest 预设。 */
async function resetShortcut() {
  if (!shortcutDraft) return;
  const result = await call("set_plugin_shortcut", {
    pluginId: shortcutDraft.pluginId,
    key: "",
    enabled: true,
  });
  if (result === undefined) return;
  $("shortcut-modal").hidden = true;
  toast(result ? `已恢复默认，但未能注册：${result}` : "已恢复插件预设", !!result);
  await renderShortcuts();
}

function shortcutRow(b) {
  const card = el("div", "card");
  const row = el("div", "shortcut-row");
  row.append(el("strong", "keys mono", b.key));
  row.append(el("span", null, b.plugin_name));
  row.append(el("span", "mono field-hint", b.tool));
  if (b.source === "user") row.append(el("span", "badge", "已改键"));
  if (!b.active) row.append(el("span", "badge error", "未生效"));
  card.append(row);
  return card;
}

async function renderShortcuts() {
  const list = $("shortcut-list");
  const data = await call("list_shortcut_bindings");
  if (!data) return;

  const nodes = data.bindings.map(shortcutRow);
  for (const issue of data.issues) {
    const card = el("div", "card");
    const owner = issue.owner_plugin ? `（占用者：${issue.owner_plugin}）` : "";
    card.append(el("p", "card-error", `${issue.plugin_id}：${issue.reason}${owner}`));
    nodes.push(card);
  }

  if (nodes.length === 0) {
    showEmpty(list, "没有插件声明快捷键。");
    return;
  }
  list.replaceChildren(...nodes);
}

// ─────────────────── 使用说明弹窗 ───────────────────

/**
 * 极简 Markdown 渲染：标题、围栏代码块、无序列表、表格、分隔线、段落、行内代码。
 *
 * 为什么手写而不用库：一是这个前端没有打包器，装不了 npm 包；二是 README 由插件
 * 作者提供，属于不可信输入，这里全程只用 `textContent` 建节点，从根上没有 XSS 的
 * 入口。代价是不支持图片、链接跳转、嵌套列表这些花样——说明文档够用了。
 */
function renderMarkdown(text) {
  const out = document.createDocumentFragment();
  const lines = text.replace(/\r\n/g, "\n").split("\n");
  let i = 0;
  let paragraph = [];

  const flushParagraph = () => {
    if (paragraph.length === 0) return;
    out.append(inlineNodes("p", null, paragraph.join(" ")));
    paragraph = [];
  };

  while (i < lines.length) {
    const line = lines[i];

    if (line.startsWith("```")) {
      flushParagraph();
      const buf = [];
      i += 1;
      while (i < lines.length && !lines[i].startsWith("```")) {
        buf.push(lines[i]);
        i += 1;
      }
      i += 1; // 吃掉收尾的围栏
      const pre = el("pre");
      pre.textContent = buf.join("\n");
      const copyBtn = el("button", "copy-code", "复制");
      copyBtn.type = "button";
      copyBtn.addEventListener("click", () => {
        navigator.clipboard.writeText(pre.textContent).then(
          () => {
            copyBtn.textContent = "已复制";
            setTimeout(() => { copyBtn.textContent = "复制"; }, 1500);
          },
          () => { copyBtn.textContent = "失败"; },
        );
      });
      const wrapper = el("div", "code-block");
      wrapper.append(copyBtn, pre);
      out.append(wrapper);
      continue;
    }

    const heading = /^(#{1,6})\s+(.*)$/.exec(line);
    if (heading) {
      flushParagraph();
      const level = Math.min(heading[1].length, 3);
      out.append(inlineNodes(`h${level}`, null, heading[2]));
      i += 1;
      continue;
    }

    if (/^(-{3,}|\*{3,})$/.test(line.trim())) {
      flushParagraph();
      out.append(el("hr"));
      i += 1;
      continue;
    }

    if (/^\s*\|/.test(line) && i + 1 < lines.length && isTableDivider(lines[i + 1])) {
      flushParagraph();
      const table = el("table", "docs-table");
      const thead = el("thead");
      const head = el("tr");
      splitTableRow(line).forEach((cell) => head.append(inlineNodes("th", null, cell)));
      thead.append(head);
      table.append(thead);
      const body = el("tbody");
      i += 2; // 表头 + 分隔行
      while (i < lines.length && /^\s*\|/.test(lines[i])) {
        const tr = el("tr");
        splitTableRow(lines[i]).forEach((cell) => tr.append(inlineNodes("td", null, cell)));
        body.append(tr);
        i += 1;
      }
      table.append(body);
      out.append(table);
      continue;
    }

    if (/^\s*[-*+]\s+/.test(line)) {
      flushParagraph();
      const ul = el("ul");
      while (i < lines.length && /^\s*[-*+]\s+/.test(lines[i])) {
        ul.append(inlineNodes("li", null, lines[i].replace(/^\s*[-*+]\s+/, "")));
        i += 1;
      }
      out.append(ul);
      continue;
    }

    if (line.trim() === "") {
      flushParagraph();
    } else {
      paragraph.push(line.trim());
    }
    i += 1;
  }

  flushParagraph();
  return out;
}

/** 表格分隔行，形如 `| --- | :-: |`。它是表格与普通竖线文本的唯一区分标志。 */
function isTableDivider(line) {
  return /^\s*\|(\s*:?-+:?\s*\|)+\s*$/.test(line.trimEnd());
}

/** 拆一行表格单元格：去掉首尾竖线再按竖线切，空单元格要保留，否则列会错位。 */
function splitTableRow(line) {
  return line.trim().replace(/^\|/, "").replace(/\|$/, "").split("|").map((cell) => cell.trim());
}

/** 建一个元素，其中 `` `x` `` 变成 code 节点，其余按纯文本拼。 */
function inlineNodes(tag, className, text) {
  const node = el(tag, className);
  // 奇数下标是被反引号包住的片段。
  text.split("`").forEach((chunk, index) => {
    if (chunk === "") return;
    node.append(index % 2 === 1 ? el("code", null, chunk) : document.createTextNode(chunk));
  });
  return node;
}

async function openPluginDocs(pluginId) {
  const data = await call("get_plugin_docs", { pluginId });
  if (!data) return;
  $("docs-title").textContent = `${data.plugin_name} 使用说明`;
  $("docs-file").textContent = data.filename;
  const search = $("docs-search");
  const body = $("docs-body");
  body.replaceChildren(renderMarkdown(data.content));
  body.scrollTop = 0;
  search.value = "";
  $("docs-modal").hidden = false;
  search.focus();
}

/** 文档搜索：按文本内容过滤顶层元素，不匹配的隐藏。 */
function filterDocs(query) {
  const q = query.trim().toLowerCase();
  for (const child of $("docs-body").children) {
    child.style.display = q === "" || child.textContent.toLowerCase().includes(q) ? "" : "none";
  }
}

// ─────────────────── AI 配置 ───────────────────

async function renderAiConfig() {
  const config = await call("get_ai_config");
  if (!config) return;
  $("ai-base-url").value = config.base_url;
  $("ai-api-key").value = config.api_key;
  $("ai-model").value = config.model;
  $("ai-config-status").textContent = "";
}

async function submitAiConfig(event) {
  event.preventDefault();
  const button = $("ai-config-form").querySelector("button[type=submit]");
  await withBusy(button, async () => {
    const config = await call("set_ai_config", {
      baseUrl: $("ai-base-url").value.trim(),
      apiKey: $("ai-api-key").value.trim(),
      model: $("ai-model").value.trim() || "deepseek-chat",
    });
    if (config) {
      $("ai-model").value = config.model;
      $("ai-config-status").textContent = "已保存";
    }
  });
}

// ─────────────────── 权限弹窗 ───────────────────

const DANGER_LABELS = { low: "低危", medium: "中危", high: "高危" };

/** 待处理的询问队列。多个插件可能同时请求授权，一次只弹一个，其余排队。 */
const promptQueue = [];
let currentPrompt = null;

function showNextPrompt() {
  if (currentPrompt || promptQueue.length === 0) return;
  currentPrompt = promptQueue.shift();
  const p = currentPrompt;

  $("prompt-body").replaceChildren(
    document.createTextNode(`插件请求 `),
    el("span", `badge ${p.danger}`, DANGER_LABELS[p.danger] ?? p.danger),
    document.createTextNode(` 权限，是否允许？`),
  );
  $("prompt-plugin").textContent = p.plugin_id;
  $("prompt-permission").textContent = p.permission;
  $("prompt-caller").textContent = p.caller;
  // 工具名 / 参数摘要：与具体工具无关的询问会得到 null/rust null，
  // 渲染成「N/A」而非空白，免得让人怀疑是不是数据丢了。
  $("prompt-tool").textContent = p.tool ?? "N/A";
  $("prompt-args").textContent = p.args_summary ?? "N/A";
  $("prompt-queue").textContent =
    promptQueue.length > 0 ? `还有 ${promptQueue.length} 条请求等待处理` : "";
  $("prompt-modal").hidden = false;
}

async function decide(decision) {
  if (!currentPrompt) return;
  const { id } = currentPrompt;
  currentPrompt = null;
  $("prompt-modal").hidden = true;

  const accepted = await call("respond_permission_prompt", { id, decision });
  // false 表示这条询问已不存在（调用方放弃或重复答复），不是错误，只提示一下。
  if (accepted === false) toast("该请求已失效");

  // 授权状态可能变了，权限页正开着就顺手刷新。
  if (location.hash.slice(1) === "permissions") await renderPermissions();
  showNextPrompt();
}

// ─────────────────── 事件订阅 ───────────────────

function subscribe() {
  // listen 是 core:event 插件命令，受 capability ACL 约束；被拒时只会 reject
  // 返回的 Promise。不接住的话事件永远收不到而界面毫无痕迹，必须显式报错。
  const guard = (name) => (error) =>
    toast(`订阅 ${name} 失败：${error}`, true);

  listen(EVENT_PERMISSION_PROMPT, ({ payload }) => {
    promptQueue.push(payload);
    showNextPrompt();
  }).catch(guard(EVENT_PERMISSION_PROMPT));

  listen(EVENT_PLUGIN_NOTIFICATION, ({ payload }) => {
    const { plugin_id, method, params } = payload;
    if (method === "notify/stream" && params && typeof params.delta === "string") {
      appendMsg("notify", "AI", params.delta);
    } else if (method === "notify/toolCall" && params) {
      const tool = params.tool || "?";
      const status = params.status || "";
      const label = status === "running" ? `调用 ${tool}…`
        : status === "error" ? `${tool} 失败`
        : `${tool} 完成`;
      const body = params.arguments ? stringify(params.arguments)
        : params.error ? params.error
        : params.result ? stringify(params.result)
        : undefined;
      appendMsg("notify", label, body);
    } else if (method === "notify/streamEnd") {
      // 流结束标记，不需要单独显示一行
    } else {
      appendMsg("notify", `${plugin_id} · ${method}`, stringify(params));
    }
  }).catch(guard(EVENT_PLUGIN_NOTIFICATION));
}

// ─────────────────── 路由 ───────────────────

const RENDERERS = {
  plugins: renderPlugins,
  chat: () => {},
  permissions: renderPermissions,
  settings: renderSettings,
};

function navigate() {
  const hash = location.hash.slice(1);
  const route = ROUTES.includes(hash) ? hash : ROUTES[0];

  for (const r of ROUTES) {
    $(`view-${r}`).classList.toggle("active", r === route);
  }
  for (const node of document.querySelectorAll(".nav-item")) {
    node.classList.toggle("active", node.dataset.route === route);
  }
  // 每次进入都重新拉数据：状态、授权、配置都可能被后端或其他页面改过，
  // 缓存带来的收益远小于显示过期状态的代价。
  RENDERERS[route]();
}

function bind() {
  document
    .querySelector('[data-act="reload-plugins"]')
    .addEventListener("click", () =>
      withBusy(document.querySelector('[data-act="reload-plugins"]'), reloadPlugins),
    );
  document
    .querySelector('[data-act="reload-permissions"]')
    .addEventListener("click", renderPermissions);

  // 按钮只负责唤起系统文件对话框，真正的活儿在 input 的 change 里，
  // 因为选文件是异步的、且用户可能取消。
  const importButton = document.querySelector('[data-act="import-plugin"]');
  importButton.addEventListener("click", () => $("import-file").click());
  $("import-file").addEventListener("change", () => importPackage(importButton));

  document.querySelector('[data-act="clear-chat"]').addEventListener("click", () => {
    clear($("chat-log"));
  });

  $("chat-form").addEventListener("submit", submitChat);
  $("settings-form").addEventListener("submit", submitSettings);
  $("ai-config-form").addEventListener("submit", submitAiConfig);
  $("settings-mcp").addEventListener("change", toggleMcp);
  $("settings-client").addEventListener("change", showMcpSnippet);
  $("settings-copy").addEventListener("click", copySnippet);

  for (const button of document.querySelectorAll(".modal-actions button[data-decision]")) {
    button.addEventListener("click", () => decide(button.dataset.decision));
  }

  $("ps-cancel").addEventListener("click", () => {
    $("plugin-settings-modal").hidden = true;
  });
  $("ps-save").addEventListener("click", () => savePluginSettingsModal());

  $("sk-capture").addEventListener("click", startCapture);
  // 监听挂在 window 上而不是捕获按钮上：Tab、Escape 这类键会先被浏览器的焦点管理
  // 吃掉，只有在顶层拦下来才抓得全。capturing 标志保证平时不干扰正常输入。
  window.addEventListener("keydown", onCaptureKeydown);
  $("sk-reset").addEventListener("click", () => withBusy($("sk-reset"), resetShortcut));
  $("sk-cancel").addEventListener("click", () => {
    stopCapture();
    $("shortcut-modal").hidden = true;
  });
  $("sk-save").addEventListener("click", () => withBusy($("sk-save"), saveShortcut));
  $("docs-close").addEventListener("click", () => {
    $("docs-modal").hidden = true;
  });
  $("docs-search").addEventListener("input", (e) => filterDocs(e.target.value));

  window.addEventListener("hashchange", navigate);
}

bind();
subscribe();
navigate();
