#!/usr/bin/env node

/**
 * 构建插件市场索引
 *
 * 扫描 plugins/ 目录下的所有插件，解析 manifest.toml，生成：
 *   - marketplace/plugins.json   插件列表（含 metadata）
 *   - marketplace/versions.json  版本历史
 *   - marketplace/search.json    搜索索引
 *   - marketplace/index.html     市场网页（静态单页）
 *
 * 去重规则：同一 plugin.id 只保留 system > test > direct 中优先级最高的条目。
 * test 分类插件默认不出现在市场页面中（可通过 SHOW_TEST_PLUGINS 打开）。
 */

const fs = require('fs');
const path = require('path');
const toml = require('toml');

// ─── 配置 ────────────────────────────────────────────────────
const PLUGINS_DIR = path.join(__dirname, '..', 'plugins');
const OUTPUT_DIR = path.join(__dirname, '..', 'marketplace');
const GITHUB_REPO = 'buxiaju/in-tools';
const SHOW_TEST_PLUGINS = process.env.SHOW_TEST_PLUGINS === '1';

// 分类优先级（数字越小越优先；同 id 只保留最高优先级的条目）
const CATEGORY_PRIORITY = { system: 0, test: 1, user: 2, direct: 99 };

// ─── 工具函数 ────────────────────────────────────────────────

/** 去除 UTF-8 BOM */
function stripBom(s) {
  return s.charCodeAt(0) === 0xFEFF ? s.slice(1) : s;
}

/** 安全读取文件 */
function safeRead(p, fallback = '') {
  try { return fs.readFileSync(p, 'utf-8'); } catch { return fallback; }
}

/** 从 manifest 提取标签（去重） */
function extractTags(manifest) {
  const tags = new Set();
  if (manifest.tools) {
    for (const tool of manifest.tools) {
      const prefix = tool.name.split(':')[0];
      tags.add(prefix);
    }
  }
  if (manifest.capabilities?.permissions) {
    for (const perm of manifest.capabilities.permissions) {
      tags.add(perm.split(':')[0]);
    }
  }
  return [...tags];
}

/** 从语言推断标签 */
function inferLanguage(manifest) {
  const cmd = manifest.exec?.command || '';
  if (cmd === 'python' || cmd === 'python3' || cmd === 'py') return 'Python';
  if (cmd === 'node' || cmd === 'nodejs' || cmd === 'npx') return 'Node.js';
  if (cmd === 'go') return 'Go';
  if (cmd === 'cargo' || cmd === 'rustc') return 'Rust';
  // 看文件
  const dir = path.dirname(manifest.__filePath || '');
  if (fs.existsSync(path.join(dir, 'main.py'))) return 'Python';
  if (fs.existsSync(path.join(dir, 'main.js')) || fs.existsSync(path.join(dir, 'main.go'))) {
    if (fs.existsSync(path.join(dir, 'main.go'))) return 'Go';
    return 'Node.js';
  }
  if (fs.existsSync(path.join(dir, 'main.rs'))) return 'Rust';
  return '未知';
}

/** 计算目录下所有文件的总大小 */
function calcDirSize(dirPath) {
  let total = 0;
  try {
    const entries = fs.readdirSync(dirPath, { withFileTypes: true });
    for (const entry of entries) {
      const fp = path.join(dirPath, entry.name);
      if (entry.isFile()) {
        total += fs.statSync(fp).size;
      } else if (entry.isDirectory() && entry.name !== '__pycache__' && entry.name !== 'node_modules') {
        total += calcDirSize(fp);
      }
    }
  } catch { /* ignore */ }
  return total;
}

// ─── 扫描插件 ────────────────────────────────────────────────

