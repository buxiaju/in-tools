// InTools 前端。原生 ES module，无打包器：靠 tauri.conf.json 的 withGlobalTauri
// 拿到 window.__TAURI__，靠 type="module" 的延迟执行保证 DOM 已就绪。
//
// 三条约定：
// 1. command 的**入参**经 #[tauri::command] 宏转成 camelCase（pluginId / toolName），
//    而**返回**的 DTO 没加 serde rename_all，键仍是 snake_case（plugin_id / last_error）。
//    两侧不对称是 Tauri 的既有行为，不是笔误，改任何一侧都会静默断链。
// 2. CSP 是 script-src 'self'，不允许行内 on* 属性，所有交互一律 addEventListener。
// 3. 一切工具调用都走 call_tool command，前端不持有任何特权路径。

// Tauri 桥接。用 withGlobalTauri 注入的 window.__TAURI__ 在 webview 里通常是
// 同步可用的，但为了不让桥缺失时把整个模块踩死在顶层（导致路由/侧边栏全部失效
// 而只剩白屏），这里不直接解构，而是惰性取用：
// - 桥就绪时，invoke/listen 透传真实实现；
// - 桥缺失时，invoke/listen 返回一个已 reject 的 Promise，由既有 call() 的
//   try/catch 和 subscribe() 的 .catch() 兜住，弹 toast 说明原因，
//   页面结构（侧边栏、各视图切换）依然正常渲染。
const __TAURI__ = window.__TAURI__;

const invoke = (cmd, args) =>
  new Promise((resolve, reject) => {
    if (!__TAURI__ || !__TAURI__.core || typeof __TAURI__.core.invoke !== "function") {
      reject(new Error("Tauri 桥未注入（window.__TAURI__ 不可用），无法调用后端命令"));
      return;
    }
    __TAURI__.core.invoke(cmd, args).then(resolve, reject);
  });

const listen = (event, handler) =>
  new Promise((resolve, reject) => {
    if (!__TAURI__ || !__TAURI__.event || typeof __TAURI__.event.listen !== "function") {
      reject(new Error("Tauri 桥未注入，无法订阅事件"));
      return;
    }
    __TAURI__.event.listen(event, handler).then(resolve, reject);
  });

const EVENT_PERMISSION_PROMPT = "intools://permission-prompt";
const EVENT_PLUGIN_NOTIFICATION = "intools://plugin-notification";

const ROUTES = ["plugins", "chat", "permissions", "dev", "settings"];

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

