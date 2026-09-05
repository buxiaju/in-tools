//! MCP 网关端到端集成测试。
//!
//! `mcp::server` 的单测用假 invoker 证明了路由与错误码自洽，但证明不了
//! 「真实 HTTP 客户端 → axum 端点 → Supervisor → 真实插件子进程」这条完整链路。
//! 这里全程走真实 TCP 与真实 `plugins/hello-plugin` 进程，覆盖实施计划 Phase 8
//! 要求的九条验证：鉴权（无 token / 错 token / 对 token）、白名单（未勾选不暴露 /
//! 勾选后暴露）、真实调用、高危插件拦截、映射反解、映射冲突排除。
//!
//! 端口一律绑 0 由系统分配。写死 7801 会和开发机上正在跑的宿主抢端口，
//! 让测试结果取决于「此刻有没有开着 InTools」。

#![allow(non_snake_case)]

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use intools::config::McpExposure;
use intools::mcp::bridge::McpToolTable;
use intools::mcp::server::{self, McpServerHandle, McpState, ToolTableSource};
use intools::permission::{FixedPrompter, PermissionChecker, PermissionStore, PromptDecision};
use intools::protocol::manifest::load_from_str;
use intools::registry::discovery::LoadedPlugin;
use intools::registry::Registry;
use intools::runtime::supervisor::{ProcessTransportFactory, Supervisor, ToolInvoker};
use serde_json::{json, Value};

/// 测试用 Bearer token。真实 token 由宿主随机生成，这里固定值便于断言。
const TOKEN: &str = "test-token-9f3c1d";

// ─────────────────── 脚手架 ───────────────────

/// 插件根目录：`<crate>/../plugins/`。
fn plugins_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("src-tauri 应有父目录")
        .join("plugins")
}

/// 搭建带真实子进程的 Supervisor。权限一律放行——本文件测的是网关，
/// 不是权限系统；权限拦截另有 `permission` 模块的单测覆盖。
async fn supervisor() -> Arc<Supervisor<FixedPrompter>> {
    let dir = tempfile::tempdir().expect("应能建临时目录");
    let registry = Registry::scan_and_build(&plugins_root()).expect("插件目录应能扫描");

    let store =
        PermissionStore::open_at(dir.path().join("permissions.json")).expect("应能打开权限存储");
    let checker = PermissionChecker::new(store, FixedPrompter(PromptDecision::AllowAlways));

    let sup = Arc::new(
        Supervisor::new(registry, checker, Arc::new(ProcessTransportFactory))
            .with_plugin_configs_dir(dir.path().join("configs"))
            .with_logs_dir(dir.path().join("logs")),
    );
    sup.install_self_ref();
    sup
}

fn exposure(items: &[&str]) -> McpExposure {
    McpExposure {
        exposed: items.iter().map(|s| s.to_string()).collect(),
    }
}

/// 启动网关，返回句柄与 `/mcp` 的完整 URL。
///
/// 句柄必须由调用方持有到测试结束：它 Drop 时会发关停信号，提前丢弃会让
/// 后续请求连接被拒。
async fn serve(invoker: Arc<dyn ToolInvoker>, table: Arc<dyn ToolTableSource>) -> (McpServerHandle, String) {
    let state = Arc::new(McpState::new(TOKEN, invoker, table));
    let handle = server::start(SocketAddr::from(([127, 0, 0, 1], 0)), state)
        .await
        .expect("绑定随机端口应当成功");
    let url = format!("http://{}/mcp", handle.local_addr());
    (handle, url)
}

/// 最常用的组合：真实 Supervisor 当 invoker，白名单固定，映射表每次现算。
async fn serve_real(exposed: &[&str]) -> (Arc<Supervisor<FixedPrompter>>, McpServerHandle, String) {
    let sup = supervisor().await;
    let exp = exposure(exposed);
    let for_table = Arc::clone(&sup);
    let table: Arc<dyn ToolTableSource> = Arc::new(move || McpToolTable::build(&for_table.registry(), &exp));
    let (handle, url) = serve(Arc::clone(&sup) as Arc<dyn ToolInvoker>, table).await;
    (sup, handle, url)
}

/// 发一次 JSON-RPC 请求。`token` 为 `None` 时不带 Authorization 头。
async fn post(url: &str, token: Option<&str>, body: Value) -> reqwest::Response {
    let mut req = reqwest::Client::new().post(url).json(&body);
    if let Some(t) = token {
        req = req.header("Authorization", format!("Bearer {t}"));
    }
    req.send().await.expect("本地 HTTP 请求不应失败")
}

/// 带正确 token 发请求并取回 JSON 响应体。
async fn rpc(url: &str, body: Value) -> Value {
    let resp = post(url, Some(TOKEN), body).await;
    assert_eq!(resp.status(), 200, "业务请求应走 HTTP 200");
    resp.json().await.expect("响应应是合法 JSON")
}

