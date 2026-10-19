//! ide-ext：进程内扩展宿主（"轻量 IDE" 的扩展机制）。
//!
//! 设计要点（性能是硬约束，不是口号）：
//!
//! 1. **扩展 = 一个依赖本 crate 的 crate + 一份编译期 `Manifest`**。所有内置功能
//!    （C++ 高亮、主题配色）都走同一套机制，第三方扩展将来只需换加载器（动态库/WASM），
//!    清单与贡献点结构不变。
//! 2. **只有贡献点能被注册**：`Capability::{Language, Theme}`。新增功能 = 加一个变体
//!    + 一个注册表查询方法（见 `docs/extensions.md`）。
//! 3. **稳态零开销**：贡献点在激活时变成查表数据（数组下标/少量线性扫描），逐帧绘制
//!    路径不新增分配、不新增查找。
//! 4. **激活只许注册**：`activate()` 里禁止文件/网络 IO 与重计算；宿主动态计时并记录，
//!    测试里对总耗时设上限（见 `host::ExtensionHost::report`）。
//! 5. **失败隔离 + 原子激活**：单个扩展返回 `Err`（或 panic）只回滚它自己的贡献点并禁用，
//!    其余扩展照常工作。注意：release 档 `panic = "abort"`，此时 panic 无法被捕获。
pub mod host;
pub mod manifest;
pub mod palette;

pub use host::{ActivationOutcome, ActivationRecord, ExtContext, ExtensionHost, Registry};
pub use manifest::{Capability, Extension, LanguageContribution, Manifest, ThemeContribution};
pub use palette::TokenPalette;

use ide_core::Highlight;

/// 把清单里声明的贡献点原样注册进宿主——纯声明式扩展的 `activate` 实现。
///
/// 声明与实现同源（都来自 `Manifest`），所以不会出现"清单写了、实现忘了"的漂移；
/// 需要条件注册的扩展才自定义 `activate`。
pub fn register_declared(ctx: &mut ExtContext<'_>, manifest: &Manifest) -> Result<(), ExtError> {
    for cap in manifest.capabilities {
        match cap {
            Capability::Language(c) => ctx.register_language(*c)?,
            Capability::Theme(c) => ctx.register_theme(*c)?,
        }
    }
    Ok(())
}

/// 0-based 行列位置；`column` 是**行内字节偏移**（与 tree-sitter 的 `Point` 一致）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Point {
    pub row: usize,
    pub column: usize,
}

/// 一次文本变更（字节偏移），语义与 tree-sitter 的 `InputEdit` 一致。
///
/// 由**编辑方**（编辑器）给出，而不是高亮器自己猜：只有调用方同时持有新旧文本，
/// 才能算出"改了哪里"；有了它 tree-sitter 才能复用旧语法树（否则只能全量重解析）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextEdit {
    /// 变更起点（旧文本与新文本在 `[0, start)` 上完全相同）。
    pub start: usize,
    /// 旧文本中变更结束的字节偏移。
    pub old_end: usize,
    /// 新文本中变更结束的字节偏移。
    pub new_end: usize,
    /// `start` 在（旧）文本中的位置。
    pub start_point: Point,
    /// `old_end` 在旧文本中的位置。
    pub old_end_point: Point,
    /// `new_end` 在新文本中的位置。
    pub new_end_point: Point,
}

impl TextEdit {
    /// 由新旧文本求出最小变更；两段文本完全相同返回 `None`。
    ///
    /// 前后缀都对齐到 char 边界，保证多字节字符（中文注释等）不会切在半个字符上。
    /// 复杂度：最坏 O(n) 字节比较（追加/删除时前缀比较是 O(旧长度) 的 memcmp 级操作）。
    pub fn between(old: &str, new: &str) -> Option<Self> {
        if old == new {
            return None;
        }
        let (ob, nb) = (old.as_bytes(), new.as_bytes());
        let max_prefix = ob.len().min(nb.len());
        let mut start = 0;
        while start < max_prefix && ob[start] == nb[start] {
            start += 1;
        }
        let max_suffix = max_prefix - start;
        let mut suffix = 0;
        while suffix < max_suffix && ob[ob.len() - 1 - suffix] == nb[nb.len() - 1 - suffix] {
            suffix += 1;
        }
        // 只把边界**向外**挪（前缀变短 / 后缀变短），不会破坏"前后缀相同"的前提。
        while start > 0 && !(old.is_char_boundary(start) && new.is_char_boundary(start)) {
            start -= 1;
        }
        let mut old_end = old.len() - suffix;
        while old_end < old.len() && !old.is_char_boundary(old_end) {
            old_end += 1;
        }
        let mut new_end = new.len() - suffix;
        while new_end < new.len() && !new.is_char_boundary(new_end) {
            new_end += 1;
        }
        Some(Self {
            start,
            old_end: old_end.max(start),
            new_end: new_end.max(start),
            start_point: point_at(ob, start),
            old_end_point: point_at(ob, old_end),
            new_end_point: point_at(nb, new_end),
        })
    }

    /// 旧文本里被替换掉的字节数。
    pub fn removed(&self) -> usize {
        self.old_end.saturating_sub(self.start)
    }

    /// 新文本里插入的字节数。
    pub fn inserted(&self) -> usize {
        self.new_end.saturating_sub(self.start)
    }
}

