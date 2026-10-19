//! ext-cpp-highlight：C/C++ 语法高亮扩展。
//!
//! 这是一份**语言扩展**：`crates/ext-cpp-highlight/queries/cpp-highlights.scm` 是它的
//! 语言资源，`activate()` 把「后缀 → 高亮器工厂」注册进宿主。IDE 本体不认识 C++，
//! 只认识 `HighlightGroup` 词汇表。
//!
//! 性能约定（迁移自 `ide-core` 时特意收紧）：
//! - grammar 与查询用 `OnceLock` **进程内共享**：查询编译是毫秒级，绝不能每个文档来一次；
//! - 每个文档只新建一个 `Parser`（几十微秒）并保留增量 `Tree`；
//! - 一次文档变更只跑一趟查询 + 一次排序，结果按字节区间交给绘制层做游标扫描。

use ide_core::{Highlight, HighlightGroup};
use ide_ext::{
    Capability, ExtContext, ExtError, Extension, LanguageContribution, LanguageHighlighter,
    Manifest, TextEdit,
};
use std::collections::HashSet;
use std::sync::OnceLock;
use tree_sitter::{InputEdit, Language, Parser, Query, QueryCursor, StreamingIterator, Tree};

/// 语言资源：语法高亮查询。
const QUERY_SRC: &str = include_str!("../queries/cpp-highlights.scm");

/// 认领的文件后缀（不含点，大小写不敏感）。
const EXTENSIONS: &[&str] = &[
    "c", "cc", "cpp", "cxx", "c++", "h", "hh", "hpp", "hxx", "h++", "inl", "ipp", "tpp", "tcc",
    "m", "mm",
];

static MANIFEST: Manifest = Manifest {
    id: "liteide.cpp-highlight",
    name: "C/C++ 语法高亮",
    version: "0.1.0",
    description: "tree-sitter C/C++ 语法高亮（grammar + query 资源）",
    capabilities: &[Capability::Language(LanguageContribution {
        id: "cpp",
        name: "C/C++",
        extensions: EXTENSIONS,
        create: || Box::new(CppHighlighter::new()),
    })],
};

/// C/C++ 语法高亮扩展（无状态，注册完即完成使命）。
#[derive(Debug, Default, Clone, Copy)]
pub struct CppHighlight;

impl Extension for CppHighlight {
    fn manifest(&self) -> &'static Manifest {
        &MANIFEST
    }

    fn activate(&mut self, ctx: &mut ExtContext<'_>) -> Result<(), ExtError> {
        ide_ext::register_declared(ctx, &MANIFEST)
    }
}

/// cpp grammar（进程内共享）。
fn language() -> &'static Language {
    static LANG: OnceLock<Language> = OnceLock::new();
    LANG.get_or_init(|| tree_sitter_cpp::LANGUAGE.into())
}

/// 编译好的高亮查询（进程内共享；编译失败只记日志，编辑器退化为无高亮）。
fn highlight_query() -> Option<&'static Query> {
    static QUERY: OnceLock<Option<Query>> = OnceLock::new();
    QUERY
        .get_or_init(|| match Query::new(language(), QUERY_SRC) {
            Ok(q) => Some(q),
            Err(err) => {
                tracing::error!(%err, "C++ 高亮查询编译失败，编辑器将无语法高亮");
                None
            }
        })
        .as_ref()
}

/// 规则具体度：同一区间冲突时，具体规则胜过兜底规则，`Override` 胜过具体规则。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Spec {
    Fallback = 0,
    Specific = 1,
    Override = 2,
}

