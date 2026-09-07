# 安全文档

> 深入了解 InTools 的安全机制、威胁模型和最佳实践。

## 📋 目录

- [安全模型](#安全模型)
- [权限系统](#权限系统)
- [网络隔离](#网络隔离)
- [资源限制](#资源限制)
- [审计系统](#审计系统)
- [最佳实践](#最佳实践)
- [已知限制](#已知限制)

## 安全模型

### 多层防护

InTools 采用纵深防御策略，多个安全层协同工作：

```
┌─────────────────────────────────────┐
│  协议层：JSON-RPC 2.0 格式校验     │
├─────────────────────────────────────┤
│  权限层：三级危险度 + 网络隔离      │
├─────────────────────────────────────┤
│  运行时层：进程隔离 + 资源监控      │
├─────────────────────────────────────┤
│  审计层：完整调用链追踪             │
└─────────────────────────────────────┘
```

### 调用方身份

所有调用都有明确的身份标识：

```rust
pub enum CallerIdentity {
    /// 用户界面发起
    Ui,
    /// 另一个插件通过反向 RPC 发起
    Plugin { id: String, depth: u8 },
    /// 外部 MCP 客户端发起
    Mcp { client_name: String },
}
```

不同身份有不同的权限和操作限制。

## 权限系统

### 三级危险度

| 等级 | 处理方式 | 示例 |
|------|---------|------|
| **低危** | manifest 声明即视为已授权 | file:read, clipboard:read, network:http |
| **中危** | 首次调用时询问，可记住 | file:write, clipboard:write |
| **高危** | 每会话首次询问 | process:spawn, shell:exec, input:control |

### 授权记录

授权记录持久化到 `~/.intools/permissions.json`：

```json
{
  "com.example.ocr": {
    "screen:capture": {
      "granted": true,
      "scope": "always",
      "at": "2026-09-02T10:00:00Z"
    },
    "file:read": {
      "granted": true,
      "scope": "always",
      "paths": ["~/Pictures"]
    }
  }
}
```

### 工具级权限

InTools 支持工具级权限控制。插件可以为每个工具声明独立的权限：

```toml
[[tools]]
name = "file:read"
description = "读取文件"
permissions = ["file:read"]

[[tools]]
name = "file:write"
description = "写入文件"
permissions = ["file:write"]
```

### MCP 安全约束

MCP 网关有额外的安全规则：

- **默认关闭**：需要用户显式开启
- **仅本机监听**：硬编码绑定 `127.0.0.1`
- **Bearer Token 鉴权**：随机生成 32 字节 token
- **默认零暴露**：新装插件不会自动出现
- **高危不外放**：高危插件全部工具不暴露给 MCP

## 网络隔离

### 控制维度

InTools 提供四维网络访问控制：

1. **协议控制**：允许/禁止 HTTP、HTTPS、WebSocket、TCP、UDP 等
2. **域名控制**：白名单/黑名单域名
3. **IP 控制**：CIDR 范围白名单/黑名单
4. **端口控制**：白名单/黑名单端口

### 优先级

- **黑名单 > 白名单 > 默认**
- 黑名单中的内容永远禁止
- 白名单非空时，只允许白名单内容
- 白名单为空时，允许所有内容

### 配置示例

```rust
// 限制只能访问 example.com 的 HTTPS
let mut policy = NetworkPolicy::default();
policy.allow_domain("example.com");
policy.allow_protocol("https");
policy.allow_port(443);
```

## 资源限制

### 限制维度

插件可以声明资源使用限制：

```toml
[lifecycle.resource_limits]
max_memory_mb = 512      # 最大内存（MB）
max_cpu_percent = 50     # 最大 CPU 使用率（%）
max_disk_mb = 1024       # 最大磁盘使用（MB）
max_network_kbps = 1024  # 最大网络带宽（KB/s）
```

### 监控机制

- 实时监控：每 5 秒检查一次
- 超限警告：日志记录
- 严重超限：拒绝调用

## 审计系统

### 增强审计

InTools 提供详细的调用链追踪：

- 每次工具调用的完整信息
- 嵌套调用的父子关系
- 性能统计（耗时、成功率）
- 安全事件记录

### 审计字段

```rust
pub struct AuditEntry {
    pub caller: String,        // 调用方
    pub tool: String,          // 工具名
    pub plugin_id: String,     // 插件 ID
    pub args_summary: String,  // 参数摘要（不记录全文）
    pub duration_ms: u64,      // 耗时
    pub outcome: String,       // 结果
}
```

### 安全事件

系统自动检测并记录以下安全事件：

- 权限拒绝
- 网络访问被阻止
- 资源使用超限
- 可疑调用模式
- 调用链过深
- 调用频率过高
- 参数异常

### 审计日志位置

- 工具调用审计：`~/.intools/logs/<plugin-id>.log`
- MCP 审计：`~/.intools/logs/mcp-audit.log`
- 审计查看：在 UI 的权限管理页面

## 最佳实践

### 插件开发者

1. **最小权限原则**：只声明必需的权限
2. **显式声明**：在 manifest 中明确列出所有依赖
3. **错误处理**：优雅处理权限拒绝错误
4. **资源节制**：避免大量内存/CPU 消耗
5. **签名发布**：为发布到插件市场的插件签名

### 用户

1. **审查权限**：安装插件前仔细阅读权限要求
2. **谨慎授权**：高危权限建议选择"仅本次"
3. **定期审计**：在权限管理页面查看已授权项
4. **及时撤销**：不再使用的插件撤销授权
5. **检查来源**：只从可信来源安装插件

### 系统管理员

1. **网络隔离**：在企业环境中配置网络策略
2. **日志监控**：定期审查审计日志
3. **更新及时**：保持 InTools 和插件为最新版本
4. **备份配置**：定期备份权限配置和授权记录

## 已知限制

### 子进程模型

在子进程架构下，插件进程拥有宿主用户的完整系统权限，宿主无法在操作系统层面阻止恶意插件直接访问文件或网络。

**缓解措施：**
- 透明化：用户清楚知晓插件声明要做什么
- 管控宿主提供的能力：插件经 `host/*` 反向 RPC 获得的能力受严格约束
- 可审计：所有工具调用记入日志

**真正的隔离需要 OS 级机制：**
- Windows AppContainer
- macOS Sandbox
- Linux namespaces

这些不在第一版范围内。

### 密码学局限

- API key 存储为明文（用户目录）
- 插件签名使用简化实现（占位）
- 文件哈希算法：SHA-256

**建议：**
- 不要在共享计算机上使用
- 定期轮换 API key
- 后续版本会增强密码学支持

### 网络攻击面

- 任何能访问 `127.0.0.1:7801` 的人都可以访问 MCP 网关
- 持有 Bearer Token 的人可以完全控制 MCP

**建议：**
- 防火墙配置
- 定期更换 token
- 不在公共网络上启用 MCP

## 漏洞报告

如果你发现了安全漏洞，请：

1. **不要**在公开的 Issue Tracker 中报告
2. 发送邮件到：[security email placeholder]
3. 包含详细的复现步骤和影响评估
4. 等待回复（通常 48 小时内）

## 未来安全增强

- [ ] OS 级沙箱（Windows AppContainer / macOS Sandbox / Linux namespaces）
- [ ] 插件代码签名强制验证
- [ ] 端到端加密的 API key 存储
- [ ] 实时安全事件告警
- [ ] 行为分析和异常检测
- [ ] 自动漏洞扫描

## 参考

- [设计文档 §7 权限模型](superpowers/specs/2026-09-02-intools-plugin-host-design.md)
- [设计文档 §10 MCP 安全约束](superpowers/specs/2026-09-02-intools-plugin-host-design.md)
- [插件开发指南](plugin-development.md)
