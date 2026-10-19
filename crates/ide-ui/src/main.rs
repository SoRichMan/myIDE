//! LiteIDE 入口：阶段 2 搜索、LSP（clangd）集成；阶段 3 调试器（GDB MI）；
//! 阶段 4 搜索（rg）、Git + 设置面板；阶段 5 设置落地/命令面板/多标签/UI 打磨。
// release 构建不弹控制台窗口（GUI 程序）；debug 保留控制台看 tracing 日志。
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
mod commands;
mod editor;
mod settings;
mod tabs;
mod tree;

use eframe::egui;
use eframe::egui::RichText;
use editor::EditorAction;
use ide_core::theme::{palette, ThemeKind};
use ide_ext::ExtensionHost;
use ide_dap::manager::{BreakpointSpec, DapManager, DapStatus, Event as DapEvent, StackFrame, VarNode};
use ide_git::{Event as GitEvent, GitManager, GitStatus};
use ide_lsp::manager::{Event as LspEvent, LspManager, LspStatus};
use ide_lsp::message::{Position, SymbolInfo};
use ide_lsp::path_map::{uri_to_win, win_to_uri};
use ide_search::{Event as SearchEvent, SearchManager, SearchMatch, SearchOptions};
use settings::AppSettings;
use tabs::{CloseResult, Tab, TabList};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

const HOVER_DWELL: Duration = Duration::from_millis(300);
const FLASH_DUR: Duration = Duration::from_millis(2500);
/// 跳转栈上限（阶段 5 性能：防无限增长）。
const JUMP_STACK_CAP: usize = 100;
/// 关闭标签的恢复栈上限。
const CLOSED_TAB_CAP: usize = 10;
/// 侧栏（Explorer/Outline）宽度范围与默认值。
const SIDEBAR_MIN_WIDTH: f32 = 160.0;
const SIDEBAR_MAX_WIDTH: f32 = 520.0;
const SIDEBAR_DEFAULT_WIDTH: f32 = 260.0;
/// 侧栏展开/收起动画时长（秒）。
const SIDEBAR_ANIM_TIME: f32 = 0.16;
/// 激活标签顶部蓝色标识条的高度（VS Code active tab indicator）。
const TAB_ACTIVE_INDICATOR_H: f32 = 2.0;
/// 双击识别窗口（秒）。Windows 默认双击间隔 500ms，而 egui 内置判定只有 300ms，
/// 慢一点的双击会漏判；这里自实现识别并放宽到 Windows 的语义。
const DOUBLE_CLICK_SECS: f64 = 0.55;

fn hex(s: &str) -> egui::Color32 {
    let s = s.trim_start_matches('#');
    let r = u8::from_str_radix(&s[0..2], 16).unwrap_or(0);
    let g = u8::from_str_radix(&s[2..4], 16).unwrap_or(0);
    let b = u8::from_str_radix(&s[4..6], 16).unwrap_or(0);
    egui::Color32::from_rgb(r, g, b)
}

/// 启动时装配的内置扩展（顺序 = 注册顺序，先注册者优先认领后缀）。
///
/// 新增功能 = 新增一个 `Extension` 实现 + 在这里（或运行时的插件目录）登记，
/// 详见 `docs/extensions.md`。
fn builtin_extensions() -> Vec<Box<dyn ide_ext::Extension>> {
    vec![
        Box::new(ext_cpp_highlight::CppHighlight),
        Box::new(ext_theme_vscode::VscodeThemes),
    ]
}

/// 取某个主题的 token 色板：用主题扩展注册的配色，缺失时退回内置 Dark+。
fn theme_tokens(host: &ExtensionHost, kind: ThemeKind) -> ide_ext::TokenPalette {
    host.registry()
        .tokens(kind)
        .copied()
        .unwrap_or(ext_theme_vscode::DARK_PLUS)
}

/// 拖动侧栏右侧分割线：把宽度夹到 `[SIDEBAR_MIN_WIDTH, SIDEBAR_MAX_WIDTH]`。
fn sidebar_drag(cur: f32, dx: f32) -> f32 {
    (cur + dx).clamp(SIDEBAR_MIN_WIDTH, SIDEBAR_MAX_WIDTH)
}

const CJK_FONT_CANDIDATES: &[&str] = &[
    "C:\\Windows\\Fonts\\msyh.ttc",
    "C:\\Windows\\Fonts\\simhei.ttf",
    "C:\\Windows\\Fonts\\msyh.ttf",
];

/// 按设置的字体系列重建 egui 字体表（Consolas / Cascadia Code / 默认 + CJK）。
/// `LITEIDE_NO_CJK=1` 时跳过 CJK 字体（诊断内存用）。
fn apply_fonts(ctx: &egui::Context, family: &str) {
    let mut fonts = egui::FontDefinitions::default();
    let no_cjk = std::env::var("LITEIDE_NO_CJK").map(|v| v == "1").unwrap_or(false);
    if !no_cjk {
        for path in CJK_FONT_CANDIDATES {
            if let Ok(bytes) = std::fs::read(path) {
                fonts
                    .font_data
                    .insert("cjk".to_owned(), egui::FontData::from_owned(bytes));
                if let Some(list) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
                    list.push("cjk".to_owned());
                }
                if let Some(list) = fonts.families.get_mut(&egui::FontFamily::Monospace) {
                    list.push("cjk".to_owned());
                }
                break;
            }
        }
    }
    let mono_candidates: &[&str] = match family {
        "Consolas" => &["C:\\Windows\\Fonts\\consola.ttf", "C:\\Windows\\Fonts\\consolab.ttf"],
        "Cascadia Code" => &[
            "C:\\Windows\\Fonts\\CascadiaCode.ttf",
            "C:\\Windows\\Fonts\\CascadiaCodePL.ttf",
        ],
        _ => &[],
    };
    for p in mono_candidates {
        if let Ok(bytes) = std::fs::read(p) {
            fonts
                .font_data
                .insert("ui_mono".to_owned(), egui::FontData::from_owned(bytes));
            if let Some(list) = fonts.families.get_mut(&egui::FontFamily::Monospace) {
                list.insert(0, "ui_mono".to_owned());
            }
            tracing::info!("loaded mono font: {p}");
            break;
        }
    }
    ctx.set_fonts(fonts);
}

fn load_initial_file() -> (Option<PathBuf>, String) {
    let cwd = std::env::current_dir().unwrap_or_default();
    let candidates = [
        "testdata/large.cpp",
        "../testdata/large.cpp",
        "../../testdata/large.cpp",
    ];
    for rel in candidates {
        if let Ok(s) = std::fs::read_to_string(rel) {
            let abs = cwd.join(rel);
            tracing::info!("opened {} ({} bytes)", rel, s.len());
            return (Some(abs), s);
        }
    }
    tracing::warn!("large.cpp not found, using built-in fallback");
    (
        None,
        String::from(
            "// LiteIDE fallback buffer\n#include <iostream>\n\nint main() {\n    std::cout << \"hello\" << std::endl;\n    return 0;\n}\n",
        ),
    )
}

/// 调试面板按钮 → 动作（渲染闭包外应用，避免借用冲突）。
enum DebugPanelAction {
    ToggleExpand(String),
    JumpTo { file: String, line: u32 },
    DeleteBreakpoint(u32),
    EnableBreakpoint { number: u32, enable: bool },
    AddWatch(String),
    Ptype(String),
    RemoveWatch(usize),
    RemoveUiBreakpoint(usize),
}

/// 左侧侧边栏内容（阶段 5 活动栏切换）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SidebarContent {
    Explorer,
    /// 全局搜索：与 VS Code 一致，搜索结果占用「资源管理器」所在的侧栏位置。
    Search,
    Outline,
}

/// 设置面板收集的动作（渲染闭包外应用）。
enum SettingsAction {
    FontSize(f32),
    Theme(ThemeKind),
    Family(String),
    Minimap(bool),
    Breadcrumbs(bool),
    Sidebar(bool),
    Reset,
}

struct LiteIdeApp {
    editor: Option<editor::Editor>,
    status_line: Arc<str>,
    // ---- LSP ----
    lsp: LspManager,
    lsp_started: bool,
    lsp_status: LspStatus,
    lsp_ready_synced: bool,
    current_file: Option<PathBuf>,
    current_uri: Option<String>,
    root_uri: String,
    doc_version: i32,
    last_sync_seq: u64,
    jump_stack: Vec<(PathBuf, usize, usize)>,
    hover_anchor: Option<(usize, Instant)>,
    hover_requested_char: Option<usize>,
    pending_hover: Option<u64>,
    hover_result: Option<(egui::Pos2, String)>,
    symbols: Vec<SymbolInfo>,
    symbol_search_open: bool,
    symbol_filter: String,
    pending_def: Option<u64>,
    pending_sym: Option<u64>,
    flash: Option<(String, Instant)>,
    fonts_ready: bool,
    boot_start: Instant,
    // ---- DAP ----
    dap: DapManager,
    dap_status: DapStatus,
    ui_breakpoints: Vec<BreakpointSpec>,
    dap_breakpoints: Vec<ide_dap::manager::BreakpointInfo>,
    dap_variables: Vec<VarNode>,
    dap_watches: Vec<(String, String)>,
    dap_stack: Vec<StackFrame>,
    dap_console: Vec<String>,
    dap_children: HashMap<String, Vec<VarNode>>,
    dap_expanded: HashSet<String>,
    dap_pending_expand: HashMap<u64, String>,
    dap_watch_input: String,
    dap_ptype_input: String,
    // ---- 阶段 4：搜索 / Git ----
    search: SearchManager,
    git: GitManager,
    file_picker_open: bool,
    file_picker_query: String,
    /// 文件浏览器当前目录（可位于工程之外）。
    file_picker_dir: PathBuf,
    /// 当前目录的条目缓存 `(名称, 是否目录)`，仅在切换目录时重读一次。
    file_picker_entries: Vec<(String, bool)>,
    /// 下一帧把焦点交给文件浏览器的输入框。
    file_picker_focus: bool,
    content_query: String,
    content_results: Vec<SearchMatch>,
    pending_search_seq: Option<u64>,
    /// 下一帧把焦点交给侧栏搜索输入框（Ctrl+Shift+F 时）。
    content_search_focus: bool,
    git_panel_open: bool,
    debug_open: bool,
    git_status: GitStatus,
    git_log: Vec<ide_git::log::GitCommit>,
    git_diff_text: String,
    git_commit_input: String,
    // ---- 阶段 5：设置 / 多标签 / 命令面板 / 视图 ----
    /// 扩展宿主：启动时激活内置扩展，注册表供界面查询语言/配色。
    host: ExtensionHost,
    settings: AppSettings,
    config_mtime: Option<SystemTime>,
    applied_family: String,
    applied_font: f32,
    applied_theme: ThemeKind,
    tabs: TabList,
    closed_tabs: Vec<Tab>,
    editor_clean_seq: u64,
    indent_hint: String,
    settings_open: bool,
    palette_open: bool,
    palette_query: String,
    palette_sel: usize,
    palette_focus_req: bool,
    palette_frames: u32,
    palette_open_at: Option<Instant>,
    theme_switch_at: Option<Instant>,
    sidebar_content: SidebarContent,
    /// 侧栏展开时的目标宽度（拖动分割线后更新；收起再展开时恢复）。
    sidebar_width: f32,
    /// 上一帧是否正在拖动分割线（用于检测“松开”时机）。
    sidebar_dragging: bool,
    goto_line_open: bool,
    goto_line_input: String,
    about_open: bool,
    keys_open: bool,
    confirm_close: Option<usize>,
    explorer_loaded: bool,
    // ---- 复核改进：查找 / 替换 / 另存为 / 搜索过滤 ----
    find_open: bool,
    find_replace_mode: bool,
    find_query: String,
    find_replace_text: String,
    find_matches: Vec<(usize, usize)>,
    find_current: usize,
    find_case: bool,
    find_last_seq: u64,
    find_last_query: String,
    find_focus_req: bool,
    save_as_open: bool,
    save_as_input: String,
    search_case: bool,
    search_regex: bool,
    search_word: bool,
    search_glob: String,
    // ---- 复核改进：资源管理器文件树 ----
    explorer_files: Vec<String>,
    explorer_tree: tree::FileTree,
    explorer_rows: Vec<tree::Row>,
    explorer_expanded: HashSet<String>,
    /// 上一次点击的文件树行 (行号, 时间)，用于自实现双击识别（见 `DOUBLE_CLICK_SECS`）。
    explorer_last_click: Option<(usize, f64)>,
    /// 上一次点击的标签页 (序号, 时间)，用于双击锁定。
    tab_last_click: Option<(usize, f64)>,
}

impl LiteIdeApp {
    fn new() -> Self {
        let (settings, config_mtime) = AppSettings::load();
        // 扩展机制：先激活内置扩展，再按注册表装配编辑器。
        // 激活只做注册（无 IO / 无重计算），耗时在微秒级——性能契约见 docs/extensions.md。
        let host = ExtensionHost::activate(builtin_extensions());
        for rec in host.report() {
            tracing::info!(
                "[ext] {} ({}) {:?} · {} 贡献点 · {} us",
                rec.id,
                rec.name,
                rec.outcome,
                rec.registered,
                rec.duration.as_micros()
            );
        }
        let (path, text) = load_initial_file();
        let theme_kind = ThemeKind::from_name(&settings.theme);
        let mut editor = editor::Editor::new(&text);
        editor.set_typography(settings.font_size);
        editor.set_theme(theme_kind, &theme_tokens(&host, theme_kind));
        editor.set_minimap(settings.show_minimap);
        if let Some(p) = path.as_ref() {
            // 语言扩展按后缀认领：.cpp/.h/... → C++ 高亮，其它文件保持纯文本。
            editor.set_highlighter(host.registry().create_highlighter(&p.to_string_lossy()));
        }
        let status_line: Arc<str> = editor.status_text().into();
        let current_file = path.clone();
        let current_uri = current_file
            .as_ref()
            .and_then(|p| win_to_uri(&p.to_string_lossy()));
        let mut tabs = TabList::new();
        if let Some(p) = path.clone() {
            tabs.open(p, text);
        } else {
            tabs.open_untitled();
        }
        let indent_hint = detect_indent(&editor.text());
        let applied_theme = theme_kind;
        Self {
            editor: Some(editor),
            status_line,
            lsp: LspManager::new(),
            lsp_started: false,
            lsp_status: LspStatus::Off,
            lsp_ready_synced: false,
            current_file,
            current_uri,
            root_uri: String::new(),
            doc_version: 1,
            last_sync_seq: 0,
            jump_stack: Vec::new(),
            hover_anchor: None,
            hover_requested_char: None,
            pending_hover: None,
            hover_result: None,
            symbols: Vec::new(),
            symbol_search_open: false,
            symbol_filter: String::new(),
            pending_def: None,
            pending_sym: None,
            flash: None,
            fonts_ready: false,
            boot_start: Instant::now(),
            dap: DapManager::new(),
            dap_status: DapStatus::Off,
            ui_breakpoints: Vec::new(),
            dap_breakpoints: Vec::new(),
            dap_variables: Vec::new(),
            dap_watches: Vec::new(),
            dap_stack: Vec::new(),
            dap_console: Vec::new(),
            dap_children: HashMap::new(),
            dap_expanded: HashSet::new(),
            dap_pending_expand: HashMap::new(),
            dap_watch_input: String::new(),
            dap_ptype_input: String::new(),
            search: SearchManager::new(),
            git: GitManager::new(),
            file_picker_open: false,
            file_picker_query: String::new(),
            file_picker_dir: PathBuf::new(),
            file_picker_entries: Vec::new(),
            file_picker_focus: false,
            content_query: String::new(),
            content_results: Vec::new(),
            pending_search_seq: None,
            content_search_focus: false,
            git_panel_open: false,
            debug_open: false,
            git_status: GitStatus::default(),
            git_log: Vec::new(),
            git_diff_text: String::new(),
            git_commit_input: String::new(),
            host,
            settings,
            config_mtime,
            applied_family: "Default".to_string(),
            applied_font: 14.0,
            applied_theme,
            tabs,
            closed_tabs: Vec::new(),
            editor_clean_seq: 0,
            indent_hint,
            settings_open: false,
            palette_open: false,
            palette_query: String::new(),
            palette_sel: 0,
            palette_focus_req: false,
            palette_frames: 0,
            palette_open_at: None,
            theme_switch_at: None,
            sidebar_content: SidebarContent::Explorer,
            sidebar_width: SIDEBAR_DEFAULT_WIDTH,
            sidebar_dragging: false,
            goto_line_open: false,
            goto_line_input: String::new(),
            about_open: false,
            keys_open: false,
            confirm_close: None,
            explorer_loaded: false,
            find_open: false,
            find_replace_mode: false,
            find_query: String::new(),
            find_replace_text: String::new(),
            find_matches: Vec::new(),
            find_current: 0,
            find_case: false,
            find_last_seq: u64::MAX,
            find_last_query: String::new(),
            find_focus_req: false,
            save_as_open: false,
            save_as_input: String::new(),
            search_case: false,
            search_regex: false,
            search_word: false,
            search_glob: String::new(),
            explorer_files: Vec::new(),
            explorer_tree: tree::FileTree::default(),
            explorer_rows: Vec::new(),
            explorer_expanded: HashSet::new(),
            explorer_last_click: None,
            tab_last_click: None,
        }
    }

    fn pick_root(&self) -> String {
        let cwd = std::env::current_dir().unwrap_or_default();
        let sample = cwd.join("testdata").join("sample_project");
        if sample.is_dir() {
            return sample.to_string_lossy().into_owned();
        }
        if let Some(f) = &self.current_file {
            if let Some(parent) = f.parent() {
                return parent.to_string_lossy().into_owned();
            }
        }
        cwd.to_string_lossy().into_owned()
    }

    fn project_root(&self) -> String {
        std::env::current_dir()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned()
    }

    // ================= 标签页（阶段 5） =================


    /// 当前激活标签路径。
    fn active_tab_path(&self) -> Option<PathBuf> {
        self.tabs.active_tab().and_then(|t| t.path.clone())
    }

    /// 把编辑器当前文本/光标暂存回激活标签（切换前调用）。
    fn stash_active_tab(&mut self) {
        let (text, cursor) = {
            let Some(ed) = self.editor.as_ref() else { return };
            (ed.text(), ed.cursor_line_col())
        };
        self.tabs.put_active_text(text, cursor);
    }

    /// 把指定标签加载进编辑器（并同步 current_file/current_uri/LSP 状态）。
    fn load_tab_into_editor(&mut self, idx: usize) {
        let Some(tab) = self.tabs.get(idx).cloned() else { return };
        // 语言扩展按后缀认领高亮器；未命名文件用空实现（纯文本）。
        let highlighter = match tab.path.as_ref() {
            Some(p) => self.host.registry().create_highlighter(&p.to_string_lossy()),
            None => Box::new(ide_ext::NullHighlighter),
        };
        if let Some(ed) = &mut self.editor {
            ed.open_document(&tab.text, highlighter);
            ed.set_cursor_line_col(tab.cursor.0, tab.cursor.1);
        }
        self.current_file = tab.path.clone();
        self.current_uri = tab
            .path
            .as_ref()
            .and_then(|p| win_to_uri(&p.to_string_lossy()));
        self.doc_version = 1;
        self.last_sync_seq = self.editor.as_ref().map(|e| e.edit_seq()).unwrap_or(0);
        self.lsp_ready_synced = false;
        self.hover_result = None;
        self.symbols.clear();
        self.editor_clean_seq = self.last_sync_seq;
        self.indent_hint = self
            .editor
            .as_ref()
            .map(|e| detect_indent(&e.text()))
            .unwrap_or_default();
        tracing::info!("tab -> {:?}", tab.path);
    }

    /// 切换标签：先暂存旧标签，再关闭旧 LSP 文档、激活新标签并加载。
    fn switch_to_tab(&mut self, idx: usize) {
        if idx >= self.tabs.len() || idx == self.tabs.active() {
            return;
        }
        self.stash_active_tab();
        if let Some(old_uri) = self.current_uri.clone() {
            if self.lsp_status.is_ready() {
                self.lsp.close_document(old_uri);
            }
        }
        self.tabs.activate(idx);
        self.load_tab_into_editor(idx);
    }

