//! 网络访问控制策略。
//!
//! 提供细粒度的网络访问控制，包括：
//! - 域名白名单/黑名单
//! - IP 地址范围限制
//! - 端口限制
//! - 协议限制（HTTP/HTTPS/WebSocket 等）

use std::collections::HashSet;
use std::net::IpAddr;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// 网络访问策略。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NetworkPolicy {
    /// 允许访问的域名列表（白名单）。为空时表示不限制。
    pub allowed_domains: HashSet<String>,
    /// 禁止访问的域名列表（黑名单）。优先级高于白名单。
    pub blocked_domains: HashSet<String>,
    /// 允许访问的 IP 地址范围（CIDR 表示法）。为空时表示不限制。
    pub allowed_ip_ranges: Vec<String>,
    /// 禁止访问的 IP 地址范围（CIDR 表示法）。优先级高于允许列表。
    pub blocked_ip_ranges: Vec<String>,
    /// 允许使用的端口列表。为空时表示不限制。
    pub allowed_ports: HashSet<u16>,
    /// 禁止使用的端口列表。优先级高于允许列表。
    pub blocked_ports: HashSet<u16>,
    /// 允许的协议列表。为空时表示不限制。
    pub allowed_protocols: HashSet<String>,
    /// 禁止的协议列表。优先级高于允许列表。
    pub blocked_protocols: HashSet<String>,
}

impl NetworkPolicy {
    /// 创建一个允许所有网络访问的策略。
    pub fn allow_all() -> Self {
        Self::default()
    }

    /// 创建一个禁止所有网络访问的策略。
    pub fn deny_all() -> Self {
        Self {
            blocked_protocols: vec!["http", "https", "ws", "wss", "tcp", "udp"]
                .into_iter()
                .map(String::from)
                .collect(),
            ..Default::default()
        }
    }

    /// 检查是否允许访问指定的域名。
    pub fn is_domain_allowed(&self, domain: &str) -> bool {
        // 黑名单优先
        if self.blocked_domains.contains(domain) {
            return false;
        }

        // 如果有白名单，检查是否在白名单中
        if !self.allowed_domains.is_empty() {
            return self.allowed_domains.contains(domain);
        }

        // 没有白名单，允许所有域名
        true
    }

    /// 检查是否允许访问指定的 IP 地址。
    pub fn is_ip_allowed(&self, ip: &IpAddr) -> bool {
        // 检查黑名单
        for range in &self.blocked_ip_ranges {
            if ip_in_range(ip, range) {
                return false;
            }
        }

        // 如果有白名单，检查是否在白名单中
        if !self.allowed_ip_ranges.is_empty() {
            for range in &self.allowed_ip_ranges {
                if ip_in_range(ip, range) {
                    return true;
                }
            }
            return false;
        }

        // 没有白名单，允许所有 IP
        true
    }

    /// 检查是否允许使用指定的端口。
    pub fn is_port_allowed(&self, port: u16) -> bool {
        // 黑名单优先
        if self.blocked_ports.contains(&port) {
            return false;
        }

        // 如果有白名单，检查是否在白名单中
        if !self.allowed_ports.is_empty() {
            return self.allowed_ports.contains(&port);
        }

        // 没有白名单，允许所有端口
        true
    }

    /// 检查是否允许使用指定的协议。
    pub fn is_protocol_allowed(&self, protocol: &str) -> bool {
        // 黑名单优先
        if self.blocked_protocols.contains(protocol) {
            return false;
        }

        // 如果有白名单，检查是否在白名单中
        if !self.allowed_protocols.is_empty() {
            return self.allowed_protocols.contains(protocol);
        }

        // 没有白名单，允许所有协议
        true
    }

    /// 检查是否允许访问指定的 URL。
    pub fn is_url_allowed(&self, url: &str) -> bool {
        // 解析 URL
        if let Ok(parsed) = url::Url::parse(url) {
            // 检查协议
            if !self.is_protocol_allowed(parsed.scheme()) {
                return false;
            }

            // 检查域名
            if let Some(host) = parsed.host_str() {
                if !self.is_domain_allowed(host) {
                    return false;
                }
            }

            // 检查端口
            if let Some(port) = parsed.port() {
                if !self.is_port_allowed(port) {
                    return false;
                }
            }

            true
        } else {
            // URL 解析失败，拒绝访问
            false
        }
    }

    /// 添加允许的域名。
    pub fn allow_domain(&mut self, domain: &str) {
        self.allowed_domains.insert(domain.to_string());
    }

