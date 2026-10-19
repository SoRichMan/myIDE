//! 多标签页模型（阶段 5）：纯逻辑，不依赖 UI。
//!
//! 每个标签持有自己的文本快照与光标位置；切换时由调用方在编辑器与标签之间
//! 搬运文本（`take_text` / `put_text`）。关闭带未保存修改的标签由 UI 层负责
//! 确认，本模块只提供 `dirty` 标记。
use std::path::PathBuf;

/// 一个打开的标签页。
#[derive(Debug, Clone, PartialEq)]
pub struct Tab {
    /// 显示标题（文件名或 "untitled-N"）。
    pub title: String,
    /// 磁盘路径；`None` 表示未命名新文件。
    pub path: Option<PathBuf>,
    /// 文本快照（切换标签时从编辑器暂存）。
    pub text: String,
    /// 未保存修改标记。
    pub dirty: bool,
    /// 光标位置（0-based 行/列）。
    pub cursor: (usize, usize),
    /// 是否已「锁定」（VS Code pinned）：`false` = 预览标签，下一次预览打开会顶替它。
    pub pinned: bool,
}

/// 由路径取显示标题（取不到文件名则用整条路径）。
fn file_title(path: &std::path::Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

/// 标签集合（有序）+ 当前激活序号。
#[derive(Debug, Clone, PartialEq)]
pub struct TabList {
    tabs: Vec<Tab>,
    active: usize,
}

/// 关闭标签的结果。
#[derive(Debug, Clone, PartialEq)]
pub enum CloseResult {
    /// 成功关闭。
    Closed,
    /// 标签有未保存修改，调用方应弹确认框。
    Dirty,
}

impl Default for TabList {
    fn default() -> Self {
        Self::new()
    }
}

impl TabList {
    pub fn new() -> Self {
        Self {
            tabs: Vec::new(),
            active: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.tabs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tabs.is_empty()
    }

    pub fn active(&self) -> usize {
        if self.tabs.is_empty() {
            return 0;
        }
        self.active.min(self.tabs.len() - 1)
    }

    pub fn get(&self, idx: usize) -> Option<&Tab> {
        self.tabs.get(idx)
    }

    pub fn get_mut(&mut self, idx: usize) -> Option<&mut Tab> {
        self.tabs.get_mut(idx)
    }

    /// 当前激活标签。
    pub fn active_tab(&self) -> Option<&Tab> {
        self.get(self.active())
    }

    pub fn iter(&self) -> std::slice::Iter<'_, Tab> {
        self.tabs.iter()
    }

    /// 未保存修改的标签数。
    pub fn dirty_count(&self) -> usize {
        self.tabs.iter().filter(|t| t.dirty).count()
    }

    /// 按路径查找标签；存在返回序号。
    pub fn index_of_path(&self, path: &std::path::Path) -> Option<usize> {
        self.tabs
            .iter()
            .position(|t| t.path.as_deref() == Some(path))
    }

    /// 打开（或激活）一个**锁定**标签。已打开 → 激活并锁定（预览标签在此“转正”）；
    /// 否则新建锁定标签并返回 `(idx, true)`。
    pub fn open(&mut self, path: PathBuf, text: String) -> (usize, bool) {
        if let Some(idx) = self.index_of_path(&path) {
            self.active = idx;
            self.pin(idx);
            return (idx, false);
        }
        let title = file_title(&path);
        self.tabs.push(Tab {
            title,
            path: Some(path),
            text,
            dirty: false,
            cursor: (0, 0),
            pinned: true,
        });
        self.active = self.tabs.len() - 1;
        (self.active, true)
    }

    /// 以「预览」方式打开：不锁定，会被下一次预览打开**顶替**（VS Code preview 语义）。
    ///
    /// - 已打开 → 仅激活（保持原有锁定状态，不降级为预览）；
    /// - 存在预览标签 → **原地替换**（保持位置，不新增标签）；
    /// - 否则新建预览标签。
    pub fn open_preview(&mut self, path: PathBuf, text: String) -> (usize, bool) {
        if let Some(idx) = self.index_of_path(&path) {
            self.active = idx;
            return (idx, false);
        }
        let title = file_title(&path);
        if let Some(idx) = self.preview_index() {
            if let Some(t) = self.tabs.get_mut(idx) {
                t.title = title;
                t.path = Some(path);
                t.text = text;
                t.dirty = false;
                t.cursor = (0, 0);
            }
            self.active = idx;
            return (idx, true);
        }
        self.tabs.push(Tab {
            title,
            path: Some(path),
            text,
            dirty: false,
            cursor: (0, 0),
            pinned: false,
        });
        self.active = self.tabs.len() - 1;
        (self.active, true)
    }

    /// 第一个预览（未锁定）标签的序号。
    pub fn preview_index(&self) -> Option<usize> {
        self.tabs.iter().position(|t| !t.pinned)
    }

    /// 指定标签是否处于预览（未锁定）状态。
    pub fn is_preview(&self, idx: usize) -> bool {
        self.tabs.get(idx).map(|t| !t.pinned).unwrap_or(false)
    }

    /// 锁定指定标签（双击标签页 / 在文件中编辑时调用）；已锁定返回 `false`。
    pub fn pin(&mut self, idx: usize) -> bool {
        match self.tabs.get_mut(idx) {
            Some(t) if !t.pinned => {
                t.pinned = true;
                true
            }
            _ => false,
        }
    }

    /// 新建未命名标签（始终锁定）。
    pub fn open_untitled(&mut self) -> usize {
        let n = self.tabs.iter().filter(|t| t.path.is_none()).count() + 1;
        self.tabs.push(Tab {
            title: format!("untitled-{n}"),
            path: None,
            text: String::new(),
            dirty: false,
            cursor: (0, 0),
            pinned: true,
        });
        self.active = self.tabs.len() - 1;
        self.active
    }

    /// 激活指定序号（越界忽略）。
    pub fn activate(&mut self, idx: usize) {
        if idx < self.tabs.len() {
            self.active = idx;
        }
    }

    /// 关闭指定标签（不检查 dirty；dirty 时返回 Dirty 且不关闭）。
    pub fn close(&mut self, idx: usize) -> CloseResult {
        if idx >= self.tabs.len() {
            return CloseResult::Closed;
        }
        if self.tabs[idx].dirty {
            return CloseResult::Dirty;
        }
        self.tabs.remove(idx);
        if self.tabs.is_empty() {
            self.active = 0;
        } else if self.active >= self.tabs.len() {
            self.active = self.tabs.len() - 1;
        } else if self.active > 0 && idx < self.active {
            self.active -= 1;
        } else if idx == self.active && self.active >= self.tabs.len() {
            self.active = self.tabs.len() - 1;
        }
        CloseResult::Closed
    }

    /// 关闭当前标签。
    pub fn close_active(&mut self) -> CloseResult {
        self.close(self.active())
    }

    /// 下一个标签（循环）。
    pub fn next(&mut self) {
        if self.tabs.len() > 1 {
            self.active = (self.active + 1) % self.tabs.len();
        }
    }

    /// 上一个标签（循环）。
    pub fn prev(&mut self) {
        if self.tabs.len() > 1 {
            self.active = if self.active == 0 {
                self.tabs.len() - 1
            } else {
                self.active - 1
            };
        }
    }

    /// 交换相邻标签（拖拽排序）：`idx` 向 `dir`（-1 左移 / +1 右移）。
    #[allow(dead_code)]
    pub fn swap_adjacent(&mut self, idx: usize, dir: isize) -> bool {
        let j = idx as isize + dir;
        if idx >= self.tabs.len() || j < 0 || j as usize >= self.tabs.len() {
            return false;
        }
        let j = j as usize;
        self.tabs.swap(idx, j);
        if self.active == idx {
            self.active = j;
        } else if self.active == j {
            self.active = idx;
        }
        true
    }

    /// 标记激活标签的脏状态。置脏时**自动锁定**，避免带未保存修改的预览标签被顶替。
    pub fn set_active_dirty(&mut self, dirty: bool) {
        let idx = self.active();
        if let Some(t) = self.tabs.get_mut(idx) {
            t.dirty = dirty;
            if dirty {
                t.pinned = true;
            }
        }
    }

    /// 把编辑器文本写回激活标签并记录光标。
    pub fn put_active_text(&mut self, text: String, cursor: (usize, usize)) {
        let idx = self.active();
        if let Some(t) = self.tabs.get_mut(idx) {
            t.text = text;
            t.cursor = cursor;
        }
    }

    /// 替换激活标签的文本（重新加载 / 未命名保存用）。
    pub fn replace_active_text(&mut self, text: String) {
        let idx = self.active();
        if let Some(t) = self.tabs.get_mut(idx) {
            t.text = text;
            t.dirty = false;
        }
    }

    /// 恢复一个已关闭的标签（追加到末尾并激活，强制锁定），返回其序号。
    pub fn restore(&mut self, mut tab: Tab) -> usize {
        tab.pinned = true;
        self.tabs.push(tab);
        self.active = self.tabs.len() - 1;
        self.active
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(n: &str) -> PathBuf {
        PathBuf::from(n)
    }

    #[test]
    fn open_dedups_by_path() {
        let mut ts = TabList::new();
        let (i0, is_new) = ts.open(path("a.cpp"), "text a".into());
        assert_eq!(i0, 0);
        assert!(is_new);
        let (i1, is_new) = ts.open(path("b.cpp"), "text b".into());
        assert_eq!(i1, 1);
        assert!(is_new);
        // 重复打开同一路径 → 激活已有，不新建
        let (i2, is_new) = ts.open(path("a.cpp"), "ignored".into());
        assert_eq!(i2, 0);
        assert!(!is_new);
        assert_eq!(ts.active(), 0);
        assert_eq!(ts.len(), 2);
    }

    #[test]
    fn untitled_gets_incrementing_names() {
        let mut ts = TabList::new();
        let i = ts.open_untitled();
        assert_eq!(ts.get(i).unwrap().title, "untitled-1");
        assert!(ts.get(i).unwrap().path.is_none());
        let j = ts.open_untitled();
        assert_eq!(ts.get(j).unwrap().title, "untitled-2");
    }

    #[test]
    fn close_adjusts_active_index() {
        let mut ts = TabList::new();
        ts.open(path("a.cpp"), "a".into());
        ts.open(path("b.cpp"), "b".into());
        ts.open(path("c.cpp"), "c".into());
        ts.activate(2);
        // 关闭激活的最后一个 → active 回退到 len-1
        assert_eq!(ts.close(2), CloseResult::Closed);
        assert_eq!(ts.active(), 1);
        // 关闭激活前面的 → active 前移
        assert_eq!(ts.close(0), CloseResult::Closed);
        assert_eq!(ts.active(), 0);
        assert_eq!(ts.len(), 1);
        // 关闭最后一个 → 空
        assert_eq!(ts.close(0), CloseResult::Closed);
        assert!(ts.is_empty());
        assert_eq!(ts.active(), 0);
    }

    #[test]
    fn dirty_tab_blocks_close() {
        let mut ts = TabList::new();
        ts.open(path("a.cpp"), "a".into());
        ts.set_active_dirty(true);
        assert_eq!(ts.close_active(), CloseResult::Dirty);
        assert_eq!(ts.len(), 1);
        ts.set_active_dirty(false);
        assert_eq!(ts.close_active(), CloseResult::Closed);
        assert!(ts.is_empty());
    }

    #[test]
    fn next_prev_wraps() {
        let mut ts = TabList::new();
        ts.open(path("a.cpp"), "a".into());
        ts.open(path("b.cpp"), "b".into());
        ts.open(path("c.cpp"), "c".into());
        ts.activate(0);
        ts.next();
        assert_eq!(ts.active(), 1);
        ts.next();
        assert_eq!(ts.active(), 2);
        ts.next();
        assert_eq!(ts.active(), 0); // 循环
        ts.prev();
        assert_eq!(ts.active(), 2);
        ts.prev();
        ts.prev();
        assert_eq!(ts.active(), 0);
    }

    #[test]
    fn swap_adjacent_moves_and_fixes_active() {
        let mut ts = TabList::new();
        ts.open(path("a.cpp"), "a".into());
        ts.open(path("b.cpp"), "b".into());
        ts.open(path("c.cpp"), "c".into());
        ts.activate(1); // b
        assert!(ts.swap_adjacent(1, -1));
        // 顺序: b, a, c；active 跟随 b → 0
        assert_eq!(ts.get(0).unwrap().title, "b.cpp");
        assert_eq!(ts.active(), 0);
        assert!(!ts.swap_adjacent(0, -1)); // 越界
        assert_eq!(ts.len(), 3);
    }

    #[test]
    fn put_and_take_active_text() {
        let mut ts = TabList::new();
        ts.open(path("a.cpp"), "old".into());
        ts.put_active_text("new".into(), (3, 5));
        assert_eq!(ts.active_tab().unwrap().text, "new");
        assert_eq!(ts.active_tab().unwrap().cursor, (3, 5));
        ts.replace_active_text("fresh".into());
        assert_eq!(ts.active_tab().unwrap().text, "fresh");
        assert!(!ts.active_tab().unwrap().dirty);
    }

    #[test]
    fn dirty_count_tracks_tabs() {
        let mut ts = TabList::new();
        ts.open(path("a.cpp"), "a".into());
        ts.set_active_dirty(true);
        ts.open(path("b.cpp"), "b".into());
        assert_eq!(ts.dirty_count(), 1);
        ts.set_active_dirty(true);
        assert_eq!(ts.dirty_count(), 2);
    }

    #[test]
    fn preview_open_replaces_previous_preview_in_place() {
        let mut ts = TabList::new();
        ts.open(path("keep.cpp"), "keep".into()); // 锁定
        let (i, _) = ts.open_preview(path("a.cpp"), "a".into());
        assert_eq!(i, 1);
        assert!(ts.is_preview(1));
        assert_eq!(ts.len(), 2);
        // 再预览打开另一个文件 → 原地顶替第一个预览（位置不变、不新增）
        let (j, _) = ts.open_preview(path("b.cpp"), "b".into());
        assert_eq!(j, 1);
        assert_eq!(ts.len(), 2);
        assert_eq!(ts.get(1).unwrap().title, "b.cpp");
        assert_eq!(ts.get(1).unwrap().text, "b");
        assert!(ts.is_preview(1));
        assert_eq!(ts.active(), 1);
    }

    #[test]
    fn preview_open_activates_already_open_tab_without_unpinning() {
        let mut ts = TabList::new();
        ts.open(path("a.cpp"), "a".into()); // 锁定
        ts.open(path("b.cpp"), "b".into());
        assert_eq!(ts.active(), 1);
        let (i, is_new) = ts.open_preview(path("a.cpp"), "ignored".into());
        assert_eq!(i, 0);
        assert!(!is_new);
        assert_eq!(ts.len(), 2);
        assert!(!ts.is_preview(0)); // 已锁定的不会被预览打开降级
    }

    #[test]
    fn open_pins_existing_preview() {
        let mut ts = TabList::new();
        ts.open_preview(path("a.cpp"), "a".into());
        assert!(ts.is_preview(0));
        let (i, is_new) = ts.open(path("a.cpp"), "a".into());
        assert_eq!(i, 0);
        assert!(!is_new);
        assert_eq!(ts.len(), 1);
        assert!(!ts.is_preview(0)); // 双击 / 永久打开 → 转正
    }

    #[test]
    fn pin_marks_and_reports_change() {
        let mut ts = TabList::new();
        ts.open(path("a.cpp"), "a".into()); // 已锁定
        assert!(!ts.pin(0));
        ts.open_preview(path("b.cpp"), "b".into());
        assert_eq!(ts.preview_index(), Some(1));
        assert!(ts.pin(1));
        assert_eq!(ts.preview_index(), None);
        assert!(!ts.pin(9)); // 越界
    }

    #[test]
    fn dirty_preview_is_auto_pinned_and_untitled_is_pinned() {
        let mut ts = TabList::new();
        ts.open_preview(path("a.cpp"), "a".into());
        ts.set_active_dirty(true);
        assert!(!ts.is_preview(0), "带未保存修改的预览标签应自动锁定");
        // 自动锁定后不会被下一次预览顶替
        ts.open_preview(path("b.cpp"), "b".into());
        assert_eq!(ts.len(), 2);
        assert_eq!(ts.get(0).unwrap().title, "a.cpp");
        // 新建未命名文件始终锁定
        let u = ts.open_untitled();
        assert!(!ts.is_preview(u));
    }

    #[test]
    fn restore_forces_pinned() {
        let mut ts = TabList::new();
        ts.open_preview(path("a.cpp"), "a".into());
        let mut t = ts.get(0).unwrap().clone();
        t.pinned = false;
        let idx = ts.restore(t);
        assert!(!ts.is_preview(idx));
    }
}
