//! ide-git：git CLI 封装（阶段 4）。
//!
//! - `git status --porcelain=v2 --branch`：分支名 + 文件增删改标记
//! - `git log --pretty=format:...`：最近提交列表
//! - `git diff` / `git diff --cached`：统一 diff 文本
//! - `git add` / `git commit` / `git restore`：基础操作
//!
//! git 在 WSL Ubuntu 内运行（与 clangd/rg 同桥接模式），
//! `LITEIDE_GIT_CMD` 整条覆盖（测试 / 原生 Linux）。

pub mod diff;
pub mod log;
pub mod manager;
pub mod porcelain;
pub mod process;

pub use manager::{Event, GitManager};
pub use porcelain::GitStatus;
