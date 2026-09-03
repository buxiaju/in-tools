//! 真实子进程的端到端验证。
//!
//! 单测里的 `MockTransport` 只能证明状态机逻辑自洽，证明不了「真的能和一个
//! 外部进程说上话」。这里全程使用 `plugins/hello-plugin` 的真实 Python 进程，
//! 覆盖 spawn → 握手 → 调用 → 关停的完整链路，以及各类异常收场。

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use intools::protocol::manifest::Lifecycle;
use intools::runtime::instance::{InstanceError, InstanceState, PluginInstance};
use intools::runtime::process::{spawn_plugin, ProcessConfig, StdioTransport};
use intools::runtime::transport::Transport;
use serde_json::json;

const PLUGIN_ID: &str = "com.intools.hello";

/// 示范插件所在目录：`<crate>/../plugins/hello-plugin`。
///
/// 用 `CARGO_MANIFEST_DIR` 而非当前工作目录——测试进程的 cwd 并无保证。
fn plugin_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("src-tauri 应有父目录")
        .join("plugins")
        .join("hello-plugin")
}

/// Windows 上可执行文件常见为 `python`，其余平台优先 `python3`。
fn python() -> &'static str {
    if cfg!(windows) {
        "python"
    } else {
        "python3"
    }
}

fn hello_config() -> ProcessConfig {
    ProcessConfig::new(
        PLUGIN_ID,
        plugin_dir(),
        python(),
        vec!["-u".to_string(), "main.py".to_string()],
    )
}

/// 拉起插件并完成握手，返回可直接调用的实例。
async fn started_instance() -> (Arc<PluginInstance>, Arc<StdioTransport>) {
    let transport = spawn_plugin(hello_config()).await.expect("插件应能启动");
    let instance = Arc::new(PluginInstance::new(
        PLUGIN_ID,
        Lifecycle::default(),
        transport.clone(),
    ));

    instance
        .start(json!({}), plugin_dir().display().to_string())
        .await
        .expect("握手应当成功");

    (instance, transport)
}

#[tokio::test]
async fn 真实插件可完成握手并上报工具清单() {
    let (instance, transport) = started_instance().await;

    assert_eq!(instance.state(), InstanceState::Idle);

    let info = instance.handshake().expect("握手信息应已记录");
    assert_eq!(info.protocol_version.to_string(), "1.0");
    assert!(
        info.full_feature,
        "插件与宿主同为 1.0，应判定为完整特性支持"
    );

    let names: Vec<&str> = info.tools.iter().map(|t| t.name.as_str()).collect();
    assert!(names.contains(&"hello:echo"), "工具清单应含 hello:echo");
    assert!(names.contains(&"hello:crash"), "工具清单应含 hello:crash");

    instance.stop().await;
    assert!(transport.is_closed());
}

#[tokio::test]
async fn 调用hello_echo可原样取回文本() {
    let (instance, _transport) = started_instance().await;

    // 特意用中文与 emoji：跨进程管道的编码问题只有非 ASCII 才暴露得出来。
    let text = "你好，InTools 🚀";
    let result = instance
        .call(
            "tools/call",
            json!({ "name": "hello:echo", "arguments": { "text": text } }),
        )
        .await
        .expect("echo 调用应成功");

    assert_eq!(result["text"], json!(text));
    assert_eq!(instance.state(), InstanceState::Idle, "调用完应回到 Idle");

    instance.stop().await;
}

#[tokio::test]
async fn 连续多次调用共用同一进程() {
    let (instance, transport) = started_instance().await;
    let pid = transport.pid().await;

    for i in 0..5 {
        let result = instance
            .call(
                "tools/call",
                json!({ "name": "hello:echo", "arguments": { "text": format!("第 {i} 次") } }),
            )
            .await
            .expect("每次调用都应成功");
        assert_eq!(result["text"], json!(format!("第 {i} 次")));
    }

    assert_eq!(transport.pid().await, pid, "多次调用不应重启进程");
    instance.stop().await;
}

#[tokio::test]
async fn 插件返回的错误会转成插件错误() {
    let (instance, _transport) = started_instance().await;

    // 缺少必填的 text，插件应回 JSON-RPC error 而非崩溃。
    let err = instance
        .call("tools/call", json!({ "name": "hello:echo", "arguments": {} }))
        .await
        .expect_err("缺参数应当报错");

    match err {
        InstanceError::Plugin { code, .. } => assert_eq!(code, -32602),
        other => panic!("期望插件级错误，实际为 {other:?}"),
    }

    // 关键：一次业务错误不能污染实例状态，后续调用仍要能用。
    assert_eq!(instance.state(), InstanceState::Idle);
    let ok = instance
        .call(
            "tools/call",
            json!({ "name": "hello:echo", "arguments": { "text": "仍然可用" } }),
        )
        .await
        .expect("错误之后应仍可正常调用");
    assert_eq!(ok["text"], json!("仍然可用"));

    instance.stop().await;
}

