//! LiteIDE 编辑器组件：行号栏、当前行高亮、光标、选区、tree-sitter 语法高亮。
//!
//! 渲染策略：手动虚拟滚动——只画可视行，避免为 5000 行每行分配 widget。
//! 高亮区间来自 `ide_core::Highlighter`（字节偏移），文本脏时重算。
//!
//! 阶段 5：字号/行高/主题色改为动态（`set_typography` / `set_theme`），
//! 新增编辑命令（select_all / delete_line / toggle_line_comment / duplicate_line /
//! cut/copy/paste）与右侧 minimap。
//!
//! 复核改进：修复"选区画在文字上方遮挡文字"、"鼠标点击定位纵向偏移"两个渲染缺陷；
//! 新增断点行号栏红点（可点击）、当前行号高亮、缩进参考线、括号匹配高亮、
//! 水平滚动 + minimap 保留列（文本不再压到 minimap 底下）、查找/替换所需的字符级 API。
use ide_core::theme::{palette, ThemeKind};
use ide_core::{Buffer, HighlightGroup};
use ide_ext::{LanguageHighlighter, NullHighlighter, TextEdit, TokenPalette};
use std::ops::Range;

/// 断点红点色（VSCode Dark+ debug breakpoint）。
const BREAKPOINT_RED: egui::Color32 = egui::Color32::from_rgb(0xe5, 0x14, 0x00);

/// 一套 token 颜色：把主题扩展贡献的 [`TokenPalette`] 展开成**定长数组**。
///
/// 这是"配色可插拔"与"逐帧性能"的分界：扩展只在切换主题/创建编辑器时跑一次，
/// 绘制路径上只做一次数组下标访问（不解析字符串、不查表、不分配）。
#[derive(Debug, Clone, Copy)]
pub struct TokenColors {
    colors: [egui::Color32; HighlightGroup::COUNT],
}

impl TokenColors {
    /// 从扩展配色表展开（配合 `ExtensionHost` 的注册表使用）。
    pub fn from_palette(p: &TokenPalette) -> Self {
        let mut colors = [egui::Color32::GRAY; HighlightGroup::COUNT];
        for (group, hex_str) in p.pairs() {
            colors[group.index()] = hex(hex_str);
        }
        Self { colors }
    }

    /// 取分组颜色（无分支、无分配）。
    #[inline]
    pub fn get(&self, group: HighlightGroup) -> egui::Color32 {
        self.colors[group.index()]
    }
}

fn hex(s: &str) -> egui::Color32 {
    let s = s.trim_start_matches('#');
    let r = u8::from_str_radix(&s[0..2], 16).unwrap_or(0);
    let g = u8::from_str_radix(&s[2..4], 16).unwrap_or(0);
    let b = u8::from_str_radix(&s[4..6], 16).unwrap_or(0);
    egui::Color32::from_rgb(r, g, b)
}

const GUTTER_WIDTH: f32 = 56.0;
const GUTTER_PAD_RIGHT: f32 = 12.0;
/// 行号栏里断点红点的水平中心（相对 rect.min.x）。
const BREAKPOINT_CX: f32 = 12.0;
/// 正文区相对行号栏的左侧内边距。
const TEXT_PAD_LEFT: f32 = 8.0;
const MINIMAP_WIDTH: f32 = 48.0;
const INDENT_STEP: usize = 4;
const MIN_FONT_SIZE: f32 = 10.0;
const MAX_FONT_SIZE: f32 = 24.0;

/// 编辑器向应用层发出的动作（快捷键 + 命令面板共用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorAction {
    /// F12：跳转定义。
    GotoDefinition,
    /// Alt+Left：返回上一位置。
    GoBack,
    /// Ctrl+T：符号搜索。
    SymbolSearch,
    /// Ctrl+S：保存文件。
    Save,
    // ---- 阶段 3：调试 ----
    /// F5：运行/继续。
    DebugStart,
    /// Shift+F5：停止调试。
    DebugStop,
    /// F9：切换当前行断点。
    DebugToggleBreakpoint,
    /// 行号栏点击：切换指定行（0-based）断点。
    DebugToggleBreakpointAtLine(usize),
    /// F10：单步跳过。
    DebugNext,
    /// F11：单步进入。
    DebugStep,
    /// Shift+F11：单步跳出。
    DebugFinish,
    /// Ctrl+Shift+W：把光标所在单词加入监视（阶段 5 起从 Ctrl+W 迁移，避免与关闭标签冲突）。
    DebugAddWatch,
    // ---- 阶段 4：搜索 / Git / 设置 ----
    /// Ctrl+P：文件快速打开（模糊匹配）。
    FilePicker,
    /// Ctrl+Shift+F：内容搜索。
    ContentSearch,
    /// Ctrl+F：编辑器内查找。
    Find,
    /// Ctrl+H：编辑器内替换。
    Replace,
    /// Ctrl+Shift+G：Git 面板。
    GitPanel,
    /// Ctrl+,：设置。
    Settings,
    // ---- 阶段 5：标签 / 命令面板 / 视图 ----
    /// Ctrl+N：新建文件。
    NewFile,
    /// Ctrl+Shift+S：另存为。
    SaveAs,
    /// Ctrl+W：关闭当前标签。
    CloseTab,
    /// Ctrl+Tab：下一个标签。
    NextTab,
    /// Ctrl+Shift+Tab：上一个标签。
    PrevTab,
    /// Ctrl+Shift+P：命令面板。
    CommandPalette,
    /// Ctrl+B：切换侧边栏。
    ToggleSidebar,
    /// Ctrl+G：跳转到行。
    GotoLine,
    /// Ctrl+=：放大字号。
    ZoomIn,
    /// Ctrl+-：缩小字号。
    ZoomOut,
    /// Ctrl+0：重置字号。
    ResetZoom,
}

/// 主题相关面板色（从 `ide_core::theme::PaletteHex` 转换而来）。
#[derive(Debug, Clone, Copy)]
struct PanelColors {
    bg_editor: egui::Color32,
    bg_sidebar: egui::Color32,
    bg_currentline: egui::Color32,
    fg_linenumber: egui::Color32,
    selection: egui::Color32,
    cursor: egui::Color32,
    fg_plain: egui::Color32,
}

impl PanelColors {
    fn from_kind(kind: ThemeKind) -> Self {
        let p = palette(kind);
        Self {
            bg_editor: hex(p.bg_editor),
            bg_sidebar: hex(p.bg_sidebar),
            bg_currentline: hex(p.bg_currentline),
            fg_linenumber: hex(p.fg_linenumber),
            selection: hex(p.selection),
            cursor: hex(p.cursor),
            fg_plain: hex(p.fg_editor),
        }
    }
}

/// 编辑器状态。
pub struct Editor {
    buffer: Buffer,
    cursor: usize,
    selection: Option<Range<usize>>,
    scroll: f32,
    /// 水平滚动偏移（像素）。
    h_scroll: f32,
    /// 语言扩展提供的高亮器（默认空实现 = 无语法高亮）。
    highlighter: Box<dyn LanguageHighlighter>,
    highlights: Vec<ide_core::Highlight>,
    /// 整个文档的字符串快照（高亮区间是字节偏移，按行切片时用）。
    raw_text: String,
    /// 每行在 raw_text 里的字节起点。
    line_byte_starts: Vec<usize>,
    /// 最长行的字符数（水平滚动上限用）。
    max_cols: usize,
    /// 上一次渲染的视口高度（滚动到光标用）。
    viewport_h: f32,
    /// 当前文件断点行号（1-based，升序）。
    breakpoints: Vec<usize>,
    dirty: bool,
    /// 打开文件时记录加载耗时（状态栏显示）。
    load_ms: u128,
    /// 编辑序列号：每次文本变更 +1，供应用层做 didChange 去重。
    edit_seq: u64,
    /// 鼠标悬停所在字符（每帧更新）。
    mouse_char: Option<usize>,
    /// 鼠标悬停位置（绘制 hover 浮窗用）。
    mouse_pos: Option<egui::Pos2>,
    // ---- 阶段 5 ----
    font_size: f32,
    line_height: f32,
    kind: ThemeKind,
    colors: PanelColors,
    /// token 颜色（主题扩展贡献；默认 Dark+，由应用层按注册表覆盖）。
    tokens: TokenColors,
    minimap: bool,
    /// 模态窗口（命令面板/设置/搜索等）打开时置 false，屏蔽编辑器快捷键防冲突。
    keys_enabled: bool,
    /// 别的文本框（侧栏搜索框等）持有键盘焦点：只放行全局快捷键。
    text_focus: bool,
    // ---- 输入法（IME）----
    /// 组合串（预编辑拼音）内容；空 = 当前没有正在组合的输入。
    ///
    /// 组合期间把拼音串**直接插进缓冲区**渲染（而不是自绘 overlay）：排版、换行、高亮、
    /// 滚动全都复用现成逻辑，肉眼所见与 VS Code 一致；提交/取消/被打断时再替换或删除。
    /// 这里记下内容，是为了在撤销 / 替换全部等外部改动后能认出「跟踪已失效」，
    /// 宁可不动、也绝不误删正文。
    ime_preedit: String,
    /// 组合串在缓冲区里的起始**字符**下标。
    ime_preedit_start: usize,
}

impl Editor {
    /// 用初始文本构造。
    pub fn new(source: &str) -> Self {
        let t0 = std::time::Instant::now();
        let mut ed = Self {
            buffer: Buffer::from_text(source),
            cursor: 0,
            selection: None,
            scroll: 0.0,
            h_scroll: 0.0,
            highlighter: Box::new(NullHighlighter),
            highlights: Vec::new(),
            raw_text: String::new(),
            line_byte_starts: Vec::new(),
            max_cols: 0,
            viewport_h: 0.0,
            breakpoints: Vec::new(),
            dirty: true,
            load_ms: 0,
            edit_seq: 0,
            mouse_char: None,
            mouse_pos: None,
            font_size: 14.0,
            line_height: 21.0, // 14 * 1.5
            kind: ThemeKind::CppDark,
            colors: PanelColors::from_kind(ThemeKind::CppDark),
            tokens: TokenColors::from_palette(&ext_theme_vscode::CPPTOOLS_DARK),
            minimap: true,
            keys_enabled: true,
            text_focus: false,
            ime_preedit: String::new(),
            ime_preedit_start: 0,
        };
        ed.refresh_highlights();
        ed.load_ms = t0.elapsed().as_millis();
        ed
    }

    /// 用新文本整体替换当前内容，并把高亮器换成该文档对应的语言扩展
    /// （打开文件/切换标签走这里：**只高亮一次**）。
    pub fn open_document(&mut self, source: &str, highlighter: Box<dyn LanguageHighlighter>) {
        self.highlighter = highlighter;
        self.highlighter.reset();
        self.load_text(source);
    }

    /// 用新文本整体替换当前内容（打开/切换文件用），重置光标与滚动。
    pub fn load_text(&mut self, source: &str) {
        let t0 = std::time::Instant::now();
        self.buffer = Buffer::from_text(source);
        self.cursor = 0;
        self.selection = None;
        self.ime_preedit.clear();
        self.ime_preedit_start = 0;
        self.scroll = 0.0;
        self.h_scroll = 0.0;
        self.mark_edited();
        self.highlights = Vec::new();
        self.raw_text = String::new();
        self.line_byte_starts = Vec::new();
        self.max_cols = 0;
        // 换文档必须丢弃旧语法树：增量解析要求旧树与旧文本对应，
        // 跨文档复用会导致 tree-sitter 查询越界 panic。
        self.highlighter.reset();
        self.refresh_highlights();
        self.load_ms = t0.elapsed().as_millis();
    }

