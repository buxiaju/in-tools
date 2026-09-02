//! JSON-RPC 2.0 消息类型与逐行编解码。
//!
//! 宿主与插件通过 stdin/stdout 交换 JSON-RPC 2.0 消息，每条消息一行，以 `\n`
//! 结尾（参见设计文档 §5）。本模块不接触任何真实 IO，仅提供：
//! 1. 消息类型 `JsonRpcRequest` / `JsonRpcResponse` / `JsonRpcNotification` /
//!    `JsonRpcError`，以及用于统一收发的 `IncomingMessage` / `OutgoingMessage`
//!    枚举。
//! 2. `LineCodec`：面向字节缓冲区的逐行解析器，正确处理粘包（一次读入多行）
//!    和半包（一行被截断分两次到达），非法 JSON 行返回可识别的错误而不 panic。

// Phase 1 产物尚未被 registry/runtime 引用，整模块允许"未使用"。
#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use thiserror::Error;

/// JSON-RPC 2.0 请求 ID 类型。插件协议使用整数 ID，与设计文档示例一致；
/// 但为兼容手动测试时手工写入的字符串 ID，也允许 `String`。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RequestId {
    Int(i64),
    Str(String),
}

impl From<i64> for RequestId {
    fn from(v: i64) -> Self {
        RequestId::Int(v)
    }
}

impl From<String> for RequestId {
    fn from(v: String) -> Self {
        RequestId::Str(v)
    }
}

/// JSON-RPC 2.0 请求：`{"jsonrpc":"2.0","id":..,"method":"..","params":..}`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: RequestId,
    pub method: String,
    /// 插件协议中 params 始终是对象；`Option` 用以支持省略。
    #[serde(default = "default_empty_object", skip_serializing_if = "is_empty_object")]
    pub params: Option<JsonValue>,
}

fn default_empty_object() -> Option<JsonValue> {
    Some(JsonValue::Object(serde_json::Map::new()))
}

fn is_empty_object(v: &Option<JsonValue>) -> bool {
    match v {
        Some(JsonValue::Object(o)) => o.is_empty(),
        _ => false,
    }
}

impl JsonRpcRequest {
    /// 构造符合协议约定的请求：`jsonrpc` 固定为 `"2.0"`，`params` 默认为空对象。
    pub fn new(id: impl Into<RequestId>, method: impl Into<String>, params: impl Into<JsonValue>) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id: id.into(),
            method: method.into(),
            params: Some(params.into()),
        }
    }
}

/// JSON-RPC 2.0 通知：与 Request 结构一致但**没有 `id` 字段**。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JsonRpcNotification {
    pub jsonrpc: String,
    pub method: String,
    #[serde(default = "default_empty_object", skip_serializing_if = "is_empty_object")]
    pub params: Option<JsonValue>,
}

impl JsonRpcNotification {
    pub fn new(method: impl Into<String>, params: impl Into<JsonValue>) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            method: method.into(),
            params: Some(params.into()),
        }
    }
}

/// JSON-RPC 2.0 响应错误对象。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<JsonValue>,
}

impl JsonRpcError {
    pub fn new(code: i32, message: impl Into<String>) -> Self {
        Self { code, message: message.into(), data: None }
    }

    // JSON-RPC 2.0 预定义错误码。
    pub const CODE_PARSE_ERROR: i32 = -32700;
    pub const CODE_INVALID_REQUEST: i32 = -32600;
    pub const CODE_METHOD_NOT_FOUND: i32 = -32601;
    pub const CODE_INVALID_PARAMS: i32 = -32602;
    pub const CODE_INTERNAL_ERROR: i32 = -32603;

    // 插件协议定义的服务端自定义错误码（-32000 ~ -32099）。
    /// 插件端通用失败（与设计文档 §5.2 示例一致）。
    pub const CODE_PLUGIN_ERROR: i32 = -32000;
    /// `host/callTool` 调用链过深（>5）。
    pub const CODE_CALL_DEPTH: i32 = -32001;
    /// 权限被拒。
    pub const CODE_PERMISSION_DENIED: i32 = -32002;
    /// 工具不存在或未暴露。
    pub const CODE_TOOL_NOT_FOUND: i32 = -32003;
    /// 请求超时。
    pub const CODE_TIMEOUT: i32 = -32004;
}