function scanPlugins() {
  const candidates = []; // { priority, plugin }
  const seenDirs = new Set();

  // 1. 扫描分类目录 (system / test / user)
  const subDirs = ['system', 'test', 'user'];
  for (const dir of subDirs) {
    const dirPath = path.join(PLUGINS_DIR, dir);
    if (!fs.existsSync(dirPath)) continue;
    for (const entry of fs.readdirSync(dirPath, { withFileTypes: true })) {
      if (!entry.isDirectory()) continue;
      seenDirs.add(entry.name);
      parsePlugin(path.join(dirPath, entry.name), dir, candidates);
    }
  }

  // 2. 扫描 plugins/ 根目录下的直接插件（兼容旧结构，跳过已扫子目录和 SDK）
  const skipDirs = new Set([...subDirs, 'python-sdk', 'node-sdk', 'tools', 'test']);
  for (const entry of fs.readdirSync(PLUGINS_DIR, { withFileTypes: true })) {
    if (!entry.isDirectory() || skipDirs.has(entry.name)) continue;
    if (seenDirs.has(entry.name)) continue;
    parsePlugin(path.join(PLUGINS_DIR, entry.name), 'direct', candidates);
  }

  // 3. 按 plugin.id 去重，保留优先级最高的
  const bestById = new Map();
  for (const c of candidates) {
    const existing = bestById.get(c.plugin.id);
    if (!existing || c.priority < existing.priority) {
      bestById.set(c.plugin.id, c);
    }
  }

  // 4. 可选排除 test 插件
  const plugins = [];
  for (const [, c] of bestById) {
    if (!SHOW_TEST_PLUGINS && c.category === 'test') continue;
    plugins.push(c.plugin);
  }

  // 按名称排序
  plugins.sort((a, b) => a.name.localeCompare(b.name, 'zh'));
  return plugins;
}

/** 解析单个插件目录，推入 candidates 数组 */
function parsePlugin(pluginPath, category, candidates) {
  const manifestPath = path.join(pluginPath, 'manifest.toml');
  if (!fs.existsSync(manifestPath)) return;

  try {
    let content = stripBom(safeRead(manifestPath));
    const manifest = toml.parse(content);
    manifest.__filePath = manifestPath;

    if (!manifest.plugin?.id || !manifest.plugin?.name) return;

    const readme = stripBom(safeRead(path.join(pluginPath, 'README.md'))).substring(0, 800);
    const lang = inferLanguage(manifest);

    candidates.push({
      priority: CATEGORY_PRIORITY[category] ?? 99,
      category,
      plugin: {
        id: manifest.plugin.id,
        name: manifest.plugin.name,
        version: manifest.plugin.version || '0.0.1',
        description: manifest.plugin.description || '',
        author: manifest.plugin.author || '社区贡献',
        language: lang,
        category,
        tags: extractTags(manifest),
        homepage: `https://github.com/${GITHUB_REPO}/tree/main/plugins/${category === 'direct' ? '' : category + '/'}${path.basename(pluginPath)}`,
        repository: `https://github.com/${GITHUB_REPO}`,
        license: manifest.license || 'MIT',
        created_at: fs.statSync(pluginPath).birthtime.toISOString(),
        updated_at: fs.statSync(manifestPath).mtime.toISOString(),
        tools: (manifest.tools || []).map(t => ({ name: t.name, description: t.description || '' })),
        capabilities: manifest.capabilities || {},
        lifecycle: manifest.lifecycle || {},
        readme,
        file_size: calcDirSize(pluginPath),
        download_url: `https://github.com/${GITHUB_REPO}/raw/main/plugins/${category === 'direct' ? '' : category + '/'}${path.basename(pluginPath)}.zip`,
      },
    });
  } catch (err) {
    console.error(`  ✗ ${manifestPath}: ${err.message}`);
  }
}

// ─── 生成 JSON 索引 ──────────────────────────────────────────

function generatePluginIndex(plugins) {
  return {
    version: '1.0.0',
    generated_at: new Date().toISOString(),
    total: plugins.length,
    plugins: plugins.map(p => ({
      id: p.id,
      name: p.name,
      version: p.version,
      description: p.description,
      author: p.author,
      language: p.language,
      category: p.category,
      tags: p.tags,
      homepage: p.homepage,
      license: p.license,
      created_at: p.created_at,
      updated_at: p.updated_at,
      file_size: p.file_size,
      tools_count: p.tools.length,
      download_count: 0,
      rating: 0,
      rating_count: 0,
    })),
  };
}

