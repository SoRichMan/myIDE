//! 集成测试：用 Python mock gdb（`testdata/mock_gdb.py`）驱动 DapManager 全流程。
//!
//! 覆盖 myIDE.md 阶段 3 守护用例（无需 WSL，CI 可跑）：
//! - 启动 → 断点停下（line=10）→ 自动刷新局部变量（i == 10）与调用栈
//! - 继续 / 单步跳过
//! - 条件断点命令拼接端到端（mock 回显 cond 字段）
//! - 监视表达式求值
//! - 结构体展开（-var-create + -var-list-children）
//! - `ptype /o` 文本捕获
//! - gdb 崩溃 → Crashed 状态、IDE 不 panic、可重启恢复
//! - Stop → gdb 进程退出、状态回 Off

use ide_dap::manager::{BreakpointSpec, DapManager, DapStatus, Event, VarNode};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// 环境变量依赖的测试串行化（同一进程内并行测试会互相覆盖 LITEIDE_GDB_CMD）。
static ENV_GUARD: Mutex<()> = Mutex::new(());

fn mock_script() -> PathBuf {
    let root = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    PathBuf::from(root).join("../../testdata/mock_gdb.py")
}

const BIN: &str = r"E:\TestDeepSeek\testdata\debug_target";
const SRC: &str = r"E:\TestDeepSeek\testdata\debug_target.cpp";

/// 设置 mock 环境变量并返回守卫（测试结束前必须 remove_var 再释放守卫）。
/// 同一进程并行测试会互相覆盖环境变量，故用静态互斥锁串行化；
/// 前一个测试失败可能导致锁被污染（poison），用 into_inner 恢复。
fn with_mock_env() -> std::sync::MutexGuard<'static, ()> {
    let guard = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
    let cmdline = format!("python {}", mock_script().display());
    unsafe { std::env::set_var("LITEIDE_GDB_CMD", &cmdline) };
    guard
}

fn cleanup_mock_env() {
    unsafe { std::env::remove_var("LITEIDE_GDB_CMD") };
}

/// 事件日志：轮询得到的事件全部入列；每个等待只消费命中的那一条，
/// 其余事件保留给后续等待（支持乱序等待，不误丢事件）。
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
            assert!(
                Instant::now() < deadline,
                "等待事件超时：共 {} 条，已消费 {} 条",
                self.events.len(),
                self.consumed.iter().filter(|c| **c).count()
            );
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

fn wait_stopped_at(log: &mut EventLog, line: u32) -> Event {
    log.wait_for(|e| matches!(e, Event::Stopped { line: l, .. } if *l == line), Duration::from_secs(15))
}

fn find_var<'a>(locals: &'a [VarNode], exp: &str) -> &'a VarNode {
    locals
        .iter()
        .find(|v| v.exp == exp)
        .unwrap_or_else(|| panic!("变量 {exp} 未出现，实际 {locals:?}"))
}

