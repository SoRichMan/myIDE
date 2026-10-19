//! LSP 管理器：UI 线程可用的门面。
//!
//! 内部跑一个 tokio 运行时 + 会话任务，负责：
//! - 探测 WSL 发行版 / clangd 可用性
//! - 启动/重启 clangd、`initialize → initialized` 握手
//! - 转发 UI 命令（打开/编辑/跳转/hover/大纲）
//! - 崩溃检测与重启（clangd 退出 → `Crashed` → 用户触发 `Restart`）
//!
//! UI 线程 **永不阻塞**：命令走 `try_send`，事件用 `try_recv` 每帧轮询；
//! 状态用 `Arc<Mutex<LspStatus>>` 随时可读。

use crate::client::{LspClient, LspError, LspEvents, ServerEvent};
use crate::message::{HoverInfo, Location, Position, SymbolInfo};
use crate::process::{pick_wsl_distro, probe_clangd, spawn_clangd};
use crate::DEFAULT_TIMEOUT;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;

/// LSP 连接状态（状态栏显示）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LspStatus {
    /// 未启动。
    Off,
    /// 探测 WSL / clangd。
    Detecting,
    /// 启动 clangd / 握手。
    Connecting,
    /// 就绪。
    Ready,
    /// clangd 崩溃或连接断开（可重启）。
    Crashed(String),
    /// WSL/clangd 缺失等不可用原因（可重试）。
    Unavailable(String),
}

impl LspStatus {
    /// 是否处于可交互状态（允许发文档请求）。
    pub fn is_ready(&self) -> bool {
        matches!(self, LspStatus::Ready)
    }
    /// 是否允许触发重启。
    pub fn is_recoverable(&self) -> bool {
        matches!(self, LspStatus::Crashed(_) | LspStatus::Unavailable(_))
    }
}

/// UI → 会话 命令。
#[derive(Debug)]
pub enum Command {
    /// 启动（幂等：已在运行则忽略）。`root_uri` 是 WSL 视角的项目根 URI。
    Start { root_uri: String },
    Open { uri: String, text: String, version: i32 },
    Change { uri: String, text: String, version: i32 },
    Save { uri: String },
    Close { uri: String },
    Definition { uri: String, pos: Position, seq: u64 },
    Hover { uri: String, pos: Position, seq: u64 },
    Symbols { uri: String, seq: u64 },
    Restart,
    Shutdown,
}

/// 会话 → UI 事件。
#[derive(Debug)]
pub enum Event {
    Status(LspStatus),
    Definition { seq: u64, result: Result<Vec<Location>, String> },
    Hover { seq: u64, result: Result<Option<HoverInfo>, String> },
    Symbols { seq: u64, result: Result<Vec<SymbolInfo>, String> },
    ServerLog(String),
    Diagnostics { uri: String, items: serde_json::Value },
}

/// 对 UI 线程暴露的 LSP 管理器。
pub struct LspManager {
    cmd_tx: mpsc::UnboundedSender<Command>,
    ev_rx: std::sync::mpsc::Receiver<Event>,
    status: Arc<Mutex<LspStatus>>,
    /// 当前连接的 WSL 发行版（无远程连接时为 None）。状态栏「连接」项读取。
    remote: Arc<Mutex<Option<String>>>,
    rt: Option<tokio::runtime::Runtime>,
    seq: u64,
}

impl Default for LspManager {
    fn default() -> Self {
        Self::new()
    }
}

impl LspManager {
    pub fn new() -> Self {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<Command>();
        let (ev_tx, ev_rx) = std::sync::mpsc::channel::<Event>();
        let status = Arc::new(Mutex::new(LspStatus::Off));
        let remote = Arc::new(Mutex::new(None));
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("lsp-worker")
            .enable_all()
            .build()
            .expect("tokio runtime 启动失败");
        let status_c = status.clone();
        let remote_c = remote.clone();
        let ev_tx_c = ev_tx.clone();
        rt.spawn(async move {
            session_loop(cmd_rx, ev_tx_c, status_c, remote_c).await;
        });
        let _ = ev_tx;
        Self {
            cmd_tx,
            ev_rx,
            status,
            remote,
            rt: Some(rt),
            seq: 0,
        }
    }

