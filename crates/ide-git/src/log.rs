//! 解析 `git log --pretty=format` 输出。

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitCommit {
    pub hash: String,
    pub author: String,
    pub date: String,
    pub subject: String,
}

/// 解析用 NUL (`%x00`) 分隔的四字段输出。
/// 命令：`git log --pretty=format:'%h%x00%an%x00%ad%x00%s' --date=short -n 100`
pub fn parse_log(output: &str) -> Vec<GitCommit> {
    output
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| {
            let mut parts = l.split('\u{0}');
            GitCommit {
                hash: parts.next().unwrap_or("").to_string(),
                author: parts.next().unwrap_or("").to_string(),
                date: parts.next().unwrap_or("").to_string(),
                subject: parts.next().unwrap_or("").to_string(),
            }
        })
        .collect()
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
}
