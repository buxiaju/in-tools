//! MCP 服务端：Streamable HTTP 端点与 Bearer Token 鉴权。
//!
//! Phase 8 将在此用 axum 实现 `/mcp` 端点，绑定 127.0.0.1:7801，
//! 支持 `initialize` / `tools/list` / `tools/call`。