    /// 当前状态（状态栏直接读）。
    pub fn status(&self) -> LspStatus {
        self.status.lock().unwrap().clone()
    }

    /// 已连接的 WSL 发行版名（未连接时为 None）。
    pub fn remote_name(&self) -> Option<String> {
        self.remote.lock().unwrap().clone()
    }

    /// 拉取一个事件（每帧调用一次，非阻塞）。
    pub fn poll(&self) -> Option<Event> {
        self.ev_rx.try_recv().ok()
    }

    fn next_seq(&mut self) -> u64 {
        self.seq += 1;
        self.seq
    }

    /// 启动（或忽略）。`root_win` 用于展示，`root_uri` 发给 clangd。
    pub fn start(&mut self, root_uri: String) {
        let _ = self.cmd_tx.send(Command::Start { root_uri });
    }

    pub fn open_document(&mut self, uri: String, text: String, version: i32) {
        let _ = self.cmd_tx.send(Command::Open { uri, text, version });
    }

    pub fn change_document(&mut self, uri: String, text: String, version: i32) {
        let _ = self.cmd_tx.send(Command::Change { uri, text, version });
    }

    pub fn save_document(&mut self, uri: String) {
        let _ = self.cmd_tx.send(Command::Save { uri });
    }

    pub fn close_document(&mut self, uri: String) {
        let _ = self.cmd_tx.send(Command::Close { uri });
    }

    /// 请求跳转定义；结果通过 `Event::Definition{seq, ..}` 返回。
    pub fn request_definition(&mut self, uri: String, pos: Position) -> u64 {
        let seq = self.next_seq();
        let _ = self.cmd_tx.send(Command::Definition { uri, pos, seq });
        seq
    }

    /// 请求 hover；结果通过 `Event::Hover{seq, ..}` 返回。
    pub fn request_hover(&mut self, uri: String, pos: Position) -> u64 {
        let seq = self.next_seq();
        let _ = self.cmd_tx.send(Command::Hover { uri, pos, seq });
        seq
    }

    /// 请求文档符号；结果通过 `Event::Symbols{seq, ..}` 返回。
    pub fn request_symbols(&mut self, uri: String) -> u64 {
        let seq = self.next_seq();
        let _ = self.cmd_tx.send(Command::Symbols { uri, seq });
        seq
    }

    /// 重启 clangd（崩溃/不可用后调用）。
    pub fn restart(&mut self) {
        let _ = self.cmd_tx.send(Command::Restart);
    }

    /// 关闭。
    pub fn shutdown(&mut self) {
        let _ = self.cmd_tx.send(Command::Shutdown);
    }
}

impl Drop for LspManager {
    fn drop(&mut self) {
        let _ = self.cmd_tx.send(Command::Shutdown);
        // 给会话一个短暂的善后时间（写 exit 通知），随后运行时随结构体销毁。
        std::thread::sleep(Duration::from_millis(100));
        self.rt.take();
    }
}

// ---------------------------------------------------------------------------
// 会话任务
// ---------------------------------------------------------------------------

struct SessionState {
    ui_tx: std::sync::mpsc::Sender<Event>,
    status: Arc<Mutex<LspStatus>>,
    remote: Arc<Mutex<Option<String>>>,
    cmd_rx: mpsc::UnboundedReceiver<Command>,
    root_uri: Option<String>,
}

/// 就绪循环的退出原因。
enum ReadyOutcome {
    /// clangd 崩溃/连接断开（保留 Crashed 状态，等用户 Restart）。
    Crash,
    /// 用户主动重启（立即重连）。
    Restart,
    /// 关闭会话。
    Exit,
}

