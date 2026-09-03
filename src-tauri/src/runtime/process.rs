//! 子进程管理：spawn、stdio 接管、stderr 转存、优雅关闭。
//!
//! 本模块把「一个插件子进程」包装成 [`StdioTransport`]，它实现
//! [`Transport`](crate::runtime::transport::Transport) trait，因此
//! [`PluginInstance`](crate::runtime::instance::PluginInstance) 无需任何改动就能
//! 从 `MockTransport` 切换到真实进程。
//!
//! # 三条后台任务
//!
//! spawn 成功后会常驻两条任务（第三条是宿主侧的读循环，由 `instance` 启动）：
//!
//! - **stdout 任务**：`read` → [`LineCodec`] 逐行解码 → 推入 mpsc 队列。
//!   `recv()` 只是从队列取，于是「无消息时挂起」「EOF 即断开」两个语义天然成立。
//! - **stderr 任务**：全量转存到日志文件。**即使不配置日志路径也必须读**，
//!   否则管道缓冲写满会把子进程卡死。
//!
//! 为什么 stdout 不放在 `recv()` 里直接读？因为 `recv(&self)` 取的是共享引用，
//! 直接持有 `ChildStdout` 就得加锁，并发调用会互相阻塞；而 `LineCodec` 的半包
//! 缓冲是有状态的，放进独立任务里所有权最干净。
//!
//! # Windows 残留进程防护
//!
//! 见 [`job`] 模块：宿主级全局 Job Object + `KILL_ON_JOB_CLOSE`。

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{mpsc, Mutex as AsyncMutex};

use crate::protocol::message::{
    encode_line, IncomingMessage, JsonRpcRequest, LineCodec, OutgoingMessage, RequestId,
};
use crate::runtime::transport::{Transport, TransportError};

/// 关停宽限期：先请插件自己退出，超过这个时间还赖着就强杀。
///
/// 设计文档只写了「超时未退出则强杀」而未给数值。这里取 5 秒：
/// 足够一个脚本插件跑完 flush 与清理，又不至于让宿主退出时明显卡顿。
pub const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

/// 关停请求的方法名。
pub const METHOD_SHUTDOWN: &str = "plugin/shutdown";

/// 关停请求使用的固定 ID。
///
/// 它刻意**不登记进** `PendingTable`：我们等的是「进程退出」而不是「响应到达」。
/// 插件即便回了响应，也只会在 `read_loop` 里配对失败后被安静丢弃。
const SHUTDOWN_REQUEST_ID: &str = "shutdown";

// ─────────────────── 错误 ───────────────────

/// 子进程启动阶段的错误。
///
/// 只覆盖 spawn 相关失败；进程跑起来之后的问题一律走 [`TransportError`]。
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ProcessError {
    /// 可执行文件不存在、没有执行权限、或 `cwd` 无效。
    #[error("启动插件 {plugin_id} 失败：无法执行 `{command}`（{message}）")]
    Spawn {
        plugin_id: String,
        command: String,
        message: String,
    },

    /// 插件目录不存在——提前拦下，否则 spawn 的报错会含糊其辞。
    #[error("插件 {plugin_id} 的目录不存在：{path}")]
    MissingPluginDir { plugin_id: String, path: String },

    /// 子进程的标准流没能接管成功。正常情况下不会发生。
    #[error("插件 {plugin_id} 的标准流接管失败：{stream}")]
    Stdio { plugin_id: String, stream: String },

    /// stderr 日志文件打不开（目录创建失败、无写权限等）。
    #[error("插件 {plugin_id} 的 stderr 日志无法写入 {path}：{message}")]
    StderrLog {
        plugin_id: String,
        path: String,
        message: String,
    },
}

// ─────────────────── 启动参数 ───────────────────

/// 拉起一个插件子进程所需的全部信息。
#[derive(Debug, Clone)]
pub struct ProcessConfig {
    /// 插件 ID，用于日志与错误信息。
    pub plugin_id: String,
    /// 工作目录，会成为子进程的 `cwd`，插件可据此定位自己的资源文件。
    pub plugin_dir: PathBuf,
    /// 可执行文件，来自 manifest 的 `exec.command`。
    pub command: String,
    /// 命令行参数，来自 manifest 的 `exec.args`。
    pub args: Vec<String>,
    /// stderr 转存目标。`None` 表示只读取丢弃（仍必须读，防止管道写满）。
    ///
    /// 做成参数而不是内部调用 `paths::plugin_stderr_log()`，是为了让测试能指向
    /// 临时目录，不去碰用户真实的 `~/.intools/logs`。
    pub stderr_log: Option<PathBuf>,
    /// 关停宽限期。
    pub shutdown_grace: Duration,
}

