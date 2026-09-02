//! 插件实例状态机：握手、请求超时、空闲回收、连接断开处理。
//!
//! Phase 3 将在此实现状态枚举
//! `Stopped` / `Starting` / `Idle` / `Busy` / `Stopping` / `Error`
//! 及其转移规则。
