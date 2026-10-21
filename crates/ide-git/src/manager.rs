//! GitManager：UI 线程门面，git 子进程在独立 tokio runtime。
//!
//! 每个事件都带上「仓库根路径」，以便 UI 同时管理多个仓库（VS Code 的
//! 多仓库工作区，每个仓库一个 CHANGES 分组）。

use crate::branch::{parse_branches, Branch};
use crate::log::{parse_commit_detail, parse_log, parse_name_status, CommitDetail, GitCommit};
use crate::porcelain::{parse_porcelain, GitStatus};
use crate::process::run_git;
use std::sync::mpsc::Receiver;
use tokio::sync::mpsc::UnboundedSender;

#[derive(Debug)]
enum Command {
    Refresh { root: String },
    Log { root: String },
    Branches { root: String },
    Commit { root: String, message: String, amend: bool },
    Add { root: String, path: String },
    AddAll { root: String },
    Unstage { root: String, path: String },
    UnstageAll { root: String },
    Restore { root: String, path: String },
    RestoreAll { root: String },
    Diff { root: String, path: String, cached: bool },
    CommitDetail { root: String, hash: String, hover: bool },
    CommitDiff { root: String, hash: String },
    Checkout { root: String, branch: String },
    CreateBranch { root: String, name: String },
    Push { root: String },
    Pull { root: String },
    Fetch { root: String },
    Sync { root: String },
}

/// 异步事件（全部回传仓库根，便于多仓库 UI 分派）。
#[derive(Debug, Clone)]
pub enum Event {
    Status { root: String, status: Box<GitStatus> },
    Log { root: String, commits: Vec<GitCommit> },
    Branches { root: String, branches: Vec<Branch> },
    Diff { root: String, path: String, text: String, cached: bool },
    CommitDetail { root: String, detail: Box<CommitDetail>, hover: bool },
    CommitDiff { root: String, hash: String, text: String },
    /// 一次性操作（commit/add/restore/push…）完成回执。
    Done { ok: bool, msg: String },
}

pub struct GitManager {
    cmd_tx: UnboundedSender<Command>,
    ev_rx: Receiver<Event>,
    rt: Option<tokio::runtime::Runtime>,
}

impl Default for GitManager {
    fn default() -> Self {
        Self::new()
    }
}

