#!/usr/bin/env node

/**
 * 构建插件市场索引
 * 
 * 这个脚本会：
 * 1. 扫描 plugins/ 目录下的所有插件
 * 2. 解析每个插件的 manifest.toml
 * 3. 生成插件索引 JSON 文件
 * 4. 生成版本信息 JSON 文件
 * 5. 输出到 marketplace/ 目录
 */

const fs = require('fs');
const path = require('path');
const toml = require('toml');

// 配置
const PLUGINS_DIR = path.join(__dirname, '..', 'plugins');
const OUTPUT_DIR = path.join(__dirname, '..', 'marketplace');
const GITHUB_REPO = 'buxiaju/in-tools'; // 替换为你的 GitHub 仓库

// 确保输出目录存在
if (!fs.existsSync(OUTPUT_DIR)) {
  fs.mkdirSync(OUTPUT_DIR, { recursive: true });
}

// 扫描插件目录
function scanPlugins() {
  const plugins = [];
  const pluginDirs = ['system', 'test', 'user'];

  // 扫描指定目录下的插件
  for (const dir of pluginDirs) {
    const dirPath = path.join(PLUGINS_DIR, dir);
    if (!fs.existsSync(dirPath)) continue;

    const entries = fs.readdirSync(dirPath, { withFileTypes: true });
    for (const entry of entries) {
      if (!entry.isDirectory()) continue;

      const pluginPath = path.join(dirPath, entry.name);
      const manifestPath = path.join(pluginPath, 'manifest.toml');

      if (!fs.existsSync(manifestPath)) continue;

      try {
        // 读取文件并去除 BOM 字符
        let manifestContent = fs.readFileSync(manifestPath, 'utf-8');
        // 移除 UTF-8 BOM
        if (manifestContent.charCodeAt(0) === 0xFEFF) {
          manifestContent = manifestContent.slice(1);
        }
        const manifest = toml.parse(manifestContent);

        // 读取 README（如果存在）
        let readme = '';
        const readmePath = path.join(pluginPath, 'README.md');
        if (fs.existsSync(readmePath)) {
          readme = fs.readFileSync(readmePath, 'utf-8');
          if (readme.charCodeAt(0) === 0xFEFF) {
            readme = readme.slice(1);
          }
        }

        // 计算文件大小
        let totalSize = 0;
        const files = fs.readdirSync(pluginPath);
        for (const file of files) {
          const filePath = path.join(pluginPath, file);
          const stat = fs.statSync(filePath);
          if (stat.isFile()) {
            totalSize += stat.size;
          }
        }

        plugins.push({
          id: manifest.plugin.id,
          name: manifest.plugin.name,
          version: manifest.plugin.version,
          description: manifest.plugin.description || '',
          author: manifest.plugin.author || '',
          category: dir,
          tags: extractTags(manifest),
          homepage: `https://github.com/${GITHUB_REPO}/tree/main/plugins/${dir}/${entry.name}`,
          repository: `https://github.com/${GITHUB_REPO}`,
          license: 'MIT',
          created_at: fs.statSync(pluginPath).birthtime.toISOString(),
          updated_at: fs.statSync(manifestPath).mtime.toISOString(),
          tools: manifest.tools || [],
          capabilities: manifest.capabilities || {},
          lifecycle: manifest.lifecycle || {},
          readme: readme.substring(0, 500), // 只保留前 500 字符
          file_size: totalSize,
          download_url: `https://github.com/${GITHUB_REPO}/raw/main/plugins/${dir}/${entry.name}.zip`,
        });
      } catch (error) {
        console.error(`Error parsing ${manifestPath}:`, error.message);
      }
    }
  }

  // 扫描 plugins/ 目录下的直接插件（兼容旧结构）
  const directEntries = fs.readdirSync(PLUGINS_DIR, { withFileTypes: true });
  for (const entry of directEntries) {
    if (!entry.isDirectory()) continue;
    if (pluginDirs.includes(entry.name)) continue; // 跳过已扫描的目录

    const pluginPath = path.join(PLUGINS_DIR, entry.name);
    const manifestPath = path.join(pluginPath, 'manifest.toml');

    if (!fs.existsSync(manifestPath)) continue;

    // 检查是否已经在子目录中扫描过
    const pluginId = entry.name;
    if (plugins.some(p => p.id === pluginId || p.id.endsWith(`.${pluginId}`))) {
      continue;
    }

    try {
      const manifestContent = fs.readFileSync(manifestPath, 'utf-8');
      const manifest = toml.parse(manifestContent);

      // 读取 README（如果存在）
      let readme = '';
      const readmePath = path.join(pluginPath, 'README.md');
      if (fs.existsSync(readmePath)) {
        readme = fs.readFileSync(readmePath, 'utf-8');
      }

      // 计算文件大小
      let totalSize = 0;
      const files = fs.readdirSync(pluginPath);
      for (const file of files) {
        const filePath = path.join(pluginPath, file);
        const stat = fs.statSync(filePath);
        if (stat.isFile()) {
          totalSize += stat.size;
        }
      }

      plugins.push({
        id: manifest.plugin.id,
        name: manifest.plugin.name,
        version: manifest.plugin.version,
        description: manifest.plugin.description || '',
        author: manifest.plugin.author || '',
        category: 'direct',
        tags: extractTags(manifest),
        homepage: `https://github.com/${GITHUB_REPO}/tree/main/plugins/${entry.name}`,
        repository: `https://github.com/${GITHUB_REPO}`,
        license: 'MIT',
        created_at: fs.statSync(pluginPath).birthtime.toISOString(),
        updated_at: fs.statSync(manifestPath).mtime.toISOString(),
        tools: manifest.tools || [],
        capabilities: manifest.capabilities || {},
        lifecycle: manifest.lifecycle || {},
        readme: readme.substring(0, 500), // 只保留前 500 字符
        file_size: totalSize,
        download_url: `https://github.com/${GITHUB_REPO}/raw/main/plugins/${entry.name}.zip`,
      });
    } catch (error) {
      console.error(`Error parsing ${manifestPath}:`, error.message);
    }
  }

  return plugins;
}

