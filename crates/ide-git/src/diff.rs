//! 统一 diff 文本的轻量分类（UI 着色用）。
//!
//! 阶段 4 不做并排视图，直接展示 `git diff` 的统一文本，
//! 按行首字符着色：`+` 绿、`-` 红、`@@` 蓝。

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffLineKind {
    Added,
    Removed,
    HunkHeader,
    Context,
}

#[derive(Debug, Clone)]
pub struct DiffLine {
    pub kind: DiffLineKind,
    pub text: String,
}

/// 把 unified diff 文本拆成着色行。
pub fn classify_diff(text: &str) -> Vec<DiffLine> {
    text.lines()
        .map(|l| {
            let kind = if let Some(rest) = l.strip_prefix("+++").or_else(|| l.strip_prefix("---")) {
                let _ = rest;
                DiffLineKind::Context
            } else if l.starts_with("@@") {
                DiffLineKind::HunkHeader
            } else if l.starts_with('+') {
                DiffLineKind::Added
            } else if l.starts_with('-') {
                DiffLineKind::Removed
            } else {
                DiffLineKind::Context
            };
            DiffLine { kind, text: l.to_string() }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
diff --git a/a.rs b/a.rs
index 111..222 100644
--- a/a.rs
+++ b/a.rs
@@ -1,3 +1,4 @@
 line1
-line2
+line2-fixed
+line3
 context
";

    #[test]
    fn classifies_lines() {
        let lines = classify_diff(SAMPLE);
        use DiffLineKind::*;
        let kinds: Vec<_> = lines.iter().map(|l| l.kind).collect();
        assert!(kinds.contains(&HunkHeader));
        assert!(kinds.contains(&Added));
        assert!(kinds.contains(&Removed));
        assert!(kinds.contains(&Context));
    }

    #[test]
    fn empty_diff() {
        assert!(classify_diff("").is_empty());
    }
}
