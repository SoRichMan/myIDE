//! DAP 管理器：UI 线程可用的门面（阶段 3）。
//!
//! 内部跑一个 tokio 运行时 + 会话任务，负责：
//! - 探测 WSL 发行版 / gdb 可用性
//! - 启动 gdb（MI2）、加载程序、设置断点、启动运行
//! - 转发 UI 命令（断点/单步/继续/停止/变量/栈/ptype）
//! - 程序停止时自动刷新局部变量与调用栈；gdb 崩溃不拖死 IDE
//!
//! UI 线程 **永不阻塞**：命令走 `try_send`，事件用 `try_recv` 每帧轮询；
//! 状态用 `Arc<Mutex<DapStatus>>` 随时可读。

use crate::client::{GdbClient, GdbEvent, GdbEvents, DEFAULT_TIMEOUT};
use crate::command as cmd;
use crate::mi::{MiValue, StoppedFrame};
use crate::path::{win_to_wsl, wsl_to_win};
use crate::process::{pick_wsl_distro, probe_gdb, probe_gdb_remote, spawn_gdb, spawn_gdb_remote, using_custom_command};
use ide_core::remote::RemoteTarget;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;

/// GDB 会话状态（状态栏显示）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DapStatus {
    /// 无调试会话。
    Off,
    /// 启动 gdb / 加载程序。
    Starting,
    /// gdb 就绪，程序未运行（尚未 run 或已退出）。
    Ready,
    /// 程序运行中。
    Running,
    /// 程序停在断点/单步/信号处（可查看变量）。
    Stopped,
    /// gdb 崩溃或连接断开。
    Crashed(String),
    /// WSL/gdb 缺失等不可用原因。
    Unavailable(String),
}

impl DapStatus {
    /// 是否处于可交互状态（允许单步/继续/变量操作）。
    pub fn is_interactive(&self) -> bool {
        matches!(self, DapStatus::Stopped | DapStatus::Ready)
    }
    pub fn is_stopped(&self) -> bool {
        matches!(self, DapStatus::Stopped)
    }
}

/// 断点规格（UI → 会话；Windows 路径）。
#[derive(Debug, Clone)]
pub struct BreakpointSpec {
    pub file_win: String,
    pub line: u32,
    pub condition: Option<String>,
}

/// 断点信息（会话 → UI；Windows 路径）。
#[derive(Debug, Clone)]
pub struct BreakpointInfo {
    pub number: u32,
    pub file_win: String,
    pub line: u32,
    pub condition: Option<String>,
    pub enabled: bool,
    pub times: u32,
}

/// 变量节点（局部变量/监视/展开结果共用）。
#[derive(Debug, Clone)]
pub struct VarNode {
    /// var 对象名（展开后用）；普通变量为空。
    pub name: String,
    /// 表达式（局部变量名 / 字段 exp）。
    pub exp: String,
    /// 值文本。
    pub value: String,
    /// 类型名。
    pub type_name: String,
    /// 子字段数（gdb 报告的 numchild）。
    pub numchild: u32,
    /// 是否可展开（结构体/数组）。
    pub expandable: bool,
    /// 已展开的子节点。
    pub children: Vec<VarNode>,
}

/// 调用栈帧。
#[derive(Debug, Clone)]
pub struct StackFrame {
    pub level: u32,
    pub func: String,
    pub file_win: String,
    pub line: Option<u32>,
}

/// UI → 会话 命令。
#[derive(Debug)]
pub enum Command {
    /// 启动调试会话（幂等：已在运行则忽略）。binary 为 Windows 路径。
    Start {
        binary_win: String,
        breakpoints: Vec<BreakpointSpec>,
    },
    /// F5：继续（Ready 时改为重新运行）。
    Continue,
    /// Shift+F5：中断并终止调试（gdb 退出）。
    Stop,
    /// F10 单步跳过。
    Next,
    /// F11 单步进入。
    Step,
    /// Shift+F11 单步跳出。
    Finish,
    /// F9：切换当前文件行断点（有则删，无则加）。
    ToggleBreakpoint { spec: BreakpointSpec },
    /// 按 gdb 编号删除断点。
    DeleteBreakpoint { number: u32 },
    /// 启用/禁用断点。
    EnableBreakpoint { number: u32, enable: bool },
    /// 增加监视表达式。
    AddWatch { expr: String },
    /// 移除监视表达式。
    RemoveWatch { expr: String },
    /// 展开结构体/数组（-var-create + -var-list-children）。
    ExpandVar { seq: u64, expr: String },
    /// `ptype /o` 查看类型布局。
    Ptype { type_name: String },
    Shutdown,
}

