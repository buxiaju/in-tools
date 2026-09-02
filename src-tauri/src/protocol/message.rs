//! JSON-RPC 2.0 消息类型与逐行编解码。
//!
//! Phase 1 将在此实现 `JsonRpcRequest` / `JsonRpcResponse` /
//! `JsonRpcNotification` / `JsonRpcError`，以及处理粘包与半包的行编解码器。