/// 取 `tools/list` 返回的 MCP 工具名集合。
async fn list_tool_names(url: &str) -> Vec<String> {
    let body = rpc(url, json!({"jsonrpc":"2.0","id":1,"method":"tools/list"})).await;
    body["result"]["tools"]
        .as_array()
        .expect("result.tools 应是数组")
        .iter()
        .map(|t| t["name"].as_str().expect("工具名应是字符串").to_string())
        .collect()
}

/// 造一个只有一个工具的合成插件 manifest。
///
/// 磁盘上的两个示范插件都声明 `permissions = []`，测不了高危拦截，也凑不出
/// 映射撞名。合成 manifest 只进注册表、不会被真的拉起，因此 exec 段随便填。
fn synthetic_manifest(id: &str, tool: &str, permissions: &[&str]) -> String {
    let perms = permissions
        .iter()
        .map(|p| format!("\"{p}\""))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        r#"
[plugin]
id = "{id}"
name = "合成插件"
version = "0.1.0"

[exec]
command = "python"
args = ["-u", "main.py"]

[[tools]]
name = "{tool}"
description = "合成工具"
[tools.input_schema]
type = "object"

[capabilities]
permissions = [{perms}]
"#
    )
}

/// 用合成 manifest 拼一个纯内存注册表，`(plugin_id, tool_name, permissions)`。
fn synthetic_registry(specs: &[(&str, &str, &[&str])]) -> Registry {
    let mut reg = Registry::new();
    for (id, tool, perms) in specs {
        let manifest =
            load_from_str(&synthetic_manifest(id, tool, perms)).expect("合成 manifest 应能通过校验");
        reg.insert_loaded(LoadedPlugin {
            plugin_dir: PathBuf::from("."),
            manifest,
        });
    }
    reg
}

// ─────────────────── 鉴权 ───────────────────

#[tokio::test]
async fn 不带token的请求返回401() {
    let (_sup, _handle, url) = serve_real(&[]).await;

    let resp = post(&url, None, json!({"jsonrpc":"2.0","id":1,"method":"tools/list"})).await;

    assert_eq!(resp.status(), 401, "缺少 Authorization 头应被传输层直接拦下");
    let www = resp
        .headers()
        .get("www-authenticate")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(www.contains("Bearer"), "401 应带 WWW-Authenticate: Bearer，实际为 `{www}`");
}

#[tokio::test]
async fn token错误的请求返回401() {
    // 等长但内容不同，这样长度检查不会提前短路，真正走到常量时间比较那一步。
    const WRONG: &str = "test-token-000000";
    assert_eq!(TOKEN.len(), WRONG.len(), "本用例前提：两个 token 等长");

    let (_sup, _handle, url) = serve_real(&["com.intools.hello:hello:echo"]).await;

    let resp = post(
        &url,
        Some(WRONG),
        json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
    )
    .await;

    assert_eq!(resp.status(), 401, "token 不匹配应返回 401");
}

#[tokio::test]
async fn token正确即可列出工具() {
    let (_sup, _handle, url) = serve_real(&["com.intools.hello:hello:echo"]).await;

    let names = list_tool_names(&url).await;

    assert_eq!(names, vec!["hello_echo"], "带对 token 应能拿到白名单内的工具");
}

// ─────────────────── 白名单 ───────────────────

#[tokio::test]
async fn 未勾选的工具不出现在列表里() {
    // 磁盘上有 hello（2 个工具）与 caller（6 个工具），白名单为空。
    let (_sup, _handle, url) = serve_real(&[]).await;

    let names = list_tool_names(&url).await;

    assert!(
        names.is_empty(),
        "默认零暴露：没勾选任何工具时列表必须为空，实际为 {names:?}"
    );
}

#[tokio::test]
async fn 勾选后工具才出现在列表里() {
    let (_sup, _handle, url) = serve_real(&[
        "com.intools.hello:hello:echo",
        "com.intools.caller:caller:list",
    ])
    .await;

    let names = list_tool_names(&url).await;

    // 恰好两个：勾了的都在，没勾的（hello:crash 及另外 5 个 caller 工具）都不在。
    assert_eq!(names, vec!["caller_list", "hello_echo"], "只应暴露勾选过的工具");
}

// ─────────────────── 真实调用 ───────────────────

#[tokio::test]
async fn toolscall能真实拉起插件并取回结果() {
    let (sup, _handle, url) = serve_real(&["com.intools.hello:hello:echo"]).await;

    let text = "经 MCP 网关的回声 🚀";
    let body = rpc(
        &url,
        json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "tools/call",
            "params": { "name": "hello_echo", "arguments": { "text": text } }
        }),
    )
    .await;

    assert_eq!(body["id"], json!(7), "响应应回带请求的 id");
    assert_eq!(
        body["result"]["structuredContent"]["text"],
        json!(text),
        "结构化结果里应是插件原样返回的文本，实际响应：{body}"
    );
    assert_eq!(body["result"]["isError"], json!(false));
    // content 是所有客户端都认的兜底渲染，中文与 emoji 不能在这里被转义坏。
    let rendered = body["result"]["content"][0]["text"]
        .as_str()
        .expect("content[0].text 应是字符串");
    assert!(rendered.contains(text), "content 文本应含原文，实际为 `{rendered}`");

    sup.shutdown_all().await;
}

