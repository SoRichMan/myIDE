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
use crate::message::{HoverInfo, Location, Position, SymbolInfo, WorkspaceSymbol};
use crate::process::{pick_wsl_distro, probe_clangd, probe_clangd_remote, spawn_clangd, spawn_clangd_remote};
use crate::DEFAULT_TIMEOUT;
use ide_core::remote::RemoteTarget;
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
    /// 按名字在工作区里找符号（definition 找不到时的兜底）。
    WorkspaceSymbol { query: String, seq: u64 },
    Restart,
    /// 换根（远程目录切换）：重启 clangd 并 `initialize` 到新根。
    Reopen { root_uri: String },
    Shutdown,
}

/// 会话 → UI 事件。
#[derive(Debug)]
pub enum Event {
    Status(LspStatus),
    Definition { seq: u64, result: Result<Vec<Location>, String> },
    Hover { seq: u64, result: Result<Option<HoverInfo>, String> },
    Symbols { seq: u64, result: Result<Vec<SymbolInfo>, String> },
    /// `workspace/symbol` 的结果。
    WorkspaceSymbol {
        seq: u64,
        result: Result<Vec<WorkspaceSymbol>, String>,
    },
    ServerLog(String),
    Diagnostics { uri: String, items: serde_json::Value },
}

/// 对 UI 线程暴露的 LSP 管理器。
pub struct LspManager {
    cmd_tx: mpsc::UnboundedSender<Command>,
    ev_rx: std::sync::mpsc::Receiver<Event>,
    status: Arc<Mutex<LspStatus>>,
    /// 当前连接的目标（`None` = 沿用原来的 WSL 自动探测）。状态栏「连接」项读取。
    remote: Arc<Mutex<Option<String>>>,
    /// SSH 远程目标：设置后 clangd 走 `ssh`，而不是本机 WSL。
    target: Arc<Mutex<Option<RemoteTarget>>>,
    /// 当前 clangd 会话的根目录 URI（切换远程根后要判断是否需要重启）。
    root: Arc<Mutex<Option<String>>>,
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
        let target: Arc<Mutex<Option<RemoteTarget>>> = Arc::new(Mutex::new(None));
        let root = Arc::new(Mutex::new(None));
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("lsp-worker")
            .enable_all()
            .build()
            .expect("tokio runtime 启动失败");
        let status_c = status.clone();
        let remote_c = remote.clone();
        let target_c = target.clone();
        let root_c = root.clone();
        let ev_tx_c = ev_tx.clone();
        rt.spawn(async move {
            session_loop(cmd_rx, ev_tx_c, status_c, remote_c, target_c, root_c).await;
        });
        let _ = ev_tx;
        Self {
            cmd_tx,
            ev_rx,
            status,
            remote,
            target,
            root,
            rt: Some(rt),
            seq: 0,
        }
    }

    /// 当前状态（状态栏直接读）。
    pub fn status(&self) -> LspStatus {
        self.status.lock().unwrap().clone()
    }

    /// 已连接的远端标签（`WSL: Ubuntu2404` / `SSH: user@host`；未连接时为 None）。
    pub fn remote_name(&self) -> Option<String> {
        self.remote.lock().unwrap().clone()
    }

    /// 切换 clangd 的远端目标（`None` = 回到原来的 WSL 自动探测）。
    ///
    /// 已启动过就带当前根重启一次，让新目标立即生效；还没启动时只记住，
    /// 等 `start()` 时直接用。
    pub fn set_remote_target(&mut self, target: Option<RemoteTarget>) {
        *self.target.lock().unwrap() = target;
        if let Some(root_uri) = self.root_uri() {
            let _ = self.cmd_tx.send(Command::Reopen { root_uri });
        }
    }

    /// 当前 clangd 会话的根 URI（尚未启动时为 None）。
    pub fn root_uri(&self) -> Option<String> {
        self.root.lock().unwrap().clone()
    }

    /// 在当前会话上换根（远程目录切换）：根变了就重启 clangd。
    ///
    /// clangd 的根只在 `initialize` 时确定，无法运行时更改，所以走一条 `Reopen`
    /// 命令让会话任务自己重启并换根——不能发 `Shutdown`，那会结束整个会话任务。
    pub fn reopen_root(&mut self, root_uri: String) {
        if self.root_uri().as_deref() == Some(root_uri.as_str()) {
            return;
        }
        let _ = self.cmd_tx.send(Command::Reopen { root_uri });
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

    /// 按名字查工作区符号；结果通过 `Event::WorkspaceSymbol{seq, ..}` 返回。
    pub fn request_workspace_symbol(&mut self, query: String) -> u64 {
        let seq = self.next_seq();
        let _ = self.cmd_tx.send(Command::WorkspaceSymbol { query, seq });
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
    target: Arc<Mutex<Option<RemoteTarget>>>,
    root: Arc<Mutex<Option<String>>>,
    cmd_rx: mpsc::UnboundedReceiver<Command>,
    root_uri: Option<String>,
}

/// 这一轮 clangd 跑在哪里。
enum Transport {
    /// WSL 发行版（`None` 仅出现在 `LITEIDE_LSP_CMD` 覆盖模式）。
    Wsl(Option<String>),
    /// SSH 主机。
    Ssh(RemoteTarget),
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
    target: Arc<Mutex<Option<RemoteTarget>>>,
    root: Arc<Mutex<Option<String>>>,
) {
    let mut st = SessionState {
        ui_tx,
        status,
        remote,
        target,
        root,
        cmd_rx,
        root_uri: None,
    };
    // 崩溃后保留 Crashed 状态显示；新会话才显示 Off。
    let mut just_crashed = false;
    // 主动重启（Restart / Reopen）后直接用当前根重连，不再等新命令。
    let mut restart_now = false;

    loop {
        if !just_crashed {
            set_status(&st, LspStatus::Off);
        }
        // ---- 等待启动/重启命令（主动重启除外）----
        if !restart_now {
            loop {
                match st.cmd_rx.recv().await {
                    Some(Command::Start { root_uri }) | Some(Command::Reopen { root_uri }) => {
                        st.root_uri = Some(root_uri.clone());
                        *st.root.lock().unwrap() = Some(root_uri);
                        break;
                    }
                    Some(Command::Restart) => break,
                    Some(Command::Shutdown) | None => return,
                    _ => { /* 未就绪时忽略文档命令 */ }
                }
            }
        }
        restart_now = false;
        just_crashed = false;

        // ---- 探测环境（LITEIDE_LSP_CMD 覆盖时跳过，测试用） ----
        let wanted = st.target.lock().unwrap().clone();
        let transport = if crate::process::using_custom_command() {
            tracing::info!("使用 LITEIDE_LSP_CMD 覆盖启动命令（跳过远端探测）");
            *st.remote.lock().unwrap() = None;
            Transport::Wsl(None)
        } else if let Some(t) = wanted.filter(|t| t.is_ssh()) {
            set_status(&st, LspStatus::Detecting);
            match probe_clangd_remote(&t).await {
                Ok(path) => tracing::info!("{} clangd at {path}", t.label()),
                Err(e) => {
                    tracing::warn!("clangd probe failed: {e}");
                    set_status(
                        &st,
                        LspStatus::Unavailable(ide_core::diagnostics::tool_missing_on(
                            &t, "clangd", &e,
                        )),
                    );
                    just_crashed = true;
                    continue;
                }
            }
            // 探测成功即视为已建立 SSH 连接（即使 clangd 随后崩溃，
            // 状态栏「连接」项仍应显示 SSH: user@host）。
            *st.remote.lock().unwrap() = Some(t.label());
            Transport::Ssh(t)
        } else {
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
            *st.remote.lock().unwrap() = Some(format!("WSL: {d}"));
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
            Transport::Wsl(Some(d))
        };

        // ---- 启动 clangd 并握手 ----
        set_status(&st, LspStatus::Connecting);
        let spawned = match &transport {
            Transport::Ssh(t) => spawn_clangd_remote(t, st.root_uri.as_deref().unwrap_or("")),
            Transport::Wsl(d) => spawn_clangd(
                d.as_deref().unwrap_or("mock"),
                st.root_uri.as_deref().unwrap_or(""),
            ),
        };
        let child = match spawned {
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
                // 重新用（可能已换过的）根重连；这里不能回到「等命令」的循环，
                // 否则只发了一条 Reopen 的调用方会永远等到 LspStatus::Off。
                just_crashed = false;
                restart_now = true;
                continue;
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
                    Command::Reopen { root_uri } => {
                        client.shutdown().await;
                        st.root_uri = Some(root_uri.clone());
                        *st.root.lock().unwrap() = Some(root_uri);
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
        Command::Start { .. } | Command::Reopen { .. } => { /* 在 ready_loop 处理 */ }
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
        Command::WorkspaceSymbol { query, seq } => {
            let result = client.workspace_symbol(&query).await.map_err(err_string);
            let _ = st.ui_tx.send(Event::WorkspaceSymbol { seq, result });
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
