//! SearchManager：UI 线程门面，子进程在独立 tokio runtime。

use crate::process::{list_files, search_content, SearchOptions};
use crate::ripgrep::parse_rg_json;
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc::UnboundedSender;

pub use crate::ripgrep::SearchMatch;

#[derive(Debug)]
enum Command {
    LoadFiles { root: String },
    Search { seq: u64, root: String, query: String, opts: SearchOptions },
}

#[derive(Debug, Clone)]
pub enum Event {
    /// `rg --files` 完成（文件清单缓存给 CPU 模糊搜索用）。
    FilesReady { files: Vec<String> },
    FilesError(String),
    /// 内容搜索结果（按到达顺序，UI 自行按文件分组）。
    SearchResults { seq: u64, matches: Vec<SearchMatch>, error: Option<String> },
}

pub struct SearchManager {
    cmd_tx: UnboundedSender<Command>,
    ev_rx: Receiver<Event>,
    /// 缓存的文件清单（Ctrl+P 模糊搜索的数据源）。
    files: Arc<Mutex<Vec<String>>>,
    rt: Option<tokio::runtime::Runtime>,
    seq: u64,
}

impl Default for SearchManager {
    fn default() -> Self {
        Self::new()
    }
}

impl SearchManager {
    pub fn new() -> Self {
        let (cmd_tx, mut cmd_rx) = tokio::sync::mpsc::unbounded_channel::<Command>();
        let (ev_tx, ev_rx) = std::sync::mpsc::channel::<Event>();
        let files = Arc::new(Mutex::new(Vec::new()));
        let files_c = files.clone();
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("search-worker")
            .enable_all()
            .build()
            .expect("tokio runtime 启动失败");
        rt.spawn(async move {
            while let Some(cmd) = cmd_rx.recv().await {
                match cmd {
                    Command::LoadFiles { root } => {
                        let tx = ev_tx.clone();
                        let fc = files_c.clone();
                        tokio::spawn(async move {
                            match list_files(&root).await {
                                Ok(mut files) => {
                                    files.sort();
                                    files.dedup();
                                    if let Ok(mut g) = fc.lock() {
                                        *g = files.clone();
                                    }
                                    let _ = tx.send(Event::FilesReady { files });
                                }
                                Err(e) => {
                                    let _ = tx.send(Event::FilesError(e));
                                }
                            }
                        });
                    }
                    Command::Search { seq, root, query, opts } => {
                        let tx = ev_tx.clone();
                        tokio::spawn(async move {
                            match search_content(&root, &query, &opts).await {
                                Ok(raw) => {
                                    let matches = parse_rg_json(&raw);
                                    let _ = tx.send(Event::SearchResults { seq, matches, error: None });
                                }
                                Err(e) => {
                                    let _ = tx.send(Event::SearchResults { seq, matches: vec![], error: Some(e) });
                                }
                            }
                        });
                    }
                }
            }
        });
        let _ = ev_tx;
        Self { cmd_tx, ev_rx, files, rt: Some(rt), seq: 0 }
    }

    pub fn poll(&self) -> Option<Event> {
        self.ev_rx.try_recv().ok()
    }

    /// 已缓存的文件清单快照（CPU 端模糊过滤用）。
    pub fn cached_files(&self) -> Vec<String> {
        self.files.lock().map(|g| g.clone()).unwrap_or_default()
    }

    /// 请求加载文件清单（Ctrl+P 首次打开时调一次）。
    pub fn load_files(&self, root: &str) {
        let _ = self.cmd_tx.send(Command::LoadFiles { root: root.to_string() });
    }

    /// 发起一次内容搜索，返回 seq（与事件对账）。
    pub fn search(&mut self, root: &str, query: &str, opts: &SearchOptions) -> u64 {
        self.seq += 1;
        let seq = self.seq;
        let _ = self.cmd_tx.send(Command::Search {
            seq,
            root: root.to_string(),
            query: query.to_string(),
            opts: opts.clone(),
        });
        seq
    }
}

impl Drop for SearchManager {
    fn drop(&mut self) {
        // 让 runtime 退出（worker 线程随 channel 关闭自然结束）。
        if let Some(rt) = self.rt.take() {
            rt.shutdown_background();
        }
    }
}
