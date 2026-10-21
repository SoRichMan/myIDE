//! 分支列表：`git branch -a --format=...`

/// 一个分支（本地或远程）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Branch {
    /// 短名（`master` / `origin/main`）。
    pub name: String,
    /// 指向的短 SHA。
    pub hash: String,
    /// 是否是当前 HEAD 所在分支。
    pub current: bool,
    /// 是否是远程跟踪分支。
    pub remote: bool,
}

/// 解析 `git branch -a --format=%(refname:short)%x00%(objectname:short)%x00%(HEAD)` 的输出。
pub fn parse_branches(output: &str) -> Vec<Branch> {
    output
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let mut parts = l.split('\u{0}');
            let name = parts.next().unwrap_or("").trim().to_string();
            let hash = parts.next().unwrap_or("").trim().to_string();
            let head = parts.next().unwrap_or("").trim();
            let remote = name.contains('/') && !name.starts_with("HEAD");
            Branch {
                current: head == "*",
                name,
                hash,
                remote,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_branch_list() {
        let out = "master\u{0}abc123\u{0}*\nmain\u{0}def456\u{0}\norigin/main\u{0}def456\u{0}\n";
        let bs = parse_branches(out);
        assert_eq!(bs.len(), 3);
        assert!(bs[0].current);
        assert_eq!(bs[0].name, "master");
        assert!(!bs[1].current);
        assert!(bs[2].remote);
    }
}
