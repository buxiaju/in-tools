//! 统一的错误处理模块。
//!
//! 提供：
//! - 统一的错误类型
//! - 错误转换
//! - 错误日志记录
//! - 用户友好的错误消息

use std::fmt;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// 应用错误类型。
#[derive(Debug, Error)]
pub enum AppError {
    /// IO 错误。
    #[error("IO 错误：{0}")]
    Io(#[from] std::io::Error),

    /// JSON 序列化错误。
    #[error("JSON 错误：{0}")]
    Json(#[from] serde_json::Error),

    /// 配置错误。
    #[error("配置错误：{0}")]
    Config(String),

    /// 插件未找到。
    #[error("插件未找到：{0}")]
    PluginNotFound(String),

    /// 工具未找到。
    #[error("工具未找到：{0}")]
    ToolNotFound(String),

    /// 权限错误。
    #[error("权限错误：{0}")]
    Permission(String),

    /// 网络错误。
    #[error("网络错误：{0}")]
    Network(String),

    /// 超时错误。
    #[error("操作超时：{0}")]
    Timeout(String),

    /// 验证错误。
    #[error("验证错误：{0}")]
    Validation(String),

    /// 内部错误。
    #[error("内部错误：{0}")]
    Internal(String),

    /// 外部错误。
    #[error("外部错误：{0}")]
    External(String),
}

impl AppError {
    /// 创建配置错误。
    pub fn config(msg: impl Into<String>) -> Self {
        AppError::Config(msg.into())
    }

    /// 创建内部错误。
    pub fn internal(msg: impl Into<String>) -> Self {
        AppError::Internal(msg.into())
    }

    /// 创建验证错误。
    pub fn validation(msg: impl Into<String>) -> Self {
        AppError::Validation(msg.into())
    }

    /// 创建权限错误。
    pub fn permission(msg: impl Into<String>) -> Self {
        AppError::Permission(msg.into())
    }

    /// 创建网络错误。
    pub fn network(msg: impl Into<String>) -> Self {
        AppError::Network(msg.into())
    }

    /// 创建超时错误。
    pub fn timeout(msg: impl Into<String>) -> Self {
        AppError::Timeout(msg.into())
    }

    /// 记录错误日志。
    pub fn log(&self) {
        tracing::error!(error = %self, "应用错误");
    }

    /// 获取用户友好的错误消息。
    pub fn user_message(&self) -> String {
        match self {
            AppError::Io(_) => "文件操作失败，请检查文件权限".to_string(),
            AppError::Json(_) => "数据格式错误".to_string(),
            AppError::Config(_) => "配置错误，请检查设置".to_string(),
            AppError::PluginNotFound(_) => "插件不存在或已卸载".to_string(),
            AppError::ToolNotFound(_) => "工具不存在".to_string(),
            AppError::Permission(_) => "权限不足，无法执行此操作".to_string(),
            AppError::Network(_) => "网络错误，请检查网络连接".to_string(),
            AppError::Timeout(_) => "操作超时，请稍后重试".to_string(),
            AppError::Validation(_) => "输入数据无效".to_string(),
            AppError::Internal(_) => "内部错误，请查看日志".to_string(),
            AppError::External(_) => "外部服务错误".to_string(),
        }
    }

    /// 获取错误代码。
    pub fn code(&self) -> &'static str {
        match self {
            AppError::Io(_) => "IO_ERROR",
            AppError::Json(_) => "JSON_ERROR",
            AppError::Config(_) => "CONFIG_ERROR",
            AppError::PluginNotFound(_) => "PLUGIN_NOT_FOUND",
            AppError::ToolNotFound(_) => "TOOL_NOT_FOUND",
            AppError::Permission(_) => "PERMISSION_ERROR",
            AppError::Network(_) => "NETWORK_ERROR",
            AppError::Timeout(_) => "TIMEOUT",
            AppError::Validation(_) => "VALIDATION_ERROR",
            AppError::Internal(_) => "INTERNAL_ERROR",
            AppError::External(_) => "EXTERNAL_ERROR",
        }
    }
}

/// 应用结果类型。
pub type AppResult<T> = Result<T, AppError>;

/// 错误响应结构（用于 IPC）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorResponse {
    /// 错误代码。
    pub code: String,
    /// 错误消息。
    pub message: String,
    /// 用户友好的消息。
    pub user_message: String,
}

impl From<AppError> for ErrorResponse {
    fn from(err: AppError) -> Self {
        ErrorResponse {
            code: err.code().to_string(),
            message: err.to_string(),
            user_message: err.user_message(),
        }
    }
}

/// 验证器。
pub struct Validator;

impl Validator {
    /// 验证字符串不为空。
    pub fn non_empty(value: &str, field: &str) -> AppResult<()> {
        if value.trim().is_empty() {
            return Err(AppError::validation(format!("{} 不能为空", field)));
        }
        Ok(())
    }

    /// 验证路径有效。
    pub fn path(value: &str, field: &str) -> AppResult<()> {
        if value.trim().is_empty() {
            return Err(AppError::validation(format!("{} 不能为空", field)));
        }
        if value.contains("..") {
            return Err(AppError::validation(format!("{} 包含非法字符", field)));
        }
        Ok(())
    }

    /// 验证 URL 有效。
    pub fn url(value: &str, field: &str) -> AppResult<()> {
        if value.trim().is_empty() {
            return Err(AppError::validation(format!("{} 不能为空", field)));
        }
        if url::Url::parse(value).is_err() {
            return Err(AppError::validation(format!("{} 不是有效的 URL", field)));
        }
        Ok(())
    }

    /// 验证数值在范围内。
    pub fn range<T: PartialOrd + fmt::Display>(
        value: T,
        min: T,
        max: T,
        field: &str,
    ) -> AppResult<()> {
        if value < min || value > max {
            return Err(AppError::validation(format!(
                "{} 必须在 {} 到 {} 之间",
                field, min, max
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_codes() {
        assert_eq!(AppError::config("test").code(), "CONFIG_ERROR");
        assert_eq!(AppError::internal("test").code(), "INTERNAL_ERROR");
        assert_eq!(AppError::validation("test").code(), "VALIDATION_ERROR");
    }

    #[test]
    fn test_user_messages() {
        let err = AppError::permission("test");
        assert!(err.user_message().contains("权限"));
    }

    #[test]
    fn test_validator() {
        assert!(Validator::non_empty("test", "field").is_ok());
        assert!(Validator::non_empty("", "field").is_err());
        assert!(Validator::non_empty("   ", "field").is_err());

        assert!(Validator::path("/valid/path", "field").is_ok());
        assert!(Validator::path("../invalid", "field").is_err());

        assert!(Validator::url("https://example.com", "field").is_ok());
        assert!(Validator::url("not a url", "field").is_err());
    }
}