#[tokio::test]
async fn 调用未暴露的工具返回工具不存在() {
    // hello:crash 存在于注册表，但没进白名单——对 MCP 客户端就该等同于不存在。
    let (_sup, _handle, url) = serve_real(&["com.intools.hello:hello:echo"]).await;

    let body = rpc(
        &url,
        json!({
            "jsonrpc": "2.0",
            "id": 8,
            "method": "tools/call",
            "params": { "name": "hello_crash", "arguments": {} }
        }),
    )
    .await;

    assert_eq!(body["error"]["code"], json!(-32003), "应为 TOOL_NOT_FOUND");
}

// ─────────────────── 高危拦截 ───────────────────

#[tokio::test]
async fn 高危插件即使勾选也不暴露() {
    let registry = synthetic_registry(&[
        ("com.test.risky", "risky:run", &["input:control"]),
        ("com.test.safe", "safe:run", &["file:read"]),
    ]);
    // 两个工具都勾上，只有低危那个该出现。
    let exp = exposure(&["com.test.risky:risky:run", "com.test.safe:safe:run"]);

    let sup = supervisor().await;
    let table: Arc<dyn ToolTableSource> = Arc::new(move || McpToolTable::build(&registry, &exp));
    let (_handle, url) = serve(sup as Arc<dyn ToolInvoker>, Arc::clone(&table)).await;

    let names = list_tool_names(&url).await;

    assert_eq!(
        names,
        vec!["safe_run"],
        "声明 input:control 的插件属高危，勾选也不得暴露"
    );
    assert!(
        table.snapshot().blocked_plugins().contains("com.test.risky"),
        "被拦下的插件应记进 blocked_plugins，供 UI 解释「为什么勾了却看不到」"
    );
}

// ─────────────────── 映射表 ───────────────────

#[tokio::test]
async fn 映射表能把mcp名反解回原始工具名() {
    let sup = supervisor().await;
    let exp = exposure(&["com.intools.hello:hello:echo"]);
    let table = McpToolTable::build(&sup.registry(), &exp);

    let entry = table.resolve("hello_echo").expect("hello_echo 应能反解");

    // to_mcp_name 是有损的（冒号被抹成下划线），反解只能查表。
    assert_eq!(entry.original_name, "hello:echo", "应还原出带冒号的原始名");
    assert_eq!(entry.plugin_id, "com.intools.hello");
    assert!(table.resolve("hello:echo").is_none(), "原始名不是映射表的键");
    assert!(table.resolve("hello_crash").is_none(), "未暴露的工具不该在表里");
}

#[tokio::test]
async fn 映射撞名时后者被排除并记录冲突() {
    // `a.run` 与 `a:run` 是两个不同的原始名（注册表层面不冲突），
    // 但都映射到 `a_run`——这正是 MCP 层特有的、有损映射带来的撞名。
    let registry = synthetic_registry(&[
        ("com.test.first", "a.run", &[]),
        ("com.test.second", "a:run", &[]),
    ]);
    let exp = exposure(&["com.test.first:a.run", "com.test.second:a:run"]);

    let table = McpToolTable::build(&registry, &exp);

    assert_eq!(table.len(), 1, "撞名的两个工具只能留一个");
    let kept = table.resolve("a_run").expect("a_run 应有归属");
    // list_all_tools 按原始名字典序，'.' (0x2E) 在 ':' (0x3A) 之前，故先到的是 a.run。
    assert_eq!(kept.original_name, "a.run", "先到先得");

    let conflicts = table.conflicts();
    assert_eq!(conflicts.len(), 1, "被排除的那个应留下冲突记录");
    assert_eq!(conflicts[0].mcp_name, "a_run");
    assert_eq!(conflicts[0].rejected, "a:run");
    assert_eq!(conflicts[0].occupied_by, "a.run");
    assert_eq!(conflicts[0].rejected_plugin, "com.test.second");
}

// ─────────────────── 协议细节 ───────────────────

#[tokio::test]
async fn initialize返回协议版本且通知不回响应() {
    let (_sup, _handle, url) = serve_real(&[]).await;

    let body = rpc(
        &url,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": { "clientInfo": { "name": "Claude Desktop", "version": "1.0" } }
        }),
    )
    .await;
    assert_eq!(body["result"]["protocolVersion"], json!("2025-06-18"));
    assert_eq!(body["result"]["serverInfo"]["name"], json!("InTools"));

    // notifications/initialized 没有 id，按 JSON-RPC 语义不能回响应体。
    let resp = post(
        &url,
        Some(TOKEN),
        json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
    )
    .await;
    assert_eq!(resp.status(), 202, "通知应回 202 且无响应体");
    assert!(resp.bytes().await.expect("应能读取响应体").is_empty());
}

#[tokio::test]
async fn 不支持的方法返回方法不存在() {
    let (_sup, _handle, url) = serve_real(&[]).await;

    let body = rpc(&url, json!({"jsonrpc":"2.0","id":9,"method":"resources/list"})).await;

    assert_eq!(body["error"]["code"], json!(-32601), "应为 METHOD_NOT_FOUND");
}
