//! 插件版本管理系统：支持插件版本控制。
//!
//! 提供以下功能：
//! - 语义化版本解析和比较
//! - 版本约束匹配
//! - 版本历史记录

use std::collections::HashMap;
use std::fmt;

use serde::{Deserialize, Serialize};

/// 语义化版本。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SemanticVersion {
    /// 主版本号。
    pub major: u32,
    /// 次版本号。
    pub minor: u32,
    /// 修订版本号。
    pub patch: u32,
    /// 预发布版本标签（可选）。
    pub pre_release: Option<String>,
    /// 构建元数据（可选）。
    pub build: Option<String>,
}

impl SemanticVersion {
    /// 创建新的语义化版本。
    pub fn new(major: u32, minor: u32, patch: u32) -> Self {
        Self {
            major,
            minor,
            patch,
            pre_release: None,
            build: None,
        }
    }

    /// 创建带预发布标签的版本。
    pub fn with_pre_release(mut self, pre_release: String) -> Self {
        self.pre_release = Some(pre_release);
        self
    }

    /// 创建带构建元数据的版本。
    pub fn with_build(mut self, build: String) -> Self {
        self.build = Some(build);
        self
    }

    /// 解析版本字符串。
    pub fn parse(version: &str) -> Result<Self, VersionParseError> {
        // 移除开头的 'v' 或 'V'
        let version = version.trim_start_matches(|c| c == 'v' || c == 'V');

        // 分离构建元数据
        let (version_part, build) = if let Some(pos) = version.find('+') {
            let (v, b) = version.split_at(pos);
            (v, Some(b[1..].to_string()))
        } else {
            (version, None)
        };

        // 分离预发布标签
        let (version_part, pre_release) = if let Some(pos) = version_part.find('-') {
            let (v, p) = version_part.split_at(pos);
            (v, Some(p[1..].to_string()))
        } else {
            (version_part, None)
        };

        // 解析版本号
        let parts: Vec<&str> = version_part.split('.').collect();
        if parts.len() != 3 {
            return Err(VersionParseError::InvalidFormat(version.to_string()));
        }

        let major = parts[0]
            .parse::<u32>()
            .map_err(|_| VersionParseError::InvalidMajor(version.to_string()))?;
        let minor = parts[1]
            .parse::<u32>()
            .map_err(|_| VersionParseError::InvalidMinor(version.to_string()))?;
        let patch = parts[2]
            .parse::<u32>()
            .map_err(|_| VersionParseError::InvalidPatch(version.to_string()))?;

        Ok(Self {
            major,
            minor,
            patch,
            pre_release,
            build,
        })
    }

    /// 检查是否为预发布版本。
    pub fn is_pre_release(&self) -> bool {
        self.pre_release.is_some()
    }

    /// 获取版本号（不含预发布和构建元数据）。
    pub fn version_number(&self) -> String {
        format!("{}.{}.{}", self.major, self.minor, self.patch)
    }
}

impl fmt::Display for SemanticVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if let Some(ref pre_release) = self.pre_release {
            write!(f, "-{}", pre_release)?;
        }
        if let Some(ref build) = self.build {
            write!(f, "+{}", build)?;
        }
        Ok(())
    }
}

impl PartialOrd for SemanticVersion {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for SemanticVersion {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // 比较主版本号
        match self.major.cmp(&other.major) {
            std::cmp::Ordering::Equal => {}
            ord => return ord,
        }

        // 比较次版本号
        match self.minor.cmp(&other.minor) {
            std::cmp::Ordering::Equal => {}
            ord => return ord,
        }

        // 比较修订版本号
        match self.patch.cmp(&other.patch) {
            std::cmp::Ordering::Equal => {}
            ord => return ord,
        }

        // 比较预发布标签（没有预发布标签的版本高于有预发布标签的版本）
        match (&self.pre_release, &other.pre_release) {
            (None, None) => std::cmp::Ordering::Equal,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (Some(_), None) => std::cmp::Ordering::Less,
            (Some(a), Some(b)) => a.cmp(b),
        }
    }
}