    /// 文本变更序列号（每次编辑 +1）。应用层据此判断是否需要 didChange / 脏标记。
    pub fn edit_seq(&self) -> u64 {
        self.edit_seq
    }

    /// 当前文档全文快照。
    pub fn text(&self) -> String {
        self.buffer.text()
    }

    /// 光标位置（0-based 行/列）。
    pub fn cursor_line_col(&self) -> (usize, usize) {
        self.buffer.char_to_line_col(self.cursor)
    }

    /// 任意字符索引 → (0-based 行, 列)。
    pub fn char_to_line_col(&self, char_idx: usize) -> (usize, usize) {
        self.buffer.char_to_line_col(char_idx)
    }

    /// 按 0-based 行列设置光标（越界自动钳制）。
    pub fn set_cursor_line_col(&mut self, line: usize, col: usize) {
        self.cursor = self.buffer.line_col_to_char(line, col);
        self.selection = None;
    }

    // ---- 阶段 5：排版 / 主题 / minimap ----

    /// 设置字号（钳制 10..=24），行高按 spec 固定 1.5 倍。
    pub fn set_typography(&mut self, font_size: f32) {
        let fs = font_size.clamp(MIN_FONT_SIZE, MAX_FONT_SIZE);
        self.font_size = fs;
        self.line_height = (fs * 1.5).round();
    }

    /// 切换主题：面板底色取自 core 的外壳色板，token 色取自主题扩展贡献的配色表。
    pub fn set_theme(&mut self, kind: ThemeKind, tokens: &TokenPalette) {
        self.kind = kind;
        self.colors = PanelColors::from_kind(kind);
        self.tokens = TokenColors::from_palette(tokens);
    }

    /// 设置语法高亮器（不传或没有匹配语言时用 [`NullHighlighter`] = 纯文本）。
    ///
    /// 由应用层在创建/切换文档时按扩展注册表注入，编辑器不认识具体语言。
    pub fn set_highlighter(&mut self, highlighter: Box<dyn LanguageHighlighter>) {
        self.highlighter = highlighter;
        self.highlighter.reset();
        self.refresh_highlights();
    }

    /// 开关 minimap。
    pub fn set_minimap(&mut self, on: bool) {
        self.minimap = on;
    }

    /// 设置当前文件的断点行（1-based，任意顺序）。
    pub fn set_breakpoints(&mut self, mut lines: Vec<usize>) {
        lines.sort_unstable();
        lines.dedup();
        self.breakpoints = lines;
    }

    /// 模态窗口打开时禁用编辑器快捷键（避免与面板抢键）。
    pub fn set_keys_enabled(&mut self, on: bool) {
        self.keys_enabled = on;
        if !on {
            // 模态窗口打开 = 编辑器不再接收输入：把临时拼音串撤掉，
            // 否则它会「变成」正文里的一段乱码。
            self.clear_ime_preedit();
        }
    }

    /// 其它文本框持有键盘焦点时，仅保留全局快捷键（见 `is_global_shortcut`）。
    pub fn set_text_focus(&mut self, on: bool) {
        self.text_focus = on;
        if on {
            // 输入法要交给那个文本框：临时拼音串先撤掉。
            self.clear_ime_preedit();
        }
    }

    // ---- 阶段 5：编辑命令（命令面板 / 快捷键共用） ----

    /// 全选。
    pub fn select_all(&mut self) {
        self.selection = Some(0..self.buffer.len_chars());
    }

    /// 选中当前行（与 VS Code Ctrl+L 一致，含行尾换行符）。
    pub fn select_line(&mut self) {
        let (line, _) = self.buffer.char_to_line_col(self.cursor);
        let start = self.buffer.line_col_to_char(line, 0);
        let end = start + self.buffer.line(line).chars().count();
        self.selection = Some(start..end);
    }

    /// 当前选中文本；无选区返回空。
    pub fn selection_text(&self) -> String {
        match self.selection_range() {
            Some(r) => self.buffer.text()[r.start..r.end].to_string(),
            None => String::new(),
        }
    }

    /// 当前选区（字符索引，已排序）；无选区返回 None。
    pub fn current_selection(&self) -> Option<(usize, usize)> {
        self.selection_range().map(|r| (r.start, r.end))
    }

    /// 直接设置选区（查找命中高亮用），光标落到选区末尾。
    pub fn select_range(&mut self, start: usize, end: usize) {
        let n = self.buffer.len_chars();
        let s = start.min(n);
        let e = end.min(n).max(s);
        self.selection = Some(s..e);
        self.cursor = e;
    }

    /// 把光标（无选区）移到指定字符索引，并把该行滚动到视口内。
    #[allow(dead_code)]
    pub fn goto_char(&mut self, idx: usize) {
        self.cursor = idx.min(self.buffer.len_chars());
        self.selection = None;
        self.scroll_to_char(self.cursor);
    }

    /// 查找所有匹配（大小写可选），返回字符区间（非重叠）。
    pub fn find_matches(&self, query: &str, case_sensitive: bool) -> Vec<(usize, usize)> {
        let needle: Vec<char> = query.chars().collect();
        if needle.is_empty() {
            return Vec::new();
        }
        let hay: Vec<char> = self.buffer.text().chars().collect();
        if needle.len() > hay.len() {
            return Vec::new();
        }
        let matches_at = |i: usize| {
            (0..needle.len()).all(|k| {
                if case_sensitive {
                    hay[i + k] == needle[k]
                } else {
                    hay[i + k].eq_ignore_ascii_case(&needle[k])
                }
            })
        };
        let mut out = Vec::new();
        let mut i = 0;
        while i + needle.len() <= hay.len() {
            if matches_at(i) {
                out.push((i, i + needle.len()));
                i += needle.len();
            } else {
                i += 1;
            }
        }
        out
    }

    /// 替换指定字符区间（查找/替换用），光标落到插入内容之后。
    pub fn replace_range(&mut self, start: usize, end: usize, text: &str) {
        let n = self.buffer.len_chars();
        let s = start.min(n);
        let e = end.min(n).max(s);
        self.buffer.delete(s..e);
        self.buffer.insert(s, text);
        self.cursor = s + text.chars().count();
        self.selection = None;
        self.mark_edited();
    }

    /// 复制选区到系统剪贴板。
    pub fn copy_to_clipboard(&self, ctx: &egui::Context) {
        let t = self.selection_text();
        if !t.is_empty() {
            ctx.output_mut(|o| o.copied_text = t);
        }
    }

    /// 剪切选区到系统剪贴板。
    pub fn cut_to_clipboard(&mut self, ctx: &egui::Context) {
        let t = self.selection_text();
        if !t.is_empty() {
            ctx.output_mut(|o| o.copied_text = t);
            self.delete_selection();
        }
    }

    /// 粘贴文本（过滤控制字符，保留换行/制表符）。
    pub fn paste_text(&mut self, text: &str) {
        let clean: String = text
            .chars()
            .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
            .collect();
        if clean.is_empty() {
            return;
        }
        self.delete_selection();
        self.buffer.insert(self.cursor, &clean);
        self.cursor += clean.chars().count();
        self.mark_edited();
    }

    /// 删除当前行（含行尾换行；末行无换行时仅删内容）。
    pub fn delete_line(&mut self) {
        let (line, _) = self.buffer.char_to_line_col(self.cursor);
        let start = self.buffer.line_col_to_char(line, 0);
        let line_text = self.buffer.line(line);
        let has_nl = line_text.ends_with('\n');
        let mut end = start + line_text.chars().count();
        // 空行且非末行：line() 可能不含换行，需补吃一个换行符。
        if !has_nl && line + 1 < self.buffer.line_count() {
            end += 1;
        }
        if end > start {
            self.buffer.delete(start..end);
            self.cursor = start.min(self.buffer.len_chars());
            self.mark_edited();
        }
    }

    /// 复制当前行（新副本插到原行上方，光标落到副本）。
    pub fn duplicate_line(&mut self) {
        let (line, _) = self.buffer.char_to_line_col(self.cursor);
        let start = self.buffer.line_col_to_char(line, 0);
        let line_text = self.buffer.line(line).to_string();
        if line_text.is_empty() && line + 1 >= self.buffer.line_count() {
            return; // 空文档末尾行不复制
        }
        let insert = format!("{line_text}\n");
        self.buffer.insert(start, &insert);
        self.cursor = start; // 光标落在新副本行首
        self.selection = None;
        self.mark_edited();
    }

    /// 切换当前行注释（行首 `//` ↔ 无注释）。
    pub fn toggle_line_comment(&mut self) {
        let (line, _) = self.buffer.char_to_line_col(self.cursor);
        let start = self.buffer.line_col_to_char(line, 0);
        let line_text = self.buffer.line(line);
        let trimmed = line_text.trim_start();
        if let Some(rest) = trimmed.strip_prefix("//") {
            // 取消注释：去掉行首空白后的 `//`；若后面紧跟空格一并删，保证往返切换。
            let prefix_len = line_text.len() - trimmed.len();
            let del_from = start + prefix_len;
            let mut end = del_from + 2;
            let chars: Vec<char> = line_text.chars().collect();
            if chars.get(prefix_len + 2) == Some(&' ') {
                end += 1;
            }
            let _ = rest;
            self.buffer.delete(del_from..end);
            self.mark_edited();
        } else {
            // 加注释：插到行首空白之后，保证与取消注释分支往返一致。
            let indent = line_text.len() - trimmed.len(); // 前缀为 ASCII 空白，字节数==字符数
            let at = start + indent;
            self.buffer.insert(at, "// ");
            self.mark_edited();
        }
    }

    /// 光标所在位置的单词（监视表达式用；非标识符字符为分隔符）。
    pub fn word_at_cursor(&self) -> Option<String> {
        let text = self.buffer.text();
        let bytes = text.as_bytes();
        let is_ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
        if self.cursor >= bytes.len() {
            return None;
        }
        // 向左扩展到词首
        let mut start = self.cursor;
        while start > 0 && is_ident(bytes[start - 1]) {
            start -= 1;
        }
        // 光标若落在分隔符上，取左侧词；否则取右侧词
        if start == self.cursor && self.cursor > 0 && !is_ident(bytes[self.cursor]) {
            let mut s = self.cursor;
            while s > 0 && is_ident(bytes[s - 1]) {
                s -= 1;
            }
            if s < self.cursor && bytes[s..self.cursor].iter().all(|&b| is_ident(b)) {
                start = s;
            }
        }
        let mut end = start;
        while end < bytes.len() && is_ident(bytes[end]) {
            end += 1;
        }
        if end > start {
            Some(text[start..end].to_string())
        } else {
            None
        }
    }

    /// 鼠标悬停所在字符索引（每帧由 `show` 更新）。
    pub fn mouse_char(&self) -> Option<usize> {
        self.mouse_char
    }

    /// 鼠标悬停位置。
    pub fn mouse_pos(&self) -> Option<egui::Pos2> {
        self.mouse_pos
    }

    fn mark_edited(&mut self) {
        self.dirty = true;
        self.edit_seq = self.edit_seq.wrapping_add(1);
    }

    #[allow(dead_code)]
    pub fn load_ms(&self) -> u128 {
        self.load_ms
    }