    /// 添加禁止的域名。
    pub fn block_domain(&mut self, domain: &str) {
        self.blocked_domains.insert(domain.to_string());
    }

    /// 添加允许的 IP 范围。
    pub fn allow_ip_range(&mut self, range: &str) {
        self.allowed_ip_ranges.push(range.to_string());
    }

    /// 添加禁止的 IP 范围。
    pub fn block_ip_range(&mut self, range: &str) {
        self.blocked_ip_ranges.push(range.to_string());
    }

    /// 添加允许的端口。
    pub fn allow_port(&mut self, port: u16) {
        self.allowed_ports.insert(port);
    }

    /// 添加禁止的端口。
    pub fn block_port(&mut self, port: u16) {
        self.blocked_ports.insert(port);
    }

    /// 添加允许的协议。
    pub fn allow_protocol(&mut self, protocol: &str) {
        self.allowed_protocols.insert(protocol.to_string());
    }

    /// 添加禁止的协议。
    pub fn block_protocol(&mut self, protocol: &str) {
        self.blocked_protocols.insert(protocol.to_string());
    }
}

/// 检查 IP 地址是否在 CIDR 范围内。
fn ip_in_range(ip: &IpAddr, range: &str) -> bool {
    // 简单实现：检查是否是单个 IP 或 CIDR 范围
    if range.contains('/') {
        // CIDR 范围
        if let Ok(network) = range.parse::<ipnetwork::IpNetwork>() {
            return network.contains(*ip);
        }
    } else {
        // 单个 IP
        if let Ok(single_ip) = IpAddr::from_str(range) {
            return *ip == single_ip;
        }
    }
    false
}

/// 网络访问违规。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NetworkViolation {
    /// 协议被禁止。
    ProtocolBlocked { protocol: String },
    /// 域名被禁止。
    DomainBlocked { domain: String },
    /// IP 地址被禁止。
    IpBlocked { ip: String },
    /// 端口被禁止。
    PortBlocked { port: u16 },
}

impl std::fmt::Display for NetworkViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NetworkViolation::ProtocolBlocked { protocol } => {
                write!(f, "协议被禁止：{}", protocol)
            }
            NetworkViolation::DomainBlocked { domain } => {
                write!(f, "域名被禁止：{}", domain)
            }
            NetworkViolation::IpBlocked { ip } => {
                write!(f, "IP 地址被禁止：{}", ip)
            }
            NetworkViolation::PortBlocked { port } => {
                write!(f, "端口被禁止：{}", port)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_policy_allows_all() {
        let policy = NetworkPolicy::default();
        assert!(policy.is_domain_allowed("example.com"));
        assert!(policy.is_protocol_allowed("http"));
        assert!(policy.is_port_allowed(80));
    }

    #[test]
    fn test_deny_all_policy() {
        let policy = NetworkPolicy::deny_all();
        assert!(!policy.is_protocol_allowed("http"));
        assert!(!policy.is_protocol_allowed("https"));
        assert!(!policy.is_protocol_allowed("ws"));
    }

    #[test]
    fn test_domain_whitelist() {
        let mut policy = NetworkPolicy::default();
        policy.allow_domain("example.com");
        policy.allow_domain("test.com");

        assert!(policy.is_domain_allowed("example.com"));
        assert!(policy.is_domain_allowed("test.com"));
        assert!(!policy.is_domain_allowed("other.com"));
    }

    #[test]
    fn test_domain_blacklist() {
        let mut policy = NetworkPolicy::default();
        policy.block_domain("evil.com");

        assert!(policy.is_domain_allowed("example.com"));
        assert!(!policy.is_domain_allowed("evil.com"));
    }

    #[test]
    fn test_port_whitelist() {
        let mut policy = NetworkPolicy::default();
        policy.allow_port(80);
        policy.allow_port(443);

        assert!(policy.is_port_allowed(80));
        assert!(policy.is_port_allowed(443));
        assert!(!policy.is_port_allowed(8080));
    }

    #[test]
    fn test_port_blacklist() {
        let mut policy = NetworkPolicy::default();
        policy.block_port(22);

        assert!(policy.is_port_allowed(80));
        assert!(!policy.is_port_allowed(22));
    }

    #[test]
    fn test_url_validation() {
        let mut policy = NetworkPolicy::default();
        policy.allow_domain("example.com");
        policy.allow_protocol("https");
        policy.allow_port(443);

        assert!(policy.is_url_allowed("https://example.com/path"));
        assert!(!policy.is_url_allowed("http://example.com/path")); // 协议被禁止
        assert!(!policy.is_url_allowed("https://other.com/path")); // 域名被禁止
    }
}