/// 版本解析错误。
#[derive(Debug, thiserror::Error)]
pub enum VersionParseError {
    /// 无效的格式。
    #[error("无效的版本格式：{0}")]
    InvalidFormat(String),

    /// 无效的主版本号。
    #[error("无效的主版本号：{0}")]
    InvalidMajor(String),

    /// 无效的次版本号。
    #[error("无效的次版本号：{0}")]
    InvalidMinor(String),

    /// 无效的修订版本号。
    #[error("无效的修订版本号：{0}")]
    InvalidPatch(String),
}

/// 版本约束。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum VersionConstraint {
    /// 精确版本。
    Exact(SemanticVersion),
    /// 大于等于。
    GreaterOrEqual(SemanticVersion),
    /// 小于等于。
    LessOrEqual(SemanticVersion),
    /// 范围（包含两端）。
    Range(SemanticVersion, SemanticVersion),
    /// 通配符（如 1.2.*）。
    Wildcard { major: u32, minor: Option<u32> },
    /// 兼容版本（^1.2.3 表示 >=1.2.3 且 <2.0.0）。
    Compatible(SemanticVersion),
    /// 波浪号（~1.2.3 表示 >=1.2.3 且 <1.3.0）。
    Tilde(SemanticVersion),
}

impl VersionConstraint {
    /// 检查版本是否满足约束。
    pub fn matches(&self, version: &SemanticVersion) -> bool {
        match self {
            VersionConstraint::Exact(required) => version == required,
            VersionConstraint::GreaterOrEqual(required) => version >= required,
            VersionConstraint::LessOrEqual(required) => version <= required,
            VersionConstraint::Range(min, max) => version >= min && version <= max,
            VersionConstraint::Wildcard { major, minor } => {
                version.major == *major
                    && minor.map_or(true, |m| version.minor == m)
            }
            VersionConstraint::Compatible(required) => {
                version >= required && version.major == required.major
            }
            VersionConstraint::Tilde(required) => {
                version >= required
                    && version.major == required.major
                    && version.minor == required.minor
            }
        }
    }

    /// 解析版本约束字符串。
    pub fn parse(constraint: &str) -> Result<Self, ConstraintParseError> {
        let constraint = constraint.trim();

        if constraint == "*" || constraint == "latest" {
            return Ok(VersionConstraint::Wildcard {
                major: 0,
                minor: None,
            });
        }

        // 精确版本
        if let Ok(version) = SemanticVersion::parse(constraint) {
            return Ok(VersionConstraint::Exact(version));
        }

        // 大于等于
        if constraint.starts_with(">=") {
            let version = SemanticVersion::parse(&constraint[2..])?;
            return Ok(VersionConstraint::GreaterOrEqual(version));
        }

        // 小于等于
        if constraint.starts_with("<=") {
            let version = SemanticVersion::parse(&constraint[2..])?;
            return Ok(VersionConstraint::LessOrEqual(version));
        }

        // 范围
        if constraint.contains(" - ") {
            let parts: Vec<&str> = constraint.split(" - ").collect();
            if parts.len() == 2 {
                let min = SemanticVersion::parse(parts[0])?;
                let max = SemanticVersion::parse(parts[1])?;
                return Ok(VersionConstraint::Range(min, max));
            }
        }

        // 通配符
        if constraint.contains('*') {
            let parts: Vec<&str> = constraint.split('.').collect();
            if parts.len() >= 1 && parts.len() <= 2 {
                let major = parts[0].parse::<u32>().map_err(|_| {
                    ConstraintParseError::InvalidConstraint(constraint.to_string())
                })?;
                let minor = if parts.len() > 1 && parts[1] != "*" {
                    Some(parts[1].parse::<u32>().map_err(|_| {
                        ConstraintParseError::InvalidConstraint(constraint.to_string())
                    })?)
                } else {
                    None
                };
                return Ok(VersionConstraint::Wildcard { major, minor });
            }
        }

        // 兼容版本
        if constraint.starts_with('^') {
            let version = SemanticVersion::parse(&constraint[1..])?;
            return Ok(VersionConstraint::Compatible(version));
        }

        // 波浪号
        if constraint.starts_with('~') {
            let version = SemanticVersion::parse(&constraint[1..])?;
            return Ok(VersionConstraint::Tilde(version));
        }

        Err(ConstraintParseError::InvalidConstraint(
            constraint.to_string(),
        ))
    }
}

