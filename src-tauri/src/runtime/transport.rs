//! `Transport` 抽象：屏蔽真实子进程与测试替身的差异。
//!
//! 本模块提供三样东西：
//!
//! 1. [`Transport`] trait —— 只有 `send` / `recv` / `close` 三个动作，
//!    Phase 4 的 `StdioTransport` 与本模块的 [`MockTransport`] 都实现它。
//! 2. [`PendingTable`] —— 请求-响应配对表：分配递增请求 ID、登记待响应槽位、
//!    收到响应后按 ID 唤醒等待方，连接断开时一次性让所有等待方失败。
//! 3. [`MockTransport`] —— 内存队列实现，可预置插件应答、可注入连接断开，
//!    使实例状态机在不启动任何进程的情况下被完整测试。

#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Mutex;

use async_trait::async_trait;
use serde_json::Value as JsonValue;
use thiserror::Error;
use tokio::sync::{mpsc, oneshot, Mutex as AsyncMutex};

use crate::protocol::message::{
    IncomingMessage, JsonRpcError, JsonRpcResponse, OutgoingMessage, RequestId, ResponseBody,
};

/// 传输层错误。
///
/// 刻意保持为「可克隆、可比较」的纯数据，方便在 `fail_all` 中广播给所有等待方，
/// 也方便单测直接用 `assert_eq!` 断言。
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TransportError {
    /// 连接已断开：对端退出、管道关闭，或宿主主动 `close`。
    #[error("传输通道已关闭")]
    Closed,

    /// 出站消息序列化失败。
    #[error("消息编码失败：{message}")]
    Encode { message: String },

    /// 底层 I/O 失败（Phase 4 的真实 stdio 会用到）。
    #[error("传输层 I/O 错误：{message}")]
    Io { message: String },

    /// 同一请求 ID 被重复登记——协议实现有 bug 时才会出现。
    #[error("请求 ID 重复登记：{id}")]
    DuplicateRequestId { id: String },
}

/// 双向消息通道。
///
/// 全部方法都取 `&self`，因此可以放进 `Arc` 被状态机与读循环共享；
/// 内部可变性由各实现自己解决。
#[async_trait]
pub trait Transport: Send + Sync {
    /// 发送一条出站消息。通道已关闭时返回 [`TransportError::Closed`]。
    async fn send(&self, msg: OutgoingMessage) -> Result<(), TransportError>;

    /// 阻塞等待下一条入站消息。通道已关闭时返回 [`TransportError::Closed`]。
    ///
    /// 没有消息且未断开时应当一直挂起，这样超时测试才能用虚拟时钟推进。
    async fn recv(&self) -> Result<IncomingMessage, TransportError>;

    /// 主动关闭通道。允许重复调用。
    async fn close(&self);
}

/// 一次请求的最终结果：要么拿到 `result`，要么拿到 JSON-RPC 错误对象。
pub type PendingOutcome = Result<JsonValue, JsonRpcError>;

#[derive(Debug, Default)]
struct PendingState {
    entries: HashMap<RequestId, oneshot::Sender<PendingOutcome>>,
    closed: bool,
}

/// 请求-响应配对表。
///
/// 用法：`next_id()` 取 ID → `register(id)` 拿到 `oneshot::Receiver` →
/// 把请求发出去 → `await` 那个 receiver。读循环收到响应后调用 `complete`
/// 按 ID 唤醒等待方；连接断开时调用 `fail_all` 让所有等待方一起失败。
#[derive(Debug)]
pub struct PendingTable {
    next_id: AtomicI64,
    state: Mutex<PendingState>,
}

impl Default for PendingTable {
    fn default() -> Self {
        Self::new()
    }
}

impl PendingTable {
    /// 创建空表，请求 ID 从 1 开始。
    pub fn new() -> Self {
        Self {
            next_id: AtomicI64::new(1),
            state: Mutex::new(PendingState::default()),
        }
    }

    /// 分配一个全新的递增请求 ID。
    pub fn next_id(&self) -> RequestId {
        RequestId::Int(self.next_id.fetch_add(1, Ordering::Relaxed))
    }