impl GitManager {
    pub fn new() -> Self {
        let (cmd_tx, mut cmd_rx) = tokio::sync::mpsc::unbounded_channel::<Command>();
        let (ev_tx, ev_rx) = std::sync::mpsc::channel::<Event>();
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("git-worker")
            .enable_all()
            .build()
            .expect("tokio runtime 启动失败");
        rt.spawn(async move {
            while let Some(cmd) = cmd_rx.recv().await {
                match cmd {
                    Command::Refresh { root } => {
                        let tx = ev_tx.clone();
                        tokio::spawn(async move {
                            let args = ["status", "--porcelain=v2", "--branch", "--untracked-files=all"];
                            match run_git(&root, &args).await {
                                Ok((ok, out)) if ok => {
                                    let st = parse_porcelain(&out);
                                    let _ = tx.send(Event::Status { root, status: Box::new(st) });
                                }
                                Ok((_, out)) => {
                                    // 非仓库或 git 失败：空状态 + 提示（不 panic）。
                                    let _ = tx.send(Event::Status {
                                        root: root.clone(),
                                        status: Box::default(),
                                    });
                                    let _ = tx.send(Event::Done {
                                        ok: false,
                                        msg: format!("git status 失败: {}", out.trim())
                                            .chars()
                                            .take(200)
                                            .collect(),
                                    });
                                }
                                Err(e) => {
                                    let _ = tx.send(Event::Status {
                                        root: root.clone(),
                                        status: Box::default(),
                                    });
                                    let _ = tx.send(Event::Done { ok: false, msg: e });
                                }
                            }
                        });
                    }
                    Command::Log { root } => {
                        let tx = ev_tx.clone();
                        tokio::spawn(async move {
                            let args = [
                                "log",
                                "--decorate",
                                "--pretty=format:%h%x00%H%x00%an%x00%ae%x00%ad%x00%s%x00%P%x00%D",
                                "--date=iso",
                                "-n",
                                "300",
                            ];
                            if let Ok((ok, out)) = run_git(&root, &args).await {
                                if ok {
                                    let commits = parse_log(&out);
                                    let _ = tx.send(Event::Log { root, commits });
                                }
                            }
                        });
                    }
                    Command::Branches { root } => {
                        let tx = ev_tx.clone();
                        tokio::spawn(async move {
                            let args = [
                                "branch",
                                "-a",
                                "--format=%(refname:short)%x00%(objectname:short)%x00%(HEAD)",
                            ];
                            if let Ok((ok, out)) = run_git(&root, &args).await {
                                if ok {
                                    let branches = parse_branches(&out);
                                    let _ = tx.send(Event::Branches { root, branches });
                                }
                            }
                        });
                    }
                    Command::Commit { root, message, amend } => {
                        let tx = ev_tx.clone();
                        tokio::spawn(async move {
                            let args: Vec<String> = if amend {
                                vec!["commit".into(), "--amend".into(), "-m".into(), message.clone()]
                            } else {
                                vec!["commit".into(), "-m".into(), message.clone()]
                            };
                            let refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
                            match run_git(&root, &refs).await {
                                Ok((ok, out)) => {
                                    let msg = if ok {
                                        let last = out
                                            .lines()
                                            .rev()
                                            .find(|l| !l.trim().is_empty())
                                            .unwrap_or("")
                                            .trim()
                                            .to_string();
                                        if last.starts_with("[") {
                                            last
                                        } else {
                                            format!("提交成功: {last}")
                                        }
                                    } else {
                                        format!(
                                            "提交失败: {}",
                                            out.trim().chars().take(300).collect::<String>()
                                        )
                                    };
                                    let _ = tx.send(Event::Done { ok, msg });
                                }
                                Err(e) => {
                                    let _ = tx.send(Event::Done { ok: false, msg: e });
                                }
                            }
                        });
                    }
                    Command::Add { root, path } => {
                        let tx = ev_tx.clone();
                        tokio::spawn(async move {
                            match run_git(&root, &["add", "--", &path]).await {
                                Ok((ok, out)) => {
                                    let _ = tx.send(Event::Done { ok, msg: out.trim().to_string() });
                                }
                                Err(e) => {
                                    let _ = tx.send(Event::Done { ok: false, msg: e });
                                }
                            }
                        });
                    }
                    Command::AddAll { root } => {
                        let tx = ev_tx.clone();
                        tokio::spawn(async move {
                            match run_git(&root, &["add", "-A"]).await {
                                Ok((ok, out)) => {
                                    let _ = tx.send(Event::Done { ok, msg: out.trim().to_string() });
                                }
                                Err(e) => {
                                    let _ = tx.send(Event::Done { ok: false, msg: e });
                                }
                            }
                        });
                    }
                    Command::Unstage { root, path } => {
                        let tx = ev_tx.clone();
                        tokio::spawn(async move {
                            match run_git(&root, &["restore", "--staged", "--", &path]).await {
                                Ok((ok, out)) => {
                                    let _ = tx.send(Event::Done { ok, msg: out.trim().to_string() });
                                }
                                Err(e) => {
                                    let _ = tx.send(Event::Done { ok: false, msg: e });
                                }
                            }
                        });
                    }
                    Command::UnstageAll { root } => {
                        let tx = ev_tx.clone();
                        tokio::spawn(async move {
                            match run_git(&root, &["reset", "HEAD", "--"]).await {
                                Ok((ok, out)) => {
                                    let _ = tx.send(Event::Done { ok, msg: out.trim().to_string() });
                                }
                                Err(e) => {
                                    let _ = tx.send(Event::Done { ok: false, msg: e });
                                }
                            }
                        });
                    }
                    Command::Restore { root, path } => {
                        let tx = ev_tx.clone();
                        tokio::spawn(async move {
                            match run_git(&root, &["checkout", "--", &path]).await {
                                Ok((ok, out)) => {
                                    let _ = tx.send(Event::Done { ok, msg: out.trim().to_string() });
                                }
                                Err(e) => {
                                    let _ = tx.send(Event::Done { ok: false, msg: e });
                                }
                            }
                        });
                    }
                    Command::RestoreAll { root } => {
                        let tx = ev_tx.clone();
                        tokio::spawn(async move {
                            match run_git(&root, &["checkout", "--", "."]).await {
                                Ok((ok, out)) => {
                                    let _ = tx.send(Event::Done { ok, msg: out.trim().to_string() });
                                }
                                Err(e) => {
                                    let _ = tx.send(Event::Done { ok: false, msg: e });
                                }
                            }
                        });
                    }
                    Command::Diff { root, path, cached } => {
                        let tx = ev_tx.clone();
                        tokio::spawn(async move {
                            let args: Vec<&str> = if cached {
                                vec!["diff", "--cached", "--", &path]
                            } else {
                                vec!["diff", "--", &path]
                            };
                            if let Ok((ok, out)) = run_git(&root, &args).await {
                                if ok {
                                    let _ = tx.send(Event::Diff {
                                        root,
                                        path,
                                        text: out,
                                        cached,
                                    });
                                }
                            }
                        });
                    }
                    Command::CommitDetail { root, hash, hover } => {
                        let tx = ev_tx.clone();
                        tokio::spawn(async move {
                            // 一次拿两样：--numstat 给增删行数，--name-status 给状态字母。
                            let args = [
                                "show",
                                "--no-color",
                                "--numstat",
                                "--date=iso",
                                "--format=%H%x00%an%x00%ae%x00%ad%x00%s%x00%b%x1e",
                                &hash,
                            ];
                            let Ok((ok, out)) = run_git(&root, &args).await else {
                                return;
                            };
                            if !ok {
                                return;
                            }
                            let mut detail = parse_commit_detail(&out);
                            let status_args = ["show", "--no-color", "--format=", "--name-status", &hash];
                            if let Ok((true, sout)) = run_git(&root, &status_args).await {
                                for (st, path) in parse_name_status(&sout) {
                                    if let Some(f) =
                                        detail.files.iter_mut().find(|f| f.path == path)
                                    {
                                        f.status = st;
                                    }
                                }
                            }
                            let _ = tx.send(Event::CommitDetail {
                                root,
                                detail: Box::new(detail),
                                hover,
                            });
                        });
                    }
                    Command::CommitDiff { root, hash } => {
                        let tx = ev_tx.clone();
                        tokio::spawn(async move {
                            let args = ["show", "--no-color", "--format=", &hash];
                            if let Ok((ok, out)) = run_git(&root, &args).await {
                                if ok {
                                    let _ = tx.send(Event::CommitDiff { root, hash, text: out });
                                }
                            }
                        });
                    }
                    Command::Checkout { root, branch } => {
                        let tx = ev_tx.clone();
                        tokio::spawn(async move {
                            match run_git(&root, &["checkout", &branch]).await {
                                Ok((ok, out)) => {
                                    let msg = if ok {
                                        format!("已切换到 {branch}")
                                    } else {
                                        format!(
                                            "切换失败: {}",
                                            out.trim().chars().take(200).collect::<String>()
                                        )
                                    };
                                    let _ = tx.send(Event::Done { ok, msg });
                                }
                                Err(e) => {
                                    let _ = tx.send(Event::Done { ok: false, msg: e });
                                }
                            }
                        });
                    }
                    Command::CreateBranch { root, name } => {
                        let tx = ev_tx.clone();
                        tokio::spawn(async move {
                            match run_git(&root, &["checkout", "-b", &name]).await {
                                Ok((ok, out)) => {
                                    let msg = if ok {
                                        format!("已创建分支 {name}")
                                    } else {
                                        format!(
                                            "创建分支失败: {}",
                                            out.trim().chars().take(200).collect::<String>()
                                        )
                                    };
                                    let _ = tx.send(Event::Done { ok, msg });
                                }
                                Err(e) => {
                                    let _ = tx.send(Event::Done { ok: false, msg: e });
                                }
                            }
                        });
                    }
                    Command::Push { root } => {
                        let tx = ev_tx.clone();
                        tokio::spawn(async move {
                            match run_git(&root, &["push"]).await {
                                Ok((ok, out)) => {
                                    let msg = if ok {
                                        format!("已推送: {}", out.trim().lines().last().unwrap_or(""))
                                    } else {
                                        format!(
                                            "推送失败: {}",
                                            out.trim().chars().take(200).collect::<String>()
                                        )
                                    };
                                    let _ = tx.send(Event::Done { ok, msg });
                                }
                                Err(e) => {
                                    let _ = tx.send(Event::Done { ok: false, msg: e });
                                }
                            }
                        });
                    }
                    Command::Pull { root } => {
                        let tx = ev_tx.clone();
                        tokio::spawn(async move {
                            match run_git(&root, &["pull"]).await {
                                Ok((ok, out)) => {
                                    let msg = if ok {
                                        format!("已拉取: {}", out.trim().lines().last().unwrap_or(""))
                                    } else {
                                        format!(
                                            "拉取失败: {}",
                                            out.trim().chars().take(200).collect::<String>()
                                        )
                                    };
                                    let _ = tx.send(Event::Done { ok, msg });
                                }
                                Err(e) => {
                                    let _ = tx.send(Event::Done { ok: false, msg: e });
                                }
                            }
                        });
                    }
                    Command::Fetch { root } => {
                        let tx = ev_tx.clone();
                        tokio::spawn(async move {
                            match run_git(&root, &["fetch", "--all"]).await {
                                Ok((ok, out)) => {
                                    let msg = if ok {
                                        format!("已获取: {}", out.trim().lines().last().unwrap_or(""))
                                    } else {
                                        format!(
                                            "获取失败: {}",
                                            out.trim().chars().take(200).collect::<String>()
                                        )
                                    };
                                    let _ = tx.send(Event::Done { ok, msg });
                                }
                                Err(e) => {
                                    let _ = tx.send(Event::Done { ok: false, msg: e });
                                }
                            }
                        });
                    }
                    Command::Sync { root } => {
                        let tx = ev_tx.clone();
                        tokio::spawn(async move {
                            let pull = run_git(&root, &["pull", "--rebase"]).await;
                            match pull {
                                Ok((ok, out)) if ok => {
                                    let _ = tx.send(Event::Done {
                                        ok: true,
                                        msg: format!("同步完成: {}", out.trim().lines().last().unwrap_or("")),
                                    });
                                }
                                Ok((_, out)) => {
                                    let _ = tx.send(Event::Done {
                                        ok: false,
                                        msg: format!(
                                            "同步失败: {}",
                                            out.trim().chars().take(200).collect::<String>()
                                        ),
                                    });
                                }
                                Err(e) => {
                                    let _ = tx.send(Event::Done { ok: false, msg: e });
                                }
                            }
                        });
                    }
                }
            }
        });
        let _ = ev_tx;
        Self { cmd_tx, ev_rx, rt: Some(rt) }
    }