impl ProcessConfig {
    /// 以默认宽限期、无 stderr 转存构造。
    pub fn new(
        plugin_id: impl Into<String>,
        plugin_dir: impl Into<PathBuf>,
        command: impl Into<String>,
        args: Vec<String>,
    ) -> Self {
        Self {
            plugin_id: plugin_id.into(),
            plugin_dir: plugin_dir.into(),
            command: command.into(),
            args,
            stderr_log: None,
            shutdown_grace: SHUTDOWN_GRACE,
        }
    }

    /// 指定 stderr 转存路径。
    pub fn with_stderr_log(mut self, path: impl Into<PathBuf>) -> Self {
        self.stderr_log = Some(path.into());
        self
    }

    /// 覆盖关停宽限期。
    pub fn with_shutdown_grace(mut self, grace: Duration) -> Self {
        self.shutdown_grace = grace;
        self
    }
}

// ─────────────────── 传输实现 ───────────────────

/// 基于子进程标准流的传输实现。
pub struct StdioTransport {
    plugin_id: String,
    /// 子进程句柄。`close()` 会把它取走用于 wait/kill，取走后即为 `None`。
    child: AsyncMutex<Option<Child>>,
    /// 子进程 stdin。关停时主动置空以向插件发送 EOF。
    stdin: AsyncMutex<Option<ChildStdin>>,
    /// stdout 任务解码出的入站消息队列。
    inbound_rx: AsyncMutex<mpsc::UnboundedReceiver<IncomingMessage>>,
    closed: AtomicBool,
    shutdown_grace: Duration,
}

impl std::fmt::Debug for StdioTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StdioTransport")
            .field("plugin_id", &self.plugin_id)
            .field("closed", &self.closed.load(Ordering::SeqCst))
            .finish()
    }
}

impl StdioTransport {
    /// 插件 ID。
    pub fn plugin_id(&self) -> &str {
        &self.plugin_id
    }

    /// 通道是否已关闭。
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    /// 子进程 PID；进程已被回收后返回 `None`。
    pub async fn pid(&self) -> Option<u32> {
        self.child.lock().await.as_ref().and_then(|c| c.id())
    }
}

/// 拉起插件子进程，返回可直接交给 `PluginInstance` 的传输通道。
///
/// 启动顺序刻意如此：先校验目录 → 再开日志文件 → 最后才 spawn。
/// 让「可预见的配置错误」在拉起进程之前就暴露，避免留下孤儿进程。
pub async fn spawn_plugin(config: ProcessConfig) -> Result<Arc<StdioTransport>, ProcessError> {
    if !config.plugin_dir.is_dir() {
        return Err(ProcessError::MissingPluginDir {
            plugin_id: config.plugin_id.clone(),
            path: config.plugin_dir.display().to_string(),
        });
    }

    let stderr_file = match &config.stderr_log {
        Some(path) => Some(open_stderr_log(&config.plugin_id, path).await?),
        None => None,
    };

    let mut command = Command::new(&config.command);
    command
        .args(&config.args)
        .current_dir(&config.plugin_dir)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        // tokio 侧兜底：Child 被丢弃时顺手杀掉。它只在 runtime 存活时有效，
        // 覆盖不了宿主崩溃，真正的兜底是下面的 Job Object。
        .kill_on_drop(true);

    let mut child = command.spawn().map_err(|e| ProcessError::Spawn {
        plugin_id: config.plugin_id.clone(),
        command: config.command.clone(),
        message: e.to_string(),
    })?;

    // 进程一起来立刻并入 Job，缩短「已存在但不受管」的窗口。
    if let Some(pid) = child.id() {
        job::assign_current_process_job(&child, pid, &config.plugin_id);
    }

    let stdin = child.stdin.take().ok_or_else(|| ProcessError::Stdio {
        plugin_id: config.plugin_id.clone(),
        stream: "stdin".to_string(),
    })?;
    let stdout = child.stdout.take().ok_or_else(|| ProcessError::Stdio {
        plugin_id: config.plugin_id.clone(),
        stream: "stdout".to_string(),
    })?;
    let stderr = child.stderr.take().ok_or_else(|| ProcessError::Stdio {
        plugin_id: config.plugin_id.clone(),
        stream: "stderr".to_string(),
    })?;

    let (inbound_tx, inbound_rx) = mpsc::unbounded_channel();

    let stdout_id = config.plugin_id.clone();
    tokio::spawn(async move {
        pump_stdout(stdout_id, stdout, inbound_tx).await;
    });

    let stderr_id = config.plugin_id.clone();
    tokio::spawn(async move {
        pump_stderr(stderr_id, stderr, stderr_file).await;
    });

    Ok(Arc::new(StdioTransport {
        plugin_id: config.plugin_id,
        child: AsyncMutex::new(Some(child)),
        stdin: AsyncMutex::new(Some(stdin)),
        inbound_rx: AsyncMutex::new(inbound_rx),
        closed: AtomicBool::new(false),
        shutdown_grace: config.shutdown_grace,
    }))
}