/// 会话 → UI 事件。
#[derive(Debug, Clone)]
pub enum Event {
    Status(DapStatus),
    /// 程序停止（断点/单步/信号）。
    Stopped { reason: String, file_win: String, line: u32, func: String },
    /// 程序开始运行。
    Running,
    /// 程序正常退出。
    Exited { code: Option<i64> },
    /// 断点列表变更（全量推送）。
    Breakpoints(Vec<BreakpointInfo>),
    /// 局部变量 + 监视值（每次停止自动刷新）。
    Variables { locals: Vec<VarNode>, watches: Vec<(String, String)> },
    /// 调用栈（每次停止自动刷新）。
    Stack { frames: Vec<StackFrame> },
    /// 结构体展开结果。
    VarChildren { seq: u64, expr: String, type_name: String, children: Vec<VarNode> },
    /// `ptype /o` 文本。
    Ptype { text: String },
    /// gdb 原始输出行（调试控制台）。
    Console(String),
    /// 友好错误提示。
    Error(String),
}

/// 对 UI 线程暴露的 DAP 管理器。
pub struct DapManager {
    cmd_tx: mpsc::UnboundedSender<Command>,
    ev_rx: std::sync::mpsc::Receiver<Event>,
    status: Arc<Mutex<DapStatus>>,
    /// SSH 远程目标：设置后 gdb 走 `ssh`，而不是本机 WSL。
    target: Arc<Mutex<Option<RemoteTarget>>>,
    rt: Option<tokio::runtime::Runtime>,
    seq: u64,
}

impl Default for DapManager {
    fn default() -> Self {
        Self::new()
    }
}

impl DapManager {
    pub fn new() -> Self {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<Command>();
        let (ev_tx, ev_rx) = std::sync::mpsc::channel::<Event>();
        let status = Arc::new(Mutex::new(DapStatus::Off));
        let target: Arc<Mutex<Option<RemoteTarget>>> = Arc::new(Mutex::new(None));
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("dap-worker")
            .enable_all()
            .build()
            .expect("tokio runtime 启动失败");
        let status_c = status.clone();
        let target_c = target.clone();
        let ev_tx_c = ev_tx.clone();
        rt.spawn(async move {
            session_loop(cmd_rx, ev_tx_c, status_c, target_c).await;
        });
        let _ = ev_tx;
        Self {
            cmd_tx,
            ev_rx,
            status,
            target,
            rt: Some(rt),
            seq: 0,
        }
    }

    /// 当前状态（状态栏直接读）。
    pub fn status(&self) -> DapStatus {
        self.status.lock().unwrap().clone()
    }

    /// 切换 gdb 的远端目标（`None` = 回到原来的 WSL 自动探测）。
    ///
    /// 调试会话每次都从 `Start` 重新探测，所以这里只记住目标即可。
    pub fn set_remote_target(&mut self, target: Option<RemoteTarget>) {
        *self.target.lock().unwrap() = target;
    }

    /// 拉取一个事件（每帧调用一次，非阻塞）。
    pub fn poll(&self) -> Option<Event> {
        self.ev_rx.try_recv().ok()
    }

    fn next_seq(&mut self) -> u64 {
        self.seq += 1;
        self.seq
    }

    /// 启动调试（F5，无会话时）。
    pub fn start(&mut self, binary_win: String, breakpoints: Vec<BreakpointSpec>) {
        let _ = self.cmd_tx.send(Command::Start { binary_win, breakpoints });
    }

    /// F5 继续。
    pub fn continue_run(&mut self) {
        let _ = self.cmd_tx.send(Command::Continue);
    }

    /// Shift+F5 停止。
    pub fn stop(&mut self) {
        let _ = self.cmd_tx.send(Command::Stop);
    }

    /// F10 单步跳过。
    pub fn step_over(&mut self) {
        let _ = self.cmd_tx.send(Command::Next);
    }

    /// F11 单步进入。
    pub fn step_into(&mut self) {
        let _ = self.cmd_tx.send(Command::Step);
    }

    /// Shift+F11 单步跳出。
    pub fn step_out(&mut self) {
        let _ = self.cmd_tx.send(Command::Finish);
    }

    /// F9 切换断点（会话活跃时生效；未启动时由 UI 自行维护列表，Start 时一并下发）。
    pub fn toggle_breakpoint(&mut self, spec: BreakpointSpec) {
        let _ = self.cmd_tx.send(Command::ToggleBreakpoint { spec });
    }

    pub fn delete_breakpoint(&mut self, number: u32) {
        let _ = self.cmd_tx.send(Command::DeleteBreakpoint { number });
    }

    pub fn enable_breakpoint(&mut self, number: u32, enable: bool) {
        let _ = self.cmd_tx.send(Command::EnableBreakpoint { number, enable });
    }

    pub fn add_watch(&mut self, expr: String) {
        let _ = self.cmd_tx.send(Command::AddWatch { expr });
    }

    pub fn remove_watch(&mut self, expr: String) {
        let _ = self.cmd_tx.send(Command::RemoveWatch { expr });
    }

    /// 展开结构体；结果经 `Event::VarChildren{seq, ..}` 返回。
    pub fn expand_var(&mut self, expr: String) -> u64 {
        let seq = self.next_seq();
        let _ = self.cmd_tx.send(Command::ExpandVar { seq, expr });
        seq
    }