    #[allow(dead_code)]
    pub fn line_count(&self) -> usize {
        self.buffer.line_count()
    }

    /// 文本变更后调用：重建 raw_text / 行字节表 / 高亮。
    ///
    /// 高亮走"编辑描述 + 增量解析"：先算出新旧文本的最小差异交给语言扩展，
    /// tree-sitter 才能复用旧语法树（大文件打字延迟从数十毫秒降到亚毫秒级）。
    fn refresh_highlights(&mut self) {
        let new_text = self.buffer.text();
        let edit = TextEdit::between(&self.raw_text, &new_text);
        self.raw_text = new_text;
        self.line_byte_starts.clear();
        self.line_byte_starts.push(0);
        for (i, b) in self.raw_text.bytes().enumerate() {
            if b == b'\n' {
                self.line_byte_starts.push(i + 1);
            }
        }
        // 统计最长行显示宽度（tab = INDENT_STEP 格、CJK 按 2 格粗估）——水平滚动上限用。
        // 这里只求量级正确（精确像素宽度要问字体，成本太高），偏大一点只是能多滚一点点。
        self.max_cols = self
            .raw_text
            .split('\n')
            .map(|l| l.chars().map(char_cells).sum::<usize>())
            .max()
            .unwrap_or(0);
        self.highlights = self.highlighter.highlight(&self.raw_text, edit);
        // 按起点排序：渲染与 minimap 都用"推进游标"扫描高亮，依赖此有序性。
        self.highlights.sort_by_key(|h| h.start);
        self.dirty = false;
    }

    fn set_cursor(&mut self, char_idx: usize) {
        // 鼠标点到别处 = 打断输入法组合：先把临时拼音串撤掉，否则光标落点会算进组合串里。
        self.clear_ime_preedit();
        let idx = char_idx.min(self.buffer.len_chars());
        self.cursor = idx;
        self.selection = None;
    }

    fn selection_range(&self) -> Option<Range<usize>> {
        self.selection.as_ref().map(|r| {
            let s = r.start.min(r.end);
            let e = r.start.max(r.end);
            s..e
        })
    }

    fn delete_selection(&mut self) {
        if let Some(r) = self.selection_range() {
            self.buffer.delete(r.start..r.end);
            self.cursor = r.start;
            self.selection = None;
            self.mark_edited();
        }
    }

    fn insert_char(&mut self, ch: char) {
        self.delete_selection();
        self.buffer.insert(self.cursor, &ch.to_string());
        self.cursor += 1;
        self.mark_edited();
    }

    /// 撤销组合期间临时插进缓冲区的预编辑串（提交前 / 取消 / 被打断时都先走这里）。
    fn clear_ime_preedit(&mut self) {
        if self.ime_preedit.is_empty() {
            return;
        }
        let pre = std::mem::take(&mut self.ime_preedit);
        let start = self.ime_preedit_start.min(self.buffer.len_chars());
        let n = pre.chars().count();
        // 组合串不含换行（插入时过滤了控制字符）→ 只可能落在同一行里。
        // 先确认它还在（撤销 / 替换全部等外部改动会挪走它）：不在了就只丢跟踪，绝不误删。
        let (line, col) = self.buffer.char_to_line_col(start);
        let still_there = self
            .buffer
            .line(line)
            .chars()
            .skip(col)
            .take(n)
            .eq(pre.chars());
        if !still_there {
            return;
        }
        self.buffer.delete(start..start + n);
        self.cursor = start;
        self.selection = None;
        self.mark_edited();
    }

    /// 输入法预编辑（拼音串）：先撤掉上一版组合串，再插到光标处。
    fn ime_preedit(&mut self, text: &str) {
        self.clear_ime_preedit();
        let start = self.cursor;
        let mut composed = String::new();
        for ch in text.chars() {
            if ch.is_control() {
                continue;
            }
            self.insert_char(ch);
            composed.push(ch);
        }
        if !composed.is_empty() {
            self.ime_preedit = composed;
            self.ime_preedit_start = start;
        }
    }

    /// 输入法提交：丢掉预编辑串，把最终文本按一次正常编辑插入（可撤销）。
    fn ime_commit(&mut self, text: &str) {
        self.clear_ime_preedit();
        for ch in text.chars() {
            if ch.is_control() {
                continue;
            }
            self.insert_char(ch);
        }
    }

    fn backspace(&mut self) {
        if self.selection_range().is_some() {
            self.delete_selection();
            return;
        }
        if self.cursor > 0 {
            self.buffer.delete(self.cursor - 1..self.cursor);
            self.cursor -= 1;
            self.mark_edited();
        }
    }

    fn enter(&mut self) {
        self.insert_char('\n');
    }

