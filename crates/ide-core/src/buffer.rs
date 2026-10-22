//! Rope 文本缓冲：O(log n) 插入/删除，带 LIFO 撤销/重做栈。
//!
//! 设计要点：
//! - 内部用 [`ropey::Rope`] 存储。
//! - 撤销/重做用“逆操作”记录：插入记 `(idx, text)`，删除记 `(idx, text)`，
//!   严格 LIFO 下记录的字符索引始终有效。
//! - 新编辑会清空 redo 栈（与主流编辑器一致）。
use ropey::Rope;
use std::ops::Range;

/// 单条 forward edit。
#[derive(Debug, Clone)]
enum Edit {
    /// 在 `char_idx` 处插入了 `text`。
    Inserted { char_idx: usize, text: String },
    /// 在 `char_idx` 处删除了 `text`。
    Deleted { char_idx: usize, text: String },
}

/// 最大撤销步数（myIDE.md 要求至少 100 步，这里给到 200）。
const MAX_UNDO: usize = 200;

/// 文本缓冲。所有索引均为“字符索引”（Unicode Scalar Value 计），不是字节索引。
#[derive(Debug)]
pub struct Buffer {
    rope: Rope,
    undo: Vec<Edit>,
    redo: Vec<Edit>,
}

impl Default for Buffer {
    fn default() -> Self {
        Self::new()
    }
}

impl Buffer {
    /// 空缓冲。
    pub fn new() -> Self {
        Self::from_text("")
    }

    /// 从文本构造缓冲。
    pub fn from_text(text: &str) -> Self {
        Self {
            rope: Rope::from_str(text),
            undo: Vec::with_capacity(16),
            redo: Vec::with_capacity(16),
        }
    }

    /// 字符总数。
    pub fn len_chars(&self) -> usize {
        self.rope.len_chars()
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.rope.len_chars() == 0
    }

    /// 行数。
    pub fn line_count(&self) -> usize {
        self.rope.len_lines()
    }

    /// 取第 `idx` 行内容（含末尾 `\n`），以 `String` 返回。
    pub fn line(&self, idx: usize) -> String {
        let n = self.line_count();
        let clamped = idx.min(n.saturating_sub(1));
        self.rope.line(clamped).to_string()
    }

