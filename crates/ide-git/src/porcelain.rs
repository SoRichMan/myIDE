//! 解析 `git status --porcelain=v2 --branch`。

/// 单个文件的 git 状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitEntry {
    /// 工作树路径（相对仓库根）。
    pub path: String,
    /// 暂存区状态字符：M/A/D/R/?? 等。
    pub staged: char,
    /// 工作区状态字符：M/D/?? 等。
    pub worktree: char,
}

impl GitEntry {
    /// 给 UI 用的单字符标记（VSCode 风格）。
    pub fn badge(&self) -> char {
        // 优先工作区改动，否则暂存。
        if self.worktree != '.' && self.worktree != ' ' {
            self.worktree
        } else {
            self.staged
        }
    }
}

/// 仓库整体状态。
#[derive(Debug, Clone, Default)]
pub struct GitStatus {
    /// 当前分支名（detached 时为短 SHA）。
    pub branch: Option<String>,
    /// 上游分支（如 `origin/master`）。
    pub upstream: Option<String>,
    /// 领先上游的提交数。
    pub ahead: usize,
    /// 落后上游的提交数。
    pub behind: usize,
    pub entries: Vec<GitEntry>,
}

impl GitStatus {
    pub fn file_state(&self, rel_path: &str) -> Option<char> {
        self.entries
            .iter()
            .find(|e| e.path == rel_path)
            .map(|e| e.badge())
    }

    /// 已暂存（index 有变化）的文件。
    pub fn staged(&self) -> Vec<&GitEntry> {
        self.entries
            .iter()
            .filter(|e| e.staged != '.' && e.staged != ' ' && e.staged != '?')
            .collect()
    }

    /// 工作区里未暂存的改动（不含纯未跟踪文件）。
    pub fn unstaged(&self) -> Vec<&GitEntry> {
        self.entries
            .iter()
            .filter(|e| e.worktree != '.' && e.worktree != ' ')
            .collect()
    }

    /// 未跟踪的新文件。
    pub fn untracked(&self) -> Vec<&GitEntry> {
        self.entries
            .iter()
            .filter(|e| e.staged == '?' || e.worktree == '?')
            .collect()
    }
}

/// 解析 porcelain v2 输出。
pub fn parse_porcelain(output: &str) -> GitStatus {
    let mut st = GitStatus::default();
    for line in output.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("# branch.head ") {
            st.branch = Some(rest.trim().to_string());
            continue;
        }
        if let Some(rest) = line.strip_prefix("# branch.upstream ") {
            st.upstream = Some(rest.trim().to_string());
            continue;
        }
        if let Some(rest) = line.strip_prefix("# branch.ab ") {
            // 形如 `+0 -0`。
            for token in rest.split_whitespace() {
                let mut chars = token.chars();
                match (chars.next(), chars.as_str().parse::<usize>()) {
                    (Some('+'), Ok(n)) => st.ahead = n,
                    (Some('-'), Ok(n)) => st.behind = n,
                    _ => {}
                }
            }
            continue;
        }
        if line.starts_with("# branch.") {
            continue;
        }
        let mut fields = line.split_whitespace();
        let Some(kind) = fields.next() else { continue };
        match kind {
            "1" | "u" | "!" => {
                let Some(xy) = fields.next() else { continue };
                // 路径是最后一个字段。
                let path = fields.next_back().unwrap_or("").to_string();
                push_entry(&mut st, xy, path);
            }
            "?" => {
                // porcelain v2 未跟踪文件可能是短格式 `? <path>`（仅两个字段），
                // 也可能是完整格式 `? XY sub ... path`。
                let rest: Vec<&str> = fields.collect();
                if rest.len() == 1 {
                    st.entries.push(GitEntry {
                        path: rest[0].to_string(),
                        staged: '?',
                        worktree: '?',
                    });
                } else if let Some(path) = rest.last() {
                    let xy = rest.first().copied().unwrap_or("??");
                    push_entry(&mut st, xy, path.to_string());
                }
            }
            "2" => {
                // rename/copy：路径在最后。
                let xy = fields.next().unwrap_or("??");
                let path = fields.next_back().unwrap_or("").to_string();
                push_entry(&mut st, xy, path);
            }
            _ => {}
        }
    }
    st
}

fn push_entry(st: &mut GitStatus, xy: &str, path: String) {
    if xy.is_empty() {
        return;
    }
    let mut ch = xy.chars();
    let staged = ch.next().unwrap_or('?');
    let worktree = ch.next().unwrap_or('?');
    st.entries.push(GitEntry { path, staged, worktree });
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
# branch.oid a1b2c3d4e5f6
# branch.head main
# branch.upstream origin/main
# branch.ab +0 -0
1 M. N... 100644 100644 100644 abc123 def456 src/main.rs
1 MM N... 100644 100644 100644 abc123 def456 src/util.rs
1 A. N... 100644 100644 100644 000000 abc789 newfile.rs
?  N... 000000 000000 000000 untracked.txt
1 D. N... 100644 100644 100644 deleted123 000000 old.rs
";

    #[test]
    fn parses_branch_and_entries() {
        let st = parse_porcelain(SAMPLE);
        assert_eq!(st.branch.as_deref(), Some("main"));
        assert_eq!(st.entries.len(), 5);
        assert_eq!(st.entries[0].path, "src/main.rs");
        assert_eq!(st.entries[0].staged, 'M');
        assert_eq!(st.entries[0].worktree, '.');
        assert_eq!(st.entries[1].worktree, 'M');
    }

    #[test]
    fn badge_prefers_worktree() {
        let e = GitEntry { path: "a".into(), staged: 'M', worktree: 'D' };
        assert_eq!(e.badge(), 'D');
        let e2 = GitEntry { path: "a".into(), staged: 'M', worktree: '.' };
        assert_eq!(e2.badge(), 'M');
    }

    #[test]
    fn file_state_lookup() {
        let st = parse_porcelain(SAMPLE);
        assert_eq!(st.file_state("src/main.rs"), Some('M'));
        assert_eq!(st.file_state("untracked.txt"), Some('N'));
        assert_eq!(st.file_state("missing.rs"), None);
    }

    #[test]
    fn empty_output() {
        let st = parse_porcelain("");
        assert!(st.branch.is_none());
        assert!(st.entries.is_empty());
    }

    #[test]
    fn untracked_xy_is_question() {
        let st = parse_porcelain("?  N... 000000 000000 000000 foo.txt\n");
        assert_eq!(st.entries[0].path, "foo.txt");
        assert_eq!(st.entries[0].staged, 'N');
        assert_eq!(st.entries[0].worktree, '.');
    }
}
