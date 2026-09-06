//! 在线插件市场：支持远程下载和安装插件。
//!
//! 提供以下功能：
//! - 插件市场 API 客户端
//! - 插件搜索和浏览
//! - 插件下载和安装
//! - 插件更新检查

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::plugin_signing::{PluginSigningManager, SignatureVerification};
use super::plugin_versioning::{PluginVersionHistory, SemanticVersion};

/// 插件市场配置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketplaceConfig {
    /// 市场 API 基础 URL。
    pub api_base_url: String,
    /// 是否启用 HTTPS。
    pub use_https: bool,
    /// API 密钥（可选）。
    pub api_key: Option<String>,
    /// 代理设置（可选）。
    pub proxy: Option<String>,
    /// 超时时间（秒）。
    pub timeout_seconds: u32,
}

impl Default for MarketplaceConfig {
    fn default() -> Self {
        Self {
            api_base_url: "https://marketplace.intools.dev".to_string(),
            use_https: true,
            api_key: None,
            proxy: None,
            timeout_seconds: 30,
        }
    }
}

/// 插件市场元数据。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginMetadata {
    /// 插件 ID。
    pub id: String,
    /// 插件名称。
    pub name: String,
    /// 插件描述。
    pub description: String,
    /// 作者信息。
    pub author: String,
    /// 分类。
    pub category: String,
    /// 标签。
    pub tags: Vec<String>,
    /// 最新版本。
    pub latest_version: SemanticVersion,
    /// 总下载次数。
    pub download_count: u64,
    /// 平均评分（1-5）。
    pub rating: f32,
    /// 评分数量。
    pub rating_count: u32,
    /// 创建时间。
    pub created_at: String,
    /// 更新时间。
    pub updated_at: String,
    /// 项目主页。
    pub homepage: Option<String>,
    /// 源代码仓库。
    pub repository: Option<String>,
    /// 许可证。
    pub license: Option<String>,
}

/// 插件搜索查询。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchQuery {
    /// 搜索关键词。
    pub query: Option<String>,
    /// 分类筛选。
    pub category: Option<String>,
    /// 标签筛选。
    pub tags: Option<Vec<String>>,
    /// 排序方式。
    pub sort_by: Option<SortBy>,
    /// 页码。
    pub page: Option<u32>,
    /// 每页数量。
    pub per_page: Option<u32>,
}

/// 排序方式。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SortBy {
    /// 相关性。
    Relevance,
    /// 下载量。
    Downloads,
    /// 评分。
    Rating,
    /// 更新时间。
    Updated,
    /// 创建时间。
    Created,
}

/// 搜索结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    /// 总数量。
    pub total: u64,
    /// 页码。
    pub page: u32,
    /// 每页数量。
    pub per_page: u32,
    /// 插件列表。
    pub plugins: Vec<PluginMetadata>,
}

/// 插件市场客户端。
pub struct PluginMarketplace {
    config: MarketplaceConfig,
    signing_manager: PluginSigningManager,
}

impl PluginMarketplace {
    /// 创建新的插件市场客户端。
    pub fn new(config: MarketplaceConfig) -> Self {
        Self {
            config,
            signing_manager: PluginSigningManager::new(),
        }
    }

    /// 搜索插件。
    pub fn search_plugins(&self, query: &SearchQuery) -> Result<SearchResult, MarketplaceError> {
        // 这里简化处理，实际实现需要：
        // 1. 构建请求 URL
        // 2. 发送 HTTP 请求
        // 3. 解析响应
        // 目前返回空结果作为占位实现
        let _ = query;
        Ok(SearchResult {
            total: 0,
            page: 1,
            per_page: 20,
            plugins: Vec::new(),
        })
    }

    /// 获取插件详情。
    pub fn get_plugin_details(&self, plugin_id: &str) -> Result<PluginMetadata, MarketplaceError> {
        // 这里简化处理，实际实现需要：
        // 1. 构建请求 URL
        // 2. 发送 HTTP 请求
        // 3. 解析响应
        // 目前返回错误作为占位实现
        let _ = plugin_id;
        Err(MarketplaceError::PluginNotFound(plugin_id.to_string()))
    }

    /// 获取插件版本历史。
    pub fn get_plugin_versions(&self, plugin_id: &str) -> Result<PluginVersionHistory, MarketplaceError> {
        // 这里简化处理，实际实现需要：
        // 1. 构建请求 URL
        // 2. 发送 HTTP 请求
        // 3. 解析响应
        // 目前返回空历史作为占位实现
        let _ = plugin_id;
        Ok(PluginVersionHistory::new(plugin_id.to_string()))
    }

    /// 下载插件。
    pub fn download_plugin(
        &self,
        plugin_id: &str,
        version: &SemanticVersion,
        dest_dir: &Path,
    ) -> Result<DownloadResult, MarketplaceError> {
        // 这里简化处理，实际实现需要：
        // 1. 构建下载 URL
        // 2. 发送 HTTP 请求下载文件
        // 3. 保存到目标目录
        // 4. 验证签名
        // 目前返回错误作为占位实现
        let _ = plugin_id;
        let _ = version;
        let _ = dest_dir;
        Err(MarketplaceError::DownloadFailed("未实现".to_string()))
    }