    /// `ptype /o`；结果经 `Event::Ptype{text}` 返回。
    pub fn ptype(&mut self, type_name: String) {
        let _ = self.cmd_tx.send(Command::Ptype { type_name });
    }

    /// 关闭。
    pub fn shutdown(&mut self) {
        let _ = self.cmd_tx.send(Command::Shutdown);
    }
}

impl Drop for DapManager {
    fn drop(&mut self) {
        let _ = self.cmd_tx.send(Command::Shutdown);
        std::thread::sleep(Duration::from_millis(150));
        self.rt.take();
    }
}

// ---------------------------------------------------------------------------
// 会话任务
// ---------------------------------------------------------------------------

struct SessionState {
    ui_tx: std::sync::mpsc::Sender<Event>,
    status: Arc<Mutex<DapStatus>>,
    /// SSH 远程目标（`None` = 沿用原来的 WSL 自动探测）。
    target: Arc<Mutex<Option<RemoteTarget>>>,
    cmd_rx: mpsc::UnboundedReceiver<Command>,
    breakpoints: Vec<BreakpointInfo>,
    watches: Vec<String>,
    binary_wsl: Option<String>,
    /// 是否正在执行 `-exec-*`（等待 *stopped 中）。
    in_exec: bool,
}

enum ReadyOutcome {
    Crash,
    Exit,
}

/// 这一轮 gdb 跑在哪里。
enum Transport {
    /// WSL 发行版（`None` 仅出现在 `LITEIDE_GDB_CMD` 覆盖模式）。
    Wsl(Option<String>),
    /// SSH 主机。
    Ssh(RemoteTarget),
}

async fn session_loop(
    cmd_rx: mpsc::UnboundedReceiver<Command>,
    ui_tx: std::sync::mpsc::Sender<Event>,
    status: Arc<Mutex<DapStatus>>,
    target: Arc<Mutex<Option<RemoteTarget>>>,
) {
    let mut st = SessionState {
        ui_tx,
        status,
        target,
        cmd_rx,
        breakpoints: Vec::new(),
        watches: Vec::new(),
        binary_wsl: None,
        in_exec: false,
    };
    let mut just_crashed = false;

    loop {
        if !just_crashed {
            set_status(&st, DapStatus::Off);
        }
        // ---- 等待启动/关闭命令 ----
        let payload = loop {
            match st.cmd_rx.recv().await {
                Some(Command::Start { binary_win, breakpoints }) => {
                    break Some((binary_win, breakpoints));
                }
                Some(Command::Shutdown) | None => return,
                _ => { /* 未启动时忽略其他命令 */ }
            }
        };
        let Some((binary_win, initial_bps)) = payload else { return };
        let Some(binary_wsl) = win_to_wsl(&binary_win) else {
            set_status(
                &st,
                DapStatus::Unavailable(format!("无法识别目标程序路径: {binary_win}")),
            );
            just_crashed = true;
            continue;
        };
        st.binary_wsl = Some(binary_wsl.clone());

        // ---- 探测环境（LITEIDE_GDB_CMD 覆盖时跳过，测试用） ----
        let wanted = st.target.lock().unwrap().clone();
        let transport = if using_custom_command() {
            tracing::info!("使用 LITEIDE_GDB_CMD 覆盖启动命令（跳过远端探测）");
            Transport::Wsl(None)
        } else if let Some(t) = wanted.filter(|t| t.is_ssh()) {
            set_status(&st, DapStatus::Starting);
            match probe_gdb_remote(&t).await {
                Ok(path) => tracing::info!("{} gdb at {path}", t.label()),
                Err(e) => {
                    tracing::warn!("gdb probe failed: {e}");
                    set_status(
                        &st,
                        DapStatus::Unavailable(ide_core::diagnostics::tool_missing_on(
                            &t, "gdb", &e,
                        )),
                    );
                    just_crashed = true;
                    continue;
                }
            }
            Transport::Ssh(t)
        } else {
            set_status(&st, DapStatus::Starting);
            let Some(d) = pick_wsl_distro().await else {
                set_status(&st, DapStatus::Unavailable(ide_core::diagnostics::wsl_missing()));
                just_crashed = true;
                continue;
            };
            match probe_gdb(&d).await {
                Ok(path) => tracing::info!("WSL distro={d}, gdb at {path}"),
                Err(e) => {
                    tracing::warn!("gdb probe failed: {e}");
                    set_status(
                        &st,
                        DapStatus::Unavailable(ide_core::diagnostics::tool_missing("gdb", &e)),
                    );
                    just_crashed = true;
                    continue;
                }
            }
            Transport::Wsl(Some(d))
        };

        // ---- 启动 gdb 并初始化 ----
        set_status(&st, DapStatus::Starting);
        let spawned = match &transport {
            Transport::Ssh(t) => spawn_gdb_remote(t),
            Transport::Wsl(d) => spawn_gdb(d.as_deref().unwrap_or("mock")),
        };
        let child = match spawned {
            Ok(c) => c,
            Err(e) => {
                set_status(&st, DapStatus::Unavailable(format!("启动 gdb 失败: {e}")));
                just_crashed = true;
                continue;
            }
        };
        let (mut client, mut events) = match GdbClient::connect(child, DEFAULT_TIMEOUT) {
            Ok((c, e, _writer)) => (c, e),
            Err(e) => {
                set_status(&st, DapStatus::Unavailable(format!("接管 gdb stdio 失败: {e}")));
                just_crashed = true;
                continue;
            }
        };

        // 初始化：关闭分页/确认（确定性），加载程序
        let _ = client.command(&cmd::gdb_set("pagination", "off")).await;
        let _ = client.command(&cmd::gdb_set("confirm", "off")).await;
        let load = client.command(&cmd::file_exec_and_symbols(&binary_wsl)).await;
        match load {
            Ok(r) if r.class == "error" => {
                let msg = r.error_msg().unwrap_or_else(|| "未知错误".to_string());
                let _ = client.kill().await;
                set_status(
                    &st,
                    DapStatus::Unavailable(format!("无法加载程序 {binary_wsl}: {msg}")),
                );
                just_crashed = true;
                continue;
            }
            Ok(_) => {}
            Err(e) => {
                tracing::warn!("file-exec-and-symbols 失败: {e}");
                let _ = client.kill().await;
                set_status(&st, DapStatus::Unavailable(format!("加载程序失败: {e}")));
                just_crashed = true;
                continue;
            }
        }

        // 设置初始断点
        st.breakpoints.clear();
        for spec in &initial_bps {
            apply_breakpoint_spec(&mut st, &mut client, spec).await;
        }
        let _ = st.ui_tx.send(Event::Breakpoints(st.breakpoints.clone()));

        set_status(&st, DapStatus::Ready);
        // 自动开始运行（F5 语义）
        if let Err(e) = run_inferior(&mut st, &mut client).await {
            emit_error(&st, &e);
        }

        // ---- 就绪循环 ----
        let outcome = ready_loop(&mut st, &mut client, &mut events).await;
        match outcome {
            ReadyOutcome::Crash => {
                just_crashed = true;
                continue;
            }
            ReadyOutcome::Exit => {
                // 会话正常结束（Stop/Shutdown）：回到 Off，通知 UI
                set_status(&st, DapStatus::Off);
                return;
            }
        }
    }
}

