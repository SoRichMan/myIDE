//! 真实 clangd 端到端测试（`#[ignore]`，手动运行）。
//!
//! 前置：WSL Ubuntu + clangd（`scripts/setup-wsl.sh`），sample_project 已生成。
//! 运行：`cargo test -p ide-lsp --test clangd_real -- --ignored --nocapture`
//!
//! 覆盖 myIDE.md 验收：打开 CMake 项目点函数跳定义、悬停显示签名、
//! clangd 崩溃不崩 IDE、可重启；并实测跳转/hover 往返延迟。

use ide_lsp::manager::{Event, LspManager, LspStatus};
use ide_lsp::message::Position;
use ide_lsp::path_map::{win_to_uri, uri_to_win};
use std::time::{Duration, Instant};

const PROJECT_WIN: &str = r"E:\TestDeepSeek\testdata\sample_project";
const MAIN_CPP: &str = r"E:\TestDeepSeek\testdata\sample_project\src\main.cpp";

fn sample_uri() -> String {
    win_to_uri(PROJECT_WIN).expect("项目路径转换失败")
}

fn main_uri() -> String {
    win_to_uri(MAIN_CPP).expect("main.cpp 路径转换失败")
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

fn wait_crashed(mgr: &LspManager, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        if matches!(mgr.status(), LspStatus::Crashed(_)) {
            return;
        }
        while mgr.poll().is_some() {}
        assert!(Instant::now() < deadline, "等待 Crashed 超时，当前 {:?}", mgr.status());
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// 请求定义并返回 (结果, 往返耗时)。
fn request_definition(mgr: &mut LspManager, uri: &str, line: u32, character: u32) -> (Vec<ide_lsp::Location>, Duration) {
    let seq = mgr.request_definition(uri.to_string(), Position { line, character });
    let t0 = Instant::now();
    let deadline = t0 + Duration::from_secs(15);
    loop {
        if let Some(Event::Definition { seq: got, result }) = mgr.poll() {
            assert_eq!(got, seq);
            return (result.expect("定义请求失败"), t0.elapsed());
        }
        assert!(Instant::now() < deadline, "定义请求超时 15s");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn request_hover(mgr: &mut LspManager, uri: &str, line: u32, character: u32) -> (Option<ide_lsp::HoverInfo>, Duration) {
    let seq = mgr.request_hover(uri.to_string(), Position { line, character });
    let t0 = Instant::now();
    let deadline = t0 + Duration::from_secs(15);
    loop {
        if let Some(Event::Hover { seq: got, result }) = mgr.poll() {
            assert_eq!(got, seq);
            return (result.expect("hover 请求失败"), t0.elapsed());
        }
        assert!(Instant::now() < deadline, "hover 请求超时 15s");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// 读取 Windows 路径文件内容（供 didOpen 使用）。
fn read_file(path: &str) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("读取 {path} 失败: {e}"))
}

#[test]
#[ignore = "需要真实 WSL clangd"]
fn real_clangd_definition_and_hover() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("info")
        .try_init();

    let mut mgr = LspManager::new();
    mgr.start(sample_uri());
    wait_ready(&mgr, Duration::from_secs(30));

    // 打开 main.cpp
    let text = read_file(MAIN_CPP);
    mgr.open_document(main_uri(), text, 1);

    // 等 clangd 完成首轮解析（给后台索引一点时间，定义请求自带等待）
    std::thread::sleep(Duration::from_millis(500));

    // main.cpp 中 `int sum = add(3, 4);` 一行，光标放在 add 上（第 7 行，0-based 6）
    // 行号由文件内容决定：见下方 find_line_of 动态查找，避免硬编码漂移。
    let content = read_file(MAIN_CPP);
    let add_line = find_line_containing(&content, "add(3, 4)").expect("main.cpp 应有 add(3, 4) 调用");
    let add_col = content.lines().nth(add_line).unwrap().find("add").unwrap() as u32;

    // 定义：add 的实现在 math_utils.cpp
    let (locs, dt_def) = request_definition(&mut mgr, &main_uri(), add_line as u32, add_col);
    println!("definition roundtrip: {dt_def:?} -> {} 个位置", locs.len());
    assert!(!locs.is_empty(), "add 的定义未找到");
    let win = uri_to_win(&locs[0].uri).expect("返回 URI 转 Windows 失败");
    println!("  目标: {win}:{}", locs[0].range.start.line + 1);
    assert!(win.ends_with("math_utils.cpp"), "add 定义应在 math_utils.cpp，实际 {win}");
    let utils = read_file(r"E:\TestDeepSeek\testdata\sample_project\src\math_utils.cpp");
    let def_line = locs[0].range.start.line as usize;
    let def_text = utils.lines().nth(def_line).unwrap_or("");
    assert!(def_text.contains("add"), "目标行应含 add 定义：{def_text}");

    // hover：同一位置
    let (hover, dt_hover) = request_hover(&mut mgr, &main_uri(), add_line as u32, add_col);
    println!("hover roundtrip: {dt_hover:?}");
    let info = hover.expect("hover 应有内容");
    println!("  hover 内容: {}", info.markdown.replace('\n', " "));
    assert!(info.markdown.contains("add"), "hover 应含 add 签名");
    assert!(info.markdown.contains("int"), "hover 应含返回类型 int");

    // 性能断言（本地项目，宽松给首轮索引余量）
    assert!(
        dt_def < Duration::from_millis(1500),
        "定义往返 {dt_def:?} 超过 1500ms"
    );
    assert!(
        dt_hover < Duration::from_millis(1500),
        "hover 往返 {dt_hover:?} 超过 1500ms"
    );

    // 二次请求（预热后）应明显更快
    let (_, dt_def2) = request_definition(&mut mgr, &main_uri(), add_line as u32, add_col);
    let (_, dt_hover2) = request_hover(&mut mgr, &main_uri(), add_line as u32, add_col);
    println!("warm definition roundtrip: {dt_def2:?}");
    println!("warm hover roundtrip: {dt_hover2:?}");

    mgr.shutdown();
}

#[test]
#[ignore = "需要真实 WSL clangd"]
fn real_clangd_crash_recovery() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("info")
        .try_init();

    let mut mgr = LspManager::new();
    mgr.start(sample_uri());
    wait_ready(&mgr, Duration::from_secs(30));

    // 打开文档并触发一次解析
    mgr.open_document(main_uri(), read_file(MAIN_CPP), 1);
    std::thread::sleep(Duration::from_millis(800));

    // 杀 clangd（WSL 内 pkill -f：clangd 主线程名为 clangd.main，-x 精确匹配不到）
    let out = std::process::Command::new("wsl.exe")
        .args(["-d", "Ubuntu2404", "--", "pkill", "-f", "clangd"])
        .output()
        .expect("wsl.exe 应可用");
    println!("pkill clangd: {:?}", out.status.code());

    // IDE 不崩，状态变为 Crashed
    wait_crashed(&mgr, Duration::from_secs(15));
    let st = mgr.status();
    println!("crash detected: {st:?}");
    assert!(matches!(st, LspStatus::Crashed(_)));

    // 重启 → 恢复 Ready
    mgr.restart();
    wait_ready(&mgr, Duration::from_secs(30));
    println!("restarted to Ready");

    // 重启后可正常跳转（重新打开文档）
    mgr.open_document(main_uri(), read_file(MAIN_CPP), 1);
    let content = read_file(MAIN_CPP);
    let add_line = find_line_containing(&content, "add(3, 4)").expect("main.cpp 应有 add(3, 4)");
    let add_col = content.lines().nth(add_line).unwrap().find("add").unwrap() as u32;
    let (locs, dt) = request_definition(&mut mgr, &main_uri(), add_line as u32, add_col);
    assert!(!locs.is_empty());
    let win = uri_to_win(&locs[0].uri).expect("URI 转 Windows 失败");
    println!("restart 后定义跳转: {win} ({dt:?})");
    assert!(win.ends_with("math_utils.cpp"));

    mgr.shutdown();
}

/// 查找包含指定子串的行号（0-based）。
fn find_line_containing(text: &str, needle: &str) -> Option<usize> {
    text.lines().position(|l| l.contains(needle))
}
