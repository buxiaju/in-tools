//! 增强审计系统：更详细的调用链追踪和安全事件记录。
//!
//! 提供以下功能：
//! - 调用链追踪（支持嵌套调用）
//! - 安全事件记录
//! - 性能统计
//! - 异常检测

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

/// 调用链节点。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallChainNode {
    /// 调用 ID。
    pub call_id: String,
    /// 父调用 ID（如果是顶层调用则为 None）。
    pub parent_call_id: Option<String>,
    /// 调用方身份。
    pub caller: String,
    /// 工具名称。
    pub tool: String,
    /// 插件 ID。
    pub plugin_id: String,
    /// 参数摘要。
    pub args_summary: String,
    /// 开始时间。
    pub started_at: String,
    /// 结束时间（如果已完成）。
    pub completed_at: Option<String>,
    /// 持续时间（毫秒）。
    pub duration_ms: Option<u64>,
    /// 调用结果。
    pub outcome: Option<String>,
    /// 子调用列表。
    pub children: Vec<CallChainNode>,
}

impl CallChainNode {
    /// 创建新的调用链节点。
    pub fn new(
        call_id: String,
        parent_call_id: Option<String>,
        caller: String,
        tool: String,
        plugin_id: String,
        args_summary: String,
    ) -> Self {
        Self {
            call_id,
            parent_call_id,
            caller,
            tool,
            plugin_id,
            args_summary,
            started_at: chrono::Utc::now().to_rfc3339(),
            completed_at: None,
            duration_ms: None,
            outcome: None,
            children: Vec::new(),
        }
    }

    /// 标记调用完成。
    pub fn complete(&mut self, outcome: String, duration_ms: u64) {
        self.completed_at = Some(chrono::Utc::now().to_rfc3339());
        self.duration_ms = Some(duration_ms);
        self.outcome = Some(outcome);
    }

    /// 添加子调用。
    pub fn add_child(&mut self, child: CallChainNode) {
        self.children.push(child);
    }
}

/// 安全事件类型。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SecurityEventType {
    /// 权限拒绝。
    PermissionDenied,
    /// 网络访问被阻止。
    NetworkAccessBlocked,
    /// 资源使用超限。
    ResourceLimitExceeded,
    /// 可疑调用模式。
    SuspiciousCallPattern,
    /// 调用链过深。
    CallChainTooDeep,
    /// 调用频率过高。
    HighCallFrequency,
    /// 参数异常。
    AnomalousParameters,
}

/// 安全事件。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityEvent {
    /// 事件类型。
    pub event_type: SecurityEventType,
    /// 事件时间。
    pub timestamp: String,
    /// 相关插件 ID。
    pub plugin_id: String,
    /// 相关工具名称。
    pub tool: Option<String>,
    /// 事件描述。
    pub description: String,
    /// 严重程度（1-10）。
    pub severity: u8,
    /// 相关调用 ID。
    pub call_id: Option<String>,
    /// 额外数据。
    pub metadata: HashMap<String, String>,
}

impl SecurityEvent {
    /// 创建新的安全事件。
    pub fn new(
        event_type: SecurityEventType,
        plugin_id: String,
        description: String,
        severity: u8,
    ) -> Self {
        Self {
            event_type,
            timestamp: chrono::Utc::now().to_rfc3339(),
            plugin_id,
            tool: None,
            description,
            severity,
            call_id: None,
            metadata: HashMap::new(),
        }
    }

    /// 设置相关工具。
    pub fn with_tool(mut self, tool: String) -> Self {
        self.tool = Some(tool);
        self
    }

    /// 设置相关调用 ID。
    pub fn with_call_id(mut self, call_id: String) -> Self {
        self.call_id = Some(call_id);
        self
    }

    /// 添加元数据。
    pub fn with_metadata(mut self, key: String, value: String) -> Self {
        self.metadata.insert(key, value);
        self
    }
}

/// 增强审计记录。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnhancedAuditEntry {
    /// 调用 ID。
    pub call_id: String,
    /// 调用方身份。
    pub caller: String,
    /// 工具名称。
    pub tool: String,
    /// 插件 ID。
    pub plugin_id: String,
    /// 参数摘要。
    pub args_summary: String,
    /// 开始时间。
    pub started_at: String,
    /// 结束时间。
    pub completed_at: String,
    /// 持续时间（毫秒）。
    pub duration_ms: u64,
    /// 调用结果。
    pub outcome: String,
    /// 是否成功。
    pub success: bool,
    /// 调用链深度。
    pub chain_depth: u8,
    /// 父调用 ID。
    pub parent_call_id: Option<String>,
    /// 安全事件（如果有）。
    pub security_events: Vec<SecurityEvent>,
}

/// 增强审计系统。
pub struct EnhancedAuditSystem {
    /// 活跃的调用链。
    active_chains: Arc<Mutex<HashMap<String, CallChainNode>>>,
    /// 完成的审计记录。
    completed_entries: Arc<Mutex<Vec<EnhancedAuditEntry>>>,
    /// 安全事件。
    security_events: Arc<Mutex<Vec<SecurityEvent>>>,
    /// 调用统计。
    call_stats: Arc<Mutex<CallStats>>,
}

/// 调用统计。
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct CallStats {
    /// 总调用次数。
    pub total_calls: u64,
    /// 成功调用次数。
    pub successful_calls: u64,
    /// 失败调用次数。
    pub failed_calls: u64,
    /// 总持续时间（毫秒）。
    pub total_duration_ms: u64,
    /// 按工具分类的调用次数。
    pub calls_by_tool: HashMap<String, u64>,
    /// 按插件分类的调用次数。
    pub calls_by_plugin: HashMap<String, u64>,
    /// 最大调用链深度。
    pub max_chain_depth: u8,
    /// 安全事件计数。
    pub security_event_count: u64,
}