/// 把一条断点规格应用到 gdb（供启动时批量设置）。
async fn apply_breakpoint_spec(st: &mut SessionState, client: &mut GdbClient, spec: &BreakpointSpec) {
    let Some(file_wsl) = win_to_wsl(&spec.file_win) else { return };
    let location = format!("{file_wsl}:{}", spec.line);
    let resp = client.command(&cmd::break_insert(&location, spec.condition.as_deref())).await;
    match resp {
        Ok(r) if r.class == "error" => {
            let msg = r.error_msg().unwrap_or_else(|| "未知错误".to_string());
            emit_error(st, &format!("断点 {location} 设置失败: {msg}"));
        }
        Ok(r) => {
            if let Some(bkpt) = r.get("bkpt").and_then(parse_bkpt) {
                upsert_breakpoint(st, bkpt);
            }
        }
        Err(e) => emit_error(st, &format!("断点 {location} 设置失败: {e}")),
    }
}

async fn run_inferior(st: &mut SessionState, client: &mut GdbClient) -> Result<(), String> {
    match client.command(&cmd::exec_run()).await {
        Ok(r) if r.class == "running" => {
            st.in_exec = true;
            set_status(st, DapStatus::Running);
            let _ = st.ui_tx.send(Event::Running);
            Ok(())
        }
        Ok(r) => {
            let msg = r.error_msg().unwrap_or_else(|| format!("意外响应 {}", r.class));
            Err(msg)
        }
        Err(e) => Err(e.to_string()),
    }
}

async fn ready_loop(
    st: &mut SessionState,
    client: &mut GdbClient,
    events: &mut GdbEvents,
) -> ReadyOutcome {
    loop {
        tokio::select! {
            cmd = st.cmd_rx.recv() => {
                let Some(cmd) = cmd else { return ReadyOutcome::Exit; };
                let done = handle_command(st, client, cmd).await;
                if let Some(outcome) = done {
                    return outcome;
                }
            }
            ev = events.recv() => {
                match ev {
                    Some(ev) => {
                        let fatal = handle_event(st, client, ev).await;
                        if let Some(outcome) = fatal {
                            return outcome;
                        }
                    }
                    None => {
                        set_status(st, DapStatus::Crashed("gdb 事件通道关闭".to_string()));
                        return ReadyOutcome::Crash;
                    }
                }
            }
        }
    }
}