function generateVersionInfo(plugins) {
  const versions = {};
  for (const p of plugins) {
    versions[p.id] = {
      plugin_id: p.id,
      versions: [
        {
          version: p.version,
          released_at: p.updated_at,
          description: p.description,
          changelog: '',
          download_url: p.download_url,
          file_size: p.file_size,
          file_hash: '',
          dependencies: {},
          min_host_version: null,
        },
      ],
    };
  }
  return { version: '1.0.0', generated_at: new Date().toISOString(), plugins: versions };
}

function generateSearchIndex(plugins) {
  return {
    version: '1.0.0',
    generated_at: new Date().toISOString(),
    plugins: plugins.map(p => ({
      id: p.id,
      name: p.name,
      description: p.description,
      author: p.author,
      language: p.language,
      category: p.category,
      tags: p.tags,
      tools: p.tools.map(t => t.name),
    })),
  };
}

// ─── 生成 HTML ───────────────────────────────────────────────

function generateIndexHtml(plugins) {
  // 统计数据
  const systemCount = plugins.filter(p => p.category === 'system').length;
  const languageSet = new Set(plugins.map(p => p.language));
  const tagSet = new Set(plugins.flatMap(p => p.tags));

  // 预生成卡片 HTML（含 data 属性供 JS 筛选）
  const cardsHtml = plugins.map(p => {
    const langColors = { Python: '#3572A5', 'Node.js': '#339933', Go: '#00ADD8', Rust: '#dea584' };
    const langColor = langColors[p.language] || '#888';
    const catLabel = { system: '系统插件', test: '测试插件', user: '用户插件', direct: '社区插件' };
    const catClass = { system: 'cat-system', test: 'cat-test', user: 'cat-user', direct: 'cat-direct' };
    return `
        <div class="plugin-card ${catClass[p.category] || ''}"
             data-id="${p.id}"
             data-name="${p.name}"
             data-description="${p.description}"
             data-category="${p.category}"
             data-tags="${p.tags.join(',')}"
             data-language="${p.language}"
             data-tools="${p.tools.map(t => t.name).join(',')}"
             onclick="showDetail(this)">
          <div class="card-header">
            <div class="card-icon">${p.name.charAt(0)}</div>
            <div class="card-title">
              <h3>${p.name}</h3>
              <span class="card-author">${p.author}</span>
            </div>
            <span class="lang-badge" style="background:${langColor}">${p.language}</span>
          </div>
          <p class="card-desc">${p.description || '暂无描述'}</p>
          <div class="card-tags">
            ${p.tags.map(t => `<span class="tag">${t}</span>`).join('')}
          </div>
          <div class="card-footer">
            <span class="card-version">v${p.version}</span>
            <span class="card-size">${formatSize(p.file_size)}</span>
            <span class="card-tools">${p.tools.length} 个工具</span>
          </div>
        </div>`;
  }).join('\n');

  // 提取所有标签供筛选
  const allTags = [...tagSet].sort();
  const allLanguages = [...languageSet].sort();

  return `<!DOCTYPE html>
<html lang="zh-CN">
<head>
  <meta charset="UTF-8">
  <meta name="viewport" content="width=device-width, initial-scale=1.0">
  <title>InTools 插件市场</title>
  <style>
    :root {
      --primary: #6366f1;
      --primary-light: #818cf8;
      --primary-dark: #4f46e5;
      --bg: #f8fafc;
      --card-bg: #ffffff;
      --text: #1e293b;
      --text-secondary: #64748b;
      --border: #e2e8f0;
      --radius: 12px;
      --shadow: 0 1px 3px rgba(0,0,0,.08), 0 1px 2px rgba(0,0,0,.06);
      --shadow-lg: 0 10px 25px rgba(0,0,0,.1);
    }

    * { margin: 0; padding: 0; box-sizing: border-box; }
    body {
      font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, 'Helvetica Neue', Arial, 'PingFang SC', 'Microsoft YaHei', sans-serif;
      line-height: 1.6;
      color: var(--text);
      background: var(--bg);
    }

    /* ── Header ── */
    .hero {
      background: linear-gradient(135deg, #4f46e5 0%, #7c3aed 50%, #a855f7 100%);
      color: #fff;
      padding: 48px 24px 40px;
      text-align: center;
    }
    .hero h1 { font-size: 2.2em; font-weight: 700; margin-bottom: 8px; letter-spacing: -0.5px; }
    .hero p { font-size: 1.1em; opacity: .88; margin-bottom: 24px; }
    .stats-row {
      display: flex; justify-content: center; gap: 32px; flex-wrap: wrap;
      margin-top: 8px;
    }
    .stat { text-align: center; }
    .stat-num { font-size: 1.6em; font-weight: 700; }
    .stat-label { font-size: .85em; opacity: .75; }

    /* ── Search & Filters ── */
    .toolbar {
      max-width: 1200px; margin: -24px auto 0; padding: 0 20px; position: relative; z-index: 10;
    }
    .search-card {
      background: var(--card-bg);
      border-radius: var(--radius);
      box-shadow: var(--shadow-lg);
      padding: 20px 24px;
    }
    .search-row {
      display: flex; gap: 12px; align-items: center;
    }
    .search-row input {
      flex: 1; padding: 12px 16px; font-size: 15px;
      border: 2px solid var(--border); border-radius: 8px; outline: none;
      transition: border-color .2s;
    }
    .search-row input:focus { border-color: var(--primary); }

    .filter-bar {
      display: flex; flex-wrap: wrap; gap: 8px; margin-top: 14px; align-items: center;
    }
    .filter-bar label { font-size: .85em; color: var(--text-secondary); margin-right: 4px; }
    .filter-btn {
      padding: 4px 12px; border-radius: 16px; border: 1px solid var(--border);
      background: transparent; color: var(--text-secondary); font-size: .82em;
      cursor: pointer; transition: all .15s;
    }
    .filter-btn:hover { border-color: var(--primary); color: var(--primary); }
    .filter-btn.active { background: var(--primary); color: #fff; border-color: var(--primary); }

    .filter-divider {
      width: 1px; height: 20px; background: var(--border); margin: 0 4px;
    }

    .result-count {
      margin-top: 16px; font-size: .9em; color: var(--text-secondary);
    }

    /* ── Grid ── */
    .grid {
      max-width: 1200px; margin: 24px auto 0; padding: 0 20px;
      display: grid; grid-template-columns: repeat(auto-fill, minmax(320px, 1fr)); gap: 18px;
    }
    .plugin-card {
      background: var(--card-bg); border-radius: var(--radius); padding: 20px;
      box-shadow: var(--shadow); border: 1px solid var(--border);
      cursor: pointer; transition: transform .18s, box-shadow .18s;
      position: relative; overflow: hidden;
    }
    .plugin-card:hover {
      transform: translateY(-3px); box-shadow: var(--shadow-lg);
    }
    .plugin-card.hidden { display: none; }

    .card-header { display: flex; align-items: center; gap: 12px; margin-bottom: 12px; }
    .card-icon {
      width: 42px; height: 42px; border-radius: 10px;
      background: linear-gradient(135deg, var(--primary), var(--primary-light));
      color: #fff; display: flex; align-items: center; justify-content: center;
      font-size: 1.2em; font-weight: 700; flex-shrink: 0;
    }
    .card-title { flex: 1; min-width: 0; }
    .card-title h3 { font-size: 1.05em; font-weight: 600; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
    .card-author { font-size: .8em; color: var(--text-secondary); }
    .lang-badge {
      padding: 2px 8px; border-radius: 10px; color: #fff;
      font-size: .72em; font-weight: 600; white-space: nowrap; flex-shrink: 0;
    }
    .card-desc {
      font-size: .9em; color: var(--text-secondary); margin-bottom: 12px;
      display: -webkit-box; -webkit-line-clamp: 2; -webkit-box-orient: vertical; overflow: hidden;
      min-height: 44px;
    }
    .card-tags { margin-bottom: 12px; }
    .tag {
      display: inline-block; background: #f1f5f9; color: var(--text-secondary);
      padding: 2px 8px; border-radius: 6px; font-size: .78em; margin: 0 4px 4px 0;
    }
    .card-footer {
      display: flex; gap: 12px; font-size: .8em; color: var(--text-secondary);
      border-top: 1px solid var(--border); padding-top: 10px;
    }

    /* ── Category strip ── */
    .plugin-card[data-category="system"]::before {
      content: ''; position: absolute; top: 0; left: 0;
      width: 3px; height: 100%; background: var(--primary);
    }
    .plugin-card[data-category="user"]::before {
      content: ''; position: absolute; top: 0; left: 0;
      width: 3px; height: 100%; background: #10b981;
    }

    /* ── Modal ── */
    .modal-overlay {
      display: none; position: fixed; inset: 0;
      background: rgba(0,0,0,.45); z-index: 1000;
      justify-content: center; align-items: center; padding: 20px;
    }
    .modal-overlay.open { display: flex; }
    .modal {
      background: var(--card-bg); border-radius: var(--radius);
      width: 100%; max-width: 640px; max-height: 85vh; overflow-y: auto;
      box-shadow: var(--shadow-lg); padding: 32px;
    }
    .modal-close {
      float: right; background: none; border: none; font-size: 1.5em;
      cursor: pointer; color: var(--text-secondary); line-height: 1;
    }
    .modal h2 { font-size: 1.3em; margin-bottom: 8px; padding-right: 32px; }
    .modal .modal-meta {
      display: flex; flex-wrap: wrap; gap: 12px; margin-bottom: 16px;
      font-size: .88em; color: var(--text-secondary);
    }
    .modal .modal-desc { margin-bottom: 16px; line-height: 1.7; }
    .modal .modal-section { margin-bottom: 16px; }
    .modal .modal-section h4 { font-size: .92em; margin-bottom: 8px; color: var(--primary); }
    .modal .tool-list {
      list-style: none; padding: 0;
    }
    .modal .tool-list li {
      padding: 8px 12px; background: #f8fafc; border-radius: 8px;
      margin-bottom: 6px; font-size: .88em;
      display: flex; flex-direction: column; gap: 2px;
    }
    .modal .tool-name { font-weight: 600; font-family: 'Fira Code', monospace; color: var(--primary-dark); }
    .modal .tool-desc { color: var(--text-secondary); font-size: .92em; }
    .modal .modal-actions { display: flex; gap: 10px; margin-top: 20px; }
    .modal .btn {
      padding: 10px 20px; border-radius: 8px; font-size: .92em;
      font-weight: 600; cursor: pointer; border: none; transition: all .15s;
    }
    .modal .btn-primary { background: var(--primary); color: #fff; }
    .modal .btn-primary:hover { background: var(--primary-dark); }
    .modal .btn-outline { background: transparent; border: 1px solid var(--border); color: var(--text); }
    .modal .btn-outline:hover { border-color: var(--primary); color: var(--primary); }

    .empty-state {
      grid-column: 1 / -1; text-align: center; padding: 60px 20px;
      color: var(--text-secondary);
    }
    .empty-state .empty-icon { font-size: 3em; margin-bottom: 12px; opacity: .4; }

    /* ── Footer ── */
    footer {
      text-align: center; padding: 32px 20px; color: var(--text-secondary);
      font-size: .88em; border-top: 1px solid var(--border); margin-top: 48px;
    }
    footer a { color: var(--primary); text-decoration: none; }
    footer a:hover { text-decoration: underline; }

    /* ── Responsive ── */
    @media (max-width: 640px) {
      .hero h1 { font-size: 1.6em; }
      .stats-row { gap: 20px; }
      .grid { grid-template-columns: 1fr; }
      .modal { padding: 20px; }
    }
  </style>
</head>
<body>
  <div class="hero">
    <h1>🧩 InTools 插件市场</h1>
    <p>一切皆插件 — 发现、安装、扩展你的桌面工具能力</p>
    <div class="stats-row">
      <div class="stat"><div class="stat-num">${plugins.length}</div><div class="stat-label">个插件</div></div>
      <div class="stat"><div class="stat-num">${systemCount}</div><div class="stat-label">系统内置</div></div>
      <div class="stat"><div class="stat-num">${plugins.reduce((s, p) => s + p.tools.length, 0)}</div><div class="stat-label">个工具</div></div>
      <div class="stat"><div class="stat-num">${languageSet.size}</div><div class="stat-label">种语言</div></div>
    </div>
  </div>

  <div class="toolbar">
    <div class="search-card">
      <div class="search-row">
        <input type="text" id="searchInput" placeholder="搜索插件名称、描述、标签或工具名..." autofocus>
      </div>
      <div class="filter-bar">
        <label>分类：</label>
        <button class="filter-btn active" data-filter-type="category" data-filter-value="all">全部</button>
        <button class="filter-btn" data-filter-type="category" data-filter-value="system">系统插件</button>
        <button class="filter-btn" data-filter-type="category" data-filter-value="user">用户插件</button>
        <div class="filter-divider"></div>
        <label>语言：</label>
        <button class="filter-btn active" data-filter-type="language" data-filter-value="all">全部</button>
        ${allLanguages.filter(l => l !== '未知').map(l => `<button class="filter-btn" data-filter-type="language" data-filter-value="${l}">${l}</button>`).join('')}
      </div>
      <div class="filter-bar" style="margin-top:8px;">
        <label>标签：</label>
        ${allTags.slice(0, 15).map(t => `<button class="filter-btn" data-filter-type="tag" data-filter-value="${t}">${t}</button>`).join('')}
      </div>
      <div class="result-count" id="resultCount">显示 ${plugins.length} 个插件</div>
    </div>
  </div>

  <div class="grid" id="pluginsGrid">
    ${cardsHtml}
  </div>

  <!-- 插件详情弹窗 -->
  <div class="modal-overlay" id="modalOverlay" onclick="closeModal(event)">
    <div class="modal" id="modalContent" onclick="event.stopPropagation()">
      <button class="modal-close" onclick="closeModal()">&times;</button>
      <div id="modalBody"></div>
    </div>
  </div>

  <footer>
    <p>InTools 插件市场 &middot; <a href="https://github.com/${GITHUB_REPO}" target="_blank">GitHub</a> &middot; MIT License</p>
  </footer>

  <script>
    // ── 搜索 & 筛选 ──
    const searchInput = document.getElementById('searchInput');
    const grid = document.getElementById('pluginsGrid');
    const resultCount = document.getElementById('resultCount');
    const cards = Array.from(grid.querySelectorAll('.plugin-card'));

    let activeCategory = 'all';
    let activeLanguage = 'all';
    let activeTags = new Set();

    searchInput.addEventListener('input', applyFilters);

    document.querySelectorAll('.filter-btn').forEach(btn => {
      btn.addEventListener('click', () => {
        const type = btn.dataset.filterType;
        const value = btn.dataset.filterValue;

        if (type === 'category') {
          activeCategory = value;
          document.querySelectorAll('.filter-btn[data-filter-type="category"]').forEach(b => b.classList.remove('active'));
          btn.classList.add('active');
        } else if (type === 'language') {
          activeLanguage = value;
          document.querySelectorAll('.filter-btn[data-filter-type="language"]').forEach(b => b.classList.remove('active'));
          btn.classList.add('active');
        } else if (type === 'tag') {
          if (activeTags.has(value)) {
            activeTags.delete(value);
            btn.classList.remove('active');
          } else {
            activeTags.add(value);
            btn.classList.add('active');
          }
        }
        applyFilters();
      });
    });

    function applyFilters() {
      const q = searchInput.value.toLowerCase();
      let shown = 0;

      cards.forEach(card => {
        const matchCategory = activeCategory === 'all' || card.dataset.category === activeCategory;
        const matchLanguage = activeLanguage === 'all' || card.dataset.language === activeLanguage;

        let matchTags = true;
        if (activeTags.size > 0) {
          const cardTags = card.dataset.tags.split(',').filter(Boolean);
          matchTags = [...activeTags].some(t => cardTags.includes(t));
        }

        const searchText = (
          card.dataset.id + ' ' + card.dataset.name + ' ' +
          card.dataset.description + ' ' + card.dataset.tags + ' ' +
          card.dataset.tools
        ).toLowerCase();
        const matchSearch = !q || searchText.includes(q);

        const visible = matchCategory && matchLanguage && matchTags && matchSearch;
        card.classList.toggle('hidden', !visible);
        if (visible) shown++;
      });

      resultCount.textContent = '显示 ' + shown + ' 个插件';

      // 空结果提示
      let empty = grid.querySelector('.empty-state');
      if (shown === 0) {
        if (!empty) {
          empty = document.createElement('div');
          empty.className = 'empty-state';
          empty.innerHTML = '<div class="empty-icon">🔍</div><p>没有找到匹配的插件</p>';
          grid.appendChild(empty);
        }
      } else if (empty) {
        empty.remove();
      }
    }

    // ── 插件详情弹窗 ──
    function showDetail(card) {
      const id = card.dataset.id;
      const name = card.dataset.name;
      const desc = card.dataset.description;
      const lang = card.dataset.language;
      const cat = card.dataset.category;
      const tags = card.dataset.tags.split(',').filter(Boolean);
      const tools = card.dataset.tools.split(',').filter(Boolean);

      const catLabel = { system: '系统插件', user: '用户插件', test: '测试插件', direct: '社区插件' };

      let toolsHtml = '';
      // 尝试从 card 的 HTML 提取工具详情
      // （工具名在 data-tools 中，描述需要从 search.json 获取，这里只展示名称）

      document.getElementById('modalBody').innerHTML =
        '<h2>' + name + '</h2>' +
        '<div class="modal-meta">' +
          '<span>📂 ' + (catLabel[cat] || cat) + '</span>' +
          '<span>🌐 ' + lang + '</span>' +
          '<span>v' + card.querySelector('.card-version').textContent.replace('v','') + '</span>' +
          '<span>💾 ' + card.querySelector('.card-size').textContent + '</span>' +
        '</div>' +
        '<p class="modal-desc">' + (desc || '暂无描述') + '</p>' +
        (tags.length ? '<div class="card-tags">' + tags.map(t => '<span class="tag">' + t + '</span>').join('') + '</div>' : '') +
        '<div class="modal-section"><h4>工具 (' + tools.length + ')</h4>' +
          '<ul class="tool-list">' + tools.map(t => '<li><span class="tool-name">' + t + '</span></li>').join('') + '</ul>' +
        '</div>' +
        '<div class="modal-actions">' +
          '<a class="btn btn-primary" href="https://github.com/${GITHUB_REPO}/tree/main/plugins/' + (cat === 'direct' ? '' : cat + '/') + name + '" target="_blank">查看源码</a>' +
          '<button class="btn btn-outline" onclick="closeModal()">关闭</button>' +
        '</div>';

      document.getElementById('modalOverlay').classList.add('open');
    }

    function closeModal(e) {
      if (!e || e.target === document.getElementById('modalOverlay')) {
        document.getElementById('modalOverlay').classList.remove('open');
      }
    }

    document.addEventListener('keydown', e => { if (e.key === 'Escape') closeModal(); });
  </script>
</body>
</html>`;
}

