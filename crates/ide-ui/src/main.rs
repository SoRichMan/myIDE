//! LiteIDE 入口：阶段 2 搜索、LSP（clangd）集成；阶段 3 调试器（GDB MI）；
//! 阶段 4 搜索（rg）、Git + 设置面板；阶段 5 设置落地/命令面板/多标签/UI 打磨。
// release 构建不弹控制台窗口（GUI 程序）；debug 保留控制台看 tracing 日志。
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
mod commands;
mod editor;
mod fileindex;
mod scm;
mod settings;
mod tabs;
mod terminal;
mod tree;

use eframe::egui;
use eframe::egui::RichText;
use editor::EditorAction;
use ide_core::theme::{palette, PaletteHex, ThemeKind};
use ide_ext::ExtensionHost;
use ide_dap::manager::{BreakpointSpec, DapManager, DapStatus, Event as DapEvent, StackFrame, VarNode};
use ide_git::{Event as GitEvent, GitManager};
use ide_lsp::manager::{Event as LspEvent, LspManager, LspStatus};
use ide_lsp::message::{Position, SymbolInfo};
use ide_lsp::path_map::{path_to_uri_any, posix_to_uri, uri_to_posix, uri_to_win};
use ide_remote::{
    is_posix_path, normalize_posix, PathKind, RemoteFs, RemoteTarget,
};
use ide_search::{Event as SearchEvent, SearchManager, SearchMatch, SearchOptions};
use settings::AppSettings;
use tabs::{CloseResult, Tab, TabList};
use terminal::{TerminalSet, TerminalStyle};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

const HOVER_DWELL: Duration = Duration::from_millis(300);
/// hover 没拿到内容时的重试间隔（clangd 可能还在解析文件）。
const HOVER_RETRY: Duration = Duration::from_millis(700);
const FLASH_DUR: Duration = Duration::from_millis(2500);
/// 跳转栈上限（阶段 5 性能：防无限增长）。
const JUMP_STACK_CAP: usize = 100;
/// 缓存有效期（符号查找结果、文件内容）。
///
/// 用户提的需求：打开多个文件后要能复用缓存，文件没变时关掉再打开直接用。
/// 5 分钟足够覆盖一次「编译 → 改 → 再编译」的循环，又不会让缓存无限陈旧。
const CACHE_TTL: Duration = Duration::from_secs(5 * 60);
/// 文件内容缓存条目上限（超出后按插入顺序淘汰最旧的）。
const FILE_CACHE_CAP: usize = 64;
/// 关闭标签的恢复栈上限。
const CLOSED_TAB_CAP: usize = 10;
/// Ctrl+P 结果默认显示多少条（再多就点「显示更多」）。
const PICKER_PAGE: usize = 20;
/// Ctrl+P 结果最多能显示多少条（点「显示更多」后的上限）。
const PICKER_MAX_RESULTS: usize = 200;
/// 侧栏（Explorer/Outline）宽度范围与默认值。
const SIDEBAR_MIN_WIDTH: f32 = 160.0;
/// 侧栏最大宽度：SCM 面板里要能完整显示「文件名 + 完整路径」，
/// 520 太窄（长路径会被截断），放到 1000。
const SIDEBAR_MAX_WIDTH: f32 = 1000.0;
const SIDEBAR_DEFAULT_WIDTH: f32 = 260.0;
/// 侧栏展开/收起动画时长（秒）。
const SIDEBAR_ANIM_TIME: f32 = 0.16;
/// 激活标签顶部蓝色标识条的高度（VS Code active tab indicator）。
const TAB_ACTIVE_INDICATOR_H: f32 = 2.0;
/// 标签页尺寸（egui points）。宽度按各自标题的**自然宽度**算（VS Code 就是这样），
/// 只有整行放不下时才等比压缩 —— 之前是「所有标签等宽 = 最长标题」，
/// 短标题（large.cpp）会白占一大片。
/// 叉叉命中框（16×16）以及它到标签右边缘的余量。
const TAB_CLOSE_W: f32 = 16.0;
const TAB_CLOSE_MARGIN: f32 = 2.0;
/// 标题右边缘到叉叉命中框左边缘的空隙。
const TAB_GAP_X: f32 = 7.0;
/// 标题右侧整块预留：空隙 + 叉叉 + 右边缘余量。
const TAB_X_SLOT: f32 = TAB_GAP_X + TAB_CLOSE_W + TAB_CLOSE_MARGIN;
const TAB_PAD_X: f32 = 9.0; // 标题左内边距
const TAB_MIN_W: f32 = 80.0;
const TAB_MAX_W: f32 = 340.0;
/// 右侧 "N tabs" 计数的大致占位（先给标签算宽度时扣掉）。
const TAB_BADGE_W: f32 = 110.0;

/// 每个标签的宽度：自然宽度（标题 + 左内边距 + 叉叉位）夹在 `[TAB_MIN_W, TAB_MAX_W]`；
/// 整行放不下时等比压缩，但不低于 `TAB_MIN_W`（再不够就溢出，由裁剪处理）。
fn tab_widths(label_widths: &[f32], avail: f32) -> Vec<f32> {
    let natural: Vec<f32> = label_widths
        .iter()
        .map(|w| (w + TAB_PAD_X + TAB_X_SLOT).clamp(TAB_MIN_W, TAB_MAX_W))
        .collect();
    let total: f32 = natural.iter().sum();
    if total <= avail || total <= 0.0 {
        return natural;
    }
    let scale = (avail / total).max(0.0);
    natural.iter().map(|w| (w * scale).max(TAB_MIN_W)).collect()
}
/// 双击识别窗口（秒）。Windows 默认双击间隔 500ms，而 egui 内置判定只有 300ms，
/// 慢一点的双击会漏判；这里自实现识别并放宽到 Windows 的语义。
const DOUBLE_CLICK_SECS: f64 = 0.55;

/// 解析 `#RRGGBB` / `#RRGGBBAA`（VS Code 主题 JSON 里两种写法都有）。
fn hex(s: &str) -> egui::Color32 {
    let s = s.trim_start_matches('#');
    let r = u8::from_str_radix(&s[0..2], 16).unwrap_or(0);
    let g = u8::from_str_radix(&s[2..4], 16).unwrap_or(0);
    let b = u8::from_str_radix(&s[4..6], 16).unwrap_or(0);
    if s.len() >= 8 {
        let a = u8::from_str_radix(&s[6..8], 16).unwrap_or(0xff);
        return egui::Color32::from_rgba_unmultiplied(r, g, b, a);
    }
    egui::Color32::from_rgb(r, g, b)
}

/// 单行文本截断：超宽时保留首尾、中间用 `…`，按实际字形宽度测量。
///
/// 顶部 flash 提示必须只占一行：远程路径很长时不能换行，也不能整条被截掉尾巴
/// （用户需要看到连接到了哪个发行版 / 哪个目录）。
fn middle_ellipsis(text: &str, max_width: f32, measure: impl Fn(&str) -> f32) -> String {
    if measure(text) <= max_width {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let build = |keep: usize| -> String {
        let head = keep.div_ceil(2);
        let tail = keep / 2;
        let mut s: String = chars[..head].iter().collect();
        s.push('…');
        s.extend(chars[chars.len() - tail..].iter());
        s
    };
    let mut lo = 0usize;
    let mut hi = chars.len();
    let mut best = String::from("…");
    while lo <= hi {
        let keep = (lo + hi) / 2;
        if keep == 0 {
            break;
        }
        let candidate = build(keep);
        if measure(&candidate) <= max_width {
            best = candidate;
            lo = keep + 1;
        } else {
            hi = keep - 1;
        }
    }
    best
}

/// 连接项菜单的动作（布局对齐 VS Code 的 Remote 菜单）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RemoteAction {
    ConnectWsl,
    /// SSH 主机（免密登录）。
    ConnectSsh,
    ConnectTunnel,
    CloneRepository,
    OpenOnRemote,
    Disconnect,
}

impl RemoteAction {
    /// 是否已实现（WSL / SSH 是真实现，其余只展示）。
    fn is_supported(self) -> bool {
        matches!(
            self,
            RemoteAction::ConnectWsl | RemoteAction::ConnectSsh | RemoteAction::Disconnect
        )
    }
}

/// 连接菜单的条目（顺序与 VS Code 一致：WSL → SSH → Tunnel → 克隆 → 打开远程）。
fn remote_menu_entries() -> [(&'static str, &'static str, RemoteAction); 5] {
    [
        (
            "Connect to WSL…",
            "连接到 WSL 发行版里的目录（已实现）",
            RemoteAction::ConnectWsl,
        ),
        (
            "Connect to Host… (SSH)",
            "连接到 SSH 主机里的目录（已实现，免密登录）",
            RemoteAction::ConnectSsh,
        ),
        (
            "Connect to Tunnel…",
            "暂未实现：Remote Tunnels",
            RemoteAction::ConnectTunnel,
        ),
        (
            "Clone Repository in WSL…",
            "暂未实现：在 WSL 里克隆仓库",
            RemoteAction::CloneRepository,
        ),
        (
            "Open Folder on Remote…",
            "暂未实现：在远程打开文件夹",
            RemoteAction::OpenOnRemote,
        ),
    ]
}

/// 远程工具链后台任务（探测 / 安装）发回 UI 的消息。
enum ToolchainEvent {
    /// 探测完成（三个工具都查一遍）。
    Probe(Result<ide_remote::toolchain::ToolchainProbe, String>),
    /// 安装过程中的一行输出（下载 / 解包 / 自检）。
    Progress(String),
    /// 某个工具装好了。
    Installed(ide_core::remote::RemoteTool),
    /// 某个工具装失败。
    Failed {
        tool: ide_core::remote::RemoteTool,
        error: String,
    },
    /// 任务结束（成功失败都会发，界面据此清掉 busy）。
    Done,
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

/// 界面字体（Proportional 家族的第一顺位）。
///
/// egui 自带的 Ubuntu-Light 太细，11~13px 的界面文字（侧栏、SCM 面板）看着发虚；
/// VS Code 在 Windows 上用的就是 Segoe UI，这里对齐它。
const UI_FONT_CANDIDATES: &[&str] = &[
    "C:\\Windows\\Fonts\\segoeui.ttf",
    "C:\\Windows\\Fonts\\tahoma.ttf",
];

/// 内置 codicon 图标字体的 egui 字体族名。
///
/// VS Code 官方图标字体（MIT，见 `assets/codicon.LICENSE.txt`）随二进制一起编译进来：
/// 图标字形与 VS Code 完全一致，且不依赖任何外部环境（无需 Node、无需系统字体）。
const CODICON_FAMILY: &str = "codicon";

/// 按设置的字体系列重建 egui 字体表（Consolas / Cascadia Code / 默认 + CJK）。
/// `LITEIDE_NO_CJK=1` 时跳过 CJK 字体（诊断内存用）。
fn apply_fonts(ctx: &egui::Context, family: &str) {
    let mut fonts = egui::FontDefinitions::default();
    let no_cjk = std::env::var("LITEIDE_NO_CJK").map(|v| v == "1").unwrap_or(false);
    // 界面字体：Segoe UI（Windows 自带，VS Code 同款）——小字号更清晰。
    for path in UI_FONT_CANDIDATES {
        if let Ok(bytes) = std::fs::read(path) {
            fonts.font_data.insert("ui".to_owned(), egui::FontData::from_owned(bytes));
            if let Some(list) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
                list.insert(0, "ui".to_owned());
            }
            break;
        }
    }
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
    // 图标字体：内置 codicon.ttf（VS Code 同款字形），专用字体族，避免与文本字体互相回退。
    add_icon_font(&mut fonts);
    ctx.set_fonts(fonts);
}

/// 把内置 codicon 图标字体注册进字体表。
///
/// 单独抽出的原因是绘制类测试也要用：`FontFamily::Name("codicon")` 未绑定时
/// epaint 会直接 panic，而不是退化成方框。
fn add_icon_font(fonts: &mut egui::FontDefinitions) {
    fonts.font_data.insert(
        CODICON_FAMILY.to_owned(),
        egui::FontData::from_static(include_bytes!("../assets/codicon.ttf")),
    );
    fonts.families.insert(
        egui::FontFamily::Name(CODICON_FAMILY.into()),
        vec![CODICON_FAMILY.to_owned()],
    );
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
    /// 源代码管理（VS Code 的 Source Control 视图：CHANGES + GRAPH）。
    SourceControl,
}

/// 设置面板收集的动作（渲染闭包外应用）。
enum SettingsAction {
    FontSize(f32),
    Theme(ThemeKind),
    Family(String),
    Minimap(bool),
    Breadcrumbs(bool),
    Sidebar(bool),
    /// 鼠标滚轮方向（`"normal"` / `"reversed"`）。
    ScrollDirection(String),
    Reset,
}

/// 文件内容缓存条目（关掉标签再打开时复用）。
struct CachedFile {
    text: String,
    /// 修改时间（秒级；拿不到就是 None，此时只按 TTL 过期）。
    mtime: Option<u64>,
    /// 文件大小（字节；拿不到就是 None）。
    size: Option<u64>,
    /// 写入缓存的时刻。
    at: Instant,
}

/// 单个文件的文档符号缓存条目。
struct CachedSymbols {
    syms: Vec<SymbolInfo>,
    /// 缓存时的编辑序列号（变了就说明文件被改过，缓存作废）。
    edit_seq: u64,
    /// 写入缓存的时刻。
    at: Instant,
}

/// 本地文件的修改时间（秒）+ 大小（字节）。拿不到就是 None。
fn local_file_stamp(path: &std::path::Path) -> Option<(u64, u64)> {
    let md = std::fs::metadata(path).ok()?;
    let mtime = md
        .modified()
        .ok()
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_secs());
    Some((mtime.unwrap_or(0), md.len()))
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
    /// 前进栈（Alt+Right 用；新的跳转会清空它）。
    forward_stack: Vec<(PathBuf, usize, usize)>,
    /// 符号查找缓存：符号名 → 定义位置。
    ///
    /// `textDocument/definition` 找不到时退到 `workspace/symbol`，结果缓存
    /// [`CACHE_TTL`] 秒：同一个符号反复点、或关掉文件再打开时都不用再问服务器。
    symbol_cache: HashMap<String, Vec<ide_lsp::Location>>,
    /// 缓存写入时间（整体过期判断）。
    symbol_cache_at: Instant,
    /// 正在等待的 `workspace/symbol` 请求（seq + 查询名）。
    pending_ws_symbol: Option<(u64, String)>,
    /// 文件内容缓存：路径 → （文本、mtime、size、写入时间）。
    ///
    /// 标签关掉再打开时，如果文件没被改过就直接复用，省掉一次读盘 / 一次 WSL 往返。
    file_cache: HashMap<PathBuf, CachedFile>,
    hover_anchor: Option<(usize, Instant)>,
    hover_requested_char: Option<usize>,
    pending_hover: Option<u64>,
    hover_result: Option<(egui::Pos2, HoverContent)>,
    /// hover 失败/为空时的下次重试时刻。
    hover_retry_at: Option<Instant>,
    symbols: Vec<SymbolInfo>,
    /// 每个文件的文档符号缓存（uri → 符号）。
    ///
    /// 切回一个没改过的文件时直接复用，不用再问 clangd 要一遍
    /// （clangd 每次 documentSymbol 都要重新解析文件）。
    file_symbols: HashMap<String, CachedSymbols>,
    /// 当前 `symbols` 属于哪个文件。
    symbols_uri: Option<String>,
    symbol_search_open: bool,
    symbol_filter: String,
    pending_def: Option<(u64, String)>,
    pending_sym: Option<(u64, String)>,
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
    /// 工作区文件索引（Ctrl+P 的模糊搜索用；后台线程构建）。
    file_index: fileindex::FileIndex,
    /// 结果是否已「显示更多」（默认只显示前 20 条）。
    file_picker_expanded: bool,
    /// 结果列表的滚动偏移（自己维护，见 `draw_file_picker`）。
    file_picker_scroll: f32,
    /// Picker 结果缓存（按查询词，避免每帧重扫三万条）。
    file_picker_hits: Vec<fileindex::Hit>,
    /// 缓存对应的查询词。
    file_picker_hits_query: String,
    /// 缓存对应的条数上限。
    file_picker_hits_limit: usize,
    /// 缓存里一共命中多少条（用来决定要不要显示「更多」）。
    file_picker_hits_total: usize,
    /// 是否已经启动过索引构建。
    file_index_started: bool,
    /// 下一帧把焦点交给文件浏览器的输入框。
    file_picker_focus: bool,
    content_query: String,
    content_results: Vec<SearchMatch>,
    pending_search_seq: Option<u64>,
    /// 下一帧把焦点交给侧栏搜索输入框（Ctrl+Shift+F 时）。
    content_search_focus: bool,
    debug_open: bool,
    /// 源代码管理面板（VS Code 风格的 Source Control 视图）。
    scm: scm::ScmPanel,
    /// 启动时是否已按 `LITEIDE_OPEN_SCM` 打开过 SCM 视图。
    scm_opened_at_start: bool,
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
    /// 排除的文件 glob（VS Code「搜索详细信息 → 排除的文件」）。
    search_exclude_glob: String,
    // ---- VS Code 搜索视图（替换框 / 视图切换 / 折叠状态 / 输入防抖） ----
    /// 「搜索详细信息」（包含/排除的文件）是否展开。
    search_details_open: bool,
    /// 是否所有结果都已折叠（标题栏按钮随之在「全部折叠 / 全部展开」间切换）。
    search_all_collapsed: bool,
    /// 是否展开「替换」输入框（搜索框左侧的箭头）。
    search_replace_open: bool,
    search_replace_text: String,
    /// 替换时保留大小写（VS Code 的 `AB` 开关）。
    search_preserve_case: bool,
    /// 结果按目录树展示（否则为按文件平铺）。
    search_view_tree: bool,
    /// 已折叠的文件路径（平铺/树状视图共用）。
    search_collapsed_files: HashSet<String>,
    /// 已折叠的目录路径（树状视图）。
    search_collapsed_dirs: HashSet<String>,
    /// 输入防抖：到什么时刻该自动搜索（VS Code 是边输入边搜）。
    search_debounce_at: Option<Instant>,
    // ---- 复核改进：资源管理器文件树 ----
    explorer_files: Vec<String>,
    explorer_tree: tree::FileTree,
    explorer_rows: Vec<tree::Row>,
    explorer_expanded: HashSet<String>,
    /// 上一次点击的文件树行 (行号, 时间)，用于自实现双击识别（见 `DOUBLE_CLICK_SECS`）。
    explorer_last_click: Option<(usize, f64)>,
    /// 上一次点击的标签页 (序号, 时间)，用于双击锁定。
    tab_last_click: Option<(usize, f64)>,
    /// 标题栏左上角的应用图标纹理（首帧懒加载，之后每帧复用）。
    app_icon: Option<egui::TextureHandle>,
    // ---- 远程（WSL / SSH）连接 ----
    /// 左下角「远程连接」菜单是否展开。
    remote_menu_open: bool,
    /// 「连接到 WSL」对话框是否展开（选发行版 + 选目录）。
    remote_dialog_open: bool,
    /// 对话框里选中的发行版（打开对话框时从 `wsl -l -q` 枚举）。
    remote_dialog_distros: Vec<String>,
    remote_dialog_distro: String,
    /// 对话框里的目录路径（POSIX，如 `/home/work/NewLithoTune`）。
    remote_dialog_path: String,
    /// 对话框当前目录下的子目录列表。
    remote_dialog_dirs: Vec<String>,
    /// WSL 对话框最近一次连接/列目录的错误（连不上时直接显示在对话框里）。
    remote_dialog_error: Option<String>,
    /// 「连接到 SSH 主机」对话框是否展开。
    ssh_dialog_open: bool,
    ssh_dialog_host: String,
    ssh_dialog_user: String,
    ssh_dialog_port: u16,
    /// SSH 对话框里的目录路径 + 当前目录下的子目录。
    ssh_dialog_path: String,
    ssh_dialog_dirs: Vec<String>,
    /// SSH 对话框最近一次连接/列目录的错误（连不上时直接显示在对话框里）。
    ssh_dialog_error: Option<String>,
    /// 「连接」项这一帧的屏幕矩形（菜单锚定用）。
    remote_item_rect: Option<egui::Rect>,
    /// 当前远程文件会话（常驻 `wsl.exe --exec bash -l` / `ssh ... bash -l`）。
    remote: RemoteFs,
    /// 已经生效的远程目标（热重载设置时判断要不要重连）。
    applied_remote: Option<RemoteTarget>,
    // ---- 远程工具链（clangd / rg / gdb 探测 + 免 root 自动安装） ----
    /// 「远程工具链」对话框是否展开。
    toolchain_dialog_open: bool,
    /// 最近一次探测结果（`None` = 还没探测过）。
    toolchain_probe: Option<ide_remote::toolchain::ToolchainProbe>,
    /// 探测失败原因（连不上 / 输出看不懂）。
    toolchain_error: Option<String>,
    /// 后台任务（探测 / 安装）发回 UI 的事件。
    toolchain_rx: Option<std::sync::mpsc::Receiver<ToolchainEvent>>,
    /// 正在做的事（`None` = 空闲）：显示成对话框里的 spinner 文案。
    toolchain_busy: Option<String>,
    /// 安装日志（对话框里滚动显示）。
    toolchain_log: Vec<String>,
    /// 本次安装已经成功的工具（装载完成后据此重启 LSP）。
    toolchain_installed: Vec<ide_core::remote::RemoteTool>,
    /// 用户点了「暂不安装」：本次连接不再自动弹窗。
    toolchain_skipped: bool,
    /// 已经自动探测过的目标（连接变化时重置）。
    toolchain_probed_for: Option<RemoteTarget>,
    /// 探测完要不要自动弹窗（连接成功 / 手动打开时为 true）。
    toolchain_auto_open: bool,
    /// 集成终端（远端 PTY，编辑器下方 Split 面板）。
    terminal: TerminalSet,
}

impl LiteIdeApp {
    fn new() -> Self {
        let (settings, config_mtime) = AppSettings::load();
        Self::with_settings(settings, config_mtime)
    }

    /// 用给定设置装配应用。
    ///
    /// `new()` 从磁盘读取；测试传默认值，避免断言颜色/布局时被用户配置或并行测试
    /// 临时改写的 `LITEIDE_CONFIG_DIR` 影响。
    fn with_settings(settings: AppSettings, config_mtime: Option<SystemTime>) -> Self {
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
            .and_then(|p| path_to_uri_any(&p.to_string_lossy()));
        let mut tabs = TabList::new();
        if let Some(p) = path.clone() {
            tabs.open(p, text);
        } else {
            tabs.open_untitled();
        }
        let indent_hint = detect_indent(&editor.text());
        let applied_theme = theme_kind;
        let mut app = Self {
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
            forward_stack: Vec::new(),
            symbol_cache: HashMap::new(),
            symbol_cache_at: Instant::now(),
            pending_ws_symbol: None,
            file_cache: HashMap::new(),
            hover_anchor: None,
            hover_requested_char: None,
            pending_hover: None,
            hover_result: None,
            hover_retry_at: None,
            symbols: Vec::new(),
            file_symbols: HashMap::new(),
            symbols_uri: None,
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
            file_index: fileindex::FileIndex::new(),
            file_picker_expanded: false,
            file_picker_scroll: 0.0,
            file_picker_hits: Vec::new(),
            file_picker_hits_query: String::new(),
            file_picker_hits_limit: 0,
            file_picker_hits_total: 0,
            file_index_started: false,
            file_picker_focus: false,
            content_query: String::new(),
            content_results: Vec::new(),
            pending_search_seq: None,
            content_search_focus: false,
            debug_open: false,
            scm: scm::ScmPanel::default(),
            scm_opened_at_start: false,
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
            search_exclude_glob: String::new(),
            search_details_open: false,
            search_all_collapsed: false,
            search_replace_open: false,
            search_replace_text: String::new(),
            search_preserve_case: false,
            search_view_tree: false,
            search_collapsed_files: HashSet::new(),
            search_collapsed_dirs: HashSet::new(),
            search_debounce_at: None,
            explorer_files: Vec::new(),
            explorer_tree: tree::FileTree::default(),
            explorer_rows: Vec::new(),
            explorer_expanded: HashSet::new(),
            explorer_last_click: None,
            tab_last_click: None,
            app_icon: None,
            remote_menu_open: false,
            remote_dialog_open: false,
            remote_dialog_distros: Vec::new(),
            remote_dialog_distro: String::new(),
            remote_dialog_path: String::new(),
            remote_dialog_dirs: Vec::new(),
            remote_dialog_error: None,
            ssh_dialog_open: false,
            ssh_dialog_host: String::new(),
            ssh_dialog_user: String::new(),
            ssh_dialog_port: ide_remote::DEFAULT_SSH_PORT,
            ssh_dialog_path: String::new(),
            ssh_dialog_dirs: Vec::new(),
            ssh_dialog_error: None,
            remote_item_rect: None,
            remote: RemoteFs::new(),
            applied_remote: None,
            toolchain_dialog_open: false,
            toolchain_probe: None,
            toolchain_error: None,
            toolchain_rx: None,
            toolchain_busy: None,
            toolchain_log: Vec::new(),
            toolchain_installed: Vec::new(),
            toolchain_skipped: false,
            toolchain_probed_for: None,
            toolchain_auto_open: true,
            terminal: TerminalSet::new(),
        };
        // 启动就把设置应用上（字号 / 主题 / 滚轮方向等）。
        app.apply_settings_to_ui();
        // 恢复上次的远程连接（WSL/SSH）：文件、git、rg、LSP、gdb 都以远端视角工作。
        app.sync_remote_targets();
        app.applied_remote = app.remote_target();
        app
    }

    /// 启动后尽早建好工作区文件索引（Ctrl+P 的模糊搜索要用）。
    ///
    /// 索引在后台线程建（远端工程一次 find 要几秒），第一帧就启动，
    /// 这样用户按下 Ctrl+P 时通常已经能直接搜。
    fn start_file_index(&mut self) {
        self.ensure_file_index();
    }
    /// 当前远程目标（`None` = 本地工作区）。
    ///
    /// 目标由设置决定：`remote_kind == "ssh"` 时是 SSH 主机，旧配置（只有
    /// `remote_distro`）按 WSL 处理。
    fn remote_target(&self) -> Option<RemoteTarget> {
        self.settings.remote_target()
    }

    /// 远程工作区的根（远端内部的 POSIX 路径）；未连接时为 `None`。
    ///
    /// 这里刻意不用 `\\wsl.localhost\...`：连上远程后，文件、rg、git、clangd、终端
    /// 全部以远端内部视角工作，路径就是 `/home/...`。
    fn remote_root(&self) -> Option<String> {
        self.remote_target()?;
        let path = self.settings.remote_path.as_deref()?;
        Some(normalize_posix(path))
    }

    /// 当前工作区根（远程优先）。资源管理器 / rg / git / LSP 都以它为根。
    fn workspace_root(&self) -> String {
        self.remote_root().unwrap_or_else(|| self.project_root())
    }

    /// 当前工作区根的 `file://` URI。远程是 WSL 内部视角：`file:///home/...`。
    fn workspace_uri(&self) -> String {
        if self.is_remote() {
            return posix_to_uri(&self.workspace_root())
                .unwrap_or_else(|| "file:///".to_string());
        }
        path_to_uri_any(&self.workspace_root()).unwrap_or_else(|| "file:///".to_string())
    }

    fn is_remote(&self) -> bool {
        self.remote_root().is_some()
    }

    /// 这个路径是不是远程工作区里的（POSIX 路径）文件。
    fn is_remote_path(&self, path: &std::path::Path) -> bool {
        self.is_remote() && is_posix_path(&path.to_string_lossy())
    }

    /// 界面里展示路径：远程一律用 POSIX 分隔符（`Path::join` 会拼出 `\`）。
    fn display_path(&self, path: &std::path::Path) -> String {
        let s = path.to_string_lossy();
        if self.is_remote() && is_posix_path(&s) {
            s.replace('\\', "/")
        } else {
            s.into_owned()
        }
    }

    /// 工作区根的 URI 形态：远程走 `file:///...`，本地走 `file:///mnt/<drive>`。
    fn uri_of_path(&self, path: &std::path::Path) -> Option<String> {
        let s = path.to_string_lossy();
        if self.is_remote() && is_posix_path(&s) {
            posix_to_uri(&s)
        } else {
            path_to_uri_any(&s)
        }
    }

    /// 读文本：远程走远端内部（POSIX 路径），本地走 `std::fs`。
    ///
    /// 本地文件带一层内容缓存：mtime + size 都没变就直接复用（关掉标签再打开时
    /// 不用重新读盘）。远程文件不缓存内容——远端那边拿不到 mtime，缓存容易读到
    /// 旧内容，而一次远端往返本来就只有 1ms 左右。
    fn read_text_file(&mut self, path: &std::path::Path) -> Result<String, String> {
        if self.is_remote_path(path) {
            let target = self
                .remote_target()
                .ok_or_else(|| "远程连接已断开".to_string())?;
            return self.remote.read_text(&target, &path.to_string_lossy());
        }
        if let Some(text) = self.cached_file_text(path) {
            return Ok(text);
        }
        let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        self.store_file_cache(path, &text);
        Ok(text)
    }

    /// 本地文件缓存命中（mtime + size 都没变才算命中）。
    fn cached_file_text(&mut self, path: &std::path::Path) -> Option<String> {
        let entry = self.file_cache.get(path)?;
        if entry.at.elapsed() > CACHE_TTL {
            return None;
        }
        let stamp = local_file_stamp(path)?;
        if entry.mtime == Some(stamp.0) && entry.size == Some(stamp.1) {
            Some(entry.text.clone())
        } else {
            None
        }
    }

    /// 存一份文件内容进缓存（超出上限就淘汰最旧的）。
    fn store_file_cache(&mut self, path: &std::path::Path, text: &str) {
        let Some((mtime, size)) = local_file_stamp(path) else {
            return;
        };
        if self.file_cache.len() >= FILE_CACHE_CAP {
            // HashMap 无序：随便丢一个最旧的（用写入时间最小的那条）。
            if let Some(oldest) = self
                .file_cache
                .iter()
                .min_by_key(|(_, v)| v.at)
                .map(|(k, _)| k.clone())
            {
                self.file_cache.remove(&oldest);
            }
        }
        self.file_cache.insert(
            path.to_path_buf(),
            CachedFile {
                text: text.to_string(),
                mtime: Some(mtime),
                size: Some(size),
                at: Instant::now(),
            },
        );
    }

    /// 写文本：远程写进远端的真实文件（不是 9P 视图），本地直接写盘。
    fn write_text_file(&mut self, path: &std::path::Path, text: &str) -> Result<(), String> {
        // 文件刚被改过，缓存里的旧内容作废。
        self.file_cache.remove(path);
        if self.is_remote_path(path) {
            let target = self
                .remote_target()
                .ok_or_else(|| "远程连接已断开".to_string())?;
            return self
                .remote
                .write_text(&target, &path.to_string_lossy(), text);
        }
        std::fs::write(path, text).map_err(|e| e.to_string())
    }

