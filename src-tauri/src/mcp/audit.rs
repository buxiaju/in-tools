//! MCP 调用的审计落盘。
//!
//! 内核 [`Supervisor`] 在每次 `call_tool` 结束时都会产出一条 [`AuditEntry`]，
//! 无论成败。本模块把其中**经 MCP 网关发起**的那部分单独抽出来，
//! 以 JSON Lines 追加写进 `~/.intools/logs/mcp-audit.log`。
//!
//! 为什么要单独一份文件而不是复用 tracing：第三方 AI 客户端是不在场的调用方，
//! 出了问题（比如某个客户端疯狂调用写盘工具）需要能独立、完整地回溯，
//! 而 tracing 的输出级别和滚动策略是给开发者调试用的，两者诉求不同。
//!
//! [`Supervisor`]: crate::runtime::supervisor::Supervisor

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::config::{ensure_dir, paths};
use crate::runtime::supervisor::{AuditEntry, AuditSink, TracingAuditSink};

/// 只有 caller 以此开头的记录才会被写进 MCP 审计日志。
///
/// 与 [`CallerIdentity::label`] 的输出格式绑定：MCP 调用固定是 `mcp:{客户端名}`。
///
/// [`CallerIdentity::label`]: crate::permission::CallerIdentity::label
const MCP_CALLER_PREFIX: &str = "mcp:";

/// 落盘的一行审计记录。
///
/// 相比 [`AuditEntry`] 多了 `timestamp`——内核那边不带时间戳，
/// 因为对内存里的断言来说时间是噪音；但写进文件就必须有，否则日志没法用。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpAuditRecord {
    /// RFC 3339 格式的本地时间，带时区偏移。
    pub timestamp: String,
    pub caller: String,
    pub tool: String,
    pub plugin_id: String,
    pub args_summary: String,
    pub duration_ms: u64,
    pub outcome: String,
}

impl McpAuditRecord {
    fn from_entry(entry: &AuditEntry) -> Self {
        Self {
            timestamp: chrono::Local::now().to_rfc3339(),
            caller: entry.caller.clone(),
            tool: entry.tool.clone(),
            plugin_id: entry.plugin_id.clone(),
            args_summary: entry.args_summary.clone(),
            duration_ms: entry.duration_ms,
            outcome: entry.outcome.clone(),
        }
    }

    /// 调用是否成功。内核成功时写 `"ok"`，失败时写 `"error: {原因}"`。
    pub fn is_ok(&self) -> bool {
        self.outcome == "ok"
    }
}

/// MCP 审计 sink：筛出 MCP 流量写文件，其余原样转交给内层 sink。
///
/// **做成装饰器而不是直接替换**是必须的：[`Supervisor::with_audit_sink`] 是覆盖语义，
/// 若直接挂上去，UI 与插件反向调用的审计就再也不会进 tracing 了。
/// 这里默认内层是 [`TracingAuditSink`]，等于在原有行为上叠加，而不是取而代之。
///
/// [`Supervisor::with_audit_sink`]: crate::runtime::supervisor::Supervisor::with_audit_sink
pub struct McpAuditSink {
    path: PathBuf,
    inner: Arc<dyn AuditSink>,
}

impl std::fmt::Debug for McpAuditSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // 内层 sink 是 trait object，没有 Debug 约束，只打路径。
        f.debug_struct("McpAuditSink")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl McpAuditSink {
    /// 用默认路径 `~/.intools/logs/mcp-audit.log` 建 sink，内层转发到 tracing。
    pub fn new() -> Result<Self, crate::config::PathError> {
        Ok(Self::at(paths::mcp_audit_log()?))
    }

    /// 指定日志路径。给测试与非常规部署用。
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            inner: Arc::new(TracingAuditSink),
        }
    }

    /// 替换内层 sink。链上已有其他 sink 时用它串起来，避免互相覆盖。
    pub fn with_inner(mut self, inner: Arc<dyn AuditSink>) -> Self {
        self.inner = inner;
        self
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 追加一行 JSON。
    ///
    /// 目录可能还不存在（首次调用、或用户手动删过 logs/），所以每次写之前兜底建一次。
    fn append(&self, record: &McpAuditRecord) -> std::io::Result<()> {
        if let Some(parent) = self.path.parent() {
            ensure_dir(parent).map_err(std::io::Error::other)?;
        }
        let mut line = serde_json::to_string(record).map_err(std::io::Error::other)?;
        line.push('\n');

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        file.write_all(line.as_bytes())
    }
}

impl AuditSink for McpAuditSink {
    fn record(&self, entry: AuditEntry) {
        if entry.caller.starts_with(MCP_CALLER_PREFIX) {
            let record = McpAuditRecord::from_entry(&entry);
            // 审计写不进去是运维问题，不该把这次工具调用一起搞崩，
            // 所以只告警不 panic、也不中断转发。
            if let Err(e) = self.append(&record) {
                tracing::warn!(
                    path = %self.path.display(),
                    error = %e,
                    "MCP 审计日志写入失败"
                );
            }
        }
        self.inner.record(entry);
    }
}