    /// 检查插件更新。
    pub fn check_updates(
        &self,
        installed_plugins: &HashMap<String, SemanticVersion>,
    ) -> Result<Vec<UpdateInfo>, MarketplaceError> {
        let mut updates = Vec::new();

        for (plugin_id, current_version) in installed_plugins {
            match self.get_plugin_versions(plugin_id) {
                Ok(history) => {
                    if let Some(latest) = history.latest_version() {
                        if latest.version > *current_version {
                            updates.push(UpdateInfo {
                                plugin_id: plugin_id.clone(),
                                current_version: current_version.clone(),
                                latest_version: latest.version.clone(),
                                description: latest.description.clone(),
                                download_url: latest.download_url.clone(),
                            });
                        }
                    }
                }
                Err(_) => continue,
            }
        }

        Ok(updates)
    }

    /// 安装插件。
    pub fn install_plugin(
        &self,
        plugin_id: &str,
        version: &SemanticVersion,
        dest_dir: &Path,
    ) -> Result<InstallResult, MarketplaceError> {
        // 下载插件
        let download_result = self.download_plugin(plugin_id, version, dest_dir)?;

        // 验证签名
        let verification = self.verify_plugin_signature(&download_result.plugin_dir)?;

        if !verification.valid {
            return Err(MarketplaceError::SignatureVerificationFailed(
                verification.details.error.unwrap_or_default(),
            ));
        }

        Ok(InstallResult {
            plugin_id: plugin_id.to_string(),
            version: version.clone(),
            plugin_dir: download_result.plugin_dir,
            signature_verification: verification,
        })
    }

    /// 验证插件签名。
    fn verify_plugin_signature(&self, plugin_dir: &Path) -> Result<SignatureVerification, MarketplaceError> {
        // 这里简化处理，实际实现需要：
        // 1. 从插件目录读取签名文件
        // 2. 验证签名
        // 目前返回验证失败作为占位实现
        let _ = plugin_dir;
        Ok(SignatureVerification {
            valid: false,
            verified_at: chrono::Utc::now().to_rfc3339(),
            details: super::plugin_signing::VerificationDetails {
                signature_valid: false,
                integrity_valid: false,
                signer_trusted: false,
                error: Some("签名验证未实现".to_string()),
            },
        })
    }
}

/// 下载结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadResult {
    /// 插件 ID。
    pub plugin_id: String,
    /// 版本号。
    pub version: SemanticVersion,
    /// 插件目录。
    pub plugin_dir: std::path::PathBuf,
    /// 文件大小（字节）。
    pub file_size: u64,
    /// 下载时间。
    pub downloaded_at: String,
}

/// 安装结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallResult {
    /// 插件 ID。
    pub plugin_id: String,
    /// 版本号。
    pub version: SemanticVersion,
    /// 插件目录。
    pub plugin_dir: std::path::PathBuf,
    /// 签名验证结果。
    pub signature_verification: SignatureVerification,
}

/// 更新信息。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateInfo {
    /// 插件 ID。
    pub plugin_id: String,
    /// 当前版本。
    pub current_version: SemanticVersion,
    /// 最新版本。
    pub latest_version: SemanticVersion,
    /// 更新描述。
    pub description: String,
    /// 下载 URL。
    pub download_url: String,
}

/// 市场错误。
#[derive(Debug, thiserror::Error)]
pub enum MarketplaceError {
    /// 网络错误。
    #[error("网络错误：{0}")]
    NetworkError(String),

    /// 插件未找到。
    #[error("插件未找到：{0}")]
    PluginNotFound(String),

    /// 下载失败。
    #[error("下载失败：{0}")]
    DownloadFailed(String),

    /// 签名验证失败。
    #[error("签名验证失败：{0}")]
    SignatureVerificationFailed(String),

    /// 安装失败。
    #[error("安装失败：{0}")]
    InstallationFailed(String),

    /// 配置错误。
    #[error("配置错误：{0}")]
    ConfigurationError(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_marketplace_config_default() {
        let config = MarketplaceConfig::default();
        assert_eq!(config.api_base_url, "https://marketplace.intools.dev");
        assert!(config.use_https);
        assert_eq!(config.timeout_seconds, 30);
    }

    #[test]
    fn test_search_query() {
        let query = SearchQuery {
            query: Some("test".to_string()),
            category: Some("tools".to_string()),
            tags: Some(vec!["utility".to_string()]),
            sort_by: Some(SortBy::Downloads),
            page: Some(1),
            per_page: Some(20),
        };

        assert_eq!(query.query, Some("test".to_string()));
        assert_eq!(query.category, Some("tools".to_string()));
    }

    #[test]
    fn test_update_info() {
        let update = UpdateInfo {
            plugin_id: "com.example.test".to_string(),
            current_version: SemanticVersion::new(1, 0, 0),
            latest_version: SemanticVersion::new(1, 1, 0),
            description: "Bug fixes".to_string(),
            download_url: "https://example.com/v1.1.0.zip".to_string(),
        };

        assert_eq!(update.plugin_id, "com.example.test");
        assert!(update.latest_version > update.current_version);
    }
}