// 提取标签
function extractTags(manifest) {
  const tags = [];
  
  // 从工具名中提取标签
  if (manifest.tools) {
    for (const tool of manifest.tools) {
      const prefix = tool.name.split(':')[0];
      if (!tags.includes(prefix)) {
        tags.push(prefix);
      }
    }
  }

  // 从权限中提取标签
  if (manifest.capabilities && manifest.capabilities.permissions) {
    for (const perm of manifest.capabilities.permissions) {
      const category = perm.split(':')[0];
      if (!tags.includes(category)) {
        tags.push(category);
      }
    }
  }

  return tags;
}

// 生成插件索引
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
      category: p.category,
      tags: p.tags,
      homepage: p.homepage,
      license: p.license,
      created_at: p.created_at,
      updated_at: p.updated_at,
      file_size: p.file_size,
      download_count: 0, // 初始下载次数为 0
      rating: 0, // 初始评分为 0
      rating_count: 0,
    })),
  };
}

// 生成版本信息
function generateVersionInfo(plugins) {
  const versions = {};
  
  for (const plugin of plugins) {
    versions[plugin.id] = {
      plugin_id: plugin.id,
      versions: [
        {
          version: plugin.version,
          released_at: plugin.updated_at,
          description: plugin.description,
          changelog: '',
          download_url: plugin.download_url,
          file_size: plugin.file_size,
          file_hash: '', // TODO: 计算文件哈希
          dependencies: {},
          min_host_version: null,
        },
      ],
    };
  }

  return {
    version: '1.0.0',
    generated_at: new Date().toISOString(),
    plugins: versions,
  };
}

// 生成搜索索引
function generateSearchIndex(plugins) {
  return {
    version: '1.0.0',
    generated_at: new Date().toISOString(),
    plugins: plugins.map(p => ({
      id: p.id,
      name: p.name,
      description: p.description,
      author: p.author,
      category: p.category,
      tags: p.tags,
      tools: p.tools.map(t => t.name),
    })),
  };
}

// 主函数
function main() {
  console.log('Scanning plugins...');
  const plugins = scanPlugins();
  console.log(`Found ${plugins.length} plugins`);

  console.log('Generating plugin index...');
  const pluginIndex = generatePluginIndex(plugins);
  fs.writeFileSync(
    path.join(OUTPUT_DIR, 'plugins.json'),
    JSON.stringify(pluginIndex, null, 2)
  );

  console.log('Generating version info...');
  const versionInfo = generateVersionInfo(plugins);
  fs.writeFileSync(
    path.join(OUTPUT_DIR, 'versions.json'),
    JSON.stringify(versionInfo, null, 2)
  );

  console.log('Generating search index...');
  const searchIndex = generateSearchIndex(plugins);
  fs.writeFileSync(
    path.join(OUTPUT_DIR, 'search.json'),
    JSON.stringify(searchIndex, null, 2)
  );

  // 生成索引页面
  const indexHtml = generateIndexHtml(plugins);
  fs.writeFileSync(
    path.join(OUTPUT_DIR, 'index.html'),
    indexHtml
  );

  console.log('Marketplace index generated successfully!');
}