/// 字节偏移 → 行列位置（`column` 为行内字节偏移）。
fn point_at(bytes: &[u8], byte: usize) -> Point {
    let head = &bytes[..byte];
    Point {
        row: head.iter().filter(|&&b| b == b'\n').count(),
        column: byte
            - head
                .iter()
                .rposition(|&b| b == b'\n')
                .map_or(0, |i| i + 1),
    }
}

/// 语法高亮器：由语言扩展提供（一个文档实例一个，构造必须便宜——解析器/查询须共享缓存）。
pub trait LanguageHighlighter: Send {
    /// 对整段文本做高亮；区间为**字节偏移**，按 `start` 升序返回。
    ///
    /// `edit`：上一版文本到 `text` 的变化（`None` = 全量解析）。
    /// 扩展**必须**在无法安全复用旧树时退化为全量解析，绝不能返回过期结果。
    fn highlight(&mut self, text: &str, edit: Option<TextEdit>) -> Vec<Highlight>;
    /// 丢弃增量解析缓存（换文档/文件被外部改动后调用）。
    fn reset(&mut self);
}

/// 空高亮器：没有匹配语言时的兜底（编辑器退化为无高亮，不 panic）。
#[derive(Debug, Default, Clone, Copy)]
pub struct NullHighlighter;

impl LanguageHighlighter for NullHighlighter {
    fn highlight(&mut self, _text: &str, _edit: Option<TextEdit>) -> Vec<Highlight> {
        Vec::new()
    }
    fn reset(&mut self) {}
}

/// 扩展注册/激活错误。
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ExtError {
    #[error("贡献点 id 重复：{0}")]
    DuplicateId(&'static str),
    #[error("贡献点非法：{0}")]
    Invalid(&'static str),
    #[error("清单声明 {declared} 个贡献点，实际成功注册 {registered} 个")]
    CapabilityMismatch { declared: usize, registered: usize },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn null_highlighter_is_noop() {
        let mut h = NullHighlighter;
        assert!(h.highlight("int main() {}", None).is_empty());
        h.reset();
        let del = TextEdit::between("abc", "").expect("有变更");
        assert!(h.highlight("", Some(del)).is_empty());
    }

    #[test]
    fn text_edit_is_copy_and_comparable() {
        let e = TextEdit::between("abc", "abd").expect("有变更");
        assert_eq!(e, e);
        assert_ne!(e, TextEdit::between("abc", "ab").expect("有变更"));
    }

    #[test]
    fn text_edit_between_detects_append() {
        let e = TextEdit::between("int a;", "int a; int b;").expect("有变更");
        assert_eq!(e.start, 6);
        assert_eq!(e.old_end, 6);
        assert_eq!(e.new_end, 13);
        assert_eq!(e.inserted(), 7);
        assert_eq!(e.removed(), 0);
        assert_eq!(e.start_point, Point { row: 0, column: 6 });
        assert_eq!(e.old_end_point, Point { row: 0, column: 6 });
        assert_eq!(e.new_end_point, Point { row: 0, column: 13 });
    }

    #[test]
    fn text_edit_between_detects_delete_and_replace() {
        // 删除中间一段
        let e = TextEdit::between("abcdef", "abef").expect("有变更");
        assert_eq!((e.start, e.old_end, e.new_end), (2, 4, 2));
        assert_eq!(e.removed(), 2);
        assert_eq!(e.inserted(), 0);
        // 替换一个字符
        let e = TextEdit::between("abc", "axc").expect("有变更");
        assert_eq!((e.start, e.old_end, e.new_end), (1, 2, 2));
    }

    #[test]
    fn text_edit_between_tracks_rows() {
        let old = "line1\nline2\n";
        let new = "line1\nline2\nline3\n";
        let e = TextEdit::between(old, new).expect("有变更");
        // 旧文本是新文本的前缀：整段新内容都是插入
        assert_eq!((e.start, e.old_end, e.new_end), (old.len(), old.len(), new.len()));
        assert_eq!(e.start_point, Point { row: 2, column: 0 });
        assert_eq!(e.new_end_point, Point { row: 3, column: 0 });
    }

    #[test]
    fn text_edit_between_respects_char_boundaries() {
        // 中文注释：前缀相同、中间替换，不能切在半个 UTF-8 字符上
        let old = "// 注释一\nint a;";
        let new = "// 注释二\nint a;";
        let e = TextEdit::between(old, new).expect("有变更");
        assert!(old.is_char_boundary(e.start));
        assert!(old.is_char_boundary(e.old_end));
        assert!(new.is_char_boundary(e.new_end));
        assert_eq!(&old[e.start..e.old_end], "一");
        assert_eq!(&new[e.start..e.new_end], "二");
    }

    #[test]
    fn text_edit_between_returns_none_when_unchanged() {
        assert!(TextEdit::between("same", "same").is_none());
        assert!(TextEdit::between("", "").is_none());
    }

    #[test]
    fn text_edit_between_handles_empty_sides() {
        let e = TextEdit::between("", "abc").expect("有变更");
        assert_eq!((e.start, e.old_end, e.new_end), (0, 0, 3));
        let e = TextEdit::between("abc", "").expect("有变更");
        assert_eq!((e.start, e.old_end, e.new_end), (0, 3, 0));
    }
}
