# InTools 插件市场

这是一个基于 GitHub Pages 的零成本插件市场解决方案。

## 工作原理

1. **自动构建**：每次推送到 `main` 分支时，GitHub Actions 会自动扫描 `plugins/` 目录
2. **生成索引**：脚本会解析所有插件的 `manifest.toml`，生成 JSON 索引文件
3. **部署到 GitHub Pages**：索引文件会自动部署到 GitHub Pages
4. **客户端集成**：InTools 客户端可以从 GitHub Pages 获取插件列表

## 文件结构

```
marketplace/
├── index.html          # 插件市场网页
├── plugins.json        # 插件列表索引
├── versions.json       # 版本信息
└── search.json         # 搜索索引
```

## 使用方法

### 浏览插件市场

访问：`https://buxiaju.github.io/InTools/marketplace/`

### 在 InTools 中使用

1. 打开 InTools 设置
2. 找到"插件市场"选项
3. 输入市场 URL：`https://buxiaju.github.io/InTools/marketplace/`
4. 浏览和安装插件

## 发布新插件

1. 将插件添加到 `plugins/system/` 或 `plugins/user/` 目录
2. 确保插件包含 `manifest.toml` 文件
3. 推送到 `main` 分支
4. GitHub Actions 会自动更新索引

## manifest.toml 示例

```toml
[plugin]
id = "com.example.my-plugin"
name = "我的插件"
version = "1.0.0"
author = "Your Name"
description = "这是一个示例插件"

[exec]
command = "python"
args = ["-u", "main.py"]

[[tools]]
name = "my:tool"
description = "我的工具"
[tools.input_schema]
type = "object"
properties = {}

[capabilities]
permissions = ["network:http"]

[lifecycle]
mode = "on-demand"
idle_timeout_sec = 300
```

## 自定义

### 修改 GitHub 仓库

编辑 `scripts/build-marketplace-index.js` 中的 `GITHUB_REPO` 变量：

```javascript
const GITHUB_REPO = 'your-username/your-repo';
```

### 添加插件分类

在 `scripts/build-marketplace-index.js` 中修改 `pluginDirs` 数组：

```javascript
const pluginDirs = ['system', 'test', 'user', 'community'];
```

### 自定义网页样式

编辑 `scripts/build-marketplace-index.js` 中的 `generateIndexHtml` 函数。

## API 端点

- `GET /plugins.json` - 获取插件列表
- `GET /versions.json` - 获取版本信息
- `GET /search.json` - 获取搜索索引

## 安全考虑

1. **插件签名**：建议所有插件都进行签名
2. **代码审查**：建议对插件进行人工审查
3. **权限声明**：插件必须声明所需权限
4. **恶意代码检测**：使用自动化工具检测恶意代码

## 后续改进

- [ ] 添加插件评分和评论功能
- [ ] 实现插件下载统计
- [ ] 添加插件分类和标签筛选
- [ ] 实现插件版本历史
- [ ] 添加插件依赖管理
- [ ] 实现插件自动更新

## 贡献

欢迎提交 Pull Request 来改进插件市场！

## 许可证

MIT License