impl fmt::Display for VersionConstraint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VersionConstraint::Exact(version) => write!(f, "{}", version),
            VersionConstraint::GreaterOrEqual(version) => write!(f, ">={}", version),
            VersionConstraint::LessOrEqual(version) => write!(f, "<={}", version),
            VersionConstraint::Range(min, max) => write!(f, "{} - {}", min, max),
            VersionConstraint::Wildcard { major, minor } => {
                write!(f, "{}.{}", major, minor.map_or("*".to_string(), |m| m.to_string()))
            }
            VersionConstraint::Compatible(version) => write!(f, "^{}", version),
            VersionConstraint::Tilde(version) => write!(f, "~{}", version),
        }
    }
}

/// 版本约束解析错误。
#[derive(Debug, thiserror::Error)]
pub enum ConstraintParseError {
    /// 无效的约束。
    #[error("无效的版本约束：{0}")]
    InvalidConstraint(String),

    /// 版本解析错误。
    #[error("版本解析错误：{0}")]
    VersionParseError(#[from] VersionParseError),
}

/// 插件版本信息。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginVersionInfo {
    /// 插件 ID。
    pub plugin_id: String,
    /// 版本号。
    pub version: SemanticVersion,
    /// 发布时间。
    pub released_at: String,
    /// 版本描述。
    pub description: String,
    /// 变更日志。
    pub changelog: Option<String>,
    /// 下载 URL。
    pub download_url: String,
    /// 文件大小（字节）。
    pub file_size: u64,
    /// 文件哈希（SHA-256）。
    pub file_hash: String,
    /// 依赖项。
    pub dependencies: HashMap<String, VersionConstraint>,
    /// 最低宿主版本要求。
    pub min_host_version: Option<SemanticVersion>,
}

/// 插件版本历史。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginVersionHistory {
    /// 插件 ID。
    pub plugin_id: String,
    /// 版本列表（按发布时间倒序）。
    pub versions: Vec<PluginVersionInfo>,
}

impl PluginVersionHistory {
    /// 创建新的版本历史。
    pub fn new(plugin_id: String) -> Self {
        Self {
            plugin_id,
            versions: Vec::new(),
        }
    }

    /// 添加版本。
    pub fn add_version(&mut self, version: PluginVersionInfo) {
        self.versions.push(version);
        // 按版本号排序（最新版本在前）
        self.versions.sort_by(|a, b| b.version.cmp(&a.version));
    }

    /// 获取最新版本。
    pub fn latest_version(&self) -> Option<&PluginVersionInfo> {
        self.versions.first()
    }

    /// 获取指定版本。
    pub fn get_version(&self, version: &SemanticVersion) -> Option<&PluginVersionInfo> {
        self.versions.iter().find(|v| &v.version == version)
    }

    /// 获取满足约束的最新版本。
    pub fn get_matching_version(&self, constraint: &VersionConstraint) -> Option<&PluginVersionInfo> {
        self.versions.iter().find(|v| constraint.matches(&v.version))
    }