/// 返回 `Some(outcome)` 表示就绪循环应退出。
async fn handle_command(
    st: &mut SessionState,
    client: &mut GdbClient,
    cmd: Command,
) -> Option<ReadyOutcome> {
    match cmd {
        Command::Start { .. } => { /* 已在运行 */ }
        Command::Continue => {
            let status = st.status.lock().unwrap().clone();
            let resp = match status {
                DapStatus::Ready => client.command(&cmd::exec_run()).await,
                DapStatus::Stopped => client.command(&cmd::exec_continue()).await,
                _ => return None,
            };
            match resp {
                Ok(r) if r.class == "running" => {
                    st.in_exec = true;
                    set_status(st, DapStatus::Running);
                    let _ = st.ui_tx.send(Event::Running);
                }
                Ok(r) => emit_error(st, &r.error_msg().unwrap_or_else(|| format!("意外响应 {}", r.class))),
                Err(e) => emit_error(st, &e.to_string()),
            }
        }
        Command::Stop => {
            // 运行中先中断，再退出 gdb
            if matches!(*st.status.lock().unwrap(), DapStatus::Running) {
                let _ = tokio::time::timeout(
                    Duration::from_secs(3),
                    client.command(&cmd::exec_interrupt()),
                )
                .await;
            }
            let _ = tokio::time::timeout(Duration::from_secs(5), client.command(&cmd::gdb_exit())).await;
            let _ = client.kill().await; // 兜底
            return Some(ReadyOutcome::Exit);
        }
        Command::Next => step_command(st, client, &cmd::exec_next()).await,
        Command::Step => step_command(st, client, &cmd::exec_step()).await,
        Command::Finish => step_command(st, client, &cmd::exec_finish()).await,
        Command::ToggleBreakpoint { spec } => {
            toggle_breakpoint(st, client, &spec).await;
        }
        Command::DeleteBreakpoint { number } => {
            let _ = client.command(&cmd::break_delete(&[number])).await;
            st.breakpoints.retain(|b| b.number != number);
            let _ = st.ui_tx.send(Event::Breakpoints(st.breakpoints.clone()));
        }
        Command::EnableBreakpoint { number, enable } => {
            let c = if enable { cmd::break_enable(&[number]) } else { cmd::break_disable(&[number]) };
            let _ = client.command(&c).await;
            if let Some(bp) = st.breakpoints.iter_mut().find(|b| b.number == number) {
                bp.enabled = enable;
            }
            let _ = st.ui_tx.send(Event::Breakpoints(st.breakpoints.clone()));
        }
        Command::AddWatch { expr } => {
            if !st.watches.iter().any(|w| w == &expr) {
                st.watches.push(expr);
            }
            if st.status.lock().unwrap().is_stopped() {
                refresh_variables(st, client).await;
            }
        }
        Command::RemoveWatch { expr } => {
            st.watches.retain(|w| w != &expr);
            if st.status.lock().unwrap().is_stopped() {
                refresh_variables(st, client).await;
            }
        }
        Command::ExpandVar { seq, expr } => {
            expand_var(st, client, seq, &expr).await;
        }
        Command::Ptype { type_name } => {
            let resp = client.command(&cmd::ptype_offset(&type_name)).await;
            match resp {
                Ok(r) => {
                    let text: String = r.console.concat();
                    let _ = st.ui_tx.send(Event::Ptype { text });
                }
                Err(e) => emit_error(st, &format!("ptype /o {type_name} 失败: {e}")),
            }
        }
        Command::Shutdown => {
            let _ = client.command(&cmd::gdb_exit()).await;
            let _ = client.kill().await;
            return Some(ReadyOutcome::Exit);
        }
    }
    None
}

async fn step_command(st: &mut SessionState, client: &mut GdbClient, command: &str) {
    if !st.status.lock().unwrap().is_stopped() {
        return;
    }
    match client.command(command).await {
        Ok(r) if r.class == "running" => {
            st.in_exec = true;
            set_status(st, DapStatus::Running);
            let _ = st.ui_tx.send(Event::Running);
        }
        Ok(r) => emit_error(st, &r.error_msg().unwrap_or_else(|| format!("意外响应 {}", r.class))),
        Err(e) => emit_error(st, &e.to_string()),
    }
}

async fn toggle_breakpoint(st: &mut SessionState, client: &mut GdbClient, spec: &BreakpointSpec) {
    // 同文件同行 → 删除；否则新增
    let existing = st
        .breakpoints
        .iter()
        .find(|b| b.file_win == spec.file_win && b.line == spec.line)
        .cloned();
    if let Some(bp) = existing {
        let _ = client.command(&cmd::break_delete(&[bp.number])).await;
        st.breakpoints.retain(|b| b.number != bp.number);
    } else {
        apply_breakpoint_spec(st, client, spec).await;
    }
    let _ = st.ui_tx.send(Event::Breakpoints(st.breakpoints.clone()));
}

