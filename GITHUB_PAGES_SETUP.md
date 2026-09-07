# GitHub Pages 启用指南

## 步骤 1: 启用 GitHub Pages

1. 访问你的 GitHub 仓库: https://github.com/buxiaju/in-tools
2. 点击 **Settings** 标签
3. 在左侧菜单中找到 **Pages**
4. 在 **Source** 部分:
   - 选择 **GitHub Actions**
5. 点击 **Save**

## 步骤 2: 等待 GitHub Actions 运行

1. 在仓库页面点击 **Actions** 标签
2. 你会看到 "Build Marketplace Index" 工作流
3. 如果它正在运行，等待它完成（通常 1-2 分钟）
4. 如果它没有运行，点击 **Run workflow** 手动触发

## 步骤 3: 验证部署

工作流完成后，访问以下 URL 验证:

- **插件市场页面**: https://buxiaju.github.io/in-tools/marketplace/
- **插件列表 API**: https://buxiaju.github.io/in-tools/marketplace/plugins.json
- **版本信息 API**: https://buxiaju.github.io/in-tools/marketplace/versions.json

## 步骤 4: 在 InTools 中使用

1. 打开 InTools 设置
2. 找到 "插件市场" 选项
3. 启用插件市场
4. 输入市场 URL: `https://buxiaju.github.io/in-tools/marketplace/`

## 故障排除

### 问题: GitHub Pages 没有显示

**解决方案**:
1. 检查 GitHub Actions 是否成功完成
2. 确认 Pages 设置中选择了 "GitHub Actions"
3. 等待几分钟，GitHub Pages 可能需要时间生效

### 问题: 插件列表为空

**解决方案**:
1. 检查 `plugins/` 目录下是否有插件
2. 确保每个插件都有 `manifest.toml` 文件
3. 重新运行 GitHub Actions 工作流

### 问题: 无法访问 API

**解决方案**:
1. 确认 GitHub Pages 已启用
2. 检查仓库是否为 Public（GitHub Pages 需要公开仓库）
3. 清除浏览器缓存后重试

## 发布新插件

要发布新插件到市场:

1. 将插件添加到 `plugins/system/` 或 `plugins/user/` 目录
2. 确保包含 `manifest.toml` 文件
3. 推送到 `main` 分支
4. GitHub Actions 会自动更新索引
5. 等待 1-2 分钟，新插件就会出现在市场中

## 自定义市场

如果你想自定义市场:

1. 编辑 `scripts/build-marketplace-index.js` 文件
2. 修改 `GITHUB_REPO` 变量为你的仓库地址
3. 自定义网页样式和布局
4. 推送更改，GitHub Actions 会自动重新构建

## 更多信息

- [GitHub Pages 文档](https://docs.github.com/en/pages)
- [GitHub Actions 文档](https://docs.github.com/en/actions)
- [InTools 插件开发指南](./docs/plugin-development.md)
