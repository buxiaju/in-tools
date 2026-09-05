// 截图框选覆盖层。
// 全屏透明窗口，用户拖拽选择区域，选定后调用 call_tool 携带 region 坐标，然后关闭窗口。
// ESC 取消，点击（未拖拽）则截取全屏。

const { invoke } = window.__TAURI__.core;

const params = new URLSearchParams(window.location.search);
const tool = params.get("tool");

const canvas = document.getElementById("canvas");
const ctx = canvas.getContext("2d");

let startX = 0, startY = 0, endX = 0, endY = 0;
let isSelecting = false;

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

async function closeWindow() {
  try {
    const win = window.__TAURI__.window.getCurrentWindow();
    await win.close();
  } catch {
    // 如果 window API 不可用，尝试用 invoke 关闭
    try {
      await invoke("plugin:window|close", { label: "overlay" });
    } catch {
      // 最后手段：退回主窗口
      window.close();
    }
  }
}

window.addEventListener("resize", resize);
resize();
