//! ide-core: 核心抽象、文档模型、主题常量。
pub mod buffer;
pub mod diagnostics;
pub mod highlight;
pub mod theme;

pub use buffer::Buffer;
pub use highlight::{Highlight, HighlightGroup, Highlighter};

/// 领域错误类型（库层统一用 thiserror）。
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid color: {0}")]
    InvalidColor(String),
}

pub type CoreResult<T> = Result<T, CoreError>;
