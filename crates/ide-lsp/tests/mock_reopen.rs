//! 集成测试：LSP 会话换根（远程工作区切换）。
//!
//! 单独一个测试二进制，因为 `LITEIDE_LSP_CMD` 是进程级环境变量，同一二进制内的
//! 测试会并行跑、互相踩变量（`mock_integration.rs` 已独占它）。
//!
//! 背景：clangd 的根只在 `initialize` 时确定。切到远程工作区（WSL 目录）时必须
//! 重启会话并换到新根；早期实现里 `Reopen` 让会话回到「等新命令」的循环，
//! 调用方只发了一条 `Reopen`，于是永远停在 `LspStatus::Off`。

use ide_lsp::manager::{Event, LspManager, LspStatus};
use ide_lsp::message::Position;
use std::path::PathBuf;
use std::time::{Duration, Instant};

fn mock_script() -> PathBuf {
    let root = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    PathBuf::from(root).join("../../testdata/mock_clangd.py")
}

const URI: &str = "file:///mnt/e/TestDeepSeek/testdata/sample_project/src/main.cpp";

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
        while mgr.poll().is_some() {}
        assert!(Instant::now() < deadline, "等待状态超时，当前 {s:?}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn manager_mock_reopen_root_reconnects() {
    let mock = mock_script();
    let cmdline = format!("python {}", mock.display());
    unsafe { std::env::set_var("LITEIDE_LSP_CMD", &cmdline) };

    let mut mgr = LspManager::new();
    let local = "file:///mnt/e/TestDeepSeek/testdata/sample_project";
    mgr.start(local.to_string());
    wait_status(
        &mgr,
        |s| matches!(s, LspStatus::Ready),
        Duration::from_secs(15),
    );
    assert_eq!(mgr.root_uri().as_deref(), Some(local));

    // 换根到远程工作区（WSL 目录的 clangd 视角 URI）。
    let remote = "file://wsl.localhost/Ubuntu2404/home/work/NewLithoTune/candanza";
    mgr.reopen_root(remote.to_string());
    // 换根会先 shutdown 再重连：等到「Ready 且根已换」，不能只看 Ready
    // （刚发命令时还是旧会话的 Ready）。
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if matches!(mgr.status(), LspStatus::Ready) && mgr.root_uri().as_deref() == Some(remote) {
            break;
        }
        while mgr.poll().is_some() {}
        assert!(
            Instant::now() < deadline,
            "换根未完成：status={:?} root={:?}",
            mgr.status(),
            mgr.root_uri()
        );
        std::thread::sleep(Duration::from_millis(50));
    }

    // 新根下请求仍然可用（证明是真的新会话，不是旧会话残留）。
    mgr.open_document(URI.to_string(), "int main() { return 0; }".into(), 1);
    let seq = mgr.request_definition(URI.to_string(), Position { line: 0, character: 10 });
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(Event::Definition { seq: got, result }) = mgr.poll() {
            assert_eq!(got, seq);
            assert_eq!(
                result.expect("定义请求应成功").len(),
                1,
                "mock 固定返回一个定义位置"
            );
            break;
        }
        assert!(Instant::now() < deadline, "换根后定义请求超时");
        std::thread::sleep(Duration::from_millis(30));
    }

    mgr.shutdown();
    unsafe { std::env::remove_var("LITEIDE_LSP_CMD") };
}