    /// Ctrl+Tab / Ctrl+Shift+Tab 循环切换。
    fn cycle_tab(&mut self, dir: i8) {
        if self.tabs.len() < 2 {
            return;
        }
        self.stash_active_tab();
        if let Some(old_uri) = self.current_uri.clone() {
            if self.lsp_status.is_ready() {
                self.lsp.close_document(old_uri);
            }
        }
        if dir > 0 {
            self.tabs.next();
        } else {
            self.tabs.prev();
        }
        self.load_tab_into_editor(self.tabs.active());
    }

    /// 关闭后重载激活标签到编辑器（标签可能为空）。
    fn reload_active_editor(&mut self) {
        if self.tabs.is_empty() {
            if let Some(ed) = &mut self.editor {
                ed.open_document("", Box::new(ide_ext::NullHighlighter));
            }
            self.current_file = None;
            self.current_uri = None;
            self.lsp_ready_synced = false;
            self.symbols.clear();
            self.editor_clean_seq = self.editor.as_ref().map(|e| e.edit_seq()).unwrap_or(0);
            self.indent_hint = "Spaces: 4".to_string();
            return;
        }
        self.load_tab_into_editor(self.tabs.active());
    }

    /// 关闭指定标签（脏标签弹确认，返回是否真正关闭）。
    fn close_tab_at(&mut self, idx: usize) -> bool {
        if idx >= self.tabs.len() {
            return false;
        }
        self.stash_active_tab();
        let tab = self.tabs.get(idx).cloned();
        match self.tabs.close(idx) {
            CloseResult::Dirty => {
                self.confirm_close = Some(idx);
                false
            }
            CloseResult::Closed => {
                if let Some(t) = tab {
                    self.closed_tabs.push(t);
                    if self.closed_tabs.len() > CLOSED_TAB_CAP {
                        self.closed_tabs.remove(0);
                    }
                }
                self.confirm_close = None;
                self.reload_active_editor();
                true
            }
        }
    }

    fn on_close_tab(&mut self) {
        if self.tabs.is_empty() {
            return;
        }
        self.stash_active_tab();
        let tab = self.tabs.active_tab().cloned();
        match self.tabs.close_active() {
            CloseResult::Dirty => {
                self.confirm_close = Some(self.tabs.active());
            }
            CloseResult::Closed => {
                if let Some(t) = tab {
                    self.closed_tabs.push(t);
                    if self.closed_tabs.len() > CLOSED_TAB_CAP {
                        self.closed_tabs.remove(0);
                    }
                }
                self.reload_active_editor();
            }
        }
    }

    fn on_close_all(&mut self) {
        if let Some(idx) = self.tabs.iter().position(|t| t.dirty) {
            self.confirm_close = Some(idx);
            self.flash("有未保存修改，先确认关闭脏标签".to_string());
            return;
        }
        while !self.tabs.is_empty() {
            if !self.close_tab_at(self.tabs.active()) {
                break;
            }
        }
    }

    fn on_new_file(&mut self) {
        self.stash_active_tab();
        if let Some(old_uri) = self.current_uri.clone() {
            if self.lsp_status.is_ready() {
                self.lsp.close_document(old_uri);
            }
        }
        let idx = self.tabs.open_untitled();
        self.load_tab_into_editor(idx);
        self.flash("新建未命名文件（暂不支持另存为）".to_string());
    }

    fn on_reopen_closed(&mut self) {
        let Some(tab) = self.closed_tabs.pop() else {
            self.flash("没有可恢复的标签".to_string());
            return;
        };
        self.stash_active_tab();
        if let Some(old_uri) = self.current_uri.clone() {
            if self.lsp_status.is_ready() {
                self.lsp.close_document(old_uri);
            }
        }
        let idx = if let Some(p) = &tab.path {
            if let Some(existing) = self.tabs.index_of_path(p) {
                self.tabs.activate(existing);
                self.load_tab_into_editor(existing);
                return;
            }
            self.tabs.restore(tab)
        } else {
            self.tabs.restore(tab)
        };
        self.load_tab_into_editor(idx);
    }

