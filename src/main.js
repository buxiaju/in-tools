const ROUTES = ["plugins", "chat", "permissions", "settings"];

const PLACEHOLDERS = {
  plugins: ["插件", "插件列表与启停将在 Phase 7 接入。"],
  chat: ["AI 对话", "对话与工具调用可视化将在 Phase 7 接入。"],
  permissions: ["权限", "授权管理与 MCP 暴露配置将在 Phase 7 接入。"],
  settings: ["设置", "插件目录、MCP 开关与日志级别将在 Phase 7 接入。"],
};

function render(route) {
  const [title, hint] = PLACEHOLDERS[route];
  const view = document.getElementById(`view-${route}`);
  if (!view.dataset.rendered) {
    view.innerHTML = `<h2>${title}</h2><p class="hint">${hint}</p>`;
    view.dataset.rendered = "1";
  }
}

function navigate() {
  const route = ROUTES.includes(location.hash.slice(1))
    ? location.hash.slice(1)
    : ROUTES[0];

  for (const r of ROUTES) {
    document.getElementById(`view-${r}`).classList.toggle("active", r === route);
  }
  for (const el of document.querySelectorAll(".nav-item")) {
    el.classList.toggle("active", el.dataset.route === route);
  }
  render(route);
}

window.addEventListener("hashchange", navigate);
navigate();
