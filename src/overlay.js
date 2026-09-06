// 截图框选覆盖层。
// 全屏透明窗口，用户拖拽选择区域，选定后调用 call_tool 携带 region 坐标，然后关闭窗口。
// ESC 取消，点击（未拖拽）则截取全屏。

const invoke = (cmd, args) => {
  if (!window.__TAURI__ || !window.__TAURI__.core || typeof window.__TAURI__.core.invoke !== "function") {
    return Promise.reject(new Error("Tauri 桥未注入"));
  }
  return window.__TAURI__.core.invoke(cmd, args);
};

const params = new URLSearchParams(window.location.search);
const tool = params.get("tool");
const mode = params.get("mode");

const canvas = document.getElementById("canvas");
const ctx = canvas.getContext("2d");

let startX = 0, startY = 0, endX = 0, endY = 0;
let isSelecting = false;

// 关闭覆盖层窗口。三个模式共用：截图框选、取色器、剪贴板历史都可能需要关窗。
async function closeWindow() {
  try {
    await invoke("close_overlay");
  } catch {
    try {
      await invoke("plugin:window|close", { label: "overlay" });
    } catch {
      window.close();
    }
  }
}

// ── 取色器模式 ──────────────────────────────────────────────
// 按下快捷键后进入全屏取色：鼠标移动时实时显示 HEX/RGB/HSL 与坐标，
// 点击锁定颜色（面板保持显示），再次点击或 ESC 关闭。

// ── 剪贴板历史模式 ──────────────────────────────────────────
// 按下快捷键后弹出浮动面板，显示剪贴板历史记录，点击条目即可复制。