/// 以追加模式打开 stderr 日志，必要时创建父目录。
async fn open_stderr_log(plugin_id: &str, path: &Path) -> Result<tokio::fs::File, ProcessError> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| ProcessError::StderrLog {
                plugin_id: plugin_id.to_string(),
                path: path.display().to_string(),
                message: e.to_string(),
            })?;
    }

    tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await
        .map_err(|e| ProcessError::StderrLog {
            plugin_id: plugin_id.to_string(),
            path: path.display().to_string(),
            message: e.to_string(),
        })
}

/// stdout 泵：读字节 → 逐行解码 → 投递。
///
/// 非法 JSON 不中断循环：`LineCodec` 已经把那一整行丢掉了，记一条告警继续读，
/// 这样插件偶尔往 stdout 打印一行调试文本不会让整个通道崩掉。
async fn pump_stdout(
    plugin_id: String,
    mut stdout: tokio::process::ChildStdout,
    tx: mpsc::UnboundedSender<IncomingMessage>,
) {
    let mut codec = LineCodec::new();
    let mut buf = vec![0u8; 8192];

    loop {
        let n = match stdout.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => {
                tracing::warn!(plugin_id = %plugin_id, error = %e, "插件 stdout 读取失败");
                break;
            }
        };
        codec.feed(&buf[..n]);

        loop {
            match codec.decode() {
                Ok(Some(msg)) => {
                    if tx.send(msg).is_err() {
                        // 接收端已丢弃，说明传输已被关停，无需再读。
                        return;
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    tracing::warn!(
                        plugin_id = %plugin_id,
                        error = %e,
                        "插件 stdout 输出了非法 JSON 行，已丢弃该行"
                    );
                }
            }
        }
    }

    tracing::debug!(plugin_id = %plugin_id, "插件 stdout 已关闭");
}

/// stderr 泵：全量转存日志文件。
///
/// 即便 `file` 为 `None` 也要把数据读干净——管道缓冲区写满会让子进程在写 stderr
/// 时永久阻塞，表现为「插件莫名其妙卡住」，是极难排查的一类故障。
async fn pump_stderr(
    plugin_id: String,
    stderr: tokio::process::ChildStderr,
    file: Option<tokio::fs::File>,
) {
    let mut reader = BufReader::new(stderr).lines();
    let mut file = file;

    loop {
        match reader.next_line().await {
            Ok(Some(line)) => {
                tracing::debug!(plugin_id = %plugin_id, "[stderr] {line}");
                if let Some(f) = file.as_mut() {
                    let record = format!("{line}\n");
                    if let Err(e) = f.write_all(record.as_bytes()).await {
                        tracing::warn!(plugin_id = %plugin_id, error = %e, "写入 stderr 日志失败");
                        file = None;
                    }
                }
            }
            Ok(None) => break,
            Err(e) => {
                tracing::warn!(plugin_id = %plugin_id, error = %e, "插件 stderr 读取失败");
                break;
            }
        }
    }

    if let Some(f) = file.as_mut() {
        let _ = f.flush().await;
    }
    tracing::debug!(plugin_id = %plugin_id, "插件 stderr 已关闭");
}

