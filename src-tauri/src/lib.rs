//! InTools 插件宿主的库入口。
//!
//! 内核逻辑全部放在 lib 里，`main.rs` 只负责初始化日志与拉起 Tauri。
//! 这样切分有两个好处：
//!
//! 1. `tests/` 下的集成测试是独立 crate，只能链接 lib target，无法链接 binary。
//!    Phase 4 要用真实子进程做端到端验证，必须有 lib 才行。
//! 2. 迫使模块间的可见性是显式的（`pub`），而不是靠 binary 内部的私有互访蒙混过关。
//!
//! **依赖 `tauri` 的模块一律不在这里**（`commands`、`ui` 都挂在 `main.rs` 下）。
//! 内核只通过 `PermissionPrompter` / `NotificationSink` 等 trait 对外留缝，
//! 保持 headless 可测；此外实测一旦 lib 链上 tauri，`cargo test --lib` 的测试
//! 二进制会在启动阶段直接以 0xC0000139 失败，把全部内核测试一起拖垮。

pub mod config;
pub mod error;
pub mod health;
pub mod logger;
pub mod mcp;
pub mod permission;
pub mod protocol;
pub mod registry;
pub mod runtime;
pub mod shortcut;