    fn move_cursor_left(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            self.selection = None;
        }
    }

    fn move_cursor_right(&mut self) {
        if self.cursor < self.buffer.len_chars() {
            self.cursor += 1;
            self.selection = None;
        }
    }

    fn move_cursor_up(&mut self) {
        let (line, col) = self.buffer.char_to_line_col(self.cursor);
        if line > 0 {
            self.cursor = self.buffer.line_col_to_char(line - 1, col);
            self.selection = None;
        }
    }

    fn move_cursor_down(&mut self) {
        let (line, col) = self.buffer.char_to_line_col(self.cursor);
        if line + 1 < self.buffer.line_count() {
            self.cursor = self.buffer.line_col_to_char(line + 1, col);
            self.selection = None;
        }
    }

    fn home(&mut self) {
        let (line, _) = self.buffer.char_to_line_col(self.cursor);
        self.cursor = self.buffer.line_col_to_char(line, 0);
        self.selection = None;
    }

    fn end(&mut self) {
        let (line, _) = self.buffer.char_to_line_col(self.cursor);
        let line_text = self.buffer.line(line);
        let col = line_text.chars().count().saturating_sub(1);
        self.cursor = self.buffer.line_col_to_char(line, col);
        self.selection = None;
    }

    pub fn undo(&mut self) {
        if let Some(pos) = self.buffer.undo() {
            self.cursor = pos;
            self.selection = None;
            self.mark_edited();
        }
    }

    pub fn redo(&mut self) {
        if let Some(pos) = self.buffer.redo() {
            self.cursor = pos;
            self.selection = None;
            self.mark_edited();
        }
    }

    /// 把指定字符所在行滚动到视口内（查找跳转用）。
    pub fn scroll_to_char(&mut self, idx: usize) {
        let (line, _) = self
            .buffer
            .char_to_line_col(idx.min(self.buffer.len_chars()));
        let target = line as f32 * self.line_height - self.viewport_h * 0.4;
        self.scroll = target.max(0.0);
    }

    /// 屏幕纵坐标 → 行号（0-based）。
    fn line_at_y(&self, y: f32, rect_top: f32) -> Option<usize> {
        let rel = (y - rect_top + self.scroll) / self.line_height;
        if rel < 0.0 {
            return None;
        }
        let line = rel.floor() as usize;
        if line < self.buffer.line_count() {
            Some(line)
        } else {
            None
        }
    }

    /// 鼠标位置 → 字符索引（`rect_min` = 编辑器左上角屏幕坐标）。
    ///
    /// 按真实字形宽度反查（与 `text_advance` 同一套宽度），否则中文行上点击会偏。
    fn pos_to_char(
        &self,
        ui: &egui::Ui,
        font_id: &egui::FontId,
        pos: egui::Pos2,
        rect_min: egui::Pos2,
        space_w: f32,
    ) -> usize {
        let line_idx = self
            .line_at_y(pos.y, rect_min.y)
            .unwrap_or(self.buffer.line_count().saturating_sub(1));
        let line_start_char = self.buffer.line_col_to_char(line_idx, 0);
        let text_x0 = rect_min.x + GUTTER_WIDTH + TEXT_PAD_LEFT - self.h_scroll;
        let x_rel = (pos.x - text_x0).max(0.0);
        let line = self.buffer.line(line_idx);
        let tab_w = space_w * INDENT_STEP as f32;
        // 走一遍真实字形宽度（与绘制时同一套宽度），否则中文行上点击会偏。
        let col = ui.fonts(|f| {
            col_at_x(&line, x_rel, |c| {
                if c == '\t' {
                    tab_w
                } else {
                    f.glyph_width(font_id, c)
                }
            })
        });
        // 点到行尾之后：光标落到行末（行尾换行符之前），而不是跑到下一行行首。
        let max_col = line.chars().count() - usize::from(line.ends_with('\n'));
        line_start_char + col.min(max_col)
    }

    /// 光标附近若为括号，返回配对括号的 (行,列)（0-based）。
    fn bracket_pair(&self) -> Option<((usize, usize), (usize, usize))> {
        let (line, col) = self.buffer.char_to_line_col(self.cursor);
        let line_text = self.buffer.line(line).to_string();
        let chars: Vec<char> = line_text.chars().collect();
        // 优先取光标右侧字符，其次取左侧字符。
        let cand = [col, col.wrapping_sub(1)];
        for &c in &cand {
            let Some(&ch) = chars.get(c) else { continue };
            if !"()[]{}".contains(ch) {
                continue;
            }
            let (l, cc) = (line, c);
            // 在整文档字符数组中定位。
            let mut start = 0usize;
            for l0 in 0..l {
                start += self.buffer.line(l0).chars().count();
            }
            let idx = start + cc;
            let all: Vec<char> = self.buffer.text().chars().collect();
            if let Some(m) = find_matching_bracket(&all, idx) {
                return Some((self.buffer.char_to_line_col(idx), self.buffer.char_to_line_col(m)));
            }
        }
        None
    }

    /// 主渲染入口。返回本帧编辑器请求的动作（跳转/返回/搜索/保存），无则 `None`。
    pub fn show(&mut self, ui: &mut egui::Ui) -> Option<EditorAction> {
        if self.dirty {
            self.refresh_highlights();
        }

        let font_id = egui::FontId::monospace(self.font_size);
        let char_width = ui.fonts(|f| f.glyph_width(&font_id, ' ')).max(1.0);
        let rect = ui.max_rect();
        self.viewport_h = rect.height();
        let visible_lines = (rect.height() / self.line_height).ceil() as usize + 1;
        let first_line = ((self.scroll / self.line_height).floor() as usize)
            .min(self.buffer.line_count().saturating_sub(1));
        let last_line = (first_line + visible_lines).min(self.buffer.line_count());

        let id = ui.id().with("editor");
        let sense = egui::Sense::click_and_drag();
        let response = ui.interact(rect, id, sense);

        let mut action: Option<EditorAction> = None;
        let cursor_before = self.cursor;

        // 鼠标悬停跟踪（hover 用）
        self.mouse_char = response
            .hover_pos()
            .map(|pos| self.pos_to_char(ui, &font_id, pos, rect.min, char_width));
        self.mouse_pos = response.hover_pos();

        // 滚轮滚动：方向对齐 VSCode（向下滚 → 看后面的行）。
        // egui 的 smooth_scroll_delta 表示“内容应移动的方向”，故取负号：内容下移 = 视口上移。
        // Shift+滚轮：水平滚动。
        //
        // 注意：滚轮增量是全局输入，不区分控件。必须同时满足
        //   ① 指针确实在编辑区之上（`response.hovered()`，浮层遮挡时为 false）；
        //   ② 指针所在的最上层不是浮层（弹窗 / 菜单 / 工具提示）。
        // 否则在弹出窗口、侧栏、控制台里滚轮会连带滚动背后的编辑器。
        let wheel_here = response.hovered() && !pointer_over_float(ui.ctx());
        let (dy, dx, shift) = if wheel_here {
            ui.input(|i| (i.smooth_scroll_delta.y, i.smooth_scroll_delta.x, i.modifiers.shift))
        } else {
            (0.0, 0.0, false)
        };
        if shift && dy != 0.0 {
            self.h_scroll = (self.h_scroll - dy).max(0.0);
        } else {
            if dy != 0.0 {
                self.scroll = (self.scroll - dy).max(0.0);
            }
            if dx != 0.0 {
                self.h_scroll = (self.h_scroll - dx).max(0.0);
            }
        }
        let max_scroll = (self.buffer.line_count() as f32 * self.line_height).max(rect.height())
            - rect.height();
        self.scroll = self.scroll.clamp(0.0, max_scroll.max(0.0));

        let minimap_on = self.minimap && self.buffer.line_count() > 1;
        let minimap_w = if minimap_on { MINIMAP_WIDTH } else { 0.0 };
        let text_left = rect.min.x + GUTTER_WIDTH;
        let text_right = (rect.right() - minimap_w).max(text_left + 1.0);
        let text_x0 = text_left + TEXT_PAD_LEFT - self.h_scroll;
        let max_h = (self.max_cols as f32 * char_width + TEXT_PAD_LEFT * 2.0
            - (text_right - text_left))
            .max(0.0);
        self.h_scroll = self.h_scroll.clamp(0.0, max_h);

        // 点击/拖动：minimap → 跳转滚动；行号栏 → 切换断点；正文 → 定位光标/选择
        if response.clicked() || response.dragged() {
            if let Some(pos) = response.hover_pos() {
                if minimap_on && pos.x >= text_right {
                    self.seek_from_minimap(pos.y, rect);
                } else if response.clicked() && pos.x < text_left {
                    if let Some(line) = self.line_at_y(pos.y, rect.min.y) {
                        action = Some(EditorAction::DebugToggleBreakpointAtLine(line));
                    }
                } else if pos.x >= text_left {
                    let idx = self.pos_to_char(ui, &font_id, pos, rect.min, char_width);
                    if response.clicked() {
                        self.set_cursor(idx);
                    } else {
                        match &mut self.selection {
                            None => {
                                let anchor = self.cursor;
                                self.selection = Some(anchor..idx);
                            }
                            Some(r) => r.end = idx,
                        }
                    }
                }
            }
        }

        let painter = ui.painter();
        painter.rect_filled(rect, 0.0, self.colors.bg_editor);
        painter.rect_filled(
            egui::Rect::from_min_size(rect.min, egui::vec2(GUTTER_WIDTH, rect.height())),
            0.0,
            self.colors.bg_sidebar,
        );
        let text_clip = egui::Rect::from_min_max(
            egui::pos2(text_left, rect.top()),
            egui::pos2(text_right, rect.bottom()),
        );
        let text_painter = painter.with_clip_rect(text_clip);

        let cursor_line = self.buffer.char_to_line_col(self.cursor).0;
        let sel = self.selection_range();
        let guide_color = self.colors.fg_linenumber.gamma_multiply(0.28);
        let bp_color = BREAKPOINT_RED;

        let mut hi_scan = 0usize;
        for li in first_line..last_line {
            let y = rect.min.y + li as f32 * self.line_height - self.scroll;

            // 当前行高亮条
            if li == cursor_line {
                painter.rect_filled(
                    egui::Rect::from_min_size(
                        egui::pos2(rect.min.x, y),
                        egui::vec2(rect.width(), self.line_height),
                    ),
                    0.0,
                    self.colors.bg_currentline,
                );
            }

            // 行号（当前行号高亮为正文色）
            let num_color = if li == cursor_line {
                self.colors.fg_plain
            } else {
                self.colors.fg_linenumber
            };
            painter.text(
                egui::pos2(
                    rect.min.x + GUTTER_WIDTH - GUTTER_PAD_RIGHT,
                    y + self.line_height * 0.5,
                ),
                egui::Align2::RIGHT_CENTER,
                format!("{}", li + 1),
                font_id.clone(),
                num_color,
            );

            // 断点红点
            if self.breakpoints.binary_search(&(li + 1)).is_ok() {
                painter.circle_filled(
                    egui::pos2(rect.min.x + BREAKPOINT_CX, y + self.line_height * 0.5),
                    4.5,
                    bp_color,
                );
            }

            let line_str = self.buffer.line(li);

            // 缩进参考线（画在文字下方，裁剪到正文区）
            let indent = indent_columns(&line_str);
            let mut k = INDENT_STEP;
            while k <= indent {
                let x = text_x0 + k as f32 * char_width;
                if x >= text_left && x <= text_right {
                    text_painter.line_segment(
                        [
                            egui::pos2(x, y),
                            egui::pos2(x, y + self.line_height),
                        ],
                        egui::Stroke::new(1.0_f32, guide_color),
                    );
                }
                k += INDENT_STEP;
            }

            // 选区（必须先于文字绘制，否则会盖住代码）
            if let Some(sel) = &sel {
                let line_start_char = self.buffer.line_col_to_char(li, 0);
                let line_end_char = line_start_char + line_str.chars().count();
                if sel.start < line_end_char && sel.end > line_start_char {
                    let s_col = sel.start.saturating_sub(line_start_char);
                    let e_col = sel.end.min(line_end_char) - line_start_char;
                    let x0 = text_x0 + text_advance(ui, &font_id, &line_str, s_col, char_width);
                    let x1 = text_x0 + text_advance(ui, &font_id, &line_str, e_col, char_width);
                    text_painter.rect_filled(
                        egui::Rect::from_min_max(
                            egui::pos2(x0, y),
                            egui::pos2(x1, y + self.line_height),
                        ),
                        0.0,
                        self.colors.selection,
                    );
                }
            }

            // 语法高亮文本（**去掉行尾换行**，见 `line_span_no_eol`：否则 galley 会多出一行、
            // 正文顶部对齐、与光标/选中高亮垂直错位）
            let (line_byte_start, line_byte_end) = line_span_no_eol(
                &self.raw_text,
                self.line_byte_starts.get(li).copied().unwrap_or(0),
                self.line_byte_starts
                    .get(li + 1)
                    .copied()
                    .unwrap_or(self.raw_text.len()),
            );

            let mut segs: Vec<(usize, usize, HighlightGroup)> = Vec::new();
            // 高亮区间按起点升序，用游标推进 O(可见行 + 区间命中)。
            while hi_scan < self.highlights.len() && self.highlights[hi_scan].end <= line_byte_start {
                hi_scan += 1;
            }
            let mut j = hi_scan;
            while j < self.highlights.len() && self.highlights[j].start < line_byte_end {
                let h = &self.highlights[j];
                if h.end > line_byte_start {
                    segs.push((
                        h.start.max(line_byte_start) - line_byte_start,
                        h.end.min(line_byte_end) - line_byte_start,
                        h.group,
                    ));
                }
                j += 1;
            }

            let mut job = egui::text::LayoutJob::default();
            let mut cursor_byte = 0usize;
            let line_bytes = &self.raw_text[line_byte_start..line_byte_end];
            let mut seg_iter = segs.into_iter().peekable();
            while cursor_byte < line_bytes.len() {
                if let Some((s, e, g)) = seg_iter.peek() {
                    if *s == cursor_byte {
                        let chunk = &line_bytes[*s..*e];
                        job.append(
                            chunk,
                            0.0,
                            egui::TextFormat::simple(font_id.clone(), self.tokens.get(*g)),
                        );
                        cursor_byte = *e;
                        seg_iter.next();
                        continue;
                    }
                }
                let next_start =
                    seg_iter.peek().map(|(s, _, _)| *s).unwrap_or(line_bytes.len());
                let take = next_start - cursor_byte;
                let chunk = &line_bytes[cursor_byte..cursor_byte + take];
                job.append(
                    chunk,
                    0.0,
                    egui::TextFormat::simple(font_id.clone(), self.colors.fg_plain),
                );
                cursor_byte += take;
            }
            let galley = text_painter.layout_job(job);
            // 在行高条带里垂直居中，保证代码与选中高亮/光标对齐。
            let ty = y + ((self.line_height - galley.size().y) * 0.5).max(0.0);
            text_painter.galley(egui::pos2(text_x0, ty), galley, self.colors.fg_plain);

            // 输入法组合串加下划线（提示「这段还没落定」，和 VS Code 一致）。
            if !self.ime_preedit.is_empty() {
                let start = self.ime_preedit_start;
                let end = start + self.ime_preedit.chars().count();
                let line_start_char = self.buffer.line_col_to_char(li, 0);
                let line_end_char = line_start_char + line_str.chars().count();
                if start < line_end_char && end > line_start_char {
                    let c0 = start.saturating_sub(line_start_char);
                    let c1 = end.min(line_end_char) - line_start_char;
                    let x0 = text_x0 + text_advance(ui, &font_id, &line_str, c0, char_width);
                    let x1 = text_x0
                        + text_advance(ui, &font_id, &line_str, c1.max(c0 + 1), char_width);
                    text_painter.rect_filled(
                        egui::Rect::from_min_max(
                            egui::pos2(x0, y + self.line_height - 2.0),
                            egui::pos2(x1.min(text_right), y + self.line_height - 0.5),
                        ),
                        0.0,
                        self.colors.fg_plain,
                    );
                }
            }
        }

        // 括号匹配高亮（描边框，不遮字）
        if let Some(((l1, c1), (l2, c2))) = self.bracket_pair() {
            let box_color = self.colors.fg_linenumber.gamma_multiply(0.9);
            for (l, c) in [(l1, c1), (l2, c2)] {
                if l < first_line || l >= last_line {
                    continue;
                }
                let line_txt = self.buffer.line(l);
                let x = text_x0 + text_advance(ui, &font_id, &line_txt, c, char_width);
                let ch = line_txt.chars().nth(c).unwrap_or(' ');
                let bw = ui.fonts(|f| f.glyph_width(&font_id, ch));
                let y = rect.min.y + l as f32 * self.line_height - self.scroll;
                text_painter.rect_stroke(
                    egui::Rect::from_min_size(
                        egui::pos2(x, y),
                        egui::vec2(bw.max(6.0), self.line_height),
                    ),
                    1.0,
                    egui::Stroke::new(1.0_f32, box_color),
                );
            }
        }

        // minimap：按行渲染缩略代码图（行宽/缩进 + token 色），可点击/拖动跳转
        if minimap_on {
            let n = self.buffer.line_count();
            let mm_rect = egui::Rect::from_min_max(
                egui::pos2(rect.right() - MINIMAP_WIDTH, rect.top()),
                rect.right_bottom(),
            );
            // 背景与编辑器同色，左侧一条细分隔线
            painter.rect_filled(mm_rect, 0.0, self.colors.bg_editor);
            painter.rect_filled(
                egui::Rect::from_min_max(
                    mm_rect.min,
                    egui::pos2(mm_rect.left() + 1.0, mm_rect.bottom()),
                ),
                0.0,
                self.colors.fg_linenumber.gamma_multiply(0.25),
            );
            let mm_painter = painter.with_clip_rect(mm_rect);
            let per = mm_rect.height() / n as f32;
            let row_h = per.max(1.0);
            let pad = 3.0;
            let usable_w = (mm_rect.width() - pad * 2.0).max(1.0);
            let cols = self.max_cols.max(1) as f32;
            // 高亮区间按起点升序，用游标 O(行数+区间数) 取每行首色，避免逐行扫描全部区间。
            let mut hi = 0usize;
            let mut last_y = f32::NEG_INFINITY;
            for li in 0..n {
                let (ls, le) = line_span_no_eol(
                    &self.raw_text,
                    self.line_byte_starts.get(li).copied().unwrap_or(0),
                    self.line_byte_starts
                        .get(li + 1)
                        .copied()
                        .unwrap_or(self.raw_text.len()),
                );
                while hi < self.highlights.len() && self.highlights[hi].end <= ls {
                    hi += 1;
                }
                let color = match self.highlights.get(hi) {
                    Some(h) if h.start < le => self.tokens.get(h.group),
                    _ => self.colors.fg_plain,
                };
                let y = mm_rect.top() + li as f32 * per;
                if y - last_y < 1.0 {
                    continue; // 同一像素行已画过
                }
                last_y = y;
                if le <= ls {
                    continue; // 空行
                }
                let line = &self.raw_text[ls..le];
                let mut lead_cols = 0usize;
                let mut lead_chars = 0usize;
                for ch in line.chars() {
                    match ch {
                        ' ' => {
                            lead_cols += 1;
                            lead_chars += 1;
                        }
                        '\t' => {
                            lead_cols += INDENT_STEP;
                            lead_chars += 1;
                        }
                        _ => break,
                    }
                }
                let len = lead_cols + line.chars().skip(lead_chars).map(char_cells).sum::<usize>();
                let x0 = mm_rect.left() + pad + (lead_cols as f32 / cols) * usable_w;
                let x1 = mm_rect.left() + pad + (len as f32 / cols) * usable_w;
                mm_painter.rect_filled(
                    egui::Rect::from_min_max(
                        egui::pos2(x0, y),
                        egui::pos2(x1.max(x0 + 1.0), y + row_h),
                    ),
                    0.0,
                    color.gamma_multiply(0.8),
                );
            }
            // 视口指示块
            let total_h = (n as f32 * self.line_height).max(1.0);
            let vh = (rect.height() / total_h * mm_rect.height()).clamp(16.0, mm_rect.height());
            let vy = mm_rect.top() + (self.scroll / total_h) * mm_rect.height();
            let vp = egui::Rect::from_min_size(
                egui::pos2(mm_rect.left(), vy),
                egui::vec2(mm_rect.width(), vh),
            );
            painter.rect_filled(vp, 0.0, self.colors.fg_linenumber.gamma_multiply(0.08));
            painter.rect_stroke(
                vp,
                0.0,
                egui::Stroke::new(1.0_f32, self.colors.fg_linenumber.gamma_multiply(0.7)),
            );
        }

        // 光标竖线（含水平滚动/保持可见）
        //
        // 列 → 像素必须走 `text_advance`（真实字形宽度）：中文一个字约 1.65 个等宽格，
        // 按「列数 × 格宽」算会让光标停在汉字中间，打字时光标跟不上字。
        let (cur_line, cur_col) = self.buffer.char_to_line_col(self.cursor);
        let cur_line_text = self.buffer.line(cur_line);
        let caret_dx = text_advance(ui, &font_id, &cur_line_text, cur_col, char_width);
        let mut cursor_x = text_left + TEXT_PAD_LEFT + caret_dx - self.h_scroll;
        if cursor_x < text_left {
            self.h_scroll -= text_left - cursor_x;
        } else if cursor_x + char_width > text_right {
            self.h_scroll += cursor_x + char_width - text_right;
        }
        self.h_scroll = self.h_scroll.clamp(0.0, max_h);
        cursor_x = (text_left + TEXT_PAD_LEFT + caret_dx - self.h_scroll)
            .clamp(text_left, text_right - 1.0);
        let cursor_y = rect.min.y + cur_line as f32 * self.line_height - self.scroll;
        painter.rect_filled(
            egui::Rect::from_min_size(
                egui::pos2(cursor_x, cursor_y + 1.0),
                egui::vec2(2.0, self.line_height - 2.0),
            ),
            0.0,
            self.colors.cursor,
        );

        // 输入法（IME）通道：编辑器是自绘文本通道，egui 只会替内置 `TextEdit` 上报
        // 候选窗位置；这里必须自己上报 —— egui-winit 看到 `output.ime == Some(..)` 才会
        // `set_ime_allowed(true)` 并把候选窗锚到光标处。不上报 = 窗口的输入法一直是关的，
        // 表现就是「切换输入法没反应、中文一个字也进不来」。
        //
        // 只上报**光标处的一小块**（而不是整个编辑区）：egui-winit 会把它当作
        // `CFS_EXCLUDE` 区交给输入法，区域一大候选窗就被挤到屏幕边缘去了。
        // 焦点在别的文本框 / 模态窗口打开时不抢（交给那边上报）。
        if self.keys_enabled && !self.text_focus {
            let caret = egui::Rect::from_min_size(
                egui::pos2(cursor_x, cursor_y),
                egui::vec2(char_width.max(2.0), self.line_height),
            );
            ui.ctx().output_mut(|o| {
                o.ime = Some(egui::output::IMEOutput {
                    rect: caret,
                    cursor_rect: caret,
                });
            });
        }

        // 键盘输入（模态窗口打开时不处理，避免快捷键冲突）
        let events = ui.input(|i| i.events.clone());
        if self.keys_enabled {
            for ev in &events {
                // 焦点在别的文本框时：只放行全局快捷键，其余（字符输入 / 退格 / 回车 / 方向键…）
                // 交给那个文本框，别同时改到正文。
                if self.text_focus && !is_global_shortcut(ev) {
                    continue;
                }
                // 组合中被会改动正文/光标的按键或字符事件打断时，先撤掉临时拼音串，
                // 免得它被留在正文里或把光标算错（字母键不打断 —— 输入法只吞掉「字符消息」，
                // `WM_KEYDOWN` 仍会送进来，若因此打断，拼音串就永远显示不出来）。
                if !self.ime_preedit.is_empty() && breaks_ime_composition(ev) {
                    self.clear_ime_preedit();
                }
                match ev {
                    egui::Event::Text(text) => {
                        for ch in text.chars() {
                            if ch.is_control() {
                                continue;
                            }
                            self.insert_char(ch);
                        }
                    }
                    // Windows IME（微软拼音等）在组合中**不会**产生 `Event::Text`：
                    // 拼音串走 Preedit、选中的汉字走 Commit。自绘编辑器必须自己处理，
                    // 否则「输入法切了也没反应」。
                    egui::Event::Ime(ime) => match ime {
                        egui::ImeEvent::Commit(text) => self.ime_commit(text),
                        egui::ImeEvent::Preedit(text) => self.ime_preedit(text),
                        egui::ImeEvent::Disabled => self.clear_ime_preedit(),
                        egui::ImeEvent::Enabled => {}
                    },
                    egui::Event::Paste(text) => {
                        self.paste_text(text);
                    }
                    egui::Event::Key {
                        key,
                        pressed: true,
                        modifiers,
                        ..
                    } => {
                        use egui::Key;
                        match (key, modifiers.command, modifiers.shift) {
                            // 阶段 2 快捷键（守卫分支优先）
                            (Key::ArrowLeft, _, _) if modifiers.alt => {
                                action = Some(EditorAction::GoBack);
                            }
                            (Key::F12, _, _) => action = Some(EditorAction::GotoDefinition),
                            (Key::T, true, false) => action = Some(EditorAction::SymbolSearch),
                            (Key::S, true, false) => action = Some(EditorAction::Save),
                            // 阶段 4 快捷键
                            (Key::P, true, false) => action = Some(EditorAction::FilePicker),
                            (Key::F, true, false) => action = Some(EditorAction::Find),
                            (Key::F, true, true) => action = Some(EditorAction::ContentSearch),
                            (Key::H, true, false) => action = Some(EditorAction::Replace),
                            (Key::G, true, true) => action = Some(EditorAction::GitPanel),
                            (Key::Comma, true, _) => action = Some(EditorAction::Settings),
                            // 阶段 5 快捷键
                            (Key::N, true, false) => action = Some(EditorAction::NewFile),
                            (Key::S, true, true) => action = Some(EditorAction::SaveAs),
                            (Key::W, true, false) => action = Some(EditorAction::CloseTab),
                            (Key::W, true, true) => action = Some(EditorAction::DebugAddWatch),
                            (Key::Tab, true, false) => action = Some(EditorAction::NextTab),
                            (Key::Tab, true, true) => action = Some(EditorAction::PrevTab),
                            (Key::P, true, true) => action = Some(EditorAction::CommandPalette),
                            (Key::B, true, false) => action = Some(EditorAction::ToggleSidebar),
                            (Key::G, true, false) => action = Some(EditorAction::GotoLine),
                            (Key::Equals, true, _) => action = Some(EditorAction::ZoomIn),
                            (Key::Minus, true, _) => action = Some(EditorAction::ZoomOut),
                            (Key::Num0, true, _) => action = Some(EditorAction::ResetZoom),
                            // 阶段 3 调试快捷键
                            (Key::F5, _, false) => action = Some(EditorAction::DebugStart),
                            (Key::F5, _, true) => action = Some(EditorAction::DebugStop),
                            (Key::F9, _, _) => action = Some(EditorAction::DebugToggleBreakpoint),
                            (Key::F10, _, _) => action = Some(EditorAction::DebugNext),
                            (Key::F11, _, false) => action = Some(EditorAction::DebugStep),
                            (Key::F11, _, true) => action = Some(EditorAction::DebugFinish),
                            // 编辑器内部命令（无需上抛应用层）
                            (Key::A, true, false) => self.select_all(),
                            (Key::C, true, false) => self.copy_to_clipboard(ui.ctx()),
                            (Key::X, true, false) => self.cut_to_clipboard(ui.ctx()),
                            (Key::V, true, false) => {} // 粘贴走 Event::Paste
                            (Key::Z, true, false) => self.undo(),
                            (Key::Z, true, true) => self.redo(),
                            (Key::Y, true, false) => self.redo(),
                            (Key::K, true, true) => self.delete_line(),
                            (Key::L, true, false) => self.select_line(),
                            (Key::Slash, true, false) => self.toggle_line_comment(),
                            (Key::D, true, true) => self.duplicate_line(),
                            (Key::Backspace, _, _) => self.backspace(),
                            (Key::Enter, _, _) => self.enter(),
                            (Key::ArrowLeft, _, _) => self.move_cursor_left(),
                            (Key::ArrowRight, _, _) => self.move_cursor_right(),
                            (Key::ArrowUp, _, _) => self.move_cursor_up(),
                            (Key::ArrowDown, _, _) => self.move_cursor_down(),
                            (Key::Home, _, _) => self.home(),
                            (Key::End, _, _) => self.end(),
                            _ => {}
                        }
                    }
                    _ => {}
                }
            }
        }

        response.changed();
        // 光标因键盘/编辑移动时，才保证其可见（滚轮浏览时不强制拉回 → 不再抖动）。
        if self.cursor != cursor_before {
            self.ensure_cursor_visible(rect);
        }
        action
    }

    /// 仅当光标移出可视区时才滚动（供键盘/编辑用）。
    fn ensure_cursor_visible(&mut self, rect: egui::Rect) {
        let (cur_line, _) = self.buffer.char_to_line_col(self.cursor);
        let first = (self.scroll / self.line_height).floor() as usize;
        let visible = (rect.height() / self.line_height).ceil() as usize + 1;
        if cur_line < first {
            self.scroll = cur_line as f32 * self.line_height;
        } else if cur_line + 1 > first + visible {
            self.scroll = (cur_line + 1) as f32 * self.line_height - rect.height();
        }
        let max =
            (self.buffer.line_count() as f32 * self.line_height).max(rect.height()) - rect.height();
        self.scroll = self.scroll.clamp(0.0, max.max(0.0));
    }

    /// minimap 点击/拖动 → 按纵向比例设置滚动位置。
    fn seek_from_minimap(&mut self, y: f32, rect: egui::Rect) {
        let n = self.buffer.line_count();
        let total_h = (n as f32 * self.line_height).max(rect.height()).max(1.0);
        let frac = ((y - rect.top()) / rect.height()).clamp(0.0, 1.0);
        let max = (total_h - rect.height()).max(0.0);
        self.scroll = (frac * total_h - rect.height() * 0.5).clamp(0.0, max);
    }

    /// 状态栏文本。
    pub fn status_text(&self) -> String {
        let (line, col) = self.buffer.char_to_line_col(self.cursor);
        format!(
            "Ln {}, Col {}  |  {} lines  |  load {} ms",
            line + 1,
            col + 1,
            self.buffer.line_count(),
            self.load_ms
        )
    }
}

