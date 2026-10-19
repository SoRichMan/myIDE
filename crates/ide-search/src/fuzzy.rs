//! 轻量模糊匹配：子序列命中 + 分数排序（Ctrl+P 文件搜索）。
//!
//! 不引入外部依赖，保持二进制体积。分数越高越靠前；不命中返回 None。

/// 判断 `query` 是否是 `candidate` 的子序列，并给出分数。
///
/// 分数规则（粗调，够用即可）：
/// - 基础命中每字符 1 分
/// - 连续命中额外 +2（"main" 命中 "main.rs" 远好于 "mxain"）
/// - 路径分隔符后命中 +3（匹配目录名/文件名开头更有价值）
/// - 完整不区分大小写
pub fn fuzzy_score(query: &str, candidate: &str) -> Option<u32> {
    if query.is_empty() {
        return Some(0);
    }
    let q: Vec<char> = query.chars().map(|c| c.to_ascii_lowercase()).collect();
    let c: Vec<char> = candidate.chars().map(|c| c.to_ascii_lowercase()).collect();
    let mut qi = 0usize;
    let mut score: u32 = 0;
    let mut streak = 0u32;
    let mut prev_was_sep = true; // 起点视为分隔符后
    for (ci, &cc) in c.iter().enumerate() {
        if qi < q.len() && cc == q[qi] {
            score += 1;
            streak += 1;
            if streak > 1 {
                score += 2;
            }
            if prev_was_sep {
                score += 3;
            }
            prev_was_sep = false;
            qi += 1;
        } else {
            streak = 0;
            prev_was_sep = cc == '/' || cc == '\\' || cc == '.';
        }
        let _ = ci;
    }
    if qi == q.len() {
        Some(score)
    } else {
        None
    }
}

/// 从候选列表中筛出模糊命中并按分数降序，取前 `limit` 条。
pub fn fuzzy_filter<'a>(query: &str, candidates: &'a [String], limit: usize) -> Vec<&'a String> {
    let mut scored: Vec<(u32, &String)> = candidates
        .iter()
        .filter_map(|c| fuzzy_score(query, c).map(|s| (s, c)))
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(b.1)));
    scored.into_iter().take(limit).map(|(_, c)| c).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subsequence_hits() {
        assert!(fuzzy_score("main", "src/main.rs").is_some());
        assert!(fuzzy_score("mn", "src/main.rs").is_some());
    }

    #[test]
    fn non_subsequence_miss() {
        assert_eq!(fuzzy_score("xyz", "src/main.rs"), None);
    }

    #[test]
    fn contiguous_scores_higher_than_scattered() {
        let contig = fuzzy_score("main", "main.rs").unwrap();
        let scatter = fuzzy_score("man", "module/alpha/node.rs").unwrap();
        assert!(contig > scatter, "{contig} should beat {scatter}");
    }

    #[test]
    fn empty_query_matches_all_with_zero() {
        assert_eq!(fuzzy_score("", "anything"), Some(0));
    }

    #[test]
    fn filter_ranks_and_limits() {
        let files: Vec<String> = [
            "src/main.rs",
            "src/lib.rs",
            "tests/main_test.rs",
            "README.md",
        ].iter().map(|s| s.to_string()).collect();
        let r = fuzzy_filter("main", &files, 2);
        assert_eq!(r.len(), 2);
        // 两个含 main 的条目；第一个应是得分更高的。
        assert!(r[0].contains("main"));
    }
}