    /// 登记一个待响应槽位，返回用于等待结果的 receiver。
    pub fn register(
        &self,
        id: RequestId,
    ) -> Result<oneshot::Receiver<PendingOutcome>, TransportError> {
        let mut state = self.lock();
        if state.closed {
            return Err(TransportError::Closed);
        }
        if state.entries.contains_key(&id) {
            return Err(TransportError::DuplicateRequestId {
                id: format!("{id:?}"),
            });
        }
        let (tx, rx) = oneshot::channel();
        state.entries.insert(id, tx);
        Ok(rx)
    }

    /// 用一条响应唤醒对应的等待方。
    ///
    /// 返回 `false` 表示这条响应无人认领：ID 缺失（如解析错误响应）、
    /// ID 未登记，或等待方已因超时取消。
    pub fn complete(&self, response: JsonRpcResponse) -> bool {
        let Some(id) = response.id else {
            return false;
        };
        let sender = {
            let mut state = self.lock();
            state.entries.remove(&id)
        };
        let Some(sender) = sender else {
            return false;
        };
        let outcome = match response.body {
            ResponseBody::Success { result } => Ok(result),
            ResponseBody::Error { error } => Err(error),
        };
        // 等待方可能已经放弃（receiver 被 drop），此时发送失败属正常情况。
        sender.send(outcome).is_ok()
    }

    /// 撤销一个待响应槽位（请求超时后调用），返回是否确实撤下了一项。
    pub fn cancel(&self, id: &RequestId) -> bool {
        self.lock().entries.remove(id).is_some()
    }

    /// 连接断开：让所有待响应请求以同一个错误结束，并封闭本表。
    ///
    /// 返回被唤醒的请求数量。
    pub fn fail_all(&self, error: JsonRpcError) -> usize {
        let drained: Vec<_> = {
            let mut state = self.lock();
            state.closed = true;
            state.entries.drain().map(|(_, tx)| tx).collect()
        };
        let count = drained.len();
        for tx in drained {
            let _ = tx.send(Err(error.clone()));
        }
        count
    }

    /// 当前待响应请求数量。
    pub fn pending_len(&self) -> usize {
        self.lock().entries.len()
    }