/// JSON-RPC 2.0 响应：或成功（`result`）或失败（`error`），二者必居其一。
/// 使用 untagged 枚举直接表达"二选一"约束，序列化时恰好写入 `result` 或 `error` 一个字段。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: Option<RequestId>,
    #[serde(flatten)]
    pub body: ResponseBody,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ResponseBody {
    Success { result: JsonValue },
    Error { error: JsonRpcError },
}

impl JsonRpcResponse {
    pub fn success(id: impl Into<RequestId>, result: impl Into<JsonValue>) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id: Some(id.into()),
            body: ResponseBody::Success { result: result.into() },
        }
    }

    pub fn error(id: Option<RequestId>, err: JsonRpcError) -> Self {
        Self { jsonrpc: "2.0".to_string(), id, body: ResponseBody::Error { error: err } }
    }
}

/// 从对端（插件或宿主）读取的**一条消息**。
///
/// 由 [`LineCodec::decode`] 产出。请求与响应分开而不是混合，避免上层每次都做一次
/// "有 id 没 id" / "有 result 没 error" 的判别。通知单独拎出，因为其 `id` 字段
/// 必须缺失，这是 JSON-RPC 规范的硬约束。
#[derive(Debug, Clone, PartialEq)]
pub enum IncomingMessage {
    Request(JsonRpcRequest),
    Response(JsonRpcResponse),
    Notification(JsonRpcNotification),
}

/// 即将写到对端的消息。与 `IncomingMessage` 对称，仅为"发送侧"语义的别名，
/// 但在 transport 层允许"收到 response 时作为 request 的配对"这种区分会变得
/// 很有用，所以我们仍明确命名两侧。
#[derive(Debug, Clone, PartialEq)]
pub enum OutgoingMessage {
    Request(JsonRpcRequest),
    Response(JsonRpcResponse),
    Notification(JsonRpcNotification),
}

impl OutgoingMessage {
    /// 序列化为一行 JSON 文本（末尾无换行，换行由 LineCodec/Transport 负责）。
    pub fn to_line(&self) -> Result<String, MessageError> {
        let json = match self {
            OutgoingMessage::Request(m) => serde_json::to_string(m),
            OutgoingMessage::Response(m) => serde_json::to_string(m),
            OutgoingMessage::Notification(m) => serde_json::to_string(m),
        }
        .map_err(MessageError::Serialize)?;
        // JSON-RPC 单行禁止换行，防止被解码器误认为两条消息。serde_json 默认不
        // 输出换行，但显式断言一下更稳。
        debug_assert!(!json.contains('\n'));
        Ok(json)
    }
}

/// 消息解析 / 序列化过程中可识别的错误。
#[derive(Debug, Error)]
pub enum MessageError {
    #[error("JSON 序列化失败：{0}")]
    Serialize(#[source] serde_json::Error),

    #[error("一行不是合法 JSON：{0}")]
    InvalidJson(String),

    #[error("消息缺少 `jsonrpc: \"2.0\"` 字段或不是对象")]
    InvalidJsonRpcVersion,

    #[error("消息同时出现 `result` 与 `error`，或两者均缺失")]
    AmbiguousResponse,

    /// `LineCodec::decode` 返回此错误时，表示该行内容应被丢弃但解码过程本身可以继续。
    #[error("非法 JSON-RPC 消息：{0}")]
    Malformed(String),
}

// 测试里想对 `MessageError` 做 variant 判别；`serde_json::Error` 不实现
// `PartialEq`，所以不自动 derive，改为手写「只比较 variant，不比较 serde 错误细节」。
impl PartialEq for MessageError {
    fn eq(&self, other: &Self) -> bool {
        use MessageError::*;
        match (self, other) {
            (Serialize(_), Serialize(_)) => true,
            (InvalidJson(a), InvalidJson(b)) => a == b,
            (InvalidJsonRpcVersion, InvalidJsonRpcVersion) => true,
            (AmbiguousResponse, AmbiguousResponse) => true,
            (Malformed(a), Malformed(b)) => a == b,
            _ => false,
        }
    }
}

/// 行编解码器：维护输入缓冲区，处理粘包与半包。
///
/// 使用方式：transport 从底层读取字节后调用 [`LineCodec::feed`] 追加，然后循环
/// 调用 [`LineCodec::decode`] 取出完整消息，直到返回 `Ok(None)`（表示没有完整
/// 行可用，等待下一次 feed）。
#[derive(Debug, Default)]
pub struct LineCodec {
    buf: Vec<u8>,
}

impl LineCodec {
    pub fn new() -> Self {
        Self::default()
    }

