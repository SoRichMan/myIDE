//! 阶段 1 性能基准（cargo test -- --ignored --nocapture）。
use ide_core::{Buffer, Highlighter};

#[test]
#[ignore]
fn bench_open_large_cpp() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../testdata/large.cpp");
    let t0 = std::time::Instant::now();
    let text = std::fs::read_to_string(path).expect("read large.cpp");
    let read_ms = t0.elapsed().as_millis();

    let t1 = std::time::Instant::now();
    let buf = Buffer::from_text(&text);
    let buf_ms = t1.elapsed().as_millis();

    let t2 = std::time::Instant::now();
    let mut h = Highlighter::new();
    let hl = h.highlight(&text);
    let hl_ms = t2.elapsed().as_millis();

    let t3 = std::time::Instant::now();
    let _snap = buf.text();
    let snap_ms = t3.elapsed().as_millis();

    eprintln!("large.cpp: {} bytes, {} lines", text.len(), buf.line_count());
    eprintln!("  read file:     {} ms", read_ms);
    eprintln!("  Buffer build:  {} ms", buf_ms);
    eprintln!("  highlight:     {} ms ({} tokens)", hl_ms, hl.len());
    eprintln!("  to_string:     {} ms", snap_ms);
    eprintln!("  TOTAL open:    {} ms", read_ms + buf_ms + hl_ms);
}

#[test]
#[ignore]
fn bench_proptest_direct() {
    // 直接跑一次 10 万次随机操作对齐 Vec<char>，看纯执行时间。
    use rand::Rng;
    use rand::rngs::OsRng;
    let mut rng = OsRng;
    let mut b = Buffer::new();
    let mut v: Vec<char> = Vec::new();
    let t0 = std::time::Instant::now();
    for _ in 0..100_000 {
        if rng.gen_bool(0.6) {
            let n = rng.gen_range(1..=8);
            let mut s = String::new();
            for _ in 0..n {
                s.push(rng.gen_range('a'..='z'));
            }
            let idx = rng.gen_range(0..=v.len());
            b.insert(idx, &s);
            v.splice(idx..idx, s.chars());
        } else {
            if v.is_empty() {
                continue;
            }
            let start = rng.gen_range(0..v.len());
            let end = (start + rng.gen_range(0..=4)).min(v.len());
            b.delete(start..end);
            v.drain(start..end);
        }
    }
    let elapsed = t0.elapsed().as_millis();
    assert_eq!(b.text(), v.iter().collect::<String>());
    eprintln!("100k ops aligned in {} ms", elapsed);
}
