//! 健康检查模块。
//!
//! 提供：
//! - 系统健康检查
//! - 插件状态监控
//! - 诊断信息导出

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::runtime::supervisor::Supervisor;
use crate::permission::PermissionPrompter;

/// 系统健康状态。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthStatus {
    /// 整体状态。
    pub status: String,
    /// 启动时间（Unix 时间戳）。
    pub uptime_secs: u64,
    /// 活跃插件数量。
    pub active_plugins: u32,
    /// 总插件数量。
    pub total_plugins: u32,
    /// 各组件状态。
    pub components: HashMap<String, ComponentHealth>,
}

/// 组件健康状态。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComponentHealth {
    /// 状态（healthy/degraded/unhealthy）。
    pub status: String,
    /// 消息。
    pub message: Option<String>,
    /// 最后检查时间。
    pub last_check: u64,
}

impl ComponentHealth {
    /// 创建健康状态。
    pub fn healthy() -> Self {
        Self {
            status: "healthy".to_string(),
            message: None,
            last_check: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        }
    }

    /// 创建降级状态。
    pub fn degraded(message: &str) -> Self {
        Self {
            status: "degraded".to_string(),
            message: Some(message.to_string()),
            last_check: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        }
    }

    /// 创建不健康状态。
    pub fn unhealthy(message: &str) -> Self {
        Self {
            status: "unhealthy".to_string(),
            message: Some(message.to_string()),
            last_check: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        }
    }
}

/// 健康检查器。
pub struct HealthChecker {
    /// 启动时间。
    start_time: SystemTime,
}

impl HealthChecker {
    /// 创建新的健康检查器。
    pub fn new() -> Self {
        Self {
            start_time: SystemTime::now(),
        }
    }

    /// 执行健康检查。
    pub fn check<P: PermissionPrompter + 'static>(
        &self,
        supervisor: &Supervisor<P>,
    ) -> HealthStatus {
        // 启动时间
        let uptime = SystemTime::now()
            .duration_since(self.start_time)
            .unwrap_or_default()
            .as_secs();

        // 插件统计
        let (active_plugins, total_plugins) = self.count_plugins(supervisor);

        // 组件状态
        let mut components = HashMap::new();
        components.insert("registry".to_string(), self.check_registry(supervisor));
        components.insert("runtime".to_string(), self.check_runtime());

        // 整体状态
        let status = if components.values().any(|c| c.status == "unhealthy") {
            "unhealthy".to_string()
        } else if components.values().any(|c| c.status == "degraded") {
            "degraded".to_string()
        } else {
            "healthy".to_string()
        };

        HealthStatus {
            status,
            uptime_secs: uptime,
            active_plugins,
            total_plugins,
            components,
        }
    }

    /// 检查注册表。
    fn check_registry<P: PermissionPrompter + 'static>(
        &self,
        supervisor: &Supervisor<P>,
    ) -> ComponentHealth {
        let registry = supervisor.registry();
        let count = registry.len();
        if count > 0 {
            ComponentHealth::healthy()
        } else {
            ComponentHealth::degraded("未加载任何插件")
        }
    }

    /// 检查运行时。
    fn check_runtime(&self) -> ComponentHealth {
        ComponentHealth::healthy()
    }

    /// 统计插件数量。
    fn count_plugins<P: PermissionPrompter + 'static>(
        &self,
        supervisor: &Supervisor<P>,
    ) -> (u32, u32) {
        let registry = supervisor.registry();
        let total = registry.len() as u32;
        // 这里简化处理，实际应该检查每个实例的运行状态
        let active = total;
        (active, total)
    }
}

impl Default for HealthChecker {
    fn default() -> Self {
        Self::new()
    }
}

/// 诊断信息。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiagnosticInfo {
    /// 应用版本。
    pub version: String,
    /// 操作系统。
    pub os: String,
    /// 架构。
    pub arch: String,
    /// Tauri 版本。
    pub tauri_version: String,
    /// 插件路径。
    pub plugins_dir: String,
    /// 配置路径。
    pub config_dir: String,
    /// 日志路径。
    pub log_dir: String,
    /// 健康状态。
    pub health: Option<HealthStatus>,
    /// 环境变量。
    pub env_vars: HashMap<String, String>,
}

impl DiagnosticInfo {
    /// 创建诊断信息。
    pub fn new<P: PermissionPrompter + 'static>(
        _supervisor: &Supervisor<P>,
    ) -> Self {
        let mut env_vars = HashMap::new();
        for key in &["PATH", "HOME", "USERPROFILE", "TEMP", "TMP"] {
            if let Ok(value) = std::env::var(key) {
                env_vars.insert(key.to_string(), value);
            }
        }

        Self {
            version: env!("CARGO_PKG_VERSION").to_string(),
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
            tauri_version: "2.x".to_string(),
            plugins_dir: "~/.intools/plugins".to_string(),
            config_dir: "~/.intools".to_string(),
            log_dir: "~/.intools/logs".to_string(),
            health: None,
            env_vars,
        }
    }

    /// 导出为 JSON。
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    /// 导出为文件。
    pub fn save_to_file(&self, path: &std::path::Path) -> Result<(), std::io::Error> {
        let json = self.to_json().map_err(|e| {
            std::io::Error::new(std::io::ErrorKind::Other, e.to_string())
        })?;
        std::fs::write(path, json)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_component_health() {
        let healthy = ComponentHealth::healthy();
        assert_eq!(healthy.status, "healthy");
        assert!(healthy.message.is_none());

        let degraded = ComponentHealth::degraded("test");
        assert_eq!(degraded.status, "degraded");
        assert_eq!(degraded.message, Some("test".to_string()));

        let unhealthy = ComponentHealth::unhealthy("test");
        assert_eq!(unhealthy.status, "unhealthy");
    }

    #[test]
    fn test_health_checker_creation() {
        let checker = HealthChecker::new();
        assert!(checker.start_time.elapsed().is_ok());
    }
}
