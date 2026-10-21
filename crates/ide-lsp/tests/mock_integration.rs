//! 集成测试：用 Python mock clangd（`testdata/mock_clangd.py`）驱动完整客户端。
//!
//! 覆盖 myIDE.md 阶段 2 要求：
//! - LSP 消息序列化正确（初始化/打开/编辑/保存/关闭）
//! - 定义/声明/类型定义路由正确
//! - hover 回显请求参数与版本号
//! - documentSymbol 嵌套解析
//! - 服务端错误、超时、崩溃三态优雅降级
//! - 崩溃后可通过 LspManager 重启并恢复

use ide_lsp::client::{LspClient, LspEvents, ServerEvent, DEFAULT_TIMEOUT};
use ide_lsp::manager::{Event, LspManager, LspStatus};
use ide_lsp::message::Position;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, Instant};
use tokio::process::Command as TokioCommand;

/// mock 脚本路径（相对 crate 根）。
fn mock_script() -> PathBuf {
    let root = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    PathBuf::from(root).join("../../testdata/mock_clangd.py")
}

/// python 解释器：先找 PATH 上的 `python`，找不到就用本机装的
/// `D:\Python311\python.exe`（这台机器的 Python 不在 PATH 上）。
pub fn python_cmd() -> String {
    let local = r"D:\Python311\python.exe";
    if std::path::Path::new(local).exists() {
        return local.to_string();
    }
    "python".to_string()
}

const URI: &str = "file:///mnt/e/TestDeepSeek/testdata/sample_project/src/main.cpp";

async fn spawn_mock() -> tokio::process::Child {
    let mut cmd = TokioCommand::new(python_cmd());
    cmd.arg(mock_script());
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd.spawn().expect("python 必须可用（mock clangd 集成测试前置）")
}

async fn connect_mock() -> (LspClient, LspEvents) {
    connect_mock_with_timeout(DEFAULT_TIMEOUT).await
}

async fn connect_mock_with_timeout(timeout: Duration) -> (LspClient, LspEvents) {
    let child = spawn_mock().await;
    let (client, events, _writer) = LspClient::connect(child, timeout).unwrap();
    (client, events)
}

/// 等 mock 进程退出（读取 monitor 推送的 Exit 事件），断言退出码为 0。
async fn expect_mock_exit_ok(events: &mut LspEvents) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match events.recv().await {
            Some(ServerEvent::Exit { code }) => {
                assert_eq!(code, Some(0), "mock 应正常退出，实际 code {code:?}");
                return;
            }
            other => {
                assert!(Instant::now() < deadline, "等待 Exit 事件超时，最后收到 {other:?}");
            }
        }
    }
}

#[tokio::test]
async fn mock_full_lifecycle() {
    let (mut client, mut events) = connect_mock().await;

    // initialize
    let init = client.initialize("file:///mnt/e/TestDeepSeek/testdata/sample_project").await.unwrap();
    assert_eq!(init["serverInfo"]["name"], "mock-clangd");
    assert_eq!(init["capabilities"]["definitionProvider"], true);
    client.initialized().await.unwrap();

    // didOpen → mock 回 logMessage 通知（跳过 stderr 等其它事件）
    client.did_open(URI, "cpp", "int main() { return 0; }", 1).await.unwrap();
    let mut saw_log = false;
    for _ in 0..10 {
        let ev = tokio::time::timeout(Duration::from_secs(3), events.recv())
            .await
            .expect("应收到 logMessage")
            .expect("事件通道未关闭");
        if let ServerEvent::LogMessage { message, .. } = ev {
            assert!(message.contains("mock opened"));
            saw_log = true;
            break;
        }
    }
    assert!(saw_log, "未收到 didOpen 的 logMessage");

    // didChange → 版本号推进
    client.did_change(URI, "int main() { int x = 1; return x; }", 2).await.unwrap();
    client.did_save(URI).await.unwrap();

    // 定义跳转 → mock 固定返回 math_utils.cpp line 3
    let defs = client
        .definition(URI, Position { line: 0, character: 10 })
        .await
        .unwrap();
    assert_eq!(defs.len(), 1);
    assert!(defs[0].uri.ends_with("math_utils.cpp"));
    assert_eq!(defs[0].range.start.line, 3);
    assert_eq!(defs[0].range.start.character, 4);

    // 声明 → math_utils.h line 6
    let decls = client
        .declaration(URI, Position { line: 0, character: 10 })
        .await
        .unwrap();
    assert!(decls[0].uri.ends_with("math_utils.h"));
    assert_eq!(decls[0].range.start.line, 6);

    // 类型定义（非 0 列）→ math_utils.h line 13
    let tdefs = client
        .type_definition(URI, Position { line: 0, character: 10 })
        .await
        .unwrap();
    assert!(tdefs[0].uri.ends_with("math_utils.h"));
    assert_eq!(tdefs[0].range.start.line, 13);

    // hover → mock 回显 didSave 后的版本 3 与请求位置（didChange v2 → didSave +1）
    let hover = client
        .hover(URI, Position { line: 3, character: 7 })
        .await
        .unwrap()
        .expect("hover 应有内容");
    assert!(hover.markdown.contains("int add(int, int)"));
    assert!(hover.markdown.contains("version=3"), "应回显 didSave 后的版本：{}", hover.markdown);
    assert!(hover.markdown.contains("line=3 char=7"));

    // documentSymbol → 嵌套 main{sum}
    let syms = client.document_symbol(URI).await.unwrap();
    assert_eq!(syms.len(), 1);
    assert_eq!(syms[0].name, "main");
    assert_eq!(syms[0].children.len(), 1);
    assert_eq!(syms[0].children[0].name, "sum");

    // didClose + 优雅关闭
    client.did_close(URI).await.unwrap();
    client.shutdown().await;
    expect_mock_exit_ok(&mut events).await;
}

