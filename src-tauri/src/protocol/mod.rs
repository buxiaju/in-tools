//! 协议层：宿主与插件之间的通信契约。
//!
//! 本模块为纯逻辑，不包含任何 IO，便于完全离线测试。

pub mod manifest;
pub mod message;