/// 一行前导空白的显示列数（tab 记为 `INDENT_STEP` 列）。
fn indent_columns(line: &str) -> usize {
    let mut col = 0;
    for ch in line.chars() {
        match ch {
            ' ' => col += 1,
            '\t' => col += INDENT_STEP,
            _ => break,
        }
    }
    col
}

/// 一行前 `n_chars` 个字符的显示宽度：宽度由 `glyph_w` 提供（便于注入假字体度量做单测）。
///
/// 关键：不能按「字符数 × 等宽格」估算。正文用的等宽字体（Hack）一格约 8.28px，
/// 而中文回退字体（微软雅黑）一个汉字约 13.64px ≈ 1.65 格 —— 于是中文行上
/// 「第 n 列 = n 个格子」会把光标画到汉字中间去。
fn advance_with(line: &str, n_chars: usize, glyph_w: impl Fn(char) -> f32) -> f32 {
    line.chars().take(n_chars).map(glyph_w).sum()
}

/// [`advance_with`] 的反函数：横向偏移 `x` 落在第几个字符上（落在字符前半段即算该字符）。
fn col_at_x(line: &str, x: f32, glyph_w: impl Fn(char) -> f32) -> usize {
    let mut acc = 0.0f32;
    let mut col = 0usize;
    for c in line.chars() {
        let w = glyph_w(c);
        if x < acc + w * 0.5 {
            break;
        }
        acc += w;
        col += 1;
    }
    col
}