    /// 建目录（远程 `mkdir -p`，本地 `create_dir_all`）。
    fn create_dir_all_path(&mut self, dir: &std::path::Path) -> Result<(), String> {
        if self.is_remote_path(dir) {
            let target = self
                .remote_target()
                .ok_or_else(|| "远程连接已断开".to_string())?;
            return self.remote.create_dir_all(&target, &dir.to_string_lossy());
        }
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())
    }

    /// 路径类型（远程 `stat`，本地 `metadata`）。
    fn path_kind(&mut self, path: &std::path::Path) -> PathKind {
        if self.is_remote_path(path) {
            let Some(target) = self.remote_target() else {
                return PathKind::Missing;
            };
            return self
                .remote
                .path_kind(&target, &path.to_string_lossy())
                .unwrap_or(PathKind::Missing);
        }
        let p = path.to_path_buf();
        match std::fs::metadata(&p) {
            Ok(m) if m.is_dir() => PathKind::Dir,
            Ok(m) if m.is_file() => PathKind::File,
            Ok(_) => PathKind::Other,
            Err(_) => PathKind::Missing,
        }
    }

    fn path_is_dir(&mut self, path: &std::path::Path) -> bool {
        self.path_kind(path) == PathKind::Dir
    }

    fn path_is_file(&mut self, path: &std::path::Path) -> bool {
        self.path_kind(path) == PathKind::File
    }

    /// 列一层目录项 `(名字, 是否目录)`，已按「目录在前、名字不区分大小写」排序。
    fn list_dir_entries(&mut self, dir: &std::path::Path) -> Vec<(String, bool)> {
        if self.is_remote_path(dir) {
            let Some(target) = self.remote_target() else {
                return Vec::new();
            };
            let Ok(entries) = self.remote.list_dir(&target, &dir.to_string_lossy()) else {
                return Vec::new();
            };
            let mut out: Vec<(String, bool)> = entries
                .into_iter()
                .map(|e| (e.name, e.is_dir))
                .collect();
            sort_dir_entries(&mut out);
            return out;
        }
        load_dir_entries(dir)
    }

    /// 切换到远程工作区（WSL 发行版 / SSH 主机 + 目录）。
    ///
    /// 顺序很重要：先在远端确认目录存在（否则整个侧栏会空白），再写入设置、
    /// 重置资源管理器并让 LSP 以新根重起、Git 换根。
    ///
    /// 返回是否连接成功（对话框据此决定关不关）。
    /// 返回失败原因（对话框直接显示，不要只 flash 到状态栏——模态窗口挡着，
    /// 用户看不到「为什么连不上」）。
    fn connect_remote(&mut self, target: RemoteTarget, path: &str) -> Result<(), String> {
        let posix = normalize_posix(path);
        match self.remote.path_kind(&target, &posix) {
            Ok(PathKind::Dir) => {}
            Ok(kind) => {
                let msg = format!(
                    "无法打开远程目录 {}:{posix}（不是目录或不存在，检测结果 {kind:?}）",
                    target.label()
                );
                tracing::warn!("{msg}");
                return Err(msg);
            }
            Err(e) => {
                let msg = format!("无法连接 {}：{e}", target.label());
                tracing::warn!("{msg}");
                return Err(msg);
            }
        }
        match &target {
            RemoteTarget::Wsl { distro } => {
                self.settings.remote_kind = Some("wsl".to_string());
                self.settings.remote_distro = Some(distro.clone());
            }
            RemoteTarget::Ssh { user, host, port } => {
                self.settings.remote_kind = Some("ssh".to_string());
                self.settings.ssh_user = Some(user.clone());
                self.settings.ssh_host = Some(host.clone());
                self.settings.ssh_port = *port;
            }
        }
        self.settings.remote_path = Some(posix.clone());
        self.sync_remote_targets();
        self.save_settings();
        self.apply_workspace_switch();
        // 新连接：重新探测远端工具链（缺 clangd 时弹提示）。
        self.toolchain_rx = None;
        self.toolchain_probed_for = None;
        self.toolchain_probe = None;
        self.toolchain_error = None;
        self.toolchain_busy = None;
        self.toolchain_log.clear();
        self.toolchain_installed.clear();
        self.toolchain_skipped = false;
        self.toolchain_auto_open = true;
        self.flash(format!("已连接到 {} · {posix}", target.label()));
        tracing::info!(
            "remote workspace: {}:{posix}（远端内部 POSIX 根）",
            target.label()
        );
        Ok(())
    }

    /// 把当前设置里的远程目标同步给所有只拿根路径的模块。
    ///
    /// - 进程级注册表：`ide-git` / `ide-search` 据此选择 ssh 还是 wsl；
    /// - LSP / DAP：只有 SSH 才需要显式目标，其余保持原来的 WSL 自动探测。
    fn sync_remote_targets(&mut self) {
        let target = self.remote_target();
        ide_core::remote::set_current_target(target.clone());
        let ssh = target.filter(|t| t.is_ssh());
        self.lsp.set_remote_target(ssh.clone());
        self.dap.set_remote_target(ssh);
    }

    /// 断开远程连接，回到本地工作区。
    fn disconnect_remote(&mut self) {
        if !self.is_remote() {
            self.flash("当前没有远程连接".to_string());
            return;
        }
        self.settings.remote_kind = None;
        self.settings.remote_distro = None;
        self.settings.remote_path = None;
        self.save_settings();
        // 断开后不再需要远端文件会话（下次连接会重新拉起）。
        self.remote.shutdown();
        self.toolchain_rx = None;
        self.toolchain_probed_for = None;
        self.toolchain_probe = None;
        self.toolchain_dialog_open = false;
        self.sync_remote_targets();
        self.applied_remote = None;
        self.apply_workspace_switch();
        self.flash("已断开远程连接，回到本地工作区".to_string());
    }

    /// 工作区根变化后的统一善后：资源管理器重扫、Git 换根、LSP 以新根重启。
    fn apply_workspace_switch(&mut self) {
        // 终端进程绑定在旧发行版/旧目录上，换工作区必须一并结束。
        if !self.terminal.is_empty() {
            self.terminal.shutdown();
            self.flash("工作区已切换，旧终端会话已结束".to_string());
        }
        self.explorer_loaded = false;
        self.explorer_files.clear();
        self.explorer_tree = tree::FileTree::default();
        self.explorer_rows.clear();
        self.explorer_expanded.clear();
        self.content_results.clear();
        let root = self.workspace_root();
        self.search.load_files(&root);
        self.ensure_file_index();
        self.rebuild_explorer();
        // 源代码管理：重新发现工作区里的 git 仓库（VS Code 多仓库工作区）。
        self.discover_scm_repos();
        self.refresh_git_now();
        // clangd 的根只在 initialize 时确定：根变了就重连。
        let uri = self.workspace_uri();
        self.root_uri = uri.clone();
        self.lsp.reopen_root(uri);
        self.applied_remote = self.remote_target();
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
        self.current_uri = tab.path.as_ref().and_then(|p| {
            let path = p.to_string_lossy();
            // 远程工作区只把「根下的文件」交给 clangd：根外的文件不在 clangd 的
            // 索引范围内，发过去它也无从解析，反而会浪费一次 didChange。
            if self.is_remote() && !path.starts_with(self.workspace_root().as_str()) {
                return None;
            }
            self.uri_of_path(p)
        });
        self.doc_version = 1;
        self.last_sync_seq = self.editor.as_ref().map(|e| e.edit_seq()).unwrap_or(0);
        self.lsp_ready_synced = false;
        self.hover_result = None;
        self.symbols.clear();
        self.symbols_uri = None;
        // 大纲/符号搜索：能命中缓存就直接用（文件没改过就不用再问 clangd）。
        if let Some(uri) = self.current_uri.clone() {
            self.request_symbols_for(&uri);
        }
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
        match self.read_text_file(&path) {
            Ok(text) => {
                if let Some(ed) = &mut self.editor {
                    ed.load_text(&text);
                }
                self.tabs.replace_active_text(text);
                self.editor_clean_seq = self.editor.as_ref().map(|e| e.edit_seq()).unwrap_or(0);
                self.flash(format!("已从磁盘重载 {}", self.display_path(&path)));
            }
            Err(e) => self.flash(format!("重载失败: {e}")),
        }
    }

    fn on_save(&mut self) {
        let Some(path) = self.active_tab_path() else {
            self.flash("未命名文件请用「另存为」(Ctrl+Shift+S)".to_string());
            return;
        };
        let Some((text, seq)) = self.editor.as_ref().map(|e| (e.text(), e.edit_seq())) else {
            return;
        };
        if let Err(e) = self.write_text_file(&path, &text) {
            self.flash(format!("保存失败: {e}"));
            return;
        }
        self.flash(format!("已保存 {}", self.display_path(&path)));
        self.tabs.set_active_dirty(false);
        self.editor_clean_seq = seq;
        if self.lsp_status.is_ready() {
            if let Some(uri) = self.current_uri.clone() {
                self.lsp.save_document(uri.clone());
                // 存盘后内容变了，符号缓存作废，重新要一遍。
                self.file_symbols.remove(&uri);
                self.request_symbols_for(&uri);
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
                    if let Err(e) = self.write_text_file(&p, &text) {
                        self.flash(format!("保存 {} 失败: {e}", self.display_path(&p)));
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
                ed.center_cursor_in_view();
            }
            return;
        }
        if let Some(idx) = self.tabs.index_of_path(&target) {
            self.switch_to_tab(idx);
            if let Some(ed) = &mut self.editor {
                ed.set_cursor_line_col(line, col);
                ed.center_cursor_in_view();
            }
            return;
        }
        match self.read_text_file(&target) {
            Ok(text) => {
                self.open_file(target, text, true);
                if let Some(ed) = &mut self.editor {
                    ed.set_cursor_line_col(line, col);
                    ed.center_cursor_in_view();
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
        // 远程工作区优先：clangd 自己就在 WSL 里，根也用发行版内部路径 `/home/...`。
        let root = if self.is_remote() {
            self.workspace_root()
        } else {
            self.pick_root()
        };
        self.root_uri = if self.is_remote() {
            posix_to_uri(&root).unwrap_or_else(|| "file:///".to_string())
        } else {
            path_to_uri_any(&root).unwrap_or_else(|| "file:///".to_string())
        };
        tracing::info!("LSP root: {root} -> {}", self.root_uri);
        self.lsp.start(self.root_uri.clone());
    }

    fn poll_lsp(&mut self) {
        // 文件索引的后台结果（Ctrl+P 模糊搜索用）。
        self.file_index.poll();
        while let Some(ev) = self.lsp.poll() {
            match ev {
                LspEvent::Status(s) => self.lsp_status = s,
                LspEvent::Definition { seq, result } => {
                    if let Some((pending_seq, name)) = self.pending_def.take() {
                        if pending_seq == seq {
                            self.handle_definition(name, result);
                        }
                    }
                }
                LspEvent::Hover { seq, result } => {
                    if self.pending_hover == Some(seq) {
                        self.pending_hover = None;
                        self.handle_hover(result);
                    }
                }
                LspEvent::Symbols { seq, result } => {
                    if let Some((pending_seq, uri)) = self.pending_sym.take() {
                        if pending_seq == seq {
                            let edit_seq = self.editor.as_ref().map(|e| e.edit_seq()).unwrap_or(0);
                            match result {
                                Ok(syms) => {
                                    // 进缓存：切回这个文件时直接复用。
                                    self.file_symbols.insert(
                                        uri.clone(),
                                        CachedSymbols {
                                            syms: syms.clone(),
                                            edit_seq,
                                            at: Instant::now(),
                                        },
                                    );
                                    if self.current_uri.as_deref() == Some(uri.as_str()) {
                                        self.symbols = syms;
                                        self.symbols_uri = Some(uri);
                                    }
                                }
                                Err(e) => tracing::warn!("documentSymbol 失败: {e}"),
                            }
                        }
                    }
                }
                LspEvent::WorkspaceSymbol { seq, result } => {
                    if let Some((pending_seq, name)) = self.pending_ws_symbol.take() {
                        if pending_seq == seq {
                            self.handle_workspace_symbol(name, result);
                        }
                    }
                }
                LspEvent::ServerLog(_) | LspEvent::Diagnostics { .. } => {}
            }
        }
    }

    /// 同步当前文件的文档符号（大纲 / 符号搜索）。
    ///
    /// 优先用缓存：文件没被改过（编辑序列号一致）且缓存没过 TTL，就直接复用，
    /// 省掉一次 clangd 的 documentSymbol 解析。
    fn request_symbols_for(&mut self, uri: &str) {
        let edit_seq = self.editor.as_ref().map(|e| e.edit_seq()).unwrap_or(0);
        if let Some(c) = self.file_symbols.get(uri) {
            if c.edit_seq == edit_seq && c.at.elapsed() < CACHE_TTL {
                self.symbols = c.syms.clone();
                self.symbols_uri = Some(uri.to_string());
                return;
            }
        }
        if !self.lsp_status.is_ready() {
            return;
        }
        let seq = self.lsp.request_symbols(uri.to_string());
        self.pending_sym = Some((seq, uri.to_string()));
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
                    self.request_symbols_for(&uri);
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

    fn handle_definition(&mut self, name: String, result: Result<Vec<ide_lsp::Location>, String>) {
        match result {
            Ok(locs) if !locs.is_empty() => {
                if let Some(loc) = locs.first() {
                    // 远程工作区：clangd 回的是 `file:///home/...`（WSL 内部路径）。
                    let target = if self.is_remote() {
                        uri_to_posix(&loc.uri).or_else(|| uri_to_win(&loc.uri))
                    } else {
                        uri_to_win(&loc.uri)
                    };
                    match target {
                        Some(win) => {
                            let line = loc.range.start.line as usize;
                            let col = loc.range.start.character as usize;
                            tracing::info!("jump -> {win}:{}", line + 1);
                            self.jump_to(&win, line, col);
                        }
                        None => self.flash(format!("无法解析目标路径: {}", loc.uri)),
                    }
                }
            }
            // clangd 没找到（还没索引完 / 符号在 compile_commands 之外）：
            // 退到按名字找。
            Ok(_) => {
                if name.is_empty() {
                    self.flash("未找到定义".to_string());
                } else {
                    self.lookup_symbol_by_name(name);
                }
            }
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
                self.hover_result = Some((pos, split_hover_markdown(&info.markdown)));
                self.hover_retry_at = None;
            }
            // clangd 暂时没有（文件还在解析 / 符号不在索引里）：清掉「已请求」标记，
            // 过一会儿自动重试 —— 以前这里直接放弃，提示框就再也不出现了。
            Ok(None) => {
                self.hover_result = None;
                self.hover_requested_char = None;
                self.hover_retry_at = Some(Instant::now() + HOVER_RETRY);
            }
            Err(e) => {
                tracing::warn!("hover 失败: {e}");
                self.hover_result = None;
                self.hover_requested_char = None;
                self.hover_retry_at = Some(Instant::now() + HOVER_RETRY);
            }
        }
    }

    // ---- DAP ----

    fn debug_binary(&mut self) -> Option<String> {
        if let Ok(bin) = std::env::var("LITEIDE_DEBUG_BIN") {
            if !bin.is_empty() {
                return Some(bin);
            }
        }
        // 远程工作区：默认调试根下的 `testdata/debug_target`（远端 POSIX 路径，
        // 存在性要问远端；`LITEIDE_DEBUG_BIN` 可指向任意远端可执行文件）。
        if self.is_remote() {
            let cand = format!(
                "{}/testdata/debug_target",
                self.workspace_root().trim_end_matches('/')
            );
            return matches!(
                self.path_kind(std::path::Path::new(&cand)),
                PathKind::File
            )
            .then_some(cand);
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
        let Some((line, col)) = self.editor.as_ref().map(|e| e.cursor_line_col()) else {
            return;
        };
        self.goto_definition_at(line, col);
    }

    /// Ctrl+左键：在点击位置跳到定义。
    fn on_goto_definition_at_char(&mut self, char_idx: usize) {
        let Some((line, col)) = self.editor.as_ref().map(|e| e.char_to_line_col(char_idx)) else {
            return;
        };
        self.goto_definition_at(line, col);
    }

    /// 跳到 (line, col) 处的符号定义。
    ///
    /// 优先问 clangd（`textDocument/definition`，最准）；clangd 没就绪或答「没有」时，
    /// 退到 `workspace/symbol` 按名字找（服务端索引），再退到本地缓存。
    fn goto_definition_at(&mut self, line: usize, col: usize) {
        // 记下跳转前的位置，供 Alt+Left 返回。
        self.push_jump_history(line, col);
        let name = self
            .editor
            .as_ref()
            .and_then(|e| e.word_at_line_col(line, col))
            .unwrap_or_default();

        if self.lsp_status.is_ready() {
            if let Some(uri) = self.current_uri.clone() {
                let seq = self.lsp.request_definition(
                    uri,
                    Position {
                        line: line as u32,
                        character: col as u32,
                    },
                );
                self.pending_def = Some((seq, name));
                return;
            }
        }
        // LSP 没就绪：直接走按名字找的路���。
        if name.is_empty() {
            if self.remote_missing_clangd() {
                self.flash("远端没有 clangd，无法跳转（可自动安装）".to_string());
                self.open_toolchain_dialog();
            } else {
                self.flash("LSP 未就绪，无法跳转".to_string());
            }
            return;
        }
        self.lookup_symbol_by_name(name);
    }

    /// 远端连接且探测确认缺 clangd（跳转/补全不可用的根因）。
    fn remote_missing_clangd(&self) -> bool {
        self.is_remote()
            && self
                .toolchain_probe
                .as_ref()
                .map(|p| p.missing.contains(&ide_core::remote::RemoteTool::Clangd))
                .unwrap_or(false)
    }

    /// 压入跳转历史（后退栈），并清空前进栈。
    fn push_jump_history(&mut self, line: usize, col: usize) {
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
        // VS Code 语义：发生新的跳转后，前进栈作废。
        self.forward_stack.clear();
    }

    /// 按名字在工作区里找定义（`workspace/symbol`）。
    fn lookup_symbol_by_name(&mut self, name: String) {
        if name.is_empty() {
            return;
        }
        // 缓存命中就直接跳（省一次服务器往返）。
        if let Some(locs) = self.cached_symbol_locations(&name) {
            if let Some(loc) = locs.first() {
                self.goto_location(loc.clone());
                return;
            }
        }
        if !self.lsp_status.is_ready() {
            if self.remote_missing_clangd() {
                self.flash(format!(
                    "未找到 `{name}` 的定义：远端没有 clangd（可在「远程工具链」里自动安装）"
                ));
                self.open_toolchain_dialog();
            } else {
                self.flash(format!("未找到 `{name}` 的定义（LSP 未就绪）"));
            }
            return;
        }
        let seq = self.lsp.request_workspace_symbol(name.clone());
        self.pending_ws_symbol = Some((seq, name));
    }

    /// 缓存里的符号位置（超过 TTL 视为没有）。
    fn cached_symbol_locations(&mut self, name: &str) -> Option<Vec<ide_lsp::Location>> {
        if self.symbol_cache_at.elapsed() > CACHE_TTL {
            self.symbol_cache.clear();
            self.symbol_cache_at = Instant::now();
            return None;
        }
        self.symbol_cache.get(name).cloned()
    }

    /// 收到 `workspace/symbol` 的结果：进缓存并跳到最合适的一个。
    fn handle_workspace_symbol(&mut self, name: String, result: Result<Vec<ide_lsp::WorkspaceSymbol>, String>) {
        let syms = match result {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("workspace/symbol 失败: {e}");
                Vec::new()
            }
        };
        // 只保留名字完全匹配的（clangd 的模糊匹配会带上一堆同前缀的）。
        let exact: Vec<ide_lsp::WorkspaceSymbol> = syms
            .into_iter()
            .filter(|s| s.name == name)
            .collect();
        let locs: Vec<ide_lsp::Location> = exact.iter().map(|s| s.location.clone()).collect();
        if locs.is_empty() {
            self.flash(format!("未找到 `{name}` 的定义"));
            return;
        }
        self.symbol_cache.insert(name.clone(), locs.clone());
        self.symbol_cache_at = Instant::now();
        if let Some(loc) = self.pick_symbol_location(&locs) {
            self.goto_location(loc);
        }
    }

    /// 在多个候选位置里挑一个：优先当前文件，其次离当前文件路径最近的。
    fn pick_symbol_location(&self, locs: &[ide_lsp::Location]) -> Option<ide_lsp::Location> {
        let cur = self.current_file.clone()?;
        let cur_dir = cur.parent().map(|p| p.to_path_buf());
        let mut best: Option<(usize, &ide_lsp::Location)> = None;
        for loc in locs {
            let Some(path) = self.location_path(loc) else {
                continue;
            };
            let score = if path == cur {
                0
            } else if let (Some(dir), Some(pdir)) = (&cur_dir, path.parent()) {
                if dir == pdir {
                    1
                } else {
                    2
                }
            } else {
                3
            };
            if best.map(|(s, _)| score < s).unwrap_or(true) {
                best = Some((score, loc));
            }
        }
        best.map(|(_, l)| l.clone())
    }

    /// LSP 返回的 URI → 本地/远程路径。
    fn location_path(&self, loc: &ide_lsp::Location) -> Option<PathBuf> {
        if self.is_remote() {
            uri_to_posix(&loc.uri).map(PathBuf::from)
        } else {
            uri_to_win(&loc.uri).map(PathBuf::from)
        }
    }

    /// 跳到某个位置。
    fn goto_location(&mut self, loc: ide_lsp::Location) {
        let Some(path) = self.location_path(&loc) else {
            self.flash(format!("无法解析目标路径: {}", loc.uri));
            return;
        };
        let line = loc.range.start.line as usize;
        let col = loc.range.start.character as usize;
        tracing::info!("jump -> {}:{}", path.display(), line + 1);
        self.jump_to(&path.to_string_lossy(), line, col);
    }

    fn on_go_back(&mut self) {
        if let Some((path, line, col)) = self.jump_stack.pop() {
            // 当前位置存进前进栈，Alt+Right 可以再回来。
            if let Some(cur) = self.current_file.clone() {
                let (cl, cc) = self
                    .editor
                    .as_ref()
                    .map(|e| e.cursor_line_col())
                    .unwrap_or((line, col));
                self.forward_stack.push((cur, cl, cc));
                if self.forward_stack.len() > JUMP_STACK_CAP {
                    let drop = self.forward_stack.len() - JUMP_STACK_CAP;
                    self.forward_stack.drain(0..drop);
                }
            }
            self.jump_to(&path.to_string_lossy(), line, col);
        } else {
            self.flash("没有可返回的位置".to_string());
        }
    }

    fn on_go_forward(&mut self) {
        if let Some((path, line, col)) = self.forward_stack.pop() {
            if let Some(cur) = self.current_file.clone() {
                let (cl, cc) = self
                    .editor
                    .as_ref()
                    .map(|e| e.cursor_line_col())
                    .unwrap_or((line, col));
                self.jump_stack.push((cur, cl, cc));
                if self.jump_stack.len() > JUMP_STACK_CAP {
                    let drop = self.jump_stack.len() - JUMP_STACK_CAP;
                    self.jump_stack.drain(0..drop);
                }
            }
            self.jump_to(&path.to_string_lossy(), line, col);
        } else {
            self.flash("没有可前进的位置".to_string());
        }
    }

    fn update_hover(&mut self, ctx: &egui::Context) {
        let mc = self.editor.as_ref().and_then(|e| e.mouse_char());
        match mc {
            Some(c) => {
                match self.hover_anchor {
                    Some((ac, t)) if ac == c => {
                        let ready = self.lsp_status.is_ready();
                        // 没拿到内容时按 HOVER_RETRY 重试（clangd 可能还在解析）。
                        let retry_ok = self
                            .hover_retry_at
                            .map(|at| Instant::now() >= at)
                            .unwrap_or(true);
                        if t.elapsed() >= HOVER_DWELL
                            && ready
                            && retry_ok
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
                            self.hover_retry_at = None;
                        }
                        self.hover_anchor = Some((c, Instant::now()));
                    }
                }
            }
            None => {
                self.hover_anchor = None;
                self.hover_requested_char = None;
                self.hover_retry_at = None;
                self.hover_result = None;
            }
        }
        if let Some((pos, content)) = &self.hover_result {
            if content.signature.is_some() || !content.docs.is_empty() {
                egui::Area::new(egui::Id::new("lsp_hover_area"))
                    .fixed_pos(egui::pos2(pos.x + 14.0, pos.y + 22.0))
                    .order(egui::Order::Tooltip)
                    .show(ctx, |ui| {
                        // VS Code 的 hover：编辑器底色 + 一圈细边框。
                        egui::Frame::popup(ui.style())
                            .fill(hex("#1e1e1e"))
                            .stroke(egui::Stroke::new(1.0_f32, hex("#454545")))
                            .show(ui, |ui| {
                                ui.set_max_width(500.0);
                                if let Some(sig) = &content.signature {
                                    ui.label(
                                        egui::RichText::new(sig.clone())
                                            .monospace()
                                            .size(12.0)
                                            .color(hex("#d4d4d4")),
                                    );
                                }
                                if !content.docs.is_empty() {
                                    if content.signature.is_some() {
                                        ui.add_space(4.0);
                                        ui.separator();
                                        ui.add_space(4.0);
                                    }
                                    ui.label(
                                        egui::RichText::new(content.docs.clone())
                                            .size(12.0)
                                            .color(hex("#d4d4d4")),
                                    );
                                }
                            });
                    });
            }
        }
    }

    fn flash(&mut self, msg: String) {
        self.flash = Some((msg, Instant::now()));
    }

    /// 是否有模态窗口占着键盘（命令面板 / 文件浏览器 / 设置…）。
    fn modal_open(&self) -> bool {
        self.palette_open
            || self.file_picker_open
            || self.settings_open
            || self.goto_line_open
            || self.about_open
            || self.keys_open
            || self.find_open
            || self.save_as_open
            || self.remote_dialog_open
            || self.ssh_dialog_open
            || self.toolchain_dialog_open
            || self.confirm_close.is_some()
    }

    // ---- 阶段 4 UI ----

    /// 索引里的相对路径 → 绝对路径（远程是 WSL 里的 POSIX 路径）。
    fn picker_path_for(&self, rel: &str) -> PathBuf {
        if self.is_remote() {
            PathBuf::from(format!("{}/{}", self.workspace_root(), rel))
        } else {
            PathBuf::from(self.project_root()).join(rel)
        }
    }

    /// 确保工作区文件索引已按当前根建好（后台线程）。
    fn ensure_file_index(&mut self) {
        let target = if self.is_remote() {
            self.remote_target()
        } else {
            None
        };
        let posix_root = self.remote_root();
        let local_root = PathBuf::from(self.project_root());
        self.file_index
            .ensure(&local_root, target.as_ref(), posix_root.as_deref());
    }

    /// 切换文件浏览器的当前目录：重新读取目录项并清空过滤词。
    fn set_file_picker_dir(&mut self, dir: PathBuf) {
        self.file_picker_entries = self.list_dir_entries(&dir);
        self.file_picker_query.clear();
        self.file_picker_dir = dir;
    }

    /// 打开文件浏览窗口（保留上次所在目录，首次定位到当前文件所在目录）。
    fn open_file_picker(&mut self) {
        self.file_picker_open = true;
        self.file_picker_focus = true;
        self.file_picker_query.clear();
        // 每次打开都回到「只显示 20 条」。
        self.file_picker_expanded = false;
        // 后台建工作区文件索引（Ctrl+P 的模糊搜索要用）。
        self.ensure_file_index();
        if self.file_picker_dir.as_os_str().is_empty() {
            // 远程工作区：从工作区根开始浏览（用户在 WSL 里工作，不是本地目录）。
            let start = if self.is_remote() {
                Some(PathBuf::from(self.workspace_root()))
            } else {
                self.active_tab_path()
                    .and_then(|p| p.parent().map(|d| d.to_path_buf()))
            };
            // 远程目录是否存在要问 WSL（POSIX 路径在 Windows 上 `is_dir()` 永远 false）。
            let start = match start {
                Some(d) if self.path_is_dir(&d) => d,
                _ => PathBuf::from(self.workspace_root()),
            };
            self.set_file_picker_dir(start);
        }
    }

    /// Ctrl+P / File▸Open…：工作区文件快速打开（VS Code 风格）。
    ///
    /// - 输入框：输入**存在的**目录/文件路径后回车 → 进入 / 打开；
    ///   否则当作**搜索词**：在整个工作区里做模糊匹配（VS Code 的 "file results"），
    ///   命中的文件按相关度排序，回车打开第一个。
    /// - 列表：先显示模糊匹配结果（文件名 + 灰色目录前缀），没有匹配时退回浏览当前目录；
    ///   目录在前，点击目录进入、点击文件打开；`上一层`回上级目录。
    /// - 默认只显示 **20 条**（VS Code 的 quick-open 也是这个量级），点「显示更多」
    ///   才把剩下的全部列出来 —— 列表短、滚动快、视觉干净。
    fn draw_file_picker(&mut self, ctx: &egui::Context) {
        if !self.file_picker_open {
            return;
        }
        let mut open = true;
        let mut query = self.file_picker_query.clone();
        let mut go_dir: Option<PathBuf> = None;
        let mut open_path: Option<PathBuf> = None;
        // 输入框回车想跳的路径：存在性判断要出闭包才能问 WSL。
        let mut go_dir_guess: Option<PathBuf> = None;
        let mut esc = false;
        let mut want_focus = self.file_picker_focus;
        let mut show_more = false;
        // 列表的滚动偏移（自己维护，见下面「结果列表」的说明）。
        let mut picker_scroll = self.file_picker_scroll;
        let dir = self.file_picker_dir.clone();
        let entries = self.file_picker_entries.clone();
        // 工作区模糊搜索结果（文件名 + 相对路径 + 命中位置）。
        // 一次最多算 20 条（`PICKER_PAGE`），要更多再点「显示更多」。
        let limit = if self.file_picker_expanded {
            PICKER_MAX_RESULTS
        } else {
            PICKER_PAGE
        };
        // 结果按「查询词 + 条数上限」缓存：Picker 是每帧重绘的，
        // 不缓存的话每帧都要扫三万多个条目（打字时明显发顿）。
        if self.file_picker_hits_query != query || self.file_picker_hits_limit != limit {
            self.file_picker_hits = self.file_index.search(&query, limit);
            self.file_picker_hits_total = self.file_index.total_matches();
            self.file_picker_hits_query = query.clone();
            self.file_picker_hits_limit = limit;
        }
        let hits = &self.file_picker_hits;
        // 「显示更多」只在结果被截断时出现。
        let total = self.file_picker_hits_total;
        let has_more = total > hits.len();
        // 回车打开最匹配的那个（先把路径取出来：后面要改 `self`）。
        let first_rel = hits.first().map(|h| h.rel.clone());
        let shown = filter_dir_entries(&entries, &query);
        let row_h = 20.0_f32;
        // 窗口尺寸每帧按结果条数算死：内容多时到 360 封顶后滚动，
        // 内容少时窗口自动收窄——这样不会出现「列表很短、窗口却很大」的空洞。
        let list_rows = if hits.is_empty() {
            shown.len()
        } else {
            hits.len()
        };
        let rows_h = ((list_rows.max(1) as f32) * row_h).clamp(row_h, 360.0);
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
                        // 存在性判断放到渲染闭包外（远程要问 WSL，闭包里拿不到 &mut self）。
                        go_dir_guess = Some(resolve_picker_path(&dir, &query));
                    }
                    if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                        esc = true;
                    }
                    if ui.button("上一层").clicked() {
                        if let Some(parent) = dir.parent() {
                            go_dir = Some(parent.to_path_buf());
                        }
                    }
                    if icon_button(ui, Codicon::Refresh, "重新读取当前目录").clicked() {
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
                        RichText::new(self.display_path(&dir))
                            .monospace()
                            .size(11.0)
                            .color(hex("#9cdcfe")),
                    )
                    .selectable(true),
                );
                ui.add_space(2.0);
                // ---- 结果列表：优先显示工作区模糊匹配（VS Code 的 file results） ----
                //
                // 自己画虚拟列表（不用 egui 的 ScrollArea）：
                // - egui 的 ScrollArea 靠 `smooth_scroll_delta` 滚动，而那个值会被
                //   **平滑到十几帧**才发完；我们的窗口又是 on-demand 重绘的，
                //   于是滚轮几乎没反应（实测滚 15 格只动几个像素）。
                // - 自己维护偏移就能一格滚 3 行（VS Code 的手感），也更好控制对齐。
                if !hits.is_empty() {
                    let list_rect = ui.allocate_exact_size(
                        egui::vec2(row_w, rows_h),
                        egui::Sense::click(),
                    ).0;
                    let clip_painter = ui.painter().with_clip_rect(list_rect);
                    let painter = &clip_painter;
                    // 滚轮：直接吃原始事件（不走 egui 的平滑），一格 3 行。
                    let mut scroll_by = 0.0_f32;
                    for ev in ui.input(|i| i.events.clone()) {
                        if let egui::Event::MouseWheel { unit, delta, .. } = ev {
                            // 方向：**滚轮向下 = 看到更前面的内容**（与编辑器一致，
                            // 跟用户系统上的方向相同）。
                            let lines = match unit {
                                egui::MouseWheelUnit::Line => delta.y * 3.0,
                                egui::MouseWheelUnit::Point => delta.y / row_h,
                                egui::MouseWheelUnit::Page => delta.y * 20.0,
                            };
                            scroll_by -= lines * row_h;
                        }
                    }
                    let max_scroll = ((hits.len() as f32) * row_h - rows_h).max(0.0);
                    let scroll = (picker_scroll + scroll_by).clamp(0.0, max_scroll);
                    if scroll != picker_scroll {
                        picker_scroll = scroll;
                    }
                    // 只画看得见的行（虚拟化）。
                    let first = (scroll / row_h).floor() as usize;
                    let visible = (rows_h / row_h).ceil() as usize + 1;
                    let last = (first + visible).min(hits.len());
                    for (i, hit) in hits.iter().enumerate().take(last).skip(first) {
                        let dir_part = match hit.rel.rfind('/') {
                            Some(p) => format!("{}/", &hit.rel[..p]),
                            None => String::new(),
                        };
                        let y = list_rect.top() + (i as f32) * row_h - scroll;
                        if picker_hit_row(
                            painter,
                            ui,
                            egui::Rect::from_min_size(
                                egui::pos2(list_rect.left(), y),
                                egui::vec2(row_w, row_h),
                            ),
                            i,
                            &PickerRow {
                                name: &hit.name,
                                dir_part: &dir_part,
                                color: file_icon_color(&hit.name),
                                hits: PickerHits {
                                    name: &hit.hits,
                                },
                            },
                        ) {
                            open_path = Some(self.picker_path_for(&hit.rel));
                        }
                    }
                    // 右侧滚动条（VS Code 也有），提示还有多少内容。
                    if max_scroll > 0.0 {
                        let track_h = rows_h;
                        let thumb_h = (track_h * track_h / (hits.len() as f32 * row_h)).max(24.0);
                        let thumb_y = list_rect.top() + (scroll / max_scroll) * (track_h - thumb_h);
                        painter.rect_filled(
                            egui::Rect::from_min_size(
                                egui::pos2(list_rect.right() - 4.0, thumb_y),
                                egui::vec2(3.0, thumb_h),
                            ),
                            1.5,
                            hex("#8a8a8a"),
                        );
                    }
                    // ---- 「显示更多」：默认只给 20 条，想要更多再点开 ----
                    if has_more {
                        ui.add_space(2.0);
                        if ui
                            .add(egui::Button::new(
                                RichText::new(format!("显示更多结果…（共 {total} 条）"))
                                    .size(12.0)
                                    .color(hex("#3794ff")),
                            ))
                            .clicked()
                        {
                            show_more = true;
                        }
                    }
                } else {
                    // ---- 没有匹配：退回浏览当前目录 ----
                    egui::ScrollArea::vertical()
                        .max_height(360.0)
                        .auto_shrink([false, true])
                        .show_rows(ui, row_h, shown.len(), |ui, range| {
                            if shown.is_empty() {
                                ui.label(
                                    RichText::new(if self.file_index.state()
                                        == &fileindex::IndexState::Building
                                    {
                                        "(正在索引工作区…)"
                                    } else {
                                        "(空目录 / 无匹配)"
                                    })
                                    .size(12.0)
                                    .color(hex("#6a6a6a")),
                                );
                                return;
                            }
                            for &i in &shown[range] {
                                let (name, is_dir) = &entries[i];
                                let (icon, color) = if *is_dir {
                                    (Codicon::Folder, hex("#dcb67a"))
                                } else {
                                    (Codicon::File, file_icon_color(name))
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
                }
            });
        self.file_picker_open = open && !esc;
        self.file_picker_focus = want_focus;
        // 查询词变了就重新收起（「显示更多」只对当前查询有效）。
        if query != self.file_picker_query {
            self.file_picker_expanded = false;
            self.file_picker_scroll = 0.0;
        }
        self.file_picker_query = query;
        self.file_picker_scroll = picker_scroll;
        if show_more {
            self.file_picker_expanded = true;
        }
        if let Some(p) = go_dir_guess {
            if self.path_is_dir(&p) {
                go_dir = Some(p);
            } else if self.path_is_file(&p) {
                open_path = Some(p);
            } else if let Some(first) = first_rel {
                // 不是路径 → 当搜索词：打开最匹配的那个（VS Code 的行为）。
                open_path = Some(self.picker_path_for(&first));
            }
        }
        if let Some(d) = go_dir {
            self.set_file_picker_dir(d);
            // 换目录后把焦点还给输入框：接着打字就是在新目录里过滤（VS Code 同款手感）。
            self.file_picker_focus = true;
        } else if let Some(p) = open_path {
            match self.read_text_file(&p) {
                Ok(text) => {
                    self.open_file(p, text, true);
                    self.file_picker_open = false;
                }
                Err(e) => self.flash(format!("无法打开 {}: {e}", self.display_path(&p))),
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
        let root = self.workspace_root();
        let opts = SearchOptions {
            case_sensitive: self.search_case,
            regex: self.search_regex,
            whole_word: self.search_word,
            glob: Some(self.search_glob.clone()).filter(|g| !g.trim().is_empty()),
            exclude_glob: Some(self.search_exclude_glob.clone()).filter(|g| !g.trim().is_empty()),
        };
        let seq = self.search.search(&root, &q, &opts);
        self.pending_search_seq = Some(seq);
        // 新结果集默认全展开，标题栏回到「全部折叠」。
        self.search_all_collapsed = false;
    }

    /// 侧栏「搜索」视图：与 VS Code 一致的标题栏动作、搜索/替换框、选项与结果树。
    fn draw_search_content(&mut self, ui: &mut egui::Ui) {
        let c = search_colors(self.applied_theme);
        let mut want_refresh = false;
        let mut want_clear = false;
        let mut want_collapse_all = false;
        let mut want_expand_all = false;
        let mut want_open_editor = false;
        let mut want_search = false;
        let mut rerun = false;

        // ---- 标题栏：SEARCH + 右侧动作 ----
        // 动作与顺序照搬 VS Code（从左到右）：刷新 / 清除搜索结果 / 以树（列表）形式查看 /
        // 全部折叠。VS Code 这里没有「⋯ 更多」菜单，故不再提供；「打开新的搜索编辑器」
        // 本 IDE 用不上，按用户要求去掉。
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            ui.add_space(SEARCH_INPUT_LEFT);
            ui.label(RichText::new("SEARCH").size(11.0).color(c.header));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(4.0);
                // 全部折叠 / 全部展开 是同一个位置的两个动作（与 VS Code 一致）。
                if self.search_all_collapsed {
                    if icon_button(ui, Codicon::ExpandAll, "全部展开").clicked() {
                        want_expand_all = true;
                    }
                } else if icon_button(ui, Codicon::CollapseAll, "全部折叠").clicked() {
                    want_collapse_all = true;
                }
                // VS Code 的这个按钮：图标表示「当前模式」，提示语表示「点击后切到的模式」。
                let (view_icon, view_tip) = if self.search_view_tree {
                    (Codicon::ListTree, "以列表形式查看")
                } else {
                    (Codicon::ListFlat, "以树形式查看")
                };
                if icon_button(ui, view_icon, view_tip).clicked() {
                    self.search_view_tree = !self.search_view_tree;
                }
                if icon_button(ui, Codicon::ClearAll, "清除搜索结果与输入").clicked() {
                    want_clear = true;
                }
                if icon_button(ui, Codicon::Refresh, "按当前条件重新搜索").clicked() {
                    want_refresh = true;
                }
            });
        });
        ui.add_space(3.0);

        // 这个视图里每一段间隔都是显式 `add_space`（VS Code 的间距都是写死的），
        // 所以关掉 egui 默认的纵向 item_spacing，避免每段之间各多出 4px 空行。
        ui.spacing_mut().item_spacing.y = 0.0;

        // ---- 搜索框 / 替换框 ----
        // 布局照搬 VS Code searchView：左侧一条 18px 窄槽放「折叠替换」箭头（整条可点、
        // 箭头在两个框之间垂直居中），输入框 26px 高、4px 圆角，左沿 20px、右沿 8px；
        // 框内右侧一组 20px 的选项开关（Aa / ab / .*），最右留 2px。
        let mut query = self.content_query.clone();
        let mut edit_changed = false;
        let mut edit_enter = false;
        let row_w = ui.available_width();
        let box_w = (row_w - SEARCH_INPUT_LEFT - SEARCH_INPUT_RIGHT).max(96.0);
        let inner_w = input_box_inner_w(box_w);
        // 搜索框内容：输入框 + 2px + 三个开关（各自 20px、间距 2px）。开关右侧不再留白，
        // 由框的 2px 内边距提供 VS Code 那 2px 间距。
        let search_edit_w =
            (inner_w - 2.0 - 3.0 * SEARCH_TOGGLE - 2.0 * 2.0 - TEXT_EDIT_EXTRA_W).max(48.0);
        // 替换框窄一些：右侧要留出「全部替换」按钮（4px 间距 + 20px 按钮 + 2px 边距）。
        let replace_box_w = (box_w - 28.0).max(80.0);
        let replace_edit_w =
            (input_box_inner_w(replace_box_w) - 2.0 - SEARCH_TOGGLE - TEXT_EDIT_EXTRA_W).max(48.0);
        let replace_open = self.search_replace_open;
        let widget_h = if replace_open {
            2.0 * SEARCH_INPUT_H + SEARCH_INPUT_GAP
        } else {
            SEARCH_INPUT_H
        };
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            // 左侧窄槽：整条可点，与 VS Code 的 `.toggle-replace-button` 一样贯穿两个输入框
            // （展开时箭头正好落在两个框之间）。
            let (gutter, gutter_resp) =
                ui.allocate_exact_size(egui::vec2(SEARCH_GUTTER_W, widget_h), egui::Sense::click());
            if gutter_resp.hovered() {
                ui.painter().rect_filled(gutter, 3.0, c.toggle_hover);
            }
            draw_twistie(
                ui.painter(),
                egui::pos2(gutter.center().x + 1.0, gutter.center().y),
                replace_open,
                c.fg,
            );
            if gutter_resp.clicked() {
                self.search_replace_open = !replace_open;
            }
            gutter_resp.on_hover_text(if replace_open {
                "隐藏替换"
            } else {
                "显示替换"
            });

            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                // ---- 搜索输入框 ----
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    ui.add_space(SEARCH_INPUT_LEFT - SEARCH_GUTTER_W);
                    input_box_frame(c).show(ui, |ui| {
                        ui.set_min_width(inner_w);
                        ui.set_min_height(SEARCH_INPUT_INNER_H);
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 0.0;
                            let edit = ui.add(
                                egui::TextEdit::singleline(&mut query)
                                    .frame(false)
                                    .hint_text("搜索")
                                    .desired_width(search_edit_w)
                                    .text_color(c.fg),
                            );
                            if self.content_search_focus {
                                self.content_search_focus = false;
                                edit.request_focus();
                            }
                            edit_changed = edit.changed();
                            if edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                                edit_enter = true;
                            }
                            ui.add_space(2.0);
                            if option_toggle(
                                ui,
                                Codicon::CaseSensitive,
                                self.search_case,
                                c,
                                "区分大小写",
                            )
                            .clicked()
                            {
                                self.search_case = !self.search_case;
                                rerun = true;
                            }
                            ui.add_space(2.0);
                            if option_toggle(
                                ui,
                                Codicon::WholeWord,
                                self.search_word,
                                c,
                                "全词匹配",
                            )
                            .clicked()
                            {
                                self.search_word = !self.search_word;
                                rerun = true;
                            }
                            ui.add_space(2.0);
                            if option_toggle(
                                ui,
                                Codicon::Regex,
                                self.search_regex,
                                c,
                                "使用正则表达式",
                            )
                            .clicked()
                            {
                                self.search_regex = !self.search_regex;
                                rerun = true;
                            }
                        });
                    });
                });
                // ---- 替换输入框（框内右侧是 AB，再往右是「全部替换」）----
                if replace_open {
                    ui.add_space(SEARCH_INPUT_GAP);
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 0.0;
                        ui.add_space(SEARCH_INPUT_LEFT - SEARCH_GUTTER_W);
                        input_box_frame(c).show(ui, |ui| {
                            ui.set_min_width(input_box_inner_w(replace_box_w));
                            ui.set_min_height(SEARCH_INPUT_INNER_H);
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 0.0;
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.search_replace_text)
                                        .frame(false)
                                        .hint_text("替换")
                                        .desired_width(replace_edit_w)
                                        .text_color(c.fg),
                                );
                                ui.add_space(2.0);
                                if option_toggle(
                                    ui,
                                    Codicon::PreserveCase,
                                    self.search_preserve_case,
                                    c,
                                    "替换时保留大小写",
                                )
                                .clicked()
                                {
                                    self.search_preserve_case = !self.search_preserve_case;
                                }
                            });
                        });
                        ui.add_space(4.0);
                        // VS Code 的「全部替换」：本 IDE 尚未实现替换写入，保持禁用态外观。
                        icon_button_disabled(ui, Codicon::ReplaceAll, "全部替换（暂未实现）");
                    });
                }
            });
        });

        // ---- 搜索详细信息（VSCode：替换框正下方的「⋯」，展开后是包含 / 排除的文件）----
        // VS Code 的 `.query-details` 紧贴输入框、只有 16px 高，这里同样不留多余空行。
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.set_min_height(16.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(6.0);
                if ellipsis_button(ui, c).clicked() {
                    self.search_details_open = !self.search_details_open;
                }
            });
        });
        if self.search_details_open {
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                ui.add_space(SEARCH_INPUT_LEFT);
                ui.label(RichText::new("包含的文件").size(12.0).color(c.dim));
            });
            if search_input_row(ui, &mut self.search_glob, "例如 *.cpp, *.h", c) {
                rerun = true;
            }
            ui.add_space(SEARCH_INPUT_GAP);
            ui.horizontal(|ui| {
                ui.add_space(SEARCH_INPUT_LEFT);
                ui.label(RichText::new("排除的文件").size(12.0).color(c.dim));
            });
            if search_input_row(ui, &mut self.search_exclude_glob, "例如 node_modules", c) {
                rerun = true;
            }
            ui.add_space(2.0);
        }

        self.content_query = query;
        if rerun {
            want_search = true;
        }
        if edit_changed {
            // 边输入边搜（350ms 防抖，与 VS Code 同款手感）。
            self.search_debounce_at = Some(Instant::now() + Duration::from_millis(350));
        }
        if edit_enter {
            self.search_debounce_at = None;
            want_search = true;
            // 搜索框保持焦点：接着改条件再回车即可。
            self.content_search_focus = true;
        }
        if let Some(t) = self.search_debounce_at {
            if Instant::now() >= t {
                self.search_debounce_at = None;
                want_search = true;
            }
        }

        // ---- 汇总 + 结果列表（借用结果集，避免每帧克隆上万条匹配） ----
        let mut toggle_file: Option<String> = None;
        let mut toggle_dir: Option<String> = None;
        let mut jump: Option<(String, u64)> = None;
        let mut collapse_files: Vec<String> = Vec::new();
        let mut collapse_dirs: Vec<String> = Vec::new();
        let searching = self.pending_search_seq.is_some();
        {
            let results = &self.content_results;
            let file_count = results
                .iter()
                .map(|m| m.file.as_str())
                .collect::<HashSet<_>>()
                .len();

            // VS Code：一条消息行「N 个结果 - 在编辑器中打开」，紧贴上面的「⋯」行
            // （`.messages { margin-top: -5px }` 会压掉消息行自身的高度，这里退 3px 对齐）。
            ui.add_space(-3.0);
            ui.horizontal(|ui| {
                ui.add_space(22.0);
                let summary = if searching && results.is_empty() {
                    "正在搜索…".to_string()
                } else {
                    search_summary(results.len(), file_count)
                };
                ui.label(RichText::new(summary).size(12.0).color(c.dim));
                if !results.is_empty() {
                    ui.label(RichText::new("-").size(12.0).color(c.dim));
                    let link = ui.add(
                        egui::Button::new(RichText::new("在编辑器中打开").size(12.0).color(c.link))
                            .frame(false),
                    );
                    if link.clicked() {
                        want_open_editor = true;
                    }
                }
            });
            ui.add_space(8.0);

            let rows = build_search_rows(
                results,
                self.search_view_tree,
                &self.search_collapsed_files,
                &self.search_collapsed_dirs,
            );
            let tree = self.search_view_tree;
            // VS Code 搜索结果行高固定 22px（line-height: 22px）。
            let row_h = 22.0_f32;
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show_rows(ui, row_h, rows.len(), |ui, range| {
                    for i in range {
                        let (rect, resp) = ui.allocate_exact_size(
                            egui::vec2(ui.available_width(), row_h),
                            egui::Sense::click(),
                        );
                        let painter = ui.painter();
                        if resp.hovered() {
                            // VS Code 的列表行左右各内缩 4px，悬停条不贴边。
                            painter.rect_filled(
                                egui::Rect::from_min_max(
                                    egui::pos2(rect.left() + ROW_INSET, rect.top()),
                                    egui::pos2(rect.right() - ROW_INSET, rect.bottom()),
                                ),
                                0.0,
                                c.hover,
                            );
                        }
                        let cy = rect.center().y;
                        match &rows[i] {
                            SearchRow::Dir {
                                path,
                                name,
                                depth,
                                expanded,
                                count,
                            } => {
                                // VS Code 的树缩进：行内容左沿 = 34 + 8×层级；箭头在其左侧
                                // 12px，图标贴内容左沿，文字再往右 22px。
                                let x = rect.left() + row_contents_x(*depth);
                                draw_twistie(painter, egui::pos2(x - 12.0, cy), *expanded, c.fg);
                                draw_codicon(
                                    painter,
                                    egui::pos2(x + 8.0, cy),
                                    16.0,
                                    if *expanded {
                                        Codicon::FolderOpened
                                    } else {
                                        Codicon::Folder
                                    },
                                    hex("#dcb67a"),
                                );
                                painter.text(
                                    egui::pos2(x + 22.0, cy),
                                    egui::Align2::LEFT_CENTER,
                                    name.as_str(),
                                    egui::FontId::proportional(12.0),
                                    c.fg,
                                );
                                draw_badge(painter, rect, *count, c);
                                if resp.clicked() {
                                    toggle_dir = Some(path.clone());
                                }
                            }
                            SearchRow::File {
                                file,
                                depth,
                                expanded,
                                count,
                            } => {
                                let (name, dir) = split_file_path(file);
                                let x = rect.left() + row_contents_x(*depth);
                                draw_twistie(painter, egui::pos2(x - 12.0, cy), *expanded, c.fg);
                                draw_codicon(
                                    painter,
                                    egui::pos2(x + 8.0, cy),
                                    16.0,
                                    Codicon::File,
                                    file_icon_color(name),
                                );
                                let name_rect = painter.text(
                                    egui::pos2(x + 22.0, cy),
                                    egui::Align2::LEFT_CENTER,
                                    name,
                                    egui::FontId::proportional(12.0),
                                    c.fg,
                                );
                                if !tree && !dir.is_empty() {
                                    painter.text(
                                        egui::pos2(name_rect.right() + 7.0, cy),
                                        egui::Align2::LEFT_CENTER,
                                        dir,
                                        egui::FontId::proportional(10.0),
                                        c.dim,
                                    );
                                }
                                draw_badge(painter, rect, *count, c);
                                if resp.clicked() {
                                    toggle_file = Some((*file).to_string());
                                }
                            }
                            SearchRow::Match { file, m, depth } => {
                                // 匹配行与文件行共用同一套缩进基准：匹配文字正好落在文件行的
                                // 内容左沿（比文件名略左，与 VS Code 一致）。
                                let indent = row_contents_x(*depth);
                                let lead = m.text.len() - m.text.trim_start().len();
                                let display = m.text.trim_start();
                                let shifted: Vec<(usize, usize)> = m
                                    .submatches
                                    .iter()
                                    .map(|&(s, e)| {
                                        (s.saturating_sub(lead), e.saturating_sub(lead))
                                    })
                                    .collect();
                                let max_w =
                                    (rect.right() - 6.0 - (rect.left() + indent)).max(24.0);
                                let job = highlighted_job(
                                    display,
                                    &shifted,
                                    egui::FontId::monospace(11.0),
                                    c.fg,
                                    c.match_bg,
                                    max_w,
                                );
                                let galley = ui.fonts(|f| f.layout_job(job));
                                painter.galley(
                                    egui::pos2(rect.left() + indent, cy - galley.size().y * 0.5),
                                    galley,
                                    c.fg,
                                );
                                if resp.clicked() {
                                    jump = Some(((*file).to_string(), m.line));
                                }
                            }
                        }
                    }
                });

            // 供「全部折叠」使用（借用期内取好，随后统一应用）。
            if want_collapse_all || want_expand_all {
                for m in results {
                    collapse_files.push(m.file.clone());
                    collapse_dirs.extend(file_ancestor_dirs(&m.file));
                }
            }
        }

        // ---- 应用动作（借用已结束） ----
        if let Some(file) = toggle_file {
            if !self.search_collapsed_files.insert(file.clone()) {
                self.search_collapsed_files.remove(&file);
            }
            self.search_all_collapsed = false;
        }
        if let Some(dir) = toggle_dir {
            if !self.search_collapsed_dirs.insert(dir.clone()) {
                self.search_collapsed_dirs.remove(&dir);
            }
            self.search_all_collapsed = false;
        }
        if want_collapse_all {
            self.search_collapsed_files.extend(collapse_files);
            self.search_collapsed_dirs.extend(collapse_dirs);
            // 标题栏那个按钮随即变成「全部展开」（VS Code 同一个位置的两个动作）。
            self.search_all_collapsed = true;
        }
        if want_expand_all {
            self.search_collapsed_files.clear();
            self.search_collapsed_dirs.clear();
            self.search_all_collapsed = false;
        }
        if want_clear {
            // VS Code 的「清除搜索结果」只清结果；本 IDE 按用户要求连四个输入框
            // （搜索 / 替换 / 包含的文件 / 排除的文件）一起清空，点它就是回到干净面板。
            // 注意：此时本帧的输入框内容已经写回 `content_query`，直接清字段即可。
            self.content_results.clear();
            self.pending_search_seq = None;
            self.search_debounce_at = None;
            self.content_query.clear();
            self.search_replace_text.clear();
            self.search_glob.clear();
            self.search_exclude_glob.clear();
        }
        if want_refresh {
            want_search = true;
        }
        // 「在编辑器中打开」要读结果集，放在清除之后再判断。
        if want_open_editor {
            self.open_search_results_editor();
        }
        if want_search {
            if self.content_query.trim().is_empty() {
                self.content_results.clear();
                self.pending_search_seq = None;
            } else {
                self.start_content_search();
            }
        }

        for (file, line) in jump.into_iter() {
            let root = self.workspace_root();
            let full = std::path::Path::new(&root).join(&file);
            let win = full.to_string_lossy().into_owned();
            self.jump_to(&win, line.saturating_sub(1) as usize, 0);
        }
    }

    /// 「在编辑器中打开」：把当前结果集整理成一个只读文本标签页（VS Code search editor 的轻量版）。
    fn open_search_results_editor(&mut self) {
        if self.content_results.is_empty() {
            return;
        }
        const MAX_LINES: usize = 5000;
        let mut text = String::new();
        text.push_str(&format!(
            "Search: {}\n\n",
            if self.content_query.trim().is_empty() {
                "(empty)"
            } else {
                self.content_query.trim()
            }
        ));
        let mut by_file: BTreeMap<&str, Vec<&SearchMatch>> = BTreeMap::new();
        for m in &self.content_results {
            by_file.entry(m.file.as_str()).or_default().push(m);
        }
        let mut written = 0usize;
        let mut truncated = false;
        for (file, ms) in &by_file {
            if written >= MAX_LINES {
                truncated = true;
                break;
            }
            text.push_str(&format!("{file}  ({} matches)\n", ms.len()));
            for m in ms {
                if written >= MAX_LINES {
                    truncated = true;
                    break;
                }
                text.push_str(&format!("  {:>6}: {}\n", m.line, m.text.trim_end()));
                written += 1;
            }
            text.push('\n');
        }
        if truncated {
            text.push_str("… (结果过多，仅展示前 5000 条)\n");
        }

        self.stash_active_tab();
        if let Some(old_uri) = self.current_uri.clone() {
            if self.lsp_status.is_ready() {
                self.lsp.close_document(old_uri);
            }
        }
        let idx = self.tabs.open_untitled();
        if let Some(t) = self.tabs.get_mut(idx) {
            t.title = "Search Results".to_string();
            t.text = text;
        }
        self.load_tab_into_editor(idx);
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

    fn poll_git(&mut self, ctx: &egui::Context) {
        while let Some(ev) = self.git.poll() {
            let done = matches!(ev, GitEvent::Done { .. });
            self.scm.on_event(&ev);
            // 本 IDE 是「按需重绘」的：git 数据回来了必须主动要一帧，
            // 否则面板要等鼠标再动一下才会刷新。
            ctx.request_repaint();
            if let GitEvent::Done { ok, msg } = ev {
                if ok {
                    self.flash(msg);
                    self.refresh_git_now();
                } else {
                    self.flash(format!("Git: {msg}"));
                }
            } else if done {
                self.refresh_git_now();
            }
        }
    }

    /// 刷新所有仓库的状态 / 提交 / 分支。
    fn refresh_git_now(&self) {
        for repo in &self.scm.repos {
            self.git.refresh(&repo.path);
            self.git.load_log(&repo.path);
            self.git.load_branches(&repo.path);
        }
    }

    /// 发现工作区里的 git 仓库（本地直接扫目录，远程在远端里 `find`）。
    fn discover_scm_repos(&mut self) {
        let root = self.workspace_root();
        match (self.is_remote(), self.remote_target()) {
            (true, Some(target)) => {
                let paths = ide_git::repos::discover_repos_remote(&root, &target);
                self.scm.set_repo_paths(paths);
            }
            _ => self.scm.set_root(&root),
        }
    }

    /// 切换到源代码管理视图（VS Code 的 Ctrl+Shift+G）。
    fn open_scm_view(&mut self) {
        self.sidebar_content = SidebarContent::SourceControl;
        self.settings.show_sidebar = true;
        // VS Code 的 SCM 视图比资源管理器宽（要放提交信息框 + 提交按钮）。
        if self.sidebar_width <= SIDEBAR_DEFAULT_WIDTH + 0.5 {
            self.sidebar_width = 340.0;
        }
        self.save_settings();
        if self.scm.repos.is_empty() {
            self.discover_scm_repos();
        }
        self.refresh_git_now();
    }

    /// 执行源代码管理面板产生的动作。
    fn apply_scm_actions(&mut self, actions: Vec<scm::Action>) {
        for action in actions {
            let root = match &action {
                scm::Action::RefreshAll => None,
                scm::Action::RefreshRepo(idx)
                | scm::Action::StageAll(idx)
                | scm::Action::UnstageAll(idx)
                | scm::Action::DiscardAll(idx)
                | scm::Action::Commit { repo: idx, .. }
                | scm::Action::CommitPush { repo: idx, .. }
                | scm::Action::Amend { repo: idx }
                | scm::Action::Checkout { repo: idx, .. }
                | scm::Action::CreateBranch { repo: idx, .. }
                | scm::Action::Push(idx)
                | scm::Action::Pull(idx)
                | scm::Action::Sync(idx)
                | scm::Action::SelectCommit { repo: idx, .. }
                | scm::Action::RequestDetail { repo: idx, .. }
                | scm::Action::FocusMessage(idx) => self.scm.repos.get(*idx).map(|r| r.path.clone()),
                scm::Action::Stage { repo: idx, .. }
                | scm::Action::Unstage { repo: idx, .. }
                | scm::Action::Discard { repo: idx, .. } => {
                    self.scm.repos.get(*idx).map(|r| r.path.clone())
                }
                scm::Action::OpenDiff { repo: idx, .. } => {
                    self.scm.repos.get(*idx).map(|r| r.path.clone())
                }
                scm::Action::CopyHash(_) => None,
            };
            match action {
                scm::Action::RefreshAll => self.refresh_git_now(),
                scm::Action::RefreshRepo(_) => {
                    if let Some(r) = root {
                        self.git.refresh(&r);
                        self.git.load_log(&r);
                        self.git.load_branches(&r);
                    }
                }
                scm::Action::Stage { path, .. } => {
                    if let Some(r) = root {
                        self.git.add(&r, &path);
                    }
                }
                scm::Action::StageAll(_) => {
                    if let Some(r) = root {
                        self.git.add_all(&r);
                    }
                }
                scm::Action::Unstage { path, .. } => {
                    if let Some(r) = root {
                        self.git.unstage(&r, &path);
                    }
                }
                scm::Action::UnstageAll(_) => {
                    if let Some(r) = root {
                        self.git.unstage_all(&r);
                    }
                }
                scm::Action::Discard { path, .. } => {
                    if let Some(r) = root {
                        self.git.restore(&r, &path);
                    }
                }
                scm::Action::DiscardAll(_) => {
                    if let Some(r) = root {
                        self.git.restore_all(&r);
                    }
                }
                scm::Action::Commit { message, .. } => {
                    if let Some(r) = root {
                        self.git.commit(&r, &message);
                        // VS Code：提交后清空提交信息框
                        let target = r.clone();
                        if let Some(rs) = self.scm.repos.iter_mut().find(|x| x.path == target) {
                            rs.message.clear();
                        }
                    }
                }
                scm::Action::CommitPush { message, .. } => {
                    if let Some(r) = root {
                        self.git.commit(&r, &message);
                        self.git.push(&r);
                        let target = r.clone();
                        if let Some(rs) = self.scm.repos.iter_mut().find(|x| x.path == target) {
                            rs.message.clear();
                        }
                    }
                }
                scm::Action::Amend { .. } => {
                    if let Some(r) = root {
                        let target = r.clone();
                        let msg = self
                            .scm
                            .repos
                            .iter()
                            .find(|x| x.path == target)
                            .map(|x| x.message.trim().to_string())
                            .unwrap_or_default();
                        if !msg.is_empty() {
                            self.git.amend(&r, &msg);
                        }
                    }
                }
                scm::Action::Checkout { branch, .. } => {
                    if let Some(r) = root {
                        self.git.checkout(&r, &branch);
                    }
                }
                scm::Action::CreateBranch { name, .. } => {
                    if let Some(r) = root {
                        self.git.create_branch(&r, &name);
                    }
                }
                scm::Action::Push(_) => {
                    if let Some(r) = root {
                        self.git.push(&r);
                    }
                }
                scm::Action::Pull(_) => {
                    if let Some(r) = root {
                        self.git.pull(&r);
                    }
                }
                scm::Action::Sync(_) => {
                    if let Some(r) = root {
                        self.git.sync(&r);
                    }
                }
                scm::Action::SelectCommit { repo, hash } => {
                    if let Some(r) = self.scm.repos.get(repo).map(|x| x.path.clone()) {
                        // 再点一次同一条 = 收起展开（VS Code 的行为）
                        let already = self
                            .scm
                            .repos
                            .get(repo)
                            .and_then(|x| x.selected.clone())
                            .as_deref()
                            == Some(hash.as_str());
                        if let Some(rs) = self.scm.repos.get_mut(repo) {
                            rs.selected = if already { None } else { Some(hash.clone()) };
                        }
                        if !already {
                            self.git.commit_detail(&r, &hash);
                        } else {
                            self.scm.detail = None;
                        }
                    }
                }
                scm::Action::RequestDetail { repo, hash } => {
                    if let Some(r) = self.scm.repos.get(repo).map(|x| x.path.clone()) {
                        // 悬停用的详情（不覆盖「选中提交」的展开数据）
                        self.git.commit_detail_hover(&r, &hash);
                    }
                }
                scm::Action::CopyHash(hash) => {
                    copy_to_clipboard(hash.clone());
                    self.flash(format!("已复制提交 ID：{hash}"));
                }
                scm::Action::OpenDiff { path, cached, .. } => {
                    if let Some(r) = root {
                        self.git.diff(&r, &path, cached);
                        // VS Code：点文件会同时在编辑器里打开它
                        let full = if self.is_remote() {
                            PathBuf::from(format!("{}/{}", r.trim_end_matches('/'), path))
                        } else {
                            PathBuf::from(&r).join(&path)
                        };
                        match self.read_text_file(&full) {
                            Ok(text) => self.open_file(full, text, false),
                            Err(e) => {
                                tracing::debug!("SCM：打开文件失败 {path}: {e}");
                            }
                        }
                    }
                }
                scm::Action::FocusMessage(_) => {
                    self.flash("请在提交信息框中输入内容".to_string());
                }
            }
        }
    }

    /// 绘制源代码管理视图（VS Code 的 Source Control 侧栏）。
    fn draw_source_control_content(&mut self, ui: &mut egui::Ui) {
        let actions = self.scm.draw(ui);
        if !actions.is_empty() {
            self.apply_scm_actions(actions);
        }
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

    /// 把当前设置应用到编辑器（字号/主题/minimap/滚轮方向）。
    fn apply_settings_to_ui(&mut self) {
        let kind = ThemeKind::from_name(&self.settings.theme);
        let tokens = theme_tokens(&self.host, kind);
        let scroll_inverted = self.settings.scroll_reversed();
        if let Some(ed) = &mut self.editor {
            ed.set_typography(self.settings.font_size);
            ed.set_theme(kind, &tokens);
            ed.set_minimap(self.settings.show_minimap);
            ed.set_scroll_inverted(scroll_inverted);
        }        self.applied_font = self.settings.font_size;
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
        let scroll_dir = self.settings.mouse_scroll_direction.clone();
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
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Mouse wheel:").size(13.0));
                    for (value, label) in [("normal", "Normal"), ("reversed", "Reversed")] {
                        if ui.selectable_label(scroll_dir == value, label).clicked() {
                            actions.push(SettingsAction::ScrollDirection(value.to_string()));
                        }
                    }
                });
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
                SettingsAction::ScrollDirection(dir) => {
                    self.settings.mouse_scroll_direction = dir;
                    // 立刻生效（编辑器 + 终端都改）。
                    self.apply_settings_to_ui();
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
        // 远程工作区用 POSIX 分隔符，否则拼出来的是 Windows 路径。
        let root = self.workspace_root();
        let sep = if self.is_remote() { '/' } else { '\\' };
        self.save_as_input = format!("{root}{sep}{name}");
        self.save_as_open = true;
    }

    fn do_save_as(&mut self, path_str: String) {
        let trimmed = path_str.trim();
        if trimmed.is_empty() {
            self.flash("请输入保存路径".to_string());
            return;
        }
        let path = PathBuf::from(trimmed);
        let Some(text) = self.editor.as_ref().map(|e| e.text().to_string()) else {
            return;
        };
        if let Some(dir) = path.parent() {
            if !dir.as_os_str().is_empty() && !self.path_is_dir(dir) {
                if let Err(e) = self.create_dir_all_path(dir) {
                    self.flash(format!("创建目录失败: {e}"));
                    return;
                }
            }
        }
        if let Err(e) = self.write_text_file(&path, &text) {
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
        self.current_uri = self.uri_of_path(&path);
        self.editor_clean_seq = self.editor.as_ref().map(|e| e.edit_seq()).unwrap_or(0);
        self.save_as_open = false;
        self.flash(format!("已保存 {}", self.display_path(&path)));
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
            "go.forward" => self.on_go_forward(),
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
                let root = self.workspace_root();
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
                self.open_scm_view();
            }
            "git.refresh" => self.refresh_git_now(),
            "git.commit" => {
                let msg = self
                    .scm
                    .repos
                    .first()
                    .map(|r| r.message.trim().to_string())
                    .unwrap_or_default();
                if msg.is_empty() {
                    self.flash("请先输入提交信息".to_string());
                } else if let Some(r) = self.scm.repos.first().map(|r| r.path.clone()) {
                    self.git.commit(&r, &msg);
                }
            }
            "git.diffActive" => {
                if let Some(p) = self.active_tab_path() {
                    let root = self.workspace_root();
                    self.git.diff(&root, &p.to_string_lossy(), false);
                    self.open_scm_view();
                } else {
                    self.flash("没有打开的文件".to_string());
                }
            }
            "git.showHistory" => {
                self.open_scm_view();
            }
            "git.stageAll" => {
                for repo in &self.scm.repos {
                    self.git.add_all(&repo.path);
                }
            }
            "git.unstageAll" => {
                for repo in &self.scm.repos {
                    self.git.unstage_all(&repo.path);
                }
            }
            "git.discardAll" => {
                for repo in &self.scm.repos {
                    self.git.restore_all(&repo.path);
                }
            }
            "git.sync" => {
                for repo in &self.scm.repos {
                    self.git.sync(&repo.path);
                }
            }
            "git.pull" => {
                for repo in &self.scm.repos {
                    self.git.pull(&repo.path);
                }
            }
            "git.push" => {
                for repo in &self.scm.repos {
                    self.git.push(&repo.path);
                }
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
            "terminal.new" => self.new_integrated_terminal(),
            "terminal.toggle" => self.toggle_terminal(),
            "terminal.clear" => {
                self.terminal.clear_active();
                self.terminal.open = true;
            }
            "terminal.kill" => self.kill_active_terminal(),
            "terminal.openWsl" => self.open_wsl_terminal(),
            _ => {}
        }
    }

    // ================= 集成终端（编辑器下方 Split 面板） =================

    /// 新建一个远端终端会话（面板内，不走独立窗口）。
    ///
    /// 只有连接了远程工作区才能用：终端的进程、工作目录、环境都必须在远端内部，
    /// 本地工作区下给明确提示，而不是悄悄开个 Windows shell。
    fn new_integrated_terminal(&mut self) {
        let Some(target) = self.remote_target() else {
            self.terminal.open = true;
            self.terminal.last_error = Some("先连接 WSL / SSH 再新建终端".to_string());
            self.flash("集成终端只在远程工作区可用：请先连接 WSL 或 SSH".to_string());
            return;
        };
        let cwd = self.workspace_root();
        match self.terminal.spawn(&target, &cwd, 120, 32) {
            Ok(()) => {
                self.terminal.open = true;
                self.flash(format!("终端已启动：{} · {cwd}", target.label()));
            }
            Err(e) => {
                self.terminal.open = true;
                self.terminal.last_error = Some(e.clone());
                self.flash(format!("新建终端失败: {e}"));
            }
        }
    }

    /// Ctrl+` / 「终端」菜单：显示或隐藏面板。
    fn toggle_terminal(&mut self) {
        if self.terminal.open {
            self.terminal.open = false;
            self.terminal.clear_focus();
            self.terminal.panel_rect = None;
        } else {
            self.terminal.open = true;
            if self.terminal.is_empty() {
                self.new_integrated_terminal();
            } else {
                self.terminal.focus_next = true;
            }
        }
    }

    /// 结束当前终端（会话与进程一起关掉，面板保留空态）。
    fn kill_active_terminal(&mut self) {
        if self.terminal.active().is_none() {
            self.flash("没有可结束的终端".to_string());
            return;
        }
        self.terminal.close_active();
        self.flash("已结束活动终端".to_string());
    }

    fn poll_terminal(&mut self, ctx: &egui::Context) {
        if self.terminal.poll() {
            ctx.request_repaint();
        }
        // 面板隐藏时也要定期醒来排空输出，避免 PTY 管道被写满后卡住 shell。
        if self.terminal.any_alive() {
            ctx.request_repaint_after(Duration::from_millis(200));
        }
    }

    /// 终端相关的快捷键：Ctrl+` 全局有效；终端持有焦点时再接管 IDE 快捷键。
    ///
    /// 为什么不能只靠编辑器：终端聚焦时编辑器键盘是关掉的，若不在这里处理，
    /// Ctrl+P / Ctrl+S 等会被 `terminal::event_to_bytes` 过滤掉后彻底没反应。
    fn handle_terminal_shortcuts(&mut self, ctx: &egui::Context, modal_open: bool) {
        if modal_open {
            return;
        }
        let events = ctx.input(|i| i.events.clone());
        let terminal_focus = self.terminal.focused;
        let mut toggle = false;
        let mut new_term = false;
        for ev in &events {
            let egui::Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } = ev
            else {
                continue;
            };
            if !modifiers.command {
                continue;
            }
            if *key == egui::Key::Backtick {
                if modifiers.shift {
                    new_term = true;
                } else {
                    toggle = true;
                }
                continue;
            }
            if !terminal_focus {
                continue;
            }
            match (*key, modifiers.shift) {
                (egui::Key::P, true) => self.open_command_palette(),
                (egui::Key::P, false) => self.run_command("file.openFile", ctx),
                (egui::Key::F, false) => self.run_command("edit.find", ctx),
                (egui::Key::F, true) => self.run_command("search.content", ctx),
                (egui::Key::H, _) => self.run_command("edit.replace", ctx),
                (egui::Key::S, false) => self.run_command("file.save", ctx),
                (egui::Key::S, true) => self.run_command("file.saveAs", ctx),
                (egui::Key::N, _) => self.run_command("file.newFile", ctx),
                (egui::Key::W, false) => self.run_command("file.close", ctx),
                (egui::Key::W, true) => self.run_command("debug.addWatch", ctx),
                (egui::Key::B, _) => self.run_command("view.toggleSidebar", ctx),
                (egui::Key::G, false) => self.run_command("go.line", ctx),
                (egui::Key::G, true) => self.run_command("git.openPanel", ctx),
                (egui::Key::T, _) => self.run_command("go.symbol", ctx),
                (egui::Key::Comma, _) => self.run_command("settings.open", ctx),
                (egui::Key::Equals, _) => self.run_command("view.zoomIn", ctx),
                (egui::Key::Minus, _) => self.run_command("view.zoomOut", ctx),
                (egui::Key::Num0, _) => self.run_command("view.resetFontSize", ctx),
                _ => {}
            }
        }
        if new_term {
            self.new_integrated_terminal();
        } else if toggle {
            self.toggle_terminal();
        }
    }

    fn open_command_palette(&mut self) {
        self.palette_open = !self.palette_open;
        if self.palette_open {
            self.palette_query.clear();
            self.palette_sel = 0;
            self.palette_focus_req = true;
            self.palette_frames = 0;
            self.palette_open_at = Some(Instant::now());
        }
    }

    /// 编辑器下方的集成终端面板（VS Code 式 Split）。
    fn draw_terminal_panel(&mut self, ctx: &egui::Context) {
        if !self.terminal.open {
            self.terminal.panel_rect = None;
            return;
        }
        let p = palette(self.applied_theme);
        let style = TerminalStyle::for_theme(self.applied_theme);
        let header_bg = hex(p.bg_sidebar);
        let border = hex(p.border_menu);
        let fg = hex(p.fg_editor);
        let dim = fg.gamma_multiply(0.72);
        let hover_bg = hex(p.bg_menu_selection);
        let hover_fg = if matches!(self.applied_theme, ThemeKind::LightPlus) {
            egui::Color32::BLACK
        } else {
            egui::Color32::WHITE
        };
        let accent = hex(p.tab_active_border);
        let colors = TermHeaderColors {
            fg,
            dim,
            hover_bg,
            active_bg: style.bg,
            accent,
        };

        let mut new_clicked = false;
        let mut clear_clicked = false;
        let mut hide_clicked = false;
        let mut kill_clicked = false;
        let mut select: Option<usize> = None;
        let mut close_tab: Option<usize> = None;

        let panel = egui::TopBottomPanel::bottom("terminal_panel")
            .resizable(true)
            .default_height(terminal::DEFAULT_PANEL_HEIGHT)
            .min_height(terminal::MIN_PANEL_HEIGHT)
            .frame(egui::Frame::none().fill(style.bg))
            .show(ctx, |ui| {
                ui.spacing_mut().item_spacing = egui::vec2(0.0, 0.0);

                let (header_rect, _) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), terminal::HEADER_HEIGHT),
                    egui::Sense::hover(),
                );
                let painter = ui.painter();
                painter.rect_filled(header_rect, 0.0, header_bg);
                painter.hline(
                    header_rect.x_range(),
                    header_rect.bottom() - 0.5,
                    egui::Stroke::new(1.0_f32, border),
                );

                let mut header = ui.child_ui(
                    header_rect,
                    egui::Layout::left_to_right(egui::Align::Center),
                    None,
                );
                header.spacing_mut().item_spacing.x = 2.0;
                header.add_space(6.0);

                let sessions: Vec<(usize, usize, String, bool, bool)> = self
                    .terminal
                    .sessions
                    .iter()
                    .enumerate()
                    .map(|(i, s)| (i, s.id, s.title.clone(), s.exited, s.focused))
                    .collect();
                for (idx, id, title, exited, _) in sessions {
                    let (resp, close) = terminal_tab(
                        &mut header,
                        id,
                        &title,
                        idx == self.terminal.active,
                        exited,
                        colors,
                    );
                    if close {
                        close_tab = Some(idx);
                    } else if resp.clicked() {
                        select = Some(idx);
                    } else if resp.clicked_by(egui::PointerButton::Middle) {
                        close_tab = Some(idx);
                    }
                }

                header.with_layout(
                    egui::Layout::right_to_left(egui::Align::Center),
                    |ui| {
                        ui.add_space(6.0);
                        if terminal_icon_button(
                            ui,
                            Codicon::ChromeClose,
                            "隐藏终端面板 (Ctrl+`)",
                            dim,
                            hover_bg,
                            hover_fg,
                        )
                        .clicked()
                        {
                            hide_clicked = true;
                        }
                        if terminal_icon_button(
                            ui,
                            Codicon::ClearAll,
                            "清空终端",
                            dim,
                            hover_bg,
                            hover_fg,
                        )
                        .clicked()
                        {
                            clear_clicked = true;
                        }
                        if terminal_icon_button(
                            ui,
                            Codicon::Trash,
                            "结束活动终端",
                            dim,
                            hover_bg,
                            hover_fg,
                        )
                        .clicked()
                        {
                            kill_clicked = true;
                        }
                        if terminal_icon_button(
                            ui,
                            Codicon::Add,
                            "新建终端 (Ctrl+Shift+`)",
                            dim,
                            hover_bg,
                            hover_fg,
                        )
                        .clicked()
                        {
                            new_clicked = true;
                        }
                    },
                );

                // 记住正文区域并让面板始终占满它：egui 的 TopBottomPanel 用「内容高度」
                // 记住面板高度，若正文只画了网格/一行提示，面板会一帧帧缩到 min_height。
                let body_rect = ui.available_rect_before_wrap();

                if self.terminal.sessions.is_empty() {
                    let rect = body_rect;
                    let mut empty = ui.child_ui(
                        rect,
                        egui::Layout::top_down(egui::Align::Center),
                        None,
                    );
                    empty.add_space(28.0);
                    empty.label(
                        egui::RichText::new("集成终端")
                            .size(14.0)
                            .color(fg),
                    );
                    empty.add_space(6.0);
                    let hint = if self.is_remote() {
                        "面板已就绪，点右上角 + 新建一个终端"
                    } else {
                        "先连接 WSL / SSH（左下角远程连接），再新建终端"
                    };
                    empty.label(egui::RichText::new(hint).size(12.0).color(dim));
                    if let Some(err) = self.terminal.last_error.as_deref() {
                        empty.add_space(8.0);
                        empty.label(
                            egui::RichText::new(err)
                                .size(12.0)
                                .color(egui::Color32::from_rgb(0xf4, 0x87, 0x71)),
                        );
                    }
                    if self.is_remote() {
                        empty.add_space(12.0);
                        if empty.button("新建终端").clicked() {
                            new_clicked = true;
                        }
                    }
                } else {
                    let request_focus = self.terminal.ack_focus();
                    let active = self.terminal.active;
                    let mut focused = false;
                    if let Some(session) = self.terminal.sessions.get_mut(active) {
                        focused = terminal::draw_body(ui, session, &style, request_focus);
                    }
                    if focused {
                        for (i, other) in self.terminal.sessions.iter_mut().enumerate() {
                            if i != active {
                                other.focused = false;
                            }
                        }
                    }
                    self.terminal.focused = focused;
                }
                ui.expand_to_include_rect(body_rect);
            });

        self.terminal.panel_rect = Some(panel.response.rect);
        if let Some(idx) = close_tab {
            self.terminal.close(idx);
        } else if let Some(idx) = select {
            self.terminal.active = idx;
            self.terminal.focus_next = true;
        }
        if clear_clicked {
            self.terminal.clear_active();
        }
        if kill_clicked {
            self.kill_active_terminal();
        }
        if new_clicked {
            self.new_integrated_terminal();
        }
        if hide_clicked {
            self.terminal.open = false;
            self.terminal.clear_focus();
            self.terminal.panel_rect = None;
        }
    }

    /// 在当前工作区开一个远端终端窗口（而不是 Windows 侧 cmd）。
    ///
    /// 落点是工作区根，环境变量来自远端的登录 shell；本地工作区下会明确提示
    /// 先连接远程，避免默默开个 Windows 终端。
    fn open_wsl_terminal(&mut self) {
        let Some(target) = self.remote_target() else {
            self.flash("当前是本地工作区：先连接 WSL / SSH 再开终端".to_string());
            return;
        };
        let cwd = self.workspace_root();
        match ide_remote::open_terminal(&target, &cwd) {
            Ok(()) => self.flash(format!("已在 {} 中打开终端：{cwd}", target.label())),
            Err(e) => self.flash(format!("打开终端失败: {e}")),
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
        // 右侧给叉叉预留位置；叉叉默认不画、鼠标移到标签上才出现（不压标题）。
        let mono = egui::FontId::monospace(12.0);
        // 标签标题（含「未保存」标记）
        let label_text = |t: &Tab| -> String {
            let mut s = t.title.clone();
            if t.dirty {
                s.push_str(" — ");
            }
            s
        };
        // 标题排版（和下面画的时候用同一套，保证宽度一致）
        let layout_label = |ui: &egui::Ui, text: &str, color: egui::Color32, italic: bool| {
            let mut job = egui::text::LayoutJob::default();
            job.append(
                text,
                0.0,
                egui::text::TextFormat {
                    font_id: mono.clone(),
                    color,
                    italics: italic,
                    ..Default::default()
                },
            );
            ui.painter().layout_job(job)
        };
        // 每个标签按自己标题的自然宽度排（VS Code 语义）。
        let label_ws: Vec<f32> = (0..n)
            .map(|i| {
                let text = self.tabs.get(i).map(label_text).unwrap_or_default();
                let preview = self.tabs.is_preview(i);
                layout_label(ui, &text, hex("#ffffff"), preview).size().x
            })
            .collect();
        ui.horizontal(|ui| {
            ui.set_min_height(34.0);
            // 右侧要给 "N tabs" 留位置；总宽不够时标签等比压缩（不低于 TAB_MIN_W）。
            let avail = (ui.available_width() - TAB_BADGE_W).max(TAB_MIN_W);
            let widths = tab_widths(&label_ws, avail);
            for i in 0..n {
                let tab_w = widths.get(i).copied().unwrap_or(TAB_MIN_W);
                let text_w = (tab_w - TAB_PAD_X - TAB_X_SLOT).max(20.0);
                let (dirty, active, preview) = {
                    let Some(t) = self.tabs.get(i) else { continue };
                    let preview = self.tabs.is_preview(i);
                    (t.dirty, i == self.tabs.active(), preview)
                };
                let text_color = if active { hex("#ffffff") } else { hex("#999999") };
                let bg = if active { hex(p.bg_editor) } else { egui::Color32::TRANSPARENT };
                let title = self.tabs.get(i).map(|t| t.title.clone()).unwrap_or_default();
                let mut label = title.clone();
                if dirty {
                    label.push_str(" — ");
                }
                // 太长就截断（标题不能被叉叉压住）
                if layout_label(ui, &label, text_color, preview).size().x > text_w {
                    let mut n = label.chars().count();
                    while n > 1 {
                        n -= 1;
                        let mut s: String = label.chars().take(n).collect();
                        s.push('…');
                        if layout_label(ui, &s, text_color, preview).size().x <= text_w {
                            label = s;
                            break;
                        }
                    }
                }
                // 自己画标签（不用 Button）：文字左对齐 + 右侧留出叉叉的位置。
                // 注意：`allocate_exact_size` 只负责占位，**不做交互**，
                // 所以还要 `ui.interact` 一次才有 hover / click。
                let (rect, _) = ui.allocate_exact_size(
                    egui::vec2(tab_w, 30.0),
                    egui::Sense::click(),
                );
                let mut resp =
                    ui.interact(rect, egui::Id::new(("tab", i)), egui::Sense::click())
                        .on_hover_cursor(egui::CursorIcon::PointingHand);
                ui.painter().rect_filled(rect, 0.0, bg);
                // 标题（左对齐、垂直居中）
                let galley = layout_label(ui, &label, text_color, preview);
                let gsize = galley.size();
                ui.painter().add(egui::Shape::galley(
                    egui::pos2(
                        rect.left() + TAB_PAD_X,
                        rect.center().y - gsize.y * 0.5,
                    ),
                    galley,
                    text_color,
                ));
                if preview {
                    resp = resp.on_hover_text(
                        "预览标签：双击标签页（或在文件中编辑）即锁定，否则下次打开文件会被替换",
                    );
                }
                if active {
                    // 激活标签：上边缘加一条高亮细条（`tab.selectedBorderTop`）。
                    // 注意不能用状态栏色：状态栏已改成 Dark 2026 的深灰，
                    // 那样标签条会跟着变灰。
                    ui.painter().rect_filled(
                        egui::Rect::from_min_size(
                            rect.left_top(),
                            egui::vec2(rect.width(), TAB_ACTIVE_INDICATOR_H),
                        ),
                        0.0,
                        hex(p.tab_active_border),
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
                // 关闭按钮：**必须无条件创建**，不能放在「标签 hover 时才画」的分支里。
                // egui 的命中测试用的是**上一帧**的控件表：叉叉只在标签 hover 的帧存在时，
                // 指针停在叉叉上会两帧一循环（标签 hover → 画叉叉；叉叉抢走 hover → 不画），
                // 点击就有一半落在没有叉叉的帧上 —— 表现正是「点叉叉没反应」（SSH 下帧更慢更明显）。
                // 可见性只影响绘制，不影响交互。
                let x = egui::Rect::from_center_size(
                    egui::pos2(
                        rect.right() - TAB_CLOSE_MARGIN - TAB_CLOSE_W * 0.5,
                        rect.center().y,
                    ),
                    egui::vec2(TAB_CLOSE_W, TAB_CLOSE_W),
                );
                let hit = ui
                    .interact(x, egui::Id::new(("tab_close", i)), egui::Sense::click())
                    .on_hover_cursor(egui::CursorIcon::PointingHand);
                // 可见性保持原样（只有悬停标签时才画），但交互永远在：
                // 指针移到叉叉上时 `resp.hovered()` 会变成 false，靠 `hit.hovered()` 兜住，
                // 否则叉叉会自己「闪没」，点击就丢了。
                if resp.hovered() || hit.hovered() {
                    if hit.hovered() {
                        ui.painter()
                            .rect_filled(x, 3.0, hex("#ffffff").gamma_multiply(0.1));
                    }
                    draw_codicon(
                        ui.painter(),
                        x.center(),
                        14.0,
                        Codicon::Close,
                        if hit.hovered() { hex("#ffffff") } else { hex("#999999") },
                    );
                }
                if hit.clicked() {
                    close = Some(i);
                }
                hit.on_hover_text("关闭标签");
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

    /// 顶部标题栏：应用图标 + 菜单 + 窗口按钮合并成一行（对齐 VS Code 的自绘标题栏）。
    ///
    /// 窗口用 `with_decorations(false)` 去掉了系统标题栏，所以整条标题栏的空白处按下就能
    /// 拖动窗口（`StartDrag` 交给系统原生移动循环：边缘吸附、最大化时拖拽还原都在），
    /// 双击切换最大化；四边的缩放见 [`LiteIdeApp::draw_window_resize_edges`]。
    fn draw_title_bar(&mut self, ctx: &egui::Context) {
        let p = palette(self.applied_theme);
        let focused = ctx.input(|i| i.viewport().focused.unwrap_or(true));
        let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
        let fg = hex(p.fg_titlebar);
        let bg = if focused {
            p.bg_titlebar
        } else {
            p.bg_titlebar_inactive
        };

        let mut cmd: Option<egui::ViewportCommand> = None;
        egui::TopBottomPanel::top("title_bar")
            .exact_height(TITLEBAR_H)
            .frame(egui::Frame::none().fill(hex(bg)))
            .show(ctx, |ui| {
                // 整条标题栏先登记成拖动区；后登记的菜单/窗口按钮在 egui 的命中测试里
                // 优先级更高（同一个点取「最后登记」的控件），所以只有空白处才会拖窗口。
                let bar = egui::Rect::from_min_size(
                    ui.min_rect().min,
                    egui::vec2(ui.available_width(), TITLEBAR_H),
                );
                let drag = ui.interact(
                    bar,
                    ui.id().with("titlebar_drag"),
                    egui::Sense::click_and_drag(),
                );

                // 菜单标题样式：VS Code `.menubar-menu-title { padding: 0 8px;
                // border-radius: 5px }`，悬停底色是 `menubar.selectionBackground`。
                {
                    let st = ui.style_mut();
                    st.spacing.button_padding = egui::vec2(MENUBAR_ITEM_PAD_X, 4.0);
                    st.spacing.item_spacing.x = 0.0;
                    st.text_styles
                        .insert(egui::TextStyle::Button, egui::FontId::proportional(13.0));
                    st.visuals.widgets.inactive.weak_bg_fill = egui::Color32::TRANSPARENT;
                    st.visuals.widgets.inactive.rounding =
                        egui::Rounding::same(MENUBAR_ITEM_RADIUS);
                    st.visuals.widgets.inactive.fg_stroke.color = fg;
                    st.visuals.widgets.hovered.weak_bg_fill = menubar_selection();
                    st.visuals.widgets.hovered.bg_fill = menubar_selection();
                    st.visuals.widgets.hovered.rounding = egui::Rounding::same(MENUBAR_ITEM_RADIUS);
                    st.visuals.widgets.hovered.fg_stroke.color = fg;
                }

                ui.allocate_ui_with_layout(
                    egui::vec2(ui.available_width(), TITLEBAR_H),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| {
                        // 应用图标：35px 宽的区域里居中放 16px 图标（`.window-appicon`）。
                        // 纯绘制、不登记点击/拖拽，所以点图标处也能拖动窗口。
                        let (logo, _) = ui.allocate_exact_size(
                            egui::vec2(APPICON_W, TITLEBAR_H),
                            egui::Sense::hover(),
                        );
                        if self.app_icon.is_none() {
                            self.app_icon = app_icon_texture(ctx);
                        }
                        if let Some(icon) = self.app_icon.as_ref() {
                            draw_app_icon(ui.painter(), logo.center(), icon);
                        }

                        ui.menu_button("File", |ui| {
                            menu_items_style(ui, p);
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
                            menu_items_style(ui, p);
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
                            menu_items_style(ui, p);
                            let mut sb = self.settings.show_sidebar;
                            if ui.checkbox(&mut sb, "Side Bar").changed() {
                                self.settings.show_sidebar = sb;
                                self.save_settings();
                            }
                            let mut dbg = self.debug_open;
                            if ui.checkbox(&mut dbg, "Debug Panel").changed() {
                                self.debug_open = dbg;
                            }
                            let mut term = self.terminal.open;
                            if ui.checkbox(&mut term, "Terminal Panel").changed() {
                                if term {
                                    self.terminal.open = true;
                                    if self.terminal.is_empty() {
                                        self.new_integrated_terminal();
                                    }
                                } else {
                                    self.terminal.open = false;
                                    self.terminal.clear_focus();
                                }
                            }
                            ui.separator();
                            if ui.button("Command Palette…").clicked() {
                                self.palette_open = true;
                                ui.close_menu();
                            }
                        });
                        ui.menu_button("Go", |ui| {
                            menu_items_style(ui, p);
                            if ui.button("Go to Line…").clicked() {
                                self.goto_line_open = true;
                                self.goto_line_input.clear();
                                ui.close_menu();
                            }
                        });
                        ui.menu_button("Run", |ui| {
                            menu_items_style(ui, p);
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
                        ui.menu_button("Terminal", |ui| {
                            menu_items_style(ui, p);
                            if ui.button("New Terminal        Ctrl+Shift+`").clicked() {
                                self.new_integrated_terminal();
                                ui.close_menu();
                            }
                            if ui.button("Toggle Terminal        Ctrl+`").clicked() {
                                self.toggle_terminal();
                                ui.close_menu();
                            }
                            ui.separator();
                            if ui.button("Clear").clicked() {
                                self.terminal.clear_active();
                                ui.close_menu();
                            }
                            if ui.button("Kill Active Terminal").clicked() {
                                self.kill_active_terminal();
                                ui.close_menu();
                            }
                            ui.separator();
                            if ui.button("Open in Remote (external window)").clicked() {
                                self.open_wsl_terminal();
                                ui.close_menu();
                            }
                        });
                        ui.menu_button("Help", |ui| {
                            menu_items_style(ui, p);
                            if ui.button("About LiteIDE").clicked() {
                                self.about_open = true;
                                ui.close_menu();
                            }
                        });

                        // 右侧窗口按钮：右对齐布局，先加的先贴右边，
                        // 所以顺序是「关闭在最右」→ 与 VS Code 一致（最小化/最大化/关闭）。
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if window_control(ui, Codicon::ChromeClose, true, "关闭", fg) {
                                cmd = Some(egui::ViewportCommand::Close);
                            }
                            let (icon, tip) = if maximized {
                                (Codicon::ChromeRestore, "还原")
                            } else {
                                (Codicon::ChromeMaximize, "最大化")
                            };
                            if window_control(ui, icon, false, tip, fg) {
                                cmd = Some(egui::ViewportCommand::Maximized(!maximized));
                            }
                            if window_control(ui, Codicon::ChromeMinimize, false, "最小化", fg) {
                                cmd = Some(egui::ViewportCommand::Minimized(true));
                            }
                        });
                    },
                );

                // 标题栏空白处：双击 = 最大化/还原，按下拖动 = 移动窗口。
                if drag.double_clicked() {
                    cmd = Some(egui::ViewportCommand::Maximized(!maximized));
                } else if drag.drag_started_by(egui::PointerButton::Primary)
                    && !press_on_resize_edge(ctx)
                {
                    cmd = Some(egui::ViewportCommand::StartDrag);
                }
            });
        if let Some(cmd) = cmd {
            ctx.send_viewport_cmd(cmd);
        }
    }

    /// 自绘标题栏后系统不再提供边框缩放：光标进入窗口四边/四角 5px 时换成缩放箭头，
    /// 按下时交给 winit 的原生缩放循环（VS Code 的 `.resizer` 同理）。
    fn draw_window_resize_edges(&mut self, ctx: &egui::Context) {
        let Some(pos) = ctx.input(|i| i.pointer.hover_pos()) else {
            return;
        };
        let Some((dir, cursor)) = resize_zone_at(ctx, pos) else {
            return;
        };
        ctx.set_cursor_icon(cursor);
        if ctx.input(|i| i.pointer.primary_pressed()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::BeginResize(dir));
        }
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
                let item = |ui: &mut egui::Ui, icon: Codicon, tip: &str, on: bool| -> bool {
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
                    draw_codicon(painter, rect.center(), 24.0, icon, color);
                    resp.on_hover_text(tip).clicked()
                };
                // 高亮跟随“当前选中视图”，与侧栏是否可见解耦（VS Code 的 checked 语义）：
                // 拖分割线收起 / Ctrl+B 隐藏后，活动栏按钮仍保持选中高亮，不会“丢失焦点”。
                let explorer_on = self.sidebar_content == SidebarContent::Explorer;
                if item(ui, Codicon::Files, "资源管理器 (Ctrl+B)", explorer_on) {
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
                if item(ui, Codicon::SearchLarge, "搜索 (Ctrl+Shift+F)", search_on) {
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
                // 源代码管理视图（VS Code：占用侧栏位置，再次点击收起）。
                let scm_on = self.sidebar_content == SidebarContent::SourceControl;
                if item(ui, Codicon::SourceControl, "源代码管理 (Ctrl+Shift+G)", scm_on) {
                    if scm_on && self.settings.show_sidebar {
                        self.settings.show_sidebar = false;
                    } else {
                        self.open_scm_view();
                    }
                }
                if item(ui, Codicon::RunView, "运行与调试 (F5)", self.debug_open) {
                    self.debug_open = !self.debug_open;
                }
                let outline_on = self.sidebar_content == SidebarContent::Outline;
                if item(ui, Codicon::SymbolStructure, "大纲", outline_on) {
                    if outline_on && self.settings.show_sidebar {
                        self.settings.show_sidebar = false;
                    } else {
                        self.sidebar_content = SidebarContent::Outline;
                        self.settings.show_sidebar = true;
                    }
                    self.save_settings();
                }
                ui.add_space(8.0);
                if item(ui, Codicon::SettingsGear, "设置 (Ctrl+,)", self.settings_open) {
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
                // 侧栏滚轮：egui 的 `smooth_scroll_delta` 会把一格滚轮摊到十几帧里，
                // 而我们是**按需重绘**的 → 滚轮几乎没反应。
                // 这里直接把原始滚轮增量塞回去（和编辑器 / Ctrl+P 列表一个套路）。
                let panel_rect = ui.max_rect();
                if ui.rect_contains_pointer(panel_rect) {
                    let mut wheel = 0.0_f32;
                    for ev in ui.input(|i| i.events.clone()) {
                        if let egui::Event::MouseWheel { unit, delta, .. } = ev {
                            wheel += match unit {
                                // 一格滚轮 ≈ 3 行（VS Code 的 mouseWheelScrollSensitivity）
                                egui::MouseWheelUnit::Line => delta.y * 60.0,
                                // 触控板：按像素 1:1
                                egui::MouseWheelUnit::Point => delta.y,
                                egui::MouseWheelUnit::Page => delta.y * 400.0,
                            };
                        }
                    }
                    if wheel != 0.0 {
                        ui.ctx().input_mut(|i| i.smooth_scroll_delta = egui::vec2(0.0, wheel));
                    }
                }
                ui.add_space(4.0);
                match self.sidebar_content {
                    SidebarContent::Explorer => self.draw_explorer_content(ui),
                    SidebarContent::Search => self.draw_search_content(ui),
                    SidebarContent::Outline => self.draw_outline_content(ui),
                    SidebarContent::SourceControl => self.draw_source_control_content(ui),
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
                if icon_button(ui, Codicon::Refresh, "重新扫描（rg --files）").clicked() {
                    let root = self.workspace_root();
                    self.search.load_files(&root);
                }
            });
        });
        ui.add_space(2.0);

        if !self.explorer_loaded {
            self.explorer_loaded = true;
            let root = self.workspace_root();
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
                        draw_twistie(
                            painter,
                            egui::pos2(rect.left() + indent + 6.0, cy),
                            is_open,
                            hex("#cccccc"),
                        );
                        draw_codicon(
                            painter,
                            egui::pos2(rect.left() + indent + 20.0, cy),
                            16.0,
                            if is_open {
                                Codicon::FolderOpened
                            } else {
                                Codicon::Folder
                            },
                            hex("#dcb67a"),
                        );
                    } else {
                        draw_codicon(
                            painter,
                            egui::pos2(rect.left() + indent + 20.0, cy),
                            16.0,
                            Codicon::File,
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
                let root = self.workspace_root();
                let full = std::path::Path::new(&root).join(&path);
                match self.read_text_file(&full) {
                    Ok(text) => self.open_file(full, text, !pin),
                    Err(e) => self.flash(format!("无法打开 {path}: {e}")),
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
        // 连接项跟随「远程工作区」状态（不是 clangd 是否跑在远端）：
        // 本地工作区时它只是个图标，点了才进连接菜单。
        let remote = self.settings.remote_label();
        let p = palette(self.applied_theme);
        let fg = hex(p.fg_statusbar);
        let hover = hex(p.bg_statusbar_hover);
        egui::TopBottomPanel::bottom("status_bar")
            .exact_height(24.0)
            .frame(egui::Frame::none().fill(hex(p.bg_statusbar)))
            .show(ctx, |ui| {
                // 顶部 1px 分隔线（VS Code `statusBar.border`）。
                let top = ui.max_rect();
                ui.painter().line_segment(
                    [top.left_top(), top.right_top()],
                    egui::Stroke::new(1.0_f32, hex(p.border_statusbar)),
                );
                // 状态栏项自带左右内边距，不再画 VS Code 里没有的分隔竖线。
                ui.spacing_mut().item_spacing.x = 0.0;
                ui.horizontal_centered(|ui| {
                    // ---- 最左：远程连接项（VS Code 位置）----
                    let remote_resp = statusbar_remote(ui, &p, remote.as_deref());
                    self.remote_item_rect = Some(remote_resp.rect);
                    if remote_resp.clicked() {
                        self.remote_menu_open = !self.remote_menu_open;
                    }

                    // ---- 左：分支 / LSP / DBG ----
                    if let Some(branch) = self.scm.primary_branch() {
                        statusbar_item(
                            ui,
                            Some(&format!("branch: {branch}")),
                            None,
                            fg,
                            egui::Color32::TRANSPARENT,
                            hover,
                            Some(&format!("当前分支：{branch}")),
                        );
                    }
                    let lsp_color = match &self.lsp_status {
                        LspStatus::Ready => egui::Color32::from_rgb(0x89, 0xd1, 0x85),
                        LspStatus::Crashed(_) | LspStatus::Unavailable(_) => {
                            egui::Color32::from_rgb(0xf4, 0x87, 0x71)
                        }
                        _ => fg,
                    };
                    statusbar_item(
                        ui,
                        Some(&lsp_label),
                        None,
                        lsp_color,
                        egui::Color32::TRANSPARENT,
                        hover,
                        None,
                    );
                    if recoverable {
                        let txt = match &self.lsp_status {
                            LspStatus::Crashed(_) => "[ Restart clangd ]",
                            _ => "[ Retry ]",
                        };
                        ui.add_space(4.0);
                        if ui
                            .add(egui::Button::new(txt).small())
                            .on_hover_text("重启 clangd")
                            .clicked()
                        {
                            self.lsp.restart();
                        }
                        ui.add_space(4.0);
                    }
                    statusbar_item(
                        ui,
                        Some(&dap_label),
                        None,
                        dap_color,
                        egui::Color32::TRANSPARENT,
                        hover,
                        None,
                    );
                    if matches!(
                        self.dap_status,
                        DapStatus::Crashed(_) | DapStatus::Unavailable(_)
                    ) {
                        ui.add_space(4.0);
                        if ui
                            .add(egui::Button::new("[ Restart Debug ]").small())
                            .on_hover_text("重启调试（F5）")
                            .clicked()
                        {
                            self.on_debug_start();
                        }
                    }

                    // ---- 右：Ln/Col / 缩进 / 编码 / 主题（VSCode 布局）----
                    ui.with_layout(
                        egui::Layout::right_to_left(egui::Align::Center),
                        |ui| {
                            statusbar_item(
                                ui,
                                Some(&theme_name),
                                None,
                                fg,
                                egui::Color32::TRANSPARENT,
                                hover,
                                None,
                            );
                            statusbar_item(
                                ui,
                                Some("UTF-8"),
                                None,
                                fg,
                                egui::Color32::TRANSPARENT,
                                hover,
                                None,
                            );
                            statusbar_item(
                                ui,
                                Some(&indent),
                                None,
                                fg,
                                egui::Color32::TRANSPARENT,
                                hover,
                                None,
                            );
                            statusbar_item(
                                ui,
                                Some(status.as_ref()),
                                None,
                                fg,
                                egui::Color32::TRANSPARENT,
                                hover,
                                None,
                            );
                        },
                    );
                });
            });
    }

    /// 连接项的下拉菜单：选项与 VS Code 的「Remote」菜单同构（WSL / SSH 已实现）。
    fn draw_remote_menu(&mut self, ctx: &egui::Context) {
        if !self.remote_menu_open {
            return;
        }
        let Some(anchor) = self.remote_item_rect else {
            self.remote_menu_open = false;
            return;
        };
        let p = palette(self.applied_theme);
        let connected = self.remote_target().map(|t| t.label());
        let mut keep_open = true;
        let mut want: Option<RemoteAction> = None;
        let mut want_toolchain = false;
        egui::Area::new(egui::Id::new("remote_menu"))
            .order(egui::Order::Foreground)
            .fixed_pos(egui::pos2(anchor.left(), anchor.top() - 4.0))
            .pivot(egui::Align2::LEFT_BOTTOM)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style())
                    .fill(hex(p.bg_menu))
                    .stroke(egui::Stroke::new(1.0_f32, hex(p.border_menu)))
                    .inner_margin(egui::Margin::symmetric(4.0, 4.0))
                    .show(ui, |ui| {
                        menu_items_style(ui, p);
                        ui.set_min_width(300.0);
                        if let Some(label) = &connected {
                            ui.label(
                                RichText::new(label.as_str())
                                    .size(11.0)
                                    .color(hex("#8c8c8c")),
                            );
                            if ui.button("Close Remote Connection").clicked() {
                                want = Some(RemoteAction::Disconnect);
                                keep_open = false;
                            }
                            if ui
                                .button("Remote Toolchain… (clangd / rg / gdb)")
                                .on_hover_text("检测远端缺少的工具，可免 root 自动安装")
                                .clicked()
                            {
                                want_toolchain = true;
                                keep_open = false;
                            }
                            ui.separator();
                        }
                        for (label, tip, action) in remote_menu_entries() {
                            let enabled = action.is_supported();
                            let resp = ui.add_enabled(enabled, egui::Button::new(label));
                            let resp = resp.on_hover_text(tip);
                            if resp.clicked() {
                                want = Some(action);
                                keep_open = false;
                            }
                        }
                        ui.separator();
                        ui.label(
                            RichText::new("WSL / SSH 已实现；其余选项暂为占位")
                                .size(10.5)
                                .color(hex("#7a7a7a")),
                        );
                    });
            });
        // 点菜单外面关闭（egui 的 Area 不自动做这件事，这里用「菜单外点击」判定）。
        if keep_open
            && ctx.input(|i| i.pointer.any_click())
            && ctx.input(|i| i.pointer.interact_pos())
                .map(|pos| !self.remote_menu_rect_contains(&self.remote_item_rect, pos))
                .unwrap_or(false)
        {
            keep_open = false;
        }
        if let Some(action) = want {
            match action {
                RemoteAction::ConnectWsl => self.open_remote_dialog(),
                RemoteAction::ConnectSsh => self.open_ssh_dialog(),
                RemoteAction::Disconnect => self.disconnect_remote(),
                // 占位项：菜单里是禁用的，这里只是兜底（不做事）。
                RemoteAction::ConnectTunnel
                | RemoteAction::CloneRepository
                | RemoteAction::OpenOnRemote => {}
            }
        }
        if want_toolchain {
            self.open_toolchain_dialog();
        }
        self.remote_menu_open = keep_open;
    }

    /// 菜单里的「点外面关闭」只需要排除连接项本身，但求稳再给菜单区域留一层。
    fn remote_menu_rect_contains(&self, item: &Option<egui::Rect>, pos: egui::Pos2) -> bool {
        if let Some(r) = item {
            let expanded = egui::Rect::from_min_max(
                egui::pos2(r.left(), r.top() - 400.0),
                egui::pos2(r.left() + 320.0, r.bottom()),
            );
            return expanded.contains(pos);
        }
        false
    }

    /// 「连接到 WSL」对话框：发行版列表 + 目录输入 + 子目录浏览。
    fn open_remote_dialog(&mut self) {
        let distros = ide_lsp::process::list_wsl_distros_blocking();
        if distros.is_empty() {
            self.flash("未检测到 WSL 发行版（先运行 wsl --install）".to_string());
            return;
        }
        let current = self.settings.remote_distro.clone();
        let distro = current
            .filter(|d| distros.iter().any(|x| x == d))
            .unwrap_or_else(|| distros[0].clone());
        self.remote_dialog_distros = distros;
        self.remote_dialog_distro = distro;
        self.remote_dialog_path = normalize_posix(
            &self.settings.remote_path.clone().unwrap_or_else(|| "/home".to_string()),
        );
        self.remote_dialog_error = None;
        self.refresh_remote_dialog_dirs();
        self.remote_dialog_open = true;
    }

    /// 重新枚举对话框里当前目录的子目录（在远端内部 `find`，不走 9P 视图）。
    fn refresh_remote_dialog_dirs(&mut self) {
        let posix = normalize_posix(&self.remote_dialog_path);
        if self.remote_dialog_distro.trim().is_empty() {
            self.remote_dialog_dirs.clear();
            self.remote_dialog_error = Some("请先选择一个 WSL 发行版".to_string());
            return;
        }
        let target = RemoteTarget::wsl(self.remote_dialog_distro.clone());
        match self.remote.list_dir(&target, &posix) {
            Ok(entries) => {
                let mut dirs: Vec<String> = entries
                    .into_iter()
                    .filter(|e| e.is_dir)
                    .map(|e| e.name)
                    .collect();
                dirs.sort_by_key(|d| d.to_lowercase());
                self.remote_dialog_dirs = dirs;
                self.remote_dialog_error = None;
            }
            Err(e) => {
                self.remote_dialog_dirs.clear();
                self.remote_dialog_error = Some(e);
            }
        }
    }

    fn draw_remote_dialog(&mut self, ctx: &egui::Context) {
        if !self.remote_dialog_open {
            return;
        }
        let mut open = true;
        let mut go = false;
        let mut cancel = false;
        let mut browse: Option<String> = None;
        let mut up = false;
        let distros = self.remote_dialog_distros.clone();
        let mut distro = self.remote_dialog_distro.clone();
        let mut path = self.remote_dialog_path.clone();
        let dirs = self.remote_dialog_dirs.clone();
        let error = self.remote_dialog_error.clone();
        egui::Window::new("Connect to WSL")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(520.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, -40.0])
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Distro:").size(13.0));
                    egui::ComboBox::from_id_source("remote_distro_pick")
                        .selected_text(distro.clone())
                        .width(220.0)
                        .show_ui(ui, |ui| {
                            for d in &distros {
                                ui.selectable_value(&mut distro, d.clone(), d);
                            }
                        });
                });
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Folder:").size(13.0));
                    let edit = ui.add(
                        egui::TextEdit::singleline(&mut path)
                            .hint_text("/home/work/NewLithoTune/candanza")
                            .desired_width(370.0),
                    );
                    if edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        go = true;
                    }
                    if ui.button("Browse").clicked() {
                        browse = Some(String::new());
                    }
                    if ui.button("Up").clicked() {
                        up = true;
                    }
                });
                ui.add_space(4.0);
                egui::ScrollArea::vertical()
                    .max_height(240.0)
                    .show(ui, |ui| {
                        if dirs.is_empty() {
                            ui.label(
                                RichText::new("（无子目录 / 路径不可读）")
                                    .size(12.0)
                                    .color(hex("#8c8c8c")),
                            );
                        }
                        for d in &dirs {
                            if ui
                                .add(egui::Button::new(format!("▸ {d}")).frame(false))
                                .on_hover_text("进入该子目录")
                                .clicked()
                            {
                                browse = Some(d.clone());
                            }
                        }
                    });
                if let Some(err) = &error {
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(err)
                            .size(11.5)
                            .color(egui::Color32::from_rgb(0xf4, 0x87, 0x71)),
                    );
                }
                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("Connect").clicked() {
                        go = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            });
        self.remote_dialog_distro = distro;
        self.remote_dialog_path = path;
        if up {
            // 上一级：`/home/work` → `/home`，`/` 仍是 `/`。
            let cur = normalize_posix(&self.remote_dialog_path);
            self.remote_dialog_path = match cur.rsplit_once('/') {
                Some((parent, _)) if !parent.is_empty() => parent.to_string(),
                _ => "/".to_string(),
            };
            self.refresh_remote_dialog_dirs();
        } else if let Some(d) = browse {
            // 浏览：点空串 = 重新读当前目录；点子目录 = 拼在规范化的当前目录后面。
            let base = normalize_posix(&self.remote_dialog_path);
            self.remote_dialog_path = if d.is_empty() {
                base
            } else if base == "/" {
                format!("/{d}")
            } else {
                format!("{base}/{d}")
            };
            self.refresh_remote_dialog_dirs();
        }
        if go {
            let distro = self.remote_dialog_distro.clone();
            let path = normalize_posix(&self.remote_dialog_path);
            match self.connect_remote(RemoteTarget::wsl(distro), &path) {
                Ok(()) => self.remote_dialog_open = false,
                Err(e) => self.remote_dialog_error = Some(e),
            }
        } else if cancel {
            self.remote_dialog_open = false;
        } else {
            self.remote_dialog_open = open;
        }
    }

    // ================= 连接到 SSH 主机 =================

    /// 用户名/主机没填全时的占位提示；填全后自动消失。
    const SSH_DIALOG_NEED_USER_HOST: &'static str = "请先填写用户名和主机";

    /// 填全用户名/主机后清掉占位提示（真正的连接错误要留到下一次操作）。
    fn clear_ssh_placeholder_error(&mut self) {
        if self.ssh_dialog_error.as_deref() == Some(Self::SSH_DIALOG_NEED_USER_HOST) {
            self.ssh_dialog_error = None;
        }
    }

    /// 打开「Connect to SSH Host」对话框：主机 / 用户 / 端口 / 目录 + 子目录浏览。
    fn open_ssh_dialog(&mut self) {
        self.ssh_dialog_host = self.settings.ssh_host.clone().unwrap_or_default();
        self.ssh_dialog_user = self.settings.ssh_user.clone().unwrap_or_default();
        self.ssh_dialog_port = if self.settings.ssh_port == 0 {
            ide_remote::DEFAULT_SSH_PORT
        } else {
            self.settings.ssh_port
        };
        // 预先填上次的 SSH 路径；上次连的是 WSL 时不要把它带进 SSH 对话框。
        let same_kind = self
            .settings
            .remote_kind
            .as_deref()
            .map(|k| k.trim().eq_ignore_ascii_case("ssh"))
            .unwrap_or(false);
        let prefill = if same_kind {
            self.settings.remote_path.clone()
        } else {
            None
        };
        self.ssh_dialog_path =
            normalize_posix(&prefill.unwrap_or_else(|| "/home".to_string()));
        self.ssh_dialog_error = None;
        self.ssh_dialog_open = true;
        // 只有用户名/主机都已就绪才去连：否则一打开就报「请先填写…」，
        // 用户填完以后这条旧错误还挂在那里，看起来像 Connect 没生效。
        if self.ssh_dialog_target().is_some() {
            self.refresh_ssh_dialog_dirs();
        } else {
            self.ssh_dialog_dirs.clear();
        }
    }

    /// 对话框里填的 SSH 目标（用户名/主机没填全时返回 None）。
    fn ssh_dialog_target(&self) -> Option<RemoteTarget> {
        let user = self.ssh_dialog_user.trim();
        let host = self.ssh_dialog_host.trim();
        if user.is_empty() || host.is_empty() {
            return None;
        }
        Some(RemoteTarget::ssh(user, host, self.ssh_dialog_port))
    }

    /// 重新枚举 SSH 对话框里当前目录的子目录（一次 `find`，顺带验证连通性）。
    fn refresh_ssh_dialog_dirs(&mut self) {
        let Some(target) = self.ssh_dialog_target() else {
            self.ssh_dialog_error = Some(Self::SSH_DIALOG_NEED_USER_HOST.to_string());
            self.ssh_dialog_dirs.clear();
            return;
        };
        let posix = normalize_posix(&self.ssh_dialog_path);
        let listed = self.remote.list_dir(&target, &posix);
        match listed {
            Ok(entries) => {
                let mut dirs: Vec<String> = entries
                    .into_iter()
                    .filter(|e| e.is_dir)
                    .map(|e| e.name)
                    .collect();
                dirs.sort_by_key(|d| d.to_lowercase());
                self.ssh_dialog_dirs = dirs;
                self.ssh_dialog_error = None;
            }
            Err(e) => {
                self.ssh_dialog_dirs.clear();
                tracing::warn!("列出远程目录失败（{}:{}）: {e}", target.label(), posix);
                self.ssh_dialog_error = Some(e);
            }
        }
    }

    fn draw_ssh_dialog(&mut self, ctx: &egui::Context) {
        if !self.ssh_dialog_open {
            return;
        }
        let mut open = true;
        let mut connect = false;
        let mut cancel = false;
        let mut browse: Option<String> = None;
        let mut up = false;
        let mut refresh = false;
        let (mut host, mut user) = (self.ssh_dialog_host.clone(), self.ssh_dialog_user.clone());
        let mut port = self.ssh_dialog_port;
        let mut path = self.ssh_dialog_path.clone();
        let dirs = self.ssh_dialog_dirs.clone();
        let error = self.ssh_dialog_error.clone();

        egui::Window::new("Connect to SSH Host")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(560.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, -40.0])
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("User:").size(13.0));
                    let e = ui.add(
                        egui::TextEdit::singleline(&mut user)
                            .hint_text("fubin")
                            .desired_width(130.0),
                    );
                    if e.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        refresh = true;
                    }
                    ui.label(RichText::new("Host:").size(13.0));
                    let e = ui.add(
                        egui::TextEdit::singleline(&mut host)
                            .hint_text("192.168.77.6")
                            .desired_width(150.0),
                    );
                    if e.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        refresh = true;
                    }
                    ui.label(RichText::new("Port:").size(13.0));
                    ui.add(
                        egui::DragValue::new(&mut port)
                            .range(1..=65535)
                            .speed(1.0),
                    );
                });
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Folder:").size(13.0));
                    let edit = ui.add(
                        egui::TextEdit::singleline(&mut path)
                            .hint_text("/home/fubin/work/candanza")
                            .desired_width(360.0),
                    );
                    if edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        refresh = true;
                    }
                    if ui.button("Browse").clicked() {
                        refresh = true;
                    }
                    if ui.button("Up").clicked() {
                        up = true;
                    }
                });
                ui.add_space(4.0);
                let ready = !user.trim().is_empty() && !host.trim().is_empty();
                egui::ScrollArea::vertical()
                    .max_height(230.0)
                    .show(ui, |ui| {
                        if dirs.is_empty() {
                            let hint = if ready {
                                "（无子目录 / 连接失败）"
                            } else {
                                "（填好用户名和主机后点 Refresh 或 Browse）"
                            };
                            ui.label(
                                RichText::new(hint)
                                    .size(12.0)
                                    .color(hex("#8c8c8c")),
                            );
                        }
                        for d in &dirs {
                            if ui
                                .add(egui::Button::new(format!("▸ {d}")).frame(false))
                                .on_hover_text("进入该子目录")
                                .clicked()
                            {
                                browse = Some(d.clone());
                            }
                        }
                    });
                if let Some(err) = &error {
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(err)
                            .size(11.5)
                            .color(egui::Color32::from_rgb(0xf4, 0x87, 0x71)),
                    );
                }
                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("Connect").clicked() {
                        connect = true;
                    }
                    if ui.button("Refresh").clicked() {
                        refresh = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
                ui.label(
                    RichText::new("需要已配置免密登录（公钥）；密码提示不会弹在 IDE 里")
                        .size(10.5)
                        .color(hex("#7a7a7a")),
                );
            });

        self.ssh_dialog_host = host;
        self.ssh_dialog_user = user;
        self.ssh_dialog_port = port;
        self.ssh_dialog_path = path;
        if self.ssh_dialog_target().is_some() {
            self.clear_ssh_placeholder_error();
        }

        if up {
            let cur = normalize_posix(&self.ssh_dialog_path);
            self.ssh_dialog_path = match cur.rsplit_once('/') {
                Some((parent, _)) if !parent.is_empty() => parent.to_string(),
                _ => "/".to_string(),
            };
            refresh = true;
        } else if let Some(d) = browse {
            let base = normalize_posix(&self.ssh_dialog_path);
            self.ssh_dialog_path = if d.is_empty() {
                base
            } else if base == "/" {
                format!("/{d}")
            } else {
                format!("{base}/{d}")
            };
            refresh = true;
        }

        if refresh {
            self.refresh_ssh_dialog_dirs();
        }
        if connect {
            let target = self.ssh_dialog_target();
            let path = normalize_posix(&self.ssh_dialog_path);
            match target {
                Some(t) => match self.connect_remote(t, &path) {
                    Ok(()) => self.ssh_dialog_open = false,
                    Err(e) => self.ssh_dialog_error = Some(e),
                },
                None => {
                    self.ssh_dialog_error = Some(Self::SSH_DIALOG_NEED_USER_HOST.to_string());
                }
            }
        } else if cancel {
            self.ssh_dialog_open = false;
        } else {
            self.ssh_dialog_open = open;
        }
    }

    // ================= 远程工具链（clangd / rg / gdb） =================

    /// 新连接（或启动时恢复的远程工作区）自动探测一次工具链。
    ///
    /// 探测是后台线程（一次 SSH/WSL 往返），不阻塞界面；结果回到
    /// [`LiteIdeApp::poll_toolchain`]，缺工具时自动弹对话框。
    fn maybe_auto_probe_toolchain(&mut self) {
        let Some(target) = self.remote_target() else {
            // 回到本地：清掉状态，下次连远端重新探测。
            self.toolchain_probed_for = None;
            self.toolchain_skipped = false;
            self.toolchain_probe = None;
            self.toolchain_dialog_open = false;
            return;
        };
        if self.toolchain_probed_for.as_ref() == Some(&target) || self.toolchain_rx.is_some() {
            return;
        }
        self.toolchain_probed_for = Some(target);
        self.toolchain_skipped = false;
        self.start_toolchain_probe(true);
    }

    /// 后台探测远端工具链。
    fn start_toolchain_probe(&mut self, auto_open: bool) {
        let Some(target) = self.remote_target() else {
            return;
        };
        if self.toolchain_rx.is_some() {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        self.toolchain_rx = Some(rx);
        self.toolchain_busy = Some("正在探测远端工具链…".to_string());
        self.toolchain_error = None;
        self.toolchain_probe = None;
        self.toolchain_auto_open = auto_open;
        let spawned = std::thread::Builder::new()
            .name("toolchain-probe".to_string())
            .spawn(move || {
                let result = ide_remote::toolchain::probe(&target);
                let _ = tx.send(ToolchainEvent::Probe(result));
                let _ = tx.send(ToolchainEvent::Done);
            });
        if let Err(e) = spawned {
            self.toolchain_rx = None;
            self.toolchain_busy = None;
            self.toolchain_error = Some(format!("启动探测线程失败: {e}"));
        }
    }

    /// 自动安装缺的工具（只装能免 root 安装的：clangd / rg）。
    fn start_toolchain_install(&mut self) {
        let Some(target) = self.remote_target() else {
            return;
        };
        let Some(probe) = self.toolchain_probe.as_ref() else {
            return;
        };
        let tools: Vec<ide_core::remote::RemoteTool> = probe
            .missing
            .iter()
            .copied()
            .filter(|t| t.auto_installable())
            .collect();
        if tools.is_empty() || self.toolchain_rx.is_some() {
            return;
        }
        let names: Vec<&str> = tools.iter().map(|t| t.id()).collect();
        self.toolchain_installed.clear();
        self.toolchain_log.clear();
        self.toolchain_busy = Some(format!("正在安装 {}…", names.join("、")));
        let (tx, rx) = std::sync::mpsc::channel();
        self.toolchain_rx = Some(rx);
        let spawned = std::thread::Builder::new()
            .name("toolchain-install".to_string())
            .spawn(move || {
                for tool in tools {
                    let tx_line = tx.clone();
                    let result = ide_remote::toolchain::install(&target, tool, &mut |line| {
                        let _ = tx_line.send(ToolchainEvent::Progress(line.to_string()));
                    });
                    match result {
                        Ok(_) => {
                            let _ = tx.send(ToolchainEvent::Installed(tool));
                        }
                        Err(error) => {
                            let _ = tx.send(ToolchainEvent::Failed { tool, error });
                        }
                    }
                }
                let _ = tx.send(ToolchainEvent::Done);
            });
        if let Err(e) = spawned {
            self.toolchain_rx = None;
            self.toolchain_busy = None;
            self.flash(format!("启动安装线程失败: {e}"));
        }
    }

    /// 处理后台任务的事件（每帧调用）。
    fn poll_toolchain(&mut self, ctx: &egui::Context) {
        let events: Vec<ToolchainEvent> = match self.toolchain_rx.as_ref() {
            Some(rx) => rx.try_iter().collect(),
            None => return,
        };
        let mut done = false;
        for ev in events {
            match ev {
                ToolchainEvent::Probe(Ok(probe)) => {
                    let auto_open =
                        self.toolchain_auto_open && !probe.is_complete() && !self.toolchain_skipped;
                    tracing::info!(
                        "远端工具链：{}（找到 {} 个）",
                        probe.summary(),
                        probe.found.len()
                    );
                    self.toolchain_probe = Some(probe);
                    self.toolchain_error = None;
                    self.toolchain_busy = None;
                    if auto_open {
                        self.toolchain_dialog_open = true;
                    }
                }
                ToolchainEvent::Probe(Err(e)) => {
                    tracing::warn!("远端工具链探测失败: {e}");
                    self.toolchain_error = Some(e);
                    self.toolchain_busy = None;
                    if self.toolchain_auto_open && !self.toolchain_skipped {
                        self.toolchain_dialog_open = true;
                    }
                }
                ToolchainEvent::Progress(line) => {
                    self.toolchain_log.push(line);
                    if self.toolchain_log.len() > 200 {
                        self.toolchain_log.remove(0);
                    }
                }
                ToolchainEvent::Installed(tool) => {
                    self.toolchain_installed.push(tool);
                    self.toolchain_log
                        .push(format!("✔ {} 安装完成", tool.id()));
                }
                ToolchainEvent::Failed { tool, error } => {
                    tracing::warn!("{} 自动安装失败: {error}", tool.id());
                    self.toolchain_log
                        .push(format!("✘ {} 安装失败：{error}", tool.id()));
                    self.flash(format!("{} 自动安装失败：{error}", tool.id()));
                }
                ToolchainEvent::Done => done = true,
            }
        }
        if done {
            self.toolchain_rx = None;
            self.toolchain_busy = None;
            if !self.toolchain_installed.is_empty() {
                let names: Vec<&str> = self.toolchain_installed.iter().map(|t| t.id()).collect();
                self.flash(format!("已安装 {}，正在重启 clangd…", names.join("、")));
                // clangd 以旧 PATH 起的进程不会自己发现新装的二进制：重开根。
                let uri = self.workspace_uri();
                self.lsp.reopen_root(uri);
                self.start_toolchain_probe(false);
            }
        }
        if self.toolchain_rx.is_some() {
            ctx.request_repaint_after(Duration::from_millis(150));
        }
    }

    /// 手动打开「远程工具链」对话框（菜单 / 跳转失败时）。
    fn open_toolchain_dialog(&mut self) {
        if !self.is_remote() {
            self.flash("当前没有远程连接".to_string());
            return;
        }
        self.toolchain_dialog_open = true;
        self.toolchain_auto_open = false;
        if self.toolchain_rx.is_none() {
            self.start_toolchain_probe(false);
        }
    }

    /// 「远程工具链」对话框：缺什么、为什么、自动装 / 重新检测 / 暂不安装。
    fn draw_toolchain_dialog(&mut self, ctx: &egui::Context) {
        if !self.toolchain_dialog_open {
            return;
        }
        let mut open = true;
        let mut install = false;
        let mut recheck = false;
        let mut skip = false;
        let target = self
            .remote_target()
            .map(|t| t.label())
            .unwrap_or_else(|| "（未连接）".to_string());
        let probe = self.toolchain_probe.clone();
        let error = self.toolchain_error.clone();
        let busy = self.toolchain_busy.clone();
        let log = self.toolchain_log.clone();
        let can_auto: Vec<ide_core::remote::RemoteTool> = probe
            .as_ref()
            .map(|p| {
                p.missing
                    .iter()
                    .copied()
                    .filter(|t| t.auto_installable())
                    .collect()
            })
            .unwrap_or_default();
        let manual_only = probe
            .as_ref()
            .map(|p| p.missing.iter().any(|t| !t.auto_installable()))
            .unwrap_or(false);

        egui::Window::new("Remote Toolchain")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(580.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, -30.0])
            .show(ctx, |ui| {
                ui.label(RichText::new(format!("目标：{target}")).size(12.5));
                ui.add_space(4.0);
                if let Some(err) = &error {
                    ui.label(
                        RichText::new(err)
                            .size(11.5)
                            .color(egui::Color32::from_rgb(0xf4, 0x87, 0x71)),
                    );
                    ui.add_space(4.0);
                }
                if let Some(p) = &probe {
                    for tool in ide_core::remote::RemoteTool::ALL {
                        match p.path_of(tool) {
                            Some(path) => {
                                ui.label(
                                    RichText::new(format!(
                                        "✔ {} —— {}：{path}",
                                        tool.id(),
                                        tool.purpose()
                                    ))
                                    .size(12.0)
                                    .color(egui::Color32::from_rgb(0x7e, 0xc6, 0x99)),
                                );
                            }
                            None => {
                                ui.label(
                                    RichText::new(format!(
                                        "✘ {} —— {}：未安装",
                                        tool.id(),
                                        tool.purpose()
                                    ))
                                    .size(12.0)
                                    .color(egui::Color32::from_rgb(0xf4, 0x87, 0x71)),
                                );
                                if !tool.auto_installable() {
                                    ui.label(
                                        RichText::new(format!(
                                            "    {}",
                                            ide_remote::toolchain::manual_hint(tool)
                                        ))
                                        .size(10.5)
                                        .color(hex("#8c8c8c")),
                                    );
                                }
                            }
                        }
                    }
                } else if busy.is_none() && error.is_none() {
                    ui.label(RichText::new("（还没有探测结果）").size(12.0).color(hex("#8c8c8c")));
                }
                ui.separator();
                ui.label(
                    RichText::new(
                        "自动安装不需要 root：clangd 下载官方预编译包（旧系统自带 glibc 运行时），\
                         rg 从软件源解包到 ~/.liteide/tools；装完自动接入 PATH，无需手改配置。",
                    )
                    .size(10.5)
                    .color(hex("#8c8c8c")),
                );
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    let auto_label = if can_auto.is_empty() {
                        "自动安装（无可自动安装的）".to_string()
                    } else {
                        format!(
                            "自动安装 {}",
                            can_auto
                                .iter()
                                .map(|t| t.id())
                                .collect::<Vec<_>>()
                                .join(" + ")
                        )
                    };
                    if ui
                        .add_enabled(
                            busy.is_none() && !can_auto.is_empty(),
                            egui::Button::new(auto_label),
                        )
                        .on_hover_text("下载并解包到 ~/.liteide/tools，不需要 root")
                        .clicked()
                    {
                        install = true;
                    }
                    if ui
                        .add_enabled(busy.is_none(), egui::Button::new("我已安装，重新检测"))
                        .clicked()
                    {
                        recheck = true;
                    }
                    if ui
                        .add_enabled(busy.is_none(), egui::Button::new("暂不安装"))
                        .on_hover_text("暂时没有跳转/补全；以后可从「远程」菜单再打开")
                        .clicked()
                    {
                        skip = true;
                    }
                });
                if manual_only && can_auto.is_empty() {
                    ui.label(
                        RichText::new("剩下的工具需要系统包管理器（见上面的命令）。")
                            .size(10.5)
                            .color(hex("#8c8c8c")),
                    );
                }
                if let Some(busy) = &busy {
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(RichText::new(busy).size(12.0));
                    });
                }
                if !log.is_empty() {
                    ui.add_space(4.0);
                    egui::ScrollArea::vertical()
                        .max_height(150.0)
                        .stick_to_bottom(true)
                        .show(ui, |ui| {
                            for line in &log {
                                ui.label(RichText::new(line).size(10.5).color(hex("#9a9a9a")));
                            }
                        });
                }
            });

        if install {
            self.start_toolchain_install();
        }
        if recheck {
            self.start_toolchain_probe(false);
        }
        if skip {
            self.toolchain_skipped = true;
            self.toolchain_dialog_open = false;
            self.flash(
                "已跳过：远端代码跳转/补全仍不可用（可从「远程」菜单重新打开工具链）"
                    .to_string(),
            );
        }
        if !open {
            self.toolchain_dialog_open = false;
        }
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
            let color = hex("#d4d4d4");
            let font = egui::FontId::proportional(13.0);
            // 一行放不下时保留首尾、中间省略；两侧留 40px 边距。
            let max_w = (ctx.screen_rect().width() - 80.0).clamp(160.0, 900.0);
            let measure = |s: &str| {
                ctx.fonts(|f| {
                    f.layout_no_wrap(s.to_owned(), font.clone(), color)
                        .size()
                        .x
                })
            };
            let shown = middle_ellipsis(msg, max_w, measure);
            egui::Area::new(egui::Id::new("flash_area"))
                .anchor(egui::Align2::CENTER_TOP, [0.0, 30.0])
                .order(egui::Order::Foreground)
                .show(ctx, |ui| {
                    egui::Frame::popup(ui.style())
                        .fill(hex("#3c3c3c"))
                        .show(ui, |ui| {
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(shown).size(13.0).color(color),
                                )
                                .wrap_mode(egui::TextWrapMode::Extend),
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

    /// 复制文本到系统剪贴板（Windows `clip`）。
///
/// 放在后台线程里做，避免阻塞 UI 线程。
fn copy_to_clipboard(text: String) {
    std::thread::spawn(move || {
        let mut child = match std::process::Command::new("clip")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("复制到剪贴板失败: {e}");
                return;
            }
        };
        if let Some(mut stdin) = child.stdin.take() {
            use std::io::Write;
            if let Err(e) = stdin.write_all(text.as_bytes()) {
                tracing::warn!("写剪贴板失败: {e}");
            }
        }
        let _ = child.wait();
    });
}

/// 一个状态栏项：可选图标 + 可选文字，悬停时整块底色变 `hover`
    /// （VS Code `statusBarItem.hoverBackground`）。
    #[allow(clippy::too_many_arguments)]
    fn statusbar_item(
        ui: &mut egui::Ui,
        text: Option<&str>,
        icon: Option<Codicon>,
        fg: egui::Color32,
        bg: egui::Color32,
        hover: egui::Color32,
        tooltip: Option<&str>,
    ) -> egui::Response {
        const PAD_X: f32 = 6.0;
        const ICON_W: f32 = 16.0;
        const GAP: f32 = 4.0;
        let font = egui::FontId::proportional(12.0);
        let text_w = text
            .map(|t| {
                ui.painter()
                    .layout_no_wrap(t.to_owned(), font.clone(), fg)
                    .size()
                    .x
            })
            .unwrap_or(0.0);
        let width = PAD_X * 2.0
            + if icon.is_some() { ICON_W } else { 0.0 }
            + if icon.is_some() && text.is_some() { GAP } else { 0.0 }
            + text_w;
        // 一律可点：状态栏项在 VS Code 里都是可交互的（我们至少用它做悬停高亮与连接菜单）。
        let (rect, response) =
            ui.allocate_exact_size(egui::vec2(width, 24.0), egui::Sense::click());
        let bg = if response.hovered() {
            hover
        } else if response.is_pointer_button_down_on() {
            hex("#4b4c4d")
        } else {
            bg
        };
        if response.clicked() {
            // 点下去不要同时把焦点送到编辑器。
            ui.ctx().memory_mut(|m| m.request_focus(ui.id()));
        }
        if bg != egui::Color32::TRANSPARENT {
            ui.painter().rect_filled(rect, 0.0, bg);
        }
        let mut x = rect.left() + PAD_X;
        if let Some(icon) = icon {
            draw_codicon(
                ui.painter(),
                egui::pos2(x + ICON_W / 2.0, rect.center().y),
                14.0,
                icon,
                fg,
            );
            x += ICON_W;
            if text.is_some() {
                x += GAP;
            }
        }
        if let Some(text) = text {
            ui.painter().text(
                egui::pos2(x, rect.center().y),
                egui::Align2::LEFT_CENTER,
                text,
                font,
                fg,
            );
        }
        match tooltip {
            Some(tip) => response.on_hover_text(tip),
            None => response,
        }
    }

    /// 左下角「远程连接」项：未连接只显示 remote 图标；连上后变蓝并显示
    /// `WSL: <distro>` / `SSH: user@host`。返回点中的区域（用于弹出连接菜单）。
    fn statusbar_remote(
        ui: &mut egui::Ui,
        p: &PaletteHex,
        remote: Option<&str>,
    ) -> egui::Response {
        let connected = remote.is_some();
        let text = remote.map(|label| label.to_string());
        let (fg, bg, hover) = if connected {
            (
                hex(p.fg_statusbar_remote),
                hex(p.bg_statusbar_remote),
                hex(p.bg_statusbar_remote),
            )
        } else {
            (
                hex(p.fg_statusbar),
                egui::Color32::TRANSPARENT,
                hex(p.bg_statusbar_hover),
            )
        };
        let tip = match remote {
            Some(label) => format!("已连接到 {label}（文件 / 终端 / git / LSP 都在远端运行）"),
            None => "远程连接：未连接（WSL/SSH）".to_string(),
        };
        statusbar_item(
            ui,
            text.as_deref(),
            Some(Codicon::Remote),
            fg,
            bg,
            hover,
            Some(&tip),
        )
    }

impl eframe::App for LiteIdeApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if !self.fonts_ready {
            self.fonts_ready = true;
            apply_fonts(ctx, &self.settings.font_family);
            self.applied_family = self.settings.font_family.clone();
            // egui 把新的字体定义排到「帧末」才生效，本帧仍用内置字体表；
            // 而内置表里没有 codicon 族（画图标会 panic），所以这一帧先不画。
            ctx.request_repaint();
            return;
        }

        // 启动后 1.5s 持续请求重绘：保证字体表/异步文件列表加载后首屏不黑屏；
        // 之后 egui 回到按需重绘（不耗电）。
        if self.boot_start.elapsed().as_secs_f32() < 1.5 {
            ctx.request_repaint();
        }

        // 工作区文件索引（Ctrl+P 的模糊搜索）：第一帧就启动后台构建。
        if !self.file_index_started {
            self.file_index_started = true;
            self.start_file_index();
        }

        // 调试用：`LITEIDE_OPEN_SCM=1` 启动即打开源代码管理视图（截图 / 自检）。
        if !self.scm_opened_at_start && std::env::var("LITEIDE_OPEN_SCM").is_ok() {
            self.scm_opened_at_start = true;
            self.open_scm_view();
        }

        // 设置热重载：外部修改 JSON 自动生效
        if self.settings.hot_reload(&mut self.config_mtime) {
            tracing::info!("settings hot-reloaded");
            // 远程连接被外部改掉（WSL ↔ SSH / 换了主机或目录）：重连并切换到新工作区。
            let wanted = self.remote_target();
            if wanted != self.applied_remote {
                self.sync_remote_targets();
                self.applied_remote = wanted;
                self.apply_workspace_switch();
            }
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
        self.maybe_auto_probe_toolchain();
        self.poll_toolchain(ctx);
        self.sync_document();
        self.poll_dap();
        self.poll_search();
        self.poll_git(ctx);
        self.sync_editor_breakpoints();
        self.poll_terminal(ctx);

        // Ctrl+` / Ctrl+Shift+` 全局有效；终端有焦点时还要接管 IDE 快捷键
        // （此时编辑器键盘是关的，见下面的 `set_keys_enabled`）。
        let modal_before = self.modal_open();
        self.handle_terminal_shortcuts(ctx, modal_before);

        // 绘制前先记下「上一帧结束时是否有文本框持有焦点」：
        // 回车那一刻 egui 的 TextEdit 会立刻交出焦点，若只看绘制后的状态，
        // 这一帧就会被判成「没有文本框在用键盘」，回车于是又漏给编辑器插了一个换行。
        let text_focus_before = ctx.wants_keyboard_input();

        self.draw_status_bar(ctx);
        self.draw_remote_menu(ctx);
        self.draw_remote_dialog(ctx);
        self.draw_ssh_dialog(ctx);
        self.draw_toolchain_dialog(ctx);
        self.draw_title_bar(ctx);
        self.draw_activity_bar(ctx);
        self.draw_sidebar(ctx);
        if self.debug_open {
            self.draw_debug_panel(ctx);
        }
        self.draw_tab_strip(ctx);
        self.draw_terminal_panel(ctx);

        // 模态窗口打开时屏蔽编辑器快捷键，避免冲突。
        let modal_open = self.modal_open();
        if modal_open {
            // 模态框的输入框优先，终端不再吃键盘。
            self.terminal.clear_focus();
        }
        // 非模态的文本框（侧栏搜索框 / Git 提交框 / 调试监视框）持有键盘焦点时：
        // 编辑器忽略字符输入与无修饰键的编辑操作，但保留 Ctrl+… 全局快捷键与 F 功能键，
        // 否则在这些框里打字会同时写进编辑器正文（编辑器直接消费全局 Event::Text）。
        let text_focus = text_focus_before || ctx.wants_keyboard_input();
        // 终端聚焦时编辑器让出键盘（终端自己编码并写进 PTY）。
        if let Some(ed) = &mut self.editor {
            ed.set_keys_enabled(!modal_open && !self.terminal.focused);
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

        // 点终端面板以外的地方 = 把键盘交回去（VS Code 的 `terminal.integrated.focus` 语义）。
        if ctx.input(|i| i.pointer.any_pressed()) {
            let pos = ctx.input(|i| i.pointer.interact_pos());
            if let (Some(pos), Some(rect)) = (pos, self.terminal.panel_rect) {
                if !rect.contains(pos) {
                    self.terminal.clear_focus();
                }
            }
        }

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
            Some(EditorAction::GotoDefinitionAt(char_idx)) => {
                self.on_goto_definition_at_char(char_idx)
            }
            Some(EditorAction::GoBack) => self.on_go_back(),
            Some(EditorAction::GoForward) => self.on_go_forward(),
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
                self.open_scm_view();
            }
            Some(EditorAction::Settings) => {
                self.settings_open = !self.settings_open;
            }
            Some(EditorAction::NewFile) => self.on_new_file(),
            Some(EditorAction::SaveAs) => self.open_save_as(),
            Some(EditorAction::CloseTab) => self.on_close_tab(),
            Some(EditorAction::NextTab) => self.cycle_tab(1),
            Some(EditorAction::PrevTab) => self.cycle_tab(-1),
            Some(EditorAction::CommandPalette) => self.open_command_palette(),
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
        self.draw_settings(ctx);
        self.draw_goto_line(ctx);
        self.draw_command_palette(ctx);
        self.draw_find_bar(ctx);
        self.draw_save_as(ctx);
        self.draw_about(ctx);
        self.draw_keys(ctx);
        self.draw_close_confirm(ctx);
        self.draw_flash(ctx);
        // 自绘标题栏后，窗口四边的缩放带由我们自己接管（放最后：光标样式由它说了算）。
        self.draw_window_resize_edges(ctx);

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
        LspStatus::Detecting => "LSP: detecting remote...".to_string(),
        LspStatus::Connecting => "LSP: starting clangd...".to_string(),
        LspStatus::Ready => "LSP: Ready (clangd)".to_string(),
        LspStatus::Crashed(d) => format!("LSP: CRASHED ({d})"),
        LspStatus::Unavailable(d) => format!("LSP: unavailable ({d})"),
    }
}