    /// 可读缓冲区当前保存的字节数（含可能不完整的末行）。
    pub fn buffered_len(&self) -> usize {
        self.buf.len()
    }

    /// 把新读到的字节追加到缓冲区。
    pub fn feed(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// 尝试从缓冲区解析出一条完整消息。
    ///
    /// - `Ok(Some(msg))`：取到一条消息
    /// - `Ok(None)`：没有完整行，下次 feed 之后再试
    /// - `Err(e)`：缓冲区有完整行但不是合法 JSON-RPC。**该整行已被丢弃**，
    ///   上层应记入日志后继续调用 `decode` 处理后续行。
    pub fn decode(&mut self) -> Result<Option<IncomingMessage>, MessageError> {
        loop {
            let Some(newline_pos) = self.buf.iter().position(|b| *b == b'\n') else {
                // 没有换行：不是粘包就是半包，等下一次 feed。
                return Ok(None);
            };

            // 切出一行（不含换行符）。注意同时可能有 `\r`。
            let mut line_bytes = self.buf.drain(..=newline_pos);
            let mut line_vec: Vec<u8> = line_bytes.by_ref().collect();
            drop(line_bytes);
            // 去掉结尾的 \n 以及可选的 \r
            if line_vec.ends_with(b"\r\n") {
                line_vec.truncate(line_vec.len() - 2);
            } else if line_vec.ends_with(b"\n") {
                line_vec.truncate(line_vec.len() - 1);
            }

            // 跳过空行（插件调试时可能偶然输出）。
            if line_vec.iter().all(|b| b.is_ascii_whitespace()) {
                continue;
            }

            let text = String::from_utf8(line_vec).map_err(|_| {
                MessageError::InvalidJson("该行不是合法 UTF-8".to_string())
            })?;

            return match parse_one_line(&text) {
                Ok(msg) => Ok(Some(msg)),
                Err(e) => Err(e),
            };
        }
    }
}

/// 把单行文本解析为 `IncomingMessage`。抽出纯函数以便单测。
fn parse_one_line(text: &str) -> Result<IncomingMessage, MessageError> {
    let value: JsonValue = serde_json::from_str(text)
        .map_err(|e| MessageError::InvalidJson(format!("{}", e)))?;

    let obj = value.as_object().ok_or(MessageError::InvalidJsonRpcVersion)?;

    // jsonrpc 字段必须为 "2.0"。
    match obj.get("jsonrpc").and_then(|v| v.as_str()) {
        Some("2.0") => {}
        _ => return Err(MessageError::InvalidJsonRpcVersion),
    }

    let has_id = obj.contains_key("id");
    let has_method = obj.contains_key("method");
    let has_result = obj.contains_key("result");
    let has_error = obj.contains_key("error");

    // 分类规则：
    // - 若包含 result 或 error → Response
    // - 否则含 method + id → Request
    // - 否则含 method 无 id → Notification
    // - 否则非法
    let msg = if has_result || has_error {
        if has_result && has_error {
            return Err(MessageError::AmbiguousResponse);
        }
        let resp: JsonRpcResponse = serde_json::from_value(value)
            .map_err(|e| MessageError::Malformed(format!("解析响应失败：{}", e)))?;
        IncomingMessage::Response(resp)
    } else if has_method && has_id {
        let req: JsonRpcRequest = serde_json::from_value(value)
            .map_err(|e| MessageError::Malformed(format!("解析请求失败：{}", e)))?;
        IncomingMessage::Request(req)
    } else if has_method {
        let notif: JsonRpcNotification = serde_json::from_value(value)
            .map_err(|e| MessageError::Malformed(format!("解析通知失败：{}", e)))?;
        IncomingMessage::Notification(notif)
    } else {
        return Err(MessageError::Malformed(
            "既不是请求、响应也不是通知（缺少 method/result/error）".to_string(),
        ));
    };

    Ok(msg)
}

/// 便捷构造：把 `OutgoingMessage` 序列化并附上 `\n`，供 transport 直接写入子进程。
pub fn encode_line(msg: &OutgoingMessage) -> Result<Vec<u8>, MessageError> {
    let mut line = msg.to_line()?.into_bytes();
    line.push(b'\n');
    Ok(line)
}

// ──────────────────────── 单元测试 ────────────────────────
#[cfg(test)]
mod tests {
    use super::*;