/// 返回 `Some(outcome)` 表示就绪循环应退出。
async fn handle_event(
    st: &mut SessionState,
    client: &mut GdbClient,
    ev: GdbEvent,
) -> Option<ReadyOutcome> {
    match ev {
        GdbEvent::Stopped { reason, frame, exit_code } => {
            st.in_exec = false;
            if reason == "exited" {
                set_status(st, DapStatus::Ready);
                let _ = st.ui_tx.send(Event::Exited { code: exit_code });
                return None;
            }
            let (file_win, line, func) = stopped_location(&frame);
            set_status(st, DapStatus::Stopped);
            let _ = st.ui_tx.send(Event::Stopped {
                reason: reason.clone(),
                file_win: file_win.clone(),
                line,
                func: func.clone(),
            });
            // 停止后自动刷新局部变量 + 调用栈 + 监视值
            refresh_variables(st, client).await;
        }
        GdbEvent::Async { class, results } => match class.as_str() {
            "breakpoint-created" | "breakpoint-modified" => {
                if let Some(bkpt) = results
                    .iter()
                    .find(|(k, _)| k == "bkpt")
                    .and_then(|(_, v)| parse_bkpt(v))
                {
                    upsert_breakpoint(st, bkpt);
                    let _ = st.ui_tx.send(Event::Breakpoints(st.breakpoints.clone()));
                }
            }
            "breakpoint-deleted" => {
                if let Some(number) = results
                    .iter()
                    .find(|(k, _)| k == "bkpt")
                    .and_then(|(_, v)| v.get("number"))
                    .and_then(|v| v.as_str())
                    .and_then(|s| s.parse().ok())
                {
                    st.breakpoints.retain(|b| b.number != number);
                    let _ = st.ui_tx.send(Event::Breakpoints(st.breakpoints.clone()));
                }
            }
            _ => { /* thread-created 等忽略 */ }
        },
        GdbEvent::Console(text) | GdbEvent::Target(text) | GdbEvent::Log(text) => {
            let _ = st.ui_tx.send(Event::Console(text));
        }
        GdbEvent::Output(text) => {
            if !text.trim().is_empty() {
                let _ = st.ui_tx.send(Event::Console(text));
            }
        }
        GdbEvent::ProcessExit { code } => {
            let detail = match code {
                Some(c) => format!("gdb 已退出（exit code {c}）"),
                None => "gdb 连接已断开".to_string(),
            };
            set_status(st, DapStatus::Crashed(detail));
            return Some(ReadyOutcome::Crash);
        }
    }
    None
}

fn stopped_location(frame: &Option<StoppedFrame>) -> (String, u32, String) {
    match frame {
        Some(f) => {
            let file_win = wsl_to_win(&f.fullname).unwrap_or_else(|| f.fullname.clone());
            (file_win, f.line.unwrap_or(0), f.func.clone())
        }
        None => (String::new(), 0, String::new()),
    }
}

// ---------------------------------------------------------------------------
// 变量 / 栈 / 断点解析
// ---------------------------------------------------------------------------

/// 停止后刷新：调用栈 + 局部变量 + 监视值（合并为一次 Variables 事件）。
async fn refresh_variables(st: &mut SessionState, client: &mut GdbClient) {
    // 调用栈
    match client.command(&cmd::stack_list_frames()).await {
        Ok(r) => {
            let frames = r
                .get("stack")
                .and_then(|v| v.as_list())
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|it| it.as_tuple())
                        .filter_map(parse_stack_frame)
                        .collect()
                })
                .unwrap_or_default();
            let _ = st.ui_tx.send(Event::Stack { frames });
        }
        Err(e) => emit_error(st, &format!("读取调用栈失败: {e}")),
    }
    // 局部变量（带值）
    let locals = match client.command(&cmd::stack_list_variables()).await {
        Ok(r) => r
            .get("variables")
            .and_then(|v| v.as_list())
            .map(|items| items.iter().filter_map(|it| it.as_tuple()).filter_map(parse_var).collect())
            .unwrap_or_default(),
        Err(e) => {
            emit_error(st, &format!("读取局部变量失败: {e}"));
            Vec::new()
        }
    };
    // 监视表达式
    let mut watches = Vec::with_capacity(st.watches.len());
    for expr in &st.watches {
        match client.command(&cmd::data_evaluate_expression(expr)).await {
            Ok(r) if r.class == "error" => {
                watches.push((expr.clone(), format!("⨯ {}", r.error_msg().unwrap_or_default())));
            }
            Ok(r) => {
                let v = r.get("value").and_then(|v| v.as_str()).unwrap_or("");
                watches.push((expr.clone(), v.to_string()));
            }
            Err(e) => watches.push((expr.clone(), format!("⨯ {e}"))),
        }
    }
    let _ = st.ui_tx.send(Event::Variables { locals, watches });
}