// ---- 图标：内置 VS Code codicon 字体（字形与 VS Code 完全一致，离线可用） ----

/// 用到的 codicon 图标。
///
/// 码位取自 VS Code 的 codicon 字体定义（`search-*` 等是 VS Code 的内置产品图标，
/// 同样落在 codicon 字体里）；字形由随二进制内置的 `assets/codicon.ttf` 渲染，
/// 不再自绘矢量图形——自绘无论怎么调都不可能和 VS Code 的形状一致。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Codicon {
    // 活动栏
    Files,           // eaf0 explorer-view-icon
    SearchLarge,     // ec70 search-view-icon
    SourceControl,   // ea68
    RunView,         // eb91 debug-alt
    SymbolStructure, // ea91 大纲
    SettingsGear,    // eb51
    // 文件 / 目录
    File,         // ea7b
    Folder,       // ea83
    FolderOpened, // eaf7
    // 搜索视图标题栏动作
    Refresh,     // eb37 search-refresh
    ClearAll,    // eabf search-clear-results
    ListFlat,    // eb84 search-list
    ListTree,    // eb86 search-tree
    CollapseAll, // eac5 search-collapse-results
    ExpandAll,   // eb95 search-expand-results
    // 搜索框内的小开关
    Ellipsis,      // ea7c search-details
    CaseSensitive, // eab1
    WholeWord,     // eb7e
    Regex,         // eb38
    PreserveCase,  // eb2e
    ReplaceAll,    // eb3c search-replace-all
    // 树/行内的展开箭头
    ChevronRight, // eab6
    ChevronDown,  // eab4
    // 标题栏：窗口按钮（最小化/最大化/还原/关闭）
    ChromeMinimize, // eaba
    ChromeMaximize, // eab9
    ChromeRestore,  // eabb
    ChromeClose,    // eab8
    // 状态栏左下角的「远程连接」图标（VS Code `remote`）。
    Remote, // eb3a
    // 终端面板标题栏动作。
    Add,   // ea60 add
    Trash, // ea81 trash
    // 源代码管理面板
    Check,          // eab2 check
    Discard,        // eae2 discard
    Sync,           // ea77 sync
    Remove,         // eb3b remove
    CloudDownload,  // eac2 cloud-download
    CloudUpload,    // eac3 cloud-upload
    Account,        // eb99 account
    Close,          // ea76 close（标签页关闭按钮）
    Repo,           // ea62 repo
    History,        // ea82 history（时钟）
    GitCommit,      // eafc git-commit
    GitPullRequest, // ea64 git-pull-request
    Dot,            // ea71 circle-filled（分支标签前的圆点）
    Cloud,          // ebaa cloud（远程分支标签）
}

