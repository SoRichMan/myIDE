//! 工作区文件索引：给 Ctrl+P 提供「像 VS Code 一样」的模糊搜索。
//!
//! 之前 Ctrl+P 只是个目录浏览器：输入文件名只在**当前目录**里过滤，
//! 工程深处的文件根本找不到（VS Code 能按文件名模糊命中整个工作区）。
//!
//! 这里在后台线程把工作区的文件列出来（本地目录直接遍历，远程工作区用
//! `wsl.exe` / `ssh` 在远端跑 `find`），再做 VS Code 风格的模糊匹配（子序列 + 打分排序）。
//!
//! 索引是**快照**：建好之后一直用；文件变了不会自动更新（VS Code 靠文件监视，
//! 我们还没有），但重新打开 Ctrl+P 时会按根重建一次。

use ide_remote::RemoteTarget;
use std::path::Path;
use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};

/// 搜索结果缓存的上限（查询词很泛时命中可能几万条，只留最相关的前 N 条）。
pub const MAX_CACHED_HITS: usize = 200;

/// 索引里的一条记录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEntry {
    /// 相对工作区根的路径（统一用 `/` 分隔）。
    pub rel: String,
    /// 文件名（含扩展名）。
    pub name: String,
    /// 文件名的**逐字符**小写形式（预处理好，搜索时不用每个条目都 `to_lowercase()`）。
    ///
    /// 逐字符（而不是整串）小写化，保证它和 `name` 的字符数**一一对应**，
    /// 这样匹配时可以把原始字符和小写字符 zip 起来走。
    lname: String,
    /// 目录前缀的逐字符小写形式（`src/utils/` 这种；没有目录就是空串）。
    ldir: String,
}

impl FileEntry {
    fn new(rel: String, name: String) -> Self {
        let ldir = lower_per_char(dir_prefix(&rel));
        Self {
            lname: lower_per_char(&name),
            ldir,
            rel,
            name,
        }
    }
}

/// 逐字符小写化（长度和原串一致）。
fn lower_per_char(s: &str) -> String {
    s.chars()
        .map(|c| c.to_lowercase().next().unwrap_or(c))
        .collect()
}

/// 索引的构建状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IndexState {
    /// 还没开始建。
    Idle,
    /// 后台正在建。
    Building,
    /// 建好了。
    Ready,
    /// 失败（目录不存在 / WSL 命令失败）。
    Failed(String),
}

/// 索引快照与后台构建。
pub struct FileIndex {
    entries: Vec<FileEntry>,
    state: IndexState,
    rx: Receiver<Vec<FileEntry>>,
    tx: Sender<Vec<FileEntry>>,
    /// 当前索引对应的根（换了根要重建）。
    root_key: String,
    building: bool,
    /// 上一次搜索的查询词 + 结果（同一查询词不重复算）。
    last_query: String,
    last_hits: Vec<Hit>,
}

impl Default for FileIndex {
    fn default() -> Self {
        Self::new()
    }
}

impl FileIndex {
    pub fn new() -> Self {
        let (tx, rx) = channel();
        Self {
            entries: Vec::new(),
            state: IndexState::Idle,
            rx,
            tx,
            root_key: String::new(),
            building: false,
            last_query: String::new(),
            last_hits: Vec::new(),
        }
    }

    pub fn state(&self) -> &IndexState {
        &self.state
    }

    /// 确保索引已按当前根建好（没有就在后台线程建）。
    ///
    /// `local_root`：本地工作区根；`target` + `posix_root`：远程（WSL / SSH）工作区。
    pub fn ensure(&mut self, local_root: &Path, target: Option<&RemoteTarget>, posix_root: Option<&str>) {
        let key = match (target, posix_root) {
            (Some(t), Some(r)) => format!("{}:{r}", t.label()),
            _ => format!("local:{}", local_root.display()),
        };
        if key == self.root_key && (self.building || !self.entries.is_empty()) {
            return;
        }
        self.root_key = key.clone();
        self.building = true;
        self.state = IndexState::Building;
        let tx = self.tx.clone();
        let local_root = local_root.to_path_buf();
        let target = target.cloned();
        let posix_root = posix_root.map(str::to_string);
        let _ = std::thread::Builder::new()
            .name("file-index".to_string())
            .spawn(move || {
                let entries = match (&target, &posix_root) {
                    (Some(t), Some(r)) => list_remote(t, r),
                    _ => list_local(&local_root),
                };
                let _ = tx.send(entries);
            });
    }

