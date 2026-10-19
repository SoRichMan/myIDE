//! GitManager：UI 线程门面，git 子进程在独立 tokio runtime。

use crate::porcelain::{parse_porcelain, GitStatus};
use crate::process::run_git;
use std::sync::mpsc::Receiver;
use tokio::sync::mpsc::UnboundedSender;

#[derive(Debug)]
enum Command {
    Refresh { root: String },
    Log { root: String },
    Commit { root: String, message: String },
    Add { root: String, path: String },
    Unstage { root: String, path: String },
    Restore { root: String, path: String },
    Diff { root: String, path: String, cached: bool },
}

#[derive(Debug, Clone)]
pub enum Event {
    Status(Box<GitStatus>),
    Log(Vec<crate::log::GitCommit>),
    Diff { path: String, text: String },
    /// 一次性操作（commit/add/restore）完成回执。
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
            .worker_threads(1)
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
                                    let _ = tx.send(Event::Status(Box::new(st)));
                                }
                                Ok((_, out)) => {
                                    // 非仓库或 git 失败：空状态 + 提示（不 panic）。
                                    let _ = tx.send(Event::Status(Box::default()));
                                    let _ = tx.send(Event::Done {
                                        ok: false,
                                        msg: format!("git status 失败: {}", out.trim()).chars().take(200).collect(),
                                    });
                                }
                                Err(e) => {
                                    let _ = tx.send(Event::Status(Box::default()));
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
                                "--pretty=format:%h%x00%an%x00%ad%x00%s",
                                "--date=short",
                                "-n",
                                "100",
                            ];
                            if let Ok((ok, out)) = run_git(&root, &args).await {
                                if ok {
                                    let commits = crate::log::parse_log(&out);
                                    let _ = tx.send(Event::Log(commits));
                                }
                            }
                        });
                    }
                    Command::Commit { root, message } => {
                        let tx = ev_tx.clone();
                        tokio::spawn(async move {
                            // 先 add -A，再 commit。失败回执给 UI。
                            if let Err(e) = run_git(&root, &["add", "-A"]).await {
                                let _ = tx.send(Event::Done { ok: false, msg: e });
                                return;
                            }
                            match run_git(&root, &["commit", "-m", &message]).await {
                                Ok((ok, out)) => {
                                    let msg = if ok {
                                        format!("提交成功: {}", out.lines().last().unwrap_or("").trim())
                                    } else {
                                        format!("提交失败: {}", out.trim().chars().take(200).collect::<String>())
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
                            match run_git(&root, &["add", &path]).await {
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
                                    let _ = tx.send(Event::Diff { path, text: out });
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

    pub fn commit(&self, root: &str, message: &str) {
        let _ = self.cmd_tx.send(Command::Commit { root: root.to_string(), message: message.to_string() });
    }

    pub fn add(&self, root: &str, path: &str) {
        let _ = self.cmd_tx.send(Command::Add { root: root.to_string(), path: path.to_string() });
    }

    pub fn restore(&self, root: &str, path: &str) {
        let _ = self.cmd_tx.send(Command::Restore { root: root.to_string(), path: path.to_string() });
    }

    /// 取消暂存（`git restore --staged -- <path>`）。
    pub fn unstage(&self, root: &str, path: &str) {
        let _ = self.cmd_tx.send(Command::Unstage { root: root.to_string(), path: path.to_string() });
    }

    pub fn diff(&self, root: &str, path: &str, cached: bool) {
        let _ = self.cmd_tx.send(Command::Diff { root: root.to_string(), path: path.to_string(), cached });
    }
}

impl Drop for GitManager {
    fn drop(&mut self) {
        if let Some(rt) = self.rt.take() {
            rt.shutdown_background();
        }
    }
}