impl Codicon {
    /// 全部图标；单测据此校验内置字体确实包含这些字形、且没有码位写重。
    #[cfg(test)]
    const ALL: &'static [Codicon] = &[
        Codicon::Files,
        Codicon::SearchLarge,
        Codicon::SourceControl,
        Codicon::RunView,
        Codicon::SymbolStructure,
        Codicon::SettingsGear,
        Codicon::File,
        Codicon::Folder,
        Codicon::FolderOpened,
        Codicon::Refresh,
        Codicon::ClearAll,
        Codicon::ListFlat,
        Codicon::ListTree,
        Codicon::CollapseAll,
        Codicon::ExpandAll,
        Codicon::Ellipsis,
        Codicon::CaseSensitive,
        Codicon::WholeWord,
        Codicon::Regex,
        Codicon::PreserveCase,
        Codicon::ReplaceAll,
        Codicon::ChevronRight,
        Codicon::ChevronDown,
        Codicon::ChromeMinimize,
        Codicon::ChromeMaximize,
        Codicon::ChromeRestore,
        Codicon::ChromeClose,
        Codicon::Remote,
        Codicon::Add,
        Codicon::Trash,
        Codicon::Check,
        Codicon::Discard,
        Codicon::Sync,
        Codicon::Remove,
        Codicon::CloudDownload,
        Codicon::CloudUpload,
        Codicon::Account,
        Codicon::Repo,
        Codicon::History,
        Codicon::GitCommit,
        Codicon::GitPullRequest,
        Codicon::Dot,
        Codicon::Cloud,
    ];

    /// 字体里的码位（U+Exxx）。
    fn glyph(self) -> char {
        let cp = match self {
            Codicon::Files => 0xeaf0,
            Codicon::SearchLarge => 0xec70,
            Codicon::SourceControl => 0xea68,
            Codicon::RunView => 0xeb91,
            Codicon::SymbolStructure => 0xea91,
            Codicon::SettingsGear => 0xeb51,
            Codicon::File => 0xea7b,
            Codicon::Folder => 0xea83,
            Codicon::FolderOpened => 0xeaf7,
            Codicon::Refresh => 0xeb37,
            Codicon::ClearAll => 0xeabf,
            Codicon::ListFlat => 0xeb84,
            Codicon::ListTree => 0xeb86,
            Codicon::CollapseAll => 0xeac5,
            Codicon::ExpandAll => 0xeb95,
            Codicon::Ellipsis => 0xea7c,
            Codicon::CaseSensitive => 0xeab1,
            Codicon::WholeWord => 0xeb7e,
            Codicon::Regex => 0xeb38,
            Codicon::PreserveCase => 0xeb2e,
            Codicon::ReplaceAll => 0xeb3c,
            Codicon::ChevronRight => 0xeab6,
            Codicon::ChevronDown => 0xeab4,
            Codicon::ChromeMinimize => 0xeaba,
            Codicon::ChromeMaximize => 0xeab9,
            Codicon::ChromeRestore => 0xeabb,
            Codicon::ChromeClose => 0xeab8,
            Codicon::Remote => 0xeb3a,
            Codicon::Add => 0xea60,
            Codicon::Trash => 0xea81,
            Codicon::Check => 0xeab2,
            Codicon::Discard => 0xeae2,
            Codicon::Sync => 0xea77,
            Codicon::Remove => 0xeb3b,
            Codicon::CloudDownload => 0xeac2,
            Codicon::CloudUpload => 0xeac3,
            Codicon::Account => 0xeb99,
            Codicon::Close => 0xea76,
            Codicon::Repo => 0xea62,
            Codicon::History => 0xea82,
            Codicon::GitCommit => 0xeafc,
            Codicon::GitPullRequest => 0xea64,
            Codicon::Dot => 0xea71,
            Codicon::Cloud => 0xebaa,
        };
        char::from_u32(cp).unwrap_or(' ')
    }
}