    /// 每帧调用：收后台结果。
    pub fn poll(&mut self) {
        match self.rx.try_recv() {
            Ok(entries) => {
                self.building = false;
                self.state = IndexState::Ready;
                self.entries = entries;
                // 索引换了 → 之前的搜索结果作废。
                self.last_query.clear();
                self.last_hits.clear();
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                self.building = false;
                if self.state == IndexState::Building {
                    self.state = IndexState::Failed("索引线程已退出".to_string());
                }
            }
        }
    }

    /// 模糊搜索（VS Code 风格）：先按文件名匹配，再按目录部分匹配。
    ///
    /// 返回按分数从高到低排列的条目（分数相同按路径排，保证顺序稳定）。
    /// 每条附带**命中字符的位置**（结果列表里要高亮出来，像 VS Code 那样）。
    ///
    /// `limit` = 最多返回多少条（UI 默认只显示 20 条，点「更多」才放开）。
    ///
    /// 结果按查询词缓存：Picker 是**每帧重绘**的，同一个查询词没必要反复
    /// 扫三万多个条目（那才是打字卡顿的真正原因）。
    pub fn search(&mut self, query: &str, limit: usize) -> Vec<Hit> {
        let q = query.trim();
        if q.is_empty() {
            return Vec::new();
        }
        if self.last_query != q {
            self.last_query = q.to_string();
            self.last_hits = self.search_uncached(q);
        }
        self.last_hits.iter().take(limit).cloned().collect()
    }

    /// 当前查询词一共有多少条命中（UI 判断要不要显示「更多」）。
    pub fn total_matches(&self) -> usize {
        self.last_hits.len()
    }

    /// 真正扫一遍索引（结果缓存在 `last_hits` 里）。
    fn search_uncached(&self, q: &str) -> Vec<Hit> {
        // 查询串只小写化一次（之前每个条目都重新 `to_lowercase()`，三万条时很慢）。
        let lower_query: Vec<char> = q.to_lowercase().chars().collect();
        let mut scored: Vec<Hit> = Vec::new();
        for e in &self.entries {
            // 先按文件名匹配（VS Code 也是文件名优先）。
            if let Some((score, hits)) = match_prepared(&e.name, &e.lname, &lower_query) {
                scored.push(Hit {
                    name: e.name.clone(),
                    rel: e.rel.clone(),
                    score,
                    hits,
                    dir_hits: Vec::new(),
                });
                continue;
            }
            // 文件名没命中，再看目录部分（`src/utils/` 这种）。
            if e.ldir.is_empty() {
                continue;
            }
            let dir = dir_prefix(&e.rel);
            let Some((score, dir_hits)) = match_prepared(dir, &e.ldir, &lower_query) else {
                continue;
            };
            scored.push(Hit {
                name: e.name.clone(),
                rel: e.rel.clone(),
                score,
                hits: Vec::new(),
                dir_hits,
            });
        }
        scored.sort_by(|a, b| {
            b.score
                .cmp(&a.score)
                .then_with(|| a.rel.cmp(&b.rel))
        });
        // 缓存只留前 N 条：查询词很泛时命中几万条，全留着既费内存又没意义
        // （UI 最多也就显示这么多）。
        scored.truncate(MAX_CACHED_HITS);
        scored
    }
}

/// 相对路径里的目录部分（含结尾的 `/`）；没有目录就是空串。
fn dir_prefix(rel: &str) -> &str {
    match rel.rfind('/') {
        Some(p) => &rel[..p + 1],
        None => "",
    }
}