#[async_trait]
impl Transport for StdioTransport {
    async fn send(&self, msg: OutgoingMessage) -> Result<(), TransportError> {
        if self.is_closed() {
            return Err(TransportError::Closed);
        }

        let bytes = encode_line(&msg).map_err(|e| TransportError::Encode {
            message: e.to_string(),
        })?;

        let mut guard = self.stdin.lock().await;
        let stdin = guard.as_mut().ok_or(TransportError::Closed)?;

        stdin
            .write_all(&bytes)
            .await
            .map_err(|e| TransportError::Io {
                message: e.to_string(),
            })?;
        stdin.flush().await.map_err(|e| TransportError::Io {
            message: e.to_string(),
        })?;

        Ok(())
    }

    async fn recv(&self) -> Result<IncomingMessage, TransportError> {
        if self.is_closed() {
            return Err(TransportError::Closed);
        }

        let mut rx = self.inbound_rx.lock().await;
        match rx.recv().await {
            Some(msg) => Ok(msg),
            // 发送端全部析构 = stdout 已 EOF = 子进程退出或关闭了输出流。
            None => {
                self.closed.store(true, Ordering::SeqCst);
                Err(TransportError::Closed)
            }
        }
    }

    /// 优雅关停：`plugin/shutdown` → 关 stdin 送 EOF → 等退出 → 超时强杀。
    ///
    /// 两级信号是有意为之：讲究的插件会响应 `plugin/shutdown`，
    /// 而只会读 stdin 的简单插件靠 EOF 也能感知到该退出了。
    async fn close(&self) {
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }

        {
            let mut guard = self.stdin.lock().await;
            if let Some(stdin) = guard.as_mut() {
                let request = OutgoingMessage::Request(JsonRpcRequest::new(
                    RequestId::Str(SHUTDOWN_REQUEST_ID.to_string()),
                    METHOD_SHUTDOWN,
                    serde_json::json!({}),
                ));
                if let Ok(bytes) = encode_line(&request) {
                    let _ = stdin.write_all(&bytes).await;
                    let _ = stdin.flush().await;
                }
            }
            // 丢弃 stdin 句柄 → 子进程读到 EOF。
            guard.take();
        }

        let mut guard = self.child.lock().await;
        let Some(mut child) = guard.take() else {
            return;
        };

        match tokio::time::timeout(self.shutdown_grace, child.wait()).await {
            Ok(Ok(status)) => {
                tracing::debug!(plugin_id = %self.plugin_id, ?status, "插件已自行退出");
            }
            Ok(Err(e)) => {
                tracing::warn!(plugin_id = %self.plugin_id, error = %e, "等待插件退出失败");
            }
            Err(_) => {
                tracing::warn!(
                    plugin_id = %self.plugin_id,
                    grace_sec = self.shutdown_grace.as_secs(),
                    "插件未在宽限期内退出，强制终止"
                );
                let _ = child.kill().await;
            }
        }
    }
}

// ─────────────────── Windows Job Object ───────────────────

