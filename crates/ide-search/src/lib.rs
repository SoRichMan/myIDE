//! ide-search：ripgrep 封装（阶段 4）。
//!
//! - `rg --files`：列出项目文件，CPU 端做模糊匹配（Ctrl+P）
//! - `rg --json`：内容搜索，按文件分组、点击跳转（Ctrl+Shift+F）
//!
//! IDE 在 Windows，rg 在 WSL Ubuntu 内运行（`wsl.exe -d <distro> -- rg ...`），
//! 与 clangd/gdb 同一套桥接模式。环境变量 `LITEIDE_RG_CMD` 可整条覆盖
//! （测试 / 原生 Linux 场景）。

pub mod fuzzy;
pub mod manager;
pub mod process;
pub mod ripgrep;

pub use manager::{Event, SearchManager, SearchMatch};
pub use process::SearchOptions;
