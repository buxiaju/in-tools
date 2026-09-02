//! 子进程管理：spawn、stdio 接管、stderr 转存、优雅关闭。
//!
//! Phase 4 将在此实现 `StdioTransport`，并在 Windows 上通过
//! Job Object 确保子进程随宿主退出而终止。
