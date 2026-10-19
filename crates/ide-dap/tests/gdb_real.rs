//! 真实 gdb 端到端集成测试（`#[ignore]`，需 WSL + gdb + 已编译的 debug_target）。
//!
//! 运行前：
//! 1. `powershell -File scripts/build_debug_target.ps1`（编译 testdata/debug_target）
//! 2. `cargo test -p ide-dap --test gdb_real -- --ignored --nocapture`
//!
//! 覆盖 myIDE.md 阶段 3 验收：
//! - 启动 → 条件断点（第 7 行，i==5）命中，变量 i==5
//! - 继续 → 第 10 行断点命中（i==10）
//! - 单步跳过（F10）
//! - `ptype /o Foo` 文本（total size 32）
//! - 结构体展开 + 100 字段结构体（Big.f[100]）展开
//! - 性能实测：启动到首断点 < 2s、单步往返 < 100ms、100 字段展开 < 300ms
//! - Shift+F5 停止，状态回 Off

use ide_dap::manager::{BreakpointSpec, DapManager, DapStatus, Event, VarNode};
use std::path::PathBuf;
use std::time::{Duration, Instant};

const BIN: &str = r"E:\TestDeepSeek\testdata\debug_target";
const SRC: &str = r"E:\TestDeepSeek\testdata\debug_target.cpp";

fn bin_missing() -> bool {
    !PathBuf::from(BIN).exists()
}

fn wait_stopped_at(log: &mut EventLog, line: u32) -> Event {
    log.wait_for(|e| matches!(e, Event::Stopped { line: l, .. } if *l == line), Duration::from_secs(20))
}

fn find_var<'a>(locals: &'a [VarNode], exp: &str) -> &'a VarNode {
    locals
        .iter()
        .find(|v| v.exp == exp)
        .unwrap_or_else(|| panic!("变量 {exp} 未出现，实际 {locals:?}"))
}

/// 事件日志：只消费命中的事件，其余保留给后续等待。
struct EventLog {
    mgr: DapManager,
    events: Vec<Event>,
    consumed: Vec<bool>,
}

impl EventLog {
    fn new(mgr: DapManager) -> Self {
        Self { mgr, events: Vec::new(), consumed: Vec::new() }
    }

    fn poll_into(&mut self) {
        while let Some(ev) = self.mgr.poll() {
            self.events.push(ev);
            self.consumed.push(false);
        }
    }