async fn session_loop(
    cmd_rx: mpsc::UnboundedReceiver<Command>,
    ui_tx: std::sync::mpsc::Sender<Event>,
    status: Arc<Mutex<LspStatus>>,
    remote: Arc<Mutex<Option<String>>>,
) {
    let mut st = SessionState {
        ui_tx,
        status,
        remote,
        cmd_rx,
        root_uri: None,
    };
    // 崩溃后保留 Crashed 状态显示；新会话才显示 Off。
    let mut just_crashed = false;

    loop {
        if !just_crashed {
            set_status(&st, LspStatus::Off);
        }
        // ---- 等待启动/重启命令 ----
        loop {
            match st.cmd_rx.recv().await {
                Some(Command::Start { root_uri }) => {
                    st.root_uri = Some(root_uri);
                    break;
                }
                Some(Command::Restart) => break,
                Some(Command::Shutdown) | None => return,
                _ => { /* 未就绪时忽略文档命令 */ }
            }
        }
        just_crashed = false;

        // ---- 探测环境（LITEIDE_LSP_CMD 覆盖时跳过，测试用） ----
        let distro = if !crate::process::using_custom_command() {
            set_status(&st, LspStatus::Detecting);
            let Some(d) = pick_wsl_distro().await else {
                set_status(
                    &st,
                    LspStatus::Unavailable(ide_core::diagnostics::wsl_missing()),
                );
                just_crashed = true;
                continue;
            };
            // 探测到发行版即视为已建立 WSL 连接（即使 clangd 缺失/崩溃，
            // 状态栏「连接」项仍应显示 WSL: <distro>）。
            *st.remote.lock().unwrap() = Some(d.clone());
            match probe_clangd(&d).await {
                Ok(path) => {
                    tracing::info!("WSL distro={d}, clangd at {path}");
                }
                Err(e) => {
                    tracing::warn!("clangd probe failed: {e}");
                    set_status(
                        &st,
                        LspStatus::Unavailable(ide_core::diagnostics::tool_missing("clangd", &e)),
                    );
                    just_crashed = true;
                    continue;
                }
            }
            Some(d)
        } else {
            tracing::info!("使用 LITEIDE_LSP_CMD 覆盖启动命令（跳过 WSL 探测）");
            *st.remote.lock().unwrap() = None;
            None
        };

        // ---- 启动 clangd 并握手 ----
        set_status(&st, LspStatus::Connecting);
        let child = match spawn_clangd(distro.as_deref().unwrap_or("mock")) {
            Ok(c) => c,
            Err(e) => {
                set_status(
                    &st,
                    LspStatus::Unavailable(format!("启动 clangd 失败: {e}")),
                );
                just_crashed = true;
                continue;
            }
        };
        let (mut client, mut events, _writer_handle) = match LspClient::connect(child, DEFAULT_TIMEOUT)
        {
            Ok(pair) => pair,
            Err(e) => {
                set_status(
                    &st,
                    LspStatus::Unavailable(format!("接管 clangd stdio 失败: {e}")),
                );
                just_crashed = true;
                continue;
            }
        };

        let Some(root_uri) = st.root_uri.clone() else {
            continue;
        };
        match client.initialize(&root_uri).await {
            Ok(_) => {
                if let Err(e) = client.initialized().await {
                    tracing::warn!("initialized 通知失败: {e}");
                }
            }
            Err(e) => {
                tracing::warn!("initialize 失败: {e}");
                let _ = client.shutdown().await;
                set_status(&st, LspStatus::Crashed(format!("initialize 失败: {e}")));
                just_crashed = true;
                continue;
            }
        }
        set_status(&st, LspStatus::Ready);

        // ---- 就绪循环 ----
        let outcome = ready_loop(&mut st, &mut client, &mut events).await;
        match outcome {
            ReadyOutcome::Crash => {
                just_crashed = true;
                continue; // 保留 Crashed 状态，等用户 Restart
            }
            ReadyOutcome::Restart => {
                just_crashed = false;
                continue; // 立即重连
            }
            ReadyOutcome::Exit => return,
        }
    }
}

