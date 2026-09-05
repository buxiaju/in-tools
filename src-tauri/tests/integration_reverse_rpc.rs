//! 反向 RPC 端到端集成测试。
//!
//! 单测里的 `MockTransport` 只能证明宿主侧逻辑自洽，证明不了「两个真实插件进程
//! 经宿主中转能完成一次反向调用」。这里全程使用 `plugins/caller-plugin` 和
//! `plugins/responder-plugin` 的真实 Python 进程，覆盖设计文档 §5.3 的五项
//! host/* 方法，以及权限拦截与深度限制在跨进程场景下的表现。

#![allow(non_snake_case)]

use std::sync::Arc;

use intools::permission::{
    CallerIdentity, FixedPrompter, PermissionChecker, PermissionStore, PromptDecision,
};
use intools::registry::Registry;
use intools::runtime::supervisor::{ProcessTransportFactory, Supervisor, ToolInvoker};
use serde_json::{json, Value};

/// 插件根目录：`<crate>/../plugins/`。
fn plugins_root() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("src-tauri 应有父目录")
        .join("plugins")
}

/// 搭建带真实子进程的 Supervisor。
///
/// `decision` 控制权限询问的固定答复——AllowAlways 放行全部、DenyAlways 拦截中高危。
async fn supervisor(decision: PromptDecision) -> Arc<Supervisor<FixedPrompter>> {
    let dir = tempfile::tempdir().expect("应能建临时目录");
    let registry = Registry::scan_and_build(&plugins_root()).expect("插件目录应能扫描");

    let store =
        PermissionStore::open_at(dir.path().join("permissions.json")).expect("应能打开权限存储");
    let checker = PermissionChecker::new(store, FixedPrompter(decision));
    let factory = Arc::new(ProcessTransportFactory);

    let sup = Arc::new(
        Supervisor::new(registry, checker, factory)
            .with_plugin_configs_dir(dir.path().join("configs"))
            .with_logs_dir(dir.path().join("logs")),
    );
    sup.install_self_ref();
    sup
}

/// 调用 caller 插件的工具，返回结果或 JSON-RPC 错误码。
async fn call_caller(
    sup: &Supervisor<FixedPrompter>,
    tool: &str,
    args: Value,
) -> Result<Value, i64> {
    match ToolInvoker::call_tool(sup, tool, args, CallerIdentity::Ui).await {
        Ok(v) => Ok(v),
        Err(err) => Err(err.to_rpc_error().code as i64),
    }
}

#[tokio::test]
async fn A经反向rpc成功调用B() {
    let sup = supervisor(PromptDecision::AllowAlways).await;

    // caller:call 让 caller 插件经 host/callTool 调用 hello:echo。
    // 链路：UI → caller → host/callTool → hello → 原样返回。
    let result = call_caller(
        &sup,
        "caller:call",
        json!({"tool": "hello:echo", "arguments": {"text": "跨插件回声"}}),
    )
    .await
    .expect("调用应成功");

    assert_eq!(result["text"], json!("跨插件回声"));
}

#[tokio::test]
async fn A调用需权限工具在拒绝后被拦() {
    let sup = supervisor(PromptDecision::DenyAlways).await;

    // responder 声明了 screen:capture（中危），DenyAlways 下应被权限层拦截。
    // caller 自身无权限声明，UI 调 caller:call 本身能通过；
    // 但 caller 经 host/callTool 调 responder:echo 时，宿主对 responder
    // 做 permission check → DenyAlways → CODE_PERMISSION_DENIED。
    // 错误码包在 result["host_error"] 里，避免被宿主包成 CODE_PLUGIN_ERROR。
    let result = call_caller(
        &sup,
        "caller:call",
        json!({"tool": "responder:echo", "arguments": {"text": "不该到这"}}),
    )
    .await
    .expect("caller 本身无权限，UI 调用应成功");

    assert_eq!(
        result["host_error"]["code"],
        json!(-32002),
        "应为 CODE_PERMISSION_DENIED，实际：{result}"
    );
}

#[tokio::test]
async fn 递归调用在深度上限被拒() {
    let sup = supervisor(PromptDecision::AllowAlways).await;

    // caller:recurse → host/callTool → caller:recurse → host/callTool → ...
    // 每层 depth+1，到 MAX_CALL_DEPTH(5) 时宿主拒绝。
    // 插件侧的递归事件循环会把最深层的错误逐层传回。
    let result = call_caller(&sup, "caller:recurse", json!({}))
        .await
        .expect("caller 本身无权限，UI 调用应成功");

    assert_eq!(
        result["host_error"]["code"],
        json!(-32001),
        "应为 CODE_CALL_DEPTH，实际：{result}"
    );
}

#[tokio::test]
async fn host配置读写往返() {
    let sup = supervisor(PromptDecision::AllowAlways).await;

    // caller:config 先 host/setConfig 再 host/getConfig，返回读回的值。
    let result = call_caller(
        &sup,
        "caller:config",
        json!({"value": {"mode": "test", "count": 42}}),
    )
    .await
    .expect("配置往返应成功");

    assert_eq!(result["mode"], json!("test"));
    assert_eq!(result["count"], json!(42));
}

#[tokio::test]
async fn 未知host方法返回method_not_found() {
    let sup = supervisor(PromptDecision::AllowAlways).await;

    // caller:unknown 调用 host/unknownMethod，宿主应回 CODE_METHOD_NOT_FOUND。
    let result = call_caller(&sup, "caller:unknown", json!({}))
        .await
        .expect("caller 本身无权限，UI 调用应成功");

    assert_eq!(
        result["host_error"]["code"],
        json!(-32601),
        "应为 CODE_METHOD_NOT_FOUND，实际：{result}"
    );
}

#[tokio::test]
async fn host列工具返回工具清单() {
    let sup = supervisor(PromptDecision::AllowAlways).await;

    let result = call_caller(&sup, "caller:list", json!({}))
        .await
        .expect("列工具应成功");

    let tools = result["tools"].as_array().expect("应返回 tools 数组");
    let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
    assert!(
        names.contains(&"hello:echo"),
        "工具清单应含 hello:echo，实际：{names:?}"
    );
    assert!(
        names.contains(&"caller:call"),
        "工具清单应含 caller:call，实际：{names:?}"
    );
}