    pub fn poll(&self) -> Option<Event> {
        self.ev_rx.try_recv().ok()
    }

    pub fn refresh(&self, root: &str) {
        let _ = self.cmd_tx.send(Command::Refresh { root: root.to_string() });
    }

    pub fn load_log(&self, root: &str) {
        let _ = self.cmd_tx.send(Command::Log { root: root.to_string() });
    }

    pub fn load_branches(&self, root: &str) {
        let _ = self.cmd_tx.send(Command::Branches { root: root.to_string() });
    }

    /// 提交（只提交已暂存内容，VS Code 语义）。
    pub fn commit(&self, root: &str, message: &str) {
        self.commit_inner(root, message, false);
    }

    /// 修补上一次提交（`git commit --amend`）。
    pub fn amend(&self, root: &str, message: &str) {
        self.commit_inner(root, message, true);
    }

    fn commit_inner(&self, root: &str, message: &str, amend: bool) {
        let _ = self.cmd_tx.send(Command::Commit {
            root: root.to_string(),
            message: message.to_string(),
            amend,
        });
    }

    pub fn add(&self, root: &str, path: &str) {
        let _ = self.cmd_tx.send(Command::Add { root: root.to_string(), path: path.to_string() });
    }

    /// 暂存全部改动。
    pub fn add_all(&self, root: &str) {
        let _ = self.cmd_tx.send(Command::AddAll { root: root.to_string() });
    }

