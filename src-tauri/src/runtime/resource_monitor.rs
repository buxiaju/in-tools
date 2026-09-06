//! 资源监控器：跟踪和限制插件进程的资源使用。
//!
//! 提供以下功能：
//! - 跟踪插件进程的内存、CPU、磁盘和网络使用
//! - 根据 manifest 中的 `resource_limits` 配置限制资源使用
//! - 提供资源使用统计信息
//! - 在资源超限时发出警告或终止进程

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::protocol::manifest::ResourceLimits;

/// 资源使用统计。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ResourceUsage {
    /// 内存使用量（字节）。
    pub memory_bytes: u64,
    /// CPU 使用率（百分比，0-100）。
    pub cpu_percent: f32,
    /// 磁盘使用量（字节）。
    pub disk_bytes: u64,
    /// 网络使用量（字节/秒）。
    pub network_bytes_per_sec: u64,
    /// 最后更新时间。
    pub last_updated: Option<String>,
}

impl ResourceUsage {
    /// 检查是否超出资源限制。
    pub fn exceeds_limits(&self, limits: &ResourceLimits) -> Vec<ResourceViolation> {
        let mut violations = Vec::new();

        if limits.max_memory_mb > 0 {
            let max_bytes = (limits.max_memory_mb as u64) * 1024 * 1024;
            if self.memory_bytes > max_bytes {
                violations.push(ResourceViolation::MemoryExceeded {
                    used: self.memory_bytes,
                    limit: max_bytes,
                });
            }
        }

        if limits.max_cpu_percent > 0 {
            if self.cpu_percent > limits.max_cpu_percent as f32 {
                violations.push(ResourceViolation::CpuExceeded {
                    used: self.cpu_percent,
                    limit: limits.max_cpu_percent as f32,
                });
            }
        }

        if limits.max_disk_mb > 0 {
            let max_bytes = (limits.max_disk_mb as u64) * 1024 * 1024;
            if self.disk_bytes > max_bytes {
                violations.push(ResourceViolation::DiskExceeded {
                    used: self.disk_bytes,
                    limit: max_bytes,
                });
            }
        }

        if limits.max_network_kbps > 0 {
            let max_bytes_per_sec = (limits.max_network_kbps as u64) * 1024;
            if self.network_bytes_per_sec > max_bytes_per_sec {
                violations.push(ResourceViolation::NetworkExceeded {
                    used: self.network_bytes_per_sec,
                    limit: max_bytes_per_sec,
                });
            }
        }

        violations
    }
}

/// 资源违规类型。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ResourceViolation {
    /// 内存使用超限。
    MemoryExceeded { used: u64, limit: u64 },
    /// CPU 使用超限。
    CpuExceeded { used: f32, limit: f32 },
    /// 磁盘使用超限。
    DiskExceeded { used: u64, limit: u64 },
    /// 网络使用超限。
    NetworkExceeded { used: u64, limit: u64 },
}

impl std::fmt::Display for ResourceViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ResourceViolation::MemoryExceeded { used, limit } => {
                write!(
                    f,
                    "内存使用超限：{} MB / {} MB",
                    used / 1024 / 1024,
                    limit / 1024 / 1024
                )
            }
            ResourceViolation::CpuExceeded { used, limit } => {
                write!(f, "CPU 使用超限：{:.1}% / {:.1}%", used, limit)
            }
            ResourceViolation::DiskExceeded { used, limit } => {
                write!(
                    f,
                    "磁盘使用超限：{} MB / {} MB",
                    used / 1024 / 1024,
                    limit / 1024 / 1024
                )
            }
            ResourceViolation::NetworkExceeded { used, limit } => {
                write!(
                    f,
                    "网络使用超限：{} KB/s / {} KB/s",
                    used / 1024,
                    limit / 1024
                )
            }
        }
    }
}

/// 资源监控器。
///
/// 跟踪所有插件的资源使用情况，并根据配置限制资源使用。
pub struct ResourceMonitor {
    /// 插件资源使用统计。
    usage: Arc<Mutex<HashMap<String, ResourceUsage>>>,
    /// 插件资源限制配置。
    limits: HashMap<String, ResourceLimits>,
    /// 监控间隔。
    monitor_interval: Duration,
}

impl ResourceMonitor {
    /// 创建新的资源监控器。
    pub fn new(monitor_interval: Duration) -> Self {
        Self {
            usage: Arc::new(Mutex::new(HashMap::new())),
            limits: HashMap::new(),
            monitor_interval,
        }
    }

    /// 设置插件的资源限制。
    pub fn set_limits(&mut self, plugin_id: &str, limits: ResourceLimits) {
        self.limits.insert(plugin_id.to_string(), limits);
    }

    /// 获取插件的资源限制。
    pub fn get_limits(&self, plugin_id: &str) -> Option<&ResourceLimits> {
        self.limits.get(plugin_id)
    }

    /// 更新插件的资源使用统计。
    pub async fn update_usage(&self, plugin_id: &str, usage: ResourceUsage) {
        let mut usage_map = self.usage.lock().await;
        usage_map.insert(plugin_id.to_string(), usage);
    }

    /// 获取插件的资源使用统计。
    pub async fn get_usage(&self, plugin_id: &str) -> Option<ResourceUsage> {
        let usage_map = self.usage.lock().await;
        usage_map.get(plugin_id).cloned()
    }

    /// 检查插件是否超出资源限制。
    pub async fn check_violations(&self, plugin_id: &str) -> Vec<ResourceViolation> {
        let usage_map = self.usage.lock().await;
        if let (Some(usage), Some(limits)) = (usage_map.get(plugin_id), self.limits.get(plugin_id))
        {
            usage.exceeds_limits(limits)
        } else {
            Vec::new()
        }
    }

    /// 启动资源监控任务。
    ///
    /// 定期检查所有插件的资源使用情况，并在超限时发出警告。
    pub fn start_monitoring(self: Arc<Self>) {
        let _usage = Arc::clone(&self.usage);
        let monitor_interval = self.monitor_interval;

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(monitor_interval);
            loop {
                interval.tick().await;
                // 这里可以添加实际的资源监控逻辑
                // 例如：读取 /proc 文件系统（Linux）、使用 Windows API 等
                // 目前只是占位实现
            }
        });
    }
}

/// 资源监控错误。
#[derive(Debug, thiserror::Error)]
pub enum ResourceMonitorError {
    /// 资源使用超限。
    #[error("资源使用超限：{0}")]
    LimitExceeded(String),

    /// 监控系统错误。
    #[error("监控系统错误：{0}")]
    SystemError(String),
}
