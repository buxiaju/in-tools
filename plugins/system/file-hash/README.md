# 文件哈希插件

计算文件的 MD5/SHA1/SHA256 哈希值，用于文件校验与完整性检查。

## 语言

本插件使用 **Go** 编写，编译为单个 `.exe` 后即插即用，无需安装运行时。

## 编译

```bash
cd plugins/file-hash
go build -o file-hash.exe main.go
```

依赖：仅 Go 标准库，无第三方模块。

## 工具

| 工具 | 说明 |
|------|------|
| `hash:file` | 计算单个文件的 MD5/SHA1/SHA256 |
| `hash:dir` | 递归计算目录下所有文件的 SHA256 |

## 特点

- **一次遍历三份哈希**：利用 `io.MultiWriter` 读一遍文件同时算 MD5、SHA1、SHA256
- **大缓冲区**：结果可能较长，scanner 缓冲区设为 256KB
- **编译型**：Go 编译后为单一二进制，启动速度极快，适合系统工具类插件