    fn on_reload_active(&mut self) {
        let Some(path) = self.active_tab_path() else {
            self.flash("未命名文件无法从磁盘重载".to_string());
            return;
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                if let Some(ed) = &mut self.editor {
                    ed.load_text(&text);
                }
                self.tabs.replace_active_text(text);
                self.editor_clean_seq = self.editor.as_ref().map(|e| e.edit_seq()).unwrap_or(0);
                self.flash(format!("已从磁盘重载 {}", path.display()));
            }
            Err(e) => self.flash(format!("重载失败: {e}")),
        }
    }

    fn on_save(&mut self) {
        let Some(path) = self.active_tab_path() else {
            self.flash("未命名文件请用「另存为」(Ctrl+Shift+S)".to_string());
            return;
        };
        let Some(ed) = &self.editor else { return };
        if let Err(e) = std::fs::write(&path, ed.text()) {
            self.flash(format!("保存失败: {e}"));
            return;
        }
        let seq = ed.edit_seq();
        self.flash(format!("已保存 {}", path.display()));
        self.tabs.set_active_dirty(false);
        self.editor_clean_seq = seq;
        if self.lsp_status.is_ready() {
            if let Some(uri) = self.current_uri.clone() {
                self.lsp.save_document(uri.clone());
                let seq = self.lsp.request_symbols(uri);
                self.pending_sym = Some(seq);
            }
        }
    }

    fn on_save_all(&mut self) {
        let mut saved = 0usize;
        let mut skipped = 0usize;
        let mut active_seq = self.editor_clean_seq;
        for i in 0..self.tabs.len() {
            let (path, text) = {
                let Some(t) = self.tabs.get(i) else { continue };
                (t.path.clone(), t.text.clone())
            };
            match path {
                Some(p) => {
                    if let Err(e) = std::fs::write(&p, &text) {
                        self.flash(format!("保存 {} 失败: {e}", p.display()));
                        return;
                    }
                    if let Some(t) = self.tabs.get_mut(i) {
                        t.dirty = false;
                    }
                    saved += 1;
                    if i == self.tabs.active() {
                        active_seq = self.editor.as_ref().map(|e| e.edit_seq()).unwrap_or(0);
                    }
                }
                None => skipped += 1,
            }
        }
        self.editor_clean_seq = active_seq;
        self.flash(format!(
            "已保存 {saved} 个文件{}",
            if skipped > 0 {
                format!("（跳过 {skipped} 个未命名）")
            } else {
                String::new()
            }
        ));
    }

    // ================= 文件 / 跳转 =================

    /// 打开文件（已打开则切换标签，不覆盖内存内容）。
    ///
    /// `preview = true` 时按 VS Code「预览」语义打开：不锁定，下一次预览打开会顶替它
    /// （单击资源管理器文件、Ctrl+P、跳转定义等）；`false` 则锁定（双击、打开已存在文件。
    fn open_file(&mut self, path: PathBuf, text: String, preview: bool) {
        if let Some(existing) = self.tabs.index_of_path(&path) {
            self.switch_to_tab(existing);
            if !preview {
                self.tabs.pin(existing);
            }
            return;
        }
        self.stash_active_tab();
        if let Some(old_uri) = self.current_uri.clone() {
            if self.lsp_status.is_ready() {
                self.lsp.close_document(old_uri);
            }
        }
        let (idx, _) = if preview {
            self.tabs.open_preview(path, text)
        } else {
            self.tabs.open(path, text)
        };
        self.load_tab_into_editor(idx);
    }

    fn jump_to(&mut self, win: &str, line: usize, col: usize) {
        let target = PathBuf::from(win);
        if self
            .active_tab_path()
            .as_deref()
            .map(|p| p == target.as_path())
            .unwrap_or(false)
        {
            if let Some(ed) = &mut self.editor {
                ed.set_cursor_line_col(line, col);
            }
            return;
        }
        if let Some(idx) = self.tabs.index_of_path(&target) {
            self.switch_to_tab(idx);
            if let Some(ed) = &mut self.editor {
                ed.set_cursor_line_col(line, col);
            }
            return;
        }
        match std::fs::read_to_string(&target) {
            Ok(text) => {
                self.open_file(target, text, true);
                if let Some(ed) = &mut self.editor {
                    ed.set_cursor_line_col(line, col);
                }
            }
            Err(e) => self.flash(format!("无法打开 {}: {e}", win)),
        }
    }

    // ================= LSP =================

    fn maybe_start_lsp(&mut self) {
        if self.lsp_started {
            return;
        }
        self.lsp_started = true;
        let root = self.pick_root();
        self.root_uri = win_to_uri(&root).unwrap_or_else(|| "file:///".to_string());
        tracing::info!("LSP root: {root} -> {}", self.root_uri);
        self.lsp.start(self.root_uri.clone());
    }

    fn poll_lsp(&mut self) {
        while let Some(ev) = self.lsp.poll() {
            match ev {
                LspEvent::Status(s) => self.lsp_status = s,
                LspEvent::Definition { seq, result } => {
                    if self.pending_def == Some(seq) {
                        self.pending_def = None;
                        self.handle_definition(result);
                    }
                }
                LspEvent::Hover { seq, result } => {
                    if self.pending_hover == Some(seq) {
                        self.pending_hover = None;
                        self.handle_hover(result);
                    }
                }
                LspEvent::Symbols { seq, result } => {
                    if self.pending_sym == Some(seq) {
                        self.pending_sym = None;
                        match result {
                            Ok(syms) => self.symbols = syms,
                            Err(e) => tracing::warn!("documentSymbol 失败: {e}"),
                        }
                    }
                }
                LspEvent::ServerLog(_) | LspEvent::Diagnostics { .. } => {}
            }
        }
    }

    fn sync_document(&mut self) {
        if self.lsp_status.is_ready() {
            let Some(uri) = self.current_uri.clone() else {
                return;
            };
            if !self.lsp_ready_synced {
                if let Some(ed) = &self.editor {
                    self.lsp
                        .open_document(uri.clone(), ed.text(), self.doc_version);
                    let seq = self.lsp.request_symbols(uri);
                    self.pending_sym = Some(seq);
                }
                self.lsp_ready_synced = true;
                self.last_sync_seq = self.editor.as_ref().map(|e| e.edit_seq()).unwrap_or(0);
            } else if let Some(ed) = &self.editor {
                let seq_now = ed.edit_seq();
                if seq_now != self.last_sync_seq {
                    self.doc_version = self.doc_version.saturating_add(1);
                    self.lsp.change_document(uri, ed.text(), self.doc_version);
                    self.last_sync_seq = seq_now;
                }
            }
        } else if self.lsp_ready_synced {
            self.lsp_ready_synced = false;
        }
    }

    fn handle_definition(&mut self, result: Result<Vec<ide_lsp::Location>, String>) {
        match result {
            Ok(locs) if !locs.is_empty() => {
                let loc = &locs[0];
                match uri_to_win(&loc.uri) {
                    Some(win) => {
                        let line = loc.range.start.line as usize;
                        let col = loc.range.start.character as usize;
                        tracing::info!("jump -> {win}:{}", line + 1);
                        self.jump_to(&win, line, col);
                    }
                    None => self.flash(format!("无法解析目标路径: {}", loc.uri)),
                }
            }
            Ok(_) => self.flash("未找到定义".to_string()),
            Err(e) => self.flash(format!("跳转失败: {e}")),
        }
    }

    fn handle_hover(&mut self, result: Result<Option<ide_lsp::HoverInfo>, String>) {
        match result {
            Ok(Some(info)) => {
                let pos = self
                    .editor
                    .as_ref()
                    .and_then(|e| e.mouse_pos())
                    .unwrap_or(egui::Pos2::ZERO);
                self.hover_result = Some((pos, clean_markdown(&info.markdown)));
            }
            Ok(None) => self.hover_result = None,
            Err(e) => {
                tracing::warn!("hover 失败: {e}");
                self.hover_result = None;
            }
        }
    }

    // ---- DAP ----

    fn debug_binary(&self) -> Option<String> {
        if let Ok(bin) = std::env::var("LITEIDE_DEBUG_BIN") {
            if !bin.is_empty() {
                return Some(bin);
            }
        }
        let cwd = std::env::current_dir().ok()?;
        let cand = cwd.join("testdata").join("debug_target");
        cand.exists().then(|| cand.to_string_lossy().into_owned())
    }

    fn poll_dap(&mut self) {
        while let Some(ev) = self.dap.poll() {
            match ev {
                DapEvent::Status(s) => self.dap_status = s,
                DapEvent::Stopped { file_win, line, func, .. } => {
                    if !file_win.is_empty() && line > 0 {
                        self.jump_to(&file_win, line.saturating_sub(1) as usize, 0);
                    }
                    self.flash(format!("已停止 @ {func} : {line}"));
                }
                DapEvent::Running => {}
                DapEvent::Exited { code } => {
                    let msg = match code {
                        Some(c) => format!("程序已退出（exit code {c}）"),
                        None => "程序已退出".to_string(),
                    };
                    self.flash(msg);
                }
                DapEvent::Breakpoints(bps) => self.dap_breakpoints = bps,
                DapEvent::Variables { locals, watches } => {
                    self.dap_variables = locals;
                    self.dap_watches = watches;
                    self.dap_children.clear();
                    self.dap_expanded.clear();
                    self.dap_pending_expand.clear();
                }
                DapEvent::Stack { frames } => self.dap_stack = frames,
                DapEvent::VarChildren { seq, expr, children, .. } => {
                    if let Some(path) = self.dap_pending_expand.remove(&seq) {
                        self.dap_children.insert(path, children);
                    } else {
                        self.dap_children.insert(expr, children);
                    }
                }
                DapEvent::Ptype { text } | DapEvent::Console(text) => {
                    self.push_console(text);
                }
                DapEvent::Error(msg) => {
                    tracing::warn!("dap: {msg}");
                    self.flash(msg);
                }
            }
        }
    }

    fn push_console(&mut self, text: String) {
        if text.trim().is_empty() {
            return;
        }
        for line in text.lines() {
            self.dap_console.push(line.to_string());
        }
        const MAX_CONSOLE: usize = 400;
        if self.dap_console.len() > MAX_CONSOLE {
            let drop = self.dap_console.len() - MAX_CONSOLE;
            self.dap_console.drain(0..drop);
        }
    }

    fn dap_interactive(&self) -> bool {
        self.dap_status.is_interactive()
    }

    fn on_debug_start(&mut self) {
        match &self.dap_status {
            DapStatus::Off | DapStatus::Unavailable(_) | DapStatus::Crashed(_) => {
                let Some(bin) = self.debug_binary() else {
                    self.flash(
                        "未找到调试二进制 testdata/debug_target：请先运行 scripts/build_debug_target.ps1"
                            .to_string(),
                    );
                    return;
                };
                let bps = self.ui_breakpoints.clone();
                tracing::info!("debug start: {bin}, {} breakpoints", bps.len());
                self.dap.start(bin, bps);
            }
            DapStatus::Stopped | DapStatus::Ready => self.dap.continue_run(),
            _ => {}
        }
    }

    fn on_debug_stop(&mut self) {
        self.dap.stop();
    }

    fn on_toggle_breakpoint(&mut self) {
        let Some((line0, _col)) = self.editor.as_ref().map(|e| e.cursor_line_col()) else {
            return;
        };
        self.toggle_breakpoint_at_line(line0);
    }

    /// 切换指定行（0-based）断点：先更新 UI 集合，调试会话中同步到 gdb。
    fn toggle_breakpoint_at_line(&mut self, line0: usize) {
        let Some(file) = self.active_tab_path() else {
            self.flash("没有打开的文件，无法设置断点".to_string());
            return;
        };
        let spec = BreakpointSpec {
            file_win: file.to_string_lossy().into_owned(),
            line: line0 as u32 + 1,
            condition: None,
        };
        let same = self
            .ui_breakpoints
            .iter()
            .find(|b| b.file_win == spec.file_win && b.line == spec.line)
            .cloned();
        match same {
            Some(_) => self.ui_breakpoints.retain(|b| {
                !(b.file_win == spec.file_win && b.line == spec.line)
            }),
            None => self.ui_breakpoints.push(spec.clone()),
        }
        if self.dap_interactive() {
            self.dap.toggle_breakpoint(spec);
        }
    }

    /// 当前文件断点行号（1-based），同步给编辑器行号栏画红点。
    fn sync_editor_breakpoints(&mut self) {
        let Some(cur) = self.current_file.clone() else {
            if let Some(ed) = &mut self.editor {
                ed.set_breakpoints(Vec::new());
            }
            return;
        };
        let cur = cur.to_string_lossy().into_owned();
        let lines: Vec<usize> = self
            .ui_breakpoints
            .iter()
            .filter(|b| b.file_win == cur)
            .map(|b| b.line as usize)
            .collect();
        if let Some(ed) = &mut self.editor {
            ed.set_breakpoints(lines);
        }
    }

    fn on_debug_next(&mut self) {
        self.dap.step_over();
    }

    fn on_debug_step(&mut self) {
        self.dap.step_into();
    }

    fn on_debug_finish(&mut self) {
        self.dap.step_out();
    }

    fn on_add_watch(&mut self) {
        let word = self
            .editor
            .as_ref()
            .and_then(|e| e.word_at_cursor())
            .unwrap_or_default();
        if word.is_empty() {
            self.flash("光标不在标识符上，无法添加监视".to_string());
            return;
        }
        self.dap.add_watch(word.clone());
        self.flash(format!("已添加监视 {word}"));
    }

    fn apply_debug_actions(&mut self, actions: Vec<DebugPanelAction>) {
        for a in actions {
            match a {
                DebugPanelAction::ToggleExpand(path) => {
                    if self.dap_expanded.contains(&path) {
                        self.dap_expanded.remove(&path);
                    } else {
                        self.dap_expanded.insert(path.clone());
                        let seq = self.dap.expand_var(path.clone());
                        self.dap_pending_expand.insert(seq, path);
                    }
                }
                DebugPanelAction::JumpTo { file, line } => {
                    self.jump_to(&file, line.saturating_sub(1) as usize, 0);
                }
                DebugPanelAction::DeleteBreakpoint(n) => self.dap.delete_breakpoint(n),
                DebugPanelAction::EnableBreakpoint { number, enable } => {
                    self.dap.enable_breakpoint(number, enable);
                }
                DebugPanelAction::AddWatch(expr) => {
                    if !expr.trim().is_empty() {
                        self.dap.add_watch(expr.trim().to_string());
                        self.dap_watch_input.clear();
                    }
                }
                DebugPanelAction::Ptype(ty) => {
                    if !ty.trim().is_empty() {
                        self.dap.ptype(ty.trim().to_string());
                        self.dap_ptype_input.clear();
                    }
                }
                DebugPanelAction::RemoveWatch(idx) => {
                    if let Some((expr, _)) = self.dap_watches.get(idx) {
                        self.dap.remove_watch(expr.clone());
                    }
                }
                DebugPanelAction::RemoveUiBreakpoint(idx) => {
                    self.ui_breakpoints.remove(idx);
                }
            }
        }
    }

    fn draw_debug_panel(&mut self, ctx: &egui::Context) {
        let mut actions: Vec<DebugPanelAction> = Vec::new();
        let status = self.dap_status.clone();
        let session_active = !matches!(status, DapStatus::Off | DapStatus::Unavailable(_));
        let dap_bps = self.dap_breakpoints.clone();
        let ui_bps = self.ui_breakpoints.clone();
        let bps_len = if session_active {
            dap_bps.len()
        } else {
            ui_bps.len()
        };
        let stack = self.dap_stack.clone();
        let locals = self.dap_variables.clone();
        let watches = self.dap_watches.clone();
        let children_map = self.dap_children.clone();
        let expanded = self.dap_expanded.clone();
        let console = self.dap_console.clone();
        let watch_input = &mut self.dap_watch_input;
        let ptype_input = &mut self.dap_ptype_input;

        egui::SidePanel::right("debug_panel")
            .resizable(true)
            .default_width(300.0)
            .width_range(220.0..=480.0)
            .frame(egui::Frame::none().fill(hex(ide_core::theme::BG_SIDEBAR)))
            .show(ctx, |ui| {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.add_space(2.0);
                    ui.label(
                        egui::RichText::new("DEBUG")
                            .size(11.5)
                            .strong()
                            .color(hex("#e0e0e0")),
                    );
                    ui.label(
                        egui::RichText::new(dap_status_label(&status))
                            .size(11.0)
                            .color(dap_status_color(&status)),
                    );
                });
                ui.add_space(4.0);
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        section(ui, "BREAKPOINTS", bps_len);
                        if session_active {
                            for bp in &dap_bps {
                                ui.horizontal(|ui| {
                                    let mark = if bp.enabled { "●" } else { "○" };
                                    let color = if bp.enabled {
                                        egui::Color32::from_rgb(0xe5, 0x14, 0x00)
                                    } else {
                                        hex("#6a6a6a")
                                    };
                                    let loc = format!(
                                        "{}:{}",
                                        bp.file_win
                                            .rsplit('\\')
                                            .next()
                                            .unwrap_or(&bp.file_win),
                                        bp.line
                                    );
                                    let mut label = format!("{mark} {loc}");
                                    if let Some(c) = &bp.condition {
                                        label.push_str(&format!("  if {c}"));
                                    }
                                    ui.label(RichText::new(label).monospace().size(11.0).color(color));
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            if ui
                                                .small_button(if bp.enabled { "off" } else { "on" })
                                                .on_hover_text("启用/禁用")
                                                .clicked()
                                            {
                                                actions.push(
                                                    DebugPanelAction::EnableBreakpoint {
                                                        number: bp.number,
                                                        enable: !bp.enabled,
                                                    },
                                                );
                                            }
                                            if ui.small_button("×").clicked() {
                                                actions.push(
                                                    DebugPanelAction::DeleteBreakpoint(bp.number),
                                                );
                                            }
                                        },
                                    );
                                });
                            }
                            if dap_bps.is_empty() {
                                ui.label(
                                    RichText::new("(none — F9 在源码行设置)")
                                        .size(11.0)
                                        .color(hex("#6a6a6a")),
                                );
                            }
                        } else {
                            for (i, bp) in ui_bps.iter().enumerate() {
                                ui.horizontal(|ui| {
                                    let file = bp
                                        .file_win
                                        .rsplit('\\')
                                        .next()
                                        .unwrap_or(&bp.file_win);
                                    ui.label(
                                        RichText::new(format!("● {file}:{}", bp.line))
                                            .monospace()
                                            .size(11.0)
                                            .color(egui::Color32::from_rgb(0xe5, 0x14, 0x00)),
                                    );
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            if ui.small_button("×").clicked() {
                                                actions.push(
                                                    DebugPanelAction::RemoveUiBreakpoint(i),
                                                );
                                            }
                                        },
                                    );
                                });
                            }
                            if ui_bps.is_empty() {
                                ui.label(
                                    RichText::new("(none — F9 在源码行设置)")
                                        .size(11.0)
                                        .color(hex("#6a6a6a")),
                                );
                            }
                        }
                        ui.separator();
                        section(ui, "CALL STACK", stack.len());
                        if stack.is_empty() {
                            ui.label(
                                RichText::new("(stopped 后显示)")
                                    .size(11.0)
                                    .color(hex("#6a6a6a")),
                            );
                        }
                        for f in &stack {
                            let loc = match &f.line {
                                Some(l) => format!("{}:{}", f.func, l),
                                None => f.func.clone(),
                            };
                            let (file, line) = (f.file_win.clone(), f.line.unwrap_or(0));
                            if ui
                                .add(
                                    egui::Label::new(
                                        RichText::new(format!("{}  {}", f.level, loc))
                                            .monospace()
                                            .size(11.0)
                                            .color(hex("#d4d4d4")),
                                    )
                                    .sense(egui::Sense::click()),
                                )
                                .clicked()
                                && line > 0
                            {
                                actions.push(DebugPanelAction::JumpTo { file, line });
                            }
                        }
                        ui.separator();
                        section(ui, "VARIABLES", locals.len());
                        if locals.is_empty() {
                            ui.label(
                                RichText::new("(stopped 后显示)")
                                    .size(11.0)
                                    .color(hex("#6a6a6a")),
                            );
                        }
                        for v in &locals {
                            draw_var_node(
                                ui,
                                v,
                                &v.exp,
                                &expanded,
                                &children_map,
                                0,
                                &mut actions,
                            );
                        }
                        ui.separator();
                        section(ui, "WATCHES", watches.len());
                        for (i, (expr, value)) in watches.iter().enumerate() {
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new(format!("{expr} = {value}"))
                                        .monospace()
                                        .size(11.0)
                                        .color(hex("#dcdcaa")),
                                );
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if ui.small_button("×").clicked() {
                                            actions.push(DebugPanelAction::RemoveWatch(i));
                                        }
                                    },
                                );
                            });
                        }
                        let add = ui
                            .horizontal(|ui| {
                                let edit = ui.add(
                                    egui::TextEdit::singleline(watch_input)
                                        .hint_text("watch expr, Enter 添加")
                                        .desired_width(170.0),
                                );
                                let clicked = ui.button("Add").clicked();
                                let enter = edit.lost_focus()
                                    && ui.input(|i| i.key_pressed(egui::Key::Enter));
                                (clicked || enter, watch_input.clone())
                            })
                            .inner;
                        if add.0 {
                            actions.push(DebugPanelAction::AddWatch(add.1));
                        }
                        ui.separator();
                        section(ui, "PTYPE /o", 0);
                        let ptype = ui
                            .horizontal(|ui| {
                                let edit = ui.add(
                                    egui::TextEdit::singleline(ptype_input)
                                        .hint_text("类型名，如 Foo")
                                        .desired_width(140.0),
                                );
                                let clicked = ui.button("布局").clicked();
                                let enter = edit.lost_focus()
                                    && ui.input(|i| i.key_pressed(egui::Key::Enter));
                                (clicked || enter, ptype_input.clone())
                            })
                            .inner;
                        if ptype.0 {
                            actions.push(DebugPanelAction::Ptype(ptype.1));
                        }
                        ui.separator();
                        section(ui, "CONSOLE", 0);
                        egui::ScrollArea::vertical()
                            .id_source("console_scroll")
                            .max_height(220.0)
                            .stick_to_bottom(true)
                            .show(ui, |ui| {
                                for line in &console {
                                    ui.label(
                                        RichText::new(line.as_str())
                                            .monospace()
                                            .size(10.5)
                                            .color(hex("#6a9955")),
                                    );
                                }
                                if console.is_empty() {
                                    ui.label(
                                        RichText::new("(gdb 原始输出)")
                                            .size(11.0)
                                            .color(hex("#6a6a6a")),
                                    );
                                }
                            });
                    });
            });
        self.apply_debug_actions(actions);
    }

    fn on_goto_definition(&mut self) {
        if !self.lsp_status.is_ready() {
            self.flash("LSP 未就绪，无法跳转".to_string());
            return;
        }
        let Some(uri) = self.current_uri.clone() else {
            return;
        };
        let Some((line, col)) = self.editor.as_ref().map(|e| e.cursor_line_col()) else {
            return;
        };
        let same_pos = match self.jump_stack.last() {
            Some((p, l, c)) => {
                let same_file = self
                    .current_file
                    .as_ref()
                    .map(|f| f.as_path() == p.as_path())
                    .unwrap_or(false);
                same_file && *l == line && *c == col
            }
            None => false,
        };
        if !same_pos {
            if let Some(f) = self.current_file.clone() {
                self.jump_stack.push((f, line, col));
                if self.jump_stack.len() > JUMP_STACK_CAP {
                    let drop = self.jump_stack.len() - JUMP_STACK_CAP;
                    self.jump_stack.drain(0..drop);
                }
            }
        }
        let seq = self.lsp.request_definition(
            uri,
            Position {
                line: line as u32,
                character: col as u32,
            },
        );
        self.pending_def = Some(seq);
    }

    fn on_go_back(&mut self) {
        if let Some((path, line, col)) = self.jump_stack.pop() {
            self.jump_to(&path.to_string_lossy(), line, col);
        } else {
            self.flash("没有可返回的位置".to_string());
        }
    }

    fn update_hover(&mut self, ctx: &egui::Context) {
        let mc = self.editor.as_ref().and_then(|e| e.mouse_char());
        match mc {
            Some(c) => {
                match self.hover_anchor {
                    Some((ac, t)) if ac == c => {
                        let ready = self.lsp_status.is_ready();
                        if t.elapsed() >= HOVER_DWELL
                            && ready
                            && self.hover_requested_char != Some(c)
                            && self.pending_hover.is_none()
                        {
                            if let (Some(uri), Some(ed)) =
                                (self.current_uri.clone(), self.editor.as_ref())
                            {
                                let (line, col) = ed.char_to_line_col(c);
                                let seq = self.lsp.request_hover(
                                    uri,
                                    Position {
                                        line: line as u32,
                                        character: col as u32,
                                    },
                                );
                                self.pending_hover = Some(seq);
                                self.hover_requested_char = Some(c);
                            }
                        }
                    }
                    _ => {
                        if self.hover_requested_char != Some(c) {
                            self.hover_result = None;
                            self.hover_requested_char = None;
                        }
                        self.hover_anchor = Some((c, Instant::now()));
                    }
                }
            }
            None => {
                self.hover_anchor = None;
                self.hover_requested_char = None;
                self.hover_result = None;
            }
        }
        if let Some((pos, text)) = &self.hover_result {
            if !text.is_empty() {
                egui::Area::new(egui::Id::new("lsp_hover_area"))
                    .fixed_pos(egui::pos2(pos.x + 14.0, pos.y + 22.0))
                    .order(egui::Order::Tooltip)
                    .show(ctx, |ui| {
                        egui::Frame::popup(ui.style())
                            .fill(hex(ide_core::theme::BG_SIDEBAR))
                            .show(ui, |ui| {
                                ui.set_max_width(560.0);
                                ui.label(
                                    egui::RichText::new(text)
                                        .monospace()
                                        .size(12.0)
                                        .color(hex("#d4d4d4")),
                                );
                            });
                    });
            }
        }
    }

    fn flash(&mut self, msg: String) {
        self.flash = Some((msg, Instant::now()));
    }

    // ---- 阶段 4 UI ----

    /// 切换文件浏览器的当前目录：重新读取目录项并清空过滤词。
    fn set_file_picker_dir(&mut self, dir: PathBuf) {
        self.file_picker_entries = load_dir_entries(&dir);
        self.file_picker_query.clear();
        self.file_picker_dir = dir;
    }

    /// 打开文件浏览窗口（保留上次所在目录，首次定位到当前文件所在目录）。
    fn open_file_picker(&mut self) {
        self.file_picker_open = true;
        self.file_picker_focus = true;
        self.file_picker_query.clear();
        if self.file_picker_dir.as_os_str().is_empty() {
            let start = self
                .active_tab_path()
                .and_then(|p| p.parent().map(|d| d.to_path_buf()))
                .filter(|d| d.is_dir())
                .unwrap_or_else(|| PathBuf::from(self.project_root()));
            self.set_file_picker_dir(start);
        }
    }

    /// Ctrl+P / File▸Open…：文件系统浏览窗口（可进入工程以外的目录）。
    ///
    /// 与旧版「只在工程文件清单里模糊匹配」不同，这里直接浏览磁盘：
    /// - 输入框：输入**存在的**目录/文件路径后回车 → 进入 / 打开；
    ///   否则当作当前目录的过滤词（打字即过滤）。
    /// - 列表：目录在前，点击目录进入、点击文件打开；`上一层`回上级目录。
    fn draw_file_picker(&mut self, ctx: &egui::Context) {
        if !self.file_picker_open {
            return;
        }
        let mut open = true;
        let mut query = self.file_picker_query.clone();
        let mut go_dir: Option<PathBuf> = None;
        let mut open_path: Option<PathBuf> = None;
        let mut esc = false;
        let mut want_focus = self.file_picker_focus;
        let dir = self.file_picker_dir.clone();
        let entries = self.file_picker_entries.clone();
        let shown = filter_dir_entries(&entries, &query);
        let row_h = 20.0_f32;
        // 窗口尺寸每帧按结果条数算死：内容多时到 360 封顶后滚动，
        // 内容少时窗口自动收窄——这样不会出现「列表很短、窗口却很大」的空洞。
        let rows_h = ((shown.len().max(1) as f32) * row_h).clamp(row_h, 360.0);
        egui::Window::new("Open File  (Ctrl+P)")
            .open(&mut open)
            .collapsible(false)
            .fixed_size([640.0, rows_h + 108.0])
            .anchor(egui::Align2::CENTER_TOP, [0.0, 60.0])
            .show(ctx, |ui| {
                // ---- 输入框：既当路径栏，也当过滤词 ----
                ui.horizontal(|ui| {
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut query)
                            .hint_text("输入路径后回车跳转，或输入名称过滤…")
                            .desired_width((ui.available_width() - 76.0).max(180.0)),
                    );
                    if want_focus {
                        want_focus = false;
                        resp.request_focus();
                    }
                    // 回车：能解析成已存在的路径就跳转/打开，否则保持为过滤词。
                    let entered =
                        resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if entered && !query.trim().is_empty() {
                        let p = resolve_picker_path(&dir, &query);
                        if p.is_dir() {
                            go_dir = Some(p);
                        } else if p.is_file() {
                            open_path = Some(p);
                        }
                    }
                    if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                        esc = true;
                    }
                    if ui.button("上一层").clicked() {
                        if let Some(parent) = dir.parent() {
                            go_dir = Some(parent.to_path_buf());
                        }
                    }
                    if icon_button(ui, IconKind::Refresh, "重新读取当前目录").clicked() {
                        go_dir = Some(dir.clone());
                    }
                });
                ui.separator();
                // 行宽固定成窗口内容宽度：ScrollArea 内部的 `available_width()`
                // 是无界值，直接用它会把窗口一路撑宽。
                let row_w = ui.available_width();
                // ---- 当前目录（可选中复制） ----
                ui.add(
                    egui::Label::new(
                        RichText::new(dir.to_string_lossy())
                            .monospace()
                            .size(11.0)
                            .color(hex("#9cdcfe")),
                    )
                    .selectable(true),
                );
                ui.add_space(2.0);
                // ---- 条目列表：虚拟滚动（大目录也不卡）；条目少时窗口贴合内容高度 ----
                egui::ScrollArea::vertical()
                    .max_height(360.0)
                    .auto_shrink([false, true])
                    .show_rows(ui, row_h, shown.len(), |ui, range| {
                        if shown.is_empty() {
                            ui.label(
                                RichText::new("(空目录 / 无匹配)")
                                    .size(12.0)
                                    .color(hex("#6a6a6a")),
                            );
                            return;
                        }
                        for &i in &shown[range] {
                            let (name, is_dir) = &entries[i];
                            let (icon, color) = if *is_dir {
                                (IconKind::Folder, hex("#dcb67a"))
                            } else {
                                (IconKind::File, file_icon_color(name))
                            };
                            if browser_row(ui, row_w, row_h, icon, name, color) {
                                let p = dir.join(name);
                                if *is_dir {
                                    go_dir = Some(p);
                                } else {
                                    open_path = Some(p);
                                }
                            }
                        }
                    });
            });
        self.file_picker_open = open && !esc;
        self.file_picker_focus = want_focus;
        self.file_picker_query = query;
        if let Some(d) = go_dir {
            self.set_file_picker_dir(d);
            // 换目录后把焦点还给输入框：接着打字就是在新目录里过滤（VS Code 同款手感）。
            self.file_picker_focus = true;
        } else if let Some(p) = open_path {
            match std::fs::read_to_string(&p) {
                Ok(text) => {
                    self.open_file(p, text, true);
                    self.file_picker_open = false;
                }
                Err(e) => self.flash(format!("无法打开 {}: {e}", p.display())),
            }
        }
    }

    /// 打开侧栏「搜索」视图。
    ///
    /// VS Code 语义：Ctrl+Shift+F 聚焦搜索视图，结果就展示在资源管理器所在的侧栏位置。
    fn open_content_search(&mut self) {
        self.sidebar_content = SidebarContent::Search;
        self.settings.show_sidebar = true;
        self.content_search_focus = true;
    }

    /// 用当前输入条件启动一次全文搜索（Enter / 重新搜索按钮）。
    fn start_content_search(&mut self) {
        let q = self.content_query.trim().to_string();
        if q.is_empty() {
            return;
        }
        let root = self.project_root();
        let opts = SearchOptions {
            case_sensitive: self.search_case,
            regex: self.search_regex,
            whole_word: self.search_word,
            glob: Some(self.search_glob.clone()).filter(|g| !g.trim().is_empty()),
        };
        let seq = self.search.search(&root, &q, &opts);
        self.pending_search_seq = Some(seq);
    }

    /// 侧栏「搜索」视图：查询框 + 选项 + 结果列表（VS Code 布局）。
    fn draw_search_content(&mut self, ui: &mut egui::Ui) {
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            ui.label(RichText::new("SEARCH").size(11.0).color(hex("#bbbbbb")));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(4.0);
                if icon_button(ui, IconKind::Refresh, "按当前条件重新搜索").clicked() {
                    self.start_content_search();
                }
            });
        });
        ui.add_space(2.0);

        let mut query = self.content_query.clone();
        let mut do_search = false;
        let edit = ui.add(
            egui::TextEdit::singleline(&mut query)
                .hint_text("在工程中搜索…")
                .desired_width(f32::INFINITY),
        );
        if self.content_search_focus {
            self.content_search_focus = false;
            edit.request_focus();
        }
        if edit.lost_focus()
            && ui.input(|i| i.key_pressed(egui::Key::Enter))
            && !query.trim().is_empty()
        {
            do_search = true;
        }
        ui.horizontal_wrapped(|ui| {
            if ui
                .selectable_label(self.search_case, "Aa")
                .on_hover_text("区分大小写")
                .clicked()
            {
                self.search_case = !self.search_case;
            }
            if ui
                .selectable_label(self.search_regex, ".*")
                .on_hover_text("正则表达式")
                .clicked()
            {
                self.search_regex = !self.search_regex;
            }
            if ui
                .selectable_label(self.search_word, "ab")
                .on_hover_text("全词匹配")
                .clicked()
            {
                self.search_word = !self.search_word;
            }
            ui.label(
                RichText::new(format!("{} 处匹配", self.content_results.len()))
                    .size(11.0)
                    .color(hex("#9cdcfe")),
            );
        });
        ui.add(
            egui::TextEdit::singleline(&mut self.search_glob)
                .hint_text("包含的文件，如 *.cpp")
                .desired_width(f32::INFINITY),
        );
        self.content_query = query;
        if do_search {
            self.start_content_search();
            // 搜索框保持焦点：接着改条件再回车即可（VS Code 同款手感）。
            self.content_search_focus = true;
        }
        ui.separator();

        // 扁平化行列表（文件头 + 每条匹配），show_rows 虚拟滚动；
        // 避免万级匹配结果全量布局拖垮内存/帧率。
        let results = self.content_results.clone();
        let mut jumps: Vec<(String, u64)> = Vec::new();
        #[derive(Clone)]
        enum Row<'a> {
            Header(String, usize),
            Match(&'a SearchMatch),
        }
        let mut rows: Vec<Row<'_>> = Vec::new();
        let mut by_file: HashMap<String, Vec<&SearchMatch>> = HashMap::new();
        for m in &results {
            by_file.entry(m.file.clone()).or_default().push(m);
        }
        // 文件名排序，保证结果顺序稳定（HashMap 迭代顺序随机）。
        let mut files: Vec<&String> = by_file.keys().collect();
        files.sort();
        for file in files {
            let matches = &by_file[file];
            rows.push(Row::Header(file.clone(), matches.len()));
            rows.extend(matches.iter().map(|m| Row::Match(m)));
        }
        if rows.is_empty() {
            ui.add_space(2.0);
            ui.label(
                RichText::new("(无结果)")
                    .size(11.0)
                    .color(hex("#6a6a6a")),
            );
        }
        let row_h = ui.fonts(|f| f.row_height(&egui::FontId::monospace(12.0))) + 3.0;
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show_rows(ui, row_h, rows.len(), |ui, range| {
                for i in range {
                    match &rows[i] {
                        Row::Header(file, n) => {
                            ui.label(
                                RichText::new(format!("{file}  ({n})"))
                                    .monospace()
                                    .size(11.5)
                                    .color(hex("#9cdcfe")),
                            );
                        }
                        Row::Match(m) => {
                            let text = format!("{}  {}", m.line, m.text.trim());
                            if ui
                                .add(
                                    egui::Button::new(
                                        RichText::new(text)
                                            .monospace()
                                            .size(11.0)
                                            .color(hex("#d4d4d4")),
                                    )
                                    .frame(false),
                                )
                                .on_hover_text(format!("{}:{}", m.file, m.line))
                                .clicked()
                            {
                                jumps.push((m.file.clone(), m.line));
                            }
                        }
                    }
                }
            });

        for (file, line) in jumps {
            let root = self.project_root();
            let full = std::path::Path::new(&root).join(&file);
            let win = full.to_string_lossy().into_owned();
            self.jump_to(&win, line.saturating_sub(1) as usize, 0);
        }
    }

    fn draw_git_panel(&mut self, ctx: &egui::Context) {
        if !self.git_panel_open {
            return;
        }
        let mut open = true;
        let status = self.git_status.clone();
        let commits = self.git_log.clone();
        let diff_text = self.git_diff_text.clone();
        let mut commit_input = self.git_commit_input.clone();
        let mut want_refresh = false;
        let mut want_diff: Option<String> = None;
        let mut want_stage: Option<String> = None;
        let mut want_unstage: Option<String> = None;
        let mut want_discard: Option<String> = None;
        let mut want_commit: Option<String> = None;
        egui::Window::new("Git  (Ctrl+Shift+G)")
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_width(680.0)
            .default_height(480.0)
            .anchor(egui::Align2::RIGHT_TOP, [-20.0, 60.0])
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("branch:").size(12.0).color(hex("#bbbbbb")));
                    ui.label(
                        RichText::new(status.branch.clone().unwrap_or_else(|| "(not a repo)".into()))
                            .size(12.0)
                            .color(hex("#dcdcaa")),
                    );
                    if ui.small_button("Refresh").clicked() {
                        want_refresh = true;
                    }
                });
                ui.separator();
                egui::ScrollArea::vertical()
                    .max_height(200.0)
                    .show(ui, |ui| {
                        ui.label(RichText::new("CHANGES").size(11.0).color(hex("#9cdcfe")));
                        if status.entries.is_empty() {
                            ui.label(RichText::new("(working tree clean)").size(11.0).color(hex("#6a6a6a")));
                        }
                        for e in &status.entries {
                            let badge = e.badge();
                            let color = match badge {
                                'M' | 'A' => hex("#73c991"),
                                'D' => hex("#f48771"),
                                '?' => hex("#c5c5c5"),
                                _ => hex("#d4d4d4"),
                            };
                            let staged = e.staged != '.' && e.staged != '?' && e.staged != ' ';
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(format!("{badge}")).monospace().size(12.0).color(color));
                                let path = e.path.clone();
                                if ui
                                    .add(
                                        egui::Button::new(RichText::new(path.as_str()).monospace().size(11.0)),
                                    )
                                    .on_hover_text("查看 diff")
                                    .clicked()
                                {
                                    want_diff = Some(path.clone());
                                }
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if ui
                                            .small_button("↺")
                                            .on_hover_text("放弃修改 (git checkout --)")
                                            .clicked()
                                        {
                                            want_discard = Some(path.clone());
                                        }
                                        if staged
                                            && ui
                                                .small_button("－")
                                                .on_hover_text("取消暂存")
                                                .clicked()
                                        {
                                            want_unstage = Some(path.clone());
                                        }
                                        if ui
                                            .small_button("＋")
                                            .on_hover_text("暂存 (git add)")
                                            .clicked()
                                        {
                                            want_stage = Some(path.clone());
                                        }
                                    },
                                );
                            });
                        }
                    });
                ui.separator();
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut commit_input)
                            .hint_text("commit message...  (Ctrl+Enter)")
                            .desired_width(380.0),
                    );
                    if ui.button("Commit").clicked() && !commit_input.trim().is_empty() {
                        want_commit = Some(commit_input.trim().to_string());
                    }
                });
                ui.separator();
                ui.label(RichText::new("RECENT COMMITS").size(11.0).color(hex("#9cdcfe")));
                egui::ScrollArea::vertical()
                    .max_height(140.0)
                    .show(ui, |ui| {
                        for c in &commits {
                            ui.label(
                                RichText::new(format!("{}  {}  {}  {}", c.hash, c.date, c.author, c.subject))
                                    .monospace()
                                    .size(11.0)
                                    .color(hex("#d4d4d4")),
                            );
                        }
                        if commits.is_empty() {
                            ui.label(RichText::new("(no commits)").size(11.0).color(hex("#6a6a6a")));
                        }
                    });
                if !diff_text.is_empty() {
                    ui.separator();
                    ui.label(RichText::new("DIFF").size(11.0).color(hex("#9cdcfe")));
                    egui::ScrollArea::vertical()
                        .max_height(140.0)
                        .show(ui, |ui| {
                            for line in ide_git::diff::classify_diff(&diff_text) {
                                let color = match line.kind {
                                    ide_git::diff::DiffLineKind::Added => hex("#73c991"),
                                    ide_git::diff::DiffLineKind::Removed => hex("#f48771"),
                                    ide_git::diff::DiffLineKind::HunkHeader => hex("#9cdcfe"),
                                    ide_git::diff::DiffLineKind::Context => hex("#858585"),
                                };
                                ui.label(RichText::new(line.text.as_str()).monospace().size(10.5).color(color));
                            }
                        });
                }
            });
        self.git_panel_open = open;
        self.git_commit_input = commit_input;
        if want_refresh {
            self.refresh_git_now();
        }
        if let Some(path) = want_diff {
            let root = self.project_root();
            self.git.diff(&root, &path, false);
        }
        if let Some(path) = want_stage {
            let root = self.project_root();
            self.git.add(&root, &path);
        }
        if let Some(path) = want_unstage {
            let root = self.project_root();
            self.git.unstage(&root, &path);
        }
        if let Some(path) = want_discard {
            let root = self.project_root();
            self.git.restore(&root, &path);
        }
        if let Some(msg) = want_commit {
            let root = self.project_root();
            self.git.commit(&root, &msg);
            self.git_commit_input.clear();
        }
    }

    fn poll_search(&mut self) {
        while let Some(ev) = self.search.poll() {
            match ev {
                SearchEvent::FilesReady { files } => {
                    self.explorer_files = files;
                    self.rebuild_explorer();
                }
                SearchEvent::FilesError(e) => {
                    tracing::warn!("rg --files 失败: {e}");
                }
                SearchEvent::SearchResults { seq, matches, error } => {
                    if self.pending_search_seq == Some(seq) {
                        self.pending_search_seq = None;
                        self.content_results = matches;
                        if let Some(e) = error {
                            self.flash(format!("搜索失败: {e}"));
                        }
                    }
                }
            }
        }
    }

    fn poll_git(&mut self) {
        while let Some(ev) = self.git.poll() {
            match ev {
                GitEvent::Status(st) => self.git_status = *st,
                GitEvent::Log(commits) => self.git_log = commits,
                GitEvent::Diff { text, .. } => self.git_diff_text = text,
                GitEvent::Done { ok, msg } => {
                    if ok {
                        self.flash(msg);
                        let root = self.project_root();
                        self.git.refresh(&root);
                        self.git.load_log(&root);
                    } else {
                        self.flash(format!("Git: {msg}"));
                    }
                }
            }
        }
    }

    fn refresh_git_now(&self) {
        let root = self.project_root();
        self.git.refresh(&root);
        self.git.load_log(&root);
    }

    // ---- 阶段 5：设置 ----

    fn save_settings(&mut self) {
        if let Err(e) = self.settings.save() {
            tracing::warn!("设置保存失败: {e}");
        }
    }

    fn adjust_font_size(&mut self, delta: f32) {
        let next = (self.settings.font_size + delta).clamp(10.0, 24.0);
        if next == self.settings.font_size {
            return;
        }
        self.settings.font_size = next;
        self.save_settings();
    }

    fn switch_theme(&mut self) {
        let kind = ThemeKind::from_name(&self.settings.theme);
        let next = match kind {
            ThemeKind::DarkPlus => ThemeKind::LightPlus,
            ThemeKind::LightPlus => ThemeKind::CppDark,
            ThemeKind::CppDark => ThemeKind::DarkPlus,
        };
        self.settings.theme = next.name().to_string();
        self.theme_switch_at = Some(Instant::now());
        self.save_settings();
    }

    /// 把当前设置应用到编辑器（字号/主题/minimap）。
    fn apply_settings_to_ui(&mut self) {
        let kind = ThemeKind::from_name(&self.settings.theme);
        let tokens = theme_tokens(&self.host, kind);
        if let Some(ed) = &mut self.editor {
            ed.set_typography(self.settings.font_size);
            ed.set_theme(kind, &tokens);
            ed.set_minimap(self.settings.show_minimap);
        }
        self.applied_font = self.settings.font_size;
        self.applied_theme = kind;
    }

    fn draw_settings(&mut self, ctx: &egui::Context) {
        if !self.settings_open {
            return;
        }
        let mut open = true;
        let mut actions: Vec<SettingsAction> = Vec::new();
        let mut font_size = self.settings.font_size;
        let theme = self.settings.theme.clone();
        let family = self.settings.font_family.clone();
        let mut show_minimap = self.settings.show_minimap;
        let mut show_breadcrumbs = self.settings.show_breadcrumbs;
        let mut show_sidebar = self.settings.show_sidebar;
        let config_path = AppSettings::config_path();
        let theme_kind = ThemeKind::from_name(&theme);
        // 扩展宿主快照（闭包里不再借用 self）。
        let ext_rows: Vec<(String, String, usize, u128, bool)> = self
            .host
            .report()
            .iter()
            .map(|r| {
                (
                    r.id.to_string(),
                    r.name.to_string(),
                    r.registered,
                    r.duration.as_micros(),
                    r.is_active(),
                )
            })
            .collect();
        let lang_count = self.host.registry().languages().len();
        let theme_count = self.host.registry().themes().len();
        egui::Window::new("Settings  (Ctrl+,)")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(420.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(
                    RichText::new(format!("Editor font size: {:.0}  (10-24)", font_size)).size(13.0),
                );
                let slider = ui.add(egui::Slider::new(&mut font_size, 10.0..=24.0));
                if slider.changed() {
                    actions.push(SettingsAction::FontSize(font_size));
                }
                ui.add_space(6.0);
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("Theme:").size(13.0));
                    for kind in ThemeKind::ALL {
                        if ui
                            .selectable_label(theme_kind == kind, kind.name())
                            .clicked()
                        {
                            actions.push(SettingsAction::Theme(kind));
                        }
                    }
                });
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Font:").size(13.0));
                    for f in ["Default", "Consolas", "Cascadia Code"] {
                        if ui.selectable_label(family == f, f).clicked() {
                            actions.push(SettingsAction::Family(f.to_string()));
                        }
                    }
                });
                ui.add_space(6.0);
                if ui.checkbox(&mut show_minimap, "Show minimap").changed() {
                    actions.push(SettingsAction::Minimap(show_minimap));
                }
                if ui.checkbox(&mut show_breadcrumbs, "Show breadcrumbs").changed() {
                    actions.push(SettingsAction::Breadcrumbs(show_breadcrumbs));
                }
                if ui.checkbox(&mut show_sidebar, "Show sidebar").changed() {
                    actions.push(SettingsAction::Sidebar(show_sidebar));
                }
                ui.add_space(6.0);
                if ui.button("Reset to defaults").clicked() {
                    actions.push(SettingsAction::Reset);
                }
                ui.separator();
                ui.label(
                    RichText::new(format!(
                        "Extensions  ({lang_count} language · {theme_count} theme)"
                    ))
                    .size(13.0)
                    .strong(),
                );
                for (id, name, registered, us, active) in &ext_rows {
                    ui.horizontal(|ui| {
                        let (mark, color) = if *active {
                            ("✓", hex("#4ec9b0"))
                        } else {
                            ("✗", hex("#e51400"))
                        };
                        ui.label(RichText::new(mark).color(color).size(12.0));
                        ui.label(RichText::new(name).size(12.0));
                        ui.label(
                            RichText::new(format!("{registered} 贡献点 · {us} us"))
                                .size(10.5)
                                .color(hex("#8a8a8a")),
                        );
                        ui.label(
                            RichText::new(id)
                                .size(10.0)
                                .color(hex("#6a6a6a")),
                        );
                    });
                }
                ui.separator();
                ui.label(
                    RichText::new(format!("Config: {}", config_path.display()))
                        .size(10.5)
                        .color(hex("#6a6a6a")),
                );
                ui.label(
                    RichText::new("JSON 配置文件，外部修改后自动热重载")
                        .size(10.5)
                        .color(hex("#6a6a6a")),
                );
            });
        self.settings_open = open;
        for a in actions {
            match a {
                SettingsAction::FontSize(v) => {
                    self.settings.font_size = v;
                    self.save_settings();
                }
                SettingsAction::Theme(kind) => {
                    self.settings.theme = kind.name().to_string();
                    self.theme_switch_at = Some(Instant::now());
                    self.save_settings();
                }
                SettingsAction::Family(f) => {
                    self.settings.font_family = f;
                    self.save_settings();
                }
                SettingsAction::Minimap(on) => {
                    self.settings.show_minimap = on;
                    if let Some(ed) = &mut self.editor {
                        ed.set_minimap(on);
                    }
                    self.save_settings();
                }
                SettingsAction::Breadcrumbs(on) => {
                    self.settings.show_breadcrumbs = on;
                    self.save_settings();
                }
                SettingsAction::Sidebar(on) => {
                    self.settings.show_sidebar = on;
                    self.save_settings();
                }
                SettingsAction::Reset => {
                    self.settings = AppSettings::default();
                    self.apply_settings_to_ui();
                    self.save_settings();
                }
            }
        }
    }

    // ---- 复核改进：编辑器内查找 / 替换 ----

    /// 打开查找（`replace=false`）或替换（`replace=true`）栏。
    fn open_find(&mut self, replace: bool) {
        self.find_open = true;
        self.find_replace_mode = replace;
        // 用编辑器当前选区预填查找词（VSCode 行为）。
        if let Some(ed) = &self.editor {
            if let Some((s, e)) = ed.current_selection() {
                let sel: String = ed.text().chars().skip(s).take(e.saturating_sub(s)).collect();
                let single_line = !sel.contains('\n') && !sel.is_empty();
                if single_line {
                    self.find_query = sel;
                }
            }
        }
        self.find_last_query.clear(); // 强制重算
        self.find_focus_req = true;
        self.recompute_find(true);
    }

    fn close_find(&mut self) {
        self.find_open = false;
        self.find_replace_mode = false;
    }

    /// 重新计算匹配。`jump_first` 为真时选中第一个命中。
    fn recompute_find(&mut self, jump_first: bool) {
        let query = self.find_query.clone();
        let case = self.find_case;
        self.find_matches = self
            .editor
            .as_ref()
            .map(|e| e.find_matches(&query, case))
            .unwrap_or_default();
        if self.find_matches.is_empty()
            || self.find_current >= self.find_matches.len()
            || jump_first
        {
            self.find_current = 0;
        }
        if self.find_matches.is_empty() && !query.is_empty() && jump_first {
            self.flash("未找到匹配".to_string());
        }
        if jump_first {
            self.select_find_match();
        }
    }

    fn select_find_match(&mut self) {
        let Some(&(s, e)) = self.find_matches.get(self.find_current) else {
            return;
        };
        if let Some(ed) = &mut self.editor {
            ed.select_range(s, e);
            ed.scroll_to_char(s);
        }
    }

    fn find_next(&mut self) {
        if self.find_matches.is_empty() {
            return;
        }
        self.find_current = (self.find_current + 1) % self.find_matches.len();
        self.select_find_match();
    }

    fn find_prev(&mut self) {
        if self.find_matches.is_empty() {
            return;
        }
        self.find_current = if self.find_current == 0 {
            self.find_matches.len() - 1
        } else {
            self.find_current - 1
        };
        self.select_find_match();
    }

    fn replace_current(&mut self) {
        let Some(&(s, e)) = self.find_matches.get(self.find_current) else {
            return;
        };
        let repl = self.find_replace_text.clone();
        if let Some(ed) = &mut self.editor {
            ed.replace_range(s, e, &repl);
        }
        self.find_last_seq = u64::MAX; // 文本已变，强制重算
        self.recompute_find(false);
        if !self.find_matches.is_empty() {
            if self.find_current >= self.find_matches.len() {
                self.find_current = 0;
            }
            self.select_find_match();
        }
    }

    fn replace_all(&mut self) {
        if self.find_query.is_empty() || self.find_matches.is_empty() {
            return;
        }
        let repl = self.find_replace_text.clone();
        let count = self.find_matches.len();
        let matches = self.find_matches.clone();
        if let Some(ed) = &mut self.editor {
            // 从后往前替换，前面匹配的字符索引不失效。
            for &(s, e) in matches.iter().rev() {
                ed.replace_range(s, e, &repl);
            }
        }
        self.find_last_seq = u64::MAX;
        self.recompute_find(false);
        self.flash(format!("已替换 {count} 处"));
    }

    // ---- 复核改进：另存为 ----

    fn open_save_as(&mut self) {
        let name = self
            .tabs
            .active_tab()
            .map(|t| t.title.clone())
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| "untitled.cpp".to_string());
        let root = self.project_root();
        self.save_as_input = format!("{root}\\{name}");
        self.save_as_open = true;
    }

    fn do_save_as(&mut self, path_str: String) {
        let trimmed = path_str.trim();
        if trimmed.is_empty() {
            self.flash("请输入保存路径".to_string());
            return;
        }
        let path = PathBuf::from(trimmed);
        let Some(ed) = &self.editor else { return };
        let text = ed.text();
        if let Some(dir) = path.parent() {
            if !dir.as_os_str().is_empty() && !dir.is_dir() {
                if let Err(e) = std::fs::create_dir_all(dir) {
                    self.flash(format!("创建目录失败: {e}"));
                    return;
                }
            }
        }
        if let Err(e) = std::fs::write(&path, &text) {
            self.flash(format!("另存为失败: {e}"));
            return;
        }
        // 旧文档从 clangd 侧关闭（真实文件改名时）。
        if let Some(old) = self.current_uri.clone() {
            if self.lsp_status.is_ready() {
                self.lsp.close_document(old);
            }
        }
        let idx = self.tabs.active();
        if let Some(t) = self.tabs.get_mut(idx) {
            t.path = Some(path.clone());
            t.title = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| trimmed.to_string());
            t.dirty = false;
            t.text = text;
        }
        self.current_file = Some(path.clone());
        self.current_uri = win_to_uri(&path.to_string_lossy());
        self.editor_clean_seq = self.editor.as_ref().map(|e| e.edit_seq()).unwrap_or(0);
        self.save_as_open = false;
        self.flash(format!("已保存 {}", path.display()));
        // 让 clangd 重新 open 新路径。
        self.lsp_ready_synced = false;
        self.last_sync_seq = 0;
    }

    // ---- 阶段 5：命令面板 ----

    /// 执行一条命令（命令面板 / 守护测试共用）。
    fn run_command(&mut self, id: &str, ctx: &egui::Context) {
        match id {
            "file.newFile" => self.on_new_file(),
            "file.openFile" => {
                if self.file_picker_open {
                    self.file_picker_open = false;
                } else {
                    self.open_file_picker();
                }
            }
            "file.save" => self.on_save(),
            "file.saveAll" => self.on_save_all(),
            "file.saveAs" => self.open_save_as(),
            "file.close" => self.on_close_tab(),
            "file.closeAll" => self.on_close_all(),
            "file.reopenClosed" => self.on_reopen_closed(),
            "file.reload" => self.on_reload_active(),
            "file.copyPath" => {
                if let Some(p) = self.active_tab_path() {
                    ctx.output_mut(|o| o.copied_text = p.to_string_lossy().into_owned());
                    self.flash("已复制文件路径".to_string());
                } else {
                    self.flash("没有打开的文件".to_string());
                }
            }
            "edit.undo" => {
                if let Some(ed) = &mut self.editor {
                    ed.undo();
                }
            }
            "edit.redo" => {
                if let Some(ed) = &mut self.editor {
                    ed.redo();
                }
            }
            "edit.selectAll" => {
                if let Some(ed) = &mut self.editor {
                    ed.select_all();
                }
            }
            "edit.cut" => {
                if let Some(ed) = &mut self.editor {
                    ed.cut_to_clipboard(ctx);
                }
            }
            "edit.copy" => {
                if let Some(ed) = &mut self.editor {
                    ed.copy_to_clipboard(ctx);
                }
            }
            "edit.paste" => {
                let pasted = ctx.input(|i| {
                    i.events.iter().find_map(|e| match e {
                        egui::Event::Paste(t) => Some(t.clone()),
                        _ => None,
                    })
                });
                match pasted {
                    Some(t) => {
                        if let Some(ed) = &mut self.editor {
                            ed.paste_text(&t);
                        }
                    }
                    None => self.flash("剪贴板没有可粘贴内容".to_string()),
                }
            }
            "edit.deleteLine" => {
                if let Some(ed) = &mut self.editor {
                    ed.delete_line();
                }
            }
            "edit.selectLine" => {
                if let Some(ed) = &mut self.editor {
                    ed.select_line();
                }
            }
            "edit.toggleComment" => {
                if let Some(ed) = &mut self.editor {
                    ed.toggle_line_comment();
                }
            }
            "edit.duplicateLine" => {
                if let Some(ed) = &mut self.editor {
                    ed.duplicate_line();
                }
            }
            "edit.find" => self.open_find(false),
            "edit.replace" => self.open_find(true),
            "view.toggleSidebar" => {
                self.settings.show_sidebar = !self.settings.show_sidebar;
                self.save_settings();
            }
            "view.toggleMinimap" => {
                self.settings.show_minimap = !self.settings.show_minimap;
                if let Some(ed) = &mut self.editor {
                    ed.set_minimap(self.settings.show_minimap);
                }
                self.save_settings();
            }
            "view.toggleBreadcrumbs" => {
                self.settings.show_breadcrumbs = !self.settings.show_breadcrumbs;
                self.save_settings();
            }
            "view.showExplorer" => {
                self.sidebar_content = SidebarContent::Explorer;
                self.settings.show_sidebar = true;
            }
            "view.showOutline" => {
                self.sidebar_content = SidebarContent::Outline;
                self.settings.show_sidebar = true;
            }
            "view.zoomIn" => self.adjust_font_size(1.0),
            "view.zoomOut" => self.adjust_font_size(-1.0),
            "view.resetFontSize" => {
                self.settings.font_size = 14.0;
                self.save_settings();
            }
            "view.switchTheme" => self.switch_theme(),
            "go.definition" => self.on_goto_definition(),
            "go.back" => self.on_go_back(),
            "go.line" => {
                self.goto_line_open = true;
                self.goto_line_input.clear();
            }
            "go.symbol" => {
                self.symbol_search_open = !self.symbol_search_open;
                self.symbol_filter.clear();
            }
            "search.file" => {
                if self.file_picker_open {
                    self.file_picker_open = false;
                } else {
                    self.open_file_picker();
                }
            }
            "search.content" => self.open_content_search(),
            "search.symbol" => {
                self.symbol_search_open = !self.symbol_search_open;
                self.symbol_filter.clear();
            }
            "search.refreshFiles" => {
                let root = self.project_root();
                self.search.load_files(&root);
                self.flash("正在刷新文件清单".to_string());
            }
            "debug.start" => self.on_debug_start(),
            "debug.stop" => self.on_debug_stop(),
            "debug.toggleBreakpoint" => self.on_toggle_breakpoint(),
            "debug.stepOver" => self.on_debug_next(),
            "debug.stepInto" => self.on_debug_step(),
            "debug.stepOut" => self.on_debug_finish(),
            "debug.addWatch" => self.on_add_watch(),
            "git.openPanel" => {
                self.git_panel_open = true;
                self.refresh_git_now();
            }
            "git.refresh" => self.refresh_git_now(),
            "git.commit" => {
                let msg = self.git_commit_input.clone();
                if msg.trim().is_empty() {
                    self.flash("请先输入提交信息".to_string());
                } else {
                    let root = self.project_root();
                    self.git.commit(&root, msg.trim());
                    self.git_commit_input.clear();
                }
            }
            "git.diffActive" => {
                if let Some(p) = self.active_tab_path() {
                    let root = self.project_root();
                    self.git.diff(&root, &p.to_string_lossy(), false);
                    self.git_panel_open = true;
                } else {
                    self.flash("没有打开的文件".to_string());
                }
            }
            "git.showHistory" => {
                self.git_panel_open = true;
                self.refresh_git_now();
            }
            "settings.open" => {
                self.settings_open = !self.settings_open;
            }
            "settings.increaseFont" => self.adjust_font_size(1.0),
            "settings.decreaseFont" => self.adjust_font_size(-1.0),
            "settings.reset" => {
                self.settings = AppSettings::default();
                self.apply_settings_to_ui();
                self.save_settings();
            }
            "help.keybindings" => {
                self.keys_open = true;
            }
            "help.about" => {
                self.about_open = true;
            }
            _ => {}
        }
    }

    fn draw_command_palette(&mut self, ctx: &egui::Context) {
        if !self.palette_open {
            return;
        }
        // 200ms 缓动：打开时从上方滑入
        let t = ctx.animate_bool_with_time(egui::Id::new("palette_anim"), true, 0.2);
        let dy = (1.0 - t) * 24.0;

        let mut query = self.palette_query.clone();
        let mut sel = self.palette_sel;
        let mut exec: Option<&'static str> = None;
        let mut do_close = false;
        let mut focus_req = self.palette_focus_req;

        egui::Area::new(egui::Id::new("cmd_palette"))
            .anchor(egui::Align2::CENTER_TOP, [0.0, 90.0 + dy])
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style())
                    .fill(hex(ide_core::theme::BG_SIDEBAR))
                    .show(ui, |ui| {
                        ui.set_width(560.0);
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(">")
                                    .size(14.0)
                                    .color(hex("#9cdcfe")),
                            );
                            let edit = ui.add(
                                egui::TextEdit::singleline(&mut query)
                                    .hint_text("Type a command name…")
                                    .desired_width(510.0),
                            );
                            if focus_req {
                                edit.request_focus();
                                focus_req = false;
                            }
                        });
                        ui.separator();
                        let matches = commands::filter_commands(&query, 12);
                        let down = ui.input(|i| i.key_pressed(egui::Key::ArrowDown));
                        let up = ui.input(|i| i.key_pressed(egui::Key::ArrowUp));
                        let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
                        let esc = ui.input(|i| i.key_pressed(egui::Key::Escape));
                        if down {
                            sel = (sel + 1).min(matches.len().saturating_sub(1));
                        }
                        if up {
                            sel = sel.saturating_sub(1);
                        }
                        if enter && !matches.is_empty() {
                            if let Some(c) = matches.get(sel.min(matches.len() - 1)) {
                                exec = Some(c.id);
                            }
                            do_close = true;
                        }
                        if esc {
                            do_close = true;
                        }
                        egui::ScrollArea::vertical()
                            .max_height(340.0)
                            .show(ui, |ui| {
                                if matches.is_empty() {
                                    ui.label(
                                        RichText::new("(no matching command)")
                                            .size(12.0)
                                            .color(hex("#6a6a6a")),
                                    );
                                }
                                for (i, c) in matches.iter().enumerate() {
                                    let resp = ui.add(
                                        egui::Label::new(
                                            RichText::new(format!(
                                                "{:<28}  {:>4}  {}",
                                                c.title,
                                                c.shortcut,
                                                c.category
                                            ))
                                            .size(13.0)
                                            .color(if i == sel {
                                                hex("#ffffff")
                                            } else {
                                                hex("#d4d4d4")
                                            }),
                                        )
                                        .sense(egui::Sense::click()),
                                    );
                                    if i == sel {
                                        ui.painter().rect_filled(
                                            resp.rect,
                                            3.0,
                                            hex("#094771"),
                                        );
                                    }
                                    if resp.clicked() {
                                        exec = Some(c.id);
                                        do_close = true;
                                    }
                                }
                            });
                    });
            });

        self.palette_query = query;
        self.palette_sel = sel;
        self.palette_focus_req = focus_req;
        if let Some(id) = exec {
            self.run_command(id, ctx);
        }
        if do_close {
            self.palette_open = false;
        }
    }

    fn draw_goto_line(&mut self, ctx: &egui::Context) {
        if !self.goto_line_open {
            return;
        }
        let mut open = true;
        let mut input = self.goto_line_input.clone();
        let mut do_jump: Option<usize> = None;
        egui::Window::new("Go to Line  (Ctrl+G)")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(260.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, -80.0])
            .show(ctx, |ui| {
                let edit = ui.add(
                    egui::TextEdit::singleline(&mut input)
                        .hint_text("line number")
                        .desired_width(200.0),
                );
                let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if enter {
                    if let Ok(n) = input.trim().parse::<usize>() {
                        do_jump = Some(n);
                    }
                }
            });
        self.goto_line_open = open;
        self.goto_line_input = input;
        if let Some(n) = do_jump {
            let line = n.saturating_sub(1);
            if let Some(ed) = &mut self.editor {
                ed.set_cursor_line_col(line, 0);
            }
            self.goto_line_open = false;
        }
    }

    /// 编辑器内查找/替换浮窗（Ctrl+F / Ctrl+H）。
    fn draw_find_bar(&mut self, ctx: &egui::Context) {
        if !self.find_open {
            return;
        }
        // 查询或文本变化时重算匹配
        let seq = self.editor.as_ref().map(|e| e.edit_seq()).unwrap_or(0);
        let query_changed = self.find_query != self.find_last_query;
        if query_changed || seq != self.find_last_seq {
            self.find_last_query = self.find_query.clone();
            self.find_last_seq = seq;
            self.recompute_find(query_changed);
        }

        let total = self.find_matches.len();
        let cur = if total == 0 { 0 } else { self.find_current + 1 };
        let count_label = format!("{cur}/{total}");
        let mut want_next = false;
        let mut want_prev = false;
        let mut want_replace = false;
        let mut want_replace_all = false;
        let mut close = false;
        let focus_req = self.find_focus_req;
        self.find_focus_req = false;

        egui::Window::new("find_bar")
            .title_bar(false)
            .resizable(false)
            .collapsible(false)
            .default_width(500.0)
            .anchor(egui::Align2::RIGHT_TOP, [-24.0, 92.0])
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("查找").size(12.0).color(hex("#9cdcfe")));
                    let edit = ui.add(
                        egui::TextEdit::singleline(&mut self.find_query)
                            .hint_text("search...")
                            .desired_width(200.0),
                    );
                    if focus_req {
                        edit.request_focus();
                    }
                    ui.label(RichText::new(&count_label).size(11.0).color(hex("#9cdcfe")));
                    if ui.small_button("↑").on_hover_text("上一个 (Shift+Enter)").clicked() {
                        want_prev = true;
                    }
                    if ui.small_button("↓").on_hover_text("下一个 (Enter)").clicked() {
                        want_next = true;
                    }
                    let mut case = self.find_case;
                    if ui
                        .selectable_label(case, "Aa")
                        .on_hover_text("区分大小写")
                        .clicked()
                    {
                        case = !case;
                    }
                    self.find_case = case;
                    if ui.small_button("✕").on_hover_text("关闭 (Esc)").clicked() {
                        close = true;
                    }

                    let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    let shift = ui.input(|i| i.modifiers.shift);
                    let esc = ui.input(|i| i.key_pressed(egui::Key::Escape));
                    if enter {
                        if shift {
                            want_prev = true;
                        } else {
                            want_next = true;
                        }
                    }
                    if esc {
                        close = true;
                    }
                });
                if self.find_replace_mode {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("替换").size(12.0).color(hex("#9cdcfe")));
                        ui.add(
                            egui::TextEdit::singleline(&mut self.find_replace_text)
                                .hint_text("replace...")
                                .desired_width(200.0),
                        );
                        if ui.button("替换").clicked() {
                            want_replace = true;
                        }
                        if ui.button("全部替换").clicked() {
                            want_replace_all = true;
                        }
                    });
                }
            });

        if close {
            self.close_find();
        } else {
            if want_next {
                self.find_next();
            }
            if want_prev {
                self.find_prev();
            }
            if want_replace {
                self.replace_current();
            }
            if want_replace_all {
                self.replace_all();
            }
        }
    }

    /// 另存为（未命名文件或改路径保存）。
    fn draw_save_as(&mut self, ctx: &egui::Context) {
        if !self.save_as_open {
            return;
        }
        let mut open = true;
        let mut path = self.save_as_input.clone();
        let mut confirm = false;
        let mut cancelled = false;
        egui::Window::new("Save As  (Ctrl+Shift+S)")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(560.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(
                    RichText::new("保存到（绝对路径）")
                        .size(12.0)
                        .color(hex("#9cdcfe")),
                );
                let edit = ui.add(
                    egui::TextEdit::singleline(&mut path)
                        .hint_text("C:\\path\\to\\file.cpp")
                        .desired_width(520.0),
                );
                let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                ui.horizontal(|ui| {
                    if ui.button("保存").clicked() {
                        confirm = true;
                    }
                    if ui.button("取消").clicked() {
                        cancelled = true;
                    }
                });
                if enter {
                    confirm = true;
                }
            });
        self.save_as_input = path;
        if confirm {
            let p = self.save_as_input.clone();
            self.do_save_as(p);
        } else if cancelled {
            self.save_as_open = false;
        } else {
            self.save_as_open = open;
        }
    }

    fn draw_about(&mut self, ctx: &egui::Context) {
        if !self.about_open {
            return;
        }
        let mut open = true;
        egui::Window::new("About LiteIDE")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(380.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, -120.0])
            .show(ctx, |ui| {
                ui.label(
                    RichText::new(format!("LiteIDE v{}", env!("CARGO_PKG_VERSION")))
                        .size(16.0)
                        .strong(),
                );
                ui.label(
                    RichText::new(
                        "Rust 编写的轻量 IDE：LSP(clangd) + DAP(gdb) + ripgrep 搜索 + Git，\n\
                         对标 VSCode Dark+/Light+。Windows + WSL2 下开发 C++",
                    )
                    .size(12.0)
                    .color(hex("#d4d4d4")),
                );
                ui.separator();
                ui.label(
                    RichText::new("Ctrl+Shift+P 打开命令面板 · Ctrl+, 设置 · Ctrl+W 关闭标签")
                        .size(11.0)
                        .color(hex("#6a6a6a")),
                );
            });
        self.about_open = open;
    }

    fn draw_keys(&mut self, ctx: &egui::Context) {
        if !self.keys_open {
            return;
        }
        let mut open = true;
        egui::Window::new("Keyboard Shortcuts")
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_width(520.0)
            .default_height(480.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for c in commands::registry() {
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new(c.title)
                                        .size(12.0)
                                        .color(hex("#d4d4d4")),
                                );
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        ui.label(
                                            RichText::new(c.shortcut)
                                                .monospace()
                                                .size(11.0)
                                                .color(hex("#9cdcfe")),
                                        );
                                    },
                                );
                            });
                        }
                    });
            });
        self.keys_open = open;
    }

    // ---- 阶段 5：标签栏 / 面包屑 / 侧边栏 ----

    fn draw_tab_bar(&mut self, ui: &mut egui::Ui) {
        if self.tabs.is_empty() {
            return;
        }
        let p = palette(self.applied_theme);
        let n = self.tabs.len();
        let mut clicked: Option<usize> = None;
        let mut close: Option<usize> = None;
        let mut pin: Option<usize> = None;
        let mut last_tab_click = self.tab_last_click;
        ui.horizontal(|ui| {
            ui.set_min_height(34.0);
            for i in 0..n {
                let (title, dirty, active, preview) = {
                    let Some(t) = self.tabs.get(i) else { continue };
                    let preview = self.tabs.is_preview(i);
                    (t.title.clone(), t.dirty, i == self.tabs.active(), preview)
                };
                let mut label = title.clone();
                if dirty {
                    label.push_str(" — ");
                }
                let text_color = if active { hex("#ffffff") } else { hex("#999999") };
                let bg = if active { hex(p.bg_editor) } else { egui::Color32::TRANSPARENT };
                // 预览（未锁定）标签用斜体区分，与 VS Code 一致。
                let mut text =
                    egui::RichText::new(&label).monospace().size(12.0).color(text_color);
                if preview {
                    text = text.italics();
                }
                let mut resp = ui
                    .add(
                        egui::Button::new(text)
                            .fill(bg)
                            .min_size(egui::vec2(0.0, 30.0))
                            .stroke(egui::Stroke::new(0.0f32, egui::Color32::TRANSPARENT)),
                    )
                    .on_hover_cursor(egui::CursorIcon::PointingHand);
                if preview {
                    resp = resp.on_hover_text(
                        "预览标签：双击标签页（或在文件中编辑）即锁定，否则下次打开文件会被替换",
                    );
                }
                if active {
                    // 激活标签：上边缘加一条蓝色细条
                    ui.painter().rect_filled(
                        egui::Rect::from_min_size(
                            resp.rect.left_top(),
                            egui::vec2(resp.rect.width(), TAB_ACTIVE_INDICATOR_H),
                        ),
                        0.0,
                        hex(p.bg_statusbar),
                    );
                }
                if resp.clicked() || resp.double_clicked() {
                    let now = ui.input(|i| i.time);
                    let double = resp.double_clicked()
                        || matches!(last_tab_click, Some((j, t)) if j == i && now - t < DOUBLE_CLICK_SECS);
                    last_tab_click = Some((i, now));
                    clicked = Some(i);
                    if double {
                        pin = Some(i); // 双击标签页 → 锁定
                    }
                }
                if resp.middle_clicked() {
                    close = Some(i); // 中键 → 关闭（VS Code 习惯）
                }
                if ui.small_button("×").on_hover_text("关闭标签").clicked() {
                    close = Some(i);
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let dirty = self.tabs.dirty_count();
                let badge = if dirty > 0 {
                    format!("{} tabs · {} dirty", n, dirty)
                } else {
                    format!("{} tabs", n)
                };
                ui.label(
                    egui::RichText::new(badge)
                        .size(10.5)
                        .color(hex("#6a6a6a")),
                );
            });
        });
        self.tab_last_click = last_tab_click;
        if let Some(i) = clicked {
            self.switch_to_tab(i);
        }
        if let Some(i) = pin {
            self.tabs.pin(i);
        }
        if let Some(i) = close {
            self.close_tab_at(i);
        }
    }

    fn draw_breadcrumb(&mut self, ui: &mut egui::Ui) {
        let Some(tab) = self.tabs.active_tab().cloned() else { return };
        let path = tab
            .path
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| tab.title.clone());
        let dirty = tab.dirty;
        ui.horizontal(|ui| {
            ui.set_min_height(20.0);
            let sep = if path.contains('\\') { '\\' } else { '/' };
            let segs: Vec<&str> = path.split(sep).filter(|s| !s.is_empty()).collect();
            let text = segs.join(" — ");
            ui.label(
                RichText::new(format!("{}  {}", text, if dirty { "●" } else { "" }))
                    .monospace()
                    .size(11.0)
                    .color(hex("#9cdcfe")),
            );
            ui.separator();
            ui.label(
                RichText::new(format!("{}  UTF-8", self.indent_hint))
                    .size(11.0)
                    .color(hex("#6a6a6a")),
            );
        });
        ui.separator();
    }

    fn draw_menu_bar(&mut self, ctx: &egui::Context) {
        let bg = palette(self.applied_theme).bg_sidebar;
        egui::TopBottomPanel::top("menu_bar")
            .exact_height(26.0)
            .frame(egui::Frame::none().fill(hex(bg)))
            .show(ctx, |ui| {
                egui::menu::bar(ui, |ui| {
                    ui.menu_button("File", |ui| {
                        if ui.button("New File").clicked() {
                            self.on_new_file();
                            ui.close_menu();
                        }
                        if ui.button("Open…").clicked() {
                            self.open_file_picker();
                            ui.close_menu();
                        }
                        if ui.button("Save").clicked() {
                            self.on_save();
                            ui.close_menu();
                        }
                        if ui.button("Save As…").clicked() {
                            self.open_save_as();
                            ui.close_menu();
                        }
                        if ui.button("Save All").clicked() {
                            self.on_save_all();
                            ui.close_menu();
                        }
                        ui.separator();
                        if ui.button("Quit").clicked() {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                    });
                    ui.menu_button("Edit", |ui| {
                        if ui.button("Undo").clicked() {
                            if let Some(ed) = self.editor.as_mut() {
                                ed.undo();
                            }
                            ui.close_menu();
                        }
                        if ui.button("Redo").clicked() {
                            if let Some(ed) = self.editor.as_mut() {
                                ed.redo();
                            }
                            ui.close_menu();
                        }
                        ui.separator();
                        if ui.button("Find…").clicked() {
                            self.open_find(false);
                            ui.close_menu();
                        }
                        if ui.button("Replace…").clicked() {
                            self.open_find(true);
                            ui.close_menu();
                        }
                    });
                    ui.menu_button("View", |ui| {
                        let mut sb = self.settings.show_sidebar;
                        if ui.checkbox(&mut sb, "Side Bar").changed() {
                            self.settings.show_sidebar = sb;
                            self.save_settings();
                        }
                        let mut dbg = self.debug_open;
                        if ui.checkbox(&mut dbg, "Debug Panel").changed() {
                            self.debug_open = dbg;
                        }
                        ui.separator();
                        if ui.button("Command Palette…").clicked() {
                            self.palette_open = true;
                            ui.close_menu();
                        }
                    });
                    ui.menu_button("Go", |ui| {
                        if ui.button("Go to Line…").clicked() {
                            self.goto_line_open = true;
                            self.goto_line_input.clear();
                            ui.close_menu();
                        }
                    });
                    ui.menu_button("Run", |ui| {
                        if ui.button("Start Debugging").clicked() {
                            self.on_debug_start();
                            self.debug_open = true;
                            ui.close_menu();
                        }
                        if ui.button("Stop Debugging").clicked() {
                            self.on_debug_stop();
                            ui.close_menu();
                        }
                    });
                    ui.menu_button("Help", |ui| {
                        if ui.button("About LiteIDE").clicked() {
                            self.about_open = true;
                            ui.close_menu();
                        }
                    });
                });
            });
    }

    fn draw_tab_strip(&mut self, ctx: &egui::Context) {
        let p = palette(self.applied_theme);
        egui::TopBottomPanel::top("tab_strip")
            .exact_height(34.0)
            .frame(egui::Frame::none().fill(hex(p.bg_sidebar)))
            .show(ctx, |ui| {
                self.draw_tab_bar(ui);
            });
    }

    /// 左侧活动栏（VSCode 风格，固定窄列，独立于侧边栏，始终可见）。
    fn draw_activity_bar(&mut self, ctx: &egui::Context) {
        let p = palette(self.applied_theme);
        let bg = hex(p.bg_activitybar);
        let accent = egui::Color32::from_rgb(0x2a, 0x7d, 0xd6);
        egui::SidePanel::left("activity_bar")
            .exact_width(48.0)
            .resizable(false)
            .frame(egui::Frame::none().fill(bg))
            .show(ctx, |ui| {
                ui.add_space(4.0);
                ui.vertical_centered(|ui| {
                let item = |ui: &mut egui::Ui, icon: IconKind, tip: &str, on: bool| -> bool {
                    let (rect, resp) =
                        ui.allocate_exact_size(egui::vec2(46.0, 44.0), egui::Sense::click());
                    let color = if on {
                        egui::Color32::WHITE
                    } else {
                        hex("#8a8a8a")
                    };
                    let painter = ui.painter();
                    if on || resp.hovered() {
                        painter.rect_filled(
                            rect.shrink2(egui::vec2(5.0, 4.0)),
                            3.0,
                            egui::Color32::WHITE.gamma_multiply(if on { 0.10 } else { 0.06 }),
                        );
                    }
                    if on {
                        painter.rect_filled(
                            egui::Rect::from_min_size(
                                egui::pos2(rect.left(), rect.top() + 4.0),
                                egui::vec2(2.0, rect.height() - 8.0),
                            ),
                            0.0,
                            accent,
                        );
                    }
                    draw_icon(painter, rect.center(), 22.0, icon, color);
                    resp.on_hover_text(tip).clicked()
                };
                // 高亮跟随“当前选中视图”，与侧栏是否可见解耦（VS Code 的 checked 语义）：
                // 拖分割线收起 / Ctrl+B 隐藏后，活动栏按钮仍保持选中高亮，不会“丢失焦点”。
                let explorer_on = self.sidebar_content == SidebarContent::Explorer;
                if item(ui, IconKind::Explorer, "资源管理器 (Ctrl+B)", explorer_on) {
                    if explorer_on && self.settings.show_sidebar {
                        // 再次点击已选中的视图 → 收起侧栏
                        self.settings.show_sidebar = false;
                    } else {
                        self.sidebar_content = SidebarContent::Explorer;
                        self.settings.show_sidebar = true;
                    }
                    self.save_settings();
                }
                // 搜索视图与 VS Code 一致：占用侧栏位置（资源管理器所在处），
                // 再次点击已选中的搜索图标 → 收起侧栏。
                let search_on = self.sidebar_content == SidebarContent::Search;
                if item(ui, IconKind::Search, "搜索 (Ctrl+Shift+F)", search_on) {
                    if search_on && self.settings.show_sidebar {
                        self.settings.show_sidebar = false;
                    } else {
                        self.sidebar_content = SidebarContent::Search;
                        self.settings.show_sidebar = true;
                        // 与 VS Code 一致：点开搜索视图即把焦点给搜索框，可直接输入。
                        self.content_search_focus = true;
                    }
                    self.save_settings();
                }
                if item(
                    ui,
                    IconKind::SourceControl,
                    "源代码管理 (Ctrl+Shift+G)",
                    self.git_panel_open,
                ) {
                    self.git_panel_open = !self.git_panel_open;
                    if self.git_panel_open {
                        self.refresh_git_now();
                    }
                }
                if item(ui, IconKind::Run, "运行与调试 (F5)", self.debug_open) {
                    self.debug_open = !self.debug_open;
                }
                let outline_on = self.sidebar_content == SidebarContent::Outline;
                if item(ui, IconKind::Outline, "大纲", outline_on) {
                    if outline_on && self.settings.show_sidebar {
                        self.settings.show_sidebar = false;
                    } else {
                        self.sidebar_content = SidebarContent::Outline;
                        self.settings.show_sidebar = true;
                    }
                    self.save_settings();
                }
                ui.add_space(8.0);
                if item(ui, IconKind::Settings, "设置 (Ctrl+,)", self.settings_open) {
                    self.settings_open = !self.settings_open;
                }
                });
            });
    }

    fn draw_sidebar(&mut self, ctx: &egui::Context) {
        let resize_id = egui::Id::new("sidebar_resize");
        // 读上一帧的拖拽响应，用于「跟手」判断并检测松开（避免一帧延迟）。
        // 额外要求指针确实按下：面板隐藏后手柄不再被注册，避免读到残留响应导致状态粘住。
        let dragging = self.settings.show_sidebar
            && ctx.input(|i| i.pointer.primary_down())
            && ctx.read_response(resize_id).is_some_and(|r| r.dragged());
        // 拖到最左后**在松开时**收起：拖拽途中面板保持可见、手柄不被回收，
        // 避免拖拽中途手柄消失导致松开被编辑器误接（误选文本/丢焦点）。
        if self.sidebar_dragging && !dragging && self.sidebar_width <= SIDEBAR_MIN_WIDTH + 0.5 {
            self.settings.show_sidebar = false;
            self.sidebar_width = SIDEBAR_DEFAULT_WIDTH;
            self.save_settings();
        }
        self.sidebar_dragging = dragging;

        let open = self.settings.show_sidebar;
        let target = if open { self.sidebar_width } else { 0.0 };
        // 始终推进动画，保证拖拽结束/收起时数值连续、不跳变。
        let animated = ctx.animate_value_with_time(
            egui::Id::new("sidebar_anim"),
            target,
            SIDEBAR_ANIM_TIME,
        );
        // 拖拽展开时立即跟手；其余（展开/收起）走缓动。
        let width = if dragging && open { target } else { animated };
        if width < 0.5 {
            return; // 完全收起：不占布局，编辑器铺满
        }

        let bg = palette(self.applied_theme).bg_sidebar;
        egui::SidePanel::left("sidebar_panel")
            .resizable(false)
            .exact_width(width)
            .frame(egui::Frame::none().fill(hex(bg)))
            .show(ctx, |ui| {
                ui.add_space(4.0);
                match self.sidebar_content {
                    SidebarContent::Explorer => self.draw_explorer_content(ui),
                    SidebarContent::Search => self.draw_search_content(ui),
                    SidebarContent::Outline => self.draw_outline_content(ui),
                }

                // ---- 右侧分割线：可拖动“挤压”编辑器；拖到最左松开则滑走收起 ----
                if open {
                    let pr = ui.max_rect();
                    let handle = egui::Rect::from_min_max(
                        egui::pos2(pr.right() - 6.0, pr.top()),
                        egui::pos2(pr.right(), pr.bottom()),
                    );
                    let resp = ui.interact(handle, resize_id, egui::Sense::drag());
                    let hovered = resp.hovered() || dragging;
                    if hovered {
                        ctx.set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                    }
                    if resp.dragged() {
                        self.sidebar_width = sidebar_drag(self.sidebar_width, resp.drag_delta().x);
                    }
                    let line_color = if hovered {
                        hex("#4a9eff")
                    } else {
                        hex("#3a3a3a")
                    };
                    ui.painter().vline(
                        pr.right() - 0.5,
                        pr.y_range(),
                        egui::Stroke::new(1.0_f32, line_color),
                    );
                }
            });
    }

    /// 文件清单变化后重建资源管理器文件树与可见行。
    fn rebuild_explorer(&mut self) {
        self.explorer_tree = tree::FileTree::build(&self.explorer_files);
        self.explorer_rows = self.explorer_tree.rows(&self.explorer_expanded);
    }

    fn draw_explorer_content(&mut self, ui: &mut egui::Ui) {
        // 标题栏：EXPLORER + 右侧“重新扫描”
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            ui.label(RichText::new("EXPLORER").size(11.0).color(hex("#bbbbbb")));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(4.0);
                if icon_button(ui, IconKind::Refresh, "重新扫描（rg --files）").clicked() {
                    let root = self.project_root();
                    self.search.load_files(&root);
                }
            });
        });
        ui.add_space(2.0);

        if !self.explorer_loaded {
            self.explorer_loaded = true;
            let root = self.project_root();
            self.search.load_files(&root);
        }

        // 统计行（文件数）
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            ui.label(
                RichText::new(format!("{} files", self.explorer_tree.file_count))
                    .size(10.5)
                    .color(hex("#6a6a6a")),
            );
        });
        ui.add_space(2.0);

        // 虚拟滚动：只绘制可见行（文件可能上万）。
        let rows = &self.explorer_rows;
        let expanded = &self.explorer_expanded;
        let mut clicked: Option<(usize, bool)> = None;
        let mut last_click = self.explorer_last_click;
        let row_h = 22.0_f32;
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show_rows(ui, row_h, rows.len(), |ui, range| {
                if rows.is_empty() {
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new("(正在加载…或目录为空)")
                            .size(12.0)
                            .color(hex("#6a6a6a")),
                    );
                    return;
                }
                for i in range {
                    let row = &rows[i];
                    let (rect, resp) = ui.allocate_exact_size(
                        egui::vec2(ui.available_width(), row_h),
                        egui::Sense::click(),
                    );
                    let painter = ui.painter();
                    if resp.hovered() {
                        painter.rect_filled(rect, 0.0, hex("#2a2d2e"));
                    }
                    let indent = 6.0 + row.depth as f32 * 12.0;
                    let cy = rect.center().y;
                    let is_open = row.is_dir && expanded.contains(&row.path);
                    if row.is_dir {
                        draw_chevron(
                            painter,
                            egui::Rect::from_center_size(
                                egui::pos2(rect.left() + indent + 5.0, cy),
                                egui::vec2(10.0, 10.0),
                            ),
                            is_open,
                            hex("#cccccc"),
                        );
                        draw_icon(
                            painter,
                            egui::pos2(rect.left() + indent + 19.0, cy),
                            16.0,
                            if is_open {
                                IconKind::FolderOpen
                            } else {
                                IconKind::Folder
                            },
                            hex("#dcb67a"),
                        );
                    } else {
                        draw_icon(
                            painter,
                            egui::pos2(rect.left() + indent + 19.0, cy),
                            16.0,
                            IconKind::File,
                            file_icon_color(&row.name),
                        );
                    }
                    painter.text(
                        egui::pos2(rect.left() + indent + 31.0, cy),
                        egui::Align2::LEFT_CENTER,
                        row.name.as_str(),
                        egui::FontId::proportional(12.5),
                        hex("#cccccc"),
                    );
                    if resp.clicked() || resp.double_clicked() {
                        let now = ui.input(|i| i.time);
                        let double = resp.double_clicked()
                            || matches!(last_click, Some((j, t)) if j == i && now - t < DOUBLE_CLICK_SECS);
                        last_click = Some((i, now));
                        // 双击 → 打开并锁定；单击 → 预览打开（会被下一次打开顶替）
                        clicked = Some((i, double));
                    }
                }
            });

        self.explorer_last_click = last_click;
        if let Some((i, pin)) = clicked {
            let (path, is_dir) = {
                let r = &self.explorer_rows[i];
                (r.path.clone(), r.is_dir)
            };
            if is_dir {
                if self.explorer_expanded.contains(&path) {
                    self.explorer_expanded.remove(&path);
                } else {
                    self.explorer_expanded.insert(path);
                }
                self.explorer_rows = self.explorer_tree.rows(&self.explorer_expanded);
            } else {
                let root = self.project_root();
                let full = std::path::Path::new(&root).join(&path);
                if let Ok(text) = std::fs::read_to_string(&full) {
                    self.open_file(full, text, !pin);
                } else {
                    self.flash(format!("无法打开 {path}"));
                }
            }
        }
    }

    fn draw_outline_content(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("OUTLINE").size(11.0).color(hex("#bbbbbb")));
        ui.separator();
        let mut jump: Option<(usize, usize)> = None;
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if self.symbols.is_empty() {
                    ui.label(
                        RichText::new("(no symbols — 打开 C++ 文件并等 clangd 索引)")
                            .size(12.0)
                            .color(hex("#6a6a6a")),
                    );
                }
                let syms = self.symbols.clone();
                for sym in &syms {
                    if let Some(p) = draw_symbol(ui, sym, 0) {
                        jump = Some(p);
                    }
                }
            });
        if let Some((line, col)) = jump {
            if let Some(f) = self.current_file.clone() {
                let win = f.to_string_lossy().into_owned();
                self.jump_to(&win, line, col);
            }
        }
    }

    fn draw_status_bar(&mut self, ctx: &egui::Context) {
        let status = self.status_line.clone();
        let lsp_label = lsp_status_label(&self.lsp_status);
        let recoverable = self.lsp_status.is_recoverable();
        let dap_label = dap_status_label(&self.dap_status);
        let dap_color = dap_status_color(&self.dap_status);
        let indent = self.indent_hint.clone();
        let theme_name = self.settings.theme.clone();
        let white = egui::Color32::WHITE;
        egui::TopBottomPanel::bottom("status_bar")
            .exact_height(24.0)
            .frame(egui::Frame::none().fill(hex(ide_core::theme::BG_STATUSBAR)))
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    // ---- 左：分支 / LSP / DBG ----
                    if let Some(branch) = &self.git_status.branch {
                        ui.label(RichText::new(format!("branch: {branch}")).color(white).size(12.0));
                        ui.separator();
                    }
                    let lsp_color = match &self.lsp_status {
                        LspStatus::Ready => egui::Color32::from_rgb(0x89, 0xd1, 0x85),
                        LspStatus::Crashed(_) | LspStatus::Unavailable(_) => {
                            egui::Color32::from_rgb(0xf4, 0x87, 0x71)
                        }
                        _ => egui::Color32::from_rgb(0xbb, 0xbb, 0xbb),
                    };
                    ui.label(RichText::new(lsp_label).color(lsp_color).size(12.0));
                    if recoverable {
                        let txt = match &self.lsp_status {
                            LspStatus::Crashed(_) => "[ Restart clangd ]",
                            _ => "[ Retry ]",
                        };
                        if ui
                            .add(egui::Button::new(txt).small())
                            .on_hover_text("重启 clangd")
                            .clicked()
                        {
                            self.lsp.restart();
                        }
                    }
                    ui.separator();
                    ui.label(RichText::new(dap_label).color(dap_color).size(12.0));
                    if matches!(
                        self.dap_status,
                        DapStatus::Crashed(_) | DapStatus::Unavailable(_)
                    ) && ui
                        .add(egui::Button::new("[ Restart Debug ]").small())
                        .on_hover_text("重启调试（F5）")
                        .clicked()
                    {
                        self.on_debug_start();
                    }

                    // ---- 右：Ln/Col / 缩进 / 编码 / 主题（VSCode 布局）----
                    ui.with_layout(
                        egui::Layout::right_to_left(egui::Align::Center),
                        |ui| {
                            ui.label(RichText::new(theme_name).color(white).size(12.0));
                            ui.separator();
                            ui.label(RichText::new("UTF-8").color(white).size(12.0));
                            ui.separator();
                            ui.label(RichText::new(indent).color(white).size(12.0));
                            ui.separator();
                            ui.label(RichText::new(status.as_ref()).color(white).size(12.0));
                        },
                    );
                });
            });
    }

    fn draw_symbol_search(&mut self, ctx: &egui::Context) {
        if !self.symbol_search_open {
            return;
        }
        let mut open = true;
        let mut jump: Option<(usize, usize)> = None;
        egui::Window::new("Symbol Search  (Ctrl+T)")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(440.0)
            .anchor(egui::Align2::CENTER_TOP, [0.0, 90.0])
            .show(ctx, |ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.symbol_filter)
                        .hint_text("filter symbols...")
                        .desired_width(410.0),
                );
                ui.separator();
                let filter = self.symbol_filter.to_lowercase();
                let all = flatten_symbols(&self.symbols);
                let mut shown = 0usize;
                egui::ScrollArea::vertical()
                    .max_height(320.0)
                    .show(ui, |ui| {
                        for sym in &all {
                            if !sym.name.to_lowercase().contains(&filter) {
                                continue;
                            }
                            shown += 1;
                            let hit = ui
                                .add(
                                    egui::Label::new(
                                        egui::RichText::new(&sym.name)
                                            .monospace()
                                            .size(13.0)
                                            .color(kind_color(sym.kind)),
                                    )
                                    .sense(egui::Sense::click()),
                                )
                                .clicked();
                            if hit {
                                jump = Some((
                                    sym.selection_range.start.line as usize,
                                    sym.selection_range.start.character as usize,
                                ));
                            }
                        }
                        if shown == 0 {
                            ui.label(
                                egui::RichText::new("(no match)")
                                    .size(12.0)
                                    .color(hex("#6a6a6a")),
                            );
                        }
                    });
            });
        self.symbol_search_open = open;
        if let Some((line, col)) = jump {
            if let Some(f) = self.current_file.clone() {
                let win = f.to_string_lossy().into_owned();
                self.jump_to(&win, line, col);
                self.symbol_search_open = false;
            }
        }
    }

    fn draw_flash(&mut self, ctx: &egui::Context) {
        if let Some((msg, t)) = &self.flash {
            if t.elapsed() >= FLASH_DUR {
                self.flash = None;
                return;
            }
            egui::Area::new(egui::Id::new("flash_area"))
                .anchor(egui::Align2::CENTER_TOP, [0.0, 30.0])
                .order(egui::Order::Foreground)
                .show(ctx, |ui| {
                    egui::Frame::popup(ui.style())
                        .fill(hex("#3c3c3c"))
                        .show(ui, |ui| {
                            ui.label(
                                egui::RichText::new(msg.as_str())
                                    .size(13.0)
                                    .color(hex("#d4d4d4")),
                            );
                        });
                });
        }
    }

    /// 关闭标签确认弹窗（脏标签）。
    fn draw_close_confirm(&mut self, ctx: &egui::Context) {
        let Some(idx) = self.confirm_close else { return };
        let title = self
            .tabs
            .get(idx)
            .map(|t| t.title.clone())
            .unwrap_or_default();
        let mut answer: Option<bool> = None;
        egui::Area::new(egui::Id::new("close_confirm"))
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style())
                    .fill(hex("#3c3c3c"))
                    .show(ui, |ui| {
                        ui.set_width(360.0);
                        ui.label(
                            RichText::new(format!("「{title}」有未保存的修改。"))
                                .size(13.0)
                                .color(hex("#d4d4d4")),
                        );
                        ui.label(
                            RichText::new("确定关闭吗？")
                                .size(13.0)
                                .color(hex("#d4d4d4")),
                        );
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            if ui.button("放弃修改并关闭").clicked() {
                                answer = Some(true);
                            }
                            if ui.button("取消").clicked() {
                                answer = Some(false);
                            }
                        });
                    });
            });
        if let Some(confirm) = answer {
            if confirm {
                // 强制关闭：先清脏标记
                if let Some(t) = self.tabs.get_mut(idx) {
                    t.dirty = false;
                }
                self.close_tab_at(idx);
            }
            self.confirm_close = None;
        }
    }
}