#[tokio::test]
async fn mock_implementation_times_out() {
    // 用 500ms 默认超时验证超时路径
    let (mut client, mut events) = connect_mock_with_timeout(Duration::from_millis(500)).await;
    client.initialize("file:///mnt/e/TestDeepSeek/testdata/sample_project").await.unwrap();
    client.initialized().await.unwrap();

    // implementation：mock 故意不响应 → 客户端应超时
    let t0 = Instant::now();
    let err = client
        .implementation(URI, Position { line: 1, character: 2 })
        .await
        .unwrap_err();
    let elapsed = t0.elapsed();
    assert!(
        matches!(err, ide_lsp::LspError::Timeout(_)),
        "应超时，实际 {err:?}"
    );
    assert!(
        elapsed >= Duration::from_millis(450),
        "超时等待过短：{elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_secs(3),
        "超时等待过长：{elapsed:?}"
    );

    // 超时后连接仍可用（下一个请求正常返回）
    let defs = client
        .definition(URI, Position { line: 1, character: 2 })
        .await
        .unwrap();
    assert_eq!(defs.len(), 1);

    client.shutdown().await;
    expect_mock_exit_ok(&mut events).await;
}

#[tokio::test]
async fn mock_server_error_degrades_gracefully() {
    let (mut client, mut events) = connect_mock().await;
    client.initialize("file:///mnt/e/TestDeepSeek/testdata/sample_project").await.unwrap();
    client.initialized().await.unwrap();

    // character == 0 → mock 返回 -32601 错误
    let err = client
        .type_definition(URI, Position { line: 0, character: 0 })
        .await
        .unwrap_err();
    match err {
        ide_lsp::LspError::Server { code, message } => {
            assert_eq!(code, -32601);
            assert!(message.contains("not found"));
        }
        other => panic!("预期 Server 错误，实际 {other:?}"),
    }

    // 错误后连接仍可用
    let defs = client
        .definition(URI, Position { line: 0, character: 5 })
        .await
        .unwrap();
    assert_eq!(defs.len(), 1);

    client.shutdown().await;
    expect_mock_exit_ok(&mut events).await;
}

// ---------------------------------------------------------------------------
// LspManager 级测试：崩溃 → 状态机 → 重启恢复
// ---------------------------------------------------------------------------

fn wait_status<F>(mgr: &LspManager, mut want: F, timeout: Duration) -> LspStatus
where
    F: FnMut(&LspStatus) -> bool,
{
    let deadline = Instant::now() + timeout;
    loop {
        let s = mgr.status();
        if want(&s) {
            return s;
        }
        // 也消费事件，防止通道堆积
        while mgr.poll().is_some() {}
        assert!(Instant::now() < deadline, "等待状态超时，当前 {s:?}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn wait_ready(mgr: &LspManager, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        if mgr.status().is_ready() {
            return;
        }
        while mgr.poll().is_some() {}
        assert!(Instant::now() < deadline, "等待 Ready 超时，当前 {:?}", mgr.status());
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn manager_mock_crash_then_restart() {
    // 用 mock 替换真实 clangd（进程级环境变量，本测试独占该变量）
    let mock = mock_script();
    let cmdline = format!("{} {}", python_cmd(), mock.display());
    unsafe { std::env::set_var("LITEIDE_LSP_CMD", &cmdline) };

    let mut mgr = LspManager::new();
    mgr.start("file:///mnt/e/TestDeepSeek/testdata/sample_project".to_string());
    wait_ready(&mgr, Duration::from_secs(15));

    // 打开文档
    mgr.open_document(URI.to_string(), "int main() { return 0; }".into(), 1);

    // 触发 mock 崩溃：didClose("exit://mock")
    mgr.close_document("exit://mock".to_string());
    let crashed = wait_status(
        &mgr,
        |s| matches!(s, LspStatus::Crashed(_)),
        Duration::from_secs(10),
    );
    assert!(
        matches!(crashed, LspStatus::Crashed(ref d) if d.contains("exit code 1")),
        "应显示崩溃原因，实际 {crashed:?}"
    );

    // 重启 → 恢复 Ready
    mgr.restart();
    wait_ready(&mgr, Duration::from_secs(15));

    // 重启后请求可正常完成（定义跳转返回 mock 固定位置）
    let seq = mgr.request_definition(URI.to_string(), Position { line: 0, character: 10 });
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(Event::Definition { seq: got, result }) = mgr.poll() {
            assert_eq!(got, seq);
            let locs = result.expect("定义请求应成功");
            assert_eq!(locs.len(), 1);
            assert!(locs[0].uri.ends_with("math_utils.cpp"));
            break;
        }
        assert!(Instant::now() < deadline, "等待定义结果超时");
        std::thread::sleep(Duration::from_millis(30));
    }

    // 清理：关闭会话，恢复环境变量
    mgr.shutdown();
    unsafe { std::env::remove_var("LITEIDE_LSP_CMD") };
}

