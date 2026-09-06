//! 插件签名系统：确保插件来源可信和完整性。
//!
//! 提供以下功能：
//! - 插件签名生成
//! - 插件签名验证
//! - 签名元数据管理

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// 插件签名元数据。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginSignature {
    /// 签名版本。
    pub version: String,
    /// 签名者信息。
    pub signer: SignerInfo,
    /// 签名时间。
    pub signed_at: String,
    /// 插件 ID。
    pub plugin_id: String,
    /// 插件版本。
    pub plugin_version: String,
    /// 文件哈希（SHA-256）。
    pub file_hashes: HashMap<String, String>,
    /// 签名值（Base64 编码）。
    pub signature: String,
}

/// 签名者信息。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignerInfo {
    /// 签名者名称。
    pub name: String,
    /// 签名者邮箱。
    pub email: Option<String>,
    /// 签名者网站。
    pub website: Option<String>,
    /// 签名者公钥（Base64 编码）。
    pub public_key: String,
}

/// 签名验证结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignatureVerification {
    /// 是否有效。
    pub valid: bool,
    /// 验证时间。
    pub verified_at: String,
    /// 验证详情。
    pub details: VerificationDetails,
}

/// 验证详情。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationDetails {
    /// 签名是否有效。
    pub signature_valid: bool,
    /// 文件完整性是否通过。
    pub integrity_valid: bool,
    /// 签名者是否可信。
    pub signer_trusted: bool,
    /// 错误信息（如果有）。
    pub error: Option<String>,
}

/// 插件签名管理器。
pub struct PluginSigningManager {
    /// 可信签名者列表。
    trusted_signers: HashMap<String, SignerInfo>,
}

impl PluginSigningManager {
    /// 创建新的插件签名管理器。
    pub fn new() -> Self {
        Self {
            trusted_signers: HashMap::new(),
        }
    }

    /// 添加可信签名者。
    pub fn add_trusted_signer(&mut self, signer_id: String, signer: SignerInfo) {
        self.trusted_signers.insert(signer_id, signer);
    }

    /// 移除可信签名者。
    pub fn remove_trusted_signer(&mut self, signer_id: &str) -> Option<SignerInfo> {
        self.trusted_signers.remove(signer_id)
    }

    /// 检查签名者是否可信。
    pub fn is_trusted_signer(&self, signer_id: &str) -> bool {
        self.trusted_signers.contains_key(signer_id)
    }

    /// 验证插件签名。
    pub fn verify_signature(
        &self,
        plugin_dir: &Path,
        signature: &PluginSignature,
    ) -> SignatureVerification {
        let verified_at = chrono::Utc::now().to_rfc3339();

        // 检查签名者是否可信
        let signer_id = format!("{}:{}", signature.signer.name, signature.signer.email.as_deref().unwrap_or(""));
        let signer_trusted = self.is_trusted_signer(&signer_id);

        // 验证文件完整性
        let integrity_valid = self.verify_file_integrity(plugin_dir, &signature.file_hashes);

        // 验证签名值（这里简化处理，实际实现需要使用密码学库）
        let signature_valid = self.verify_signature_value(signature);

        let valid = signature_valid && integrity_valid && signer_trusted;

        let error = if !valid {
            let mut errors = Vec::new();
            if !signature_valid {
                errors.push("签名验证失败".to_string());
            }
            if !integrity_valid {
                errors.push("文件完整性验证失败".to_string());
            }
            if !signer_trusted {
                errors.push("签名者不受信任".to_string());
            }
            Some(errors.join("; "))
        } else {
            None
        };

        SignatureVerification {
            valid,
            verified_at,
            details: VerificationDetails {
                signature_valid,
                integrity_valid,
                signer_trusted,
                error,
            },
        }
    }

    /// 验证文件完整性。
    fn verify_file_integrity(
        &self,
        plugin_dir: &Path,
        expected_hashes: &HashMap<String, String>,
    ) -> bool {
        // 这里简化处理，实际实现需要：
        // 1. 遍历插件目录中的所有文件
        // 2. 计算每个文件的 SHA-256 哈希
        // 3. 与签名中的哈希值比较
        // 目前返回 true 作为占位实现
        let _ = plugin_dir;
        let _ = expected_hashes;
        true
    }

    /// 验证签名值。
    fn verify_signature_value(&self, signature: &PluginSignature) -> bool {
        // 这里简化处理，实际实现需要：
        // 1. 使用签名者的公钥
        // 2. 验证签名值是否与文件哈希匹配
        // 目前返回 true 作为占位实现
        let _ = signature;
        true
    }