/// 读取审计日志的全部记录。给测试和将来的「查看日志」界面用。
///
/// 文件不存在时返回空列表——还没发生过 MCP 调用是正常状态，不是错误。
/// 解析不了的行直接跳过：日志是追加写的，最后一行可能因为进程被杀而截断，
/// 不能让一行坏数据毁掉整份日志的可读性。
pub fn read_audit_log(path: &Path) -> std::io::Result<Vec<McpAuditRecord>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(path)?;
    Ok(text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    fn entry(caller: &str, tool: &str) -> AuditEntry {
        AuditEntry {
            caller: caller.to_string(),
            tool: tool.to_string(),
            plugin_id: "com.demo.ocr".to_string(),
            args_summary: "{\"path\":\"a.png\"}".to_string(),
            duration_ms: 12,
            outcome: "ok".to_string(),
        }
    }

    #[derive(Default)]
    struct CollectingSink {
        seen: StdMutex<Vec<AuditEntry>>,
    }

    impl AuditSink for CollectingSink {
        fn record(&self, entry: AuditEntry) {
            self.seen.lock().unwrap().push(entry);
        }
    }

    fn sink_in(dir: &tempfile::TempDir) -> (McpAuditSink, PathBuf) {
        let path = dir.path().join("logs").join("mcp-audit.log");
        (McpAuditSink::at(&path), path)
    }

    #[test]
    fn mcp调用被写进日志() {
        let dir = tempfile::tempdir().unwrap();
        let (sink, path) = sink_in(&dir);

        sink.record(entry("mcp:claude-desktop", "ocr:recognize"));

        let records = read_audit_log(&path).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].caller, "mcp:claude-desktop");
        assert_eq!(records[0].tool, "ocr:recognize");
        assert_eq!(records[0].plugin_id, "com.demo.ocr");
        assert_eq!(records[0].duration_ms, 12);
        assert!(records[0].is_ok());
    }

    #[test]
    fn 非mcp调用不写入日志() {
        let dir = tempfile::tempdir().unwrap();
        let (sink, path) = sink_in(&dir);

        sink.record(entry("ui", "ocr:recognize"));
        sink.record(entry("plugin:com.demo.other@1", "ocr:recognize"));

        assert!(
            read_audit_log(&path).unwrap().is_empty(),
            "UI 与插件调用不属于 MCP 审计范围"
        );
    }

    #[test]
    fn 记录带有时间戳() {
        let dir = tempfile::tempdir().unwrap();
        let (sink, path) = sink_in(&dir);

        sink.record(entry("mcp:cline", "ocr:recognize"));

        let records = read_audit_log(&path).unwrap();
        let ts = &records[0].timestamp;
        assert!(
            chrono::DateTime::parse_from_rfc3339(ts).is_ok(),
            "时间戳应当是合法 RFC 3339，实际为 {ts}"
        );
    }

    #[test]
    fn 失败的调用同样留痕() {
        let dir = tempfile::tempdir().unwrap();
        let (sink, path) = sink_in(&dir);

        let mut e = entry("mcp:claude-desktop", "fs:write");
        e.outcome = "error: 权限被拒绝".to_string();
        sink.record(e);

        let records = read_audit_log(&path).unwrap();
        assert_eq!(records.len(), 1, "失败的调用恰恰是最需要留痕的");
        assert!(!records[0].is_ok());
        assert!(records[0].outcome.contains("权限被拒绝"));
    }

    #[test]
    fn 多次调用按顺序追加而不是覆盖() {
        let dir = tempfile::tempdir().unwrap();
        let (sink, path) = sink_in(&dir);

        for i in 0..3 {
            sink.record(entry("mcp:claude-desktop", &format!("ocr:tool{i}")));
        }

        let records = read_audit_log(&path).unwrap();
        let tools: Vec<_> = records.iter().map(|r| r.tool.as_str()).collect();
        assert_eq!(tools, vec!["ocr:tool0", "ocr:tool1", "ocr:tool2"]);
    }

    #[test]
    fn 日志目录不存在时自动创建() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a").join("b").join("mcp-audit.log");
        let sink = McpAuditSink::at(&path);

        sink.record(entry("mcp:claude-desktop", "ocr:recognize"));

        assert!(path.exists(), "多级父目录应当被自动建出来");
        assert_eq!(read_audit_log(&path).unwrap().len(), 1);
    }

    #[test]
    fn 所有记录都会转发给内层sink() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp-audit.log");
        let inner = Arc::new(CollectingSink::default());
        let sink = McpAuditSink::at(&path).with_inner(inner.clone());

        sink.record(entry("ui", "ocr:recognize"));
        sink.record(entry("mcp:claude-desktop", "ocr:recognize"));

        let seen = inner.seen.lock().unwrap();
        assert_eq!(
            seen.len(),
            2,
            "装饰器不能吞掉任何记录，否则挂上它就等于关掉了原有审计"
        );
        assert_eq!(seen[0].caller, "ui");
        assert_eq!(seen[1].caller, "mcp:claude-desktop");
    }

    #[test]
    fn 写入失败不影响转发() {
        let dir = tempfile::tempdir().unwrap();
        // 把日志路径指向一个已存在的**目录**，写文件必然失败。
        let path = dir.path().join("occupied");
        std::fs::create_dir(&path).unwrap();

        let inner = Arc::new(CollectingSink::default());
        let sink = McpAuditSink::at(&path).with_inner(inner.clone());

        sink.record(entry("mcp:claude-desktop", "ocr:recognize"));

        assert_eq!(
            inner.seen.lock().unwrap().len(),
            1,
            "落盘失败只该告警，不该中断审计链路"
        );
    }

    #[test]
    fn 截断的坏行不会毁掉整份日志() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp-audit.log");
        let sink = McpAuditSink::at(&path);

        sink.record(entry("mcp:claude-desktop", "ocr:recognize"));
        // 模拟进程被杀导致的半行。
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"{\"timestamp\":\"2026-09\r\n")
            .unwrap();
        sink.record(entry("mcp:cline", "ocr:recognize"));

        let records = read_audit_log(&path).unwrap();
        assert_eq!(records.len(), 2, "坏行被跳过，好行必须都还在");
    }

    #[test]
    fn 日志文件不存在时读出空列表() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("从未写过.log");

        assert!(
            read_audit_log(&path).unwrap().is_empty(),
            "没发生过 MCP 调用是正常状态，不该报错"
        );
    }
}
