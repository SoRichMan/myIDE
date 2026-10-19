//! 语法高亮性能基准（`cargo test -p ext-cpp-highlight --release --test bench_highlight -- --ignored --nocapture`）。
//!
//! 从 `ide-core/tests/bench_stage1.rs` 迁移而来：高亮现在是语言扩展的职责，
//! 基准也应跟着实现走，这样"扩展机制有没有把性能拖慢"能被独立测量。

use ext_cpp_highlight::CppHighlighter;
use ide_ext::{LanguageHighlighter, TextEdit};

#[test]
#[ignore]
fn bench_open_large_cpp() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../testdata/large.cpp");
    let t0 = std::time::Instant::now();
    let text = std::fs::read_to_string(path).expect("read large.cpp");
    let read_ms = t0.elapsed().as_millis();

    let t1 = std::time::Instant::now();
    let buf = ide_core::Buffer::from_text(&text);
    let buf_ms = t1.elapsed().as_millis();

    // 首次打开：全量解析
    let t2 = std::time::Instant::now();
    let mut h = CppHighlighter::new();
    let hl = h.highlight(&text, None);
    let hl_ms = t2.elapsed().as_millis();

    // 稳态：同一文本再算一次（无编辑描述 → 全量解析）
    let t3 = std::time::Instant::now();
    let hl2 = h.highlight(&text, None);
    let hl2_ms = t3.elapsed().as_millis();

    // 增量：末尾追加一个字符（编辑描述来自 TextEdit::between）
    let mut next = text.clone();
    next.push(' ');
    let edit = TextEdit::between(&text, &next);
    let t4 = std::time::Instant::now();
    let hl3 = h.highlight(&next, edit);
    let hl3_ms = t4.elapsed().as_millis();

    eprintln!("large.cpp: {} bytes, {} lines", text.len(), buf.line_count());
    eprintln!("  read file:     {} ms", read_ms);
    eprintln!("  Buffer build:  {} ms", buf_ms);
    eprintln!("  highlight:     {} ms ({} tokens)", hl_ms, hl.len());
    eprintln!("  highlight#2:   {} ms ({} tokens)", hl2_ms, hl2.len());
    eprintln!(
        "  incremental+1: {} ms ({} tokens)",
        hl3_ms,
        hl3.len()
    );
    assert_eq!(hl.len(), hl2.len(), "两次高亮结果应一致");
    assert!(!hl.is_empty());
    assert!(!hl3.is_empty());
}

#[test]
#[ignore]
fn bench_synthetic_edit_latency() {
    // 模拟"打字"：在 4000 行文件末尾追加字符，测量每次重高亮的耗时。
    let mut src = String::with_capacity(240 * 1024);
    for i in 0..2000 {
        src.push_str(&format!(
            "// line {i}\nint add_{i}(int a, int b) {{ return a + b + 1; }}\n"
        ));
    }
    let mut h = CppHighlighter::new();
    let base = h.highlight(&src, None);
    eprintln!("synthetic {} bytes -> {} tokens", src.len(), base.len());

    let mut worst = 0u128;
    let mut total = 0u128;
    for _ in 0..50 {
        let mut next = src.clone();
        next.push(' ');
        let edit = TextEdit::between(&src, &next);
        let t = std::time::Instant::now();
        let hl = h.highlight(&next, edit);
        let ms = t.elapsed().as_millis();
        worst = worst.max(ms);
        total += ms;
        assert!(!hl.is_empty());
        src = next;
    }
    eprintln!(
        "incremental append: worst {worst} ms, avg {} ms (50 edits)",
        total / 50
    );

    // 对照：不给编辑描述（= 全量解析）
    let t = std::time::Instant::now();
    let _ = CppHighlighter::new().highlight(&src, None);
    eprintln!("full parse (baseline): {} ms", t.elapsed().as_millis());
}