    // ── 基础消息 round-trip ──────────────────────────────

    #[test]
    fn request_round_trip_with_params() {
        let req = JsonRpcRequest::new(
            1i64,
            "tools/call",
            serde_json::json!({"name":"ocr:recognize","arguments":{"lang":"zh-CN"}}),
        );
        let out = OutgoingMessage::Request(req.clone());
        let line = out.to_line().unwrap();
        assert!(!line.ends_with('\n'));
        let got = parse_one_line(&line).unwrap();
        match got {
            IncomingMessage::Request(r) => assert_eq!(r, req),
            _ => panic!("期望 Request"),
        }
    }

    #[test]
    fn request_empty_params_omitted_in_serialization() {
        // 与设计文档 §5.1 示例格式一致：params:{} 被省略，便于人眼可读。
        let req = JsonRpcRequest::new(1i64, "tools/list", serde_json::json!({}));
        let line = OutgoingMessage::Request(req).to_line().unwrap();
        assert!(!line.contains("params"));
    }

    #[test]
    fn response_success_round_trip() {
        let resp = JsonRpcResponse::success(2i64, serde_json::json!({"text":"你好世界","confidence":0.98}));
        let out = OutgoingMessage::Response(resp.clone());
        let line = out.to_line().unwrap();
        match parse_one_line(&line).unwrap() {
            IncomingMessage::Response(r) => {
                assert_eq!(r, resp);
                match r.body {
                    ResponseBody::Success { result } => {
                        assert_eq!(result["text"], "你好世界");
                    }
                    _ => panic!("期望 Success"),
                }
            }
            _ => panic!("期望 Response"),
        }
    }

    #[test]
    fn response_error_round_trip() {
        let err = JsonRpcError::new(JsonRpcError::CODE_PLUGIN_ERROR, "图片文件不存在");
        let resp = JsonRpcResponse::error(Some(RequestId::Int(2)), err.clone());
        let out = OutgoingMessage::Response(resp.clone());
        match parse_one_line(&out.to_line().unwrap()).unwrap() {
            IncomingMessage::Response(r) => match r.body {
                ResponseBody::Error { error } => assert_eq!(error, err),
                _ => panic!("期望 Error"),
            },
            _ => panic!("期望 Response"),
        }
    }

    #[test]
    fn notification_progress_round_trip() {
        // 设计文档 §5.4 示例。
        let notif = JsonRpcNotification::new(
            "notify/progress",
            serde_json::json!({"task_id":"abc","progress":50}),
        );
        let out = OutgoingMessage::Notification(notif.clone());
        let line = out.to_line().unwrap();
        match parse_one_line(&line).unwrap() {
            IncomingMessage::Notification(n) => {
                assert_eq!(n.method, "notify/progress");
                assert_eq!(n.params.as_ref().unwrap()["progress"], 50);
            }
            _ => panic!("期望 Notification"),
        }
    }

