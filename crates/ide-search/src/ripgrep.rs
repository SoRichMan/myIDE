//! 解析 `rg --json` 的 JSON Lines 输出。
//!
//! 我们只关心 `type:"match"` 事件，提炼成 [`SearchMatch`]。
//! `begin`/`context`/`end`/`summary` 事件忽略。

use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchMatch {
    pub file: String,
    pub line: u64,
    pub text: String,
    /// (char start, char end) in line，用于高亮。
    pub submatches: Vec<(usize, usize)>,
}

#[derive(Debug, Deserialize)]
struct JsonLine {
 #[serde(rename = "type")]
    kind: String,
    data: serde_json::Value,
}

/// 解析 `rg --json` 全量输出，返回匹配列表。解析失败的行静默跳过
/// （rg 偶尔输出非 JSON 的进度行，不能让一行坏数据拖死整个搜索）。
pub fn parse_rg_json(output: &str) -> Vec<SearchMatch> {
    let mut out = Vec::new();
    for line in output.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(j) = serde_json::from_str::<JsonLine>(line) {
            if j.kind == "match" {
                if let Some(m) = parse_match_data(&j.data) {
                    out.push(m);
                }
            }
        }
    }
    out
}

fn parse_match_data(d: &serde_json::Value) -> Option<SearchMatch> {
    let file = d.get("path")?.get("text")?.as_str()?.to_string();
    // rg 拿到显式路径参数（`.`）时会把结果路径写成 `./a/b.rs`；
    // 去掉前缀，保持与 `rg --files` 相同的相对路径（UI / LSP 都按 root 拼接）。
    let file = file.strip_prefix("./").unwrap_or(&file).to_string();
    let line = d.get("line_number")?.as_u64()?;
    let text = d
        .get("lines")
        .and_then(|l| l.get("text"))
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .trim_end_matches('\n')
        .trim_end_matches('\r')
        .to_string();
    let mut submatches = Vec::new();
    if let Some(arr) = d.get("submatches").and_then(|s| s.as_array()) {
        for s in arr {
            let start = s.get("start").and_then(|x| x.as_u64()).unwrap_or(0) as usize;
            let end = s.get("end").and_then(|x| x.as_u64()).unwrap_or(0) as usize;
            submatches.push((start, end));
        }
    }
    Some(SearchMatch { file, line, text, submatches })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = concat!(
        r#"{"type":"begin","data":{"path":{"text":"src/main.rs"}}}"#, "\n",
        r#"{"type":"match","data":{"path":{"text":"src/main.rs"},"lines":{"text":"fn main() {\n"},"line_number":3,"absolute_offset":12,"submatches":[{"match":{"text":"main"},"start":3,"end":7}]}}"#, "\n",
        r#"{"type":"match","data":{"path":{"text":"src/main.rs"},"lines":{"text":"  return 0;\n"},"line_number":4,"absolute_offset":30,"submatches":[]}}"#, "\n",
        r#"{"type":"summary","data":{"elapsed":{"total_secs":0.01},"exit_status":0}}"#,
    );

    #[test]
    fn parses_match_events() {
        let m = parse_rg_json(SAMPLE);
        assert_eq!(m.len(), 2);
        assert_eq!(m[0].file, "src/main.rs");
        assert_eq!(m[0].line, 3);
        assert_eq!(m[0].text, "fn main() {");
        assert_eq!(m[0].submatches, vec![(3, 7)]);
        assert_eq!(m[1].line, 4);
        assert!(m[1].submatches.is_empty());
    }

    #[test]
    fn skips_garbage_lines() {
        let out = "not json at all\n{\"type\":\"summary\",\"data\":{}}\n";
        assert!(parse_rg_json(out).is_empty());
    }

    #[test]
    fn empty_input() {
        assert!(parse_rg_json("").is_empty());
    }

    #[test]
    fn malformed_match_is_skipped() {
        // line_number 缺失 → 整条丢弃，不 panic。
        let out = r#"{"type":"match","data":{"path":{"text":"a.rs"}}}"#;
        assert!(parse_rg_json(out).is_empty());
    }

    #[test]
    fn strips_dot_slash_path_prefix() {
        // rg 带路径参数时会输出 `./a/b.rs`，统一去掉前缀。
        let out = r#"{"type":"match","data":{"path":{"text":"./src/main.rs"},"lines":{"text":"x\n"},"line_number":1,"submatches":[]}}"#;
        let m = parse_rg_json(out);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].file, "src/main.rs");
    }
}