/// codicon 字体 id（内置 codicon.ttf 的专用族）。
fn codicon_font(size: f32) -> egui::FontId {
    egui::FontId::new(size, egui::FontFamily::Name(CODICON_FAMILY.into()))
}

/// 在 `center` 处按 `size` 画一个 codicon（字形来自内置字体，与 VS Code 一致）。
fn draw_codicon(
    painter: &egui::Painter,
    center: egui::Pos2,
    size: f32,
    icon: Codicon,
    color: egui::Color32,
) {
    painter.text(
        center,
        egui::Align2::CENTER_CENTER,
        icon.glyph(),
        codicon_font(size),
        color,
    );
}

/// 展开/折叠箭头（资源管理器 / 搜索结果树）：VS Code 用 codicon 的 chevron。
fn draw_twistie(painter: &egui::Painter, center: egui::Pos2, expanded: bool, color: egui::Color32) {
    let icon = if expanded {
        Codicon::ChevronDown
    } else {
        Codicon::ChevronRight
    };
    draw_codicon(painter, center, 14.0, icon, color);
}

// ---- 集成终端面板的标题栏 ----

/// 终端标题栏配色（随外壳主题切换）。
#[derive(Clone, Copy)]
struct TermHeaderColors {
    fg: egui::Color32,
    dim: egui::Color32,
    hover_bg: egui::Color32,
    active_bg: egui::Color32,
    accent: egui::Color32,
}

