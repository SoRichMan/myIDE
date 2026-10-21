//! ide-git：git CLI 封装（阶段 4）。
//!
//! - `git status --porcelain=v2 --branch`：分支名 + 文件增删改标记
//! - `git log --pretty=format:...`：最近提交列表（含引用标签、父提交）
//! - `git branch -a --format=...`：本地 / 远程分支列表
//! - `git show --numstat`：单个提交的详情与改动文件
//! - `git diff` / `git diff --cached`：统一 diff 文本
//! - `git add` / `git commit` / `git restore` / `git push` / `git pull`：基础操作
//!
//! git 在 WSL Ubuntu 内运行（与 clangd/rg 同桥接模式），
//! `LITEIDE_GIT_CMD` 整条覆盖（测试 / 原生 Linux）。

pub mod branch;
pub mod diff;
pub mod log;
pub mod manager;
pub mod porcelain;
pub mod process;
pub mod repos;

pub use branch::Branch;
pub use log::{ChangedFile, CommitDetail, GitCommit};
pub use manager::{Event, GitManager};
pub use porcelain::{GitEntry, GitStatus};
pub use repos::discover_repos;