/// 展开结构体/数组：-var-create → -var-list-children。
async fn expand_var(st: &mut SessionState, client: &mut GdbClient, seq: u64, expr: &str) {
    let created = client.command(&cmd::var_create(expr)).await;
    let (name, type_name) = match created {
        Ok(r) if r.class == "error" => {
            emit_error(st, &format!("变量 '{expr}' 不可用: {}", r.error_msg().unwrap_or_default()));
            return;
        }
        Ok(r) => {
            let name = r.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let ty = r.get("type").and_then(|v| v.as_str()).unwrap_or("").to_string();
            (name, ty)
        }
        Err(e) => {
            emit_error(st, &format!("创建变量对象 '{expr}' 失败: {e}"));
            return;
        }
    };
    if name.is_empty() {
        emit_error(st, &format!("变量 '{expr}' 创建失败（无名称）"));
        return;
    }
    let resp = client.command(&cmd::var_list_children(&name)).await;
    let mut children: Vec<VarNode> = match resp {
        Ok(r) => r
            .get("children")
            .and_then(|v| v.as_list())
            .map(|items| {
                items
                    .iter()
                    .filter_map(|it| it.as_tuple())
                    .filter_map(parse_var)
                    .collect()
            })
            .unwrap_or_default(),
        Err(e) => {
            emit_error(st, &format!("展开 '{expr}' 失败: {e}"));
            return;
        }
    };

    // 展平 C++ 访问级别分组：gdb 15.x 对结构体 var 对象会包一层 `public`/`private`
    // 伪子节点（name="var1.public", exp="public", numchild=N）。单子节点且为访问
    // 级别时自动下钻一层，让面板直接显示真实字段。
    if children.len() == 1
        && children[0].type_name.is_empty()
        && matches!(children[0].exp.as_str(), "public" | "private" | "protected")
    {
        if let Ok(r) = client.command(&cmd::var_list_children(&children[0].name)).await {
            children = r
                .get("children")
                .and_then(|v| v.as_list())
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|it| it.as_tuple())
                        .filter_map(parse_var)
                        .collect()
                })
                .unwrap_or_default();
        }
    }

    let _ = st.ui_tx.send(Event::VarChildren { seq, expr: expr.to_string(), type_name, children });
}

/// 解析 `-stack-list-frames` 的 frame tuple。
fn parse_stack_frame(t: &[(String, MiValue)]) -> Option<StackFrame> {
    let level = t
        .iter()
        .find(|(k, _)| k == "level")
        .and_then(|(_, v)| v.as_str())
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let func = t.iter().find(|(k, _)| k == "func").and_then(|(_, v)| v.as_str()).unwrap_or("").to_string();
    let fullname = t
        .iter()
        .find(|(k, _)| k == "fullname")
        .and_then(|(_, v)| v.as_str())
        .unwrap_or("")
        .to_string();
    let file = t.iter().find(|(k, _)| k == "file").and_then(|(_, v)| v.as_str()).unwrap_or("").to_string();
    let file_win = wsl_to_win(&fullname).unwrap_or({
        if fullname.is_empty() { file } else { fullname }
    });
    let line = t
        .iter()
        .find(|(k, _)| k == "line")
        .and_then(|(_, v)| v.as_str())
        .and_then(|s| s.parse().ok());
    Some(StackFrame { level, func, file_win, line })
}

/// 解析局部变量/子字段 tuple → VarNode。
fn parse_var(t: &[(String, MiValue)]) -> Option<VarNode> {
    let get = |key: &str| t.iter().find(|(k, _)| k == key).and_then(|(_, v)| v.as_str());
    let name = get("name").unwrap_or("").to_string();
    let exp_raw = get("exp").unwrap_or("").to_string();
    let value = get("value").unwrap_or("").to_string();
    let type_name = get("type").unwrap_or("").to_string();
    let numchild = get("numchild").and_then(|s| s.parse().ok()).unwrap_or(0);
    // 结构体/类/联合/数组/指针可展开
    let expandable = numchild > 0
        || type_name.starts_with("struct ")
        || type_name.starts_with("class ")
        || type_name.starts_with("union ")
        || type_name.ends_with(']');
    let exp = if exp_raw.is_empty() { name.clone() } else { exp_raw };
    Some(VarNode {
        name,
        exp,
        value,
        type_name,
        numchild,
        expandable,
        children: Vec::new(),
    })
}

/// 解析 bkpt tuple → BreakpointInfo。
fn parse_bkpt(v: &MiValue) -> Option<BreakpointInfo> {
    let number = v.get("number")?.as_str()?.parse().ok()?;
    let fullname = v.get("fullname").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let file = v.get("file").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let file_win = wsl_to_win(&fullname).unwrap_or({
        if fullname.is_empty() { file } else { fullname }
    });
    let line = v.get("line").and_then(|v| v.as_str()).and_then(|s| s.parse().ok()).unwrap_or(0);
    let condition = v.get("cond").and_then(|v| v.as_str()).map(str::to_string);
    let enabled = v.get("enabled").and_then(|v| v.as_str()) == Some("y");
    let times = v.get("times").and_then(|v| v.as_str()).and_then(|s| s.parse().ok()).unwrap_or(0);
    Some(BreakpointInfo { number, file_win, line, condition, enabled, times })
}