/// 一行前 `n_chars` 个字符在屏幕上占的**像素**宽度（真实字形宽度）。
///
/// 光标 / 选区 / 组合串下划线 / 点击定位全部走这条路径，中文与 ASCII 都对得上。
/// tab 记作 `space_w * INDENT_STEP`：与 epaint 的 `TAB_SIZE`（也是 4）和 `indent_columns` 一致。
fn text_advance(
    ui: &egui::Ui,
    font_id: &egui::FontId,
    line: &str,
    n_chars: usize,
    space_w: f32,
) -> f32 {
    let tab_w = space_w * INDENT_STEP as f32;
    ui.fonts(|f| {
        advance_with(line, n_chars, |c| {
            if c == '\t' {
                tab_w
            } else {
                f.glyph_width(font_id, c)
            }
        })
    })
}

/// 字符大约占几个等宽格子（tab = [`INDENT_STEP`]；东亚宽字符按 2 格粗算）。
///
/// 只用于**整文档级别的粗估**（水平滚动上限、minimap 缩略宽度）——那里拿不到字体，
/// 也不可能为每行都去问字形宽度。需要精确像素的地方一律用 [`text_advance`]。
fn char_cells(c: char) -> usize {
    if c == '\t' {
        INDENT_STEP
    } else if is_wide_char(c) {
        2
    } else {
        1
    }
}

/// 粗略的「东亚宽 / 全角字符」判定（汉字、假名、谚文、全角标点、常见 emoji 区）。
fn is_wide_char(c: char) -> bool {
    matches!(
        c as u32,
        0x1100..=0x115F      // Hangul Jamo
        | 0x2E80..=0x303E    // CJK 部首 / 标点
        | 0x3041..=0x33FF    // 假名、注音、CJK 兼容
        | 0x3400..=0x4DBF    // CJK 扩展 A
        | 0x4E00..=0x9FFF    // CJK 基本区
        | 0xA000..=0xA4CF    // 彝文
        | 0xAC00..=0xD7A3    // 谚文音节
        | 0xF900..=0xFAFF    // CJK 兼容表意
        | 0xFE30..=0xFE6F    // CJK 兼容形式
        | 0xFF00..=0xFF60    // 全角 ASCII
        | 0xFFE0..=0xFFE6    // 全角符号
        | 0x1F300..=0x1F64F  // 常见 emoji
        | 0x1F900..=0x1F9FF
        | 0x20000..=0x2FFFD  // CJK 扩展 B+
        | 0x30000..=0x3FFFD
    )
}

/// 行内字节区间，去掉行尾 `\r`/`\n`。
///
/// `Buffer::line()` / `line_byte_starts` 给出的行区间含行尾换行；若把换行交给
/// `LayoutJob`，会多排出一个空行、galley 高度翻倍，导致正文与光标/选中高亮垂直错位。
/// 统一在此裁掉行尾换行。
fn line_span_no_eol(raw: &str, start: usize, end: usize) -> (usize, usize) {
    let start = start.min(raw.len());
    let mut end = end.min(raw.len()).max(start);
    while end > start
        && matches!(
            raw.as_bytes().get(end - 1),
            Some(b'\n') | Some(b'\r')
        )
    {
        end -= 1;
    }
    (start, end)
}

/// 浮层层级判定：面板（`SidePanel`/`CentralPanel`）都在 `Order::Background` 背景层，
/// `Window`/`Area`/工具提示在更高层级。
fn order_is_float(order: egui::Order) -> bool {
    order != egui::Order::Background
}

/// 指针是否落在浮层（弹窗 / 菜单 / 工具提示）之上。
///
/// 滚轮增量是全局输入、不区分控件，用该判断把浮层上的滚轮挡在编辑器之外，
/// 否则在弹出窗口 / 侧栏 / 控制台里滚动会连带滚动背后的编辑器。
fn pointer_over_float(ctx: &egui::Context) -> bool {
    ctx.input(|i| i.pointer.interact_pos())
        .and_then(|p| ctx.layer_id_at(p))
        .map(|l| order_is_float(l.order))
        .unwrap_or(false)
}

/// 是否为「全局快捷键」：Ctrl/⌘ 组合键，或 F5/F9/F10/F11/F12 功能键。
///
/// 焦点在侧栏搜索框等非模态文本框时，编辑器仍应响应这些键（对齐 VS Code），
/// 但字符输入与无修饰键的编辑键必须让给那个文本框。
fn is_global_shortcut(ev: &egui::Event) -> bool {
    match ev {
        egui::Event::Key {
            key,
            pressed: true,
            modifiers,
            ..
        } => {
            modifiers.command
                || matches!(
                    key,
                    egui::Key::F5
                        | egui::Key::F9
                        | egui::Key::F10
                        | egui::Key::F11
                        | egui::Key::F12
                )
        }
        _ => false,
    }
}

/// 会改动正文或光标的按键，会打断输入法组合。
///
/// 字母/数字/符号键**不算**：输入法只吞掉字符消息（`WM_CHAR`），`WM_KEYDOWN` 照样送进来，
/// 若把它们也算作打断，组合串会每帧被清掉、永远显示不出来。
fn breaks_ime_composition(ev: &egui::Event) -> bool {
    use egui::Key;
    match ev {
        egui::Event::Ime(_) => false,
        egui::Event::Key {
            key,
            pressed,
            modifiers,
            ..
        } => {
            *pressed
                && (modifiers.command
                    || modifiers.alt
                    || matches!(
                        key,
                        Key::Backspace
                            | Key::Delete
                            | Key::Enter
                            | Key::ArrowLeft
                            | Key::ArrowRight
                            | Key::ArrowUp
                            | Key::ArrowDown
                            | Key::Home
                            | Key::End
                            | Key::Tab
                            | Key::F5
                            | Key::F9
                            | Key::F10
                            | Key::F11
                            | Key::F12
                    ))
        }
        // 字符 / 粘贴等直接改正文的事件
        _ => true,
    }
}