/// 一条搜索命中。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    /// 文件名。
    pub name: String,
    /// 相对工作区根的路径。
    pub rel: String,
    /// 相关度分数。
    pub score: i32,
    /// 命中的字符在文件名里的下标（用来高亮）。
    pub hits: Vec<usize>,
    /// 命中的字符在目录前缀里的下标（文件名没命中、改由目录命中时用）。
    pub dir_hits: Vec<usize>,
}

/// VS Code 风格的模糊匹配：查询串必须是名字的子序列，返回一个分数。
///
/// 打分思路（和 VS Code 的 `quickOpen` 接近）：
/// - 命中在开头 / 分隔符后 / 驼峰边界 → 加分越多；
/// - 中间跳过的字符越多 → 扣分越多；
/// - 名字越短 → 略微加分（同等命中度下短名字更可能是想要的）。
///
/// 只看分数用这个；结果列表要标出命中位置时用 [`fuzzy_match`]。
#[cfg(test)]
pub fn fuzzy_score(name: &str, query: &str) -> Option<i32> {
    fuzzy_match(name, query).map(|(score, _)| score)
}

/// 模糊匹配 + 命中位置（结果列表里要把命中的字符高亮出来）。
///
/// 返回 `(分数, 命中字符在名字里的下标)`。
#[cfg(test)]
pub fn fuzzy_match(name: &str, query: &str) -> Option<(i32, Vec<usize>)> {
    if query.is_empty() {
        return Some((0, Vec::new()));
    }
    let lower_query: Vec<char> = query.to_lowercase().chars().collect();
    match_prepared(name, &lower_per_char(name), &lower_query)
}

/// 用**已经小写化**的名字和查询串做匹配（热路径上不分配内存）。
///
/// `name` 是原始文本（判断驼峰边界要用），`lname` 是**逐字符**小写化后的等长文本。
fn match_prepared(name: &str, lname: &str, lower_query: &[char]) -> Option<(i32, Vec<usize>)> {
    let mut score: i32 = 0;
    let mut at = 0usize;
    let mut hits: Vec<usize> = Vec::new();
    // 两个迭代器同步走：`orig` 用来判断驼峰边界，`low` 用来比较。
    let mut name_chars = name.chars();
    let mut lower_chars = lname.chars();
    // 上一个字符（原始大小写），用来加分。
    let mut prev_orig: Option<char> = None;
    let mut idx = 0usize;
    for qc in lower_query.iter().copied() {
        // 找下一个匹配位置（子序列）。
        let mut found: Option<(usize, char)> = None;
        for (orig, low) in name_chars.by_ref().zip(lower_chars.by_ref()) {
            if low == qc {
                found = Some((idx, orig));
                break;
            }
            prev_orig = Some(orig);
            idx += 1;
        }
        let (hit_idx, cur) = found?;
        let bonus = if hit_idx == 0 {
            16
        } else {
            let prev = prev_orig.unwrap_or(' ');
            if matches!(prev, '/' | '\\' | '_' | '-' | '.' | ' ') {
                12
            } else if cur.is_uppercase() && prev.is_lowercase() {
                10
            } else {
                4
            }
        };
        score += bonus;
        score -= (hit_idx - at) as i32;
        hits.push(hit_idx);
        at = hit_idx + 1;
        prev_orig = Some(cur);
        idx = hit_idx + 1;
    }
    // 名字短一点更相关。
    score += 20 - (name.chars().count() as i32).min(20);
    Some((score, hits))
}

/// 本地工作区：递归收集文件（跳过隐藏目录和构建产物目录）。
fn list_local(root: &Path) -> Vec<FileEntry> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    let mut budget = 60_000usize;
    while let Some(dir) = stack.pop() {
        if budget == 0 {
            break;
        }
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in rd.flatten() {
            if budget == 0 {
                break;
            }
            budget -= 1;
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            if is_dir {
                if skip_dir(&name) {
                    continue;
                }
                stack.push(path);
                continue;
            }
            if let Some(rel) = relative_posix(root, &path) {
                if indexed_ext(&name) {
                    out.push(FileEntry::new(rel, name));
                }
            }
        }
    }
    out.sort_by(|a, b| a.rel.cmp(&b.rel));
    out
}