impl EnhancedAuditSystem {
    /// 创建新的增强审计系统。
    pub fn new() -> Self {
        Self {
            active_chains: Arc::new(Mutex::new(HashMap::new())),
            completed_entries: Arc::new(Mutex::new(Vec::new())),
            security_events: Arc::new(Mutex::new(Vec::new())),
            call_stats: Arc::new(Mutex::new(CallStats::default())),
        }
    }

    /// 开始记录调用。
    pub async fn start_call(
        &self,
        call_id: String,
        parent_call_id: Option<String>,
        caller: String,
        tool: String,
        plugin_id: String,
        args_summary: String,
    ) -> CallChainNode {
        let node = CallChainNode::new(
            call_id.clone(),
            parent_call_id,
            caller,
            tool,
            plugin_id,
            args_summary,
        );

        let mut chains = self.active_chains.lock().await;
        chains.insert(call_id.clone(), node.clone());

        // 更新统计
        let mut stats = self.call_stats.lock().await;
        stats.total_calls += 1;
        *stats.calls_by_tool.entry(node.tool.clone()).or_insert(0) += 1;
        *stats.calls_by_plugin.entry(node.plugin_id.clone()).or_insert(0) += 1;

        node
    }

    /// 完成调用。
    pub async fn complete_call(
        &self,
        call_id: &str,
        outcome: String,
        duration_ms: u64,
        success: bool,
    ) {
        let mut chains = self.active_chains.lock().await;
        if let Some(mut node) = chains.remove(call_id) {
            node.complete(outcome.clone(), duration_ms);

            // 创建审计记录
            let entry = EnhancedAuditEntry {
                call_id: call_id.to_string(),
                caller: node.caller.clone(),
                tool: node.tool.clone(),
                plugin_id: node.plugin_id.clone(),
                args_summary: node.args_summary.clone(),
                started_at: node.started_at.clone(),
                completed_at: node.completed_at.clone().unwrap_or_default(),
                duration_ms,
                outcome,
                success,
                chain_depth: self.calculate_depth(&node),
                parent_call_id: node.parent_call_id.clone(),
                security_events: Vec::new(),
            };

            // 保存审计记录
            let mut entries = self.completed_entries.lock().await;
            entries.push(entry);

            // 更新统计
            let mut stats = self.call_stats.lock().await;
            if success {
                stats.successful_calls += 1;
            } else {
                stats.failed_calls += 1;
            }
            stats.total_duration_ms += duration_ms;

            let depth = self.calculate_depth(&node);
            if depth > stats.max_chain_depth {
                stats.max_chain_depth = depth;
            }
        }
    }

    /// 记录安全事件。
    pub async fn record_security_event(&self, event: SecurityEvent) {
        let mut events = self.security_events.lock().await;
        events.push(event);

        // 更新统计
        let mut stats = self.call_stats.lock().await;
        stats.security_event_count += 1;
    }

    /// 获取调用统计。
    pub async fn get_stats(&self) -> CallStats {
        self.call_stats.lock().await.clone()
    }

    /// 获取最近的安全事件。
    pub async fn get_recent_security_events(&self, limit: usize) -> Vec<SecurityEvent> {
        let events = self.security_events.lock().await;
        events.iter().rev().take(limit).cloned().collect()
    }

    /// 获取活跃的调用链。
    pub async fn get_active_chains(&self) -> Vec<CallChainNode> {
        let chains = self.active_chains.lock().await;
        chains.values().cloned().collect()
    }

    /// 计算调用链深度。
    fn calculate_depth(&self, node: &CallChainNode) -> u8 {
        let mut depth = 0;
        let mut current = node.parent_call_id.clone();

        while let Some(_parent_id) = current {
            depth += 1;
            // 这里简化处理，实际实现需要递归查找
            // 为了避免死循环，设置最大深度限制
            if depth > 10 {
                break;
            }
            current = None; // 简化：不再向上查找
        }

        depth
    }

    /// 检查可疑调用模式。
    pub async fn detect_suspicious_patterns(&self) -> Vec<SecurityEvent> {
        let mut events = Vec::new();
        let stats = self.call_stats.lock().await;

        // 检查调用频率
        if stats.total_calls > 1000 {
            events.push(SecurityEvent::new(
                SecurityEventType::HighCallFrequency,
                "system".to_string(),
                format!("调用次数过多：{}", stats.total_calls),
                5,
            ));
        }

        // 检查失败率
        if stats.total_calls > 0 {
            let failure_rate = stats.failed_calls as f64 / stats.total_calls as f64;
            if failure_rate > 0.5 {
                events.push(SecurityEvent::new(
                    SecurityEventType::SuspiciousCallPattern,
                    "system".to_string(),
                    format!("失败率过高：{:.1}%", failure_rate * 100.0),
                    6,
                ));
            }
        }

        // 检查调用链深度
        if stats.max_chain_depth > 5 {
            events.push(SecurityEvent::new(
                SecurityEventType::CallChainTooDeep,
                "system".to_string(),
                format!("调用链过深：{}", stats.max_chain_depth),
                4,
            ));
        }

        events
    }
}

impl Default for EnhancedAuditSystem {
    fn default() -> Self {
        Self::new()
    }
}

/// 增强审计系统错误。
#[derive(Debug, thiserror::Error)]
pub enum EnhancedAuditError {
    /// 记录失败。
    #[error("审计记录失败：{0}")]
    RecordFailed(String),

    /// 查询失败。
    #[error("审计查询失败：{0}")]
    QueryFailed(String),
}