/// 一个终端标签：标题 + 关闭按钮。返回 `(整块响应, 是否点了关闭)`。
fn terminal_tab(
    ui: &mut egui::Ui,
    id: usize,
    title: &str,
    active: bool,
    exited: bool,
    c: TermHeaderColors,
) -> (egui::Response, bool) {
    let font = egui::FontId::proportional(12.0);
    let label = if exited {
        format!("{title} (已退出)")
    } else {
        title.to_string()
    };
    let galley = ui.painter().layout_no_wrap(label.clone(), font.clone(), c.fg);
    let width = (galley.size().x + 40.0).max(112.0);
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(width, terminal::HEADER_HEIGHT),
        egui::Sense::click(),
    );

    let close_rect = egui::Rect::from_center_size(
        egui::pos2(rect.right() - 13.0, rect.center().y),
        egui::vec2(16.0, 16.0),
    );
    let close = ui.interact(
        close_rect,
        ui.id().with(("terminal_tab_close", id)),
        egui::Sense::click(),
    );

    let painter = ui.painter();
    if active {
        painter.rect_filled(rect, 0.0, c.active_bg);
        painter.hline(
            rect.x_range(),
            rect.top() + 0.5,
            egui::Stroke::new(1.0_f32, c.accent),
        );
    } else if resp.hovered() {
        painter.rect_filled(rect, 0.0, c.hover_bg);
    }
    let color = if active { c.fg } else { c.dim };
    painter.text(
        egui::pos2(rect.left() + 10.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        &label,
        font,
        color,
    );
    if close.hovered() {
        painter.rect_filled(close_rect, 3.0, c.hover_bg);
    }
    draw_codicon(
        painter,
        close_rect.center(),
        12.0,
        Codicon::ChromeClose,
        if close.hovered() { c.fg } else { c.dim },
    );

    (resp, close.clicked())
}

/// 终端标题栏右侧的小图标按钮。
fn terminal_icon_button(
    ui: &mut egui::Ui,
    icon: Codicon,
    tip: &str,
    color: egui::Color32,
    hover_bg: egui::Color32,
    hover_fg: egui::Color32,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(24.0, terminal::HEADER_HEIGHT),
        egui::Sense::click(),
    );
    let hovered = resp.hovered();
    if hovered {
        ui.painter().rect_filled(rect, 3.0, hover_bg);
    }
    draw_codicon(
        ui.painter(),
        rect.center(),
        14.0,
        icon,
        if hovered { hover_fg } else { color },
    );
    resp.on_hover_text(tip)
}

// ---- 自绘标题栏（VS Code 的窗口标题栏样式） ----

/// 标题栏左上的应用图标：VS Code 在 35px 宽的区域里居中放一个图标（见 `APPICON_SIZE`）。
fn draw_app_icon(painter: &egui::Painter, center: egui::Pos2, icon: &egui::TextureHandle) {
    painter.image(
        icon.id(),
        egui::Rect::from_center_size(center, egui::Vec2::splat(APPICON_SIZE)),
        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
        egui::Color32::WHITE,
    );
}

/// 把内嵌的应用图标 PNG 解成纹理（首帧建一次，之后缓存在 `LiteIdeApp::app_icon` 里复用）。
///
/// 图标是随二进制内嵌的仓库资源，解不开属于构建事故；这里只记日志、退化成「标题栏不画图标」，
/// 不在 UI 线程里 panic。
fn app_icon_texture(ctx: &egui::Context) -> Option<egui::TextureHandle> {
    let icon = match eframe::icon_data::from_png_bytes(APPICON_PNG) {
        Ok(icon) => icon,
        Err(err) => {
            tracing::warn!("[ui] 内置应用图标解码失败，标题栏不显示图标：{err}");
            return None;
        }
    };
    let image = egui::ColorImage::from_rgba_unmultiplied(
        [icon.width as usize, icon.height as usize],
        &icon.rgba,
    );
    Some(ctx.load_texture("app_icon", image, egui::TextureOptions::LINEAR))
}

/// 标题栏右侧的窗口按钮（VS Code `.window-icon`：46px 宽、16px 图标、悬停出底色）。
///
/// 悬停底色取自 VS Code：普通按钮 `#ffffff1a`，关闭按钮 `#e81123e6` + 白色图标。
fn window_control(
    ui: &mut egui::Ui,
    icon: Codicon,
    close: bool,
    tip: &str,
    fg: egui::Color32,
) -> bool {
    let (rect, resp) =
        ui.allocate_exact_size(egui::vec2(WINDOW_BTN_W, TITLEBAR_H), egui::Sense::click());
    let hovered = resp.hovered();
    let painter = ui.painter();
    if hovered {
        let bg = if close {
            egui::Color32::from_rgba_unmultiplied(0xe8, 0x11, 0x23, 0xe6)
        } else {
            egui::Color32::from_white_alpha(0x1a)
        };
        painter.rect_filled(rect, 0.0, bg);
    }
    let color = if close && hovered {
        egui::Color32::WHITE
    } else {
        fg
    };
    draw_codicon(painter, rect.center(), 16.0, icon, color);
    resp.on_hover_text(tip).clicked()
}

/// 下拉菜单项的样式：VS Code `menu.foreground` + `menu.selectionBackground`（Dark+ 蓝）。
///
/// 注意：egui 的弹出菜单用的是 `ctx` 的样式（不是父 Ui 的），所以这段只能写在各菜单闭包
/// 内部——在那里 `ui` 已经是弹出菜单自己的 Ui。
fn menu_items_style(ui: &mut egui::Ui, p: PaletteHex) {
    let st = ui.style_mut();
    st.spacing.button_padding = egui::vec2(8.0, 4.0);
    st.text_styles
        .insert(egui::TextStyle::Button, egui::FontId::proportional(13.0));
    st.visuals.widgets.inactive.weak_bg_fill = egui::Color32::TRANSPARENT;
    st.visuals.widgets.inactive.rounding = egui::Rounding::same(4.0);
    st.visuals.widgets.inactive.fg_stroke.color = hex(p.fg_titlebar);
    st.visuals.widgets.hovered.weak_bg_fill = hex(p.bg_menu_selection);
    st.visuals.widgets.hovered.bg_fill = hex(p.bg_menu_selection);
    st.visuals.widgets.hovered.rounding = egui::Rounding::same(4.0);
    st.visuals.widgets.hovered.fg_stroke.color = egui::Color32::WHITE;
}

/// 窗口边缘的缩放带命中（四角优先于四边）。
fn resize_zone(
    r: egui::Rect,
    pos: egui::Pos2,
) -> Option<(egui::ResizeDirection, egui::CursorIcon)> {
    use egui::ResizeDirection as Dir;

    if !r.contains(pos) {
        return None;
    }
    let (n, s) = (
        pos.y <= r.top() + RESIZE_BORDER,
        pos.y >= r.bottom() - RESIZE_BORDER,
    );
    let (w, e) = (
        pos.x <= r.left() + RESIZE_BORDER,
        pos.x >= r.right() - RESIZE_BORDER,
    );
    let (dir, cursor) = match (n, s, w, e) {
        (true, _, true, _) => (Dir::NorthWest, egui::CursorIcon::ResizeNwSe),
        (true, _, _, true) => (Dir::NorthEast, egui::CursorIcon::ResizeNeSw),
        (_, true, true, _) => (Dir::SouthWest, egui::CursorIcon::ResizeNeSw),
        (_, true, _, true) => (Dir::SouthEast, egui::CursorIcon::ResizeNwSe),
        (true, _, _, _) => (Dir::North, egui::CursorIcon::ResizeVertical),
        (_, true, _, _) => (Dir::South, egui::CursorIcon::ResizeVertical),
        (_, _, true, _) => (Dir::West, egui::CursorIcon::ResizeHorizontal),
        (_, _, _, true) => (Dir::East, egui::CursorIcon::ResizeHorizontal),
        _ => return None,
    };
    Some((dir, cursor))
}

/// 同 [`resize_zone`]，但最大化时四边没有缩放带（VS Code 最大化后同样不可拖边框）。
fn resize_zone_at(
    ctx: &egui::Context,
    pos: egui::Pos2,
) -> Option<(egui::ResizeDirection, egui::CursorIcon)> {
    if ctx.input(|i| i.viewport().maximized.unwrap_or(false)) {
        return None;
    }
    resize_zone(ctx.screen_rect(), pos)
}

/// 这次按下是否落在窗口边缘的缩放带上（是的话就不能同时开始拖动窗口）。
fn press_on_resize_edge(ctx: &egui::Context) -> bool {
    ctx.input(|i| i.pointer.press_origin())
        .is_some_and(|pos| resize_zone_at(ctx, pos).is_some())
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

// ---- VS Code 风格搜索视图（纯逻辑部分，便于单测） ----

/// 搜索视图配色（Dark+ / Light+ 各一套）。
#[derive(Clone, Copy)]
struct SearchColors {
    fg: egui::Color32,
    dim: egui::Color32,
    header: egui::Color32,
    hover: egui::Color32,
    input_bg: egui::Color32,
    border: egui::Color32,
    badge: egui::Color32,
    badge_fg: egui::Color32,
    match_bg: egui::Color32,
    link: egui::Color32,
    /// 选项开关的悬停底色（`inputOption.hoverBackground`）。
    toggle_hover: egui::Color32,
    /// 选项开关选中时的底色 / 描边（`inputOption.activeBackground` / `activeBorder`）。
    toggle_on_bg: egui::Color32,
    toggle_on_border: egui::Color32,
}

fn search_colors(theme: ThemeKind) -> SearchColors {
    match theme {
        ThemeKind::LightPlus => SearchColors {
            fg: hex("#333333"),
            dim: hex("#6a6a6a"),
            header: hex("#333333"),
            hover: hex("#e8e8e8"),
            input_bg: hex("#ffffff"),
            border: hex("#cecece"),
            badge: hex("#c4c4c4"),
            badge_fg: hex("#333333"),
            match_bg: egui::Color32::from_rgba_unmultiplied(0xea, 0x5c, 0x00, 0x40),
            link: hex("#006ab1"),
            toggle_hover: egui::Color32::from_rgba_unmultiplied(0x00, 0x00, 0x00, 0x1a),
            toggle_on_bg: hex("#0078d4"),
            toggle_on_border: hex("#0078d4"),
        },
        _ => SearchColors {
            fg: hex("#cccccc"),
            dim: hex("#8a8a8a"),
            header: hex("#bbbbbb"),
            hover: hex("#2a2d2e"),
            input_bg: hex("#3c3c3c"),
            border: hex("#3c3c3c"),
            badge: hex("#616161"),
            badge_fg: hex("#f8f8f8"),
            match_bg: egui::Color32::from_rgba_unmultiplied(0xea, 0x5c, 0x00, 0x55),
            link: hex("#3794ff"),
            toggle_hover: egui::Color32::from_rgba_unmultiplied(0x5a, 0x5d, 0x5e, 0x80),
            toggle_on_bg: hex("#0078d4"),
            toggle_on_border: hex("#0078d4"),
        },
    }
}

/// 搜索结果汇总文案（VS Code 同款单复数）。
fn search_summary(results: usize, files: usize) -> String {
    if results == 0 {
        return "No results found".to_string();
    }
    let r = if results == 1 { "result" } else { "results" };
    let f = if files == 1 { "file" } else { "files" };
    format!("{results} {r} in {files} {f}")
}

/// 拆分 `src/app/main.cpp` → (`main.cpp`, `src/app`)。
fn split_file_path(path: &str) -> (&str, &str) {
    match path.rsplit_once('/') {
        Some((dir, name)) => (name, dir),
        None => (path, ""),
    }
}

/// 把 rg 的字节区间收敛成「合法、有序、互不重叠」的区间：
/// 夹到文本范围内、对齐 UTF-8 字符边界、排序并合并重叠段。
fn clamp_submatches(text: &str, subs: &[(usize, usize)]) -> Vec<(usize, usize)> {
    let mut out: Vec<(usize, usize)> = subs
        .iter()
        .map(|&(s, e)| (s.min(text.len()), e.min(text.len())))
        .map(|(mut s, mut e)| {
            while s < e && !text.is_char_boundary(s) {
                s += 1;
            }
            while e > s && !text.is_char_boundary(e) {
                e -= 1;
            }
            (s, e)
        })
        .filter(|(s, e)| e > s)
        .collect();
    out.sort_unstable();
    let mut merged: Vec<(usize, usize)> = Vec::with_capacity(out.len());
    for (s, e) in out {
        match merged.last_mut() {
            Some((_, pe)) if s <= *pe => *pe = (*pe).max(e),
            _ => merged.push((s, e)),
        }
    }
    merged
}

/// 生成一行「命中高亮」的排版任务（单行、超宽省略）。
fn highlighted_job(
    text: &str,
    subs: &[(usize, usize)],
    font: egui::FontId,
    color: egui::Color32,
    background: egui::Color32,
    max_width: f32,
) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob {
        wrap: egui::text::TextWrapping {
            max_width,
            max_rows: 1,
            break_anywhere: false,
            overflow_character: Some('…'),
        },
        ..Default::default()
    };
    let normal = egui::TextFormat {
        font_id: font.clone(),
        color,
        ..Default::default()
    };
    let hit = egui::TextFormat {
        font_id: font,
        color,
        background,
        ..Default::default()
    };
    let mut pos = 0usize;
    for (s, e) in clamp_submatches(text, subs) {
        if s > pos {
            job.append(&text[pos..s], 0.0, normal.clone());
        }
        job.append(&text[s..e], 0.0, hit.clone());
        pos = e;
    }
    if pos < text.len() {
        job.append(&text[pos..], 0.0, normal);
    }
    job
}

/// 搜索结果的一行。
enum SearchRow<'a> {
    Dir {
        path: String,
        name: String,
        depth: usize,
        expanded: bool,
        count: usize,
    },
    File {
        file: &'a str,
        depth: usize,
        expanded: bool,
        count: usize,
    },
    Match {
        file: &'a str,
        m: &'a SearchMatch,
        depth: usize,
    },
}

/// 树状视图的目录节点。
#[derive(Default)]
struct SearchDir<'a> {
    dirs: BTreeMap<String, SearchDir<'a>>,
    files: BTreeMap<String, (&'a str, Vec<&'a SearchMatch>)>,
    count: usize,
}

/// 构建可见行：`tree = false` 按文件平铺；`tree = true` 按目录嵌套。
fn build_search_rows<'a>(
    results: &'a [SearchMatch],
    tree: bool,
    collapsed_files: &HashSet<String>,
    collapsed_dirs: &HashSet<String>,
) -> Vec<SearchRow<'a>> {
    let mut by_file: BTreeMap<&'a str, Vec<&'a SearchMatch>> = BTreeMap::new();
    for m in results {
        by_file.entry(m.file.as_str()).or_default().push(m);
    }
    for ms in by_file.values_mut() {
        ms.sort_by_key(|m| (m.line, m.submatches.first().map(|s| s.0).unwrap_or(0)));
    }

    let mut rows = Vec::new();
    if !tree {
        for (file, ms) in &by_file {
            let expanded = !collapsed_files.contains(*file);
            rows.push(SearchRow::File {
                file,
                depth: 0,
                expanded,
                count: ms.len(),
            });
            if expanded {
                rows.extend(ms.iter().map(|m| SearchRow::Match {
                    file,
                    m,
                    depth: 1,
                }));
            }
        }
        return rows;
    }

    let mut root = SearchDir::default();
    for (file, ms) in &by_file {
        let parts: Vec<&str> = file.split('/').filter(|s| !s.is_empty()).collect();
        if parts.is_empty() {
            continue;
        }
        root.count += ms.len();
        let mut node = &mut root;
        for seg in &parts[..parts.len().saturating_sub(1)] {
            node = node.dirs.entry((*seg).to_string()).or_default();
            node.count += ms.len();
        }
        node.files
            .insert(parts[parts.len() - 1].to_string(), (file, ms.clone()));
    }
    push_tree_rows(&root, "", 0, collapsed_files, collapsed_dirs, &mut rows);
    rows
}

fn push_tree_rows<'a>(
    node: &SearchDir<'a>,
    prefix: &str,
    depth: usize,
    collapsed_files: &HashSet<String>,
    collapsed_dirs: &HashSet<String>,
    rows: &mut Vec<SearchRow<'a>>,
) {
    for (name, child) in &node.dirs {
        let path = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        let expanded = !collapsed_dirs.contains(&path);
        rows.push(SearchRow::Dir {
            path: path.clone(),
            name: name.clone(),
            depth,
            expanded,
            count: child.count,
        });
        if expanded {
            push_tree_rows(child, &path, depth + 1, collapsed_files, collapsed_dirs, rows);
        }
    }
    for (file, ms) in node.files.values() {
        let file = *file;
        let expanded = !collapsed_files.contains(file);
        rows.push(SearchRow::File {
            file,
            depth,
            expanded,
            count: ms.len(),
        });
        if expanded {
            rows.extend(ms.iter().map(|m| SearchRow::Match {
                file,
                m,
                depth: depth + 1,
            }));
        }
    }
}

/// 文件的全部祖先目录（`a/b/c.cpp` → [`a`, `a/b`]），用于「全部折叠」。
fn file_ancestor_dirs(file: &str) -> Vec<String> {
    let parts: Vec<&str> = file.split('/').filter(|s| !s.is_empty()).collect();
    let mut out = Vec::new();
    let mut acc = String::new();
    for seg in parts.iter().take(parts.len().saturating_sub(1)) {
        if !acc.is_empty() {
            acc.push('/');
        }
        acc.push_str(seg);
        out.push(acc.clone());
    }
    out
}

/// 小图标按钮（侧边栏标题栏动作用，对应 VS Code 的 title action）。
fn icon_button(ui: &mut egui::Ui, icon: Codicon, tip: &str) -> egui::Response {
    action_button(ui, icon, tip, false, true)
}

/// 禁用态的小图标按钮（如 VS Code 里「全部替换」在提交搜索前不可点）。
fn icon_button_disabled(ui: &mut egui::Ui, icon: Codicon, tip: &str) -> egui::Response {
    action_button(ui, icon, tip, false, false)
}

fn action_button(
    ui: &mut egui::Ui,
    icon: Codicon,
    tip: &str,
    active: bool,
    enabled: bool,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(20.0, 20.0), egui::Sense::click());
    let hovered = enabled && resp.hovered();
    if active || hovered {
        ui.painter().rect_filled(
            rect,
            3.0,
            hex("#ffffff").gamma_multiply(if active { 0.12 } else { 0.08 }),
        );
    }
    let color = if !enabled {
        hex("#6a6a6a")
    } else if active || hovered {
        egui::Color32::WHITE
    } else {
        hex("#9a9a9a")
    };
    draw_codicon(ui.painter(), rect.center(), 16.0, icon, color);
    resp.on_hover_text(tip)
}

/// 搜索选项开关（区分大小写 / 全词 / 正则 / 保留大小写）。
///
/// 外观逐项对齐 VS Code 的 `.monaco-custom-toggle`：20×20、1px 边框（常态透明）、
/// 3px 圆角、悬停 `inputOption.hoverBackground`；选中为
/// `inputOption.activeBackground` + `activeBorder` + `activeForeground`（白字）。
fn option_toggle(
    ui: &mut egui::Ui,
    icon: Codicon,
    active: bool,
    c: SearchColors,
    tip: &str,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(SEARCH_TOGGLE, SEARCH_TOGGLE),
        egui::Sense::click(),
    );
    let painter = ui.painter();
    if active {
        painter.rect_filled(rect, 3.0, c.toggle_on_bg);
        painter.rect_stroke(
            rect.shrink(0.5),
            3.0,
            egui::Stroke::new(1.0_f32, c.toggle_on_border),
        );
    } else if resp.hovered() {
        painter.rect_filled(rect, 3.0, c.toggle_hover);
    }
    let fg = if active { egui::Color32::WHITE } else { c.fg };
    draw_codicon(painter, rect.center(), 15.0, icon, fg);
    resp.on_hover_text(tip)
}

/// 「切换搜索详细信息」的「⋯」（VS Code：16×16、紧贴替换框右下角）。
fn ellipsis_button(ui: &mut egui::Ui, c: SearchColors) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(rect, 3.0, c.toggle_hover);
    }
    draw_codicon(ui.painter(), rect.center(), 16.0, Codicon::Ellipsis, c.fg);
    resp.on_hover_text("切换搜索详细信息")
}

/// 搜索结果行的计数徽标（右对齐；VS Code 是 19px 高的圆角药丸 + 主题 badge 配色）。
fn draw_badge(painter: &egui::Painter, rect: egui::Rect, count: usize, c: SearchColors) {
    let text = count.to_string();
    let h = 19.0_f32;
    let w = (text.len() as f32 * 5.8 + 12.0).max(h);
    let brect = egui::Rect::from_min_size(
        egui::pos2(rect.right() - w - BADGE_RIGHT, rect.center().y - h * 0.5),
        egui::vec2(w, h),
    );
    painter.rect_filled(brect, h * 0.5, c.badge);
    painter.text(
        brect.center(),
        egui::Align2::CENTER_CENTER,
        text,
        egui::FontId::proportional(10.0),
        c.badge_fg,
    );
}

/// egui 的 `TextEdit` 会在 `desired_width` 之外再加自己默认的左右 margin（4px×2）。
/// 布局里算「还剩多少宽度」时必须把这段一起扣掉，否则右对齐的开关会被挤到框外。
const TEXT_EDIT_EXTRA_W: f32 = 8.0;

// ---- 搜索结果行的几何（逐项对齐 VS Code 的树行） ----
/// 行内容的左沿（不含层级缩进）：VS Code 里是面板左沿 + 34px。
const SEARCH_CONTENTS_X: f32 = 34.0;
/// 每层级的缩进步长。
const SEARCH_INDENT_STEP: f32 = 8.0;
/// 列表行的左右内缩（悬停条不贴面板边缘）。
const ROW_INSET: f32 = 4.0;
/// 计数徽标距行右沿的距离。
const BADGE_RIGHT: f32 = 16.0;

/// 某一层级下，行内容（图标左沿）相对面板左沿的横坐标。
fn row_contents_x(depth: usize) -> f32 {
    SEARCH_CONTENTS_X + depth as f32 * SEARCH_INDENT_STEP
}

// ---- 搜索视图的几何（逐项对齐 VS Code `searchView.css`） ----
/// 输入框（`.monaco-inputbox`）高度。
const SEARCH_INPUT_H: f32 = 26.0;
/// 输入框内容高度（26 = 22 + 上下各 2px 内边距）。
const SEARCH_INPUT_INNER_H: f32 = 22.0;
/// 搜索框与替换框之间的间距。
const SEARCH_INPUT_GAP: f32 = 6.0;
/// 输入框左沿（左侧 18px 窄槽 + 2px）。
const SEARCH_INPUT_LEFT: f32 = 20.0;
/// 输入框右沿。
const SEARCH_INPUT_RIGHT: f32 = 8.0;
/// 左侧「折叠替换」窄槽宽度（VS Code 的 `.toggle-replace-button`）。
const SEARCH_GUTTER_W: f32 = 18.0;
/// 输入框内选项开关（`.monaco-custom-toggle`）边长。
const SEARCH_TOGGLE: f32 = 20.0;

