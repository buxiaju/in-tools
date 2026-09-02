//! `Transport` 抽象：屏蔽真实子进程与测试替身的差异。
//!
//! Phase 3 将在此实现 `Transport` trait 与 `MockTransport`（内存队列），
//! 使实例状态机可在不启动任何进程的情况下完整测试。