impl eframe::App for LiteIdeApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if !self.fonts_ready {
            self.fonts_ready = true;
            apply_fonts(ctx, &self.settings.font_family);
            self.applied_family = self.settings.font_family.clone();
        }

        // 启动后 1.5s 持续请求重绘：保证字体表/异步文件列表加载后首屏不黑屏；
        // 之后 egui 回到按需重绘（不耗电）。
        if self.boot_start.elapsed().as_secs_f32() < 1.5 {
            ctx.request_repaint();
        }

        // 设置热重载：外部修改 JSON 自动生效
        if self.settings.hot_reload(&mut self.config_mtime) {
            tracing::info!("settings hot-reloaded");
        }

        // 应用设置差异（字号 / 字体族 / 主题 / minimap）。
        if self.settings.font_family != self.applied_family {
            apply_fonts(ctx, &self.settings.font_family);
            self.applied_family = self.settings.font_family.clone();
        }
        if self.settings.font_size != self.applied_font {
            if let Some(ed) = &mut self.editor {
                ed.set_typography(self.settings.font_size);
            }
            self.applied_font = self.settings.font_size;
        }
        let theme_kind = ThemeKind::from_name(&self.settings.theme);
        if theme_kind != self.applied_theme {
            let tokens = theme_tokens(&self.host, theme_kind);
            if let Some(ed) = &mut self.editor {
                ed.set_theme(theme_kind, &tokens);
            }
            self.applied_theme = theme_kind;
        }

        // 主题切换耗时（实测数据）
        if let Some(t0) = self.theme_switch_at.take() {
            tracing::info!("[perf] theme switch -> {} ms", t0.elapsed().as_millis());
        }

        // 命令面板打开耗时（第二帧时记录）。
        if self.palette_open {
            self.palette_frames = self.palette_frames.saturating_add(1);
            if self.palette_frames == 2 {
                if let Some(t0) = self.palette_open_at.take() {
                    tracing::info!(
                        "[perf] command palette open -> {} ms",
                        t0.elapsed().as_millis()
                    );
                }
            }
        } else {
            self.palette_frames = 0;
        }

        let mut visuals = match theme_kind {
            ThemeKind::DarkPlus | ThemeKind::CppDark => egui::Visuals::dark(),
            ThemeKind::LightPlus => egui::Visuals::light(),
        };
        let p = palette(theme_kind);
        visuals.panel_fill = hex(p.bg_editor);
        visuals.window_fill = hex(p.bg_sidebar);
        visuals.override_text_color = Some(hex(p.fg_editor));
        ctx.set_visuals(visuals);

        self.maybe_start_lsp();
        self.poll_lsp();
        self.sync_document();
        self.poll_dap();
        self.poll_search();
        self.poll_git();
        self.sync_editor_breakpoints();

        // 绘制前先记下「上一帧结束时是否有文本框持有焦点」：
        // 回车那一刻 egui 的 TextEdit 会立刻交出焦点，若只看绘制后的状态，
        // 这一帧就会被判成「没有文本框在用键盘」，回车于是又漏给编辑器插了一个换行。
        let text_focus_before = ctx.wants_keyboard_input();

        self.draw_status_bar(ctx);
        self.draw_menu_bar(ctx);
        self.draw_activity_bar(ctx);
        self.draw_sidebar(ctx);
        if self.debug_open {
            self.draw_debug_panel(ctx);
        }
        self.draw_tab_strip(ctx);

        // 模态窗口打开时屏蔽编辑器快捷键，避免冲突。
        let modal_open = self.palette_open
            || self.file_picker_open
            || self.settings_open
            || self.goto_line_open
            || self.about_open
            || self.keys_open
            || self.find_open
            || self.save_as_open
            || self.confirm_close.is_some();
        // 非模态的文本框（侧栏搜索框 / Git 提交框 / 调试监视框）持有键盘焦点时：
        // 编辑器忽略字符输入与无修饰键的编辑操作，但保留 Ctrl+… 全局快捷键与 F 功能键，
        // 否则在这些框里打字会同时写进编辑器正文（编辑器直接消费全局 Event::Text）。
        let text_focus = text_focus_before || ctx.wants_keyboard_input();
        if let Some(ed) = &mut self.editor {
            ed.set_keys_enabled(!modal_open);
            ed.set_text_focus(text_focus && !modal_open);
        }

        let action = egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(hex(p.bg_editor)))
            .show(ctx, |ui| {
                if self.settings.show_breadcrumbs {
                    self.draw_breadcrumb(ui);
                }
                if let Some(ed) = self.editor.as_mut() {
                    let act = ed.show(ui);
                    self.status_line = ed.status_text().into();
                    act
                } else {
                    None
                }
            })
            .inner;

        // 脏标记跟踪（编辑序列号变化 → 激活标签置脏 + 缩进提示刷新）。
        let seq_now = self.editor.as_ref().map(|e| e.edit_seq()).unwrap_or(0);
        if seq_now != self.editor_clean_seq {
            self.tabs.set_active_dirty(true);
            self.editor_clean_seq = seq_now;
            if let Some(ed) = &self.editor {
                self.indent_hint = detect_indent(&ed.text());
            }
        }

        match action {
            Some(EditorAction::GotoDefinition) => self.on_goto_definition(),
            Some(EditorAction::GoBack) => self.on_go_back(),
            Some(EditorAction::SymbolSearch) => {
                self.symbol_search_open = !self.symbol_search_open;
                self.symbol_filter.clear();
            }
            Some(EditorAction::Save) => self.on_save(),
            Some(EditorAction::DebugStart) => self.on_debug_start(),
            Some(EditorAction::DebugStop) => self.on_debug_stop(),
            Some(EditorAction::DebugToggleBreakpoint) => self.on_toggle_breakpoint(),
            Some(EditorAction::DebugToggleBreakpointAtLine(line)) => {
                self.toggle_breakpoint_at_line(line)
            }
            Some(EditorAction::Find) => self.open_find(false),
            Some(EditorAction::Replace) => self.open_find(true),
            Some(EditorAction::DebugNext) => self.on_debug_next(),
            Some(EditorAction::DebugStep) => self.on_debug_step(),
            Some(EditorAction::DebugFinish) => self.on_debug_finish(),
            Some(EditorAction::DebugAddWatch) => self.on_add_watch(),
            Some(EditorAction::FilePicker) => {
                if self.file_picker_open {
                    self.file_picker_open = false;
                } else {
                    self.open_file_picker();
                }
            }
            Some(EditorAction::ContentSearch) => self.open_content_search(),
            Some(EditorAction::GitPanel) => {
                self.git_panel_open = !self.git_panel_open;
                if self.git_panel_open {
                    self.refresh_git_now();
                }
            }
            Some(EditorAction::Settings) => {
                self.settings_open = !self.settings_open;
            }
            Some(EditorAction::NewFile) => self.on_new_file(),
            Some(EditorAction::SaveAs) => self.open_save_as(),
            Some(EditorAction::CloseTab) => self.on_close_tab(),
            Some(EditorAction::NextTab) => self.cycle_tab(1),
            Some(EditorAction::PrevTab) => self.cycle_tab(-1),
            Some(EditorAction::CommandPalette) => {
                self.palette_open = !self.palette_open;
                if self.palette_open {
                    self.palette_query.clear();
                    self.palette_sel = 0;
                    self.palette_focus_req = true;
                    self.palette_frames = 0;
                    self.palette_open_at = Some(Instant::now());
                }
            }
            Some(EditorAction::ToggleSidebar) => {
                self.settings.show_sidebar = !self.settings.show_sidebar;
                self.save_settings();
            }
            Some(EditorAction::GotoLine) => {
                self.goto_line_open = !self.goto_line_open;
                self.goto_line_input.clear();
            }
            Some(EditorAction::ZoomIn) => self.adjust_font_size(1.0),
            Some(EditorAction::ZoomOut) => self.adjust_font_size(-1.0),
            Some(EditorAction::ResetZoom) => {
                self.settings.font_size = 14.0;
                self.save_settings();
            }
            None => {}
        }

        self.update_hover(ctx);
        self.draw_symbol_search(ctx);
        self.draw_file_picker(ctx);
        self.draw_git_panel(ctx);
        self.draw_settings(ctx);
        self.draw_goto_line(ctx);
        self.draw_command_palette(ctx);
        self.draw_find_bar(ctx);
        self.draw_save_as(ctx);
        self.draw_about(ctx);
        self.draw_keys(ctx);
        self.draw_close_confirm(ctx);
        self.draw_flash(ctx);

        // egui 默认「按需重绘」：后台线程（rg / LSP / DAP）刚送到的结果如果不主动重绘，
        // 界面就一直停在旧帧，直到用户再动一下鼠标 —— 表现就是「搜索没反应」。
        // 有任务在飞时用 100ms 低频轮询事件通道，代价可忽略。
        if self.pending_search_seq.is_some()
            || matches!(
                self.lsp_status,
                LspStatus::Detecting | LspStatus::Connecting
            )
            || matches!(self.dap_status, DapStatus::Starting)
        {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }

        // 输入法（IME）通道：**不能**全局关掉 `o.ime`（旧版本为了绕开「回车被输入法吞掉」
        // 而每帧强制 `o.ime = None`，代价是中文彻底打不进来）。
        // 现在交给各个文本框自己上报：
        //   · 内置 `egui::TextEdit`（命令面板 / 文件浏览器 / 搜索框…）本来就会上报；
        //   · 自绘编辑器在 `Editor::show` 里上报，并在事件循环里处理 `Event::Ime`
        //     （Preedit 组合串 / Commit 上屏）。
        // 没人上报时 `output.ime` 自然是 None，egui-winit 会关掉窗口的输入法通道。
    }
}