/// 捕获名 → (分组, 具体度)。未识别的捕获不着色。
fn capture_to_group(capture: &str) -> Option<(HighlightGroup, Spec)> {
    use HighlightGroup as G;
    use Spec::{Fallback, Override, Specific};
    Some(match capture {
        "comment" => (G::Comment, Specific),
        "string" => (G::String, Specific),
        "number" => (G::Number, Specific),
        "constant" => (G::Constant, Specific),
        "control" => (G::KeywordControl, Specific),
        "keyword" => (G::Keyword, Specific),
        "type" => (G::Type, Specific),
        "namespace" => (G::Namespace, Specific),
        "function" => (G::Function, Specific),
        "parameter" => (G::Parameter, Specific),
        "macro" => (G::Macro, Specific),
        "enumMember" => (G::EnumMember, Specific),
        "preproc" => (G::Preproc, Specific),
        "operator" => (G::Operator, Specific),
        // 例外规则：`int32_t`/`size_t` 这类标准库 typedef 虽被 grammar 归为
        // primitive_type，但要按类型色着色。
        "typedefName" => (G::Type, Override),
        // 兜底：下面还会按命名习惯细分（全大写 → 宏；同文件枚举成员 → 枚举值）
        "variable" => (G::Variable, Fallback),
        "field" => (G::Property, Fallback),
        _ => return None,
    })
}

/// 全大写标识符 ≈ 宏名（VSCode 里是 `entity.name.function.preprocessor`）。
///
/// 单字符（如 `T`）不判为宏，避免把模板参数、循环变量误判。
fn is_macro_name(name: &str) -> bool {
    name.len() >= 2
        && name.chars().any(|c| c.is_ascii_uppercase())
        && name
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

/// 单条候选高亮（带具体度，用于定序去重）。
struct Entry {
    start: usize,
    end: usize,
    group: HighlightGroup,
    spec: Spec,
}

/// 跑一趟查询并整理成高亮区间。
fn collect(query: &Query, tree: &Tree, text: &str) -> Vec<Highlight> {
    let names = query.capture_names();
    let mut raw: Vec<(usize, usize, u16, HighlightGroup, Spec)> = Vec::new();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, tree.root_node(), text.as_bytes());
    while let Some(m) = matches.next() {
        for cap in m.captures {
            let idx = cap.index as usize;
            let Some(name) = names.get(idx).copied() else {
                continue;
            };
            let Some((group, spec)) = capture_to_group(name) else {
                continue;
            };
            raw.push((
                cap.node.start_byte(),
                cap.node.end_byte(),
                cap.index as u16,
                group,
                spec,
            ));
        }
    }

    // 本文件里声明的枚举成员名：使用处据此判为枚举值（不做跨文件语义分析，够用且零成本）。
    let mut enum_members: HashSet<&str> = HashSet::new();
    for &(start, end, idx, _, _) in &raw {
        if names.get(idx as usize).copied() == Some("enumMember") {
            if let Some(member) = text.get(start..end) {
                enum_members.insert(member);
            }
        }
    }

    let mut entries: Vec<Entry> = raw
        .into_iter()
        .filter_map(|(start, end, idx, mut group, spec)| {
            if start >= end {
                return None;
            }
            if spec == Spec::Fallback {
                let word = text.get(start..end)?;
                if names.get(idx as usize).copied() == Some("variable") {
                    if enum_members.contains(word) {
                        group = HighlightGroup::EnumMember;
                    } else if is_macro_name(word) {
                        group = HighlightGroup::Macro;
                    }
                }
            }
            Some(Entry {
                start,
                end,
                group,
                spec,
            })
        })
        .collect();

    // 同区间冲突 → 具体规则优先（不依赖查询内部的匹配顺序，结果可复现）。
    entries.sort_unstable_by(|a, b| {
        a.start
            .cmp(&b.start)
            .then(b.spec.cmp(&a.spec))
            .then(b.end.cmp(&a.end))
    });
    entries.dedup_by(|a, b| a.start == b.start && a.end == b.end);

    entries
        .into_iter()
        .map(|e| Highlight::new(e.start, e.end, e.group))
        .collect()
}

/// C/C++ 高亮器：每个文档一个实例；grammar/查询全局共享。
pub struct CppHighlighter {
    parser: Parser,
    tree: Option<Tree>,
}

