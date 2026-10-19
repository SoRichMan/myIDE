//! 异步 GDB MI 客户端（阶段 3）。
//!
//! 架构（与 ide-lsp 的 LspClient 同构）：
//! - **writer task**：从通道取命令串行写 stdin（避免穿插）。
//! - **reader task**：读 stdout → 逐行解析 MI 记录 → 结果记录按 token 路由到
//!   pending，流记录/异步通知/控制台输出转发到事件通道。
//! - 控制台输出（`~"..."`）在命令等待期间会附着到该命令的响应上（`ptype /o`
//!   的文本需要它），同时也转发事件通道（调试控制台显示原始输出）。
//! - 每个命令带超时；gdb 崩溃/退出时所有 pending 立即失败返回。

use crate::mi::{parse_record, MiRecord, MiValue, StoppedFrame};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::{mpsc, oneshot};

/// 默认命令超时。
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

/// GDB 层错误。
#[derive(Debug, thiserror::Error)]
pub enum GdbError {
    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),
    #[error("请求超时（{0}ms）")]
    Timeout(u64),
    #[error("连接已关闭")]
    Shutdown,
    #[error("MI 解析失败: {0}")]
    Parse(String),
}

/// 一条命令的响应。
#[derive(Debug)]
pub struct MiResponse {
    /// 结果类：`done` / `error` / `running` / `exit` / ...
    pub class: String,
    /// 结果字段。
    pub results: Vec<(String, MiValue)>,
    /// 命令等待期间收到的控制台输出（`ptype /o` 等用）。
    pub console: Vec<String>,
}

impl MiResponse {
    /// `^error` 的 msg 字段。
    pub fn error_msg(&self) -> Option<String> {
        self.results
            .iter()
            .find(|(k, _)| k == "msg")
            .and_then(|(_, v)| v.as_str())
            .map(str::to_string)
    }

    /// 取指定名字的第一个结果值。
    pub fn get(&self, key: &str) -> Option<&MiValue> {
        self.results.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }
}

/// gdb 主动推送的事件（客户端 → 上层）。
#[derive(Debug)]
pub enum GdbEvent {
    /// `*stopped`：程序停止（断点/单步/信号/退出）。
    /// `reason == "exited"` 时 `exit_code` 为程序退出码。
    Stopped { reason: String, frame: Option<StoppedFrame>, exit_code: Option<i64> },
    /// 其他流记录/异步通知（如 `=breakpoint-created`）。
    Async { class: String, results: Vec<(String, MiValue)> },
    /// 控制台输出 `~"..."`。
    Console(String),
    /// 目标输出 `@"..."`。
    Target(String),
    /// 日志输出 `&"..."`。
    Log(String),
    /// 无法归类的原始输出行。
    Output(String),
    /// 子进程退出（正常或崩溃）。
    ProcessExit { code: Option<i32> },
}

/// 响应帧（reader 任务产出的中间形态）。
enum Incoming {
    Response { token: u64, class: String, results: Vec<(String, MiValue)>, console: Vec<String> },
}

/// 共享状态：pending 请求表 + 命令等待期的控制台缓冲 + 存活标志。
struct Shared {
    pending: Mutex<HashMap<u64, oneshot::Sender<Incoming>>>,
    console_buf: Mutex<HashMap<u64, Vec<String>>>,
    alive: AtomicBool,
}

/// 对单条 gdb 连接的客户端。
pub struct GdbClient {
    writer_tx: mpsc::Sender<String>,
    shared: Arc<Shared>,
    kill_tx: mpsc::Sender<()>,
    next_token: u64,
    default_timeout: Duration,
}

/// gdb 事件接收端（与客户端分离，便于 `select!` 借用）。
pub struct GdbEvents {
    rx: mpsc::Receiver<GdbEvent>,
}

impl GdbEvents {
    pub async fn recv(&mut self) -> Option<GdbEvent> {
        self.rx.recv().await
    }
}

