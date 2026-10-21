//! 解析 `git log --pretty=format` 输出（含引用标签、父提交）。
//!
//! 格式：`%h%x00%H%x00%an%x00%ae%x00%ad%x00%s%x00%P%x00%D`，
//! 字段之间用 NUL (`%x00`) 分隔，最后一个字段是引用标签（`HEAD -> master` 等）。

/// 一条提交记录。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GitCommit {
    /// 短 SHA（7~8 位）。
    pub hash: String,
    /// 完整 SHA。
    pub full: String,
    /// 作者名。
    pub author: String,
    /// 作者邮箱。
    pub email: String,
    /// 提交时间（ISO 8601）。
    pub date: String,
    /// 标题行。
    pub subject: String,
    /// 父提交 SHA 列表（首个为第一父）。
    pub parents: Vec<String>,
    /// 引用标签（`HEAD`、`master`、`origin/main` …）。
    pub refs: Vec<String>,
}

impl GitCommit {
    /// 引用标签里是否含当前 HEAD（`HEAD -> master`）。
    pub fn is_head(&self) -> bool {
        self.refs.iter().any(|r| r.contains("HEAD"))
    }
}

/// 解析 `git log --pretty=format:...` 的输出。
///
/// 兼容旧格式（4 字段：`hash/author/date/subject`），字段不足时留空。
pub fn parse_log(output: &str) -> Vec<GitCommit> {
    output
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| {
            let parts: Vec<&str> = l.split('\u{0}').collect();
            if parts.len() >= 8 {
                // 新格式：hash / full / author / email / date / subject / parents / refs
                let refs = parse_refs(parts[7]);
                GitCommit {
                    hash: parts[0].to_string(),
                    full: parts[1].to_string(),
                    author: parts[2].to_string(),
                    email: parts[3].to_string(),
                    date: parts[4].to_string(),
                    subject: parts[5].to_string(),
                    parents: parts[6]
                        .split_whitespace()
                        .filter(|p| !p.is_empty())
                        .map(|p| p.to_string())
                        .collect(),
                    refs,
                }
            } else {
                // 旧格式：hash / author / date / subject
                GitCommit {
                    hash: parts.first().copied().unwrap_or("").to_string(),
                    author: parts.get(1).copied().unwrap_or("").to_string(),
                    date: parts.get(2).copied().unwrap_or("").to_string(),
                    subject: parts.get(3).copied().unwrap_or("").to_string(),
                    ..Default::default()
                }
            }
        })
        .collect()
}

/// 拆分 `--decorate` 的引用串：`"HEAD -> master, origin/main"` → `["HEAD -> master", "origin/main"]`。
pub fn parse_refs(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect()
}

/// 一个提交里改动的文件（来自 `git show --numstat`）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChangedFile {
    /// 仓库内相对路径。
    pub path: String,
    /// 新增行数。
    pub insertions: usize,
    /// 删除行数。
    pub deletions: usize,
}

/// 提交详情（作者 / 时间 / 标题 / 正文 / 改动文件）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommitDetail {
    pub hash: String,
    pub author: String,
    pub email: String,
    pub date: String,
    pub subject: String,
    pub body: String,
    pub files: Vec<ChangedFile>,
}

impl CommitDetail {
    /// 总新增行数。
    pub fn insertions(&self) -> usize {
        self.files.iter().map(|f| f.insertions).sum()
    }

    /// 总删除行数。
    pub fn deletions(&self) -> usize {
        self.files.iter().map(|f| f.deletions).sum()
    }

    /// "1 file changed, 70 insertions(+), 61 deletions(-)" 风格摘要。
    pub fn summary(&self) -> String {
        let n = self.files.len();
        let file_word = if n == 1 { "file" } else { "files" };
        let mut s = format!("{n} {file_word} changed");
        let ins = self.insertions();
        let del = self.deletions();
        if ins > 0 {
            s.push_str(&format!(", {ins} insertions(+)"));
        }
        if del > 0 {
            s.push_str(&format!(", {del} deletions(-)"));
        }
        s
    }
}

/// 解析 `git show --numstat --format=...` 的输出。
///
/// 头部用 `\x1e`（RS）结尾，其后是 numstat 行：`<ins>\t<del>\t<path>`。
pub fn parse_commit_detail(output: &str) -> CommitDetail {
    let mut detail = CommitDetail::default();
    let (head, rest) = match output.split_once('\u{1e}') {
        Some((h, r)) => (h, r),
        None => (output, ""),
    };
    let mut fields = head.split('\u{0}');
    detail.hash = fields.next().unwrap_or("").trim().to_string();
    detail.author = fields.next().unwrap_or("").trim().to_string();
    detail.email = fields.next().unwrap_or("").trim().to_string();
    detail.date = fields.next().unwrap_or("").trim().to_string();
    detail.subject = fields.next().unwrap_or("").trim().to_string();
    detail.body = fields.next().unwrap_or("").trim().to_string();
    for line in rest.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            continue;
        }
        let mut cols = line.split('\t');
        let ins = cols.next().unwrap_or("").trim();
        let del = cols.next().unwrap_or("").trim();
        let path = cols.next().unwrap_or("").trim().to_string();
        if path.is_empty() {
            continue;
        }
        detail.files.push(ChangedFile {
            path,
            insertions: ins.parse::<usize>().unwrap_or(0),
            deletions: del.parse::<usize>().unwrap_or(0),
        });
    }
    detail
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_log_lines() {
        let out = "a1b2c3d\u{0}Alice\u{0}2026-10-10\u{0}initial commit\n\
                   d4e5f6g\u{0}Bob\u{0}2026-10-12\u{0}fix bug\n";
        let commits = parse_log(out);
        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0].hash, "a1b2c3d");
        assert_eq!(commits[0].author, "Alice");
        assert_eq!(commits[0].subject, "initial commit");
        assert_eq!(commits[1].date, "2026-10-12");
    }

    #[test]
    fn empty_log() {
        assert!(parse_log("").is_empty());
    }

    #[test]
    fn parses_full_log_format() {
        let out = "a1b2c3d\u{0}aaaabbbbccccdddd\u{0}Alice\u{0}a@x.com\u{0}2026-10-10T12:00:00+08:00\u{0}init\u{0}p1 p2\u{0}HEAD -> master, origin/master\n";
        let c = &parse_log(out)[0];
        assert_eq!(c.hash, "a1b2c3d");
        assert_eq!(c.full, "aaaabbbbccccdddd");
        assert_eq!(c.email, "a@x.com");
        assert_eq!(c.subject, "init");
        assert_eq!(c.parents, vec!["p1".to_string(), "p2".to_string()]);
        assert_eq!(c.refs.len(), 2);
        assert!(c.is_head());
    }

    #[test]
    fn parses_commit_detail() {
        let out = "abc\u{0}Alice\u{0}a@x.com\u{0}2026-10-10\u{0}fix\u{0}body line\u{1e}\n12\t3\tsrc/a.cpp\n0\t7\tsrc/b.cpp\n";
        let d = parse_commit_detail(out);
        assert_eq!(d.subject, "fix");
        assert_eq!(d.body, "body line");
        assert_eq!(d.files.len(), 2);
        assert_eq!(d.insertions(), 12);
        assert_eq!(d.deletions(), 10);
        assert!(d.summary().contains("2 files changed"));
    }
}