    pub fn switch_root(&self, root: &str) {
        self.refresh(root);
        self.load_log(root);
        self.load_branches(root);
    }

    pub fn restore(&self, root: &str, path: &str) {
        let _ = self.cmd_tx.send(Command::Restore { root: root.to_string(), path: path.to_string() });
    }

    /// 丢弃全部工作区改动。
    pub fn restore_all(&self, root: &str) {
        let _ = self.cmd_tx.send(Command::RestoreAll { root: root.to_string() });
    }

    /// 取消暂存（`git restore --staged -- <path>`）。
    pub fn unstage(&self, root: &str, path: &str) {
        let _ = self.cmd_tx.send(Command::Unstage { root: root.to_string(), path: path.to_string() });
    }

    /// 取消全部暂存。
    pub fn unstage_all(&self, root: &str) {
        let _ = self.cmd_tx.send(Command::UnstageAll { root: root.to_string() });
    }

    pub fn diff(&self, root: &str, path: &str, cached: bool) {
        let _ = self.cmd_tx.send(Command::Diff {
            root: root.to_string(),
            path: path.to_string(),
            cached,
        });
    }

    /// 读取提交详情（作者 / 正文 / 改动文件）。
    pub fn commit_detail(&self, root: &str, hash: &str) {
        self.commit_detail_inner(root, hash, false);
    }