// 生成索引页面
function generateIndexHtml(plugins) {
  return `<!DOCTYPE html>
<html lang="zh-CN">
<head>
  <meta charset="UTF-8">
  <meta name="viewport" content="width=device-width, initial-scale=1.0">
  <title>InTools 插件市场</title>
  <style>
    * {
      margin: 0;
      padding: 0;
      box-sizing: border-box;
    }
    
    body {
      font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, 'Helvetica Neue', Arial, sans-serif;
      line-height: 1.6;
      color: #333;
      background: #f5f5f5;
    }
    
    .container {
      max-width: 1200px;
      margin: 0 auto;
      padding: 20px;
    }
    
    header {
      background: linear-gradient(135deg, #667eea 0%, #764ba2 100%);
      color: white;
      padding: 40px 0;
      margin-bottom: 30px;
    }
    
    header h1 {
      font-size: 2.5em;
      margin-bottom: 10px;
    }
    
    header p {
      font-size: 1.2em;
      opacity: 0.9;
    }
    
    .search-box {
      background: white;
      padding: 20px;
      border-radius: 8px;
      box-shadow: 0 2px 10px rgba(0,0,0,0.1);
      margin-bottom: 30px;
    }
    
    .search-box input {
      width: 100%;
      padding: 12px;
      font-size: 16px;
      border: 2px solid #ddd;
      border-radius: 4px;
    }
    
    .plugins-grid {
      display: grid;
      grid-template-columns: repeat(auto-fill, minmax(300px, 1fr));
      gap: 20px;
    }
    
    .plugin-card {
      background: white;
      border-radius: 8px;
      padding: 20px;
      box-shadow: 0 2px 10px rgba(0,0,0,0.1);
      transition: transform 0.2s;
    }
    
    .plugin-card:hover {
      transform: translateY(-5px);
    }
    
    .plugin-card h3 {
      color: #667eea;
      margin-bottom: 10px;
    }
    
    .plugin-card p {
      color: #666;
      margin-bottom: 15px;
    }
    
    .plugin-meta {
      display: flex;
      justify-content: space-between;
      color: #999;
      font-size: 0.9em;
    }
    
    .tags {
      margin-top: 10px;
    }
    
    .tag {
      display: inline-block;
      background: #e9ecef;
      padding: 2px 8px;
      border-radius: 4px;
      font-size: 0.8em;
      margin-right: 5px;
      margin-bottom: 5px;
    }
    
    footer {
      text-align: center;
      padding: 30px;
      color: #666;
      margin-top: 50px;
    }
  </style>
</head>
<body>
  <header>
    <div class="container">
      <h1>InTools 插件市场</h1>
      <p>发现和安装 InTools 插件，扩展你的桌面工具能力</p>
    </div>
  </header>

  <div class="container">
    <div class="search-box">
      <input type="text" id="searchInput" placeholder="搜索插件..." oninput="filterPlugins()">
    </div>

    <div class="plugins-grid" id="pluginsGrid">
      ${plugins.map(p => `
        <div class="plugin-card" data-id="${p.id}" data-name="${p.name}" data-description="${p.description}">
          <h3>${p.name}</h3>
          <p>${p.description || '暂无描述'}</p>
          <div class="tags">
            ${p.tags.map(t => `<span class="tag">${t}</span>`).join('')}
          </div>
          <div class="plugin-meta">
            <span>${p.author || '未知作者'}</span>
            <span>v${p.version}</span>
          </div>
        </div>
      `).join('')}
    </div>
  </div>

  <footer>
    <p>InTools 插件市场 - 一切皆插件</p>
  </footer>

  <script>
    function filterPlugins() {
      const input = document.getElementById('searchInput').value.toLowerCase();
      const cards = document.querySelectorAll('.plugin-card');
      
      cards.forEach(card => {
        const id = card.dataset.id.toLowerCase();
        const name = card.dataset.name.toLowerCase();
        const description = card.dataset.description.toLowerCase();
        
        if (id.includes(input) || name.includes(input) || description.includes(input)) {
          card.style.display = 'block';
        } else {
          card.style.display = 'none';
        }
      });
    }
  </script>
</body>
</html>`;
}

// 运行
main();