/// 在字符数组中查找 `idx` 处括号的配对（`()[]{}`）；无匹配返回 None。
fn find_matching_bracket(chars: &[char], idx: usize) -> Option<usize> {
    const OPEN: &str = "([{";
    const CLOSE: &str = ")]}";
    let c = *chars.get(idx)?;
    if let Some(pos) = OPEN.find(c) {
        let close = CLOSE.chars().nth(pos)?;
        let mut depth = 0i32;
        for (j, &d) in chars.iter().enumerate().skip(idx) {
            if d == c {
                depth += 1;
            } else if d == close {
                depth -= 1;
                if depth == 0 {
                    return Some(j);
                }
            }
        }
    } else if let Some(pos) = CLOSE.find(c) {
        let open = OPEN.chars().nth(pos)?;
        let mut depth = 0i32;
        for j in (0..=idx).rev() {
            let d = chars[j];
            if d == c {
                depth += 1;
            } else if d == open {
                depth -= 1;
                if depth == 0 {
                    return Some(j);
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn select_all_covers_document() {
        let mut ed = Editor::new("ab\ncd\nef");
        ed.select_all();
        assert_eq!(ed.selection_text(), "ab\ncd\nef");
        ed.cut_to_clipboard(&egui::Context::default());
        assert_eq!(ed.text(), "");
        assert_eq!(ed.edit_seq(), 1); // 一次剪切
    }

    #[test]
    fn selection_text_respects_range() {
        let mut ed = Editor::new("hello world");
        ed.set_cursor_line_col(0, 0);
        ed.selection = Some(0..5);
        assert_eq!(ed.selection_text(), "hello");
    }

    #[test]
    fn delete_line_removes_with_newline() {
        let mut ed = Editor::new("aaa\nbbb\nccc");
        ed.set_cursor_line_col(1, 0);
        ed.delete_line();
        assert_eq!(ed.text(), "aaa\nccc");
        assert_eq!(ed.cursor_line_col(), (1, 0));
    }

    #[test]
    fn delete_last_line_keeps_previous_newline() {
        let mut ed = Editor::new("aaa\nbbb");
        ed.set_cursor_line_col(1, 0);
        ed.delete_line();
        assert_eq!(ed.text(), "aaa\n");
    }

    #[test]
    fn duplicate_line_copies_above() {
        let mut ed = Editor::new("aaa\nbbb");
        ed.set_cursor_line_col(1, 0);
        ed.duplicate_line();
        assert_eq!(ed.text(), "aaa\nbbb\nbbb");
        assert_eq!(ed.cursor_line_col(), (1, 0));
    }

    #[test]
    fn toggle_comment_on_and_off() {
        let mut ed = Editor::new("int x;\n");
        ed.set_cursor_line_col(0, 0);
        ed.toggle_line_comment();
        assert_eq!(ed.text(), "// int x;\n");
        ed.toggle_line_comment();
        assert_eq!(ed.text(), "int x;\n");
    }

    #[test]
    fn toggle_comment_respects_leading_whitespace() {
        let mut ed = Editor::new("    int x;\n");
        ed.set_cursor_line_col(0, 4);
        ed.toggle_line_comment();
        assert_eq!(ed.text(), "    // int x;\n");
        ed.toggle_line_comment();
        assert_eq!(ed.text(), "    int x;\n");
    }

    #[test]
    fn paste_text_filters_control_chars() {
        let mut ed = Editor::new("ab");
        ed.set_cursor_line_col(0, 1);
        ed.paste_text("X\x01Y\n");
        assert_eq!(ed.text(), "aXY\nb");
    }

    #[test]
    fn select_line_selects_whole_line() {
        let mut ed = Editor::new("abc\ndef\n");
        ed.set_cursor_line_col(1, 1);
        ed.select_line();
        // 与 VSCode Ctrl+L 一致：非末行包含行尾换行。
        assert_eq!(ed.selection_text(), "def\n");
    }

    #[test]
    fn typography_clamps_font_size() {
        let mut ed = Editor::new("x");
        ed.set_typography(99.0);
        assert_eq!(ed.font_size, MAX_FONT_SIZE);
        ed.set_typography(14.0);
        assert_eq!(ed.line_height, 21.0); // 14 * 1.5
    }

    #[test]
    fn theme_switches_palette() {
        // 编辑器默认外观 = 用户 VS Code 的 "Dark (Visual Studio - C/C++)"。
        let mut ed = Editor::new("x");
        assert_eq!(ed.kind, ThemeKind::CppDark);
        assert_eq!(ed.tokens.get(HighlightGroup::Comment), hex("#57a64a"));
        assert_eq!(ed.colors.bg_editor, hex("#1E1E1E"));
        ed.set_theme(ThemeKind::DarkPlus, &ext_theme_vscode::DARK_PLUS);
        assert_eq!(ed.kind, ThemeKind::DarkPlus);
        assert_eq!(ed.tokens.get(HighlightGroup::Comment), hex("#6a9955"));
        assert_eq!(ed.colors.bg_editor, hex("#1e1e1e"));
        ed.set_theme(ThemeKind::LightPlus, &ext_theme_vscode::LIGHT_PLUS);
        assert_eq!(ed.kind, ThemeKind::LightPlus);
        assert_eq!(ed.colors.bg_editor, hex("#FFFFFF"));
        assert_eq!(ed.colors.selection, hex("#ADD6FF"));
        assert_eq!(ed.tokens.get(HighlightGroup::Comment), hex("#008000"));
        // C/C++ Themes：注释 #57A64A / 控制流 #D8A0DF / 形参 #9A9A9A
        ed.set_theme(ThemeKind::CppDark, &ext_theme_vscode::CPPTOOLS_DARK);
        assert_eq!(ed.tokens.get(HighlightGroup::Comment), hex("#57a64a"));
        assert_eq!(ed.tokens.get(HighlightGroup::KeywordControl), hex("#d8a0df"));
        assert_eq!(ed.tokens.get(HighlightGroup::Parameter), hex("#9a9a9a"));
        assert_eq!(ed.tokens.get(HighlightGroup::Plain), hex("#dadada"));
        assert_eq!(ed.colors.bg_editor, hex("#1E1E1E"));
    }

    #[test]
    fn injected_highlighter_drives_syntax_colors() {
        // 编辑器只持有 `dyn LanguageHighlighter`：换成语言扩展即可高亮。
        let mut ed = Editor::new("int main() { return 0; }");
        assert!(ed.highlights.is_empty(), "默认应是空实现（纯文本）");
        ed.set_highlighter(Box::new(ext_cpp_highlight::CppHighlighter::new()));
        assert!(!ed.highlights.is_empty(), "注入 C++ 扩展后应有高亮");
        assert!(ed
            .highlights
            .iter()
            .any(|h| h.group == HighlightGroup::Keyword));
    }

    #[test]
    fn null_highlighter_leaves_text_plain() {
        let mut ed = Editor::new("int main() {}");
        ed.set_highlighter(Box::new(ide_ext::NullHighlighter));
        assert!(ed.highlights.is_empty());
    }

    #[test]
    fn incremental_typing_matches_full_parse() {
        // 编辑 → `TextEdit::between` → 语言扩展增量解析；结果必须与全量解析逐字节一致。
        let ctx = egui::Context::default();
        let mut ed = Editor::new("int main() {\n    int x = 1;\n    return x;\n}\n");
        ed.set_highlighter(Box::new(ext_cpp_highlight::CppHighlighter::new()));
        for (i, ch) in "// 注释".chars().enumerate() {
            let n = ed.text().chars().count();
            ed.replace_range(n, n, &ch.to_string());
            // 高亮按帧懒刷新（同一帧内多次编辑只重算一次），这里手动跑一帧。
            render_once(&ctx, &mut ed);
            let full = ext_cpp_highlight::CppHighlighter::new().highlight(&ed.text(), None);
            assert_eq!(ed.highlights, full, "第 {i} 次输入 {ch:?} 后增量结果与全量不一致");
        }
        assert!(ed
            .highlights
            .iter()
            .any(|h| h.group == HighlightGroup::Comment));
    }

    /// 跑一帧渲染（只为触发懒刷新）。
    fn render_once(ctx: &egui::Context, ed: &mut Editor) {
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let _ = ed.show(ui);
            });
        });
    }

    // ---- 输入法（IME）----
    //
    // Windows 输入法组合时**不产生** `Event::Text`：拼音串走 `ImeEvent::Preedit`、
    // 上屏结果走 `ImeEvent::Commit`。自绘编辑器必须自己处理，否则切了输入法也打不出中文。

    fn ime_preedit_ev(text: &str) -> egui::Event {
        egui::Event::Ime(egui::ImeEvent::Preedit(text.to_string()))
    }

    fn ime_commit_ev(text: &str) -> egui::Event {
        egui::Event::Ime(egui::ImeEvent::Commit(text.to_string()))
    }

    /// 跑一帧并注入原始事件（模拟 winit → egui 的输入通道）。
    fn render_with_events(
        ctx: &egui::Context,
        ed: &mut Editor,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        ctx.run(
            egui::RawInput {
                events,
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let _ = ed.show(ui);
                });
            },
        )
    }

    #[test]
    fn ime_commit_writes_chinese_into_document() {
        let ctx = egui::Context::default();
        let mut ed = Editor::new("// \n");
        ed.goto_char(3);
        render_with_events(&ctx, &mut ed, vec![ime_commit_ev("中文注释")]);
        assert_eq!(ed.text(), "// 中文注释\n");
        assert_eq!(ed.cursor_line_col(), (0, 7));
    }

    #[test]
    fn ime_preedit_is_visible_and_commit_replaces_it() {
        let ctx = egui::Context::default();
        let mut ed = Editor::new("abc");
        ed.goto_char(3);
        render_with_events(&ctx, &mut ed, vec![ime_preedit_ev("ni")]);
        assert_eq!(ed.text(), "abcni", "组合串应立刻可见（不能等上屏才显示）");
        render_with_events(&ctx, &mut ed, vec![ime_preedit_ev("nihao")]);
        assert_eq!(ed.text(), "abcnihao", "新的组合串要替换旧串，不能叠加");
        render_with_events(&ctx, &mut ed, vec![ime_commit_ev("你好")]);
        assert_eq!(ed.text(), "abc你好", "上屏后不能残留拼音");
    }

    #[test]
    fn ime_commit_inside_comment_keeps_surrounding_text() {
        let ctx = egui::Context::default();
        let mut ed = Editor::new("// \nint x = 1;\n");
        ed.goto_char(3);
        render_with_events(&ctx, &mut ed, vec![ime_commit_ev("你好")]);
        assert_eq!(ed.text(), "// 你好\nint x = 1;\n");
    }

    #[test]
    fn ime_cancel_and_interrupt_drop_preedit() {
        // ① Esc / 失焦：输入法取消组合 → 拼音串必须消失，正文回到原样。
        let ctx = egui::Context::default();
        let mut ed = Editor::new("abc");
        ed.goto_char(3);
        render_with_events(&ctx, &mut ed, vec![ime_preedit_ev("ni")]);
        render_with_events(
            &ctx,
            &mut ed,
            vec![egui::Event::Ime(egui::ImeEvent::Disabled)],
        );
        assert_eq!(ed.text(), "abc");

        // ② 组合中被别的按键打断：先撤掉拼音串，再处理按键（不能把拼音留在正文里）。
        let mut ed = Editor::new("abc");
        ed.goto_char(3);
        render_with_events(&ctx, &mut ed, vec![ime_preedit_ev("ni")]);
        render_with_events(&ctx, &mut ed, vec![key_ev(egui::Key::Backspace, false, false)]);
        assert_eq!(ed.text(), "ab", "打断后应按真实正文退格");
    }

    #[test]
    fn ime_preedit_dropped_when_modal_takes_input() {
        // 组合进行中模态窗口抢走输入：拼音串不能留成正文。
        let ctx = egui::Context::default();
        let mut ed = Editor::new("abc");
        ed.goto_char(3);
        render_with_events(&ctx, &mut ed, vec![ime_preedit_ev("ni")]);
        assert_eq!(ed.text(), "abcni");
        ed.set_keys_enabled(false);
        assert_eq!(ed.text(), "abc");
        ed.set_keys_enabled(true);
        ed.set_text_focus(true);
        assert_eq!(ed.text(), "abc");
    }

    #[test]
    fn stale_ime_preedit_never_deletes_other_text() {
        // 组合串的跟踪位置失效后（外部撤销 / 替换全部…），撤销跟踪必须只丢跟踪、不误删正文。
        let ctx = egui::Context::default();
        let mut ed = Editor::new("abc");
        ed.goto_char(3);
        render_with_events(&ctx, &mut ed, vec![ime_preedit_ev("ni")]);
        assert_eq!(ed.text(), "abcni");
        ed.replace_range(3, 5, ""); // 外部改动（菜单里的「替换全部」）动了组合串所在位置 → 跟踪失效
        assert_eq!(ed.text(), "abc");
        render_with_events(&ctx, &mut ed, vec![ime_preedit_ev("nihao")]);
        assert!(ed.text().starts_with("abc"), "正文被误删：{}", ed.text());
        render_with_events(&ctx, &mut ed, vec![ime_commit_ev("你好")]);
        assert!(ed.text().starts_with("abc"), "提交后正文被破坏：{}", ed.text());
    }

    #[test]
    fn ime_composition_survives_letter_keys_but_not_edit_keys() {
        // 组合期间字母键也会到达（输入法只吞字符消息）：不能因此打断组合，
        // 否则拼音串会被每帧清掉、永远显示不出来。
        assert!(!breaks_ime_composition(&key_ev(egui::Key::N, false, false)));
        assert!(!breaks_ime_composition(&ime_preedit_ev("ni")));
        assert!(!breaks_ime_composition(&ime_commit_ev("你")));
        assert!(!breaks_ime_composition(&egui::Event::Key {
            key: egui::Key::Backspace,
            physical_key: None,
            pressed: false, // 输入法吞掉 keydown 时只剩 keyup，不能据此打断
            repeat: false,
            modifiers: egui::Modifiers::default(),
        }));
        // 会改动正文 / 光标的输入必须打断（否则临时拼音串会被留在正文里）。
        assert!(breaks_ime_composition(&key_ev(egui::Key::Backspace, false, false)));
        assert!(breaks_ime_composition(&key_ev(egui::Key::Enter, false, false)));
        assert!(breaks_ime_composition(&key_ev(egui::Key::ArrowLeft, false, false)));
        assert!(breaks_ime_composition(&key_ev(egui::Key::Z, true, false)));
        assert!(breaks_ime_composition(&egui::Event::Text("中".to_string())));
        assert!(breaks_ime_composition(&egui::Event::Paste("中".to_string())));
    }

    #[test]
    fn ime_ignored_while_other_text_box_has_focus() {
        let ctx = egui::Context::default();
        let mut ed = Editor::new("abc");
        ed.goto_char(3);
        ed.set_text_focus(true);
        render_with_events(&ctx, &mut ed, vec![ime_commit_ev("你好")]);
        assert_eq!(ed.text(), "abc", "焦点在别的文本框时编辑器不得抢输入法事件");
    }

    #[test]
    fn editor_opens_ime_channel_with_caret_area() {
        let ctx = egui::Context::default();
        let mut ed = Editor::new("abc");
        let out = render_with_events(&ctx, &mut ed, Vec::new());
        let ime = out
            .platform_output
            .ime
            .expect("编辑器接管输入时必须上报 IME（否则窗口输入法通道是关的）");
        assert!(ime.cursor_rect.height() > 0.0, "候选窗要锚在光标上");
        assert!(ime.rect.width() > 0.0);

        // 焦点让给别的文本框 / 模态窗口打开：不再抢（交给那边上报）。
        ed.set_text_focus(true);
        let out = render_with_events(&ctx, &mut ed, Vec::new());
        assert!(out.platform_output.ime.is_none());
    }

    // 列 → 像素换算：中文不是等宽格（实测 Hack 一格 8.28px、微软雅黑汉字 13.64px ≈ 1.65 格），
    // 按「列数 × 格宽」定位会让光标 / 选区停在汉字中间。下面用假字体度量把这条契约钉死。

    /// 假字体：ASCII 一格 `10.0`，汉字 `16.5`（≈ 真实字体里的 1.65 倍）。
    fn fake_glyph_w(c: char) -> f32 {
        if is_wide_char(c) {
            16.5
        } else {
            10.0
        }
    }

    #[test]
    fn advance_uses_real_glyph_widths_not_char_cells() {
        let line = "ab中中";
        assert_eq!(advance_with(line, 0, fake_glyph_w), 0.0);
        assert_eq!(advance_with(line, 2, fake_glyph_w), 20.0);
        assert_eq!(advance_with(line, 3, fake_glyph_w), 36.5);
        // 关键：中文行尾的光标位置是真实宽度之和，而不是「4 个字 × 一格」。
        assert_eq!(advance_with(line, 4, fake_glyph_w), 53.0);
        assert_ne!(advance_with(line, 4, fake_glyph_w), 40.0);
    }

    #[test]
    fn click_column_is_inverse_of_advance() {
        let line = "中ab";
        // 汉字中点（8.25）之前算汉字前，之后算汉字后。
        assert_eq!(col_at_x(line, 0.0, fake_glyph_w), 0);
        assert_eq!(col_at_x(line, 8.0, fake_glyph_w), 0);
        assert_eq!(col_at_x(line, 9.0, fake_glyph_w), 1);
        assert_eq!(col_at_x(line, 26.4, fake_glyph_w), 2);
        assert_eq!(col_at_x(line, 26.5, fake_glyph_w), 2);
        assert_eq!(col_at_x(line, 31.5, fake_glyph_w), 3);
        // 点到行尾之后：光标落在最后一个字符后面（与 VSCode 一致），而不是钉在倒数第二个字符上。
        assert_eq!(col_at_x(line, 9999.0, fake_glyph_w), 3);
    }

    #[test]
    fn col_at_x_round_trips_advance_on_cjk_line() {
        let line = "// 中文abc";
        for n in 0..=line.chars().count() {
            let x = advance_with(line, n, fake_glyph_w);
            // 每个字符的前半段都归属自己 → 从该字符的起点稍偏右点回去，仍回到同一个字符。
            let probe = x + 1.0;
            let col = col_at_x(line, probe, fake_glyph_w);
            assert_eq!(col, n, "偏移 {x} 应落在第 {n} 个字符上，实际 {col}");
        }
    }

    #[test]
    fn tab_and_wide_chars_count_as_multiple_cells() {
        assert_eq!(char_cells('a'), 1);
        assert_eq!(char_cells('\t'), INDENT_STEP);
        assert_eq!(char_cells('中'), 2);
        assert_eq!(char_cells('，'), 2); // 全角标点
        assert_eq!(char_cells('ｱ'), 1); // 半角片假名是窄字符
    }

    #[test]
    fn max_cols_accounts_for_wide_chars() {
        // 横向滚动上限按显示宽度算：3 个汉字 = 6 格（否则中文长行滚不到行尾）。
        let mut ed = Editor::new("中中中\n");
        ed.refresh_highlights();
        assert_eq!(ed.max_cols, 6);
    }

    #[test]
    fn ime_caret_area_follows_real_text_width_on_cjk_line() {
        // 端到端：光标处的 IME 锚点（= 光标位置）必须按真实字形宽度走。
        // 装了系统中文字体才量得出差异，没装就跳过（否则汉字被渲染成替换字符，宽度≈一格）。
        let ctx = egui::Context::default();
        crate::apply_fonts(&ctx, "Default");
        let font_id = egui::FontId::monospace(14.0);
        let mut widths = (0.0f32, 0.0f32);
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            widths = (
                ctx.fonts(|f| f.glyph_width(&font_id, ' ')),
                ctx.fonts(|f| f.glyph_width(&font_id, '中')),
            );
        });
        let (space, cjk) = widths;
        assert!(space > 0.0, "量不到空格宽度");
        if (cjk - space).abs() < 0.5 {
            return; // 系统里没有中文字体：测不出「汉字比一格宽」，跳过。
        }

        let mut ed = Editor::new("abc中文\n");
        let caret_x = |ed: &mut Editor| -> f32 {
            let out = render_with_events(&ctx, ed, Vec::new());
            out.platform_output
                .ime
                .expect("编辑器接管输入时必须上报 IME 锚点")
                .cursor_rect
                .min
                .x
        };
        ed.goto_char(3); // "abc|中文"
        let x_ascii = caret_x(&mut ed);
        ed.goto_char(5); // "abc中文|"
        let x_cjk = caret_x(&mut ed);
        let delta = x_cjk - x_ascii;
        // 两个汉字 = 2 × 真实汉字宽（不是 2 × 等宽格）。
        assert!(
            (delta - 2.0 * cjk).abs() < 0.5,
            "汉字宽度没被算进去：位移 {delta}，期望 {}",
            2.0 * cjk
        );
        assert!(
            (delta - 2.0 * space).abs() > 0.5,
            "位移 {delta} 说明还是按「列数 × 等宽格」在算"
        );
    }

    #[test]
    fn find_matches_case_insensitive_and_overlap_step() {
        let ed = Editor::new("abc ABC abc");
        let m = ed.find_matches("abc", false);
        assert_eq!(m, vec![(0, 3), (4, 7), (8, 11)]);
        let m2 = ed.find_matches("ABC", true);
        assert_eq!(m2, vec![(4, 7)]);
        assert!(ed.find_matches("", true).is_empty());
    }

    #[test]
    fn select_range_and_replace_range() {
        let mut ed = Editor::new("hello world");
        ed.select_range(0, 5);
        assert_eq!(ed.current_selection(), Some((0, 5)));
        assert_eq!(ed.selection_text(), "hello");
        ed.replace_range(0, 5, "bye");
        assert_eq!(ed.text(), "bye world");
        assert_eq!(ed.cursor_line_col(), (0, 3));
    }

    #[test]
    fn goto_char_scrolls_toward_line() {
        let mut ed = Editor::new("a\nb\nc\nd\ne");
        ed.viewport_h = 40.0; // 2 行视口
        ed.goto_char(8); // 第 4 行（0-based 4）
        assert!(ed.scroll > 0.0);
    }

    #[test]
    fn matching_bracket_finds_pair() {
        let chars: Vec<char> = "int f(int a[3]) { return a[0]; }".chars().collect();
        let open = chars.iter().position(|&c| c == '(').unwrap();
        let close = find_matching_bracket(&chars, open).unwrap();
        assert_eq!(chars[close], ')');
        let br = chars.iter().position(|&c| c == '[').unwrap();
        let bclose = find_matching_bracket(&chars, br).unwrap();
        assert_eq!(chars[bclose], ']');
        assert!(find_matching_bracket(&chars, 0).is_none()); // 'i' 非括号
    }

    #[test]
    fn indent_columns_counts_tabs_and_spaces() {
        assert_eq!(indent_columns("    x"), 4);
        assert_eq!(indent_columns("\tx"), 4);
        assert_eq!(indent_columns("\t  x"), 6);
        assert_eq!(indent_columns("x"), 0);
    }

    #[test]
    fn line_span_strips_trailing_newline() {
        // 行区间含行尾换行时需裁掉，否则 LayoutJob 多排一行 → 正文与光标垂直错位。
        let raw = "abc\ndef\n\nghi";
        assert_eq!(line_span_no_eol(raw, 0, 4), (0, 3)); // "abc\n"
        assert_eq!(line_span_no_eol(raw, 4, 8), (4, 7)); // "def\n"
        assert_eq!(line_span_no_eol(raw, 8, 9), (8, 8)); // 空行 "\n"
        assert_eq!(line_span_no_eol(raw, 9, 12), (9, 12)); // 末行无换行
    }

    #[test]
    fn line_span_strips_crlf() {
        let raw = "abc\r\nx";
        assert_eq!(line_span_no_eol(raw, 0, 5), (0, 3)); // "abc\r\n"
        assert_eq!(line_span_no_eol(raw, 5, 6), (5, 6)); // "x"
    }

    fn key_ev(key: egui::Key, command: bool, shift: bool) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers {
                command,
                shift,
                ..Default::default()
            },
        }
    }

    #[test]
    fn global_shortcuts_survive_text_focus() {
        // Ctrl+… 与 F 功能键：文本框聚焦时编辑器仍要响应。
        assert!(is_global_shortcut(&key_ev(egui::Key::P, true, false)));
        assert!(is_global_shortcut(&key_ev(egui::Key::F5, false, false)));
        assert!(is_global_shortcut(&key_ev(egui::Key::F9, false, false)));
        // 普通字符 / 编辑键 / 粘贴：必须让给聚焦的文本框。
        assert!(!is_global_shortcut(&egui::Event::Text("a".to_string())));
        assert!(!is_global_shortcut(&egui::Event::Paste("x".to_string())));
        assert!(!is_global_shortcut(&key_ev(egui::Key::Backspace, false, false)));
        assert!(!is_global_shortcut(&key_ev(egui::Key::Enter, false, false)));
        assert!(!is_global_shortcut(&key_ev(egui::Key::ArrowDown, false, false)));
    }

    #[test]
    fn pointer_over_float_detects_top_layer() {
        // 背景层（面板）→ 不算浮层；其它层级（弹窗/工具提示）→ 算浮层。
        assert!(!order_is_float(egui::Order::Background));
        assert!(order_is_float(egui::Order::Middle));
        assert!(order_is_float(egui::Order::Tooltip));
    }
}