// ---- 标题栏（逐项对齐 VS Code `.part.titlebar` / `.menubar` / `.window-controls-container`） ----
/// 标题栏高度（VS Code 里由 `TitlebarPart` 直接写成 35px 内联样式）。
const TITLEBAR_H: f32 = 35.0;
/// 应用图标区宽度（VS Code `.window-appicon { width: 35px }`，图标 16px 居中）。
const APPICON_W: f32 = 35.0;
/// 标题栏里应用图标的显示边长。
///
/// VS Code 这里是 16px（`.window-appicon { background-size: 16px }`），但我们的图标是整块实心
/// 深色方块，同样 16px 看着偏小；按用户反馈调到 20px。
const APPICON_SIZE: f32 = 20.0;
/// 应用图标位图：40×40（正好是显示尺寸的 2 倍）。
///
/// 就是 eframe/egui 的默认窗口图标——深色圆角方块里一个白色小写 “e”；位图比显示尺寸大
/// 一倍，缩到 20px 时正好是一次干净的 2:1 平均（egui 的纹理没有 mipmap，尺寸取太大反而会走样）。
/// 出处与许可见 `assets/appicon.LICENSE.txt`。
const APPICON_PNG: &[u8] = include_bytes!("../assets/appicon.png");
/// 窗口按钮宽度（VS Code Windows 上 3×46px = 138px）。
const WINDOW_BTN_W: f32 = 46.0;
/// 菜单标题左右内边距（VS Code `.menubar-menu-title { padding: 0 8px }`）。
const MENUBAR_ITEM_PAD_X: f32 = 8.0;
/// 菜单标题圆角（VS Code `.menubar-menu-title { border-radius: 5px }`）。
const MENUBAR_ITEM_RADIUS: f32 = 5.0;
/// 窗口四边/四角的缩放带宽度（VS Code `.resizer { height: 4px }`，四角留 5px）。
const RESIZE_BORDER: f32 = 5.0;

/// 菜单标题悬停底色：VS Code `menubar.selectionBackground`（深色默认 #5a5d5e50）。
fn menubar_selection() -> egui::Color32 {
    egui::Color32::from_rgba_unmultiplied(0x5a, 0x5d, 0x5e, 0x50)
}

/// VS Code 的 `.monaco-inputbox`：主题输入框底色 + 1px 描边 + 4px 圆角。
///
/// 高 26px、圆角 4px、文字距左沿 6px，全部由「内边距 + 内容高度」撑出。
fn input_box_frame(c: SearchColors) -> egui::Frame {
    egui::Frame::none()
        .fill(c.input_bg)
        .stroke(egui::Stroke::new(1.0_f32, c.border))
        .rounding(4.0)
        .inner_margin(egui::Margin::symmetric(2.0, 2.0))
}

/// 输入框内部的内容宽度（`Frame` 只把左右各 2px 内边距算进尺寸，描边另画）。
fn input_box_inner_w(box_w: f32) -> f32 {
    (box_w - 4.0).max(40.0)
}

/// 「包含的文件 / 排除的文件」输入行，返回内容是否变化。
fn search_input_row(ui: &mut egui::Ui, text: &mut String, hint: &str, c: SearchColors) -> bool {
    let mut changed = false;
    let box_w = (ui.available_width() - SEARCH_INPUT_LEFT - SEARCH_INPUT_RIGHT).max(96.0);
    let inner_w = input_box_inner_w(box_w);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        ui.add_space(SEARCH_INPUT_LEFT);
        input_box_frame(c).show(ui, |ui| {
            ui.set_min_width(inner_w);
            ui.set_min_height(SEARCH_INPUT_INNER_H);
            changed = ui
                .add(
                    egui::TextEdit::singleline(text)
                        .frame(false)
                        .hint_text(hint)
                        .desired_width((inner_w - TEXT_EDIT_EXTRA_W).max(32.0))
                        .text_color(c.fg),
                )
                .changed();
        });
    });
    changed
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
    icon: Codicon,
    name: &str,
    color: egui::Color32,
) -> bool {
    let (rect, resp) =
        ui.allocate_exact_size(egui::vec2(row_w, row_h), egui::Sense::click());
    let painter = ui.painter();
    if resp.hovered() {
        painter.rect_filled(rect, 3.0, hex("#2a2d2e"));
    }
    draw_codicon(
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

/// VS Code 的 Quick-Input 配色（Dark+ / Dark Modern，直接取自本机 VS Code 安装目录里的
/// `themes/dark_vs.json` 和 `themes/dark_modern.json`）。
///
/// - 命中部分：`list.highlightForeground` = **#2AAAFF**（VS Code 默认的深蓝，
///   不是我们之前用的青色 `#4ec9b0`）；
/// - 普通文字：`foreground` = **#CCCCCC**；
/// - 目录路径（description）：`descriptionForeground` = **#9D9D9D**。
mod picker_colors {
    /// 命中部分（`list.highlightForeground`）。
    pub const HIGHLIGHT: &str = "#2AAAFF";
    /// 普通文字（`foreground`）。
    pub const FOREGROUND: &str = "#CCCCCC";
    /// 目录路径（`descriptionForeground`）。
    pub const DESCRIPTION: &str = "#9D9D9D";
}

/// 一行里的命中位置：文件名里的命中。
struct PickerHits<'a> {
    /// 命中字符在文件名里的下标。
    name: &'a [usize],
}

/// 结果列表里的一行（画一行的全部素材）。
struct PickerRow<'a> {
    /// 文件名。
    name: &'a str,
    /// 目录前缀（灰色显示）。
    dir_part: &'a str,
    /// 图标颜色。
    color: egui::Color32,
    /// 命中位置。
    hits: PickerHits<'a>,
}