    /// 生成插件签名。
    pub fn generate_signature(
        &self,
        plugin_dir: &Path,
        plugin_id: &str,
        plugin_version: &str,
        signer: SignerInfo,
    ) -> Result<PluginSignature, SigningError> {
        // 计算文件哈希
        let file_hashes = self.calculate_file_hashes(plugin_dir)?;

        // 生成签名值（这里简化处理，实际实现需要使用私钥签名）
        let signature_value = self.sign_data(&file_hashes, &signer)?;

        let signature = PluginSignature {
            version: "1.0".to_string(),
            signer,
            signed_at: chrono::Utc::now().to_rfc3339(),
            plugin_id: plugin_id.to_string(),
            plugin_version: plugin_version.to_string(),
            file_hashes,
            signature: signature_value,
        };

        Ok(signature)
    }

    /// 计算文件哈希。
    fn calculate_file_hashes(
        &self,
        plugin_dir: &Path,
    ) -> Result<HashMap<String, String>, SigningError> {
        // 这里简化处理，实际实现需要：
        // 1. 遍历插件目录中的所有文件
        // 2. 计算每个文件的 SHA-256 哈希
        // 目前返回空 HashMap 作为占位实现
        let _ = plugin_dir;
        Ok(HashMap::new())
    }

    /// 签名数据。
    fn sign_data(
        &self,
        data: &HashMap<String, String>,
        signer: &SignerInfo,
    ) -> Result<String, SigningError> {
        // 这里简化处理，实际实现需要：
        // 1. 将数据序列化为字节
        // 2. 使用私钥签名
        // 3. 返回 Base64 编码的签名值
        let _ = data;
        let _ = signer;
        Ok("placeholder_signature".to_string())
    }

    /// 保存签名到文件。
    pub fn save_signature(
        &self,
        signature: &PluginSignature,
        path: &Path,
    ) -> Result<(), SigningError> {
        let json = serde_json::to_string_pretty(signature)
            .map_err(|e| SigningError::SerializationFailed(e.to_string()))?;
        std::fs::write(path, json)
            .map_err(|e| SigningError::IoError(e.to_string()))?;
        Ok(())
    }

    /// 从文件加载签名。
    pub fn load_signature(&self, path: &Path) -> Result<PluginSignature, SigningError> {
        let json = std::fs::read_to_string(path)
            .map_err(|e| SigningError::IoError(e.to_string()))?;
        let signature: PluginSignature = serde_json::from_str(&json)
            .map_err(|e| SigningError::DeserializationFailed(e.to_string()))?;
        Ok(signature)
    }
}

impl Default for PluginSigningManager {
    fn default() -> Self {
        Self::new()
    }
}

/// 签名错误。
#[derive(Debug, thiserror::Error)]
pub enum SigningError {
    /// IO 错误。
    #[error("IO 错误：{0}")]
    IoError(String),

    /// 序列化失败。
    #[error("序列化失败：{0}")]
    SerializationFailed(String),

    /// 反序列化失败。
    #[error("反序列化失败：{0}")]
    DeserializationFailed(String),

    /// 签名生成失败。
    #[error("签名生成失败：{0}")]
    SignatureGenerationFailed(String),

    /// 签名验证失败。
    #[error("签名验证失败：{0}")]
    SignatureVerificationFailed(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_plugin_signature_serialization() {
        let signature = PluginSignature {
            version: "1.0".to_string(),
            signer: SignerInfo {
                name: "Test Signer".to_string(),
                email: Some("test@example.com".to_string()),
                website: None,
                public_key: "test_public_key".to_string(),
            },
            signed_at: "2026-09-06T00:00:00Z".to_string(),
            plugin_id: "com.example.test".to_string(),
            plugin_version: "1.0.0".to_string(),
            file_hashes: HashMap::new(),
            signature: "test_signature".to_string(),
        };

        let json = serde_json::to_string(&signature).unwrap();
        let deserialized: PluginSignature = serde_json::from_str(&json).unwrap();

        assert_eq!(signature.version, deserialized.version);
        assert_eq!(signature.plugin_id, deserialized.plugin_id);
        assert_eq!(signature.plugin_version, deserialized.plugin_version);
    }

    #[test]
    fn test_signature_verification() {
        let manager = PluginSigningManager::new();
        let signature = PluginSignature {
            version: "1.0".to_string(),
            signer: SignerInfo {
                name: "Test Signer".to_string(),
                email: Some("test@example.com".to_string()),
                website: None,
                public_key: "test_public_key".to_string(),
            },
            signed_at: "2026-09-06T00:00:00Z".to_string(),
            plugin_id: "com.example.test".to_string(),
            plugin_version: "1.0.0".to_string(),
            file_hashes: HashMap::new(),
            signature: "test_signature".to_string(),
        };

        let verification = manager.verify_signature(Path::new("."), &signature);
        // 由于签名者不受信任，应该验证失败
        assert!(!verification.valid);
        assert!(!verification.details.signer_trusted);
    }
}