/** 格式化文件大小 */
function formatSize(bytes) {
  if (bytes < 1024) return bytes + ' B';
  if (bytes < 1024 * 1024) return (bytes / 1024).toFixed(1) + ' KB';
  return (bytes / (1024 * 1024)).toFixed(1) + ' MB';
}

// ─── 主函数 ──────────────────────────────────────────────────

function main() {
  if (!fs.existsSync(OUTPUT_DIR)) {
    fs.mkdirSync(OUTPUT_DIR, { recursive: true });
  }

  console.log('🔍 Scanning plugins...');
  const plugins = scanPlugins();
  console.log(`   Found ${plugins.length} unique plugins (after dedup)`);

  console.log('📄 Generating plugins.json...');
  const pluginIndex = generatePluginIndex(plugins);
  fs.writeFileSync(path.join(OUTPUT_DIR, 'plugins.json'), JSON.stringify(pluginIndex, null, 2));

  console.log('📦 Generating versions.json...');
  const versionInfo = generateVersionInfo(plugins);
  fs.writeFileSync(path.join(OUTPUT_DIR, 'versions.json'), JSON.stringify(versionInfo, null, 2));

  console.log('🔎 Generating search.json...');
  const searchIndex = generateSearchIndex(plugins);
  fs.writeFileSync(path.join(OUTPUT_DIR, 'search.json'), JSON.stringify(searchIndex, null, 2));

  console.log('🌐 Generating index.html...');
  const html = generateIndexHtml(plugins);
  fs.writeFileSync(path.join(OUTPUT_DIR, 'index.html'), html);

  console.log('✅ Marketplace index generated successfully!');
}

main();