function showEmpty(node, text, hint) {
  const wrap = el("div", "empty");
  wrap.innerHTML = `<div class="empty-icon"><svg viewBox="0 0 48 48" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"><rect x="8" y="8" width="32" height="32" rx="4"/><path d="M18 20h12M18 26h8"/></svg></div><p>${text}</p>${hint ? `<p class="empty-hint">${hint}</p>` : ""}`;
  node.replaceChildren(wrap);
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

/** 按权限动作部分判断危险度，用于药丸着色。 */
function permPermClass(perm) {
  // "file:write:*" → action = "write"
  const action = perm.split(":")[1]?.split(/[[(]/)[0];
  if (["control", "spawn", "exec", "install", "uninstall", "socket"].includes(action))
    return "perm-high";
  if (["write", "capture", "record", "modify", "manage"].includes(action))
    return "perm-mid";
  return "";
}

const STATE_LABELS = {
  stopped: "已停止",
  starting: "启动中",
  idle: "空闲",
  busy: "忙碌",
  stopping: "停止中",
  error: "错误",
};

const CATEGORY_LABELS = {
  system: "系统",
  test: "测试",
  user: "用户",
};

function pluginCard(p) {
  const card = el("div", "card");
  if (uninstalled.has(p.id)) card.classList.add("removed");

  const head = el("div", "card-head");
  const title = el("div", "card-title");
  title.append(el("strong", null, p.name), el("span", "mono", p.id));
  // badge 的状态修饰类与后端 state_label 的取值一一对应。
  title.append(el("span", `badge ${p.state}`, STATE_LABELS[p.state] ?? p.state));
  // 分类标签：系统 / 测试 / 用户
  const catLabel = CATEGORY_LABELS[p.category] || p.category;
  title.append(el("span", `badge cat-${p.category}`, catLabel));
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
    const primaryLabel = !p.enabled ? "启用" : running ? "停止" : "启动";
    const primary = el("button", "btn primary", primaryLabel);
    primary.addEventListener("click", () =>
      withBusy(primary, async () => {
        if (!p.enabled) {
          const changed = await call("set_plugin_enabled", { pluginId: p.id, enabled: true });
          if (changed !== undefined) {
            await call("start_plugin", { pluginId: p.id });
            toast(`${p.name} 已启用并启动`);
            await renderPlugins();
          }
        } else if (running) {
          const ok = await call("stop_plugin", { pluginId: p.id });
          if (ok !== undefined) {
            toast(`${p.name} 已停止`);
            await renderPlugins();
          }
        } else {
          const ok = await call("start_plugin", { pluginId: p.id });
          if (ok !== undefined) {
            toast(`${p.name} 已启动`);
            await renderPlugins();
          }
        }
      }),
    );

    const cfg = el("button", "btn", "设置");
    cfg.addEventListener("click", () => openPluginSettings(p.id, p.name, p.enabled));

    const remove = el("button", "btn danger", "卸载");
    remove.addEventListener("click", () =>
      withBusy(remove, async () => {
        if (!confirm(`卸载 ${p.name}？将删除插件目录并撤销其全部授权。`)) return;
        const ok = await call("uninstall_plugin", { pluginId: p.id });
        if (ok !== undefined) {
          uninstalled.add(p.id);
          toast(`${p.name} 已卸载，点「重载插件」即可从列表移除`);
          await renderPlugins();
        }
      }),
    );

    actions.append(primary, cfg, remove);

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

  if (p.description) card.append(el("p", "card-desc", p.description));

  const meta = [`v${p.version}`, p.author, p.lifecycle];
  if (p.inflight > 0) meta.push(`进行中 ${p.inflight}`);
  if (p.restart_attempts > 0) meta.push(`重启 ${p.restart_attempts} 次`);
  card.append(el("p", "card-meta", meta.filter(Boolean).join(" · ")));

  // 工具 + 权限药丸行
  if (p.tools.length > 0 || p.permissions.length > 0) {
    const pills = el("div", "card-pills");
    for (const t of p.tools) {
      const pill = el("span", "pill tool", t);
      pills.append(pill);
    }
    for (const perm of p.permissions) {
      const cls = permPermClass(perm);
      const pill = el("span", `pill perm ${cls}`, perm);
      pills.append(pill);
    }
    card.append(pills);
  }
  if (p.last_error) card.append(el("p", "card-error", p.last_error));

  // 可折叠详情区
  const detail = el("div", "card-detail hidden");
  detail.dataset.pluginId = p.id;

  // 工具表格
  if (p.tools.length > 0) {
    detail.append(el("h4", null, `工具（${p.tools.length}）`));
    const tbl = el("table", "detail-table");
    tbl.innerHTML = `<thead><tr><th>工具名</th><th>权限</th></tr></thead>`;
    const tbody = document.createElement("tbody");
    for (const t of p.tools) {
      const tr = document.createElement("tr");
      tr.append(el("td", "mono", t));
      const permCell = el("td");
      for (const perm of p.permissions) {
        permCell.append(el("span", `pill perm ${permPermClass(perm)}`, perm));
      }
      tr.append(permCell);
      tbody.append(tr);
    }
    tbl.append(tbody);
    detail.append(tbl);
  }

  // 结果展示声明
  if (p.result_display && p.result_display.length > 0) {
    detail.append(el("h4", null, "结果展示"));
    for (const rd of p.result_display) {
      const row = el("div", "detail-row");
      row.append(
        el("span", "mono", rd.tool),
        el("span", "field-hint", `→ ${rd.display_type || rd.type || "raw"}`),
      );
      detail.append(row);
    }
  }

  // 权限详情
  if (p.permissions.length > 0) {
    detail.append(el("h4", null, "权限声明"));
    for (const perm of p.permissions) {
      const row = el("div", "detail-row");
      const cls = permPermClass(perm);
      const level = cls === "perm-high" ? "高危" : cls === "perm-mid" ? "中危" : "低危";
      row.append(
        el("span", "mono", perm),
        el("span", `badge ${cls || "idle"}`, level),
      );
      detail.append(row);
    }
  }

  card.append(detail);

  // 整个卡片头部可点击展开/折叠
  head.style.cursor = "pointer";
  head.addEventListener("click", (e) => {
    // 按钮点击不触发折叠
    if (e.target.closest("button")) return;
    detail.classList.toggle("hidden");
  });

  return card;
}

/** 缓存完整插件列表，供搜索过滤用。 */
let allPlugins = [];

async function renderPlugins() {
  const list = $("plugins-list");
  const plugins = await call("list_plugins");
  if (!plugins) return;
  allPlugins = plugins;
  applyPluginFilter();
}

/** 按搜索框内容过滤并渲染插件列表。 */
function applyPluginFilter() {
  const list = $("plugins-list");
  const q = ($("plugin-filter")?.value || "").toLowerCase().trim();

  const filtered = q
    ? allPlugins.filter((p) =>
        p.name.toLowerCase().includes(q) ||
        p.id.toLowerCase().includes(q) ||
        p.description.toLowerCase().includes(q) ||
        p.tools.some((t) => t.toLowerCase().includes(q))
      )
    : allPlugins;

  if (filtered.length === 0) {
    showEmpty(list, q ? `没有匹配「${q}」的插件` : "插件目录为空。在设置页确认插件目录后重启。");
    return;
  }

  // 按分类分组：系统 → 测试 → 用户
  const groups = { system: [], test: [], user: [] };
  for (const p of filtered) {
    (groups[p.category] || groups.user).push(p);
  }

  const fragment = document.createDocumentFragment();
  for (const [cat, plugins] of Object.entries(groups)) {
    if (plugins.length === 0) continue;
    const catLabel = CATEGORY_LABELS[cat] || cat;

    const section = el("div", "category-section");
    section.dataset.cat = cat;

    const header = el("div", "category-header");
    header.innerHTML = `<span class="category-arrow">▼</span><span class="badge cat-${cat}">${catLabel}</span><span class="category-count">${plugins.length} 个插件</span>`;
    header.addEventListener("click", () => {
      section.classList.toggle("collapsed");
    });

    const body = el("div", "category-body");
    body.append(...plugins.map(pluginCard));

    section.append(header, body);
    fragment.append(section);
  }
  list.replaceChildren(fragment);
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
  log.scrollTop = log.scrollHeight;
  return msg;
}

/** 显示 AI 打字指示器，返回元素供后续移除。 */
function appendTyping() {
  const log = $("chat-log");
  const msg = el("div", "msg typing");
  const dots = el("div", "typing-dots");
  dots.append(el("span"), el("span"), el("span"));
  msg.append(el("div", "msg-head", "AI"), dots);
  log.append(msg);
  log.scrollTop = log.scrollHeight;
  return msg;
}

function stringify(value) {
  return typeof value === "string" ? value : JSON.stringify(value, null, 2);
}

/**
 * 处理插件 UI 请求（第三期扩展）。
 *
 * 根据 ui_type 渲染不同的 UI 组件：
 * - "overlay": 全屏覆盖层（如截图框选）
 * - "dialog": 模态对话框
 * - "form": 表单
 *
 * @param {string} pluginId - 发起请求的插件 ID
 * @param {object} params - UI 请求参数
 * @param {string} params.ui_type - UI 类型
 * @param {object} params.schema - 声明式 UI schema
 * @param {string} params.callback_method - 用户操作完成后回调的方法名
 */
function handlePluginUiRequest(pluginId, params) {
  const { ui_type, schema, callback_method } = params;

  console.log(`[UI Request] 插件 ${pluginId} 请求 UI:`, ui_type, schema);

  // 根据 UI 类型分发处理
  switch (ui_type) {
    case "overlay":
      // 全屏覆盖层（如截图框选）
      handleOverlayRequest(pluginId, schema, callback_method);
      break;
    case "dialog":
      // 模态对话框
      handleDialogRequest(pluginId, schema, callback_method);
      break;
    case "form":
      // 表单
      handleFormRequest(pluginId, schema, callback_method);
      break;
    default:
      console.warn(`[UI Request] 未知的 UI 类型: ${ui_type}`);
      appendMsg("notify", `${pluginId} · UI 请求`, `未知的 UI 类型: ${ui_type}`);
  }
}

/**
 * 处理全屏覆盖层请求（如截图框选）。
 *
 * @param {string} pluginId - 插件 ID
 * @param {object} schema - UI schema
 * @param {string} callbackMethod - 回调方法名
 */
function handleOverlayRequest(pluginId, schema, callbackMethod) {
  // 创建全屏覆盖层
  const overlay = document.createElement("div");
  overlay.id = "plugin-overlay";
  overlay.style.cssText = `
    position: fixed;
    top: 0;
    left: 0;
    width: 100vw;
    height: 100vh;
    background: rgba(0, 0, 0, 0.3);
    z-index: 10000;
    cursor: crosshair;
    display: flex;
    align-items: center;
    justify-content: center;
  `;

  // 添加提示文本
  const hint = document.createElement("div");
  hint.style.cssText = `
    color: white;
    font-size: 18px;
    text-align: center;
    pointer-events: none;
  `;
  hint.textContent = schema.hint || "拖拽选择区域，按 ESC 取消";
  overlay.appendChild(hint);

  // 添加到 DOM
  document.body.appendChild(overlay);

  // 框选逻辑
  let startX, startY, selectionBox;
  let isSelecting = false;

  overlay.addEventListener("mousedown", (e) => {
    isSelecting = true;
    startX = e.clientX;
    startY = e.clientY;

    // 创建选区框
    selectionBox = document.createElement("div");
    selectionBox.style.cssText = `
      position: fixed;
      border: 2px dashed #00ff00;
      background: rgba(0, 255, 0, 0.1);
      pointer-events: none;
    `;
    overlay.appendChild(selectionBox);
  });

  overlay.addEventListener("mousemove", (e) => {
    if (!isSelecting || !selectionBox) return;

    const x = Math.min(startX, e.clientX);
    const y = Math.min(startY, e.clientY);
    const width = Math.abs(e.clientX - startX);
    const height = Math.abs(e.clientY - startY);

    selectionBox.style.left = x + "px";
    selectionBox.style.top = y + "px";
    selectionBox.style.width = width + "px";
    selectionBox.style.height = height + "px";
  });

  overlay.addEventListener("mouseup", (e) => {
    if (!isSelecting) return;
    isSelecting = false;

    const x = Math.min(startX, e.clientX);
    const y = Math.min(startY, e.clientY);
    const width = Math.abs(e.clientX - startX);
    const height = Math.abs(e.clientY - startY);

    // 移除覆盖层
    overlay.remove();

    // 回调插件
    if (callbackMethod && width > 5 && height > 5) {
      // 通过 Tauri 命令回调插件
      invoke("call_plugin_callback", {
        pluginId: pluginId,
        method: callbackMethod,
        args: {
          x: x,
          y: y,
          width: width,
          height: height,
        },
      }).catch(console.error);
    }
  });

  // ESC 键取消
  const handleEsc = (e) => {
    if (e.key === "Escape") {
      overlay.remove();
      document.removeEventListener("keydown", handleEsc);
    }
  };
  document.addEventListener("keydown", handleEsc);
}

/**
 * 处理模态对话框请求。
 *
 * @param {string} pluginId - 插件 ID
 * @param {object} schema - UI schema
 * @param {string} callbackMethod - 回调方法名
 */
function handleDialogRequest(pluginId, schema, callbackMethod) {
  // 创建对话框容器
  const dialog = document.createElement("div");
  dialog.style.cssText = `
    position: fixed;
    top: 0;
    left: 0;
    width: 100vw;
    height: 100vh;
    background: rgba(0, 0, 0, 0.5);
    z-index: 10000;
    display: flex;
    align-items: center;
    justify-content: center;
  `;

  // 对话框内容
  const content = document.createElement("div");
  content.style.cssText = `
    background: white;
    border-radius: 8px;
    padding: 24px;
    max-width: 500px;
    max-height: 80vh;
    overflow-y: auto;
    box-shadow: 0 4px 20px rgba(0, 0, 0, 0.3);
  `;

  // 标题
  if (schema.title) {
    const title = document.createElement("h3");
    title.textContent = schema.title;
    title.style.marginTop = "0";
    content.appendChild(title);
  }

  // 内容
  if (schema.message) {
    const message = document.createElement("p");
    message.textContent = schema.message;
    content.appendChild(message);
  }

  // 按钮容器
  const buttons = document.createElement("div");
  buttons.style.cssText = `
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    margin-top: 16px;
  `;

  // 取消按钮
  const cancelBtn = document.createElement("button");
  cancelBtn.textContent = "取消";
  cancelBtn.className = "btn";
  cancelBtn.onclick = () => dialog.remove();
  buttons.appendChild(cancelBtn);

  // 确认按钮
  if (schema.confirmText) {
    const confirmBtn = document.createElement("button");
    confirmBtn.textContent = schema.confirmText;
    confirmBtn.className = "btn primary";
    confirmBtn.onclick = () => {
      dialog.remove();
      if (callbackMethod) {
        invoke("call_plugin_callback", {
          pluginId: pluginId,
          method: callbackMethod,
          args: { confirmed: true },
        }).catch(console.error);
      }
    };
    buttons.appendChild(confirmBtn);
  }

  content.appendChild(buttons);
  dialog.appendChild(content);
  document.body.appendChild(dialog);
}

/**
 * 处理表单请求。
 *
 * @param {string} pluginId - 插件 ID
 * @param {object} schema - UI schema
 * @param {string} callbackMethod - 回调方法名
 */
function handleFormRequest(pluginId, schema, callbackMethod) {
  // 创建表单容器
  const formContainer = document.createElement("div");
  formContainer.style.cssText = `
    position: fixed;
    top: 0;
    left: 0;
    width: 100vw;
    height: 100vh;
    background: rgba(0, 0, 0, 0.5);
    z-index: 10000;
    display: flex;
    align-items: center;
    justify-content: center;
  `;

  // 表单内容
  const form = document.createElement("form");
  form.style.cssText = `
    background: white;
    border-radius: 8px;
    padding: 24px;
    max-width: 500px;
    max-height: 80vh;
    overflow-y: auto;
    box-shadow: 0 4px 20px rgba(0, 0, 0, 0.3);
  `;

  // 标题
  if (schema.title) {
    const title = document.createElement("h3");
    title.textContent = schema.title;
    title.style.marginTop = "0";
    form.appendChild(title);
  }

  // 表单字段
  const formData = {};
  if (schema.fields && Array.isArray(schema.fields)) {
    schema.fields.forEach((field) => {
      const fieldContainer = document.createElement("div");
      fieldContainer.style.marginBottom = "12px";

      // 标签
      const label = document.createElement("label");
      label.textContent = field.label || field.name;
      label.style.display = "block";
      label.style.marginBottom = "4px";
      label.style.fontWeight = "bold";
      fieldContainer.appendChild(label);

      // 输入框
      const input = document.createElement("input");
      input.type = field.type || "text";
      input.name = field.name;
      input.placeholder = field.placeholder || "";
      input.style.cssText = `
        width: 100%;
        padding: 8px;
        border: 1px solid #ccc;
        border-radius: 4px;
        box-sizing: border-box;
      `;
      fieldContainer.appendChild(input);

      form.appendChild(fieldContainer);
    });
  }

  // 按钮容器
  const buttons = document.createElement("div");
  buttons.style.cssText = `
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    margin-top: 16px;
  `;

  // 取消按钮
  const cancelBtn = document.createElement("button");
  cancelBtn.type = "button";
  cancelBtn.textContent = "取消";
  cancelBtn.className = "btn";
  cancelBtn.onclick = () => formContainer.remove();
  buttons.appendChild(cancelBtn);

  // 提交按钮
  const submitBtn = document.createElement("button");
  submitBtn.type = "submit";
  submitBtn.textContent = "提交";
  submitBtn.className = "btn primary";
  buttons.appendChild(submitBtn);

  form.appendChild(buttons);

  // 表单提交处理
  form.onsubmit = (e) => {
    e.preventDefault();
    const formData = new FormData(form);
    const data = {};
    formData.forEach((value, key) => {
      data[key] = value;
    });

    formContainer.remove();

    if (callbackMethod) {
      invoke("call_plugin_callback", {
        pluginId: pluginId,
        method: callbackMethod,
        args: data,
      }).catch(console.error);
    }
  };

  formContainer.appendChild(form);
  document.body.appendChild(formContainer);
}

async function submitChat(event) {
  event.preventDefault();
  const input = $("chat-input");
  const message = input.value.trim();
  if (!message) return;

  // 隐藏欢迎页
  $("chat-welcome")?.remove();

  appendMsg("call", "你", message);
  input.value = "";

  const button = $("chat-form").querySelector("button[type=submit]");
  await withBusy(button, async () => {
    // 显示打字指示器
    const typing = appendTyping();
    try {
      const result = await invoke("call_tool", {
        toolName: "ai:chat",
        args: { message },
      });
      typing.remove();
      appendMsg("result", "AI", result.response || stringify(result));
    } catch (e) {
      typing.remove();
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
let currentSettingsPluginEnabled = true;

async function openPluginSettings(pluginId, pluginName, enabled) {
  currentSettingsPluginId = pluginId;
  currentSettingsPluginEnabled = !!enabled;
  $("ps-title").textContent = `${pluginName} 设置`;

  const data = await call("get_plugin_settings", { pluginId });
  if (!data) return;

  const form = $("ps-form");
  form.replaceChildren();

  // 「启用插件」开关
  const enableLabel = el("label", "field checkbox");
  const enableInput = el("input");
  enableInput.type = "checkbox";
  enableInput.id = "ps-enabled";
  enableInput.checked = currentSettingsPluginEnabled;
  enableLabel.append(enableInput, el("span", null, "启用插件"));
  form.append(enableLabel);

  if (data.fields && data.fields.length > 0) {
    const sep = el("hr");
    sep.style.margin = "12px 0";
    form.append(sep);
  }

  if (!data.fields || data.fields.length === 0) {
    form.append(el("p", "empty", "该插件无可配置参数。"));
  } else {
    // 按 group 分组渲染
    let currentGroup = null;
    for (const f of data.fields) {
      // 分组处理
      const groupKey = f.group || null;
      if (groupKey !== currentGroup) {
        currentGroup = groupKey;
        if (groupKey) {
          const groupTitle = el("h4", "settings-group-title", groupKey);
          form.append(groupTitle);
        }
      }

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
        form.append(label);
      } else if (f.field_type === "select") {
        input = el("select", "input");
        for (const opt of f.options) {
          const o = el("option", null, opt);
          o.value = opt;
          if (opt === current) o.selected = true;
          input.append(o);
        }
        input.dataset.key = f.key;
        input.dataset.type = f.field_type;
        label.append(input);
        form.append(label);
      } else if (f.field_type === "color") {
        input = el("input", "input");
        input.type = "color";
        input.value = current || "#000000";
        input.dataset.key = f.key;
        input.dataset.type = f.field_type;
        label.append(input);
        form.append(label);
      } else if (f.field_type === "password") {
        input = el("input", "input");
        input.type = "password";
        input.value = current ?? "";
        if (f.placeholder) input.placeholder = f.placeholder;
        input.dataset.key = f.key;
        input.dataset.type = f.field_type;
        label.append(input);
        form.append(label);
      } else if (f.field_type === "path") {
        const pathWrap = el("div", "path-input-wrap");
        input = el("input", "input");
        input.type = "text";
        input.value = current ?? "";
        if (f.placeholder) input.placeholder = f.placeholder;
        input.dataset.key = f.key;
        input.dataset.type = f.field_type;
        pathWrap.append(input);
        label.append(pathWrap);
        form.append(label);
      } else if (f.field_type === "number") {
        input = el("input", "input");
        input.type = "number";
        input.value = current ?? "";
        if (f.min !== undefined && f.min !== null) input.min = f.min;
        if (f.max !== undefined && f.max !== null) input.max = f.max;
        if (f.step !== undefined && f.step !== null) input.step = f.step;
        if (f.placeholder) input.placeholder = f.placeholder;
        input.dataset.key = f.key;
        input.dataset.type = f.field_type;
        label.append(input);
        form.append(label);
      } else {
        input = el("input", "input");
        input.type = "text";
        input.value = current ?? "";
        if (f.placeholder) input.placeholder = f.placeholder;
        input.dataset.key = f.key;
        input.dataset.type = f.field_type;
        label.append(input);
        form.append(label);
      }

      // 字段描述
      if (f.description) {
        const desc = el("p", "field-desc", f.description);
        form.append(desc);
      }
    }
  }

  $("plugin-settings-modal").hidden = false;
}

async function savePluginSettingsModal() {
  if (!currentSettingsPluginId) return;
  const form = $("ps-form");
  const values = {};

  for (const input of form.querySelectorAll("input, select")) {
    // 「启用插件」开关不走插件参数保存，单独在下面处理。
    if (input.id === "ps-enabled") continue;
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
    // 启用状态有变化时同步到后端（持久化到 HostConfig.disabled_plugins）。
    const enableInput = $("ps-enabled");
    if (enableInput && enableInput.checked !== currentSettingsPluginEnabled) {
      await call("set_plugin_enabled", {
        pluginId: currentSettingsPluginId,
        enabled: enableInput.checked,
      });
    }
    toast("设置已保存");
    $("plugin-settings-modal").hidden = true;
    await renderPlugins();
  }
}

// ─────────────────── 结果展示渲染 ───────────────────

/**
 * 根据 result_display schema 渲染工具结果。
 * 如果插件声明了 result_display，用结构化 UI 展示；否则降级为 JSON。
 */
function renderToolResult(result, resultDisplaySchema) {
  // 没有 schema 或找到匹配的 schema 时，用 JSON 降级
  if (!resultDisplaySchema || resultDisplaySchema.length === 0) {
    return stringify(result);
  }

  // 尝试匹配（resultDisplaySchema 会在外部传入，这里只做渲染）
  // 实际匹配逻辑在调用处
  return renderResultWithSchema(result, resultDisplaySchema);
}

function renderResultWithSchema(result, schema) {
  if (!schema) return stringify(result);

  const wrap = el("div", "result-display");

  if (schema.display_type === "kv") {
    for (const field of schema.fields) {
      const val = result[field.key];
      if (val === undefined && !field.label) continue;
      const row = el("div", "result-kv-row");
      row.append(el("span", "result-kv-label", field.label));
      row.append(el("span", "result-kv-value", val !== undefined ? String(val) : "—"));
      wrap.append(row);
    }
  } else if (schema.display_type === "table") {
    if (Array.isArray(result)) {
      const table = el("table", "result-table");
      const thead = el("tr");
      for (const col of schema.columns) {
        thead.append(el("th", null, col.label));
      }
      table.append(thead);
      for (const row of result) {
        const tr = el("tr");
        for (const col of schema.columns) {
          tr.append(el("td", null, row[col.key] !== undefined ? String(row[col.key]) : ""));
        }
        table.append(tr);
      }
      wrap.append(table);
    } else {
      wrap.textContent = stringify(result);
    }
  } else if (schema.display_type === "markdown") {
    const md = schema.content_key ? result[schema.content_key] : result;
    wrap.append(renderMarkdown(String(md || "")));
  } else {
    wrap.textContent = stringify(result);
  }

  return wrap;
}

/** 极简 markdown 渲染（支持标题、列表、代码块、粗体）。 */
function renderMarkdown(text) {
  const container = el("div", "result-markdown");
  const lines = text.split("\n");
  let inCode = false;
  let codeBlock = [];

  for (const line of lines) {
    if (line.startsWith("```")) {
      if (inCode) {
        const pre = el("pre", "code-block");
        pre.textContent = codeBlock.join("\n");
        container.append(pre);
        codeBlock = [];
        inCode = false;
      } else {
        inCode = true;
      }
      continue;
    }
    if (inCode) {
      codeBlock.push(line);
      continue;
    }
    if (line.startsWith("# ")) {
      container.append(el("h3", null, line.slice(2)));
    } else if (line.startsWith("## ")) {
      container.append(el("h4", null, line.slice(3)));
    } else if (line.startsWith("- ")) {
      container.append(el("li", null, line.slice(2)));
    } else if (line.trim()) {
      container.append(el("p", null, line));
    }
  }
  if (inCode && codeBlock.length) {
    const pre = el("pre", "code-block");
    pre.textContent = codeBlock.join("\n");
    container.append(pre);
  }
  return container;
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

/**
 * 打开插件开发手册弹窗。共用 [`openPluginDocs`] 的弹窗、Markdown 渲染与
 * 搜索框——渲染路径统一，省一份维护。
 */
async function openDevDoc() {
  const content = await call("get_plugin_dev_doc");
  if (content === null || content === undefined) return;
  if (typeof content === "string" && content.length === 0) {
    toast("开发手册不可用（随安装包分发的 docs/plugin-development.md 未找到）");
    return;
  }
  $("docs-title").textContent = "插件开发手册";
  $("docs-file").textContent = "docs/plugin-development.md";
  const search = $("docs-search");
  const body = $("docs-body");
  body.replaceChildren(renderMarkdown(content));
  body.scrollTop = 0;
  search.value = "";
  $("docs-modal").hidden = false;
  search.focus();
}

/**
 * 「插件开发」视图渲染：只显示章节级摘要，让用户一眼看到文档大致结构。
 * 全文仍需打开弹窗，避免在主视图里塞下整本手册（432 行 + 主题混杂，
 * 会在主视图里占屏过大）。
 */
async function renderDev() {
  const summary = $("dev-summary");
  if (!summary) return;
  const content = await call("get_plugin_dev_doc");
  if (!content) {
    summary.replaceChildren(el("p", "field-hint", "开发手册加载失败。"));
    return;
  }
  // 解析 H1 / H2，作为左侧章节目录的雏形。Markdown 简单到不值得引入解析器，
  // 手写扫描足够。
  const sections = [];
  for (const line of content.split("\n")) {
    if (line.startsWith("# ")) sections.push({ level: 1, title: line.slice(2).trim() });
    else if (line.startsWith("## ")) sections.push({ level: 2, title: line.slice(3).trim() });
  }
  summary.replaceChildren(
    el(
      "p",
      "field-hint",
      `共 ${sections.filter((s) => s.level === 1).length} 章、${sections.filter((s) => s.level === 2).length} 节。点「打开开发手册」查看完整内容。`,
    ),
    ...sections.map((s) =>
      el("div", `dev-section level-${s.level}`, s.title),
    ),
  );
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

async function testAiConnection() {
  const btn = $("ai-test-btn");
  const status = $("ai-config-status");
  await withBusy(btn, async () => {
    status.textContent = "测试中…";
    status.className = "field-hint";
    try {
      const result = await call("test_ai_connection", {
        baseUrl: $("ai-base-url").value.trim(),
        apiKey: $("ai-api-key").value.trim(),
        model: $("ai-model").value.trim(),
      });
      if (result?.ok) {
        status.textContent = `✓ 连接成功 · 模型：${result.model}`;
        status.className = "field-hint ok";
      } else {
        status.textContent = `✗ ${result?.message || "连接失败"}`;
        status.className = "field-hint warn";
      }
    } catch (e) {
      status.textContent = `✗ 测试失败：${e}`;
      status.className = "field-hint warn";
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
    } else if (method === "ui/request" && params) {
      // 处理插件 UI 请求
      handlePluginUiRequest(plugin_id, params);
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
  dev: renderDev,
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

  // 插件搜索：输入时实时过滤
  $("plugin-filter")?.addEventListener("input", applyPluginFilter);
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
  $("ai-test-btn").addEventListener("click", testAiConnection);
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
  $("dev-open").addEventListener("click", () => withBusy($("dev-open"), openDevDoc));
  $("dev-reload").addEventListener("click", () => withBusy($("dev-reload"), renderDev));

  // 对话页建议按钮
  for (const btn of document.querySelectorAll(".chat-suggestion")) {
    btn.addEventListener("click", () => {
      $("chat-input").value = btn.dataset.msg;
      $("chat-form").requestSubmit();
    });
  }

  window.addEventListener("hashchange", navigate);
}

// bind() 注册事件监听器；其中任一 querySelector 返回 null 都会使后续监听器
// 丢失。用 try-catch 包裹，保证 navigate() 始终执行、侧边栏始终可切换。
try {
  bind();
} catch (err) {
  console.error("[InTools] bind() 失败，部分交互可能不可用：", err);
}
subscribe();
navigate();