impl CppHighlighter {
    /// 构造（只建 parser，微秒级）。
    pub fn new() -> Self {
        let mut parser = Parser::new();
        if parser.set_language(language()).is_err() {
            tracing::error!("C++ grammar 加载失败，该文档将无语法高亮");
        }
        Self { parser, tree: None }
    }
}

impl Default for CppHighlighter {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for CppHighlighter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CppHighlighter")
            .field("cached_tree", &self.tree.is_some())
            .finish()
    }
}

impl LanguageHighlighter for CppHighlighter {
    fn highlight(&mut self, text: &str, edit: Option<TextEdit>) -> Vec<Highlight> {
        // 空文本短路：否则会用旧树做增量解析，旧节点偏移越界会 panic。
        if text.is_empty() {
            self.tree = None;
            return Vec::new();
        }
        let Some(query) = highlight_query() else {
            return Vec::new();
        };

        // 增量解析：调用方给出编辑描述、且旧树确实覆盖旧文本时，克隆旧树并套用编辑。
        //
        // 没有编辑描述就必须全量解析：直接把旧树交给 `parse` 会让 tree-sitter
        // 产出"只覆盖旧长度"的树，新增文本会没有高亮（实测过）。
        // `Tree` 内部是 Arc，克隆只是引用计数 +1，很便宜。
        let old = match (edit, self.tree.as_ref()) {
            (Some(edit), Some(tree)) if edit.old_end <= tree.root_node().end_byte() => {
                let mut tree = tree.clone();
                tree.edit(&InputEdit {
                    start_byte: edit.start,
                    old_end_byte: edit.old_end,
                    new_end_byte: edit.new_end,
                    start_position: ts_point(edit.start_point),
                    old_end_position: ts_point(edit.old_end_point),
                    new_end_position: ts_point(edit.new_end_point),
                });
                Some(tree)
            }
            _ => None,
        };

        let mut tree = self.parser.parse(text, old.as_ref());
        // 防御：拿到的树没覆盖全文（例如编辑描述有误），退化为全量解析，
        // 宁可多花一次解析也绝不把"过期语法树"交出去。
        let covers_all = tree
            .as_ref()
            .map(|t| t.root_node().end_byte() == text.len())
            .unwrap_or(false);
        if !covers_all {
            tree = self.parser.parse(text, None);
        }
        let Some(tree) = tree else {
            return Vec::new();
        };

        let out = collect(query, &tree, text);
        self.tree = Some(tree);
        out
    }

    fn reset(&mut self) {
        self.tree = None;
    }
}

