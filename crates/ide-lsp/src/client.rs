//! 异步 LSP 客户端：连接 clangd 子进程，实现请求/通知收发与生命周期。
//!
//! 架构：
//! - 一个 **writer task**：从通道取帧写 stdin（避免请求互相穿插）。
//! - 一个 **reader task**：读 stdout → 增量解码 → 响应按 id 路由到 pending，
//!   通知（logMessage / publishDiagnostics 等）转发到事件通道。
//! - 每个请求带超时；clangd 崩溃/退出时所有 pending 立即失败返回。
//!
//! `LspClient` 本身只处理单条连接；崩溃后的重启策略在 `manager` 层。

use crate::codec::{encode, FrameDecoder};
use crate::message::{
    parse_hover, parse_locations, parse_symbols, parse_workspace_symbols, HoverInfo, Location,
    Position, SymbolInfo, WorkspaceSymbol,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::{mpsc, oneshot};

/// 默认请求超时。
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);
/// initialize 超时（clangd 冷启动 + WSL 桥接较慢）。
pub const INIT_TIMEOUT: Duration = Duration::from_secs(30);
/// `workspace/symbol` 超时（要查服务端索引，比普通请求慢）。
pub const WORKSPACE_SYMBOL_TIMEOUT: Duration = Duration::from_secs(15);

/// LSP 层错误。
#[derive(Debug, thiserror::Error)]
pub enum LspError {
    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),
    #[error("帧编解码错误: {0}")]
    Codec(String),
    #[error("JSON 序列化错误: {0}")]
    Json(#[from] serde_json::Error),
    #[error("请求超时（{0}ms）")]
    Timeout(u64),
    #[error("服务端错误 {code}: {message}")]
    Server { code: i64, message: String },
    #[error("连接已关闭")]
    Shutdown,
}

impl From<crate::codec::CodecError> for LspError {
    fn from(e: crate::codec::CodecError) -> Self {
        LspError::Codec(e.to_string())
    }
}

/// 服务端主动推送的事件（客户端 → 上层）。
#[derive(Debug)]
pub enum ServerEvent {
    LogMessage { kind: i32, message: String },
    ShowMessage { kind: i32, message: String },
    PublishDiagnostics { uri: String, version: Option<i32>, items: Value },
    /// stderr 输出（调试用）。
    Stderr(String),
    /// 子进程退出（正常或崩溃）。
    Exit { code: Option<i32> },
}

/// 响应帧（reader 任务产出的中间形态）。
enum Incoming {
    Response { id: u64, result: Result<Value, LspError> },
    Notification(String, Value),
}

/// 共享状态：pending 请求表 + 存活标志。
struct Shared {
    pending: Mutex<HashMap<u64, oneshot::Sender<Result<Value, LspError>>>>,
    alive: AtomicBool,
}

/// 对单条 clangd 连接的客户端。
pub struct LspClient {
    writer_tx: mpsc::Sender<Vec<u8>>,
    shared: Arc<Shared>,
    next_id: u64,
    default_timeout: Duration,
}

/// 服务端通知事件接收端（与客户端分离，便于 `select!` 借用）。
pub struct LspEvents {
    rx: mpsc::Receiver<ServerEvent>,
}

impl LspEvents {
    pub async fn recv(&mut self) -> Option<ServerEvent> {
        self.rx.recv().await
    }
}