fn lsp_status_label(s: &LspStatus) -> String {
    match s {
        LspStatus::Off => "LSP: off".to_string(),
        LspStatus::Detecting => "LSP: detecting WSL...".to_string(),
        LspStatus::Connecting => "LSP: starting clangd...".to_string(),
        LspStatus::Ready => "LSP: Ready (clangd)".to_string(),
        LspStatus::Crashed(d) => format!("LSP: CRASHED ({d})"),
        LspStatus::Unavailable(d) => format!("LSP: unavailable ({d})"),
    }
}

// ---- 复核改进：矢量图标绘制（避免字体缺字显示成方框） ----

/// 图标种类（活动栏 + 资源管理器）。
#[derive(Clone, Copy, PartialEq, Eq)]
enum IconKind {
    Explorer,
    Search,
    SourceControl,
    Run,
    Outline,
    Settings,
    Refresh,
    Folder,
    FolderOpen,
    File,
}

/// 在 `center` 处按 `size` 画一个矢量图标（纯 painter 图元，不依赖字体字形）。
fn draw_icon(
    painter: &egui::Painter,
    center: egui::Pos2,
    size: f32,
    icon: IconKind,
    color: egui::Color32,
) {
    let (cx, cy) = (center.x, center.y);
    let p = painter;
    let stroke = |w: f32| egui::Stroke::new(w, color);
    match icon {
        IconKind::Explorer => {
            let (w, h) = (size * 0.56, size * 0.72);
            let back = egui::Rect::from_min_size(
                egui::pos2(cx - w * 0.5 + size * 0.14, cy - h * 0.5 - size * 0.08),
                egui::vec2(w, h),
            );
            let front = egui::Rect::from_min_size(
                egui::pos2(cx - w * 0.5 - size * 0.10, cy - h * 0.5 + size * 0.10),
                egui::vec2(w, h),
            );
            p.rect_stroke(back, 1.0, stroke(1.3));
            p.rect_stroke(front, 1.0, stroke(1.4));
        }
        IconKind::Search => {
            let r = size * 0.30;
            let c = egui::pos2(cx - size * 0.08, cy - size * 0.08);
            p.circle_stroke(c, r, stroke(1.6));
            p.line_segment(
                [
                    egui::pos2(c.x + r * 0.72, c.y + r * 0.72),
                    egui::pos2(cx + size * 0.34, cy + size * 0.34),
                ],
                stroke(1.6),
            );
        }
        IconKind::SourceControl => {
            let r = size * 0.10;
            let a = egui::pos2(cx - size * 0.22, cy - size * 0.32);
            let b = egui::pos2(cx - size * 0.22, cy + size * 0.32);
            let c = egui::pos2(cx + size * 0.28, cy);
            p.line_segment([a, b], stroke(1.4));
            p.line_segment([a, c], stroke(1.4));
            for n in [a, b, c] {
                p.circle_filled(n, r, color);
            }
        }
        IconKind::Run => {
            p.add(egui::Shape::convex_polygon(
                vec![
                    egui::pos2(cx - size * 0.22, cy - size * 0.34),
                    egui::pos2(cx - size * 0.22, cy + size * 0.34),
                    egui::pos2(cx + size * 0.34, cy),
                ],
                color,
                egui::Stroke::NONE,
            ));
        }
        IconKind::Outline => {
            for k in 0..3 {
                let y = cy + size * (k as f32 - 1.0) * 0.28;
                p.circle_filled(egui::pos2(cx - size * 0.30, y), 1.6, color);
                p.line_segment(
                    [
                        egui::pos2(cx - size * 0.16, y),
                        egui::pos2(cx + size * 0.32, y),
                    ],
                    stroke(1.4),
                );
            }
        }
        IconKind::Settings => {
            let c = egui::pos2(cx, cy);
            let r1 = size * 0.28;
            let r2 = size * 0.42;
            p.circle_stroke(c, r1, stroke(1.4));
            p.circle_stroke(c, r1 * 0.34, stroke(1.4));
            for i in 0..8 {
                let a = std::f32::consts::TAU * (i as f32) / 8.0;
                let (s, co) = a.sin_cos();
                p.line_segment(
                    [
                        egui::pos2(cx + co * r1, cy + s * r1),
                        egui::pos2(cx + co * r2, cy + s * r2),
                    ],
                    stroke(1.4),
                );
            }
        }
        IconKind::Refresh => {
            let c = egui::pos2(cx, cy);
            let r = size * 0.28;
            p.circle_stroke(c, r, stroke(1.4));
            let a = egui::pos2(cx + r * 0.72, cy - r * 0.72);
            p.add(egui::Shape::convex_polygon(
                vec![
                    egui::pos2(a.x - size * 0.11, a.y - size * 0.05),
                    egui::pos2(a.x + size * 0.05, a.y - size * 0.07),
                    egui::pos2(a.x - size * 0.01, a.y + size * 0.11),
                ],
                color,
                egui::Stroke::NONE,
            ));
        }
        IconKind::Folder | IconKind::FolderOpen => {
            let (w, h) = (size * 0.66, size * 0.46);
            let x0 = cx - w * 0.5;
            let y0 = cy - h * 0.5 + size * 0.07;
            p.rect_filled(
                egui::Rect::from_min_size(
                    egui::pos2(x0, y0 - size * 0.12),
                    egui::vec2(w * 0.46, size * 0.13),
                ),
                1.0,
                color,
            );
            p.rect_filled(
                egui::Rect::from_min_size(egui::pos2(x0, y0), egui::vec2(w, h)),
                1.5,
                color,
            );
            if icon == IconKind::FolderOpen {
                p.rect_filled(
                    egui::Rect::from_min_size(
                        egui::pos2(x0, y0 + h * 0.55),
                        egui::vec2(w, h * 0.45),
                    ),
                    0.5,
                    color.gamma_multiply(0.7),
                );
            }
        }
        IconKind::File => {
            let (w, h) = (size * 0.46, size * 0.62);
            let rect = egui::Rect::from_center_size(egui::pos2(cx, cy), egui::vec2(w, h));
            p.rect_stroke(rect, 1.0, stroke(1.3));
            for k in 0..3 {
                let y = rect.top() + h * (0.28 + k as f32 * 0.20);
                p.line_segment(
                    [
                        egui::pos2(rect.left() + w * 0.20, y),
                        egui::pos2(rect.right() - w * 0.20, y),
                    ],
                    stroke(1.0),
                );
            }
        }
    }
}

