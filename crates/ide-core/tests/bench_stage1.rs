//! 阶段 1 性能基准（cargo test -- --ignored --nocapture）。
//!
//! 语法高亮的基准已随实现搬到 `ext-cpp-highlight/tests/bench_highlight.rs`
//! （高亮现在是语言扩展的职责）。
use ide_core::Buffer;

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