async fn ready_loop(
    st: &mut SessionState,
    client: &mut LspClient,
    events: &mut LspEvents,
) -> ReadyOutcome {
    loop {
        tokio::select! {
            cmd = st.cmd_rx.recv() => {
                let Some(cmd) = cmd else { return ReadyOutcome::Exit; };
                match cmd {
                    Command::Restart => {
                        client.shutdown().await;
                        return ReadyOutcome::Restart;
                    }
                    Command::Shutdown => {
                        client.shutdown().await;
                        return ReadyOutcome::Exit;
                    }
                    other => {
                        handle_command(st, client, other).await;
                    }
                }
            }
            ev = events.recv() => {
                match ev {
                    Some(ServerEvent::Exit { code }) => {
                        let detail = match code {
                            Some(c) => format!("clangd 已退出（exit code {c}）"),
                            None => "clangd 连接已断开".to_string(),
                        };
                        set_status(st, LspStatus::Crashed(detail));
                        return ReadyOutcome::Crash;
                    }
                    Some(ServerEvent::LogMessage { message, .. })
                    | Some(ServerEvent::ShowMessage { message, .. }) => {
                        tracing::info!("clangd: {message}");
                        let _ = st.ui_tx.send(Event::ServerLog(message));
                    }
                    Some(ServerEvent::Stderr(line)) => {
                        tracing::debug!("clangd stderr: {line}");
                    }
                    Some(ServerEvent::PublishDiagnostics { uri, items, .. }) => {
                        let _ = st.ui_tx.send(Event::Diagnostics { uri, items });
                    }
                    None => {
                        set_status(st, LspStatus::Crashed("clangd 事件通道关闭".to_string()));
                        return ReadyOutcome::Crash;
                    }
                }
            }
        }
    }
}

async fn handle_command(st: &mut SessionState, client: &mut LspClient, cmd: Command) {
    match cmd {
        Command::Start { .. } => { /* 已在运行 */ }
        Command::Restart | Command::Shutdown => { /* 在 ready_loop 处理 */ }
        Command::Open { uri, text, version } => {
            if let Err(e) = client.did_open(&uri, "cpp", &text, version).await {
                tracing::warn!("didOpen 失败: {e}");
            }
        }
        Command::Change { uri, text, version } => {
            if let Err(e) = client.did_change(&uri, &text, version).await {
                tracing::warn!("didChange 失败: {e}");
            }
        }
        Command::Save { uri } => {
            if let Err(e) = client.did_save(&uri).await {
                tracing::warn!("didSave 失败: {e}");
            }
        }
        Command::Close { uri } => {
            if let Err(e) = client.did_close(&uri).await {
                tracing::warn!("didClose 失败: {e}");
            }
        }
        Command::Definition { uri, pos, seq } => {
            let result = client.definition(&uri, pos).await.map_err(err_string);
            let _ = st.ui_tx.send(Event::Definition { seq, result });
        }
        Command::Hover { uri, pos, seq } => {
            let result = client.hover(&uri, pos).await.map_err(err_string);
            let _ = st.ui_tx.send(Event::Hover { seq, result });
        }
        Command::Symbols { uri, seq } => {
            let result = client.document_symbol(&uri).await.map_err(err_string);
            let _ = st.ui_tx.send(Event::Symbols { seq, result });
        }
    }
}

fn err_string(e: LspError) -> String {
    e.to_string()
}

fn set_status(st: &SessionState, status: LspStatus) {
    tracing::info!("LSP status: {status:?}");
    *st.status.lock().unwrap() = status.clone();
    let _ = st.ui_tx.send(Event::Status(status));
}