/// 防止插件子进程在宿主消失后残留。
///
/// # 为什么是「宿主级全局 Job」而不是「每进程一个 Job」
///
/// `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` 的语义是：**最后一个 job 句柄被关闭时，
/// 杀光 job 内所有进程**。宿主无论正常退出还是崩溃（乃至被任务管理器结束），
/// 内核都会回收它的句柄表，job 句柄随之关闭，成员进程被一并清理。
///
/// 这是唯一能覆盖「宿主崩溃」的方案——`Drop` 里手动 kill 在崩溃时根本不会执行。
/// 全局单例还顺带保证了 job 句柄的生命周期与进程等长，不会被提前释放。
///
/// 创建时 `lpJobAttributes` 传 NULL，等价于 `bInheritHandle = FALSE`。这点很关键：
/// 若子进程继承了 job 句柄，「最后一个句柄」就永远关不掉，整个机制会失效。
#[cfg(windows)]
mod job {
    use std::sync::OnceLock;

    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, SetInformationJobObject,
        JobObjectExtendedLimitInformation, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };

    /// 裸句柄包装：`HANDLE` 是裸指针，默认不是 `Send`/`Sync`。
    /// Job 句柄创建后只读不改，跨线程共享是安全的。
    struct JobHandle(HANDLE);
    unsafe impl Send for JobHandle {}
    unsafe impl Sync for JobHandle {}

    static JOB: OnceLock<Option<JobHandle>> = OnceLock::new();

    fn global_job() -> Option<HANDLE> {
        JOB.get_or_init(|| unsafe {
            let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if handle.is_null() {
                tracing::warn!("创建 Job Object 失败，插件进程将失去残留防护");
                return None;
            }

            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;

            let ok = SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                std::ptr::addr_of!(info) as *const core::ffi::c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            if ok == 0 {
                tracing::warn!("配置 Job Object 失败，插件进程将失去残留防护");
                return None;
            }

            Some(JobHandle(handle))
        })
        .as_ref()
        .map(|j| j.0)
    }

    /// 把子进程并入全局 job。失败只告警不阻断启动——
    /// 少一层兜底也比插件直接起不来强。
    pub fn assign_current_process_job(child: &tokio::process::Child, pid: u32, plugin_id: &str) {
        let Some(job) = global_job() else {
            return;
        };
        let Some(handle) = child.raw_handle() else {
            return;
        };

        let ok = unsafe { AssignProcessToJobObject(job, handle as HANDLE) };
        if ok == 0 {
            tracing::warn!(
                plugin_id = %plugin_id,
                pid,
                "无法把插件进程并入 Job Object，宿主异常退出时可能残留"
            );
        }
    }
}

/// 非 Windows 平台：内核没有等价机制，依赖 `kill_on_drop` 与显式 `close()`。
#[cfg(not(windows))]
mod job {
    pub fn assign_current_process_job(
        _child: &tokio::process::Child,
        _pid: u32,
        _plugin_id: &str,
    ) {
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> tempfile::TempDir {
        tempfile::tempdir().expect("创建临时目录失败")
    }

    #[test]
    fn 配置构造器填入默认宽限期() {
        let cfg = ProcessConfig::new("com.example.demo", ".", "python", vec!["main.py".into()]);
        assert_eq!(cfg.shutdown_grace, SHUTDOWN_GRACE);
        assert!(cfg.stderr_log.is_none());
        assert_eq!(cfg.args, vec!["main.py".to_string()]);
    }

    #[test]
    fn 配置构造器可覆盖日志与宽限期() {
        let cfg = ProcessConfig::new("com.example.demo", ".", "python", vec![])
            .with_stderr_log("/tmp/demo.log")
            .with_shutdown_grace(Duration::from_millis(200));
        assert_eq!(cfg.stderr_log, Some(PathBuf::from("/tmp/demo.log")));
        assert_eq!(cfg.shutdown_grace, Duration::from_millis(200));
    }

    #[tokio::test]
    async fn 插件目录不存在时提前报错() {
        let cfg = ProcessConfig::new(
            "com.example.demo",
            "./definitely-not-here-4f2a",
            "python",
            vec![],
        );
        let err = spawn_plugin(cfg).await.expect_err("应当拒绝启动");
        assert!(matches!(err, ProcessError::MissingPluginDir { .. }));
    }

    #[tokio::test]
    async fn 可执行文件不存在时报错包含命令名() {
        let dir = temp_dir();
        let cfg = ProcessConfig::new(
            "com.example.demo",
            dir.path(),
            "intools-no-such-binary-9c1d",
            vec![],
        );
        let err = spawn_plugin(cfg).await.expect_err("应当启动失败");

        match err {
            ProcessError::Spawn {
                ref command,
                ref plugin_id,
                ..
            } => {
                assert_eq!(command, "intools-no-such-binary-9c1d");
                assert_eq!(plugin_id, "com.example.demo");
            }
            other => panic!("错误类型不对：{other:?}"),
        }
        // 错误信息要能直接给用户看。
        assert!(err.to_string().contains("intools-no-such-binary-9c1d"));
    }

    #[tokio::test]
    async fn stderr日志目录会被自动创建() {
        let dir = temp_dir();
        let log_path = dir.path().join("nested").join("deep").join("demo.log");
        let file = open_stderr_log("com.example.demo", &log_path).await;
        assert!(file.is_ok(), "应当自动创建父目录");
        assert!(log_path.exists());
    }
}