    /// 第 `idx` 行的**字符迭代器**（不分配）。
    ///
    /// 高频路径（悬停取词、括号配对）用它代替 `line()`：后者每次都复制一行。
    pub fn line_chars(&self, idx: usize) -> impl Iterator<Item = char> + '_ {
        let n = self.line_count();
        let clamped = idx.min(n.saturating_sub(1));
        self.rope.line(clamped).chars()
    }

    /// 从 `char_idx` 开始的字符迭代器（不分配）。
    pub fn chars_from(&self, char_idx: usize) -> impl Iterator<Item = char> + '_ {
        let start = char_idx.min(self.len_chars());
        self.rope.slice(start, self.len_chars()).chars()
    }

    /// 字符索引 → `(行, 列)`。
    pub fn char_to_line_col(&self, char_idx: usize) -> (usize, usize) {
        let clamped = char_idx.min(self.len_chars());
        let line = self.rope.char_to_line(clamped);
        let line_start = self.rope.line_to_char(line);
        (line, clamped - line_start)
    }

    /// `(行, 列)` → 字符索引。
    pub fn line_col_to_char(&self, line: usize, col: usize) -> usize {
        let n = self.line_count();
        let l = line.min(n.saturating_sub(1));
        let line_start = self.rope.line_to_char(l);
        let line_chars = self.rope.line(l).len_chars();
        line_start + col.min(line_chars.saturating_sub(1))
    }

    /// 整个缓冲的字符串快照。
    pub fn text(&self) -> String {
        self.rope.to_string()
    }

    /// 在 `char_idx` 处插入文本。空文本为 no-op。
    pub fn insert(&mut self, char_idx: usize, text: &str) {
        if text.is_empty() {
            return;
        }
        let idx = char_idx.min(self.len_chars());
        self.rope.insert(idx, text);
        self.push_undo(Edit::Inserted {
            char_idx: idx,
            text: text.to_string(),
        });
    }

    /// 删除 `range` 区间内的字符。空/反向区间为 no-op。
    pub fn delete(&mut self, range: Range<usize>) {
        if range.start >= range.end {
            return;
        }
        let end = range.end.min(self.len_chars());
        if range.start >= end {
            return;
        }
        let text = self.rope.slice(range.start, end).to_string();
        self.rope.remove(range.start, end);
        self.push_undo(Edit::Deleted {
            char_idx: range.start,
            text,
        });
    }

    fn push_undo(&mut self, e: Edit) {
        self.undo.push(e);
        if self.undo.len() > MAX_UNDO {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    /// 撤销一步，返回撤销后应放置光标的字符索引。
    pub fn undo(&mut self) -> Option<usize> {
        let edit = self.undo.pop()?;
        let cursor = match edit {
            Edit::Inserted { char_idx, ref text } => {
                let end = char_idx + text.chars().count();
                self.rope.remove(char_idx, end);
                self.redo.push(edit);
                char_idx
            }
            Edit::Deleted { char_idx, ref text } => {
                self.rope.insert(char_idx, text);
                self.redo.push(edit);
                char_idx
            }
        };
        Some(cursor)
    }

    /// 重做一步，返回重做后应放置光标的字符索引。
    pub fn redo(&mut self) -> Option<usize> {
        let edit = self.redo.pop()?;
        let cursor = match edit {
            Edit::Inserted { char_idx, ref text } => {
                self.rope.insert(char_idx, text);
                let after = char_idx + text.chars().count();
                self.undo.push(edit);
                after
            }
            Edit::Deleted { char_idx, ref text } => {
                let end = char_idx + text.chars().count();
                self.rope.remove(char_idx, end);
                self.undo.push(edit);
                char_idx
            }
        };
        Some(cursor)
    }

    /// 当前 undo 栈深度。
    pub fn undo_depth(&self) -> usize {
        self.undo.len()
    }

    /// 当前 redo 栈深度。
    pub fn redo_depth(&self) -> usize {
        self.redo.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// `line_chars` / `chars_from`：不分配的行/字符迭代器。
    #[test]
    fn line_chars_and_chars_from_match_snapshot() {
        let b = Buffer::from_text("ab\ncdé\nf");
        assert_eq!(b.line_chars(0).collect::<String>(), "ab\n");
        assert_eq!(b.line_chars(1).collect::<String>(), "cdé\n");
        assert_eq!(b.line_chars(9).collect::<String>(), "f");
        assert_eq!(b.chars_from(0).collect::<String>(), "ab\ncdé\nf");
        assert_eq!(b.chars_from(3).collect::<String>(), "cdé\nf");
        // 越界要夹住，不能 panic。
        assert_eq!(b.chars_from(9999).count(), 0);
    }

    #[test]
    fn basic_insert_delete_and_undo_redo() {
        let mut b = Buffer::new();
        b.insert(0, "hello");
        assert_eq!(b.text(), "hello");
        b.insert(5, " world");
        assert_eq!(b.text(), "hello world");
        b.delete(5..11);
        assert_eq!(b.text(), "hello");

        assert_eq!(b.undo(), Some(5));
        assert_eq!(b.text(), "hello world");
        assert_eq!(b.undo(), Some(5));
        assert_eq!(b.text(), "hello");
        assert_eq!(b.undo(), Some(0));
        assert_eq!(b.text(), "");
        assert_eq!(b.undo(), None);

        assert_eq!(b.redo(), Some(5));
        assert_eq!(b.redo(), Some(11));
        assert_eq!(b.redo(), Some(5));
        assert_eq!(b.text(), "hello");
    }

    #[test]
    fn line_col_roundtrip() {
        let b = Buffer::from_text("abc\ndef\nghi");
        assert_eq!(b.line_count(), 3);
        assert_eq!(b.line(0), "abc\n");
        assert_eq!(b.line(1), "def\n");
        assert_eq!(b.char_to_line_col(5), (1, 1));
        assert_eq!(b.line_col_to_char(1, 1), 5);
        let _ = b.char_to_line_col(9999);
        let _ = b.line_col_to_char(9999, 9999);
    }

    #[test]
    fn new_edit_clears_redo() {
        let mut b = Buffer::new();
        b.insert(0, "a");
        b.undo();
        assert_eq!(b.redo_depth(), 1);
        b.insert(0, "b");
        assert_eq!(b.redo_depth(), 0);
    }

    #[test]
    fn undo_caps_at_200() {
        let mut b = Buffer::new();
        for i in 0..500 {
            b.insert(i % 2, "x");
        }
        assert!(b.undo_depth() <= 200);
    }

    /// proptest：随机操作序列后，Buffer 内容与 Vec<char> 一致。
    /// 200 cases × 500 ops ≈ 10 万次随机操作。
    #[derive(Debug, Clone)]
    enum Op {
        Insert { idx: usize, text: String },
        Delete { start: usize, end: usize },
    }

    fn op_sequence(len: usize) -> impl Strategy<Value = Vec<Op>> {
        let text = prop::collection::vec(prop::char::range('a', 'z'), 1..=8)
            .prop_map(|chars| chars.into_iter().collect::<String>());
        let insert = (0usize..16, text).prop_map(|(idx, text)| Op::Insert { idx, text });
        let delete = (0usize..16usize, 0usize..8usize).prop_map(|(a, n)| {
            let (start, end) = if a <= n { (a, a) } else { (a - n, a) };
            Op::Delete { start, end }
        });
        prop::collection::vec(prop_oneof![insert, delete], 0..=len)
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(200))]
        #[test]
        fn rope_matches_vec_char(ops in op_sequence(500)) {
            let mut b = Buffer::new();
            let mut v: Vec<char> = Vec::new();
            for op in ops {
                match op {
                    Op::Insert { idx, text } => {
                        let idx = idx.min(v.len());
                        b.insert(idx, &text);
                        let chars: Vec<char> = text.chars().collect();
                        v.splice(idx..idx, chars);
                    }
                    Op::Delete { start, end } => {
                        let start = start.min(v.len());
                        let end = end.min(v.len()).max(start);
                        b.delete(start..end);
                        v.drain(start..end);
                    }
                }
            }
            assert_eq!(b.text(), v.iter().collect::<String>());
        }
    }
}
