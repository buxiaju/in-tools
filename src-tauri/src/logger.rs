//! 日志模块：统一的日志记录和管理。
//!
//! 提供：
//! - 结构化日志记录
//! - 日志级别管理
//! - 日志文件轮转
//! - 性能日志

use std::sync::Arc;
use std::sync::Mutex;
use std::collections::VecDeque;

use serde::{Deserialize, Serialize};
use chrono::{DateTime, Utc};


/// 日志级别。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

impl LogLevel {
    pub fn as_str(&self) -> &'static str {
        match self {
            LogLevel::Trace => "TRACE",
            LogLevel::Debug => "DEBUG",
            LogLevel::Info => "INFO",
            LogLevel::Warn => "WARN",
            LogLevel::Error => "ERROR",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_uppercase().as_str() {
            "TRACE" => Some(LogLevel::Trace),
            "DEBUG" => Some(LogLevel::Debug),
            "INFO" => Some(LogLevel::Info),
            "WARN" => Some(LogLevel::Warn),
            "ERROR" => Some(LogLevel::Error),
            _ => None,
        }
    }
}

/// 日志条目。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    /// 时间戳。
    pub timestamp: DateTime<Utc>,
    /// 日志级别。
    pub level: LogLevel,
    /// 目标模块。
    pub target: String,
    /// 消息。
    pub message: String,
    /// 字段。
    pub fields: Vec<(String, String)>,
}

impl LogEntry {
    /// 创建新的日志条目。
    pub fn new(level: LogLevel, target: &str, message: &str) -> Self {
        Self {
            timestamp: Utc::now(),
            level,
            target: target.to_string(),
            message: message.to_string(),
            fields: Vec::new(),
        }
    }

    /// 添加字段。
    pub fn with_field(mut self, key: &str, value: &str) -> Self {
        self.fields.push((key.to_string(), value.to_string()));
        self
    }

    /// 格式化为字符串。
    pub fn format(&self) -> String {
        let mut result = format!(
            "[{}] {} {} - {}",
            self.timestamp.to_rfc3339(),
            self.level.as_str(),
            self.target,
            self.message
        );

        for (key, value) in &self.fields {
            result.push_str(&format!(" {}={}", key, value));
        }

        result
    }
}

/// 日志存储。
pub struct LogStore {
    /// 内存中的日志条目。
    entries: Arc<Mutex<VecDeque<LogEntry>>>,
    /// 最大条目数。
    max_entries: usize,
    /// 日志文件路径。
    file_path: Option<std::path::PathBuf>,
}

impl LogStore {
    /// 创建新的日志存储。
    pub fn new(max_entries: usize) -> Self {
        Self {
            entries: Arc::new(Mutex::new(VecDeque::with_capacity(max_entries))),
            max_entries,
            file_path: None,
        }
    }

    /// 设置日志文件路径。
    pub fn with_file(mut self, path: impl Into<std::path::PathBuf>) -> Self {
        self.file_path = Some(path.into());
        self
    }

    /// 添加日志条目。
    pub fn add(&self, entry: LogEntry) {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        entries.push_back(entry.clone());

        // 限制大小
        while entries.len() > self.max_entries {
            entries.pop_front();
        }

        // 写入文件
        if let Some(path) = &self.file_path {
            if let Ok(formatted) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .and_then(|mut file| {
                    use std::io::Write;
                    writeln!(file, "{}", entry.format())?;
                    Ok(())
                })
            {
                let _ = formatted;
            }
        }
    }

    /// 获取所有日志条目。
    pub fn all(&self) -> Vec<LogEntry> {
        self.entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .cloned()
            .collect()
    }

    /// 获取最近 N 条日志。
    pub fn recent(&self, n: usize) -> Vec<LogEntry> {
        self.entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .rev()
            .take(n)
            .cloned()
            .collect()
    }

    /// 按级别过滤。
    pub fn by_level(&self, level: LogLevel) -> Vec<LogEntry> {
        self.entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|e| e.level == level)
            .cloned()
            .collect()
    }

    /// 清空日志。
    pub fn clear(&self) {
        self.entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }

    /// 获取日志数量。
    pub fn count(&self) -> usize {
        self.entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .len()
    }
}

/// 性能监控器。
pub struct PerformanceMonitor {
    /// 计时器。
    timers: Arc<Mutex<std::collections::HashMap<String, std::time::Instant>>>,
    /// 性能指标。
    metrics: Arc<Mutex<Vec<PerformanceMetric>>>,
}

/// 性能指标。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerformanceMetric {
    /// 操作名称。
    pub name: String,
    /// 持续时间（毫秒）。
    pub duration_ms: u64,
    /// 时间戳。
    pub timestamp: DateTime<Utc>,
}

impl PerformanceMonitor {
    /// 创建新的性能监控器。
    pub fn new() -> Self {
        Self {
            timers: Arc::new(Mutex::new(std::collections::HashMap::new())),
            metrics: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// 开始计时。
    pub fn start(&self, name: &str) {
        let mut timers = self.timers.lock().unwrap_or_else(|e| e.into_inner());
        timers.insert(name.to_string(), std::time::Instant::now());
    }

    /// 结束计时并记录。
    pub fn stop(&self, name: &str) -> u64 {
        let mut timers = self.timers.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(start) = timers.remove(name) {
            let duration = start.elapsed().as_millis() as u64;
            let metric = PerformanceMetric {
                name: name.to_string(),
                duration_ms: duration,
                timestamp: Utc::now(),
            };
            self.metrics.lock().unwrap_or_else(|e| e.into_inner()).push(metric);
            duration
        } else {
            0
        }
    }

    /// 获取所有指标。
    pub fn all_metrics(&self) -> Vec<PerformanceMetric> {
        self.metrics
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// 获取平均耗时。
    pub fn average_duration(&self, name: &str) -> Option<f64> {
        let metrics = self.metrics.lock().unwrap_or_else(|e| e.into_inner());
        let filtered: Vec<u64> = metrics
            .iter()
            .filter(|m| m.name == name)
            .map(|m| m.duration_ms)
            .collect();

        if filtered.is_empty() {
            None
        } else {
            Some(filtered.iter().sum::<u64>() as f64 / filtered.len() as f64)
        }
    }
}

impl Default for PerformanceMonitor {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_log_entry_format() {
        let entry = LogEntry::new(LogLevel::Info, "test", "test message")
            .with_field("key1", "value1");
        let formatted = entry.format();
        assert!(formatted.contains("INFO"));
        assert!(formatted.contains("test"));
        assert!(formatted.contains("test message"));
        assert!(formatted.contains("key1=value1"));
    }

    #[test]
    fn test_log_store() {
        let store = LogStore::new(10);
        store.add(LogEntry::new(LogLevel::Info, "test", "msg1"));
        store.add(LogEntry::new(LogLevel::Error, "test", "msg2"));

        assert_eq!(store.count(), 2);
        assert_eq!(store.by_level(LogLevel::Error).len(), 1);
    }

    #[test]
    fn test_performance_monitor() {
        let monitor = PerformanceMonitor::new();
        monitor.start("op1");
        std::thread::sleep(std::time::Duration::from_millis(10));
        let duration = monitor.stop("op1");

        assert!(duration >= 10);
        assert_eq!(monitor.all_metrics().len(), 1);
    }
}