impl LspClient {
    /// 接管子进程的 stdio，启动 reader/writer 任务。
    /// 返回 `(客户端, 事件接收端, writer 任务句柄)`。
    pub fn connect(
        mut child: Child,
        default_timeout: Duration,
    ) -> std::io::Result<(Self, LspEvents, tokio::task::JoinHandle<()>)> {
        let stdin = child.stdin.take().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::BrokenPipe, "clangd stdin 不可用")
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::BrokenPipe, "clangd stdout 不可用")
        })?;

        let shared = Arc::new(Shared {
            pending: Mutex::new(HashMap::new()),
            alive: AtomicBool::new(true),
        });

        let (writer_tx, writer_rx) = mpsc::channel::<Vec<u8>>(256);
        let (events_tx, events_rx) = mpsc::channel::<ServerEvent>(256);

        // stderr → 事件（单独任务，防阻塞）
        let stderr = child.stderr.take();
        let ev_tx = events_tx.clone();
        let stderr_task = tokio::spawn(async move {
            let Some(mut stderr) = stderr else { return };
            let mut buf = [0u8; 4096];
            loop {
                match stderr.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let text = String::from_utf8_lossy(&buf[..n]).trim().to_string();
                        if !text.is_empty() {
                            let _ = ev_tx.send(ServerEvent::Stderr(text)).await;
                        }
                    }
                }
            }
        });
        drop(stderr_task);

        let shared_w = shared.clone();
        let reader = tokio::spawn(reader_task(stdout, shared_w, events_tx.clone()));

        let shared_r = shared.clone();
        let monitor_ev_tx = events_tx.clone();
        let monitor = tokio::spawn(async move {
            let status = child.wait().await;
            shared_r.alive.store(false, Ordering::SeqCst);
            let code = status.ok().and_then(|s| s.code());
            let _ = monitor_ev_tx.send(ServerEvent::Exit { code }).await;
            drop(monitor_ev_tx);
        });

        let writer_task = tokio::spawn(writer_task_impl(stdin, writer_rx));
        let _ = (reader, monitor);

        Ok((
            Self {
                writer_tx,
                shared,
                next_id: 0,
                default_timeout,
            },
            LspEvents { rx: events_rx },
            writer_task,
        ))
    }

    pub fn is_alive(&self) -> bool {
        self.shared.alive.load(Ordering::SeqCst)
    }

    /// 发送一条通知（不等待响应）。
    pub async fn notify(&self, method: &str, params: Value) -> Result<(), LspError> {
        if !self.is_alive() {
            return Err(LspError::Shutdown);
        }
        let body = serde_json::to_string(&json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        }))?;
        self.send_frame(&body).await
    }

    /// 发送一条请求并等待响应（带超时）。
    pub async fn request(
        &mut self,
        method: &str,
        params: Value,
        timeout: Option<Duration>,
    ) -> Result<Value, LspError> {
        if !self.is_alive() {
            return Err(LspError::Shutdown);
        }
        self.next_id = self.next_id.wrapping_add(1);
        if self.next_id == 0 {
            self.next_id = 1;
        }
        let id = self.next_id;
        let (tx, rx) = oneshot::channel();
        self.shared.pending.lock().unwrap().insert(id, tx);

        let body = serde_json::to_string(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }))?;
        if let Err(e) = self.send_frame(&body).await {
            self.shared.pending.lock().unwrap().remove(&id);
            return Err(e);
        }

        let timeout = timeout.unwrap_or(self.default_timeout);
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(Ok(v))) => Ok(v),
            Ok(Ok(Err(e))) => Err(e),
            Ok(Err(_)) => {
                // sender 被丢弃（连接关闭）→ pending 已被 reader 清空
                Err(LspError::Shutdown)
            }
            Err(_elapsed) => {
                self.shared.pending.lock().unwrap().remove(&id);
                Err(LspError::Timeout(timeout.as_millis() as u64))
            }
        }
    }

    async fn send_frame(&self, body: &str) -> Result<(), LspError> {
        let frame = encode(body);
        self.writer_tx
            .send(frame)
            .await
            .map_err(|_| LspError::Shutdown)
    }

    // -----------------------------------------------------------------------
    // 生命周期
    // -----------------------------------------------------------------------

    /// `initialize` 请求。
    pub async fn initialize(&mut self, root_uri: &str) -> Result<Value, LspError> {
        let params = json!({
            "processId": null,
            "clientInfo": {"name": "LiteIDE", "version": env!("CARGO_PKG_VERSION")},
            "rootUri": root_uri,
            "capabilities": {
                "textDocument": {
                    "hover": {"contentFormat": ["markdown", "plaintext"]},
                    "definition": {},
                    "documentSymbol": {"hierarchicalDocumentSymbolSupport": true}
                },
                "workspace": {"workspaceFolders": false, "symbol": {}}
            }
        });
        self.request("initialize", params, Some(INIT_TIMEOUT)).await
    }

    /// `initialized` 通知。
    pub async fn initialized(&self) -> Result<(), LspError> {
        self.notify("initialized", json!({})).await
    }

    /// `textDocument/didOpen`。
    pub async fn did_open(
        &self,
        uri: &str,
        language_id: &str,
        text: &str,
        version: i32,
    ) -> Result<(), LspError> {
        self.notify(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": language_id,
                    "version": version,
                    "text": text
                }
            }),
        )
        .await
    }

    /// `textDocument/didChange`（全量同步：每次发送整个文档）。
    pub async fn did_change(&self, uri: &str, text: &str, version: i32) -> Result<(), LspError> {
        self.notify(
            "textDocument/didChange",
            json!({
                "textDocument": {"uri": uri, "version": version},
                "contentChanges": [{"text": text}]
            }),
        )
        .await
    }

    /// `textDocument/didSave`。
    pub async fn did_save(&self, uri: &str) -> Result<(), LspError> {
        self.notify(
            "textDocument/didSave",
            json!({"textDocument": {"uri": uri}}),
        )
        .await
    }

    /// `textDocument/didClose`。
    pub async fn did_close(&self, uri: &str) -> Result<(), LspError> {
        self.notify(
            "textDocument/didClose",
            json!({"textDocument": {"uri": uri}}),
        )
        .await
    }

    // -----------------------------------------------------------------------
    // 语义请求
    // -----------------------------------------------------------------------

    async fn goto(
        &mut self,
        method: &str,
        uri: &str,
        pos: Position,
    ) -> Result<Vec<Location>, LspError> {
        let result = self
            .request(
                method,
                json!({
                    "textDocument": {"uri": uri},
                    "position": pos
                }),
                None,
            )
            .await?;
        Ok(parse_locations(&result))
    }

    /// `textDocument/definition`。
    pub async fn definition(&mut self, uri: &str, pos: Position) -> Result<Vec<Location>, LspError> {
        self.goto("textDocument/definition", uri, pos).await
    }

    /// `textDocument/declaration`。
    pub async fn declaration(&mut self, uri: &str, pos: Position) -> Result<Vec<Location>, LspError> {
        self.goto("textDocument/declaration", uri, pos).await
    }

    /// `textDocument/typeDefinition`。
    pub async fn type_definition(
        &mut self,
        uri: &str,
        pos: Position,
    ) -> Result<Vec<Location>, LspError> {
        self.goto("textDocument/typeDefinition", uri, pos).await
    }

    /// `textDocument/implementation`。
    pub async fn implementation(
        &mut self,
        uri: &str,
        pos: Position,
    ) -> Result<Vec<Location>, LspError> {
        self.goto("textDocument/implementation", uri, pos).await
    }

    /// `textDocument/hover`。
    pub async fn hover(&mut self, uri: &str, pos: Position) -> Result<Option<HoverInfo>, LspError> {
        let result = self
            .request(
                "textDocument/hover",
                json!({
                    "textDocument": {"uri": uri},
                    "position": pos
                }),
                None,
            )
            .await?;
        Ok(parse_hover(&result))
    }

    /// `textDocument/documentSymbol`（大纲）。
    pub async fn document_symbol(&mut self, uri: &str) -> Result<Vec<SymbolInfo>, LspError> {
        let result = self
            .request(
                "textDocument/documentSymbol",
                json!({"textDocument": {"uri": uri}}),
                None,
            )
            .await?;
        Ok(parse_symbols(&result))
    }

    /// `workspace/symbol`（按名字在工作区里找符号）。
    ///
    /// `textDocument/definition` 找不到时（clangd 还没索引完、符号在
    /// compile_commands 之外的文件里……）用它兜底。
    pub async fn workspace_symbol(
        &mut self,
        query: &str,
    ) -> Result<Vec<WorkspaceSymbol>, LspError> {
        let result = self
            .request(
                "workspace/symbol",
                json!({"query": query}),
                Some(WORKSPACE_SYMBOL_TIMEOUT),
            )
            .await?;
        Ok(parse_workspace_symbols(&result))
    }

    /// 优雅关闭：`shutdown` 请求 + `exit` 通知。失败也不报错（尽力而为）。
    pub async fn shutdown(&mut self) {
        if !self.is_alive() {
            return;
        }
        let _ = self
            .request("shutdown", Value::Null, Some(Duration::from_secs(5)))
            .await;
        let _ = self.notify("exit", json!({})).await;
    }
}