/// 远程（WSL / SSH）工作区：一次 `find` 拿全部文件。
///
/// 排掉 `-path` 的子目录用单引号写在脚本里（`shell_quote` 之后整段交给
/// `bash -l -c`），不会被远端 shell 提前展开；必须排掉构建目录：这个工程光
/// `build*` 下面就有十几万个 .o 文件。
fn list_remote(target: &RemoteTarget, posix_root: &str) -> Vec<FileEntry> {
    let script = format!(
        "cd {} && find . -type f -not -path '*/.git/*' -not -path '*/build*/*' \
-not -path '*/out/*' -not -path '*/CMakeFiles/*' -not -path '*/node_modules/*'",
        ide_core::remote::shell_quote(posix_root)
    );
    let (program, args) = target.shell_argv(&script);
    let mut cmd = std::process::Command::new(&program);
    cmd.args(&args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // 后台索引不能弹出控制台窗口。
        cmd.creation_flags(0x0800_0000);
    }
    let Ok(output) = cmd.output() else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&output.stdout);
    let mut out = Vec::new();
    for line in text.lines() {
        let rel = line.trim_start_matches("./").replace('\\', "/");
        if rel.is_empty() {
            continue;
        }
        let name = rel.rsplit('/').next().unwrap_or(&rel).to_string();
        // 只留源码 / 配置类文件：这个工程光 find 出来就有五万多个文件
        // （备份目录、构建产物…），全量索引既慢又费内存，Ctrl+P 找的也基本是这些。
        if !indexed_ext(&name) {
            continue;
        }
        out.push(FileEntry::new(rel, name));
    }
    out.sort_by(|a, b| a.rel.cmp(&b.rel));
    out
}

/// 索引里保留的文件后缀（源码 / 配置 / 文档）。
fn indexed_ext(name: &str) -> bool {
    const EXTS: &[&str] = &[
        "c", "cc", "cpp", "cxx", "h", "hh", "hpp", "hxx", "inl", "ipp", "cu", "cuh", "m", "mm",
        "rs", "py", "js", "jsx", "ts", "tsx", "css", "scss", "html", "htm", "xml", "json", "yml",
        "yaml", "toml", "ini", "cfg", "conf", "txt", "md", "cmake", "sh", "ps1", "bat", "cmd",
        "rc", "pro", "pri", "qrc", "ui", "glsl", "hlsl", "metal", "def", "sql", "lua", "gradle",
        "mk", "props", "targets",
    ];
    let lower = name.to_lowercase();
    match lower.rsplit_once('.') {
        Some((_, ext)) => EXTS.contains(&ext),
        None => false,
    }
}

/// 目录名是否跳过（隐藏目录 / 构建产物 / 依赖目录）。
fn skip_dir(name: &str) -> bool {
    if name.starts_with('.') {
        return true;
    }
    if name.starts_with("build") || name.starts_with("out") {
        return true;
    }
    matches!(
        name,
        "node_modules" | "target" | "dist" | "__pycache__" | "CMakeFiles"
    )
}