impl GdbClient {
    /// 接管子进程 stdio，启动 reader/writer/monitor 任务。
    /// 返回 `(客户端, 事件接收端, writer 任务句柄)`。
    pub fn connect(
        mut child: Child,
        default_timeout: Duration,
    ) -> std::io::Result<(Self, GdbEvents, tokio::task::JoinHandle<()>)> {
        let stdin = child.stdin.take().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::BrokenPipe, "gdb stdin 不可用")
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::BrokenPipe, "gdb stdout 不可用")
        })?;

        let shared = Arc::new(Shared {
            pending: Mutex::new(HashMap::new()),
            console_buf: Mutex::new(HashMap::new()),
            alive: AtomicBool::new(true),
        });

        let (writer_tx, writer_rx) = mpsc::channel::<String>(256);
        let (events_tx, events_rx) = mpsc::channel::<GdbEvent>(256);
        let (kill_tx, mut kill_rx) = mpsc::channel::<()>(1);

        let shared_w = shared.clone();
        let reader = tokio::spawn(reader_task(stdout, shared_w, events_tx.clone()));

        let shared_r = shared.clone();
        let monitor_ev_tx = events_tx.clone();
        // 子进程退出监控；收到 kill 信号时强制终止（Shift+F5 兜底）。
        let mut child = child;
        let monitor = tokio::spawn(async move {
            let status = tokio::select! {
                _ = kill_rx.recv() => {
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                    None
                }
                s = child.wait() => s.ok(),
            };
            shared_r.alive.store(false, Ordering::SeqCst);
            let code = status.and_then(|s| s.code());
            let _ = monitor_ev_tx.send(GdbEvent::ProcessExit { code }).await;
            drop(monitor_ev_tx);
        });

        let writer_task = tokio::spawn(writer_task_impl(stdin, writer_rx));
        let _ = (reader, monitor);

        Ok((
            Self {
                writer_tx,
                shared,
                kill_tx,
                next_token: 0,
                default_timeout,
            },
            GdbEvents { rx: events_rx },
            writer_task,
        ))
    }

    pub fn is_alive(&self) -> bool {
        self.shared.alive.load(Ordering::SeqCst)
    }

    /// 强制终止 gdb 子进程（兜底清理；正常路径是 `-gdb-exit`）。
    pub async fn kill(&self) {
        let _ = self.kill_tx.send(()).await;
    }

    /// 发送一条 MI/CLI 命令并等待响应（带超时）。
    ///
    /// `^error` 属于正常响应（返回 `class == "error"`），由上层决定如何处理；
    /// 超时/断连才是 `Err`。
    pub async fn command(&mut self, cmd: &str) -> Result<MiResponse, GdbError> {
        if !self.is_alive() {
            return Err(GdbError::Shutdown);
        }
        self.next_token = self.next_token.wrapping_add(1);
        if self.next_token == 0 {
            self.next_token = 1;
        }
        let token = self.next_token;

        let (tx, rx) = oneshot::channel();
        self.shared.pending.lock().unwrap().insert(token, tx);
        self.shared.console_buf.lock().unwrap().insert(token, Vec::new());

        let line = format!("{token}{cmd}\n");
        if self.writer_tx.send(line).await.is_err() {
            self.shared.pending.lock().unwrap().remove(&token);
            self.shared.console_buf.lock().unwrap().remove(&token);
            return Err(GdbError::Shutdown);
        }

        let timeout = self.default_timeout;
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(Incoming::Response { class, results, console, .. })) => {
                Ok(MiResponse { class, results, console })
            }
            Ok(Err(_)) => {
                // sender 被丢弃（连接关闭）→ pending 已被 reader 清空
                Err(GdbError::Shutdown)
            }
            Err(_elapsed) => {
                self.shared.pending.lock().unwrap().remove(&token);
                self.shared.console_buf.lock().unwrap().remove(&token);
                Err(GdbError::Timeout(timeout.as_millis() as u64))
            }
        }
    }

    /// 直接发送一行原始输入（不等待响应；用于 `-exec-interrupt` 后的兜底等）。
    pub async fn send_raw(&self, line: &str) -> Result<(), GdbError> {
        if !self.is_alive() {
            return Err(GdbError::Shutdown);
        }
        self.writer_tx
            .send(format!("{line}\n"))
            .await
            .map_err(|_| GdbError::Shutdown)
    }
}

/// writer 任务：串行写命令。
async fn writer_task_impl(mut stdin: ChildStdin, mut rx: mpsc::Receiver<String>) {
    while let Some(line) = rx.recv().await {
        if stdin.write_all(line.as_bytes()).await.is_err() || stdin.flush().await.is_err() {
            break;
        }
    }
}