#[test]
fn mock_full_debug_flow() {
    let _g = with_mock_env();
    let mut log = EventLog::new(DapManager::new());

    log.mgr.start(
        BIN.to_string(),
        vec![BreakpointSpec {
            file_win: SRC.to_string(),
            line: 10,
            condition: None,
        }],
    );

    // 1) 断点停下 @ line 10，Windows 路径正确
    let stopped = wait_stopped_at(&mut log, 10);
    if let Event::Stopped { file_win, func, .. } = &stopped {
        assert_eq!(file_win, SRC);
        assert_eq!(func, "main");
    }

    // 2) 自动刷新：局部变量 i == 10 + 调用栈
    let vars = log.wait_for(
        |e| matches!(e, Event::Variables { locals, .. } if !locals.is_empty()),
        Duration::from_secs(10),
    );
    if let Event::Variables { locals, .. } = &vars {
        assert_eq!(find_var(locals, "i").value, "10");
        assert_eq!(find_var(locals, "sum").value, "55");
        assert!(find_var(locals, "foo").expandable, "foo 是结构体，应可展开");
    }
    log.wait_for(
        |e| matches!(e, Event::Stack { frames } if !frames.is_empty()),
        Duration::from_secs(10),
    );

    // 3) F5 继续 → 下一次停下 @ line 11（mock 的 stepping 落点）
    log.mgr.continue_run();
    wait_stopped_at(&mut log, 11);

    // 4) F10 单步跳过 → line 11
    log.mgr.step_over();
    wait_stopped_at(&mut log, 11);

    // 5) 监视表达式
    log.mgr.add_watch("sum * 2".to_string());
    let vars = log.wait_for(
        |e| matches!(e, Event::Variables { watches, .. } if !watches.is_empty()),
        Duration::from_secs(10),
    );
    if let Event::Variables { watches, .. } = &vars {
        let (expr, value) = &watches[0];
        assert_eq!(expr, "sum * 2");
        assert_eq!(value, "sum * 2", "mock 应回显表达式作为值");
    }

    // 6) 结构体展开：foo → 3 个字段 a/b/c
    let seq = log.mgr.expand_var("foo".to_string());
    let ev = log.wait_for(
        |e| matches!(e, Event::VarChildren { seq: s, .. } if *s == seq),
        Duration::from_secs(10),
    );
    if let Event::VarChildren { children, type_name, .. } = &ev {
        assert_eq!(type_name, "struct Foo");
        let names: Vec<&str> = children.iter().map(|c| c.exp.as_str()).collect();
        assert_eq!(names, vec!["a", "b", "c"]);
        assert_eq!(children[0].value, "42");
        assert_eq!(children[0].type_name, "int");
    }

    // 7) ptype /o Foo → 控制台文本
    log.mgr.ptype("Foo".to_string());
    let ev = log.wait_for(|e| matches!(e, Event::Ptype { .. }), Duration::from_secs(10));
    if let Event::Ptype { text } = &ev {
        assert!(
            text.contains("total size (bytes):   32"),
            "ptype 文本应含 total size，实际: {text:?}"
        );
        assert!(text.contains("int a"));
    }

    // 8) Shift+F5 停止 → gdb 退出，状态回 Off
    log.mgr.stop();
    log.wait_status(|s| *s == DapStatus::Off, Duration::from_secs(10));

    log.mgr.shutdown();
    cleanup_mock_env();
}

#[test]
fn mock_conditional_breakpoint_roundtrip() {
    let _g = with_mock_env();
    let mut log = EventLog::new(DapManager::new());

    // 条件断点：i == 5（第 7 行 ++i 处，命中时 i == 5）
    log.mgr.start(
        BIN.to_string(),
        vec![BreakpointSpec {
            file_win: SRC.to_string(),
            line: 7,
            condition: Some("i == 5".to_string()),
        }],
    );

    // mock 回显 cond 字段 → 端到端验证命令拼接（-c "i == 5"）
    let ev = log.wait_for(
        |e| matches!(e, Event::Breakpoints(b) if !b.is_empty()),
        Duration::from_secs(15),
    );
    if let Event::Breakpoints(bps) = &ev {
        assert_eq!(bps.len(), 1);
        assert_eq!(bps[0].condition.as_deref(), Some("i == 5"));
        assert_eq!(bps[0].line, 10, "mock 固定回显 line=10");
    }

    wait_stopped_at(&mut log, 10);
    log.mgr.shutdown();
    cleanup_mock_env();
}

#[test]
fn mock_crash_then_restart() {
    let _g = with_mock_env();
    let mut log = EventLog::new(DapManager::new());

    log.mgr.start(BIN.to_string(), Vec::new());
    wait_stopped_at(&mut log, 10);

    // 触发崩溃：mock 对 -exec-finish 回复 ^error 后退出(1)
    log.mgr.step_out();
    let crashed = log.wait_status(
        |s| matches!(s, DapStatus::Crashed(_)),
        Duration::from_secs(10),
    );
    assert!(
        matches!(crashed, DapStatus::Crashed(ref d) if d.contains("exit code 1")),
        "应显示崩溃原因，实际 {crashed:?}"
    );

    // 重启：新会话可恢复
    log.mgr.start(BIN.to_string(), Vec::new());
    wait_stopped_at(&mut log, 10);

    log.mgr.shutdown();
    cleanup_mock_env();
}
