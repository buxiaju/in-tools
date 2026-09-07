# 贡献指南

感谢你对 InTools 项目的关注！我们欢迎任何形式的贡献，包括但不限于：

- 🐛 报告 Bug
- 💡 提出新功能建议
- 📝 改进文档
- 🔧 提交代码修复
- ✨ 开发新插件
- 🌍 翻译

## 📋 目录

- [行为准则](#行为准则)
- [如何贡献](#如何贡献)
- [开发流程](#开发流程)
- [代码规范](#代码规范)
- [提交规范](#提交规范)
- [插件开发](#插件开发)

## 行为准则

### 我们的承诺

为了营造一个开放和友好的环境，我们承诺：

- 尊重所有贡献者，无论其经验水平、性别、性取向、残疾、外貌、体型、种族或宗教信仰
- 接受建设性的批评
- 关注对社区最有利的事情
- 对其他社区成员表示同理心

### 不可接受的行为

- 使用性化的语言或图像
- 人身攻击或政治攻击
- 公开或私下骚扰
- 未经许可发布他人的私人信息
- 其他不专业或不适当的行为

## 如何贡献

### 报告 Bug

在提交 Bug 报告之前，请：

1. 检查 [Issues](https://github.com/buxiaju/in-tools/issues) 是否已有相关报告
2. 尝试更新到最新版本，看问题是否仍然存在

提交 Bug 报告时，请包含：

- **清晰的标题**：简明扼要地描述问题
- **详细描述**：包括发生的情况、期望情况和实际行为
- **复现步骤**：如何触发这个 Bug
- **环境信息**：
  - 操作系统版本
  - InTools 版本
  - 相关插件版本
- **截图或日志**：如果适用

### 提出新功能建议

提交功能建议时，请说明：

- 功能的用途和使用场景
- 期望的行为
- 是否有替代方案
- 是否愿意自己实现

### 提交代码

1. Fork 这个仓库
2. 创建你的特性分支 (`git checkout -b feature/AmazingFeature`)
3. 提交你的更改 (`git commit -m 'Add some AmazingFeature'`)
4. 推送到分支 (`git push origin feature/AmazingFeature`)
5. 开启一个 Pull Request

## 开发流程

### 环境要求

- **Rust**：1.88+ （推荐使用 rustup 安装最新稳定版）
- **Node.js**：20+ （用于插件市场构建脚本）
- **Python**：3.13+ （用于开发 Python 插件）
- **MSVC 构建工具**：Windows 下编译 Tauri 应用需要
- **WebView2 Runtime**：Windows 10/11 通常已预装

### 本地开发

```bash
# 克隆仓库
git clone https://github.com/buxiaju/in-tools.git
cd in-tools

# 编译并运行（开发模式）
cd src-tauri
cargo tauri dev

# 运行测试
cargo test

# 构建发布版本
cargo tauri build
```

### 项目结构

```
in-tools/
├── src-tauri/              # Rust 宿主程序
│   ├── src/
│   │   ├── commands.rs     # Tauri commands
│   │   ├── config/         # 配置管理
│   │   ├── gateway.rs      # MCP 网关
│   │   ├── hotkey.rs       # 全局快捷键
│   │   ├── mcp/           # MCP 协议实现
│   │   ├── permission/    # 权限管理
│   │   ├── protocol/      # 插件通信协议
│   │   ├── registry/      # 插件注册表
│   │   ├── runtime/       # 运行时管理
│   │   └── shortcut/      # 快捷键
│   └── tests/             # 集成测试
├── plugins/                # 插件源码
│   ├── system/            # 系统插件
│   ├── test/              # 测试插件
│   ├── python-sdk/        # Python SDK
│   ├── node-sdk/          # Node.js SDK
│   └── tools/             # 开发者工具
├── src/                   # 前端资源
├── docs/                  # 文档
└── scripts/               # 构建脚本
```

## 代码规范

### Rust 代码

- 使用 `cargo fmt` 格式化代码
- 使用 `cargo clippy` 检查代码质量
- 所有公开 API 必须有文档注释
- 关键路径必须有单元测试
- 错误处理使用 `Result` 类型，避免 `panic!`

### Python 插件

- 遵循 PEP 8 规范
- 使用类型提示
- 关键功能添加文档字符串
- 避免使用第三方依赖（优先使用标准库）

### 前端代码

- 使用 2 空格缩进
- 避免全局变量
- 优先使用 ES6+ 特性
- 保持代码简洁可读

### 提交规范

使用 [Conventional Commits](https://www.conventionalcommits.org/zh-hans/) 规范：

```
<类型>(<范围>): <描述>

[可选的正文]

[可选的脚注]
```

**类型：**
- `feat`：新功能
- `fix`：Bug 修复
- `docs`：仅文档更改
- `style`：代码格式（不影响代码运行）
- `refactor`：重构（既不是新功能也不是 Bug 修复）
- `perf`：性能优化
- `test`：添加或修改测试
- `chore`：构建过程或辅助工具的变动

**示例：**
```
feat(plugins): 添加文件监控插件
fix(permission): 修复权限检查在 MCP 调用时的绕过问题
docs(readme): 更新安装说明
```

## 插件开发

InTools 是一个"一切皆插件"的平台。我们非常欢迎新的插件贡献！

### 快速开始

1. 查看 [插件开发文档](docs/plugin-development.md)
2. 使用 `plugin-builder` 插件生成新插件的骨架
3. 实现你的插件功能
4. 编写测试和文档
5. 提交到插件市场

### 插件规范

- 必须在 `manifest.toml` 中声明所有依赖的权限
- 关键功能必须有单元测试
- 必须包含 README 文档
- 资源使用应当节制（参考资源限制功能）
- 错误处理要优雅

## Pull Request 流程

1. 确保你的代码通过所有测试 (`cargo test`)
2. 确保代码通过 clippy 检查 (`cargo clippy --all-targets -- -D warnings`)
3. 更新相关文档
4. 在 PR 描述中说明：
   - 解决的问题
   - 实现的方法
   - 测试情况
   - 任何破坏性更改
5. 等待代码审查

## 许可证

通过贡献代码，你同意你的贡献将根据 [MIT 许可证](LICENSE) 进行授权。

## 联系方式

- GitHub Issues: https://github.com/buxiaju/in-tools/issues
- Gitee Issues: https://gitee.com/buxiaju/in-tools/issues

## 致谢

感谢所有为 InTools 做出贡献的开发者！🎉