/// 调试面板的分节标题（VS Code 风格：折叠箭头 + 名称 + 数量）。
fn section(ui: &mut egui::Ui, title: &str, count: usize) {
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), 20.0),
        egui::Sense::hover(),
    );
    let painter = ui.painter();
    painter.rect_filled(rect, 0.0, hex("#ffffff").gamma_multiply(0.03));
    let c = egui::pos2(rect.left() + 10.0, rect.center().y);
    let r = 3.2_f32;
    painter.add(egui::Shape::convex_polygon(
        vec![
            egui::pos2(c.x - r, c.y - r * 0.6),
            egui::pos2(c.x + r, c.y - r * 0.6),
            egui::pos2(c.x, c.y + r * 0.9),
        ],
        hex("#cccccc"),
        egui::Stroke::NONE,
    ));
    painter.text(
        egui::pos2(rect.left() + 20.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        title,
        egui::FontId::proportional(11.0),
        hex("#cccccc"),
    );
    if count > 0 {
        painter.text(
            egui::pos2(rect.right() - 10.0, rect.center().y),
            egui::Align2::RIGHT_CENTER,
            count.to_string(),
            egui::FontId::proportional(10.5),
            hex("#8a8a8a"),
        );
    }
}

/// 展开/折叠三角（资源管理器）。
fn draw_chevron(painter: &egui::Painter, rect: egui::Rect, expanded: bool, color: egui::Color32) {
    let c = rect.center();
    let r = 3.5_f32;
    let pts = if expanded {
        vec![
            egui::pos2(c.x - r, c.y - r * 0.5),
            egui::pos2(c.x + r, c.y - r * 0.5),
            egui::pos2(c.x, c.y + r * 0.8),
        ]
    } else {
        vec![
            egui::pos2(c.x - r * 0.5, c.y - r),
            egui::pos2(c.x - r * 0.5, c.y + r),
            egui::pos2(c.x + r * 0.8, c.y),
        ]
    };
    painter.add(egui::Shape::convex_polygon(pts, color, egui::Stroke::NONE));
}