    /// 读取提交详情（悬停卡片用；与「选中」的详情分开存，互不覆盖）。
    pub fn commit_detail_hover(&self, root: &str, hash: &str) {
        self.commit_detail_inner(root, hash, true);
    }

    fn commit_detail_inner(&self, root: &str, hash: &str, hover: bool) {
        let _ = self.cmd_tx.send(Command::CommitDetail {
            root: root.to_string(),
            hash: hash.to_string(),
            hover,
        });
    }

    /// 读取提交的完整 diff。
    pub fn commit_diff(&self, root: &str, hash: &str) {
        let _ = self.cmd_tx.send(Command::CommitDiff {
            root: root.to_string(),
            hash: hash.to_string(),
        });
    }

    pub fn checkout(&self, root: &str, branch: &str) {
        let _ = self.cmd_tx.send(Command::Checkout {
            root: root.to_string(),
            branch: branch.to_string(),
        });
    }

    pub fn create_branch(&self, root: &str, name: &str) {
        let _ = self.cmd_tx.send(Command::CreateBranch {
            root: root.to_string(),
            name: name.to_string(),
        });
    }

    pub fn push(&self, root: &str) {
        let _ = self.cmd_tx.send(Command::Push { root: root.to_string() });
    }

    pub fn pull(&self, root: &str) {
        let _ = self.cmd_tx.send(Command::Pull { root: root.to_string() });
    }

    pub fn fetch(&self, root: &str) {
        let _ = self.cmd_tx.send(Command::Fetch { root: root.to_string() });
    }

    pub fn sync(&self, root: &str) {
        let _ = self.cmd_tx.send(Command::Sync { root: root.to_string() });
    }
}

impl Drop for GitManager {
    fn drop(&mut self) {
        if let Some(rt) = self.rt.take() {
            rt.shutdown_background();
        }
    }
}