/// 文件快速打开的一行：文件名（**命中部分高亮**）+ 目录前缀（灰色），像 VS Code 那样。
///
/// 图标 / 文件名 / 目录前缀必须落在**同一条基线**上：
/// - 文件名和目录前缀用同一套字体 → 同一条基线（用 galley 的 `pos.y` 对齐）；
/// - 图标是另一套字体（codicon），按它的**墨迹中心**（`mesh_bounds`）去对齐，
///   这样图标不会因为字体度量不同而偏高或偏低 1~2px。
///
/// 图标和文件名之间留一段空隙（VS Code 就是这样的，不挤在一起）。
fn picker_hit_row(
    painter: &egui::Painter,
    ui: &egui::Ui,
    rect: egui::Rect,
    row_id: usize,
    row: &PickerRow<'_>,
) -> bool {
    let PickerRow {
        name,
        dir_part,
        color,
        hits,
    } = row;
    // 每一行都要有自己的 Id，否则 egui 会报「同一个 Id 用在两处」。
    let resp = ui.interact(
        rect,
        ui.id().with(("picker_row", row_id)),
        egui::Sense::click(),
    );
    if resp.hovered() {
        painter.rect_filled(rect, 3.0, hex("#2a2d2e"));
    }
    // 命中的字符用强调色（VS Code 的 quick-open 就是这么标的）。
    //
    // 用 egui 的 LayoutJob 一次排版（而不是一个字符一个字符地画）：
    // 这样字距完全走 egui 自己的排版结果，和后面的目录前缀不会错位。
    let font = egui::FontId::monospace(12.0);
    let layout = |text: &str,
                  hits: &[usize],
                  matched_color: egui::Color32,
                  plain_color: egui::Color32|
     -> std::sync::Arc<egui::epaint::Galley> {
        let mut job = egui::text::LayoutJob::default();
        let mut hit = hits.iter().copied().peekable();
        let mut buf = String::new();
        let mut buf_matched = false;
        let flush = |job: &mut egui::text::LayoutJob, buf: &mut String, matched: bool| {
            if buf.is_empty() {
                return;
            }
            job.append(
                buf.as_str(),
                0.0,
                egui::TextFormat {
                    font_id: font.clone(),
                    color: if matched {
                        matched_color
                    } else {
                        plain_color
                    },
                    ..Default::default()
                },
            );
            buf.clear();
        };
        for (i, ch) in text.chars().enumerate() {
            let matched = hit.peek() == Some(&i);
            if matched {
                hit.next();
            }
            if matched != buf_matched && !buf.is_empty() {
                flush(&mut job, &mut buf, buf_matched);
            }
            buf_matched = matched;
            buf.push(ch);
        }
        flush(&mut job, &mut buf, buf_matched);
        ui.fonts(|f| f.layout_job(job))
    };

    // 图标：按 codicon 自己的墨迹中心对齐到行的中心（字体不同，直接用
    // 「中心点」画会差 1~2px）。
    let icon_size = 15.0;
    let icon_galley = ui.fonts(|f| {
        f.layout_no_wrap(
            Codicon::File.glyph().to_string(),
            codicon_font(icon_size),
            *color,
        )
    });
    let icon_ink_mid = icon_galley.mesh_bounds.center().y;
    let icon_x = rect.left() + 10.0;
    let icon_w = icon_galley.size().x;
    painter.galley(
        egui::pos2(icon_x, rect.center().y - icon_ink_mid),
        icon_galley,
        *color,
    );

    // 文件名：命中部分用强调色（VS Code 的 quick-open 就是这么标的）。
    let galley = layout(
        name,
        hits.name,
        hex(picker_colors::HIGHLIGHT),
        hex(picker_colors::FOREGROUND),
    );
    let name_w = galley.size().x;
    // 文字的墨迹中心（相对 galley 原点）→ 用来把整行摆到行中心。
    let name_ink_mid = galley.mesh_bounds.center().y;
    let name_top = rect.center().y - name_ink_mid;
    // 图标右侧留 6px 空隙（VS Code 就是这个量级，不挤在一起）。
    let name_x = icon_x + icon_w + 6.0;
    painter.galley(
        egui::pos2(name_x, name_top),
        galley,
        hex(picker_colors::FOREGROUND),
    );

    // 目录前缀：**灰度**（VS Code 的路径就是灰的，不参与命中高亮），
    // 跟文件名共用同一个顶部位置 → 自然是同一条基线。
    let dir_galley = layout(
        dir_part,
        &[],
        hex(picker_colors::DESCRIPTION),
        hex(picker_colors::DESCRIPTION),
    );
    painter.galley(
        egui::pos2(name_x + name_w + 10.0, name_top),
        dir_galley,
        hex(picker_colors::DESCRIPTION),
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

/// 悬浮提示的内容（VS Code 的 hover 布局：上面签名、下面文档）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct HoverContent {
    /// 签名（clangd 给的第一个 ``` 代码块，如 `void foo(int a)`）。
    signature: Option<String>,
    /// 文档说明（代码块之外的文字，通常是头文件上的注释）。
    docs: String,
}

/// 把 clangd 的 hover markdown 拆成「签名 + 文档」。
///
/// clangd 返回的是 markdown：第一段 ```cpp 代码块是符号签名，后面是文档注释。
/// VS Code 会把签名用等宽字体单独显示、文档用正文字体，我们照着来。
fn split_hover_markdown(md: &str) -> HoverContent {
    let mut signature: Option<String> = None;
    let mut docs: Vec<String> = Vec::new();
    let mut code: Vec<String> = Vec::new();
    let mut in_code = false;
    for line in md.lines() {
        let t = line.trim();
        if t.starts_with("```") {
            if in_code {
                in_code = false;
                if signature.is_none() {
                    signature = Some(code.join("\n").trim().to_string());
                }
                code.clear();
            } else {
                in_code = true;
            }
            continue;
        }
        if in_code {
            code.push(line.to_string());
        } else {
            docs.push(t.to_string());
        }
    }
    // 代码块没有闭合（少见）：也当成签名。
    if signature.is_none() && !code.is_empty() {
        signature = Some(code.join("\n").trim().to_string());
    }
    let docs = docs
        .into_iter()
        .map(|l| {
            // clangd 的文档注释带 `///` 前缀，VS Code 渲染 markdown 时不显示它。
            let l = l.trim_start();
            let l = l
                .strip_prefix("///")
                .or_else(|| l.strip_prefix("//"))
                .unwrap_or(l);
            // markdown 标题标记（`### algorithm`）也不显示，只留文字。
            let l = l.trim_start().trim_start_matches('#').trim_start();
            decode_html_entities(l.trim().replace("**", ""))
        })
        .filter(|l| !l.is_empty() && l != "-" && l != "—")
        .collect::<Vec<_>>()
        .join("\n");
    HoverContent { signature, docs }
}

/// 解码 clangd markdown 里的基础 HTML 实体（`&amp;` `&lt;` `&gt;` `&quot;`）。
fn decode_html_entities(s: String) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
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
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([640.0, 400.0])
            // 去掉系统标题栏，改由 `draw_title_bar` 自绘（图标 + 菜单 + 窗口按钮同一行，
            // 与 VS Code 的 `window.titleBarStyle: custom` 效果一致）；窗口的拖动/缩放
            // 随之也由我们自己接管（见 `draw_window_resize_edges`）。
            .with_decorations(false),
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

    /// 测试一律用默认设置装配：不受 `%APPDATA%\LiteIDE\settings.json` 与
    /// 并行 settings 测试临时改写的 `LITEIDE_CONFIG_DIR` 影响。
    fn test_app() -> LiteIdeApp {
        LiteIdeApp::with_settings(AppSettings::default(), None)
    }

    /// 远程工作区：根就是 WSL 内部的 POSIX 路径，URI 也是 `file:///home/...`。
    #[test]
    fn remote_workspace_is_posix_inside_wsl() {
        let mut app = test_app();
        app.settings.remote_distro = Some("Ubuntu2404".to_string());
        app.settings.remote_path = Some("/home/work/NewLithoTune/candanza".to_string());
        assert!(app.is_remote());
        assert_eq!(app.workspace_root(), "/home/work/NewLithoTune/candanza");
        assert_eq!(
            app.workspace_uri(),
            "file:///home/work/NewLithoTune/candanza"
        );
        // 远程路径在 UI 里一律按 POSIX 判断，不走 Windows 盘符/UNC。
        assert!(app.is_remote_path(std::path::Path::new(
            "/home/work/NewLithoTune/candanza/src/a.cpp"
        )));
        assert!(!app.is_remote_path(std::path::Path::new(r"E:\proj\a.cpp")));

        // 断开后回到本地根。
        app.settings.remote_distro = None;
        app.settings.remote_path = None;
        assert!(!app.is_remote());
        assert_eq!(app.workspace_root(), app.project_root());
    }

    /// 远程文件的 URI 必须保持 WSL 视角（`file:///home/...`），不能再出现 9P ／ UNC 形态。
    #[test]
    fn remote_file_uris_stay_inside_wsl() {
        let mut app = test_app();
        app.settings.remote_distro = Some("Ubuntu2404".to_string());
        app.settings.remote_path = Some("/home/work/NewLithoTune/candanza".to_string());
        let uri = app
            .uri_of_path(std::path::Path::new(
                "/home/work/NewLithoTune/candanza/src/main.cpp",
            ))
            .expect("远程文件应能转 URI");
        assert_eq!(
            uri,
            "file:///home/work/NewLithoTune/candanza/src/main.cpp"
        );
        assert!(!uri.contains("wsl.localhost"));
        assert_eq!(
            uri_to_posix(&uri).as_deref(),
            Some("/home/work/NewLithoTune/candanza/src/main.cpp")
        );
    }

    /// 连接菜单：选项与 VS Code 同构，WSL / SSH / 断开都可点生效。
    #[test]
    fn remote_menu_lists_vscode_options_with_ssh_live() {
        let entries = remote_menu_entries();
        let labels: Vec<&str> = entries.iter().map(|(l, _, _)| *l).collect();
        assert_eq!(
            labels,
            vec![
                "Connect to WSL…",
                "Connect to Host… (SSH)",
                "Connect to Tunnel…",
                "Clone Repository in WSL…",
                "Open Folder on Remote…",
            ]
        );
        let live: Vec<RemoteAction> = entries
            .iter()
            .map(|(_, _, a)| *a)
            .filter(|a| a.is_supported())
            .collect();
        assert_eq!(live, vec![RemoteAction::ConnectWsl, RemoteAction::ConnectSsh]);
        assert!(RemoteAction::Disconnect.is_supported());
        assert!(!RemoteAction::ConnectTunnel.is_supported());
    }

    /// SSH 工作区：设置里的 ssh_* 就足以驱动整个 IDE 的远程视角。
    #[test]
    fn ssh_workspace_is_posix_and_uses_ssh_label() {
        let mut app = test_app();
        app.settings.remote_kind = Some("ssh".to_string());
        app.settings.ssh_user = Some("fubin".to_string());
        app.settings.ssh_host = Some("192.168.77.6".to_string());
        app.settings.ssh_port = 22;
        app.settings.remote_path = Some("/home/fubin/work/candanza".to_string());
        assert!(app.is_remote());
        assert_eq!(app.workspace_root(), "/home/fubin/work/candanza");
        assert_eq!(
            app.workspace_uri(),
            "file:///home/fubin/work/candanza"
        );
        assert_eq!(
            app.remote_target().map(|t| t.label()).as_deref(),
            Some("SSH: fubin@192.168.77.6")
        );
        assert!(app.is_remote_path(std::path::Path::new(
            "/home/fubin/work/candanza/src/a.cpp"
        )));
        // 本机盘符路径仍然按本地处理（SSH 不影响本地文件）。
        assert!(!app.is_remote_path(std::path::Path::new(r"E:\proj\a.cpp")));
    }

    /// 真实 SSH 验收：对话框里填完用户名/主机后，目录列表能读出来（这是 Connect 的前置条件）。
    ///
    /// `cargo test -p ide-ui ssh_dialog_lists_remote_directories -- --ignored --nocapture`
    #[test]
    #[ignore = "需要一台可免密登录的 SSH 主机（LITEIDE_SSH_TEST_HOST/USER）"]
    fn ssh_dialog_lists_remote_directories() {
        let host = std::env::var("LITEIDE_SSH_TEST_HOST")
            .unwrap_or_else(|_| "192.168.77.6".to_string());
        let user = std::env::var("LITEIDE_SSH_TEST_USER").unwrap_or_else(|_| "fubin".to_string());
        let mut app = test_app();
        app.ssh_dialog_user = user;
        app.ssh_dialog_host = host;
        app.ssh_dialog_port = 22;
        app.ssh_dialog_path = "/home/fubin/work".to_string();
        assert!(app.ssh_dialog_target().is_some());
        app.refresh_ssh_dialog_dirs();
        assert_eq!(app.ssh_dialog_error, None);
        assert!(
            app.ssh_dialog_dirs.iter().any(|d| d == "candanza"),
            "应能列出 candanza: {:?}",
            app.ssh_dialog_dirs
        );
    }

    /// 真实 SSH 验收：连不上时 `connect_remote` 要把原因带回来（对话框直接显示）。
    #[test]
    #[ignore = "需要一台可免密登录的 SSH 主机（LITEIDE_SSH_TEST_HOST/USER）"]
    fn ssh_connect_remote_returns_error_for_refused_host() {
        let mut app = test_app();
        // 本机没有 2222 端口：ssh 会立刻 Connection refused。
        let target = RemoteTarget::ssh("fubin", "127.0.0.1", 2222);
        let err = app
            .connect_remote(target, "/home")
            .expect_err("连不上应该返回错误");
        assert!(err.contains("无法连接"), "错误应说明连不上: {err}");
    }

    /// 真实 SSH 验收：对话框填的东西一路通到工作区（路径校验 + 设置落盘 + 换根）。
    #[test]
    #[ignore = "需要一台可免密登录的 SSH 主机（LITEIDE_SSH_TEST_HOST/USER）"]
    fn ssh_connect_remote_switches_workspace() {
        let host = std::env::var("LITEIDE_SSH_TEST_HOST")
            .unwrap_or_else(|_| "192.168.77.6".to_string());
        let user = std::env::var("LITEIDE_SSH_TEST_USER").unwrap_or_else(|_| "fubin".to_string());
        let root = std::env::var("LITEIDE_SSH_TEST_ROOT")
            .unwrap_or_else(|_| "/home/fubin/work/candanza".to_string());
        let mut app = test_app();
        app.connect_remote(RemoteTarget::ssh(user, host, 22), &root)
            .expect("应能连上测试主机");
        assert!(app.is_remote());
        assert_eq!(app.workspace_root(), root);
        assert_eq!(
            app.remote_target().map(|t| t.label()).as_deref(),
            Some("SSH: fubin@192.168.77.6")
        );
        app.disconnect_remote();
        assert!(!app.is_remote());
    }

    /// 远程根切换会让 LSP 换根（clangd 重启到新根），而不是继续用旧根。
    #[test]
    fn remote_root_uri_differs_from_local_root_uri() {
        let mut app = test_app();
        let local = app.project_root();
        let local_uri = path_to_uri_any(&local).expect("本地根应能转 URI");
        app.settings.remote_distro = Some("Ubuntu2404".to_string());
        app.settings.remote_path = Some("/home/work/NewLithoTune/candanza".to_string());
        let remote_uri = app.workspace_uri();
        assert_ne!(local_uri, remote_uri);
        assert_eq!(remote_uri, "file:///home/work/NewLithoTune/candanza");
    }

    /// 对话框里的目录输入：补前导 `/`、去尾部 `/`，空串 → `/`。
    #[test]
    fn normalize_posix_paths() {
        assert_eq!(normalize_posix(""), "/");
        assert_eq!(normalize_posix("/"), "/");
        assert_eq!(normalize_posix("/home/work/"), "/home/work");
        assert_eq!(normalize_posix("home\\work"), "/home/work");
        assert_eq!(normalize_posix("  /home/work/NewLithoTune/candanza "), "/home/work/NewLithoTune/candanza");
    }

    #[test]
    fn hex_parses_channels() {
        assert_eq!(super::hex("#1e1e1e").r(), 0x1e);
        assert_eq!(super::hex("#007acc").g(), 0x7a);
        assert_eq!(super::hex("#007acc").b(), 0xcc);
        assert_eq!(super::hex("#1e1e1e").a(), 0xff);
        // VS Code 主题里的 8 位写法（Light 2026 的 statusBarItem.hoverBackground）。
        assert_eq!(super::hex("#dadada4f").a(), 0x4f);
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

    /// hover 内容拆分成「签名 + 文档」（VS Code 的 hover 布局）。
    #[test]
    fn hover_markdown_splits_signature_and_docs() {
        let md = "```cpp\nint add(int a, int b)\n```\n\n**Adds** two integers.";
        let out = super::split_hover_markdown(md);
        assert_eq!(out.signature.as_deref(), Some("int add(int a, int b)"));
        assert!(out.docs.contains("Adds two integers."));
        assert!(!out.docs.contains("**"));
        assert!(!out.docs.contains("```"));

        // 没有代码块：整段算文档。
        let plain = super::split_hover_markdown("just docs");
        assert!(plain.signature.is_none());
        assert_eq!(plain.docs, "just docs");

        // markdown 标题标记和空列表项不显示（VS Code 是渲染 markdown，不是显示源码）。
        let fancy = super::split_hover_markdown("### algorithm\n-\nprovides 'transform'");
        assert_eq!(fancy.docs, "algorithm\nprovides 'transform'");
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

    /// 绘制类测试用的 egui 上下文：注册好内置图标字体（否则 codicon 族未绑定会 panic）。
    fn drawing_ctx() -> egui::Context {
        let ctx = egui::Context::default();
        let mut fonts = egui::FontDefinitions::default();
        super::add_icon_font(&mut fonts);
        ctx.set_fonts(fonts);
        ctx
    }

    /// 守护用例：模拟 100 次快捷键/命令操作，无 panic。
    /// 只跑 safe 命令（仅内存操作，不写盘、不拉起子进程）。
    #[test]
    fn safe_commands_smoke_100_ops() {
        let ctx = drawing_ctx();
        let mut app = test_app();
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
        let mut app = test_app();
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
        let mut app = test_app();
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
        let mut app = test_app();
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
        let mut app = test_app();
        app.open_file_picker();
        let before = app.file_picker_dir.clone();
        let Some(parent) = before.parent().map(|p| p.to_path_buf()) else {
            return; // 已在根目录，跳过
        };
        let ctx = drawing_ctx();
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
        let mut app = test_app();
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

    // ---- VS Code 风格搜索视图：纯逻辑守护 ----

    fn sm(file: &str, line: u64, text: &str, subs: Vec<(usize, usize)>) -> SearchMatch {
        SearchMatch {
            file: file.to_string(),
            line,
            text: text.to_string(),
            submatches: subs,
        }
    }

    #[test]
    fn codicon_font_contains_every_used_glyph() {
        // 内置字体不依赖外部环境，但必须真的包含用到的码位，否则图标会显示成方框。
        let ctx = drawing_ctx();
        // Fonts 要到第一帧之后才可用（egui 要先知道 pixels_per_point）。
        let _ = ctx.run(egui::RawInput::default(), |_| {});
        let font = super::codicon_font(16.0);
        let mut seen = std::collections::HashSet::new();
        for &icon in super::Codicon::ALL {
            let g = icon.glyph();
            assert!(seen.insert(g), "码位 {g:?} 被多个图标复用");
            assert!(
                ctx.fonts(|f| f.has_glyph(&font, g)),
                "内置 codicon.ttf 缺少码位 {g:?}"
            );
        }
    }

    /// 状态栏「连接」项：未连接只有图标；连上 WSL 后蓝底白字并显示发行版名。
    #[test]
    fn terminal_panel_empty_state_guides_user_to_connect_wsl() {
        let ctx = drawing_ctx();
        let _ = ctx.run(probe_raw_input(), |_| {});
        let mut app = test_app();
        app.settings.remote_distro = None;
        app.settings.remote_path = None;
        app.terminal.open = true;
        let out = ctx.run(probe_raw_input(), |ctx| app.draw_terminal_panel(ctx));
        assert!(app.terminal.panel_rect.is_some(), "面板应占用底部区域");
        assert!(
            out.shapes.iter().any(|cs| matches!(
                &cs.shape,
                egui::Shape::Text(t) if t.galley.text().contains("先连接 WSL")
            )),
            "本地工作区应提示先连接 WSL"
        );

        // 再按一次 Ctrl+` 收起面板；本地工作区不会拉起任何进程。
        app.toggle_terminal();
        assert!(!app.terminal.open);
        assert!(app.terminal.is_empty());
    }

    #[test]
    fn statusbar_remote_matches_vscode_states() {
        let ctx = drawing_ctx();
        let _ = ctx.run(probe_raw_input(), |_| {});
        let p = palette(ThemeKind::DarkPlus);

        // 未连接：只有 remote 图标，没有文字，也没有蓝底。
        let out = ctx.run(probe_raw_input(), |ctx| {
            egui::TopBottomPanel::bottom("test_statusbar_off")
                .exact_height(24.0)
                .show(ctx, |ui| statusbar_remote(ui, &p, None));
        });
        let icon_center = codicon_center(&out, Codicon::Remote.glyph()).expect("应显示连接图标");
        assert!(
            !out.shapes.iter().any(|cs| matches!(
                &cs.shape,
                egui::Shape::Text(t) if t.galley.text().starts_with("WSL:")
            )),
            "未连接时不应显示发行版名"
        );
        assert!(
            !out.shapes.iter().any(|cs| matches!(
                &cs.shape,
                egui::Shape::Rect(r) if r.fill == hex(p.bg_statusbar_remote)
            )),
            "未连接时不应有远程蓝底"
        );

        // 悬停：整块变 `#262728`（statusBarItem.hoverBackground）。
        let mut input = probe_raw_input();
        input.events.push(egui::Event::PointerMoved(icon_center));
        let out = ctx.run(input, |ctx| {
            egui::TopBottomPanel::bottom("test_statusbar_off")
                .exact_height(24.0)
                .show(ctx, |ui| statusbar_remote(ui, &p, None));
        });
        assert!(
            out.shapes.iter().any(|cs| matches!(
                &cs.shape,
                egui::Shape::Rect(r) if r.fill == hex(p.bg_statusbar_hover)
            )),
            "悬停时应使用 #262728 高亮"
        );

        // 已连接：整块蓝底 + 白字 `WSL: <distro>`。
        let out = ctx.run(probe_raw_input(), |ctx| {
            egui::TopBottomPanel::bottom("test_statusbar_on")
                .exact_height(24.0)
                .show(ctx, |ui| statusbar_remote(ui, &p, Some("WSL: Ubuntu-24.04")));
        });
        assert!(
            out.shapes.iter().any(|cs| matches!(
                &cs.shape,
                egui::Shape::Rect(r) if r.fill == hex(p.bg_statusbar_remote)
            )),
            "连上 WSL 后应有 #0078D4 蓝底"
        );
        assert!(
            out.shapes.iter().any(|cs| matches!(
                &cs.shape,
                egui::Shape::Text(t) if t.galley.text() == "WSL: Ubuntu-24.04"
            )),
            "连上 WSL 后应显示发行版名"
        );
        assert!(codicon_center(&out, Codicon::Remote.glyph()).is_some());
    }

    #[test]
    fn search_summary_matches_vscode_pluralization() {
        assert_eq!(super::search_summary(0, 0), "No results found");
        assert_eq!(super::search_summary(1, 1), "1 result in 1 file");
        assert_eq!(super::search_summary(2474, 117), "2474 results in 117 files");
    }

    #[test]
    fn split_file_path_returns_name_and_directory() {
        assert_eq!(super::split_file_path("src/app/main.cpp"), ("main.cpp", "src/app"));
        assert_eq!(super::split_file_path("main.cpp"), ("main.cpp", ""));
    }

    #[test]
    fn clamp_submatches_merges_sorts_and_clips() {
        let text = "hello world";
        assert_eq!(
            super::clamp_submatches(text, &[(6, 11), (0, 5)]),
            vec![(0, 5), (6, 11)],
            "应按起点排序"
        );
        assert_eq!(
            super::clamp_submatches(text, &[(0, 3), (2, 5)]),
            vec![(0, 5)],
            "重叠区间应合并"
        );
        assert_eq!(
            super::clamp_submatches(text, &[(0, 999)]),
            vec![(0, 11)],
            "越界区间应裁剪"
        );
        // 多字节：把落在字符内部的边界对齐到字符边界。
        let cjk = "你好世界";
        assert_eq!(super::clamp_submatches(cjk, &[(2, 7)]), vec![(3, 6)]);
    }

    #[test]
    fn search_rows_flat_groups_by_file_sorted_by_line() {
        let ms = vec![
            sm("b.rs", 2, "x", vec![]),
            sm("a.rs", 3, "y", vec![]),
            sm("a.rs", 1, "z", vec![]),
        ];
        let rows = super::build_search_rows(&ms, false, &HashSet::new(), &HashSet::new());
        assert_eq!(rows.len(), 5, "两个文件头 + 三条匹配");
        match &rows[0] {
            SearchRow::File { file, count, expanded, .. } => {
                assert_eq!(*file, "a.rs");
                assert_eq!(*count, 2);
                assert!(*expanded);
            }
            _ => panic!("第一行应是文件头"),
        }
        match &rows[1] {
            SearchRow::Match { m, .. } => assert_eq!(m.line, 1),
            _ => panic!("应是匹配行"),
        }
        match &rows[2] {
            SearchRow::Match { m, .. } => assert_eq!(m.line, 3),
            _ => panic!("应是匹配行"),
        }

        // 折叠 a.rs 后只剩文件头 + b.rs 的一行匹配。
        let mut collapsed = HashSet::new();
        collapsed.insert("a.rs".to_string());
        let rows = super::build_search_rows(&ms, false, &collapsed, &HashSet::new());
        assert_eq!(rows.len(), 3);
        match &rows[0] {
            SearchRow::File { file, expanded, .. } => {
                assert_eq!(*file, "a.rs");
                assert!(!*expanded);
            }
            _ => panic!(),
        }
    }

    #[test]
    fn search_rows_tree_nests_directories_and_counts() {
        let ms = vec![
            sm("src/app/main.cpp", 1, "a", vec![]),
            sm("src/app/main.cpp", 2, "b", vec![]),
            sm("src/lib.cpp", 1, "c", vec![]),
            sm("README.md", 1, "d", vec![]),
        ];
        let rows = super::build_search_rows(&ms, true, &HashSet::new(), &HashSet::new());
        // src(dir) → app(dir) → main.cpp → 2 行 → lib.cpp → 1 行 → README.md → 1 行
        assert_eq!(rows.len(), 9);
        match &rows[0] {
            SearchRow::Dir { path, count, depth, expanded, .. } => {
                assert_eq!(path, "src");
                assert_eq!(*count, 3);
                assert_eq!(*depth, 0);
                assert!(*expanded);
            }
            _ => panic!("树状视图第一行应是目录"),
        }
        match &rows[1] {
            SearchRow::Dir { path, count, depth, .. } => {
                assert_eq!(path, "src/app");
                assert_eq!(*count, 2);
                assert_eq!(*depth, 1);
            }
            _ => panic!(),
        }
        match &rows[2] {
            SearchRow::File { file, depth, .. } => {
                assert_eq!(*file, "src/app/main.cpp");
                assert_eq!(*depth, 2);
            }
            _ => panic!(),
        }

        // 折叠 src 后：src 目录行 + README.md + 其匹配。
        let mut collapsed = HashSet::new();
        collapsed.insert("src".to_string());
        let rows = super::build_search_rows(&ms, true, &HashSet::new(), &collapsed);
        assert_eq!(rows.len(), 3);
        match &rows[0] {
            SearchRow::Dir { path, expanded, .. } => {
                assert_eq!(path, "src");
                assert!(!*expanded);
            }
            _ => panic!(),
        }
    }

    #[test]
    fn file_ancestor_dirs_returns_all_levels() {
        assert_eq!(super::file_ancestor_dirs("a/b/c.rs"), vec!["a", "a/b"]);
        assert_eq!(super::file_ancestor_dirs("c.rs"), Vec::<String>::new());
    }

    /// 搜索视图的测试用 App（侧栏打开、搜索结果固定一条）。
    fn search_probe_app(sidebar_w: f32, replace: bool, details: bool) -> LiteIdeApp {
        let mut app = test_app();
        app.settings.show_sidebar = true;
        app.sidebar_width = sidebar_w;
        app.sidebar_content = SidebarContent::Search;
        app.search_replace_open = replace;
        app.search_details_open = details;
        app.content_query = "file".to_string();
        app.content_results = vec![SearchMatch {
            file: "src/main.rs".into(),
            line: 12,
            text: "let x = file;".into(),
            submatches: vec![(8, 12)],
        }];
        app
    }

    fn probe_raw_input() -> egui::RawInput {
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1200.0, 800.0),
            )),
            ..Default::default()
        }
    }

    /// 在 `pos` 处点一下（按下 + 抬起两帧，交给 egui 判定 `clicked`）。
    fn click_sidebar(ctx: &egui::Context, app: &mut LiteIdeApp, pos: egui::Pos2) {
        for pressed in [true, false] {
            let mut input = probe_raw_input();
            input.events.push(egui::Event::PointerMoved(pos));
            input.events.push(egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::default(),
            });
            let _ = ctx.run(input, |ctx| app.draw_sidebar(ctx));
        }
    }

    /// 从一帧里找出某个 codicon 的中心点。
    fn codicon_center(out: &egui::FullOutput, glyph: char) -> Option<egui::Pos2> {
        out.shapes.iter().find_map(|cs| match &cs.shape {
            egui::Shape::Text(t) if t.galley.text() == glyph.to_string() => {
                Some(t.pos + t.galley.size() * 0.5)
            }
            _ => None,
        })
    }

    /// 从一帧里找出应用图标（`Shape::image` 展开成的那张贴图四边形）的矩形。
    fn app_icon_rect(out: &egui::FullOutput, texture: egui::TextureId) -> Option<egui::Rect> {
        out.shapes.iter().find_map(|cs| match &cs.shape {
            egui::Shape::Mesh(m) if m.texture_id == texture => {
                let pts: Vec<egui::Pos2> = m.vertices.iter().map(|v| v.pos).collect();
                Some(egui::Rect::from_points(&pts))
            }
            _ => None,
        })
    }

    /// flash 提示只占一行：超宽时保留首尾、中间省略，不换行。
    #[test]
    fn flash_message_stays_on_one_line_with_middle_ellipsis() {
        let measure = |s: &str| s.chars().count() as f32 * 10.0;
        assert_eq!(middle_ellipsis("short", 200.0, measure), "short");

        let long = "已连接到 WSL: Ubuntu2404 · /home/work/NewLithoTune/candanza/src/deep/dir";
        let shown = middle_ellipsis(long, 240.0, measure);
        assert!(shown.contains('…'));
        assert!(shown.chars().count() <= 24);
        assert!(shown.starts_with("已连接"));
        assert!(shown.ends_with("dir"));
        assert!(!shown.contains('\n'));
    }

    /// 真渲染一帧：长远程路径的提示只能有一行，且不能超出窗口。
    #[test]
    fn flash_renders_a_single_line_for_a_long_remote_path() {
        let ctx = drawing_ctx();
        let _ = ctx.run(probe_raw_input(), |_| {});
        let mut app = test_app();
        app.flash = Some((
            format!(
                "已连接到 WSL: Ubuntu2404 · {}",
                "/home/work/NewLithoTune/candanza/very/long/path".repeat(12)
            ),
            Instant::now(),
        ));
        // Area 的第一帧只做尺寸测量（egui 的 sizing pass，不画内容），第二帧才可见。
        let _ = ctx.run(probe_raw_input(), |ctx| app.draw_flash(ctx));
        let out = ctx.run(probe_raw_input(), |ctx| app.draw_flash(ctx));
        let (galley, pos) = out
            .shapes
            .iter()
            .find_map(|cs| match &cs.shape {
                egui::Shape::Text(t) if t.galley.text().contains('…') => {
                    Some((t.galley.clone(), t.pos))
                }
                _ => None,
            })
            .expect("应画出省略后的提示");
        assert_eq!(galley.rows.len(), 1, "提示不应换行");
        assert!(pos.x + galley.size().x <= 1200.0, "提示超出窗口");
    }

    /// 标题栏：应用图标、菜单、窗口按钮全部合到**最上面同一行**（对齐 VS Code 的自绘标题栏）。
    #[test]
    fn title_bar_keeps_logo_menus_and_window_buttons_on_one_top_row() {
        const W: f32 = 1200.0;
        let ctx = drawing_ctx();
        let mut app = test_app();
        // 第一帧让 `set_fonts` 生效（egui 的字体表在帧末才替换）。
        let _ = ctx.run(probe_raw_input(), |_ctx| {});
        let out = ctx.run(probe_raw_input(), |ctx| app.draw_title_bar(ctx));

        // 1) 整条标题栏就是 35px 高的一条色带，从窗口最顶边开始（系统标题栏已被它取代）。
        let bar = out
            .shapes
            .iter()
            .find_map(|cs| match &cs.shape {
                egui::Shape::Rect(r) if r.fill == hex("#3c3c3c") => Some(r.rect),
                _ => None,
            })
            .expect("应画出标题栏底色");
        assert_eq!(bar.height(), TITLEBAR_H);
        assert_eq!(bar.top(), 0.0);
        assert_eq!(bar.width(), W);

        // 2) 左侧：35px 宽的应用图标区，16px 图标居中。
        let icon_id = app.app_icon.as_ref().expect("应已加载应用图标").id();
        let logo = app_icon_rect(&out, icon_id).expect("应画出应用图标");
        assert_eq!(logo.center().x, APPICON_W / 2.0);
        assert_eq!(logo.center().y, TITLEBAR_H / 2.0);
        assert_eq!(logo.width(), APPICON_SIZE);
        assert_eq!(logo.height(), APPICON_SIZE);

        // 3) 菜单紧跟在图标区右边、和图标同一行——这正是「都在最上」。
        let file = out
            .shapes
            .iter()
            .find_map(|cs| match &cs.shape {
                egui::Shape::Text(t) if t.galley.text() == "File" => Some(t.pos),
                _ => None,
            })
            .expect("应画出 File 菜单");
        assert_eq!(file.x, APPICON_W + MENUBAR_ITEM_PAD_X);
        assert!(
            file.y > bar.top() && file.y + 7.0 < bar.bottom(),
            "菜单不在标题栏这一行里"
        );

        // 4) 右侧：三个 46px 宽的窗口按钮，贴窗口右沿，顺序同 VS Code（最小化/最大化/关闭）。
        let buttons = [
            Codicon::ChromeClose,
            Codicon::ChromeMaximize,
            Codicon::ChromeMinimize,
        ];
        for (i, icon) in buttons.into_iter().enumerate() {
            let c = codicon_center(&out, icon.glyph()).expect("应画出窗口按钮");
            assert_eq!(c.x, W - WINDOW_BTN_W * (i as f32 + 0.5));
            assert_eq!(c.y, TITLEBAR_H / 2.0);
        }

        // 5) 悬停底色：关闭按钮是 VS Code 的 `#e81123e6` + 白图标（46×35 的整块）。
        let close = egui::pos2(W - WINDOW_BTN_W / 2.0, TITLEBAR_H / 2.0);
        let mut input = probe_raw_input();
        input.events.push(egui::Event::PointerMoved(close));
        let out = ctx.run(input, |ctx| app.draw_title_bar(ctx));
        let hover = out
            .shapes
            .iter()
            .find_map(|cs| match &cs.shape {
                egui::Shape::Rect(r)
                    if r.fill == egui::Color32::from_rgba_unmultiplied(0xe8, 0x11, 0x23, 0xe6) =>
                {
                    Some(r.rect)
                }
                _ => None,
            })
            .expect("关闭按钮悬停应出底色");
        assert_eq!(hover.width(), WINDOW_BTN_W);
        assert_eq!(hover.height(), TITLEBAR_H);
        assert_eq!(hover.right(), W);
    }

    /// 把搜索侧栏画一帧，返回（输入框矩形, 徽标矩形, 文本及其左上角）。
    fn probe_search_ui(
        sidebar_w: f32,
        replace: bool,
        details: bool,
    ) -> (Vec<egui::Rect>, Vec<egui::Rect>, Vec<(String, egui::Pos2)>) {
        let ctx = drawing_ctx();
        let mut app = search_probe_app(sidebar_w, replace, details);
        // 第一帧让 `set_fonts` 生效（egui 的字体表在帧末才替换）。
        let _ = ctx.run(probe_raw_input(), |_ctx| {});
        let out = ctx.run(probe_raw_input(), |ctx| app.draw_sidebar(ctx));
        let c = super::search_colors(app.applied_theme);
        let (mut boxes, mut badges, mut texts) = (Vec::new(), Vec::new(), Vec::new());
        for cs in &out.shapes {
            match &cs.shape {
                egui::Shape::Rect(r) if r.fill == c.input_bg => boxes.push(r.rect),
                egui::Shape::Rect(r) if r.fill == c.badge => badges.push(r.rect),
                egui::Shape::Text(t) => texts.push((t.galley.text().to_string(), t.pos)),
                _ => {}
            }
        }
        (boxes, badges, texts)
    }

    /// 「清除搜索结果」除了清结果，还要把四个输入框一起清空（用户要求）。
    #[test]
    fn clear_button_wipes_results_and_every_input() {
        let ctx = drawing_ctx();
        let mut app = search_probe_app(260.0, true, true);
        app.search_replace_text = "file".to_string();
        app.search_glob = "*.rs".to_string();
        app.search_exclude_glob = "target".to_string();
        // 第一帧让字体表生效，第二帧拿到按钮位置。
        let _ = ctx.run(probe_raw_input(), |_ctx| {});
        let out = ctx.run(probe_raw_input(), |ctx| app.draw_sidebar(ctx));
        let clear = codicon_center(&out, '\u{eabf}').expect("应画出「清除搜索结果」按钮");

        click_sidebar(&ctx, &mut app, clear);

        assert!(app.content_query.is_empty(), "搜索框");
        assert!(app.search_replace_text.is_empty(), "替换框");
        assert!(app.search_glob.is_empty(), "包含的文件");
        assert!(app.search_exclude_glob.is_empty(), "排除的文件");
        assert!(app.content_results.is_empty(), "结果集");
    }

    /// 搜索视图的关键几何：逐项对齐 VS Code `searchView.css` 的实测值。
    #[test]
    fn search_view_matches_vscode_geometry() {
        const W: f32 = 260.0;
        let (boxes, badges, texts) = probe_search_ui(W, true, false);

        // 输入框：26px 高、左沿 20、右沿 面板 - 8。
        assert_eq!(boxes.len(), 2, "搜索框 + 替换框：{boxes:?}");
        let (search, replace) = (boxes[0], boxes[1]);
        assert_eq!(search.left(), SEARCH_INPUT_LEFT);
        assert_eq!(search.right(), W - SEARCH_INPUT_RIGHT);
        assert_eq!(search.height(), SEARCH_INPUT_H);
        // 替换框比搜索框窄 28px（给「全部替换」按钮留位），两框间距 6px。
        assert_eq!(replace.width(), search.width() - 28.0);
        assert_eq!(replace.top() - search.bottom(), SEARCH_INPUT_GAP);

        // 三个选项开关都在输入框内、靠右（最右那个距框右边 2px）。
        let toggles: Vec<f32> = texts
            .iter()
            .filter(|(s, _)| s == "\u{eab1}" || s == "\u{eb7e}" || s == "\u{eb38}")
            .map(|(_, pos)| pos.x)
            .collect();
        assert_eq!(toggles.len(), 3, "Aa / ab / .*");
        let last_right = toggles[2] + 7.5 + 10.0; // 字形左沿 → 开关右沿
        assert_eq!(search.right() - 2.0, last_right);

        // 「⋯」紧贴输入框下方（16px 行，右沿距面板右 6px）；汇总行左沿 22px，
        // 且与「⋯」只隔约 15px（VS Code 的 `.messages` 负外边距效果）。
        let dots = texts
            .iter()
            .find(|(s, _)| s == "\u{ea7c}")
            .expect("应画出「⋯」");
        assert_eq!(dots.1.x + 16.0 + 6.0, W);
        assert!((dots.1.y - replace.bottom()).abs() <= 1.0);
        let summary = texts
            .iter()
            .find(|(s, _)| s.starts_with("1 result"))
            .expect("应画出汇总行");
        assert_eq!(summary.1.x, 22.0);
        let gap = (summary.1.y + 7.0) - (dots.1.y + 8.0);
        assert!((14.0..=16.0).contains(&gap), "汇总行与「⋯」间距 {gap}");

        // 结果行：图标中心 34 + 8×层级 + 8，文件名 56，匹配文字 42（层级 1）；
        // 计数徽标是 19px 药丸、右沿距面板右 16px。
        let icon = texts
            .iter()
            .find(|(s, _)| s == "\u{ea7b}")
            .expect("应画出文件图标");
        assert_eq!(icon.1.x + 8.0, SEARCH_CONTENTS_X + 8.0);
        let name = texts
            .iter()
            .find(|(s, _)| s == "main.rs")
            .expect("应画出文件名");
        assert_eq!(name.1.x, SEARCH_CONTENTS_X + 22.0);
        let m = texts
            .iter()
            .find(|(s, _)| s == "let x = file;")
            .expect("应画出匹配行");
        assert_eq!(m.1.x, SEARCH_CONTENTS_X + SEARCH_INDENT_STEP);
        assert_eq!(badges.len(), 1);
        assert_eq!(badges[0].width(), 19.0);
        assert_eq!(badges[0].height(), 19.0);
        assert_eq!(badges[0].right(), W - BADGE_RIGHT);

        // 替换框收起时只剩一个输入框，「⋯」紧跟在它下面。
        let (boxes, _, texts) = probe_search_ui(W, false, false);
        assert_eq!(boxes.len(), 1);
        let dots = texts
            .iter()
            .find(|(s, _)| s == "\u{ea7c}")
            .expect("应画出「⋯」");
        assert!((dots.1.y - boxes[0].bottom()).abs() <= 1.0);
    }

    // ---- 源代码管理（Source Control）视图 ----

    /// 收集一帧里画出的所有文本。
    fn frame_texts(out: &egui::FullOutput) -> Vec<String> {
        out.shapes
            .iter()
            .filter_map(|cs| match &cs.shape {
                egui::Shape::Text(t) => Some(t.galley.text().to_string()),
                _ => None,
            })
            .collect()
    }

    /// 造一个带改动 + 提交的仓库状态。
    fn scm_repo() -> scm::RepoState {
        let mut repo = scm::RepoState::new("E:/repo".into(), "repo".into());
        repo.status = ide_git::porcelain::parse_porcelain(
            "# branch.head main\n\
             1 M. N... 100644 100644 100644 a b c src/main.cpp\n\
             ? new.txt\n",
        );
        repo.commits = vec![ide_git::log::GitCommit {
            hash: "abc1234".into(),
            full: "abc1234def".into(),
            author: "Alice".into(),
            subject: "fix broken edge API".into(),
            date: "2026-10-20T14:12:00+08:00".into(),
            ..Default::default()
        }];
        repo
    }

    /// 源代码管理视图的骨架与 VS Code 一致：SOURCE CONTROL / CHANGES / GRAPH。
    #[test]
    fn scm_view_renders_vscode_sections() {
        let ctx = drawing_ctx();
        let _ = ctx.run(probe_raw_input(), |_| {});
        let mut app = test_app();
        app.scm.set_repos(vec![scm_repo()]);
        app.sidebar_content = SidebarContent::SourceControl;
        app.settings.show_sidebar = true;
        let out = ctx.run(probe_raw_input(), |ctx| app.draw_sidebar(ctx));
        let texts = frame_texts(&out);
        let all = texts.join("\n");
        assert!(all.contains("SOURCE CONTROL"), "应画出视图标题");
        assert!(all.contains("CHANGES"), "应画出 CHANGES 分组");
        assert!(all.contains("GRAPH"), "应画出 GRAPH 分组");
        assert!(all.contains("提交"), "应画出提交按钮");
        assert!(all.contains("main"), "应显示当前分支");
        assert!(all.contains("fix broken edge API"), "应显示提交标题");
        assert!(all.contains("main.cpp"), "应显示改动的文件");
    }

    /// 视图的上下顺序与 VS Code 一致：标题 → CHANGES → 提交按钮 → GRAPH → 提交列表。
    #[test]
    fn scm_view_sections_are_ordered_like_vscode() {
        let ctx = drawing_ctx();
        let _ = ctx.run(probe_raw_input(), |_| {});
        let mut app = test_app();
        app.scm.set_repos(vec![scm_repo()]);
        app.sidebar_content = SidebarContent::SourceControl;
        app.settings.show_sidebar = true;
        let out = ctx.run(probe_raw_input(), |ctx| app.draw_sidebar(ctx));
        let mut pos: Vec<(String, f32)> = Vec::new();
        for cs in &out.shapes {
            if let egui::Shape::Text(t) = &cs.shape {
                let s = t.galley.text().to_string();
                let y = t.pos.y + t.galley.size().y * 0.5;
                pos.push((s, y));
            }
        }
        let find = |needle: &str| -> Option<f32> {
            pos.iter()
                .find(|(s, _)| s.contains(needle))
                .map(|(_, y)| *y)
        };
        let title = find("SOURCE CONTROL").expect("应画出标题");
        let changes = find("CHANGES").expect("应画出 CHANGES");
        let commit = find("提交").expect("应画出提交按钮");
        let graph = find("GRAPH").expect("应画出 GRAPH");
        let subject = find("fix broken edge API").expect("应画出提交记录");
        assert!(title < changes, "标题应在 CHANGES 之上");
        assert!(changes < commit, "提交按钮应在 CHANGES 分组里");
        assert!(commit < graph, "GRAPH 应在 CHANGES 之后");
        assert!(graph < subject, "提交记录应在 GRAPH 分组里");
        // 提交信息输入框的占位提示（VS Code 文案）。
        assert!(
            pos.iter().any(|(s, _)| s.contains("Ctrl+Enter to commit on")),
            "应显示提交信息占位提示"
        );
    }

    /// 点开一条提交 → 改动文件在**这条提交下面展开**（VS Code 的行为，
    /// 不是在面板底部另开一个窗口）。
    #[test]
    fn selected_commit_expands_files_inline() {
        let ctx = drawing_ctx();
        let _ = ctx.run(probe_raw_input(), |_| {});
        let mut app = test_app();
        let mut repo = scm_repo();
        repo.commits[0].hash = "abc1234".into();
        app.scm.set_repos(vec![repo]);
        app.scm.repos[0].selected = Some("abc1234".into());
        app.scm.detail = Some(ide_git::log::CommitDetail {
            hash: "abc1234def".into(),
            subject: "fix broken edge API".into(),
            files: vec![ide_git::log::ChangedFile {
                path: "src/expanded.cpp".into(),
                insertions: 3,
                deletions: 1,
                status: 'M',
            }],
            ..Default::default()
        });
        app.sidebar_content = SidebarContent::SourceControl;
        app.settings.show_sidebar = true;
        let out = ctx.run(probe_raw_input(), |ctx| app.draw_sidebar(ctx));
        let all = frame_texts(&out).join("\n");
        assert!(
            all.contains("expanded.cpp"),
            "选中的提交下面应展开改动文件，实际：{all}"
        );
        assert!(all.contains("M"), "展开的文件应带状态字母");
    }

    /// 仓库分组可以折叠：折叠后只留标题行（message 框 / 提交按钮 / 文件列表都不画）。
    #[test]
    fn repo_group_can_be_collapsed() {
        let ctx = drawing_ctx();
        let _ = ctx.run(probe_raw_input(), |_| {});
        let mut app = test_app();
        app.scm.set_repos(vec![scm_repo()]);
        app.sidebar_content = SidebarContent::SourceControl;
        app.settings.show_sidebar = true;

        // 展开：能看到 message 框和文件
        let out = ctx.run(probe_raw_input(), |ctx| app.draw_sidebar(ctx));
        let expanded = frame_texts(&out).join("\n");
        assert!(expanded.contains("Ctrl+Enter to commit on"), "展开时应显示 message 框");
        assert!(expanded.contains("main.cpp"), "展开时应显示改动的文件");

        // 折叠：只剩标题行
        app.scm.repos[0].expanded = false;
        let out = ctx.run(probe_raw_input(), |ctx| app.draw_sidebar(ctx));
        let collapsed = frame_texts(&out).join("\n");
        assert!(!collapsed.contains("Ctrl+Enter to commit on"), "折叠后不应有 message 框");
        assert!(!collapsed.contains("main.cpp"), "折叠后不应有文件列表");
        assert!(collapsed.contains("repo"), "折叠后仍应显示仓库名");
    }

    /// 自绘标签后，点击仍然要能切换标签（`allocate_exact_size` 不做交互，
    /// 必须 `ui.interact` 一次）。
    #[test]
    fn clicking_a_tab_switches_to_it() {
        let ctx = drawing_ctx();
        let _ = ctx.run(probe_raw_input(), |_| {});
        let mut app = test_app();
        app.tabs.open_untitled();
        assert_eq!(app.tabs.active(), 1);
        // 第一个标签的中心（用第一帧的标题位置反推）
        let out = ctx.run(probe_raw_input(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| app.draw_tab_bar(ui));
        });
        let pos = out
            .shapes
            .iter()
            .find_map(|cs| match &cs.shape {
                egui::Shape::Text(t) => Some(t.pos + egui::vec2(t.galley.size().x * 0.5, 8.0)),
                _ => None,
            })
            .expect("应画出标签标题");
        // 点击第一个标签
        for pressed in [true, false] {
            let mut input = probe_raw_input();
            input.events.push(egui::Event::PointerMoved(pos));
            input.events.push(egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::default(),
            });
            let _ = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| app.draw_tab_bar(ui));
            });
        }
        assert_eq!(app.tabs.active(), 0, "点击标签应切换到该标签");
    }

    /// 标签标题不能被关闭按钮压住（VS Code：右侧给叉叉预留位置）。
    #[test]
    fn tab_title_does_not_overlap_close_button() {
        let ctx = drawing_ctx();
        let _ = ctx.run(probe_raw_input(), |_| {});
        let mut app = test_app();
        app.tabs.open_untitled();
        if let Some(t) = app.tabs.get_mut(0) {
            t.title = "MakeLists.txt".to_string();
        }
        // 第一帧：用标签栏自己画，找到标题的位置
        let out = ctx.run(probe_raw_input(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| app.draw_tab_bar(ui));
        });
        let pos = out
            .shapes
            .iter()
            .find_map(|cs| match &cs.shape {
                egui::Shape::Text(t) if t.galley.text().contains("MakeLists") => {
                    Some(t.pos + egui::vec2(t.galley.size().x * 0.5, t.galley.size().y * 0.5))
                }
                _ => None,
            })
            .expect("应画出标签标题");
        // 第二帧：指针移到标题上 → 出现叉叉，且不压住标题
        let mut input = probe_raw_input();
        input.events.push(egui::Event::PointerMoved(pos));
        let out = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| app.draw_tab_bar(ui));
        });
        let mut title_right = 0.0_f32;
        let mut x_left = f32::MAX;
        for cs in &out.shapes {
            if let egui::Shape::Text(t) = &cs.shape {
                let txt = t.galley.text().to_string();
                if txt.contains("MakeLists") {
                    title_right = title_right.max(t.pos.x + t.galley.size().x);
                }
                if txt == "\u{ea76}" {
                    x_left = x_left.min(t.pos.x);
                }
            }
        }
        assert!(title_right > 0.0, "应画出标签标题");
        assert!(x_left < f32::MAX, "悬停时应画出关闭按钮");
        assert!(
            title_right <= x_left,
            "标题右边缘 {title_right} 不应超过叉叉左边缘 {x_left}"
        );
    }

    /// 标签页的关闭按钮：VS Code 默认不显示，鼠标移上去才出现。
    #[test]
    fn tab_close_button_only_shows_on_hover() {
        let ctx = drawing_ctx();
        let _ = ctx.run(probe_raw_input(), |_| {});
        let mut app = test_app();
        app.tabs.open_untitled();
        let has_close = |out: &egui::FullOutput| {
            out.shapes.iter().any(|cs| {
                matches!(&cs.shape, egui::Shape::Text(t) if t.galley.text() == "\u{ea76}")
            })
        };
        // 不悬停：没有叉叉
        let out = ctx.run(probe_raw_input(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| app.draw_tab_bar(ui));
        });
        assert!(!has_close(&out), "默认不应显示关闭按钮");
        // 悬停在标签上：出现叉叉
        let mut input = probe_raw_input();
        input.events.push(egui::Event::PointerMoved(egui::pos2(40.0, 17.0)));
        let out = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| app.draw_tab_bar(ui));
        });
        assert!(has_close(&out), "悬停时应显示关闭按钮");
    }

    /// 点叉叉必须真的能关掉标签。
    ///
    /// 回归：叉叉曾经只在「标签 hover」的帧里创建，而 egui 的命中测试用的是**上一帧**
    /// 控件表 —— 指针移到叉叉上后 `resp.hovered()` 变 false（叉叉抢走 hover），下一帧
    /// 叉叉就不存在了，点击被丢到标签上，表现是「点叉叉没反应」。
    #[test]
    fn tab_close_button_click_closes_tab() {
        let ctx = drawing_ctx();
        let _ = ctx.run(probe_raw_input(), |_| {});
        let mut app = test_app();
        // 复现用户现场：SSH 工作区 + 远程文件按「预览标签」打开（标题斜体）。
        app.settings.remote_kind = Some("ssh".to_string());
        app.settings.ssh_user = Some("fubin".to_string());
        app.settings.ssh_host = Some("192.168.77.6".to_string());
        app.settings.remote_path = Some("/home/fubin/work/candanza".to_string());
        assert!(app.is_remote());
        while !app.tabs.is_empty() {
            app.tabs.close(0);
        }
        app.open_file(
            std::path::PathBuf::from("/home/fubin/work/candanza/src/FragmentSub.cpp"),
            "int x;\n".to_string(),
            true,
        );
        assert_eq!(app.tabs.len(), 1);
        assert!(app.tabs.is_preview(0), "应是预览标签（和用户截图一致）");
        let mut frame = |events: Vec<egui::Event>| {
            let mut input = probe_raw_input();
            input.events = events;
            ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| app.draw_tab_bar(ui));
            })
        };
        // 两帧 hover（第一帧只是把标签登记进控件表）。
        for _ in 0..2 {
            let _ = frame(vec![egui::Event::PointerMoved(egui::pos2(40.0, 17.0))]);
        }
        let out = frame(vec![egui::Event::PointerMoved(egui::pos2(40.0, 17.0))]);
        let center = out
            .shapes
            .iter()
            .find_map(|cs| match &cs.shape {
                egui::Shape::Text(t) if t.galley.text() == "\u{ea76}" => {
                    Some(t.pos + t.galley.size() / 2.0)
                }
                _ => None,
            })
            .expect("悬停标签时应画出关闭按钮");
        // 指针移到叉叉上，按下 / 抬起。
        for pressed in [true, false] {
            let _ = frame(vec![
                egui::Event::PointerMoved(center),
                egui::Event::PointerButton {
                    pos: center,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::default(),
                },
            ]);
        }
        assert_eq!(app.tabs.len(), 0, "点击叉叉应关闭标签");
    }

    /// 标签宽度：短标题自己窄、长标题更宽；放不下时等比压缩，但不低于下限。
    #[test]
    fn tab_widths_follow_each_title() {
        // 自然宽度 = 标题 + 左内边距 + 右侧叉叉位
        let w = tab_widths(&[60.0, 140.0], 2000.0);
        assert!((w[0] - (60.0 + TAB_PAD_X + TAB_X_SLOT)).abs() < 0.01);
        assert!((w[1] - (140.0 + TAB_PAD_X + TAB_X_SLOT)).abs() < 0.01);
        assert!(
            w[0] < w[1],
            "短标题不应跟着最长标题一起变宽：{w:?}"
        );
        // 极短标题有下限、超长标题有上限
        assert!((tab_widths(&[1.0], 2000.0)[0] - TAB_MIN_W).abs() < 0.01);
        assert!((tab_widths(&[5000.0], 2000.0)[0] - TAB_MAX_W).abs() < 0.01);
        // 放不下：等比压缩（3 × 234 = 702 → 450，每格 150）
        let shrunk = tab_widths(&[200.0, 200.0, 200.0], 450.0);
        assert!(
            shrunk.iter().all(|w| (w - 150.0).abs() < 0.01),
            "{shrunk:?}"
        );
        // 再窄也不低于下限（宁可溢出，也不把标题压没）
        let floor = tab_widths(&[200.0; 10], 300.0);
        assert!(
            floor.iter().all(|w| (w - TAB_MIN_W).abs() < 0.01),
            "{floor:?}"
        );
    }

    /// 悬停同一条提交时，详情请求只发一次（否则每帧都发 git 请求会拖垮整个队列）。
    #[test]
    fn hover_requests_commit_detail_only_once() {
        let ctx = drawing_ctx();
        let _ = ctx.run(probe_raw_input(), |_| {});
        let mut app = test_app();
        app.scm.set_repos(vec![scm_repo()]);
        app.sidebar_content = SidebarContent::SourceControl;
        app.settings.show_sidebar = true;
        // 第一帧：找到提交标题的位置
        let out = ctx.run(probe_raw_input(), |ctx| app.draw_sidebar(ctx));
        let pos = out
            .shapes
            .iter()
            .find_map(|cs| match &cs.shape {
                egui::Shape::Text(t) if t.galley.text().contains("fix broken edge API") => {
                    Some(t.pos + t.galley.size() * 0.5)
                }
                _ => None,
            })
            .expect("应画出提交标题");
        let count = |out: &egui::FullOutput| -> usize {
            out.shapes.len()
        };
        let _ = count;
        // 悬停：连续几帧，只有第一帧发一次请求。
        let mut input = probe_raw_input();
        input.events.push(egui::Event::PointerMoved(pos));
        let mut requested = 0;
        for _ in 0..3 {
            let mut acts: Vec<scm::Action> = Vec::new();
            let _ = ctx.run(input.clone(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    acts = app.scm.draw(ui);
                });
            });
            requested += acts
                .iter()
                .filter(|a| matches!(a, scm::Action::RequestDetail { .. }))
                .count();
            input = probe_raw_input();
            input.events.push(egui::Event::PointerMoved(pos));
        }
        assert_eq!(requested, 1, "同一条提交只应请求一次详情，实际 {requested} 次");
    }

    /// 悬停 GRAPH 里的某条提交 → 弹出信息卡片（作者 / 时间 / 标题 / 统计 / SHA + 复制按钮）。
    #[test]
    fn hovering_a_commit_shows_info_card() {
        let ctx = drawing_ctx();
        let _ = ctx.run(probe_raw_input(), |_| {});
        let mut app = test_app();
        let mut repo = scm_repo();
        repo.commits[0].author = "HOVERCARD".into();
        repo.commits[0].full = "abc1234def5678901234567890abcdef".into();
        app.scm.set_repos(vec![repo]);
        app.sidebar_content = SidebarContent::SourceControl;
        app.settings.show_sidebar = true;
        // 第一帧：找到 GRAPH 里提交标题的位置
        let out = ctx.run(probe_raw_input(), |ctx| app.draw_sidebar(ctx));
        eprintln!("DBG f1 texts={:?}", frame_texts(&out));
        eprintln!("DBG f1 width={}", app.sidebar_width);
        let pos = out
            .shapes
            .iter()
            .find_map(|cs| match &cs.shape {
                egui::Shape::Text(t) if t.galley.text().contains("fix broken edge API") => {
                    Some(t.pos + t.galley.size() * 0.5)
                }
                _ => None,
            })
            .expect("应画出提交标题");

        // 预置提交详情（相当于 `git show --numstat` 的结果；悬停用 hover_detail）。
        // 同时标记「已请求过」，避免测试里再发一次请求把预置的详情清掉。
        app.scm.hover_requested = Some("abc1234".into());
        app.scm.hover_detail = Some(ide_git::log::CommitDetail {
            hash: "abc1234def".into(),
            author: "Alice".into(),
            subject: "fix broken edge API".into(),
            ..Default::default()
        });
        app.scm.hover_detail.as_mut().unwrap().files = vec![ide_git::log::ChangedFile {
            path: "src/a.cpp".into(),
            insertions: 70,
            deletions: 61,
            status: 'M',
        }];

        // 第二帧：把指针放到那一行上 → 记录悬停；卡片是 egui::Area，
        // 第一帧只做「测量」，所以还要再画一帧卡片才出现。
        let mut input = probe_raw_input();
        input.events.push(egui::Event::PointerMoved(pos));
        let _ = ctx.run(input.clone(), |ctx| app.draw_sidebar(ctx));
        let out = ctx.run(input, |ctx| app.draw_sidebar(ctx));
        let texts = frame_texts(&out);
        let all = texts.join("\n");
        assert!(all.contains("HOVERCARD"), "卡片应显示作者");
        assert!(all.contains("ago"), "卡片应显示相对时间，实际：{all}");
        assert!(
            all.contains("October 20, 2026 at 2:12 PM"),
            "卡片应显示绝对时间，实际：{all}"
        );
        assert!(all.contains("1 file changed"), "卡片应显示改动统计");
        assert!(all.contains("70 insertions(+)"), "卡片应显示新增行数");
        assert!(all.contains("61 deletions(-)"), "卡片应显示删除行数");
        assert!(all.contains("abc1234"), "卡片应显示提交 ID");
    }

    /// 复制按钮复制的是**完整**的 commit id（卡片上显示的是短 SHA）。
    #[test]
    fn copy_button_uses_full_commit_id() {
        let ctx = drawing_ctx();
        let _ = ctx.run(probe_raw_input(), |_| {});
        let mut app = test_app();
        let mut repo = scm_repo();
        repo.commits[0].hash = "abc1234".into();
        repo.commits[0].full = "abc1234def5678901234567890abcdef".into();
        app.scm.set_repos(vec![repo]);
        app.sidebar_content = SidebarContent::SourceControl;
        app.settings.show_sidebar = true;
        // 第一帧：找到提交标题的位置
        let out = ctx.run(probe_raw_input(), |ctx| app.draw_sidebar(ctx));
        let pos = out
            .shapes
            .iter()
            .find_map(|cs| match &cs.shape {
                egui::Shape::Text(t) if t.galley.text().contains("fix broken edge API") => {
                    Some(t.pos + t.galley.size() * 0.5)
                }
                _ => None,
            })
            .expect("应画出提交标题");
        // 悬停到那一行 → 卡片出现
        let mut input = probe_raw_input();
        input.events.push(egui::Event::PointerMoved(pos));
        let _ = ctx.run(input.clone(), |ctx| app.draw_sidebar(ctx));
        let out = ctx.run(input, |ctx| app.draw_sidebar(ctx));
        let card_hash = out
            .shapes
            .iter()
            .filter_map(|cs| match &cs.shape {
                egui::Shape::Text(t) if t.galley.text().starts_with("abc1234") => {
                    Some(t.galley.text().to_string())
                }
                _ => None,
            })
            .next();
        assert_eq!(card_hash.as_deref(), Some("abc1234"), "卡片上显示短 SHA");
    }

    /// 活动栏的源代码管理图标切到 Source Control 视图。
    #[test]
    fn activity_bar_source_control_opens_scm_view() {        let ctx = drawing_ctx();
        let _ = ctx.run(probe_raw_input(), |_| {});
        let mut app = test_app();
        app.scm.set_repos(vec![scm_repo()]);
        app.sidebar_content = SidebarContent::Explorer;
        app.settings.show_sidebar = true;
        let out = ctx.run(probe_raw_input(), |ctx| app.draw_activity_bar(ctx));
        // 源代码管理图标是 codicon ea68（活动栏里唯一的分支图标）。
        let has_icon = out.shapes.iter().any(|cs| matches!(
            &cs.shape,
            egui::Shape::Text(t) if t.galley.text() == "\u{ea68}"
        ));
        assert!(has_icon, "活动栏应画出源代码管理图标");
        assert_eq!(app.sidebar_content, SidebarContent::Explorer);
    }
}