/// 文件图标颜色（按扩展名，近似 VSCode 文件图标配色）。
fn file_icon_color(name: &str) -> egui::Color32 {
    match name.rsplit('.').next().unwrap_or("") {
        "rs" => hex("#dea584"),
        "c" | "cc" | "cpp" | "cxx" | "h" | "hh" | "hpp" | "hxx" => hex("#519aba"),
        "json" => hex("#cbcb41"),
        "md" => hex("#519aba"),
        "toml" => hex("#9c8f6b"),
        "py" => hex("#519aba"),
        _ => hex("#c5c5c5"),
    }
}

/// 小图标按钮（侧边栏标题栏动作用）。
fn icon_button(ui: &mut egui::Ui, icon: IconKind, tip: &str) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::click());
    let color = if resp.hovered() {
        egui::Color32::WHITE
    } else {
        hex("#9a9a9a")
    };
    if resp.hovered() {
        ui.painter()
            .rect_filled(rect, 3.0, hex("#ffffff").gamma_multiply(0.08));
    }
    draw_icon(ui.painter(), rect.center(), 15.0, icon, color);
    resp.on_hover_text(tip)
}

/// 读取目录项 `(名称, 是否目录)`。
///
/// 只在用户切换目录时调用一次（单个目录的浅读取，量小），不做递归遍历。
fn load_dir_entries(dir: &std::path::Path) -> Vec<(String, bool)> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<(String, bool)> = rd
        .flatten()
        .map(|e| {
            let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
            (e.file_name().to_string_lossy().into_owned(), is_dir)
        })
        .collect();
    sort_dir_entries(&mut out);
    out
}

