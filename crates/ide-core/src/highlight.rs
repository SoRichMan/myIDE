//! tree-sitter C++ 语法高亮封装。
//!
//! - 复用 `tree-sitter-cpp` 自带的 `HIGHLIGHT_QUERY`，并追加 `comment` / `number` / `char` 规则。
//! - 每次调用传入上一次的 `Tree`，由 tree-sitter 做增量解析。
//! - 输出 `Vec<Highlight>`，区间为**字节偏移**（与 tree-sitter 原生一致）；UI 侧按行切片时用字节定位。
use tree_sitter::{Language, Parser, Query, QueryCursor, StreamingIterator};
use tree_sitter_cpp as ts_cpp;

/// 高亮分组（VSCode Dark+ 调色板由 UI 层映射）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HighlightGroup {
    Keyword,
    String,
    Number,
    Comment,
    Function,
    Type,
    Operator,
    Plain,
}

/// 一段高亮区间。
#[derive(Debug, Clone)]
pub struct Highlight {
    pub start: usize,
    pub end: usize,
    pub group: HighlightGroup,
}

/// 在 cpp grammar 自带 query 基础上补齐注释/数字/字符字面量。
/// 返回 `None` 表示内置 query 编译失败（理论上不可能；失败时编辑器退化为无高亮）。
fn build_query(language: &Language) -> Option<Query> {
    let mut src = String::from(ts_cpp::HIGHLIGHT_QUERY);
    src.push_str(
        r#"
; 追加规则（官方 query 里没有 comment/number/常见控制流关键字/基本类型）
(comment) @comment
(number_literal) @number
(char_literal) @string

; 控制流关键字（这些在 cpp grammar 里是裸 token）
[
  "if" "else" "for" "while" "do" "return" "break" "continue"
  "switch" "case" "default" "goto"
] @keyword

; 基本类型（cpp grammar 里 int/void 等是 primitive_type 节点，不是裸 token）
(primitive_type) @type
"#,
    );
    Query::new(language, &src).ok()
}

fn capture_to_group(name: &str) -> HighlightGroup {
    match name {
        "keyword" => HighlightGroup::Keyword,
        "string" => HighlightGroup::String,
        "number" => HighlightGroup::Number,
        "comment" => HighlightGroup::Comment,
        "function" => HighlightGroup::Function,
        "type" => HighlightGroup::Type,
        "constant" => HighlightGroup::Number,
        _ => HighlightGroup::Plain,
    }
}

/// tree-sitter 高亮器。内部缓存上一次的语法树以支持增量解析。
pub struct Highlighter {
    parser: Parser,
    query: Option<Query>,
    tree: Option<tree_sitter::Tree>,
}

impl std::fmt::Debug for Highlighter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Highlighter").finish()
    }
}

impl Default for Highlighter {
    fn default() -> Self {
        Self::new()
    }
}

impl Highlighter {
    pub fn new() -> Self {
        let language: Language = ts_cpp::LANGUAGE.into();
        let mut parser = Parser::new();
        // 内置 grammar 加载失败时退化：parse 返回 None → 高亮为空（不 panic）。
        let _ = parser.set_language(&language);
        let query = build_query(&language);
        Self {
            parser,
            query,
            tree: None,
        }
    }

    /// 对整段文本做高亮。文本通常为整个文档内容。
    pub fn highlight(&mut self, text: &str) -> Vec<Highlight> {
        // 空文本直接短路：避免用上一次的旧树做增量解析后，查询遇到
        // 旧节点偏移超出空 source 而 panic（tree-sitter TextProvider 切片越界）。
        if text.is_empty() {
            self.tree = None;
            return Vec::new();
        }
        // query 构建失败（理论不可能）→ 无高亮。
        let Some(query) = &self.query else {
            self.tree = None;
            return Vec::new();
        };
        let tree = match self.parser.parse(text, self.tree.as_ref()) {
            Some(t) => t,
            None => return Vec::new(),
        };
        self.tree = Some(tree);
        // 从缓存取引用跑查询（刚存入，必然存在；取不到则退化为无高亮）。
        let Some(tree) = self.tree.as_ref() else {
            return Vec::new();
        };

        let mut cursor = QueryCursor::new();
        let mut out = Vec::new();
        let mut matches = cursor.matches(query, tree.root_node(), text.as_bytes());
        while let Some(m) = matches.next() {
            for cap in m.captures {
                let name = query.capture_names()[cap.index as usize];
                let group = capture_to_group(name);
                if group == HighlightGroup::Plain {
                    continue;
                }
                out.push(Highlight {
                    start: cap.node.start_byte(),
                    end: cap.node.end_byte(),
                    group,
                });
            }
        }
        out
    }

    /// 丢弃缓存的语法树（文件外部改动后调用）。
    pub fn reset(&mut self) {
        self.tree = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_main() {
        let mut h = Highlighter::new();
        let src = "int main() { return 0; }";
        let hl = h.highlight(src);
        assert!(!hl.is_empty(), "expected at least one highlight token");
        // 至少应命中一个 keyword（return）和一个 type（int）
        let groups: std::collections::HashSet<HighlightGroup> =
            hl.iter().map(|h| h.group).collect();
        assert!(groups.contains(&HighlightGroup::Keyword), "no keyword: {:?}", hl);
        assert!(groups.contains(&HighlightGroup::Type), "no type: {:?}", hl);
    }

    #[test]
    fn comment_and_number_captured() {
        let mut h = Highlighter::new();
        let src = "// comment\nint x = 42;";
        let hl = h.highlight(src);
        let groups: std::collections::HashSet<HighlightGroup> =
            hl.iter().map(|h| h.group).collect();
        assert!(groups.contains(&HighlightGroup::Comment), "{:?}", hl);
        assert!(groups.contains(&HighlightGroup::Number), "{:?}", hl);
    }

    #[test]
    fn incremental_parse_is_stable() {
        let mut h = Highlighter::new();
        h.highlight("int a = 1;");
        let hl2 = h.highlight("int a = 1; int b = 2;");
        assert!(!hl2.is_empty());
    }

    #[test]
    fn empty_input_does_not_panic() {
        let mut h = Highlighter::new();
        assert!(h.highlight("").is_empty());
    }
}
