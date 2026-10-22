//! 真实 SSH clangd 端到端测试（`#[ignore]`，手动运行）。
//!
//! 验证用户的真实场景：SSH 连上 CentOS 7 工程后，点符号要能跳定义。
//! 前置：远端 clangd 可用（系统安装或 IDE「远程工具链 → 自动安装」），
//! 且工程里有 `build/compile_commands.json`。
//!
//! `cargo test -p ide-lsp --test clangd_ssh_real -- --ignored --nocapture`

use ide_core::remote::{shell_quote, RemoteTarget};
use ide_lsp::manager::{Event, LspManager};
use ide_lsp::message::Position;
use ide_lsp::path_map::posix_to_uri;
use std::time::{Duration, Instant};

const DEFAULT_SYMBOL: &str = "GetInstance";

fn symbol() -> String {
    std::env::var("LITEIDE_SSH_TEST_SYMBOL").unwrap_or_else(|_| DEFAULT_SYMBOL.to_string())
}

fn target() -> RemoteTarget {
    let user = std::env::var("LITEIDE_SSH_TEST_USER").unwrap_or_else(|_| "fubin".to_string());
    let host =
        std::env::var("LITEIDE_SSH_TEST_HOST").unwrap_or_else(|_| "192.168.77.6".to_string());
    RemoteTarget::ssh(user, host, 22)
}

fn root() -> String {
    std::env::var("LITEIDE_SSH_TEST_ROOT").unwrap_or_else(|_| "/home/fubin/work/candanza".to_string())
}

/// 在远端跑一段脚本（stdout 即结果）。
fn run_remote(target: &RemoteTarget, script: &str) -> String {
    let (program, args) = target.shell_argv(script);
    let out = std::process::Command::new(&program)
        .args(&args)
        .output()
        .unwrap_or_else(|e| panic!("调用 {program} 失败: {e}"));
    assert!(
        out.status.success(),
        "远端脚本失败: {}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// `compile_commands.json` 里的源文件列表（拿不到就返回空）。
fn compile_commands_files(target: &RemoteTarget, root: &str) -> Vec<String> {
    let script = format!(
        "python -c \"import json,sys; [sys.stdout.write(e['file']+chr(10)) for e in json.load(open('{}/build/compile_commands.json'))]\" 2>/dev/null",
        root.trim_end_matches('/')
    );
    let (program, args) = target.shell_argv(&script);
    match std::process::Command::new(&program).args(&args).output() {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect(),
        _ => Vec::new(),
    }
}

fn wait_ready(mgr: &LspManager, timeout: Duration) {    let deadline = Instant::now() + timeout;
    loop {
        if mgr.status().is_ready() {
            return;
        }
        while mgr.poll().is_some() {}
        assert!(
            Instant::now() < deadline,
            "等待 clangd Ready 超时，当前 {:?}",
            mgr.status()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
#[ignore = "需要 SSH 主机 + 远端 clangd + compile_commands.json"]
fn ssh_clangd_jumps_to_definition() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(std::env::var("RUST_LOG").unwrap_or_else(|_| "info".to_string()))
        .try_init();

    let target = target();
    let root = root();
    let symbol = symbol();

    // 1) 找到符号所在的工程源码文件（优先 compile_commands.json 里的文件，
    //    没有编译命令的文件 clangd 只能用猜测的参数，跳转不靠谱）。
    let file = match std::env::var("LITEIDE_SSH_TEST_FILE") {
        Ok(f) if !f.trim().is_empty() => {
            let f = f.trim().to_string();
            if f.starts_with('/') {
                f
            } else {
                format!("{}/{}", root.trim_end_matches('/'), f)
            }
        }
        _ => {
            let script = format!(
                "export PATH=\"$HOME/.cache/opencode/bin:$PATH\"; \
                 rg -l -uu {} {} --glob '!external/**' --glob '!build/**' \
                 --glob '*.cpp' --glob '*.cc' --glob '*.h' --glob '*.hpp' 2>/dev/null | head -n 20",
                shell_quote(&symbol),
                shell_quote(&root)
            );
            let listing = run_remote(&target, &script);
            let files: Vec<String> = listing
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(str::to_string)
                .collect();
            assert!(!files.is_empty(), "工程里找不到 {symbol}：\n{listing}");
            let cdb = compile_commands_files(&target, &root);
            files
                .iter()
                .find(|f| cdb.iter().any(|c| c == *f))
                .unwrap_or_else(|| &files[0])
                .clone()
        }
    };
    println!("symbol {symbol} in file: {file}");

    let text = run_remote(&target, &format!("cat {}", shell_quote(&file)));
    let (line, col) = text
        .lines()
        .enumerate()
        .find_map(|(i, l)| l.find(&symbol).map(|c| (i, c)))
        .unwrap_or_else(|| panic!("{file} 里找不到 {symbol}"));
    println!("position: {}:{}", line + 1, col + 1);

    // 2) 以远端 POSIX 根启动 clangd（走 SSH 传输）。
    let uri = posix_to_uri(&file).expect("POSIX 路径转 URI 失败");
    let root_uri = posix_to_uri(&root).expect("工程根转 URI 失败");
    let mut mgr = LspManager::new();
    mgr.set_remote_target(Some(target.clone()));
    mgr.start(root_uri);
    wait_ready(&mgr, Duration::from_secs(120));
    println!("clangd ready: {:?}", mgr.status());

    mgr.open_document(uri.clone(), text, 1);
    std::thread::sleep(Duration::from_secs(2));

    // 诊断：能出文档符号说明 clangd 真的建出了 AST（而不是卡在缺头文件）。
    let sym_seq = mgr.request_symbols(uri.clone());
    let sym_deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(Event::Symbols { seq: got, result }) = mgr.poll() {
            assert_eq!(got, sym_seq);
            match result {
                Ok(syms) => println!("document symbols: {}", syms.len()),
                Err(e) => println!("document symbols 失败: {e}"),
            }
            break;
        }
        if Instant::now() >= sym_deadline {
            println!("document symbols 超时");
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }

    let hover_seq = mgr.request_hover(
        uri.clone(),
        Position {
            line: line as u32,
            character: col as u32,
        },
    );
    let hover_deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(Event::Hover { seq: got, result }) = mgr.poll() {
            assert_eq!(got, hover_seq);
            match result {
                Ok(Some(h)) => println!("hover: {}", h.markdown.replace('\n', " ")),
                Ok(None) => println!("hover: 空"),
                Err(e) => println!("hover 失败: {e}"),
            }
            break;
        }
        if Instant::now() >= hover_deadline {
            println!("hover 超时");
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }

    // 3) 请求定义，必须拿到位置（这正是用户 Ctrl+左键做的事）。
    let seq = mgr.request_definition(
        uri.clone(),
        Position {
            line: line as u32,
            character: col as u32,
        },
    );
    let deadline = Instant::now() + Duration::from_secs(120);
    let locs = loop {
        if let Some(Event::Definition { seq: got, result }) = mgr.poll() {
            assert_eq!(got, seq);
            break result.unwrap_or_else(|e| panic!("定义请求失败: {e}"));
        }
        assert!(Instant::now() < deadline, "定义请求超时 120s");
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(!locs.is_empty(), "{symbol} 的定义未找到");
    for loc in &locs {
        println!(
            "  -> {}:{}",
            loc.uri,
            loc.range.start.line + 1
        );
        assert!(
            loc.uri.starts_with("file:///"),
            "定义位置应是远端文件 URI: {}",
            loc.uri
        );
    }
    mgr.shutdown();
}