/// ide-ext 的 `Point` → tree-sitter 的 `Point`（两者语义一致：行 + 行内字节偏移）。
fn ts_point(p: ide_ext::Point) -> tree_sitter::Point {
    tree_sitter::Point {
        row: p.row,
        column: p.column,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 高亮一次（全新高亮器 = 全量解析）。
    fn highlights(src: &str) -> Vec<Highlight> {
        CppHighlighter::new().highlight(src, None)
    }

    /// `needle` 首次出现处必须被**恰好覆盖**，返回其分组。
    fn group_of(src: &str, needle: &str) -> HighlightGroup {
        let hl = highlights(src);
        group_at(&hl, src, needle)
    }

    fn group_at(hl: &[Highlight], src: &str, needle: &str) -> HighlightGroup {
        let off = src.find(needle).expect("needle 不在源码里");
        let end = off + needle.len();
        hl.iter()
            .find(|h| h.start == off && h.end == end)
            .map(|h| h.group)
            .unwrap_or_else(|| panic!("`{needle}` 未被高亮；全部 = {hl:?}"))
    }

    /// 覆盖 `needle` 起始字节的那条高亮（用于词边界不好确定的场景）。
    fn group_covering(hl: &[Highlight], src: &str, needle: &str) -> HighlightGroup {
        let off = src.find(needle).expect("needle 不在源码里");
        hl.iter()
            .find(|h| h.start <= off && off < h.end)
            .map(|h| h.group)
            .unwrap_or_else(|| panic!("`{needle}` 处没有高亮；全部 = {hl:?}"))
    }

    /// 打印某个源码的高亮清单（排查用：`cargo test -- --nocapture debug_dump`）。
    fn dump(src: &str) {
        for h in highlights(src) {
            eprintln!(
                "{:>4}..{:<4} {:?}\t{:?}",
                h.start,
                h.end,
                h.group,
                &src[h.start..h.end]
            );
        }
    }

    const KITCHEN_SINK: &str = r#"
// 注释
#include <vector>
#define MAX_SIZE 10
enum Color { RED, GREEN };
int add(int a, int b) {
    int sum = a + b + 1;
    struct Point { int x; } p;
    std::vector<int> v;
    p.x = MAX_SIZE;
    Color c = RED;
    bool ok = true;
    const char* s = "hi";
    if (ok) { sum = sum + 1; }
    return sum;
}
"#;

    #[test]
    fn parses_simple_main() {
        let src = "int main() { return 0; }";
        let hl = highlights(src);
        assert!(!hl.is_empty());
        assert_eq!(group_of(src, "int"), HighlightGroup::Keyword);
        assert_eq!(group_of(src, "main"), HighlightGroup::Function);
        assert_eq!(group_of(src, "return"), HighlightGroup::KeywordControl);
        assert_eq!(group_of(src, "0"), HighlightGroup::Number);
        assert_eq!(group_of(src, "("), HighlightGroup::Operator);
    }

    #[test]
    fn comment_and_number_captured() {
        let src = "// comment\nint x = 42;";
        let hl = highlights(src);
        assert_eq!(group_at(&hl, src, "// comment"), HighlightGroup::Comment);
        assert_eq!(group_at(&hl, src, "42"), HighlightGroup::Number);
    }

    #[test]
    fn control_keywords_have_their_own_group() {
        let src = "void f() { if (a) { for (;;) {} } while (b) {} do {} while (c); switch (d) { case 1: break; default: continue; } return; }";
        let hl = highlights(src);
        for kw in [
            "if", "for", "while", "do", "switch", "case", "default", "break", "continue", "return",
        ] {
            assert_eq!(
                group_at(&hl, src, kw),
                HighlightGroup::KeywordControl,
                "`{kw}` 应是控制流关键字"
            );
        }
    }

    #[test]
    fn primitive_types_and_storage_are_keywords() {
        let src = "unsigned int f(const char* s) { static void* p = nullptr; return 0; }";
        let hl = highlights(src);
        for kw in ["unsigned", "int", "const", "char", "static", "void", "nullptr"] {
            let want = if kw == "nullptr" {
                HighlightGroup::Constant
            } else {
                HighlightGroup::Keyword
            };
            assert_eq!(group_at(&hl, src, kw), want, "`{kw}` 分组不符");
        }
    }

    #[test]
    fn user_types_and_namespaces() {
        let src = "std::vector<int32_t> v; MyClass c;";
        let hl = highlights(src);
        assert_eq!(group_at(&hl, src, "std"), HighlightGroup::Namespace);
        assert_eq!(group_at(&hl, src, "vector"), HighlightGroup::Type);
        assert_eq!(group_at(&hl, src, "int32_t"), HighlightGroup::Type);
        assert_eq!(group_at(&hl, src, "MyClass"), HighlightGroup::Type);
        assert_eq!(group_covering(&hl, src, "v;"), HighlightGroup::Variable);
    }

    #[test]
    fn function_declaration_and_call() {
        let src = "int foo(int x) { return bar(x); }\nvoid ns::baz() {}";
        let hl = highlights(src);
        assert_eq!(group_at(&hl, src, "foo"), HighlightGroup::Function);
        assert_eq!(group_at(&hl, src, "bar"), HighlightGroup::Function);
        assert_eq!(group_at(&hl, src, "ns"), HighlightGroup::Namespace);
        assert_eq!(group_at(&hl, src, "baz"), HighlightGroup::Function);
    }

    #[test]
    fn parameters_and_variables_are_distinguished() {
        let src = "int f(int a) { int b = a; return b; }";
        let hl = highlights(src);
        // 形参声明 `int a)` 里的 a
        assert_eq!(group_covering(&hl, src, "a)"), HighlightGroup::Parameter);
        // 局部变量声明 `int b =`
        assert_eq!(group_covering(&hl, src, "b ="), HighlightGroup::Variable);
        // 形参的引用 `a;`
        assert_eq!(group_covering(&hl, src, "a;"), HighlightGroup::Variable);
        // 末尾的 b 是变量引用
        let last_b = src.rfind('b').expect("b");
        assert!(hl
            .iter()
            .any(|h| h.start == last_b && h.end == last_b + 1 && h.group == HighlightGroup::Variable));
        // return 是控制流关键字
        assert_eq!(group_covering(&hl, src, "return"), HighlightGroup::KeywordControl);
    }

    #[test]
    fn member_access_is_property() {
        let src = "void f() { obj.field = 1; obj.method(); }";
        let hl = highlights(src);
        assert_eq!(group_at(&hl, src, "field"), HighlightGroup::Property);
        // 方法调用优先判为函数
        assert_eq!(group_at(&hl, src, "method"), HighlightGroup::Function);
        assert_eq!(group_at(&hl, src, "obj"), HighlightGroup::Variable);
    }

    #[test]
    fn macros_and_uppercase_identifiers() {
        let src = "#define MAX_SIZE 10\nint x = MAX_SIZE;\n";
        let hl = highlights(src);
        assert_eq!(group_at(&hl, src, "#define"), HighlightGroup::Preproc);
        assert_eq!(group_at(&hl, src, "MAX_SIZE"), HighlightGroup::Macro);
        assert_eq!(group_at(&hl, src, "x"), HighlightGroup::Variable);
        // 单字符大写不判为宏（模板参数、循环变量）
        assert_eq!(group_of("void g() { int T = 1; }", "T"), HighlightGroup::Variable);
    }

    #[test]
    fn enum_members_at_declaration_and_use() {
        let src = "enum E { RS_DEFAULT, RS_OTHER };\nint x = RS_DEFAULT;";
        let hl = highlights(src);
        assert_eq!(group_at(&hl, src, "E"), HighlightGroup::Type);
        assert_eq!(group_at(&hl, src, "RS_OTHER"), HighlightGroup::EnumMember);
        let decl = src.find("RS_DEFAULT").expect("decl");
        let use_site = src.rfind("RS_DEFAULT").expect("use");
        assert_ne!(decl, use_site);
        for off in [decl, use_site] {
            assert!(
                hl.iter().any(|h| h.start == off
                    && h.end == off + "RS_DEFAULT".len()
                    && h.group == HighlightGroup::EnumMember),
                "偏移 {off} 处应是枚举值"
            );
        }
    }

    #[test]
    fn constants_strings_and_operators() {
        let src = r#"bool ok = true; char* p = nullptr; const char* s = "hi"; int n = NULL;"#;
        let hl = highlights(src);
        for c in ["true", "nullptr", "NULL"] {
            assert_eq!(group_at(&hl, src, c), HighlightGroup::Constant, "{c}");
        }
        assert_eq!(group_at(&hl, src, "\"hi\""), HighlightGroup::String);
        assert_eq!(group_at(&hl, src, "="), HighlightGroup::Operator);
    }

    #[test]
    fn include_directive_and_angle_path() {
        let src = "#include <vector>\n#include \"local.h\"";
        let hl = highlights(src);
        assert_eq!(group_at(&hl, src, "#include"), HighlightGroup::Preproc);
        assert_eq!(group_at(&hl, src, "<vector>"), HighlightGroup::String);
        assert_eq!(group_at(&hl, src, "\"local.h\""), HighlightGroup::String);
    }

    #[test]
    fn every_group_is_reachable() {
        let hl = highlights(KITCHEN_SINK);
        let seen: HashSet<HighlightGroup> = hl.iter().map(|h| h.group).collect();
        let missing: Vec<&str> = HighlightGroup::ALL
            .iter()
            .filter(|g| **g != HighlightGroup::Plain && !seen.contains(g))
            .map(|g| g.name())
            .collect();
        assert!(
            missing.is_empty(),
            "这些分组没有任何规则覆盖：{missing:?}\n{}",
            {
                dump(KITCHEN_SINK);
                String::new()
            }
        );
    }

    #[test]
    fn ranges_are_valid_sorted_and_never_plain() {
        let hl = highlights(KITCHEN_SINK);
        let mut last = 0usize;
        for h in &hl {
            assert!(h.start < h.end, "空区间：{h:?}");
            assert!(h.end <= KITCHEN_SINK.len(), "越界：{h:?}");
            assert!(h.start >= last, "未按 start 升序：{h:?}");
            assert_ne!(h.group, HighlightGroup::Plain, "Plain 不应出现在结果里");
            last = h.start;
        }
    }

    #[test]
    fn highlighting_is_deterministic() {
        let a = highlights(KITCHEN_SINK);
        let b = highlights(KITCHEN_SINK);
        assert_eq!(a, b, "同一输入必须得到同一结果（不依赖查询内部顺序）");
    }

    #[test]
    fn every_claimed_extension_maps_to_cpp() {
        let host = ide_ext::ExtensionHost::activate(vec![Box::new(CppHighlight)]);
        assert_eq!(host.active_ids(), vec!["liteide.cpp-highlight"]);
        let reg = host.registry();
        for ext in EXTENSIONS {
            let path = format!("src/file.{ext}");
            assert!(reg.language_for_path(&path).is_some(), "后缀 {ext} 未认领");
        }
        assert!(reg.language_for_path("README.md").is_none());
        assert_eq!(reg.language("cpp").map(|l| l.name), Some("C/C++"));
        // 工厂产物可用
        let mut h = reg.create_highlighter("a.cpp");
        assert!(!h.highlight("int a = 1;", None).is_empty());
    }

    #[test]
    fn query_compiles_without_error() {
        match Query::new(language(), QUERY_SRC) {
            Ok(_) => {}
            Err(err) => panic!("高亮查询编译失败：{err}"),
        }
    }

    #[test]
    fn same_length_edit_stays_correct() {
        let mut h = CppHighlighter::new();
        let a = "int a = 1;";
        let b = "int a = 2;";
        assert!(!h.highlight(a, None).is_empty());
        let edit = TextEdit::between(a, b);
        assert_eq!(h.highlight(b, edit), highlights(b), "同长度改动必须被重算");
    }

    #[test]
    fn length_changing_edits_never_return_stale_highlights() {
        // 回归：直接把「短旧树」交给 parse 会得到只覆盖旧长度的树，
        // 新增文本就没有高亮了。带编辑描述时也必须覆盖到新内容。
        let mut h = CppHighlighter::new();
        let a = "int a = 1;";
        let longer = "int a = 1; int b = 2;";
        h.highlight(a, None);
        let edit = TextEdit::between(a, longer);
        assert_eq!(
            h.highlight(longer, edit),
            highlights(longer),
            "追加文本后必须覆盖到新内容"
        );
        let edit = TextEdit::between(longer, a);
        assert_eq!(
            h.highlight(a, edit),
            highlights(a),
            "缩短文本后同样要正确"
        );
    }

    #[test]
    fn omitted_edit_hint_falls_back_to_full_parse() {
        // 接口允许不给编辑描述（尽管会失去增量解析）——结果必须依然正确。
        let mut h = CppHighlighter::new();
        h.highlight("int a = 1;", None);
        assert_eq!(
            h.highlight("int a = 1; int b = 2;", None),
            highlights("int a = 1; int b = 2;")
        );
    }

    #[test]
    fn wrong_edit_hint_falls_back_to_full_parse() {
        // 编辑描述与旧树不匹配（old_end 超出旧树范围）→ 必须退化为全量解析。
        let mut h = CppHighlighter::new();
        let a = "int a = 1;";
        h.highlight(a, None);
        let next = "int a = 1; int b = 2;";
        let bad = TextEdit {
            start: 0,
            old_end: 4096,
            new_end: 5,
            start_point: ide_ext::Point { row: 0, column: 0 },
            old_end_point: ide_ext::Point {
                row: 99,
                column: 0,
            },
            new_end_point: ide_ext::Point { row: 0, column: 5 },
        };
        assert_eq!(h.highlight(next, Some(bad)), highlights(next));
    }

    #[test]
    fn unicode_edit_keeps_highlights_consistent() {
        // 中文注释：编辑边界必须落在 char 边界上，增量结果与全量一致。
        let mut h = CppHighlighter::new();
        let a = "// 注释\nint x = 1;";
        let b = "// 注释更长一些\nint x = 1;";
        h.highlight(a, None);
        let edit = TextEdit::between(a, b);
        assert_eq!(h.highlight(b, edit), highlights(b));
        let edit = TextEdit::between(b, a);
        assert_eq!(h.highlight(a, edit), highlights(a));
    }

    #[test]
    fn typing_sequence_matches_full_parse_every_step() {
        // 模拟真实打字：每一步都用上一步的文本算编辑描述，结果必须与全量解析逐步一致。
        let base = "int main() {\n    int x = 1;\n    return x;\n}\n";
        let mut h = CppHighlighter::new();
        let mut cur = String::from(base);
        h.highlight(&cur, None);
        for (i, ch) in "// hello".chars().enumerate() {
            let mut next = cur.clone();
            next.insert(base.len(), ch);
            let edit = TextEdit::between(&cur, &next);
            assert_eq!(
                h.highlight(&next, edit),
                highlights(&next),
                "第 {i} 次输入 {ch:?} 后结果不一致"
            );
            cur = next;
        }
    }

    #[test]
    fn reset_and_empty_input_do_not_panic() {
        let mut h = CppHighlighter::new();
        assert!(h.highlight("", None).is_empty());
        assert!(!h.highlight("int a;", None).is_empty());
        assert!(h.highlight("", None).is_empty(), "清空后不应残留旧树的影响");
        h.reset();
        assert!(!h.highlight("int a;", None).is_empty());
    }

    #[test]
    fn highlight_synthetic_file_is_within_budget() {
        // 性能硬约束：2000 行量级的文件，一次全量高亮必须远低于交互可接受阈值。
        let mut src = String::with_capacity(160 * 1024);
        for i in 0..2000 {
            src.push_str(&format!(
                "// line {i}\nint add_{i}(int a, int b) {{ return a + b + 1; }}\n"
            ));
        }
        let mut h = CppHighlighter::new();
        let t0 = std::time::Instant::now();
        let hl = h.highlight(&src, None);
        let elapsed = t0.elapsed();
        assert!(!hl.is_empty());
        assert!(
            elapsed < std::time::Duration::from_millis(1500),
            "{} 字节高亮耗时 {elapsed:?} 超预算",
            src.len()
        );
    }

    #[test]
    fn incremental_typing_is_much_cheaper_than_full_parse() {
        // 性能契约：带编辑描述时，追加式编辑必须显著快于全量解析（否则增量没生效）。
        let mut src = String::with_capacity(160 * 1024);
        for i in 0..2000 {
            src.push_str(&format!(
                "// line {i}\nint add_{i}(int a, int b) {{ return a + b + 1; }}\n"
            ));
        }
        let mut h = CppHighlighter::new();
        h.highlight(&src, None);

        // 全量：丢掉缓存重算
        let t_full = std::time::Instant::now();
        CppHighlighter::new().highlight(&src, None);
        let full = t_full.elapsed();

        // 增量：末尾追加一个字符
        let mut next = src.clone();
        next.push(' ');
        let edit = TextEdit::between(&src, &next);
        let t_incr = std::time::Instant::now();
        let hl = h.highlight(&next, edit);
        let incr = t_incr.elapsed();
        assert_eq!(hl, highlights(&next));
        assert!(
            incr < full,
            "增量解析({incr:?}) 应快于全量({full:?})"
        );
    }
}