    /// 获取所有版本号。
    pub fn all_versions(&self) -> Vec<&SemanticVersion> {
        self.versions.iter().map(|v| &v.version).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_semantic_version_parsing() {
        let version = SemanticVersion::parse("1.2.3").unwrap();
        assert_eq!(version.major, 1);
        assert_eq!(version.minor, 2);
        assert_eq!(version.patch, 3);
        assert_eq!(version.pre_release, None);
        assert_eq!(version.build, None);

        let version = SemanticVersion::parse("v1.2.3-beta.1+build.123").unwrap();
        assert_eq!(version.major, 1);
        assert_eq!(version.minor, 2);
        assert_eq!(version.patch, 3);
        assert_eq!(version.pre_release, Some("beta.1".to_string()));
        assert_eq!(version.build, Some("build.123".to_string()));
    }

    #[test]
    fn test_semantic_version_ordering() {
        let v1 = SemanticVersion::new(1, 0, 0);
        let v2 = SemanticVersion::new(1, 0, 1);
        let v3 = SemanticVersion::new(1, 1, 0);
        let v4 = SemanticVersion::new(2, 0, 0);

        assert!(v1 < v2);
        assert!(v2 < v3);
        assert!(v3 < v4);
    }

    #[test]
    fn test_version_constraint_matching() {
        let exact = VersionConstraint::Exact(SemanticVersion::new(1, 2, 3));
        assert!(exact.matches(&SemanticVersion::new(1, 2, 3)));
        assert!(!exact.matches(&SemanticVersion::new(1, 2, 4)));

        let gte = VersionConstraint::GreaterOrEqual(SemanticVersion::new(1, 2, 3));
        assert!(gte.matches(&SemanticVersion::new(1, 2, 3)));
        assert!(gte.matches(&SemanticVersion::new(1, 2, 4)));
        assert!(!gte.matches(&SemanticVersion::new(1, 2, 2)));

        let range = VersionConstraint::Range(
            SemanticVersion::new(1, 0, 0),
            SemanticVersion::new(2, 0, 0),
        );
        assert!(range.matches(&SemanticVersion::new(1, 5, 0)));
        assert!(!range.matches(&SemanticVersion::new(2, 0, 1)));

        let compatible = VersionConstraint::Compatible(SemanticVersion::new(1, 2, 3));
        assert!(compatible.matches(&SemanticVersion::new(1, 2, 3)));
        assert!(compatible.matches(&SemanticVersion::new(1, 9, 9)));
        assert!(!compatible.matches(&SemanticVersion::new(2, 0, 0)));

        let tilde = VersionConstraint::Tilde(SemanticVersion::new(1, 2, 3));
        assert!(tilde.matches(&SemanticVersion::new(1, 2, 3)));
        assert!(tilde.matches(&SemanticVersion::new(1, 2, 9)));
        assert!(!tilde.matches(&SemanticVersion::new(1, 3, 0)));
    }

    #[test]
    fn test_version_constraint_parsing() {
        let constraint = VersionConstraint::parse("1.2.3").unwrap();
        assert!(matches!(constraint, VersionConstraint::Exact(_)));

        let constraint = VersionConstraint::parse(">=1.2.3").unwrap();
        assert!(matches!(constraint, VersionConstraint::GreaterOrEqual(_)));

        let constraint = VersionConstraint::parse("^1.2.3").unwrap();
        assert!(matches!(constraint, VersionConstraint::Compatible(_)));

        let constraint = VersionConstraint::parse("~1.2.3").unwrap();
        assert!(matches!(constraint, VersionConstraint::Tilde(_)));

        let constraint = VersionConstraint::parse("*").unwrap();
        assert!(matches!(constraint, VersionConstraint::Wildcard { .. }));
    }

    #[test]
    fn test_plugin_version_history() {
        let mut history = PluginVersionHistory::new("com.example.test".to_string());

        let v1 = PluginVersionInfo {
            plugin_id: "com.example.test".to_string(),
            version: SemanticVersion::new(1, 0, 0),
            released_at: "2026-01-01T00:00:00Z".to_string(),
            description: "Initial release".to_string(),
            changelog: None,
            download_url: "https://example.com/v1.0.0.zip".to_string(),
            file_size: 1024,
            file_hash: "hash1".to_string(),
            dependencies: HashMap::new(),
            min_host_version: None,
        };

        let v2 = PluginVersionInfo {
            plugin_id: "com.example.test".to_string(),
            version: SemanticVersion::new(1, 1, 0),
            released_at: "2026-02-01T00:00:00Z".to_string(),
            description: "Bug fixes".to_string(),
            changelog: None,
            download_url: "https://example.com/v1.1.0.zip".to_string(),
            file_size: 1100,
            file_hash: "hash2".to_string(),
            dependencies: HashMap::new(),
            min_host_version: None,
        };

        history.add_version(v1);
        history.add_version(v2);

        assert_eq!(history.versions.len(), 2);
        assert_eq!(history.latest_version().unwrap().version, SemanticVersion::new(1, 1, 0));
    }
}
