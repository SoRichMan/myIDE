//! token 分类词汇表（语法高亮与配色的公共语言）。
//!
//! 这里只定义"token 是什么"，不定义"谁来解析、谁来着色"：
//! - 解析器由语言扩展提供（见 `ext-cpp-highlight`）；
//! - 颜色由主题扩展提供（见 `ide-ext::TokenPalette` / `ext-theme-vscode`）。
//!
//! 分组是 VSCode TextMate scope 家族的粗粒度映射，取值来源见各主题色表注释：
//! `keyword.control` → `KeywordControl`、`entity.name.type` → `Type`、
//! `variable.parameter` → `Parameter` …… 好处是"换主题只换数据"。

/// token 分组。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum HighlightGroup {
    /// 未着色文本（编辑器前景色）。
    Plain,
    /// 关键字 + 存储类（`class`/`namespace`/`const`/`auto`/`inline`/`new` ……）。
    Keyword,
    /// 控制流关键字（`if`/`for`/`while`/`return`/`break`/`switch` ……）。
    KeywordControl,
    /// 语言常量（`true`/`false`/`nullptr`）。
    Constant,
    /// 用户类型（`vector`/`int32_t`/自定义类/结构体）。
    Type,
    /// 命名空间与作用域限定（`std`、`ns::`）。
    Namespace,
    /// 函数名（声明与调用）。
    Function,
    /// 变量（局部/成员）。
    Variable,
    /// 函数形参。
    Parameter,
    /// 成员访问（`obj.field` 里的 `field`）。
    Property,
    /// 宏名（`#define` 的名字、全大写标识符）。
    Macro,
    /// 同一文件内枚举声明的成员（`enum { RS_DEFAULT }` 里的 `RS_DEFAULT`）。
    EnumMember,
    /// 预处理指令（`#include`/`#define`/`#if` ……）。
    Preproc,
    /// 字符串与字符字面量（含 `#include <...>` 的路径）。
    String,
    /// 数字字面量。
    Number,
    /// 注释。
    Comment,
    /// 标点与运算符。
    Operator,
}

impl HighlightGroup {
    /// 全部分组（配色表按此顺序索引）。
    pub const ALL: [HighlightGroup; 17] = [
        HighlightGroup::Plain,
        HighlightGroup::Keyword,
        HighlightGroup::KeywordControl,
        HighlightGroup::Constant,
        HighlightGroup::Type,
        HighlightGroup::Namespace,
        HighlightGroup::Function,
        HighlightGroup::Variable,
        HighlightGroup::Parameter,
        HighlightGroup::Property,
        HighlightGroup::Macro,
        HighlightGroup::EnumMember,
        HighlightGroup::Preproc,
        HighlightGroup::String,
        HighlightGroup::Number,
        HighlightGroup::Comment,
        HighlightGroup::Operator,
    ];

    /// 分组数量（配色表长度，用于数组维度）。
    pub const COUNT: usize = HighlightGroup::ALL.len();

    /// 数组下标（`ALL` 的顺序 = 下标顺序）。
    pub const fn index(self) -> usize {
        self as usize
    }

    /// 稳定标识名（测试/诊断输出用）。
    pub fn name(self) -> &'static str {
        match self {
            HighlightGroup::Plain => "plain",
            HighlightGroup::Keyword => "keyword",
            HighlightGroup::KeywordControl => "control",
            HighlightGroup::Constant => "constant",
            HighlightGroup::Type => "type",
            HighlightGroup::Namespace => "namespace",
            HighlightGroup::Function => "function",
            HighlightGroup::Variable => "variable",
            HighlightGroup::Parameter => "parameter",
            HighlightGroup::Property => "property",
            HighlightGroup::Macro => "macro",
            HighlightGroup::EnumMember => "enumMember",
            HighlightGroup::Preproc => "preproc",
            HighlightGroup::String => "string",
            HighlightGroup::Number => "number",
            HighlightGroup::Comment => "comment",
            HighlightGroup::Operator => "operator",
        }
    }
}

/// 一段高亮区间（**字节偏移**，与 tree-sitter 原生一致）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Highlight {
    pub start: usize,
    pub end: usize,
    pub group: HighlightGroup,
}

impl Highlight {
    pub fn new(start: usize, end: usize, group: HighlightGroup) -> Self {
        Self { start, end, group }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_index_matches_discriminant() {
        for (i, g) in HighlightGroup::ALL.iter().enumerate() {
            assert_eq!(g.index(), i, "分组 {g:?} 下标不一致");
        }
        assert_eq!(HighlightGroup::COUNT, HighlightGroup::ALL.len());
    }

    #[test]
    fn group_names_are_unique() {
        let mut names: Vec<&str> = HighlightGroup::ALL.iter().map(|g| g.name()).collect();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(before, names.len(), "分组名重复");
    }

    #[test]
    fn highlight_carries_byte_range() {
        let h = Highlight::new(3, 9, HighlightGroup::Function);
        assert_eq!((h.start, h.end, h.group), (3, 9, HighlightGroup::Function));
    }
}