#[tokio::test]
async fn 优雅关停后进程真正退出() {
    let (instance, transport) = started_instance().await;
    let pid = transport.pid().await.expect("应能取到 pid");

    // 自检：先确认探针对「活着的进程」确实返回 true。
    // 否则下面那句「不应残留」会在探针失效时永远通过，成为假绿。
    assert!(进程存活(pid), "关停前插件进程应当存活，否则探针不可信");

    instance.stop().await;

    assert_eq!(instance.state(), InstanceState::Stopped);
    assert!(transport.is_closed());
    assert!(
        transport.pid().await.is_none(),
        "close() 已回收 Child，pid 应不再可得"
    );
    assert!(!进程存活(pid), "宽限期内插件应已退出，不应残留");

    // 关停后再调用要被状态机挡住，而不是打到已死的管道上。
    let err = instance
        .call("tools/call", json!({ "name": "hello:echo", "arguments": { "text": "x" } }))
        .await
        .expect_err("已停止的实例不应接受请求");
    assert!(matches!(err, InstanceError::NotReady { .. }));
}

#[tokio::test]
async fn 重复关停是幂等的() {
    let (instance, transport) = started_instance().await;

    instance.stop().await;
    // 第二次不得 panic，也不得挂起在 child.wait() 上。
    instance.stop().await;
    transport.close().await;

    assert_eq!(instance.state(), InstanceState::Stopped);
}

#[tokio::test]
async fn 插件崩溃后待处理请求以错误收场() {
    let (instance, _transport) = started_instance().await;

    // hello:crash 用 os._exit 猝死，不会回响应。宿主只能靠 stdout 的 EOF
    // 察觉断连，进而把在途请求全部失败掉——这正是要验证的路径。
    let err = instance
        .call("tools/call", json!({ "name": "hello:crash", "arguments": {} }))
        .await
        .expect_err("崩溃时调用不应成功返回");

    // 断连由 read_loop 统一收口：pending 表被 fail_all 成内部错误。
    // 不能只断言「不是超时」——那样插件回一个普通业务错误也会通过，
    // 验不到「进程猝死时在途请求被正确收场」这条真正关心的路径。
    match &err {
        InstanceError::Plugin { code, message } => {
            assert_eq!(*code, -32603, "应为内部错误码，实际消息：{message}");
            assert!(
                message.contains("断开"),
                "错误信息应指明是连接断开：{message}"
            );
        }
        InstanceError::Transport { .. } => {}
        other => panic!("期望断连类错误，实际为 {other:?}"),
    }

    // 状态机应识别出这是非预期断开。
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(instance.state(), InstanceState::Error);
    assert!(instance.last_error().is_some(), "应记录断开原因供 UI 展示");
}

#[tokio::test]
async fn 可执行文件不存在时报错清晰() {
    let config = ProcessConfig::new(
        PLUGIN_ID,
        plugin_dir(),
        "definitely-not-an-executable-xyz",
        vec![],
    );

    let err = spawn_plugin(config).await.expect_err("不存在的命令应启动失败");
    let message = err.to_string();

    assert!(
        message.contains("definitely-not-an-executable-xyz"),
        "错误信息应含命令名，便于用户定位 manifest 里写错的 command：{message}"
    );
}

#[tokio::test]
async fn 插件目录不存在时不会拉起进程() {
    let config = ProcessConfig::new(PLUGIN_ID, plugin_dir().join("不存在的子目录"), python(), vec![]);

    let err = spawn_plugin(config).await.expect_err("目录不存在应提前失败");
    assert!(err.to_string().contains("目录不存在"), "实际：{err}");
}

#[tokio::test]
async fn stderr被转存到指定日志文件() {
    let dir = tempfile::tempdir().expect("应能建临时目录");
    // 刻意指向多层未创建的路径，一并验证父目录会被自动建出来。
    let log_path = dir.path().join("logs").join("hello.log");

    let transport = spawn_plugin(hello_config().with_stderr_log(&log_path))
        .await
        .expect("插件应能启动");
    let instance = Arc::new(PluginInstance::new(
        PLUGIN_ID,
        Lifecycle::default(),
        transport,
    ));
    instance
        .start(json!({}), plugin_dir().display().to_string())
        .await
        .expect("握手应当成功");

    // 插件在响应 plugin/ready 时会往 stderr 写一行「握手完成」。
    instance.stop().await;

    // 泵任务是独立 tokio 任务，进程退出后还需一瞬把尾巴刷完。
    let content = 轮询读取(&log_path, "握手完成").await;
    assert!(
        content.contains("握手完成"),
        "stderr 应被完整转存，实际内容：{content}"
    );
}