/// reader 任务：读 stdout → 逐行解析 → 路由。
async fn reader_task(stdout: ChildStdout, shared: Arc<Shared>, events_tx: mpsc::Sender<GdbEvent>) {
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line).await {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                if let Some(rec) = route_line(&line, &shared, &events_tx).await {
                    let Incoming::Response { token, .. } = rec;
                    if let Some(tx) = shared.pending.lock().unwrap().remove(&token) {
                        let console = shared
                            .console_buf
                            .lock()
                            .unwrap()
                            .remove(&token)
                            .unwrap_or_default();
                        let _ = tx.send(with_console(rec, console));
                    }
                }
            }
        }
    }
    // stdout EOF：连接关闭，清空 pending。
    shared.alive.store(false, Ordering::SeqCst);
    let mut pend = shared.pending.lock().unwrap();
    for (_, tx) in pend.drain() {
        let _ = tx.send(Incoming::Response {
            token: 0,
            class: "shutdown".to_string(),
            results: Vec::new(),
            console: Vec::new(),
        });
    }
}

fn with_console(mut rec: Incoming, console: Vec<String>) -> Incoming {
    let Incoming::Response { console: c, .. } = &mut rec;
    *c = console;
    rec
}

/// 解析一行并路由：返回需要交给 pending 的响应帧；事件已直接发送。
async fn route_line(
    line: &str,
    shared: &Shared,
    events_tx: &mpsc::Sender<GdbEvent>,
) -> Option<Incoming> {
    let trimmed = line.trim_end_matches(['\r', '\n']);
    match parse_record(trimmed) {
        MiRecord::Prompt => None,
        MiRecord::Result { token, class, results } => {
            let token = token.unwrap_or(0);
            Some(Incoming::Response { token, class, results, console: Vec::new() })
        }
        MiRecord::Stream { class, results } => {
            if class == "stopped" {
                let reason = results
                    .iter()
                    .find(|(k, _)| k == "reason")
                    .and_then(|(_, v)| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let frame = StoppedFrame::from_results(&results);
                let exit_code = results
                    .iter()
                    .find(|(k, _)| k == "exit-code")
                    .and_then(|(_, v)| v.as_str())
                    .and_then(|s| s.trim().parse().ok());
                let _ = events_tx
                    .send(GdbEvent::Stopped { reason, frame, exit_code })
                    .await;
            } else {
                let _ = events_tx.send(GdbEvent::Async { class, results }).await;
            }
            None
        }
        MiRecord::Async { class, results } => {
            let _ = events_tx.send(GdbEvent::Async { class, results }).await;
            None
        }
        MiRecord::Console(text) => {
            // 附着到当前唯一的在飞命令（命令串行，安全）；锁在块内提前释放
            {
                let pend = shared.pending.lock().unwrap();
                if pend.len() == 1 {
                    let token = *pend.keys().next().unwrap();
                    shared
                        .console_buf
                        .lock()
                        .unwrap()
                        .entry(token)
                        .or_default()
                        .push(text.clone());
                }
            }
            let _ = events_tx.send(GdbEvent::Console(text)).await;
            None
        }
        MiRecord::Target(text) => {
            let _ = events_tx.send(GdbEvent::Target(text)).await;
            None
        }
        MiRecord::Log(text) => {
            let _ = events_tx.send(GdbEvent::Log(text)).await;
            None
        }
        MiRecord::Output(text) => {
            let _ = events_tx.send(GdbEvent::Output(text)).await;
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn response_error_msg_extraction() {
        let resp = MiResponse {
            class: "error".to_string(),
            results: vec![(
                "msg".to_string(),
                MiValue::String("No symbol table is loaded.".to_string()),
            )],
            console: Vec::new(),
        };
        assert_eq!(resp.error_msg().as_deref(), Some("No symbol table is loaded."));
        assert_eq!(resp.get("msg").and_then(|v| v.as_str()), Some("No symbol table is loaded."));
    }

    #[test]
    fn response_no_error_msg() {
        let resp = MiResponse {
            class: "done".to_string(),
            results: vec![],
            console: Vec::new(),
        };
        assert_eq!(resp.error_msg(), None);
    }
}