/// 目录项排序：目录在前，同类按名称不区分大小写升序（同名再按原序稳定）。
fn sort_dir_entries(entries: &mut [(String, bool)]) {
    entries.sort_by(|a, b| {
        b.1.cmp(&a.1)
            .then_with(|| a.0.to_lowercase().cmp(&b.0.to_lowercase()))
            .then_with(|| a.0.cmp(&b.0))
    });
}

/// 目录项过滤：`query` 为空返回全部下标；否则按名称「包含」匹配（不区分大小写）。
///
/// 含路径分隔符（`/` `\`）或盘符冒号的输入被当作「要跳转的路径」而非过滤词，
/// 此时不做过滤——否则列表会被清空、用户反而看不到当前目录内容。
fn filter_dir_entries(entries: &[(String, bool)], query: &str) -> Vec<usize> {
    let q = query.trim().to_lowercase();
    let path_like = q.contains('/') || q.contains('\\') || q.contains(':');
    entries
        .iter()
        .enumerate()
        .filter(|(_, (name, _))| q.is_empty() || path_like || name.to_lowercase().contains(&q))
        .map(|(i, _)| i)
        .collect()
}

/// 把文件浏览器输入解析成路径：绝对路径原样使用，相对路径相对当前目录。
fn resolve_picker_path(dir: &std::path::Path, text: &str) -> PathBuf {
    let p = PathBuf::from(text.trim());
    if p.is_absolute() {
        p
    } else {
        dir.join(p)
    }
}

/// 文件浏览器的一行（图标 + 名称，悬停高亮）；返回是否被点击。
fn browser_row(
    ui: &mut egui::Ui,
    row_w: f32,
    row_h: f32,
    icon: IconKind,
    name: &str,
    color: egui::Color32,
) -> bool {
    let (rect, resp) =
        ui.allocate_exact_size(egui::vec2(row_w, row_h), egui::Sense::click());
    let painter = ui.painter();
    if resp.hovered() {
        painter.rect_filled(rect, 3.0, hex("#2a2d2e"));
    }
    draw_icon(
        painter,
        egui::pos2(rect.left() + 12.0, rect.center().y),
        15.0,
        icon,
        color,
    );
    painter.text(
        egui::pos2(rect.left() + 26.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        name,
        egui::FontId::monospace(12.0),
        hex("#cccccc"),
    );
    resp.clicked()
}

fn dap_status_label(s: &DapStatus) -> String {
    match s {
        DapStatus::Off => "DBG: off".to_string(),
        DapStatus::Starting => "DBG: starting gdb...".to_string(),
        DapStatus::Ready => "DBG: ready".to_string(),
        DapStatus::Running => "DBG: running".to_string(),
        DapStatus::Stopped => "DBG: stopped".to_string(),
        DapStatus::Crashed(d) => format!("DBG: CRASHED ({d})"),
        DapStatus::Unavailable(d) => format!("DBG: unavailable ({d})"),
    }
}

fn dap_status_color(s: &DapStatus) -> egui::Color32 {
    match s {
        DapStatus::Running => egui::Color32::from_rgb(0x89, 0xd1, 0x85),
        DapStatus::Stopped => egui::Color32::from_rgb(0x4e, 0xc9, 0xb0),
        DapStatus::Crashed(_) | DapStatus::Unavailable(_) => {
            egui::Color32::from_rgb(0xf4, 0x87, 0x71)
        }
        _ => egui::Color32::from_rgb(0xbb, 0xbb, 0xbb),
    }
}

/// 检测文档缩进风格："Spaces: N" / "Tabs"（状态栏/面包屑显示）。
fn detect_indent(text: &str) -> String {
    let mut space_counts: HashMap<usize, usize> = HashMap::new();
    let mut saw_tab = false;
    for line in text.lines().take(200) {
        if line.starts_with('\t') {
            saw_tab = true;
            break;
        }
        let lead = line.bytes().take_while(|b| *b == b' ').count();
        if lead > 0 {
            *space_counts.entry(lead).or_insert(0) += 1;
        }
    }
    if saw_tab {
        return "Tabs".to_string();
    }
    let best = space_counts
        .iter()
        .max_by_key(|(_, c)| **c)
        .map(|(n, _)| *n)
        .unwrap_or(4);
    format!("Spaces: {best}")
}

fn draw_var_node(
    ui: &mut egui::Ui,
    node: &VarNode,
    path: &str,
    expanded: &HashSet<String>,
    children_map: &HashMap<String, Vec<VarNode>>,
    depth: usize,
    actions: &mut Vec<DebugPanelAction>,
) {
    let indent = 8.0 + depth as f32 * 14.0;
    let has_children = node.expandable || node.numchild > 0;
    let is_open = has_children && expanded.contains(path);
    ui.horizontal(|ui| {
        ui.add_space(indent);
        if has_children {
            let arrow = if is_open { "▼" } else { "▶" };
            if ui
                .add(egui::Button::new(arrow).small())
                .on_hover_text(format!("展开/折叠 {path}"))
                .clicked()
            {
                actions.push(DebugPanelAction::ToggleExpand(path.to_string()));
            }
        } else {
            ui.add_space(16.0);
        }
        let mut value = node.value.clone();
        if value.len() > 60 {
            value.truncate(57);
            value.push_str("...");
        }
        let text = if node.type_name.is_empty() {
            format!("{path} = {value}")
        } else {
            format!("{} {path} = {value}", node.type_name)
        };
        ui.label(
            RichText::new(text)
                .monospace()
                .size(11.0)
                .color(hex("#d4d4d4")),
        );
    });
    if is_open {
        let kids = children_map.get(path).cloned().unwrap_or_default();
        for k in &kids {
            let child_path = if k.exp.is_empty() {
                format!("{path}.")
            } else {
                format!("{path}.{}", k.exp)
            };
            draw_var_node(ui, k, &child_path, expanded, children_map, depth + 1, actions);
        }
        if kids.is_empty() {
            ui.horizontal(|ui| {
                ui.add_space(indent + 14.0);
                ui.label(
                    RichText::new("(展开…)")
                        .size(11.0)
                        .color(hex("#6a6a6a")),
                );
            });
        }
    }
}

fn clean_markdown(s: &str) -> String {
    s.lines()
        .filter(|l| !l.trim_start().starts_with("```"))
        .map(|l| {
            let l = l.trim().trim_start_matches('>').trim();
            l.replace("**", "")
        })
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

fn kind_color(kind: u32) -> egui::Color32 {
    match kind {
        5 | 11 => hex("#4ec9b0"),
        6 | 12 => hex("#dcdcaa"),
        8 | 7 => hex("#9cdcfe"),
        13 => hex("#9cdcfe"),
        3 => hex("#569cd6"),
        _ => hex("#d4d4d4"),
    }
}

/// 画一个（可点击的）符号节点；点击返回 (0-based 行、列)。
fn draw_symbol(ui: &mut egui::Ui, sym: &SymbolInfo, depth: usize) -> Option<(usize, usize)> {
    let indent = 10.0 + depth as f32 * 14.0;
    let label = match &sym.detail {
        Some(d) if !d.is_empty() => format!("{} : {}", sym.name, d),
        _ => sym.name.clone(),
    };
    let node_clicked = ui
        .horizontal(|ui| {
            ui.add_space(indent);
            ui.add(
                egui::Label::new(
                    egui::RichText::new(label)
                        .monospace()
                        .size(12.0)
                        .color(kind_color(sym.kind)),
                )
                .sense(egui::Sense::click()),
            )
            .clicked()
        })
        .inner;
    let mut hit = node_clicked.then_some((
        sym.selection_range.start.line as usize,
        sym.selection_range.start.character as usize,
    ));
    for child in &sym.children {
        if let Some(p) = draw_symbol(ui, child, depth + 1) {
            hit = Some(p);
        }
    }
    hit
}

fn flatten_symbols(syms: &[SymbolInfo]) -> Vec<SymbolInfo> {
    let mut out = Vec::new();
    for s in syms {
        out.push(s.clone());
        out.extend(flatten_symbols(&s.children));
    }
    out
}

fn main() -> eframe::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    tracing::info!("LiteIDE starting (stage 5)");

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("LiteIDE")
            .with_inner_size([1280.0, 800.0]),
        ..Default::default()
    };
    eframe::run_native(
        "LiteIDE",
        options,
        Box::new(|_cc| Ok(Box::new(LiteIdeApp::new()))),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_parses_channels() {
        assert_eq!(super::hex("#1e1e1e").r(), 0x1e);
        assert_eq!(super::hex("#007acc").g(), 0x7a);
        assert_eq!(super::hex("#007acc").b(), 0xcc);
    }

    #[test]
    fn sidebar_drag_clamps_width() {
        // 向右拖：加宽，且不超上限。
        assert_eq!(
            super::sidebar_drag(SIDEBAR_DEFAULT_WIDTH, 60.0),
            SIDEBAR_DEFAULT_WIDTH + 60.0
        );
        assert_eq!(super::sidebar_drag(SIDEBAR_MAX_WIDTH, 500.0), SIDEBAR_MAX_WIDTH);
        // 向左拖到最左：停在最小宽度（松开时才收起）。
        assert_eq!(
            super::sidebar_drag(SIDEBAR_DEFAULT_WIDTH, -10_000.0),
            SIDEBAR_MIN_WIDTH
        );
    }

    #[test]
    fn clean_markdown_strips_fences_and_bold() {
        let md = "```cpp\nint add(int a, int b)\n```\n\n**Adds** two integers.";
        let out = super::clean_markdown(md);
        assert!(!out.contains("```"));
        assert!(!out.contains("**"));
        assert!(out.contains("int add(int a, int b)"));
        assert!(out.contains("Adds"));
    }

    #[test]
    fn flatten_symbols_recurses() {
        use ide_lsp::message::{Position, Range};
        let mk = |name: &str, children: Vec<ide_lsp::SymbolInfo>| ide_lsp::SymbolInfo {
            name: name.to_string(),
            kind: 12,
            detail: None,
            range: Range {
                start: Position { line: 0, character: 0 },
                end: Position { line: 1, character: 0 },
            },
            selection_range: Range {
                start: Position { line: 0, character: 0 },
                end: Position { line: 1, character: 0 },
            },
            children,
        };
        let tree = vec![mk(
            "a",
            vec![mk("b", vec![mk("c", vec![])])],
        )];
        let flat = super::flatten_symbols(&tree);
        let names: Vec<&str> = flat.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["a", "b", "c"]);
    }

    #[test]
    fn detect_indent_counts_spaces() {
        assert_eq!(detect_indent("int x;\n  int y;\n  int z;\n"), "Spaces: 2");
        assert_eq!(detect_indent("int x;\n    int y;\n    int z;\n"), "Spaces: 4");
        assert_eq!(detect_indent("int x;\n"), "Spaces: 4"); // 无缩进 → 默认 4
    }

    #[test]
    fn detect_indent_detects_tabs() {
        assert_eq!(detect_indent("\tint x;\n\tint y;\n"), "Tabs");
        assert_eq!(detect_indent("  a\n\tb\n"), "Tabs"); // 制表符优先
    }

    /// 守护用例：模拟 100 次快捷键/命令操作，无 panic。
    /// 只跑 safe 命令（仅内存操作，不写盘、不拉起子进程）。
    #[test]
    fn safe_commands_smoke_100_ops() {
        let ctx = egui::Context::default();
        let mut app = LiteIdeApp::new();
        let safe_ids = [
            "file.newFile", "file.close", "file.closeAll", "file.reopenClosed", "file.copyPath",
            "file.saveAs",
            "edit.undo", "edit.redo", "edit.selectAll", "edit.cut", "edit.copy",
            "edit.paste", "edit.deleteLine", "edit.selectLine", "edit.toggleComment",
            "edit.duplicateLine", "edit.find", "edit.replace",
            "view.showExplorer", "view.showOutline",
            "go.back", "go.line", "go.symbol",
            "search.symbol",
            "debug.toggleBreakpoint", "debug.addWatch",
            "settings.open",
            "help.keybindings", "help.about",
        ];
        for _round in 0..4 {
            for id in safe_ids {
                app.run_command(id, &ctx);
            }
        }
        // 29 命令 × 4 轮 = 116 次操作，无 panic
        assert!(!app.tabs.is_empty());
        assert!(app.editor.is_some());
    }

    /// 断点集合切换：同一行两次应互逆；行号 1-based 存储。
    #[test]
    fn toggle_breakpoint_updates_set() {
        let mut app = LiteIdeApp::new();
        assert!(!app.tabs.is_empty(), "默认应打开一个标签");
        let idx = app.tabs.active();
        if let Some(tab) = app.tabs.get_mut(idx) {
            tab.path = Some(std::path::PathBuf::from("test.cpp"));
        }
        app.toggle_breakpoint_at_line(9);
        assert_eq!(app.ui_breakpoints.len(), 1);
        assert_eq!(app.ui_breakpoints[0].line, 10);
        assert_eq!(app.ui_breakpoints[0].file_win, "test.cpp");
        app.toggle_breakpoint_at_line(9);
        assert!(app.ui_breakpoints.is_empty(), "再次切换应移除断点");
    }

    /// 无文件路径时设置断点应被拒绝（不 panic、不新增）。
    #[test]
    fn toggle_breakpoint_without_path_is_rejected() {
        let mut app = LiteIdeApp::new();
        let idx = app.tabs.active();
        if let Some(tab) = app.tabs.get_mut(idx) {
            tab.path = None;
        }
        app.toggle_breakpoint_at_line(3);
        assert!(app.ui_breakpoints.is_empty());
    }

    /// 目录项排序：目录排在前，同类按名称不区分大小写升序。
    #[test]
    fn sort_dir_entries_dirs_first() {
        let mut v = vec![
            ("zeta.txt".to_string(), false),
            ("Beta".to_string(), true),
            ("alpha.txt".to_string(), false),
            ("Gamma".to_string(), true),
        ];
        super::sort_dir_entries(&mut v);
        let names: Vec<&str> = v.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["Beta", "Gamma", "alpha.txt", "zeta.txt"]);
    }

    /// 目录项过滤：空查询返回全部；否则按名字包含（不区分大小写）。
    #[test]
    fn filter_dir_entries_matches_case_insensitively() {
        let entries = vec![
            ("src".to_string(), true),
            ("README.md".to_string(), false),
            ("main.rs".to_string(), false),
        ];
        assert_eq!(super::filter_dir_entries(&entries, ""), vec![0, 1, 2]);
        assert_eq!(super::filter_dir_entries(&entries, "MAIN"), vec![2]);
        assert_eq!(super::filter_dir_entries(&entries, ".rs"), vec![2]);
        assert!(super::filter_dir_entries(&entries, "找不到").is_empty());
        // 路径样式的输入不过滤（它是要跳转的目标）。
        assert_eq!(super::filter_dir_entries(&entries, r"E:\TestDeepSeek"), vec![0, 1, 2]);
        assert_eq!(super::filter_dir_entries(&entries, "src/"), vec![0, 1, 2]);
    }

    /// 输入解析：绝对路径原样，相对路径拼到当前目录。
    #[test]
    fn resolve_picker_path_joins_relative() {
        let dir = std::path::PathBuf::from(r"E:\proj");
        assert_eq!(
            super::resolve_picker_path(&dir, r"D:\other"),
            std::path::PathBuf::from(r"D:\other")
        );
        assert_eq!(
            super::resolve_picker_path(&dir, "sub"),
            std::path::PathBuf::from(r"E:\proj\sub")
        );
    }

    /// Ctrl+Shift+F 应把侧栏切到「搜索」视图（结果占用资源管理器位置）。
    #[test]
    fn content_search_opens_sidebar_view() {
        let mut app = LiteIdeApp::new();
        app.settings.show_sidebar = false;
        app.open_content_search();
        assert_eq!(app.sidebar_content, SidebarContent::Search);
        assert!(app.settings.show_sidebar, "搜索视图应展开侧栏");
        assert!(app.content_search_focus, "应请求把焦点交给搜索框");
    }

    /// 文件浏览器：输入「存在的目录路径」+ 回车 → 切换到那个目录。
    /// 用 headless egui 连跑两帧模拟「打字 → 回车」。
    #[test]
    fn file_picker_enter_navigates_to_typed_dir() {
        let mut app = LiteIdeApp::new();
        app.open_file_picker();
        let before = app.file_picker_dir.clone();
        let Some(parent) = before.parent().map(|p| p.to_path_buf()) else {
            return; // 已在根目录，跳过
        };
        let ctx = egui::Context::default();
        let screen = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1280.0, 800.0));

        // 先空跑一帧：窗口首帧才请求焦点，同帧到达的按键会被 TextEdit 丢掉。
        let _ = ctx.run(
            egui::RawInput {
                screen_rect: Some(screen),
                ..Default::default()
            },
            |ctx| app.draw_file_picker(ctx),
        );

        let mut input = egui::RawInput {
            screen_rect: Some(screen),
            ..Default::default()
        };
        input.events.push(egui::Event::Text("..".to_string()));
        let _ = ctx.run(input, |ctx| app.draw_file_picker(ctx));
        assert_eq!(app.file_picker_query, "..", "打字应进入输入框");

        let mut input = egui::RawInput {
            screen_rect: Some(screen),
            ..Default::default()
        };
        input.events.push(egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::default(),
        });
        let _ = ctx.run(input, |ctx| app.draw_file_picker(ctx));
        assert_eq!(
            app.file_picker_dir,
            before.join(".."),
            "回车应切换到上级目录"
        );
        let _ = parent;
    }

    /// 文件浏览器打开后必须有可用目录，且再次打开保留上次目录。
    #[test]
    fn file_picker_opens_with_directory() {
        let mut app = LiteIdeApp::new();
        app.open_file_picker();
        assert!(app.file_picker_open);
        assert!(app.file_picker_focus, "应请求把焦点交给输入框");
        assert!(!app.file_picker_dir.as_os_str().is_empty());
        // 再次打开（已有目录）不应重置到别处。
        app.file_picker_open = false;
        let dir = app.file_picker_dir.clone();
        app.open_file_picker();
        assert_eq!(app.file_picker_dir, dir);
    }
}