/// writer 任务：串行写帧。
async fn writer_task_impl(mut stdin: ChildStdin, mut rx: mpsc::Receiver<Vec<u8>>) {
    while let Some(frame) = rx.recv().await {
        if stdin.write_all(&frame).await.is_err() || stdin.flush().await.is_err() {
            break;
        }
    }
}

/// reader 任务：读 stdout → 解码 → 路由。
async fn reader_task(stdout: ChildStdout, shared: Arc<Shared>, events_tx: mpsc::Sender<ServerEvent>) {
    let mut reader = BufReader::new(stdout);
    let mut decoder = FrameDecoder::new();
    let mut buf = [0u8; 8192];
    loop {
        match reader.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => match decoder.push(&buf[..n]) {
                Ok(frames) => {
                    for frame in frames {
                        match parse_incoming(&frame) {
                            Ok(Incoming::Response { id, result }) => {
                                if let Some(tx) = shared.pending.lock().unwrap().remove(&id) {
                                    let _ = tx.send(result);
                                }
                            }
                            Ok(Incoming::Notification(method, params)) => {
                                forward_notification(&method, &params, &events_tx).await;
                            }
                            Err(e) => {
                                let _ = events_tx
                                    .send(ServerEvent::Stderr(format!("bad frame: {e}")))
                                    .await;
                            }
                        }
                    }
                }
                Err(e) => {
                    let _ = events_tx.send(ServerEvent::Stderr(format!("codec: {e}"))).await;
                }
            },
        }
    }
    // stdout EOF：连接关闭，清空 pending。
    shared.alive.store(false, Ordering::SeqCst);
    let mut pend = shared.pending.lock().unwrap();
    for (_, tx) in pend.drain() {
        let _ = tx.send(Err(LspError::Shutdown));
    }
}