/// 宿主异常退出时，插件不应残留。
///
/// 这条验的是 Windows Job Object，而 `优雅关停后进程真正退出` 证明不了它——
/// 那条走的是 `close()` 里的正常路径，宿主崩溃时没有任何 Rust 代码有机会执行。
/// 宿主就是测试进程本身，不能真把它杀掉，于是让测试重新执行自身二进制，
/// 派生出一个「会被强杀的宿主」来充当被试。
#[cfg(windows)]
#[tokio::test]
async fn 宿主被强杀后插件不残留() {
    let exe = std::env::current_exe().expect("应能取到测试二进制路径");
    let mut host = std::process::Command::new(exe)
        // 过滤串是位置参数，`--exact` 只是修饰它的开关。
        .args([
            "扮演宿主拉起插件并挂起",
            "--exact",
            "--ignored",
            "--nocapture",
        ])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("应能重新执行测试二进制");

    let stdout = host.stdout.take().expect("上一行已声明 piped");
    let plugin_pid = 读取宿主上报的pid(stdout);

    // 自检：强杀之前插件必须确实活着，否则后面那句断言不成立也测不出问题。
    assert!(进程存活(plugin_pid), "强杀宿主前，插件应当存活");

    // kill() 在 Windows 上走 TerminateProcess，不给宿主任何清理机会，等价于崩溃。
    host.kill().expect("应能强杀宿主");
    host.wait().ok();

    // 内核回收句柄表、进而关闭 job 是异步的，给它一点时间而不是立刻断言。
    assert!(
        轮询直到进程消亡(plugin_pid).await,
        "宿主崩溃后插件仍在运行（pid {plugin_pid}），Job Object 未生效"
    );
}

/// 供上一条测试作为子进程调用，不参与常规测试轮次。
///
/// 刻意不用 hello-plugin 当被试：宿主一死，stdin 管道随之关闭，hello-plugin
/// 读到 EOF 会自行退出——那样即便 Job Object 完全失效，测试也照样是绿的。
/// 这里的被试进程对 stdin 全然无感，只有被外力杀死才会消失。
#[cfg(windows)]
#[tokio::test]
#[ignore = "由 宿主被强杀后插件不残留 作为子进程调用"]
async fn 扮演宿主拉起插件并挂起() {
    let config = ProcessConfig::new(
        PLUGIN_ID,
        plugin_dir(),
        python(),
        vec!["-c".to_string(), "import time; time.sleep(600)".to_string()],
    );

    let transport = spawn_plugin(config).await.expect("插件应能启动");
    let pid = transport.pid().await.expect("应能取到 pid");

    println!("{pid}");
    // 父进程正阻塞在读这一行上，必须立刻刷出去。
    use std::io::Write;
    std::io::stdout().flush().ok();

    // 挂起等待被强杀。设上限是为了万一父进程失手，也不会留下长命孤儿。
    tokio::time::sleep(Duration::from_secs(60)).await;
}

/// 从宿主子进程的 stdout 中捞出它上报的插件 pid。
#[cfg(windows)]
fn 读取宿主上报的pid(stdout: std::process::ChildStdout) -> u32 {
    use std::io::{BufRead, BufReader};

    // libtest 自己也会往 stdout 写「running 1 test」之类的行，
    // 因此不能取首行，而要找第一条能整体解析成数字的行。
    for line in BufReader::new(stdout).lines().map_while(Result::ok) {
        if let Ok(pid) = line.trim().parse::<u32>() {
            return pid;
        }
    }
    panic!("宿主子进程未上报插件 pid");
}

/// 轮询等待进程消失，返回是否在时限内确实消亡。
#[cfg(windows)]
async fn 轮询直到进程消亡(pid: u32) -> bool {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        if !进程存活(pid) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}

/// 反复读取日志直到出现期望片段或超时，避免依赖固定 sleep 时长。
async fn 轮询读取(path: &std::path::Path, needle: &str) -> String {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut last = String::new();
    while std::time::Instant::now() < deadline {
        last = tokio::fs::read_to_string(path).await.unwrap_or_default();
        if last.contains(needle) {
            return last;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    last
}

/// 查询指定 pid 是否仍然存活。
#[cfg(windows)]
fn 进程存活(pid: u32) -> bool {
    let output = std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH"])
        .output()
        .expect("tasklist 应可执行");
    // 没有匹配时 tasklist 输出的是提示语而非进程行，pid 不会出现在其中。
    String::from_utf8_lossy(&output.stdout).contains(&pid.to_string())
}

#[cfg(not(windows))]
fn 进程存活(pid: u32) -> bool {
    std::path::Path::new(&format!("/proc/{pid}")).exists()
}
