//! ide-lsp：LSP JSON-RPC 客户端与 clangd 集成（阶段 2）。
//!
//! 分层：
//! - [`codec`]：Content-Length 帧编解码（纯逻辑，可单测）
//! - [`message`]：JSON-RPC 信封 + 最小 LSP 类型与宽容解析
//! - [`path_map`]：Windows ↔ WSL ↔ `file://` URI 转换
//! - [`process`]：WSL 发行版探测 / clangd 启动
//! - [`client`]：异步 LSP 客户端（生命周期、请求/通知、超时）
//! - [`manager`]：UI 门面（状态机、崩溃重启，UI 线程不阻塞）

pub mod client;
pub mod codec;
pub mod manager;
pub mod message;
pub mod path_map;
pub mod process;

pub use client::{LspClient, LspError, LspEvents, ServerEvent, DEFAULT_TIMEOUT};
pub use manager::{Command, Event, LspManager, LspStatus};
pub use message::{HoverInfo, Location, Position, Range, SymbolInfo};