fn upsert_breakpoint(st: &mut SessionState, bp: BreakpointInfo) {
    if let Some(existing) = st.breakpoints.iter_mut().find(|b| b.number == bp.number) {
        *existing = bp;
    } else {
        st.breakpoints.push(bp);
    }
}

fn emit_error(st: &SessionState, msg: &str) {
    tracing::warn!("dap: {msg}");
    let _ = st.ui_tx.send(Event::Error(msg.to_string()));
}

fn set_status(st: &SessionState, status: DapStatus) {
    tracing::info!("DAP status: {status:?}");
    *st.status.lock().unwrap() = status.clone();
    let _ = st.ui_tx.send(Event::Status(status));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mi::MiValue;

    fn mk_tuple(fields: &[(&str, &str)]) -> MiValue {
        MiValue::Tuple(
            fields
                .iter()
                .map(|(k, v)| (k.to_string(), MiValue::String(v.to_string())))
                .collect(),
        )
    }

    #[test]
    fn parse_var_leaf_and_struct() {
        let leaf = mk_tuple(&[("name", "i"), ("value", "10"), ("type", "int")]);
        let v = parse_var(leaf.as_tuple().unwrap()).unwrap();
        assert_eq!(v.exp, "i");
        assert_eq!(v.value, "10");
        assert!(!v.expandable);

        let st = mk_tuple(&[
            ("name", "foo"),
            ("value", "{a = 42}"),
            ("type", "struct Foo"),
        ]);
        let v = parse_var(st.as_tuple().unwrap()).unwrap();
        assert!(v.expandable);
        assert_eq!(v.type_name, "struct Foo");

        let arr = mk_tuple(&[("name", "c"), ("value", "\"hello\""), ("type", "char [16]")]);
        let v = parse_var(arr.as_tuple().unwrap()).unwrap();
        assert!(v.expandable, "数组应可展开");
    }

    #[test]
    fn parse_bkpt_basic() {
        let v = mk_tuple(&[
            ("number", "1"),
            ("enabled", "y"),
            ("line", "10"),
            ("times", "2"),
            ("fullname", "/mnt/e/TestDeepSeek/testdata/debug_target.cpp"),
            ("file", "debug_target.cpp"),
        ]);
        let bp = parse_bkpt(&v).unwrap();
        assert_eq!(bp.number, 1);
        assert_eq!(bp.line, 10);
        assert_eq!(bp.file_win, r"E:\TestDeepSeek\testdata\debug_target.cpp");
        assert!(bp.enabled);
        assert_eq!(bp.times, 2);
        assert_eq!(bp.condition, None);
    }

    #[test]
    fn parse_bkpt_with_condition() {
        let v = mk_tuple(&[
            ("number", "2"),
            ("enabled", "y"),
            ("line", "7"),
            ("times", "0"),
            ("cond", "i == 5"),
            ("fullname", "/mnt/e/x/debug_target.cpp"),
            ("file", "debug_target.cpp"),
        ]);
        let bp = parse_bkpt(&v).unwrap();
        assert_eq!(bp.condition.as_deref(), Some("i == 5"));
    }

    #[test]
    fn parse_stack_frame_basic() {
        let v = mk_tuple(&[
            ("level", "0"),
            ("func", "main"),
            ("line", "10"),
            ("fullname", "/mnt/e/TestDeepSeek/testdata/debug_target.cpp"),
            ("file", "debug_target.cpp"),
        ]);
        let f = parse_stack_frame(v.as_tuple().unwrap()).unwrap();
        assert_eq!(f.level, 0);
        assert_eq!(f.func, "main");
        assert_eq!(f.line, Some(10));
        assert_eq!(f.file_win, r"E:\TestDeepSeek\testdata\debug_target.cpp");
    }

    #[test]
    fn upsert_dedup_by_number() {
        let mut st = SessionState {
            ui_tx: std::sync::mpsc::channel().0,
            status: Arc::new(Mutex::new(DapStatus::Off)),
            target: Arc::new(Mutex::new(None)),
            cmd_rx: mpsc::unbounded_channel().1,
            breakpoints: Vec::new(),
            watches: Vec::new(),
            binary_wsl: None,
            in_exec: false,
        };
        let mk = |n: u32| BreakpointInfo {
            number: n,
            file_win: "f".to_string(),
            line: 1,
            condition: None,
            enabled: true,
            times: 0,
        };
        upsert_breakpoint(&mut st, mk(1));
        upsert_breakpoint(&mut st, mk(1));
        upsert_breakpoint(&mut st, mk(2));
        assert_eq!(st.breakpoints.len(), 2);
    }
}