    /// 本表是否已因断开而封闭。
    pub fn is_closed(&self) -> bool {
        self.lock().closed
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, PendingState> {
        // 互斥区内不会 await、不会 panic，中毒锁直接取回内层数据即可。
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// [`MockTransport`] 的入站事件：一条消息，或一次断开注入。
#[derive(Debug, Clone, PartialEq)]
enum MockEvent {
    Message(Box<IncomingMessage>),
    Disconnect,
}

/// 内存队列传输替身。
///
/// - `push_incoming` 预置插件将要发回的消息，`recv` 按 FIFO 顺序取出；
/// - 队列空且未断开时 `recv` 一直挂起，配合 `tokio::time::pause()` 测超时；
/// - `disconnect` 在队列尾部注入断开事件，之前排队的消息仍会被先读到；
/// - 宿主发出的消息全部留痕，可用 `sent_messages` / `next_sent` 断言。
#[derive(Debug)]
pub struct MockTransport {
    inbound_tx: mpsc::UnboundedSender<MockEvent>,
    inbound_rx: AsyncMutex<mpsc::UnboundedReceiver<MockEvent>>,
    sent: Mutex<Vec<OutgoingMessage>>,
    sent_tx: mpsc::UnboundedSender<OutgoingMessage>,
    sent_rx: AsyncMutex<mpsc::UnboundedReceiver<OutgoingMessage>>,
    closed: Mutex<bool>,
}

impl Default for MockTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl MockTransport {
    /// 创建一个空的替身通道。
    pub fn new() -> Self {
        let (inbound_tx, inbound_rx) = mpsc::unbounded_channel();
        let (sent_tx, sent_rx) = mpsc::unbounded_channel();
        Self {
            inbound_tx,
            inbound_rx: AsyncMutex::new(inbound_rx),
            sent: Mutex::new(Vec::new()),
            sent_tx,
            sent_rx: AsyncMutex::new(sent_rx),
            closed: Mutex::new(false),
        }
    }

    /// 预置一条「插件发给宿主」的消息。
    pub fn push_incoming(&self, msg: IncomingMessage) {
        let _ = self.inbound_tx.send(MockEvent::Message(Box::new(msg)));
    }

    /// 注入一次连接断开。排在它之前的消息仍可被读到。
    pub fn disconnect(&self) {
        let _ = self.inbound_tx.send(MockEvent::Disconnect);
    }

    /// 宿主迄今发出的全部消息（累积，不清空）。
    pub fn sent_messages(&self) -> Vec<OutgoingMessage> {
        self.lock_sent().clone()
    }

    /// 等待宿主发出的下一条消息；通道彻底关闭时返回 `None`。
    pub async fn next_sent(&self) -> Option<OutgoingMessage> {
        self.sent_rx.lock().await.recv().await
    }

    /// 通道是否已关闭。
    pub fn is_closed(&self) -> bool {
        *self.lock_closed()
    }

    fn lock_sent(&self) -> std::sync::MutexGuard<'_, Vec<OutgoingMessage>> {
        self.sent.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn lock_closed(&self) -> std::sync::MutexGuard<'_, bool> {
        self.closed.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[async_trait]
impl Transport for MockTransport {
    async fn send(&self, msg: OutgoingMessage) -> Result<(), TransportError> {
        if self.is_closed() {
            return Err(TransportError::Closed);
        }
        self.lock_sent().push(msg.clone());
        let _ = self.sent_tx.send(msg);
        Ok(())
    }

    async fn recv(&self) -> Result<IncomingMessage, TransportError> {
        if self.is_closed() {
            return Err(TransportError::Closed);
        }
        let mut rx = self.inbound_rx.lock().await;
        match rx.recv().await {
            Some(MockEvent::Message(msg)) => Ok(*msg),
            Some(MockEvent::Disconnect) | None => {
                *self.lock_closed() = true;
                Err(TransportError::Closed)
            }
        }
    }

    async fn close(&self) {
        *self.lock_closed() = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::message::{JsonRpcNotification, JsonRpcRequest};
    use serde_json::json;

    fn notification(method: &str) -> IncomingMessage {
        IncomingMessage::Notification(JsonRpcNotification::new(method, json!({})))
    }

    #[test]
    fn 请求_id_从_1_开始递增() {
        let table = PendingTable::new();
        assert_eq!(table.next_id(), RequestId::Int(1));
        assert_eq!(table.next_id(), RequestId::Int(2));
        assert_eq!(table.next_id(), RequestId::Int(3));
    }

    #[tokio::test]
    async fn 成功响应按_id_唤醒等待方() {
        let table = PendingTable::new();
        let id = table.next_id();
        let rx = table.register(id.clone()).expect("登记应成功");
        assert_eq!(table.pending_len(), 1);

        assert!(table.complete(JsonRpcResponse::success(id, json!({"ok": true}))));
        assert_eq!(table.pending_len(), 0);
        assert_eq!(rx.await.expect("应被唤醒"), Ok(json!({"ok": true})));
    }

    #[tokio::test]
    async fn 错误响应按_id_唤醒等待方() {
        let table = PendingTable::new();
        let id = table.next_id();
        let rx = table.register(id.clone()).expect("登记应成功");

        let err = JsonRpcError::new(JsonRpcError::CODE_TOOL_NOT_FOUND, "无此工具");
        assert!(table.complete(JsonRpcResponse::error(Some(id), err.clone())));
        assert_eq!(rx.await.expect("应被唤醒"), Err(err));
    }

    #[test]
    fn 只唤醒对应_id_其他请求保持等待() {
        let table = PendingTable::new();
        let first = table.next_id();
        let second = table.next_id();
        let _rx1 = table.register(first).expect("登记应成功");
        let _rx2 = table.register(second.clone()).expect("登记应成功");

        assert!(table.complete(JsonRpcResponse::success(second, json!(1))));
        assert_eq!(table.pending_len(), 1);
    }

    #[test]
    fn 未登记的_id_无人认领() {
        let table = PendingTable::new();
        assert!(!table.complete(JsonRpcResponse::success(99_i64, json!(null))));
    }

    #[test]
    fn 缺少_id_的响应无人认领() {
        let table = PendingTable::new();
        let id = table.next_id();
        let _rx = table.register(id).expect("登记应成功");

        let err = JsonRpcError::new(JsonRpcError::CODE_PARSE_ERROR, "解析失败");
        assert!(!table.complete(JsonRpcResponse::error(None, err)));
        assert_eq!(table.pending_len(), 1);
    }

    #[test]
    fn 撤销后再来的响应无人认领() {
        let table = PendingTable::new();
        let id = table.next_id();
        let _rx = table.register(id.clone()).expect("登记应成功");

        assert!(table.cancel(&id));
        assert!(!table.cancel(&id));
        assert!(!table.complete(JsonRpcResponse::success(id, json!(null))));
    }

    #[tokio::test]
    async fn 断开时所有待响应请求一起失败() {
        let table = PendingTable::new();
        let mut receivers = Vec::new();
        for _ in 0..3 {
            let id = table.next_id();
            receivers.push(table.register(id).expect("登记应成功"));
        }

        let err = JsonRpcError::new(JsonRpcError::CODE_INTERNAL_ERROR, "连接断开");
        assert_eq!(table.fail_all(err.clone()), 3);
        assert_eq!(table.pending_len(), 0);
        assert!(table.is_closed());

        for rx in receivers {
            assert_eq!(rx.await.expect("应被唤醒"), Err(err.clone()));
        }
    }

    #[test]
    fn 封闭后不再接受新的登记() {
        let table = PendingTable::new();
        table.fail_all(JsonRpcError::new(
            JsonRpcError::CODE_INTERNAL_ERROR,
            "连接断开",
        ));
        let id = table.next_id();
        assert_eq!(
            table.register(id).unwrap_err(),
            TransportError::Closed
        );
    }

    #[test]
    fn 重复登记同一_id_被拒绝() {
        let table = PendingTable::new();
        let id = RequestId::Int(7);
        let _rx = table.register(id.clone()).expect("首次登记应成功");
        assert_eq!(
            table.register(id).unwrap_err(),
            TransportError::DuplicateRequestId {
                id: "Int(7)".to_string()
            }
        );
    }

    #[test]
    fn 等待方放弃后响应不再计入唤醒() {
        let table = PendingTable::new();
        let id = table.next_id();
        let rx = table.register(id.clone()).expect("登记应成功");
        drop(rx);

        assert!(!table.complete(JsonRpcResponse::success(id, json!(null))));
        assert_eq!(table.pending_len(), 0);
    }

    #[tokio::test]
    async fn 替身记录宿主发出的消息() {
        let transport = MockTransport::new();
        let msg = OutgoingMessage::Request(JsonRpcRequest::new(1_i64, "tools/list", json!({})));
        transport.send(msg.clone()).await.expect("发送应成功");

        assert_eq!(transport.sent_messages(), vec![msg.clone()]);
        assert_eq!(transport.next_sent().await, Some(msg));
    }

    #[tokio::test]
    async fn 替身按先进先出返回预置消息() {
        let transport = MockTransport::new();
        transport.push_incoming(notification("plugin/hello"));
        transport.push_incoming(notification("notify/progress"));

        assert_eq!(
            transport.recv().await.expect("应取到消息"),
            notification("plugin/hello")
        );
        assert_eq!(
            transport.recv().await.expect("应取到消息"),
            notification("notify/progress")
        );
    }

    #[tokio::test]
    async fn 断开注入前排队的消息仍可读到() {
        let transport = MockTransport::new();
        transport.push_incoming(notification("plugin/hello"));
        transport.disconnect();

        assert_eq!(
            transport.recv().await.expect("应取到消息"),
            notification("plugin/hello")
        );
        assert_eq!(transport.recv().await, Err(TransportError::Closed));
        assert!(transport.is_closed());
    }

    #[tokio::test]
    async fn 断开后收发一律失败() {
        let transport = MockTransport::new();
        transport.disconnect();
        assert_eq!(transport.recv().await, Err(TransportError::Closed));

        let msg = OutgoingMessage::Notification(JsonRpcNotification::new("notify/x", json!({})));
        assert_eq!(transport.send(msg).await, Err(TransportError::Closed));
        assert_eq!(transport.recv().await, Err(TransportError::Closed));
    }

    #[tokio::test]
    async fn 主动关闭后收发一律失败() {
        let transport = MockTransport::new();
        transport.push_incoming(notification("plugin/hello"));
        transport.close().await;
        transport.close().await;

        assert_eq!(transport.recv().await, Err(TransportError::Closed));
        let msg = OutgoingMessage::Notification(JsonRpcNotification::new("notify/x", json!({})));
        assert_eq!(transport.send(msg).await, Err(TransportError::Closed));
    }

    #[tokio::test]
    async fn 队列为空时接收保持挂起() {
        let transport = MockTransport::new();
        let pending = tokio::time::timeout(
            std::time::Duration::from_millis(20),
            transport.recv(),
        )
        .await;
        assert!(pending.is_err(), "无消息且未断开时不应返回");
    }
}