    fn wait_for<F>(&mut self, mut pred: F, timeout: Duration) -> Event
    where
        F: FnMut(&Event) -> bool,
    {
        let deadline = Instant::now() + timeout;
        loop {
            self.poll_into();
            for i in 0..self.events.len() {
                if !self.consumed[i] && pred(&self.events[i]) {
                    self.consumed[i] = true;
                    return self.events[i].clone();
                }
            }
            assert!(Instant::now() < deadline, "等待事件超时（共 {} 条）", self.events.len());
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn wait_status<F>(&mut self, mut want: F, timeout: Duration) -> DapStatus
    where
        F: FnMut(&DapStatus) -> bool,
    {
        let deadline = Instant::now() + timeout;
        loop {
            self.poll_into();
            let s = self.mgr.status();
            if want(&s) {
                return s;
            }
            assert!(Instant::now() < deadline, "等待状态超时，当前 {s:?}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

#[test]
#[ignore = "需要 WSL + 已编译的 debug_target（scripts/build_debug_target.ps1）"]
fn real_gdb_end_to_end() {
    if bin_missing() {
        panic!("未找到 {BIN}，请先运行 scripts/build_debug_target.ps1 编译调试目标");
    }

    let mut log = EventLog::new(DapManager::new());
    log.mgr.start(
        BIN.to_string(),
        vec![
            BreakpointSpec { file_win: SRC.to_string(), line: 7, condition: Some("i == 5".to_string()) },
            BreakpointSpec { file_win: SRC.to_string(), line: 10, condition: None },
        ],
    );

    // 1) 首断点：第 7 行条件断点（i==5）——含启动时间（WSL + gdb 冷启动）
    let t0 = Instant::now();
    let _stopped = wait_stopped_at(&mut log, 7);
    let start_to_first_stop = t0.elapsed();
    println!("[perf] 启动到首断点: {start_to_first_stop:?}");
    assert!(start_to_first_stop < Duration::from_secs(2), "启动到首断点超过 2s");

    let mut vars = log.wait_for(
        |e| matches!(e, Event::Variables { locals, .. } if !locals.is_empty()),
        Duration::from_secs(10),
    );
    if let Event::Variables { locals, .. } = &vars {
        assert_eq!(find_var(locals, "i").value, "5", "条件断点命中时 i 应为 5");
    }

    // 2) F5 继续 → 第 10 行普通断点（i==10）
    log.mgr.continue_run();
    wait_stopped_at(&mut log, 10);
    vars = log.wait_for(
        |e| matches!(e, Event::Variables { locals, .. } if !locals.is_empty()),
        Duration::from_secs(10),
    );
    if let Event::Variables { locals, .. } = &vars {
        assert_eq!(find_var(locals, "i").value, "10", "第 10 行断点命中时 i 应为 10");
    }

    // 3) F10 单步跳过（时间实测：单步往返 < 100ms）
    let t0 = Instant::now();
    log.mgr.step_over();
    wait_stopped_at(&mut log, 11);
    let step_roundtrip = t0.elapsed();
    println!("[perf] 单步往返: {step_roundtrip:?}");
    assert!(step_roundtrip < Duration::from_millis(100), "单步往返超过 100ms");

    // 4) ptype /o Foo → 控制台文本
    log.mgr.ptype("Foo".to_string());
    let ev = log.wait_for(|e| matches!(e, Event::Ptype { .. }), Duration::from_secs(10));
    if let Event::Ptype { text } = &ev {
        assert!(text.contains("total size (bytes):   32"), "ptype 文本: {text:?}");
    }

    // 5) 结构体展开（Foo，3 字段）
    let seq = log.mgr.expand_var("foo".to_string());
    let ev = log.wait_for(
        |e| matches!(e, Event::VarChildren { seq: s, .. } if *s == seq),
        Duration::from_secs(10),
    );
    if let Event::VarChildren { children, .. } = &ev {
        assert_eq!(children.len(), 3);
    }

    // 6) 100 字段结构体展开——性能实测 < 300ms
    //    `big.f` 是 int[100] 数组：直接对其建 var 对象展开得到 100 个元素 f[0]..f[99]。
    let t0 = Instant::now();
    let seq = log.mgr.expand_var("big.f".to_string());
    let ev = log.wait_for(
        |e| matches!(e, Event::VarChildren { seq: s, .. } if *s == seq),
        Duration::from_secs(15),
    );
    let big_expand = t0.elapsed();
    println!("[perf] 100 字段展开: {big_expand:?}");
    assert!(big_expand < Duration::from_millis(300), "100 字段展开超过 300ms");
    if let Event::VarChildren { children, .. } = &ev {
        assert_eq!(children.len(), 100, "Big.f[100] 应展开出 100 个子字段");
        // gdb 数组元素用索引作 exp（"0".."99"），name 形如 "var3.f.0"
        assert_eq!(children[0].exp, "0");
        assert_eq!(children[99].exp, "99");
        assert!(children[0].name.starts_with("var"), "元素 name 应为 var 对象路径: {}", children[0].name);
    }

    // 7) Shift+F5 停止 → gdb 退出、状态回 Off
    log.mgr.stop();
    log.wait_status(|s| *s == DapStatus::Off, Duration::from_secs(15));

    log.mgr.shutdown();
    println!("[ok] real gdb end-to-end 全部通过");
}