/// 解析一帧 JSON：响应 or 通知。
fn parse_incoming(frame: &str) -> Result<Incoming, serde_json::Error> {
    let v: Value = serde_json::from_str(frame)?;
    if let Some(id_val) = v.get("id") {
        if let Some(id) = id_val.as_u64() {
            // 响应帧：有 result 或 error
            let result = if let Some(err) = v.get("error") {
                Err(LspError::Server {
                    code: err.get("code").and_then(|c| c.as_i64()).unwrap_or(-1),
                    message: err
                        .get("message")
                        .and_then(|m| m.as_str())
                        .unwrap_or("unknown error")
                        .to_string(),
                })
            } else {
                Ok(v.get("result").cloned().unwrap_or(Value::Null))
            };
            return Ok(Incoming::Response { id, result });
        }
    }
    let method = v
        .get("method")
        .and_then(|m| m.as_str())
        .unwrap_or("")
        .to_string();
    let params = v.get("params").cloned().unwrap_or(Value::Null);
    Ok(Incoming::Notification(method, params))
}

async fn forward_notification(method: &str, params: &Value, events_tx: &mpsc::Sender<ServerEvent>) {
    let event = match method {
        "window/logMessage" => Some(ServerEvent::LogMessage {
            kind: params.get("type").and_then(|t| t.as_i64()).unwrap_or(0) as i32,
            message: params
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("")
                .to_string(),
        }),
        "window/showMessage" => Some(ServerEvent::ShowMessage {
            kind: params.get("type").and_then(|t| t.as_i64()).unwrap_or(0) as i32,
            message: params
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("")
                .to_string(),
        }),
        "textDocument/publishDiagnostics" => Some(ServerEvent::PublishDiagnostics {
            uri: params
                .get("uri")
                .and_then(|u| u.as_str())
                .unwrap_or("")
                .to_string(),
            version: params.get("version").and_then(|v| v.as_i64()).map(|v| v as i32),
            items: params.get("diagnostics").cloned().unwrap_or(Value::Null),
        }),
        _ => None,
    };
    if let Some(ev) = event {
        let _ = events_tx.send(ev).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_response_frame() {
        let frame = r#"{"jsonrpc":"2.0","id":3,"result":{"capabilities":{}}}"#;
        match parse_incoming(frame).unwrap() {
            Incoming::Response { id, result } => {
                assert_eq!(id, 3);
                assert!(result.unwrap().get("capabilities").is_some());
            }
            _ => panic!("should be response"),
        }
    }

    #[test]
    fn parse_error_frame() {
        let frame = r#"{"jsonrpc":"2.0","id":9,"error":{"code":-32601,"message":"method not found"}}"#;
        match parse_incoming(frame).unwrap() {
            Incoming::Response { id, result } => {
                assert_eq!(id, 9);
                match result {
                    Err(LspError::Server { code, message }) => {
                        assert_eq!(code, -32601);
                        assert_eq!(message, "method not found");
                    }
                    _ => panic!("should be server error"),
                }
            }
            _ => panic!("should be response"),
        }
    }

    #[test]
    fn parse_notification_frame() {
        let frame = r#"{"jsonrpc":"2.0","method":"window/logMessage","params":{"type":3,"message":"indexing"}}"#;
        match parse_incoming(frame).unwrap() {
            Incoming::Notification(method, params) => {
                assert_eq!(method, "window/logMessage");
                assert_eq!(params["message"], "indexing");
            }
            _ => panic!("should be notification"),
        }
    }
}