/// 绝对路径 → 相对根的 POSIX 路径。
fn relative_posix(root: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    let s = rel.to_string_lossy().replace('\\', "/");
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str) -> FileEntry {
        FileEntry::new(format!("src/utils/{name}"), name.to_string())
    }

    /// 真实 SSH 验收：索引脚本在远端跑通（排除 build/.git、只留源码类文件）。
    ///
    /// `cargo test -p ide-ui ssh_file_index_lists_remote_sources -- --ignored --nocapture`
    #[test]
    #[ignore = "需要一台可免密登录的 SSH 主机（LITEIDE_SSH_TEST_HOST/USER）"]
    fn ssh_file_index_lists_remote_sources() {
        let user = std::env::var("LITEIDE_SSH_TEST_USER").unwrap_or_else(|_| "fubin".to_string());
        let host = std::env::var("LITEIDE_SSH_TEST_HOST")
            .unwrap_or_else(|_| "192.168.77.6".to_string());
        let root = std::env::var("LITEIDE_SSH_TEST_ROOT")
            .unwrap_or_else(|_| "/home/fubin/work/candanza".to_string());
        let target = RemoteTarget::ssh(user, host, 22);
        let entries = list_remote(&target, &root);
        assert!(!entries.is_empty(), "远程索引不应为空: {root}");
        assert!(
            entries.iter().any(|e| e.name.ends_with(".cpp") || e.name.ends_with(".h")),
            "应索引到 C/C++ 文件"
        );
        assert!(
            !entries
                .iter()
                .any(|e| e.rel.starts_with("build/") || e.rel.contains("/build/")),
            "构建目录应被排除"
        );
        assert!(
            !entries.iter().any(|e| e.rel.contains("/.git/")),
            ".git 应被排除"
        );
    }

    #[test]
    fn fuzzy_matches_subsequence_case_insensitive() {
        assert!(fuzzy_score("Fragmentation.h", "frag").is_some());
        assert!(fuzzy_score("Fragmentation.h", "FRAGMENT").is_some());
        assert!(fuzzy_score("Fragmentation.h", "xyz").is_none());
        // 顺序不能乱。
        assert!(fuzzy_score("Fragmentation.h", "tfrag").is_none());
    }

    #[test]
    fn fuzzy_prefers_earlier_and_shorter_matches() {
        let a = fuzzy_score("Fragmentation.h", "frag").unwrap_or(0);
        let b = fuzzy_score("MyFragmentationThing.h", "frag").unwrap_or(0);
        assert!(a > b, "开头命中应得分更高：{a} vs {b}");
        let c = fuzzy_score("Frag.h", "frag").unwrap_or(0);
        assert!(c >= a, "更短的名字不应更差：{c} vs {a}");
    }

    #[test]
    fn search_ranks_by_score_and_limits() {
        let mut idx = FileIndex::new();
        idx.entries = vec![
            entry("Fragmentation.h"),
            entry("Fragmentation.cpp"),
            entry("MyFragmentationThing.h"),
            entry("unrelated.cpp"),
        ];
        let hits = idx.search("frag", 10);
        assert_eq!(hits.len(), 3, "只有含 frag 的文件应命中");
        assert!(hits[0].name.starts_with("Fragmentation"));
        let limited = idx.search("frag", 1);
        assert_eq!(limited.len(), 1);
        assert!(idx.search("", 10).is_empty(), "空查询不返回结果");
    }

    #[test]
    fn search_falls_back_to_path_match() {
        let mut idx = FileIndex::new();
        idx.entries = vec![FileEntry::new(
            "src/utils/LogInfo.cpp".to_string(),
            "LogInfo.cpp".to_string(),
        )];
        // 文件名里没有，但路径里有。
        let hits = idx.search("utils", 5);
        assert_eq!(hits.len(), 1);
        // 路径命中：命中位置记在目录前缀上，文件名里没有命中。
        assert!(hits[0].hits.is_empty());
        assert!(!hits[0].dir_hits.is_empty());
    }

    #[test]
    fn name_hit_marks_only_the_matched_characters() {
        let mut idx = FileIndex::new();
        idx.entries = vec![entry("Fragmentation.h")];
        let hits = idx.search("frag", 5);
        assert_eq!(hits.len(), 1);
        // f,r,a,g → 下标 0,1,2,3（大小写不敏感，位置按原名算）。
        assert_eq!(hits[0].hits, vec![0, 1, 2, 3]);
        assert!(hits[0].dir_hits.is_empty());
    }

    #[test]
    fn dir_prefix_splits_at_the_last_slash() {
        assert_eq!(dir_prefix("src/utils/LogInfo.cpp"), "src/utils/");
        assert_eq!(dir_prefix("LogInfo.cpp"), "");
    }
}