if (mode === "clipboard-history") {
  canvas.style.display = "none";
  document.getElementById("hint").style.display = "none";

  const panel = document.getElementById("clipboard-panel");
  const listEl = document.getElementById("clip-list");
  const filterEl = document.getElementById("clip-filter");
  const footerEl = document.getElementById("clip-footer");
  const closeBtn = document.getElementById("clip-close");

  let allEntries = [];

  function timeAgo(ts) {
    const diff = Math.floor(Date.now() / 1000) - ts;
    if (diff < 60) return `${diff} 秒前`;
    if (diff < 3600) return `${Math.floor(diff / 60)} 分钟前`;
    if (diff < 86400) return `${Math.floor(diff / 3600)} 小时前`;
    return `${Math.floor(diff / 86400)} 天前`;
  }

  function renderList(filter) {
    const q = (filter || "").toLowerCase();
    const items = q
      ? allEntries.filter((e) => e.text.toLowerCase().includes(q))
      : allEntries;

    if (items.length === 0) {
      listEl.innerHTML = '<div class="clip-empty">暂无剪贴板记录<br/>复制文本后会自动出现在这里</div>';
      return;
    }

    listEl.innerHTML = "";
    for (let i = 0; i < items.length; i++) {
      const e = items[i];
      const div = document.createElement("div");
      div.className = "clip-item";
      div.dataset.text = e.text;

      const textEl = document.createElement("div");
      textEl.className = "clip-item-text";
      textEl.textContent = e.text;

      const metaEl = document.createElement("div");
      metaEl.className = "clip-item-meta";
      metaEl.textContent = `${e.length} 字符 · ${timeAgo(e.timestamp)}`;

      div.appendChild(textEl);
      div.appendChild(metaEl);

      div.addEventListener("click", async () => {
        try {
          await invoke("copy_clipboard_entry", { text: e.text });
          div.classList.add("copied");
          metaEl.textContent = "已复制 ✓";
          footerEl.textContent = "已复制到剪贴板，可到其他地方粘贴";
          setTimeout(() => closeWindow(), 600);
        } catch (err) {
          metaEl.textContent = `复制失败: ${err}`;
        }
      });

      listEl.appendChild(div);
    }
  }

  async function loadHistory() {
    try {
      allEntries = await invoke("get_clipboard_history");
      renderList(filterEl.value);
    } catch (err) {
      listEl.innerHTML = `<div class="clip-empty">加载失败: ${err}</div>`;
    }
  }

  filterEl.addEventListener("input", () => renderList(filterEl.value));
  closeBtn.addEventListener("click", () => closeWindow());
  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape") closeWindow();
  });

  panel.classList.remove("hidden");
  filterEl.focus();
  loadHistory();

  // 取色器模式到此为止，不执行下面的截图选区逻辑
} else if (mode === "color-picker") {
  canvas.style.display = "none";
  document.getElementById("hint").textContent = "移动鼠标取色 · 点击锁定 · ESC 取消";

  const panel = document.getElementById("picker-panel");
  const swatch = document.getElementById("picker-swatch");
  const hexEl = document.getElementById("picker-hex");
  const rgbEl = document.getElementById("picker-rgb");
  const hslEl = document.getElementById("picker-hsl");
  const posEl = document.getElementById("picker-pos");

  let locked = false;
  let closed = false;
  let lastFetch = 0;
  let currentColor = null;

  function rgbToHsl(r, g, b) {
    r /= 255; g /= 255; b /= 255;
    const max = Math.max(r, g, b), min = Math.min(r, g, b);
    let h, s, l = (max + min) / 2;
    if (max === min) { h = s = 0; }
    else {
      const d = max - min;
      s = l > 0.5 ? d / (2 - max - min) : d / (max + min);
      switch (max) {
        case r: h = (g - b) / d + (g < b ? 6 : 0); break;
        case g: h = (b - r) / d + 2; break;
        case b: h = (r - g) / d + 4; break;
      }
      h /= 6;
    }
    return [Math.round(h * 360), Math.round(s * 100), Math.round(l * 100)];
  }

  function updatePanel(r, g, b, hex, px, py) {
    const [h, s, l] = rgbToHsl(r, g, b);
    swatch.style.background = hex;
    hexEl.textContent = hex;
    rgbEl.textContent = `${r}, ${g}, ${b}`;
    hslEl.textContent = `${h}°, ${s}%, ${l}%`;
    posEl.textContent = `${px}, ${py}`;
  }

  function positionPanel(mx, my) {
    const pw = panel.offsetWidth;
    const ph = panel.offsetHeight;
    const margin = 16;
    let x = mx + margin;
    let y = my + margin;
    if (x + pw > window.innerWidth) x = mx - pw - margin;
    if (y + ph > window.innerHeight) y = my - ph - margin;
    panel.style.left = `${x}px`;
    panel.style.top = `${y}px`;
  }

  async function fetchColor(px, py) {
    try {
      const c = await invoke("get_pixel_color", { x: px, y: py });
      currentColor = c;
      updatePanel(c.r, c.g, c.b, c.hex, px, py);
    } catch (e) {
      // 取色失败时静默，不打断用户操作
    }
  }

  // 用命名函数 + removeEventListener，否则锁定后 mousemove 还在 positionPanel
  // 重排 panel，每次动鼠标都触发 layout——叠加 IPC 期间在「锁定态」做毫无意义
  // 的工作，正是用户感知到的「卡顿」。
  function onMouseMove(e) {
    if (locked) return;
    const px = Math.round(e.clientX * window.devicePixelRatio);
    const py = Math.round(e.clientY * window.devicePixelRatio);
    positionPanel(e.clientX, e.clientY);
    // 节流：30fps 足够追眼，再高只会让 IPC 与合成器互相等。
    const now = performance.now();
    if (now - lastFetch > 33) {
      lastFetch = now;
      fetchColor(px, py);
    }
  }
  document.addEventListener("mousemove", onMouseMove);

  async function closePicker() {
    if (closed) return;
    closed = true;
    // 覆盖层还在最上层时用户的鼠标已经被它吸走，整个系统看起来像「卡住」。
    // 「卡住」的真正含义不是 IPC 卡了——是覆盖层没走、用户误以为系统无响应。
    // 这里无条件关窗：摘掉鼠标监听后再关，避免 close 过程中再次触发布局。
    document.removeEventListener("mousemove", onMouseMove);
    document.removeEventListener("click", onClick);
    document.removeEventListener("keydown", onKeyDown);
    try {
      const win = window.__TAURI__.window.getCurrentWindow();
      await win.close();
    } catch {
      window.close();
    }
  }
  // 把 closePicker 暴露给 ESC 用——`addEventListener` 里不能直接调匿名箭头
  // 后再 remove；统一走命名函数。
  function onKeyDown(e) {
    if (e.key === "Escape") closePicker();
  }
  document.addEventListener("keydown", onKeyDown);

  async function onClick(e) {
    if (locked) {
      // 第二次点击：再确认一次（已锁定的色直接关闭即可）。
      await closePicker();
      return;
    }
    // 第一次点击：锁定颜色。
    locked = true;
    panel.classList.add("locked");
    document.getElementById("hint").textContent = "已锁定 · HEX 已复制 · ESC 关闭";
    const px = Math.round(e.clientX * window.devicePixelRatio);
    const py = Math.round(e.clientY * window.devicePixelRatio);
    // 取色 + 复制剪贴板全部走完，再短暂展示「锁定」反馈，然后关窗。
    await fetchColor(px, py);
    if (tool && currentColor) {
      try {
        await invoke("call_tool", {
          tool,
          args: {
            hex: currentColor.hex,
            r: currentColor.r,
            g: currentColor.g,
            b: currentColor.b,
            x: px,
            y: py,
          },
        });
      } catch (err) {
        // 工具调用失败不影响已锁定的颜色展示
      }
    }
    // 600ms 是「已锁定」反馈的最短可读时间。再短用户来不及看清面板上的 HEX，
    // 长得超过 1s 又会让用户怀疑「没关掉」。
    setTimeout(closePicker, 600);
  }
  document.addEventListener("click", onClick);

  panel.classList.remove("hidden");
  // 取色器模式到此为止，不执行下面的截图选区逻辑
} else {

function resize() {
  canvas.width = window.innerWidth;
  canvas.height = window.innerHeight;
  draw();
}

function draw() {
  // 全屏半透明遮罩
  ctx.fillStyle = "rgba(0, 0, 0, 0.3)";
  ctx.fillRect(0, 0, canvas.width, canvas.height);

  if (!isSelecting) return;

  const x = Math.min(startX, endX);
  const y = Math.min(startY, endY);
  const w = Math.abs(endX - startX);
  const h = Math.abs(endY - startY);

  // 清除选区遮罩，露出实际屏幕内容
  ctx.clearRect(x, y, w, h);

  // 选区边框
  ctx.strokeStyle = "#3b82f6";
  ctx.lineWidth = 2;
  ctx.strokeRect(x, y, w, h);

  // 尺寸标签
  if (w > 2 && h > 2) {
    const label = `${Math.round(w)} × ${Math.round(h)}`;
    ctx.font = "13px sans-serif";
    const tw = ctx.measureText(label).width + 12;
    ctx.fillStyle = "rgba(0, 0, 0, 0.75)";
    ctx.fillRect(x, y - 24, tw, 22);
    ctx.fillStyle = "#fff";
    ctx.fillText(label, x + 6, y - 8);
  }
}

canvas.addEventListener("mousedown", (e) => {
  isSelecting = true;
  startX = endX = e.clientX;
  startY = endY = e.clientY;
});

canvas.addEventListener("mousemove", (e) => {
  if (!isSelecting) return;
  endX = e.clientX;
  endY = e.clientY;
  draw();
});

canvas.addEventListener("mouseup", async () => {
  if (!isSelecting) return;
  isSelecting = false;

  const x = Math.min(startX, endX);
  const y = Math.min(startY, endY);
  const w = Math.abs(endX - startX);
  const h = Math.abs(endY - startY);

  if (w < 5 || h < 5) {
    // 拖拽距离太小，视为点击 → 截取全屏
    await capture(null);
  } else {
    // 坐标要从 CSS 逻辑像素换算成物理像素：Python 侧调了 SetProcessDPIAware，
    // GetSystemMetrics/BitBlt 用的都是物理像素，而 clientX/clientY 是逻辑像素。
    // 不换算的话，在 150% 缩放的屏幕上选区会整体偏移并缩小三分之一。
    const dpr = window.devicePixelRatio || 1;
    await capture({
      x: Math.round(x * dpr),
      y: Math.round(y * dpr),
      width: Math.round(w * dpr),
      height: Math.round(h * dpr),
    });
  }
});

document.addEventListener("keydown", (e) => {
  if (e.key === "Escape") {
    closeWindow();
  }
});

async function capture(region) {
  // 关键：必须先把覆盖层藏掉再截图。BitBlt 抓的是屏幕 DC 的实时内容，
  // 覆盖层还在最上层的话，半透明遮罩和选区边框会一起被拍进照片里。
  // hide() 返回后合成器未必已经把画面刷掉，所以再等两帧保底。
  await hideOverlay();

  const args = region ? { region } : {};
  try {
    await invoke("call_tool", { toolName: tool, args });
  } catch (err) {
    console.error("截图失败:", err);
  } finally {
    closeWindow();
  }
}

async function hideOverlay() {
  try {
    await window.__TAURI__.window.getCurrentWindow().hide();
  } catch (err) {
    console.error("隐藏覆盖层失败:", err);
  }
  await new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve)));
  await new Promise((resolve) => setTimeout(resolve, 60));
}

window.addEventListener("resize", resize);
resize();
}