    // ── 非法/边界输入（parse_one_line）────────────────────

    #[test]
    fn parse_invalid_json_is_error() {
        let err = parse_one_line("this is not json").unwrap_err();
        assert!(matches!(err, MessageError::InvalidJson(_)));
    }

    #[test]
    fn parse_non_object_is_rejected() {
        let err = parse_one_line(r#"["a", "list"]"#).unwrap_err();
        assert!(matches!(err, MessageError::InvalidJsonRpcVersion));
    }

    #[test]
    fn parse_wrong_jsonrpc_version_is_rejected() {
        let err = parse_one_line(
            r#"{"jsonrpc":"1.0","id":1,"method":"foo"}"#,
        )
        .unwrap_err();
        assert!(matches!(err, MessageError::InvalidJsonRpcVersion));
    }

    #[test]
    fn parse_response_both_result_and_error_rejected() {
        let err = parse_one_line(
            r#"{"jsonrpc":"2.0","id":1,"result":1,"error":{"code":-1,"message":"x"}}"#,
        )
        .unwrap_err();
        assert!(matches!(err, MessageError::AmbiguousResponse));
    }

    #[test]
    fn parse_response_neither_result_nor_error_rejected() {
        let err = parse_one_line(r#"{"jsonrpc":"2.0","id":1}"#).unwrap_err();
        let MessageError::Malformed(txt) = err else {
            panic!("期望 Malformed，实际 {:?}", err);
        };
        // 没有 method/result/error，所以由分类规则报 Malformed，而不是反序列化错误。
        assert!(txt.contains("缺少 method/result/error"), "txt={:?}", txt);
    }

    #[test]
    fn parse_orphan_fields_malformed() {
        // 没有 method / result / error
        let err = parse_one_line(r#"{"jsonrpc":"2.0","foo":"bar"}"#).unwrap_err();
        assert!(matches!(err, MessageError::Malformed(_)));
    }

    #[test]
    fn request_with_string_id_is_accepted() {
        let req = JsonRpcRequest::new("abc-1".to_string(), "tools/list", serde_json::json!({}));
        let line = OutgoingMessage::Request(req.clone()).to_line().unwrap();
        match parse_one_line(&line).unwrap() {
            IncomingMessage::Request(r) => {
                assert_eq!(r.id, RequestId::Str("abc-1".to_string()));
                assert_eq!(r.method, "tools/list");
            }
            _ => panic!("期望 Request"),
        }
    }

    // ── LineCodec：粘包、半包、空行、非法行 ─────────────────

    #[test]
    fn linecodec_sticky_packet_two_messages_at_once() {
        let mut codec = LineCodec::new();
        let a = OutgoingMessage::Request(JsonRpcRequest::new(
            1i64,
            "tools/list",
            serde_json::json!({}),
        ));
        let b = OutgoingMessage::Request(JsonRpcRequest::new(
            2i64,
            "tools/call",
            serde_json::json!({"name":"echo"}),
        ));
        let mut bytes = encode_line(&a).unwrap();
        bytes.extend(encode_line(&b).unwrap());
        codec.feed(&bytes);

        let msg_a = codec.decode().unwrap().unwrap();
        let msg_b = codec.decode().unwrap().unwrap();
        let none = codec.decode().unwrap();

        assert_eq!(msg_a, IncomingMessage::Request(a.into_request().unwrap()));
        assert_eq!(msg_b, IncomingMessage::Request(b.into_request().unwrap()));
        assert!(none.is_none());
        assert_eq!(codec.buffered_len(), 0);
    }

    // 为测试便利，给 OutgoingMessage 加一个仅在测试里使用的拆箱辅助。
    impl OutgoingMessage {
        fn into_request(self) -> Option<JsonRpcRequest> {
            match self {
                OutgoingMessage::Request(r) => Some(r),
                _ => None,
            }
        }
    }

    #[test]
    fn linecodec_partial_packet_truncated_line() {
        let mut codec = LineCodec::new();
        let msg = OutgoingMessage::Request(JsonRpcRequest::new(
            1i64,
            "tools/list",
            serde_json::json!({}),
        ));
        let full = encode_line(&msg).unwrap();
        let split = full.len() - 4;
        // 先喂一半（不完整，无换行）
        codec.feed(&full[..split]);
        assert!(codec.decode().unwrap().is_none());
        assert_eq!(codec.buffered_len(), split);
        // 再喂剩余（含换行）
        codec.feed(&full[split..]);
        let got = codec.decode().unwrap().unwrap();
        match got {
            IncomingMessage::Request(r) => assert_eq!(r.method, "tools/list"),
            _ => panic!(),
        }
    }

    #[test]
    fn linecodec_skips_blank_lines() {
        let mut codec = LineCodec::new();
        let msg = OutgoingMessage::Request(JsonRpcRequest::new(
            1i64,
            "m",
            serde_json::json!({}),
        ));
        let bytes = encode_line(&msg).unwrap();
        let mut mixed = vec![];
        mixed.extend_from_slice(b"\n\n\r\n   \n"); // 多个空行，含 CRLF
        mixed.extend_from_slice(&bytes);
        codec.feed(&mixed);
        let got = codec.decode().unwrap().unwrap();
        assert!(matches!(got, IncomingMessage::Request(_)));
        assert!(codec.decode().unwrap().is_none());
    }

    #[test]
    fn linecodec_invalid_line_discards_only_that_line() {
        // 非法 JSON 行之后紧跟合法消息：非法行被报错但合法行仍然可解析。
        let mut codec = LineCodec::new();
        let bad = b"not a json line at all\n";
        let good = OutgoingMessage::Notification(JsonRpcNotification::new(
            "notify/stream",
            serde_json::json!({"delta":"hi"}),
        ));
        let mut bytes = bad.to_vec();
        bytes.extend(encode_line(&good).unwrap());
        codec.feed(&bytes);

        // 第一行必须是 InvalidJson 错误，不 panic
        let err = codec.decode().unwrap_err();
        assert!(matches!(err, MessageError::InvalidJson(_)));
        // 第二行正常解析
        let got = codec.decode().unwrap().unwrap();
        assert!(matches!(got, IncomingMessage::Notification(_)));
        assert!(codec.decode().unwrap().is_none());
    }

    #[test]
    fn linecodec_handles_crlf_line_endings() {
        let mut codec = LineCodec::new();
        codec.feed(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"m\"}\r\n");
        let got = codec.decode().unwrap().unwrap();
        match got {
            IncomingMessage::Request(r) => assert_eq!(r.method, "m"),
            _ => panic!(),
        }
    }

    #[test]
    fn linecodec_non_utf8_line_rejected_as_invalid_json() {
        let mut codec = LineCodec::new();
        // 非法 UTF-8 字节序列 + 换行
        codec.feed(&[0xFF, 0xFE, 0xFD, b'\n']);
        let err = codec.decode().unwrap_err();
        assert!(matches!(err, MessageError::InvalidJson(_)));
    }

    #[test]
    fn encode_line_appends_newline() {
        let msg = OutgoingMessage::Request(JsonRpcRequest::new(
            1i64,
            "m",
            serde_json::json!({}),
        ));
        let bytes = encode_line(&msg).unwrap();
        assert_eq!(bytes.last(), Some(&b'\n'));
    }

    // ── RequestId / From 实现校验 ────────────────────────

    #[test]
    fn request_id_from_int_and_str() {
        let a: RequestId = 42i64.into();
        let b: RequestId = "s".to_string().into();
        assert_eq!(a, RequestId::Int(42));
        assert_eq!(b, RequestId::Str("s".to_string()));
    }
}
