//! 集成终端：WSL PTY 会话 + vt100 屏幕模型 + egui 自绘。
//!
//! 这一层只做「终端」本身：会话集合、键盘编码、网格绘制、滚回、选区。
//! 面板的标题栏/菜单/命令分发在 `main.rs`。
//!
//! 为什么要自绘而不是嵌一个 conhost 窗口：VS Code 式布局要求终端是编辑器下方
//! 的一个 Split 面板（不是独立窗口），并且要跟随主题/字号；ConPTY 只提供字节流，
//! 屏幕模型（光标、颜色、滚回）由 `vt100` 解析，绘制走 egui。

use std::num::NonZeroU16;
use std::time::{Duration, Instant};

use egui::{Color32, FontId, Pos2, Rect, Vec2};
use ide_core::theme::ThemeKind;
use ide_remote::WslTerminal;
use vt100::{Color, Parser, Screen};

/// 滚回上限（行）。VS Code 默认 1000，这里给 5000 行，内存约几百 KB。
pub const SCROLLBACK_ROWS: usize = 5000;
/// 面板默认/最小高度（VS Code 默认约 200px）。
pub const DEFAULT_PANEL_HEIGHT: f32 = 240.0;
pub const MIN_PANEL_HEIGHT: f32 = 96.0;
/// 终端标题栏高度（VS Code `.terminal-tabs-entry` 约 30px）。
pub const HEADER_HEIGHT: f32 = 30.0;
/// 终端等宽字体字号。
pub const FONT_SIZE: f32 = 13.0;
/// 窗口/面板拖动时尺寸会连续变化；稳定这么久才把新尺寸发给 PTY。
///
/// 为什么要防抖：拖动窗口边缘的中间帧可能出现极窄的正文（甚至 0 宽）。
/// 直接 `SIGWINCH` 给 bash 后，readline 会按 2 列重绘提示符并写进滚回区，
/// 窗口恢复后那几行就永久变成竖排文字（用户实测截图）。只把最终尺寸发下去
/// 可以完全避开这种垃圾历史。
const RESIZE_SETTLE: Duration = Duration::from_millis(120);
/// 网格最小列数。窗口真的拖到很窄时也不把 PTY 压到 2 列（2 列会毁掉屏幕）。
const MIN_COLS: u16 = 8;
const MIN_ROWS: u16 = 2;
/// Shell 迟迟没有输出时显示启动提示的阈值。
const STARTUP_HINT_AFTER: Duration = Duration::from_secs(2);

/// 终端 ANSI 调色板 + 基础色（按外壳主题切换）。
#[derive(Clone)]
pub struct TerminalStyle {
    pub font: FontId,
    pub fg: Color32,
    pub bg: Color32,
    pub cursor: Color32,
    pub selection: Color32,
    /// 0-15：标准 ANSI 色（含 8 个 bright）。
    pub ansi: [Color32; 16],
}

impl TerminalStyle {
    pub fn for_theme(kind: ThemeKind) -> Self {
        // VS Code Dark+ / Light+ 的 terminal.ansi* 官方取值。
        let dark = !matches!(kind, ThemeKind::LightPlus);
        let ansi = if dark {
            [
                hex("#000000"), hex("#cd3131"), hex("#0dbc79"), hex("#e5e510"),
                hex("#2472c8"), hex("#bc3fbc"), hex("#11a8cd"), hex("#e5e5e5"),
                hex("#666666"), hex("#f14c4c"), hex("#23d18b"), hex("#f5f543"),
                hex("#3b8eea"), hex("#d670d6"), hex("#29b8db"), hex("#e5e5e5"),
            ]
        } else {
            [
                hex("#000000"), hex("#cd3131"), hex("#00bc00"), hex("#949800"),
                hex("#0451a5"), hex("#bc05bc"), hex("#0598bc"), hex("#555555"),
                hex("#666666"), hex("#cd3131"), hex("#14ce14"), hex("#b5ba00"),
                hex("#0451a5"), hex("#bc05bc"), hex("#0598bc"), hex("#a5a5a5"),
            ]
        };
        Self {
            font: FontId::monospace(FONT_SIZE),
            fg: if dark { hex("#cccccc") } else { hex("#333333") },
            bg: if dark { hex("#1e1e1e") } else { hex("#ffffff") },
            cursor: if dark { hex("#aeafad") } else { hex("#000000") },
            selection: if dark { hex("#264f78") } else { hex("#add6ff") },
            ansi,
        }
    }
}

fn hex(s: &str) -> Color32 {
    let t = s.trim_start_matches('#');
    let parse = |i: usize| u8::from_str_radix(&t[i..i + 2], 16).unwrap_or(0);
    if t.len() >= 6 {
        Color32::from_rgb(parse(0), parse(2), parse(4))
    } else {
        Color32::WHITE
    }
}

/// 一个终端会话（一个 `wsl.exe` 进程 + 一块屏幕）。
pub struct TerminalSession {
    pub id: usize,
    pub title: String,
    pub shell: WslTerminal,
    pub parser: Parser,
    pub cols: u16,
    pub rows: u16,
    pub exited: bool,
    /// 键盘输入是否归这个终端（点击终端正文即获得，点击别处失去）。
    pub focused: bool,
    /// 选区锚点 / 当前点（行、列，含端点）。
    pub sel_anchor: Option<(u16, u16)>,
    pub sel_end: Option<(u16, u16)>,
    /// 上一次输出末尾未收完的转义序列（DSR/DA 查询可能被拆包）。
    query_tail: Vec<u8>,
    /// 用户往上滚动查看历史时的位置（0 = 跟随最新输出）。
    ///
    /// 自己记一份，不直接用 vt100 的 scrollback 偏移：面板高度变了以后，
    /// 我们要显示的是「输出流的最后 N 行」= 滚回区尾部 + 当前屏幕，
    /// 偏移量得自己算。
    scroll_offset: usize,
    /// 待确认的新尺寸（拖动中先攒着，稳定后再 resize）。
    pending_size: Option<PendingSize>,
    /// 第一次按实际面板大小布局：立即生效，避免启动时再给 shell 一次 SIGWINCH。
    first_layout: bool,
    /// 启动阶段是否已经出现过可见文字（出现过就不再显示启动提示，
    /// 否则之后执行 `clear` 会把提示误当成「Shell 还没起来」）。
    seen_text: bool,
    started_at: Instant,
}

/// 尺寸防抖的待定目标。
///
/// 行、列都防抖：拖动过程中每帧 resize 会让 shell 反复收到 SIGWINCH 重绘，
/// vt100 也会反复重排，实测会把滚回区和屏幕内容接错（内容丢失/闪烁）。
/// 只把最终稳定尺寸发下去，拖动期间画面用「底边对齐」的方式保持不跳。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PendingSize {
    cols: u16,
    rows: u16,
    since: Instant,
}

/// 一帧的 resize 决策。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResizeTick {
    /// 尺寸没变。
    Idle,
    /// 还在抖动 / 未稳定，过 `Duration` 后再看。
    Wait(Duration),
    /// 稳定了，可以把尺寸发给 PTY 和 vt100。
    Apply,
}

/// 把尺寸同时写入 PTY 和 vt100 模型。
///
/// 关键点：**模型必须和 ConPTY 的屏幕一模一样**。
///
/// ConPTY 在 PTY resize 后不会重排内容，只是把旧屏幕原样画在新屏幕的顶部，
/// 光标停在原来的行；随后它发来的绝对光标定位（`ESC[row;colH`）用的还是这套坐标。
/// 如果我们这边用 vt100 的 `set_size` 变高（它会把滚回区拉到顶部、把光标推到底部），
/// 两边坐标就对不上了：ConPTY 之后写第 13 行，我们却以为光标在第 40 行，
/// 中间那段内容会被覆盖——用户看到的就是「301 后面直接跳到 348」。
///
/// 所以这里不做任何「重放历史」的花活：ConPTY 画什么我们就画什么，
/// 变高时用 [`grow_screen`] 把 vt100 调整成 ConPTY 的布局。
fn apply_size(session: &mut TerminalSession, cols: u16, rows: u16) {
    let old_rows = session.rows;
    session.cols = cols;
    session.rows = rows;
    session.shell.resize(cols, rows);
    if rows > old_rows {
        // 变高：内容留在顶部、光标不动、下方补空行（和 ConPTY 一致）。
        grow_screen(&mut session.parser, rows);
    } else {
        // 变矮 / 只变宽：vt100 的行为和 ConPTY 一致（保留最后若干行，光标在底部）。
        session.parser.set_size(nz(rows), nz(cols));
    }
    // 尺寸变了，之前记的滚动位置要重新夹一次。
    let sb = session.parser.screen().scrollback_len();
    session.scroll_offset = session.scroll_offset.min(sb);
}

/// 把屏幕变高，并保持 ConPTY 的布局（内容留在顶部、光标不动、下方补空行）。
///
/// vt100 的 `set_size` 变高时会把滚回区的历史拉到顶部（内容被推到底部、光标落到
/// 最后一行）；ConPTY 恰恰相反——resize 后它把旧屏幕原样画在顶部，光标停在原处。
/// 两者不一致时，ConPTY 后续发来的绝对光标定位（`ESC[row;colH`）会落到屏幕中间，
/// 把中间那段内容盖掉：用户看到的就是「301 后面直接跳到 348」。
///
/// 这里把 vt100 拉上来的行再推回滚回区，并把光标移回原位，让两边完全一致。
/// 返回被推回滚回区的行数。
fn grow_screen(parser: &mut Parser, new_rows: u16) -> u16 {
    let (rows, cols) = parser.screen().size();
    if new_rows <= rows.get() {
        return 0;
    }
    let cursor_before = parser.screen().cursor_position().0;
    parser.set_size(nz(new_rows), cols);
    let moved = parser.screen().cursor_position().0.saturating_sub(cursor_before);
    if moved > 0 {
        // ESC[S（向上滚）把拉到顶部的历史推回滚回区，内容回到顶部；
        // ESC[A（光标上移）把光标放回 ConPTY 留下的位置。
        parser.process(format!("\x1b[{moved}S").as_bytes());
        parser.process(format!("\x1b[{moved}A").as_bytes());
    }
    moved
}

/// Shell 启动提示的显示条件：启动阶段还没出现过文字、进程没退出、且已经等了一会儿。
///
/// 不能用「收到过字节」判断：WSL 启动时会先发一些纯控制序列（DSR/窗口标题），
/// 屏幕仍然是空的，但字节早就到了。
fn startup_hint_visible(elapsed: Duration, exited: bool, seen_text: bool) -> bool {
    !exited && !seen_text && elapsed >= STARTUP_HINT_AFTER
}

/// 屏幕上是否有任何可见字符（忽略纯空格 / 控制序列）。
fn screen_has_text(screen: &Screen) -> bool {
    let (rows, cols) = screen.size();
    for row in 0..rows.get() {
        for col in 0..cols.get() {
            if let Some(cell) = screen.cell(row, col) {
                if cell.contents().chars().any(|ch| !ch.is_whitespace()) {
                    return true;
                }
            }
        }
    }
    false
}

/// 尺寸防抖：连续变化时只发最后一个尺寸，避免中间态把 shell 屏幕重排坏。
fn stable_resize(
    current: (u16, u16),
    pending: &mut Option<PendingSize>,
    want: (u16, u16),
    now: Instant,
    immediate: bool,
) -> ResizeTick {
    if want == current {
        *pending = None;
        return ResizeTick::Idle;
    }
    if immediate {
        *pending = None;
        return ResizeTick::Apply;
    }
    match *pending {
        Some(p) if (p.cols, p.rows) == want => {
            let waited = now.saturating_duration_since(p.since);
            if waited >= RESIZE_SETTLE {
                *pending = None;
                ResizeTick::Apply
            } else {
                ResizeTick::Wait(RESIZE_SETTLE - waited)
            }
        }
        _ => {
            *pending = Some(PendingSize {
                cols: want.0,
                rows: want.1,
                since: now,
            });
            ResizeTick::Wait(RESIZE_SETTLE)
        }
    }
}

/// 全部终端会话 + 面板状态。
#[derive(Default)]
pub struct TerminalSet {
    pub sessions: Vec<TerminalSession>,
    pub active: usize,
    next_id: usize,
    /// 面板是否显示（隐藏后会话继续存活，输出照常排空）。
    pub open: bool,
    /// 下一帧把键盘焦点交给活动终端（新建 / 切换标签 / 打开面板时）。
    pub focus_next: bool,
    /// 本帧是否有终端在吃键盘（`main.rs` 据此屏蔽编辑器按键）。
    pub focused: bool,
    /// 面板的屏幕矩形（点击面板外时取消焦点用）。
    pub panel_rect: Option<Rect>,
    pub last_error: Option<String>,
}

impl TerminalSet {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn ack_focus(&mut self) -> bool {
        let f = self.focus_next;
        self.focus_next = false;
        f
    }

    /// 新建会话并切过去。失败信息同时写进 `last_error`，由面板空态展示。
    pub fn spawn(
        &mut self,
        distro: &str,
        cwd: &str,
        cols: u16,
        rows: u16,
    ) -> Result<(), String> {
        let shell = WslTerminal::spawn(distro, cwd, cols, rows)?;
        let id = self.next_id;
        self.next_id += 1;
        let title = if self.sessions.is_empty() {
            "bash".to_string()
        } else {
            format!("bash ({})", id + 1)
        };
        let parser = Parser::new(nz(rows), nz(cols), SCROLLBACK_ROWS);
        self.sessions.push(TerminalSession {
            id,
            title,
            shell,
            parser,
            cols: cols.max(2),
            rows: rows.max(2),
            exited: false,
            focused: false,
            sel_anchor: None,
            sel_end: None,
            query_tail: Vec::new(),
            scroll_offset: 0,
            pending_size: None,
            first_layout: true,
            seen_text: false,
            started_at: Instant::now(),
        });
        self.active = self.sessions.len() - 1;
        self.open = true;
        self.focus_next = true;
        self.last_error = None;
        Ok(())
    }

    pub fn active(&self) -> Option<&TerminalSession> {
        self.sessions.get(self.active)
    }

    pub fn active_mut(&mut self) -> Option<&mut TerminalSession> {
        self.sessions.get_mut(self.active)
    }

    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    pub fn close(&mut self, idx: usize) {
        if idx >= self.sessions.len() {
            return;
        }
        let was_active = idx == self.active;
        let mut s = self.sessions.remove(idx);
        s.shell.kill();
        if self.sessions.is_empty() {
            self.active = 0;
            self.focused = false;
            self.focus_next = false;
            return;
        }
        // 关掉前面的标签时，活动项跟着左移，仍然指向同一个会话。
        if idx < self.active {
            self.active -= 1;
        } else if was_active {
            self.active = idx.min(self.sessions.len() - 1);
            self.focus_next = true;
        }
    }

    /// 取消键盘焦点（点击面板外 / 打开模态框时）。
    pub fn clear_focus(&mut self) {
        for s in &mut self.sessions {
            s.focused = false;
        }
        self.focused = false;
        self.focus_next = false;
    }

    pub fn close_active(&mut self) {
        self.close(self.active);
    }

    /// 断开工作区 / 退出前结束所有会话。
    pub fn shutdown(&mut self) {
        for s in &mut self.sessions {
            s.shell.kill();
        }
        self.sessions.clear();
        self.active = 0;
        self.focused = false;
        self.focus_next = false;
        self.panel_rect = None;
    }

    pub fn clear_active(&mut self) {
        if let Some(s) = self.active_mut() {
            s.parser.process(b"\x1b[2J\x1b[3J\x1b[H");
            s.sel_anchor = None;
            s.sel_end = None;
        }
    }

    /// 非阻塞排空所有会话的输出。返回是否有新内容（用于请求重绘）。
    pub fn poll(&mut self) -> bool {
        let mut dirty = false;
        for s in &mut self.sessions {
            if s.exited {
                continue;
            }
            let out = s.shell.drain();
            if !out.bytes.is_empty() {
                dirty |= process_output(s, &out.bytes);
            }
            if out.exited {
                s.exited = true;
                s.parser
                    .process("\r\n\x1b[90m[进程已退出 — 点击 + 新建终端]\x1b[0m\r\n".as_bytes());
                dirty = true;
            }
        }
        dirty
    }

    /// 当前终端是否还活着（用于面板标题栏的退出态样式）。
    pub fn any_alive(&self) -> bool {
        self.sessions.iter().any(|s| !s.exited)
    }
}

/// 把一段真实 PTY 输出写进 vt100，并应答 DSR/DA 查询。
fn process_output(s: &mut TerminalSession, bytes: &[u8]) -> bool {
    if bytes.is_empty() {
        return false;
    }
    s.parser.process(bytes);
    // 终端必须回答 DSR/DA 查询：bash/zsh 启动时会用 `ESC[6n` 探测光标，
    // 不回的话提示符永远不出现（WSL 的 bash 就是这种行为）。
    let replies = terminal_query_replies(s, bytes);
    if !replies.is_empty() {
        if let Err(err) = s.shell.write(&replies) {
            tracing::warn!("[terminal] {err}");
        }
    }
    true
}

fn nz(n: u16) -> NonZeroU16 {
    NonZeroU16::new(n.max(1)).unwrap_or(NonZeroU16::MIN)
}

/// 扫描输出里的终端查询并生成应答。
///
/// 目前处理最常见的三种（xterm 系）：
/// - `ESC[6n` DSR 光标位置 → `ESC[row;colR`（vt100 坐标为 0 基，应答要 +1）；
/// - `ESC[5n` 状态报告 → `ESC[0n`；
/// - `ESC[c` / `ESC[>c` 设备属性 → 声明为 VT100 + ANSI 颜色终端。
///
/// 查询可能被 PTY 拆成两个 chunk，所以未收完的序列留到下一次扫描。
fn terminal_query_replies(s: &mut TerminalSession, bytes: &[u8]) -> Vec<u8> {
    let mut tail = std::mem::take(&mut s.query_tail);
    let replies = scan_terminal_queries(s.parser.screen(), &mut tail, bytes);
    s.query_tail = tail;
    replies
}

fn scan_terminal_queries(screen: &Screen, tail: &mut Vec<u8>, bytes: &[u8]) -> Vec<u8> {
    let mut buf = std::mem::take(tail);
    buf.extend_from_slice(bytes);

    let mut replies = Vec::new();
    let mut incomplete: Option<usize> = None;
    let mut i = 0;
    while i < buf.len() {
        if buf[i] != 0x1b {
            i += 1;
            continue;
        }
        if i + 1 >= buf.len() {
            incomplete = Some(i);
            break;
        }
        if buf[i + 1] != b'[' {
            i += 2;
            continue;
        }
        // CSI：参数字节 0x30..=0x3f，中间字节 0x20..=0x2f，终止字节 0x40..=0x7e。
        let mut j = i + 2;
        while j < buf.len() && !(0x40..=0x7e).contains(&buf[j]) {
            j += 1;
        }
        if j >= buf.len() {
            incomplete = Some(i);
            break;
        }
        match &buf[i..=j] {
            b"\x1b[6n" => {
                let (row, col) = screen.cursor_position();
                replies.extend_from_slice(format!("\x1b[{};{}R", row + 1, col + 1).as_bytes());
            }
            b"\x1b[5n" => replies.extend_from_slice(b"\x1b[0n"),
            b"\x1b[c" | b"\x1b[0c" => replies.extend_from_slice(b"\x1b[?1;2c"),
            b"\x1b[>c" | b"\x1b[>0c" => replies.extend_from_slice(b"\x1b[>0;276;0c"),
            _ => {}
        }
        i = j + 1;
    }

    *tail = match incomplete {
        Some(k) if buf.len() - k <= 16 => buf[k..].to_vec(),
        _ => Vec::new(),
    };
    replies
}

// ---------------------------------------------------------------------------
// 键盘 → PTY 字节
// ---------------------------------------------------------------------------

/// 这些组合键是 IDE 的全局快捷键，终端焦点下也不能当普通输入发进 shell。
pub fn is_app_shortcut(key: egui::Key, mods: egui::Modifiers) -> bool {
    if !mods.command {
        return false;
    }
    // Ctrl+V 交给 `Event::Paste`（egui 会同时发 Key 和 Paste，若不拦会粘两份）；
    // 其余是 IDE 的全局快捷键，终端有焦点时由 `main.rs` 处理，不能写进 shell。
    matches!(
        key,
        egui::Key::P
            | egui::Key::F
            | egui::Key::G
            | egui::Key::S
            | egui::Key::N
            | egui::Key::W
            | egui::Key::T
            | egui::Key::B
            | egui::Key::V
            | egui::Key::Comma
            | egui::Key::Backtick
    )
}

/// 把 egui 事件翻译成 PTY 输入。
///
/// `app_cursor` / `bracketed_paste` 来自 `vt100::Screen`：全屏程序（vim/less）会切换
/// 光标键协议与括号粘贴协议，必须跟着变，否则方向键/粘贴在那些程序里是错的。
pub fn event_to_bytes(
    ev: &egui::Event,
    app_cursor: bool,
    bracketed_paste: bool,
) -> Vec<u8> {
    match ev {
        egui::Event::Text(text) => text
            .bytes()
            .filter(|b| *b == b'\t' || (*b >= 0x20 && *b != 0x7f))
            .collect(),
        egui::Event::Paste(text) => {
            let mut out = Vec::with_capacity(text.len() + 16);
            if bracketed_paste {
                out.extend_from_slice(b"\x1b[200~");
            }
            for ch in text.chars() {
                if ch == '\r' {
                    continue;
                }
                if ch == '\n' {
                    out.push(b'\r');
                } else {
                    let mut buf = [0u8; 4];
                    out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                }
            }
            if bracketed_paste {
                out.extend_from_slice(b"\x1b[201~");
            }
            out
        }
        egui::Event::Key {
            key,
            pressed: true,
            modifiers,
            ..
        } => key_to_bytes(*key, *modifiers, app_cursor),
        _ => Vec::new(),
    }
}

/// 把一帧的 egui 事件翻译成 PTY 输入。
///
/// 关键点：egui-winit 会把 Ctrl+C / Ctrl+Insert 直接翻译成 `Event::Copy`，不会
/// 再产生 `Key::C`。所以「有选区复制、没选区发 SIGINT」必须在这里分流；否则
/// 终端里按 Ctrl+C 永远停不下前台命令（`~/.bashrc` 卡住时也没法中断）。
fn terminal_input(
    events: &[egui::Event],
    selection: Option<((u16, u16), (u16, u16))>,
    screen: &Screen,
    app_cursor: bool,
    bracketed_paste: bool,
) -> (Vec<u8>, Option<String>) {
    let mut out = Vec::new();
    let mut copy_text = None;
    for ev in events {
        if matches!(ev, egui::Event::Copy) {
            match selection {
                Some(sel) => copy_text = Some(selection_text(screen, sel)),
                None => out.push(0x03),
            }
            continue;
        }
        out.extend_from_slice(&event_to_bytes(ev, app_cursor, bracketed_paste));
    }
    (out, copy_text)
}

fn key_to_bytes(key: egui::Key, mods: egui::Modifiers, app_cursor: bool) -> Vec<u8> {
    if is_app_shortcut(key, mods) {
        return Vec::new();
    }

    if mods.command {
        // Ctrl+字母 → C0 控制码（Ctrl+C = 0x03、Ctrl+D = 0x04…）。
        if let Some(b) = ctrl_letter(key) {
            return vec![b];
        }
        if key == egui::Key::Backspace {
            return vec![0x08];
        }
        if key == egui::Key::Space {
            return vec![0x00];
        }
    }

    let arrow = |ch: u8| {
        if app_cursor {
            vec![0x1b, b'O', ch]
        } else {
            vec![0x1b, b'[', ch]
        }
    };

    match key {
        egui::Key::Enter => vec![b'\r'],
        egui::Key::Backspace => vec![0x7f],
        egui::Key::Tab => {
            if mods.shift {
                b"\x1b[Z".to_vec()
            } else {
                vec![b'\t']
            }
        }
        egui::Key::Escape => vec![0x1b],
        egui::Key::ArrowUp => arrow(b'A'),
        egui::Key::ArrowDown => arrow(b'B'),
        egui::Key::ArrowRight => arrow(b'C'),
        egui::Key::ArrowLeft => arrow(b'D'),
        egui::Key::Home => {
            if app_cursor {
                b"\x1bOH".to_vec()
            } else {
                b"\x1b[H".to_vec()
            }
        }
        egui::Key::End => {
            if app_cursor {
                b"\x1bOF".to_vec()
            } else {
                b"\x1b[F".to_vec()
            }
        }
        egui::Key::Insert => b"\x1b[2~".to_vec(),
        egui::Key::Delete => b"\x1b[3~".to_vec(),
        egui::Key::PageUp => b"\x1b[5~".to_vec(),
        egui::Key::PageDown => b"\x1b[6~".to_vec(),
        egui::Key::F1 => b"\x1bOP".to_vec(),
        egui::Key::F2 => b"\x1bOQ".to_vec(),
        egui::Key::F3 => b"\x1bOR".to_vec(),
        egui::Key::F4 => b"\x1bOS".to_vec(),
        egui::Key::F5 => b"\x1b[15~".to_vec(),
        egui::Key::F6 => b"\x1b[17~".to_vec(),
        egui::Key::F7 => b"\x1b[18~".to_vec(),
        egui::Key::F8 => b"\x1b[19~".to_vec(),
        egui::Key::F9 => b"\x1b[20~".to_vec(),
        egui::Key::F10 => b"\x1b[21~".to_vec(),
        egui::Key::F11 => b"\x1b[23~".to_vec(),
        egui::Key::F12 => b"\x1b[24~".to_vec(),
        _ => Vec::new(),
    }
}

fn ctrl_letter(key: egui::Key) -> Option<u8> {
    let c = match key {
        egui::Key::A => b'a',
        egui::Key::B => b'b',
        egui::Key::C => b'c',
        egui::Key::D => b'd',
        egui::Key::E => b'e',
        egui::Key::F => b'f',
        egui::Key::G => b'g',
        egui::Key::H => b'h',
        egui::Key::I => b'i',
        egui::Key::J => b'j',
        egui::Key::K => b'k',
        egui::Key::L => b'l',
        egui::Key::M => b'm',
        egui::Key::N => b'n',
        egui::Key::O => b'o',
        egui::Key::P => b'p',
        egui::Key::Q => b'q',
        egui::Key::R => b'r',
        egui::Key::S => b's',
        egui::Key::T => b't',
        egui::Key::U => b'u',
        egui::Key::V => b'v',
        egui::Key::W => b'w',
        egui::Key::X => b'x',
        egui::Key::Y => b'y',
        egui::Key::Z => b'z',
        _ => return None,
    };
    Some(c & 0x1f)
}

// ---------------------------------------------------------------------------
// 绘制
// ---------------------------------------------------------------------------

/// 画一个终端正文并处理输入。返回本帧终端是否持有键盘焦点。
///
/// `request_focus` 为 true（新建 / 切标签 / 刚打开面板）时本帧直接获得焦点。
pub fn draw_body(
    ui: &mut egui::Ui,
    session: &mut TerminalSession,
    style: &TerminalStyle,
    request_focus: bool,
) -> bool {
    let font_id = style.font.clone();
    let (cell_w, cell_h) = ui.ctx().fonts(|f| cell_metrics(f, &font_id));
    let avail = ui.available_size();
    let want_cols = ((avail.x / cell_w).floor() as i64).clamp(MIN_COLS as i64, 800) as u16;
    let want_rows = ((avail.y / cell_h).floor() as i64).clamp(MIN_ROWS as i64, 400) as u16;
    // 最小化窗口时 Windows 会报 0×0；这时绝不能把 8×2 发给 shell。
    if avail.x >= 1.0 && avail.y >= 1.0 {
        let immediate = session.first_layout;
        session.first_layout = false;
        let tick = stable_resize(
            (session.cols, session.rows),
            &mut session.pending_size,
            (want_cols, want_rows),
            Instant::now(),
            immediate,
        );
        match tick {
            ResizeTick::Apply => apply_size(session, want_cols, want_rows),
            ResizeTick::Wait(delay) => ui.ctx().request_repaint_after(delay),
            ResizeTick::Idle => {}
        }
    } else {
        session.pending_size = None;
    }

    let (cols, rows) = (session.cols, session.rows);
    // 只分配「当前正文区域」，不要按网格尺寸分配。
    //
    // 网格在防抖期间可能还是旧尺寸（窗口刚变矮时它比面板高）。若按网格分配，
    // `TopBottomPanel` 会把内容 rect 记成面板高度（见 egui `PanelState`），下一帧
    // 面板就长到旧网格那么高，把中间的编辑器整块挤没，拖动窗口时表现为面板来回跳。
    // 网格超出正文的部分用裁剪画掉即可。
    let (rect, response) = ui.allocate_exact_size(avail, egui::Sense::click_and_drag());

    // 视图 = 输出流的最后 N 行 = 滚回区尾部 + 当前屏幕内容。
    //
    // 面板变高时 ConPTY 不会重排内容（它只是把旧屏幕原样画在顶部），但我们可以
    // 把自己滚回区里的历史接到上面，于是「拉高面板 = 看到更多历史」，
    // 和 VS Code / Windows Terminal 的行为一致，而且一行内容都不会少。
    let view_rows = want_rows as usize;
    let model_rows = rows as usize;
    let sb_len = session.parser.screen().scrollback_len();
    let cursor_model_row = session.parser.screen().cursor_position().0;
    let live_rows = if session.parser.screen().alternate_screen() {
        // 全屏程序（vim/htop）自己占满整个屏幕，滚回区不参与。
        model_rows
    } else {
        (cursor_model_row as usize + 1).min(model_rows)
    };
    // 视图在「滚回区 + 屏幕」这条输出流里的起点。
    // scroll_offset = 用户往上翻了多少行（0 = 跟着最新输出）。
    let view_start = (sb_len + live_rows)
        .saturating_sub(view_rows)
        .saturating_sub(session.scroll_offset);

    // 防抖期间网格可能还是旧尺寸：把它按**底边**对齐，而不是顶边。
    //
    // 终端最重要的是光标/提示符那一行。拖上边缘时如果按顶边画：
    // - 变矮：网格底部（提示符）被裁掉，看起来像内容丢了；
    // - 变高：网格下方留一大块空白，等 resize 生效时内容突然往上跳。
    // 底边对齐后，正文底边固定不动；变矮只裁掉顶部旧行，变高只在上面留白，
    // 120ms 后一次性 resize，内容不会丢。
    let grid_h = view_rows as f32 * cell_h;
    let grid_rect = Rect::from_min_size(
        Pos2::new(rect.left(), rect.bottom() - grid_h),
        Vec2::new(cols as f32 * cell_w, grid_h),
    );

    if request_focus || response.clicked() {
        session.focused = true;
        session.sel_anchor = None;
        session.sel_end = None;
        // 侧栏搜索框 / Git 输入框等内置 TextEdit 可能还占着键盘焦点；
        // 若不主动交还，`wants_keyboard_input()` 会一直是 true，终端永远收不到键。
        if let Some(id) = ui.memory(|m| m.focused()) {
            ui.memory_mut(|m| m.surrender_focus(id));
        }
    }

    // 滚轮回看历史：向上滚 = 往 scrollback 里翻。
    if response.hovered() {
        let dy = ui.input(|i| i.smooth_scroll_delta.y);
        if dy.abs() > 0.5 {
            let step = (dy / cell_h).round() as i64;
            let next = (session.scroll_offset as i64 + step * 3).clamp(0, sb_len as i64);
            session.scroll_offset = next as usize;
        }
    }

    // 指针位置 → 模型行号。滚回区里的历史行不参与选择（vt100 只能按屏幕行选）。
    let pick_cell = |pos: Pos2| -> Option<(u16, u16)> {
        let (vrow, col) = pointer_cell(pos, grid_rect, cell_w, cell_h, cols, view_rows);
        let idx = view_start + vrow;
        if idx < sb_len {
            return None;
        }
        let mrow = (idx - sb_len).min(model_rows.saturating_sub(1)) as u16;
        Some((mrow, col))
    };

    // 鼠标拖选（按单元格）。
    if response.drag_started() {
        if let Some(pos) = response.interact_pointer_pos() {
            if let Some(cell) = pick_cell(pos) {
                session.sel_anchor = Some(cell);
                session.sel_end = Some(cell);
            }
        }
    } else if response.dragged() {
        if let Some(pos) = response.interact_pointer_pos() {
            if let Some(cell) = pick_cell(pos) {
                session.sel_end = Some(cell);
            }
        }
    }
    if response.clicked() {
        session.sel_anchor = None;
        session.sel_end = None;
    }

    let selection = match (session.sel_anchor, session.sel_end) {
        (Some(a), Some(b)) => Some((min_cell(a, b), max_cell(a, b))),
        _ => None,
    };

    {
        let screen = session.parser.screen_mut();
        // vt100 一次只能给出 model_rows 行的窗口：窗口偏移 k 表示「顶部露出 k 行滚回区」。
        // 视图比模型高时（防抖中面板先变高）分几段画，每段各自设置滚回偏移。
        let mut painted = 0usize;
        while painted < view_rows {
            let want_start = view_start + painted;
            let k = sb_len.saturating_sub(want_start);
            let skip = want_start - (sb_len - k);
            let take = (model_rows - skip).min(view_rows - painted);
            if take == 0 {
                break;
            }
            screen.set_scrollback(k);
            // 窗口第 0 行对应的模型行号（负数表示还在滚回区里）。
            let model_row_base = k as i64 - sb_len as i64;
            paint_screen(
                &ui.painter().with_clip_rect(rect),
                grid_rect,
                screen,
                style,
                &font_id,
                cell_w,
                cell_h,
                cols,
                skip as u16,
                take as u16,
                model_row_base,
                painted as u16,
                cursor_model_row,
                session.scroll_offset == 0,
                selection,
            );
            painted += take;
        }
        // 还原到实时视图，别让绘制用的偏移污染滚轮行为。
        screen.set_scrollback(0);
    }

    // Shell 启动慢时给个解释：~/.bashrc 里的同步命令（典型是 SSH 隧道）会卡住
    // 第一个提示符，用户容易以为终端坏了。
    if !session.seen_text && screen_has_text(session.parser.screen()) {
        session.seen_text = true;
    }
    if startup_hint_visible(
        session.started_at.elapsed(),
        session.exited,
        session.seen_text,
    ) {
        let hint = "正在等待 Shell 输出…\n若长时间无响应，检查 ~/.bashrc 里的同步命令（如 SSH 隧道），或按 Ctrl+C 中断。";
        let color = style.fg.gamma_multiply(0.6);
        let galley = ui.ctx().fonts(|f| {
            f.layout(
                hint.to_owned(),
                FontId::proportional(12.0),
                color,
                (rect.width() - 16.0).max(80.0),
            )
        });
        ui.painter()
            .galley(rect.left_top() + Vec2::new(8.0, cell_h + 4.0), galley, color);
    }

    // 键盘输入（终端焦点时）。有文本框持有焦点（模态框/搜索框）时不抢。
    let mut out: Vec<u8> = Vec::new();
    let mut copy_text: Option<String> = None;
    let wants_keyboard = ui.ctx().wants_keyboard_input();
    let accept = session.focused && !wants_keyboard;
    if accept {
        let app_cursor = session.parser.screen().application_cursor();
        let bracketed_paste = session.parser.screen().bracketed_paste();
        let events = ui.input(|i| i.events.clone());
        let (bytes, copied) = terminal_input(
            &events,
            selection,
            session.parser.screen(),
            app_cursor,
            bracketed_paste,
        );
        out = bytes;
        copy_text = copied;
    }

    if let Some(text) = copy_text {
        if !text.is_empty() {
            ui.ctx().copy_text(text);
            ui.ctx().request_repaint();
        }
    }
    if !out.is_empty() {
        if let Err(err) = session.shell.write(&out) {
            tracing::warn!("[terminal] {err}");
        }
        // 回到底部（用户输入时通常希望看最新输出）。
        session.parser.screen_mut().set_scrollback(0);
        ui.ctx().request_repaint();
    }
    ui.ctx().request_repaint_after(std::time::Duration::from_millis(60));

    session.focused
}

/// 终端网格的单元格尺寸。
///
/// 不能用 `Fonts::glyph_width('M')`：epaint 排版时每写一个字符都会把 `cursor_x`
/// 四舍五入到物理像素，实际步进是「取整后的 advance」（100% DPI 下 Hack 13px 是
/// 8.0，而 `glyph_width` 报 7.758）。两者相差 0.24px/字符，一行 60 个字符就漂移
/// ~15px——光标会落在文字中间。这里直接量一段真实排版结果的平均步进。
fn cell_metrics(fonts: &egui::epaint::Fonts, font: &FontId) -> (f32, f32) {
    const SAMPLE: usize = 64;
    let galley = fonts.layout_no_wrap("M".repeat(SAMPLE), font.clone(), Color32::WHITE);
    (
        (galley.size().x / SAMPLE as f32).max(1.0),
        fonts.row_height(font).max(1.0),
    )
}

/// 画一段终端网格。
///
/// - `first_row` / `row_count`：vt100 窗口里的哪几行（窗口 = 滚回区尾部 + 屏幕）；
/// - `model_row_base`：窗口第 0 行对应的模型行号（负数 = 还在滚回区里）；
/// - `view_row_base`：这一段在视图里的起始行（视图 = 面板上显示的那几行）；
/// - `cursor_model_row`：光标在模型里的行号（用来画光标块）。
#[allow(clippy::too_many_arguments)]
fn paint_screen(
    painter: &egui::Painter,
    rect: Rect,
    screen: &Screen,
    style: &TerminalStyle,
    font: &FontId,
    cw: f32,
    ch: f32,
    cols: u16,
    first_row: u16,
    row_count: u16,
    model_row_base: i64,
    view_row_base: u16,
    cursor_model_row: u16,
    show_cursor: bool,
    selection: Option<((u16, u16), (u16, u16))>,
) {
    let painter = painter.with_clip_rect(rect);
    painter.rect_filled(rect, 0.0, style.bg);

    for row in first_row..first_row.saturating_add(row_count) {
        let view_row = view_row_base + (row - first_row);
        let y = rect.top() + view_row as f32 * ch;
        let model_row = model_row_base + i64::from(row);
        let mut col = 0u16;
        while col < cols {
            let Some(cell) = screen.cell(row, col) else {
                break;
            };
            // 宽字符的后半格由前一个宽字符绘制覆盖，这里跳过。
            if cell.is_wide_continuation() {
                col += 1;
                continue;
            }

            let (fg, bg) = cell_colors(cell, style);
            let bg = if model_row >= 0 {
                effective_bg(model_row as u16, col, bg, selection, style)
            } else {
                bg
            };

            if cell.is_wide() {
                // 宽字符单独画（占两格宽），避免后面的字符整体左移。
                let cell_rect = Rect::from_min_size(
                    Pos2::new(rect.left() + col as f32 * cw, y),
                    Vec2::new(cw * 2.0, ch),
                );
                if bg != style.bg {
                    painter.rect_filled(cell_rect, 0.0, bg);
                }
                let text = if cell.has_contents() {
                    cell.contents()
                } else {
                    " "
                };
                painter.text(
                    Pos2::new(cell_rect.left(), y),
                    egui::Align2::LEFT_TOP,
                    text,
                    font.clone(),
                    fg,
                );
                col += 2;
                continue;
            }

            // 把同色、同属性的连续单元格合成一段文本，减少 draw call。
            let run_start = col;
            let mut run = String::new();
            let run_fg = fg;
            let run_bg = bg;
            let run_underline = cell.underline();
            while col < cols {
                let Some(c) = screen.cell(row, col) else {
                    break;
                };
                if c.is_wide() || c.is_wide_continuation() {
                    break;
                }
                let (cf, cb) = cell_colors(c, style);
                let cb = if model_row >= 0 {
                    effective_bg(model_row as u16, col, cb, selection, style)
                } else {
                    cb
                };
                if (cf, cb) != (run_fg, run_bg) || c.underline() != run_underline {
                    break;
                }
                if c.has_contents() {
                    run.push_str(c.contents());
                } else {
                    run.push(' ');
                }
                col += 1;
            }
            if run.is_empty() {
                // 理论上至少会进入一次循环；保底避免死循环。
                col = (run_start + 1).min(cols);
                run.push(' ');
            }
            let x = rect.left() + run_start as f32 * cw;
            let w = (col - run_start) as f32 * cw;
            if run_bg != style.bg {
                painter.rect_filled(
                    Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, ch)),
                    0.0,
                    run_bg,
                );
            }
            // 反色/同色时仍保证文字可见（例如黑底黑字）。
            let text_color = if run_fg == run_bg { style.fg } else { run_fg };
            painter.text(
                Pos2::new(x, y),
                egui::Align2::LEFT_TOP,
                &run,
                font.clone(),
                text_color,
            );
            if run_underline {
                painter.hline(
                    x..=(x + w),
                    y + ch - 2.0,
                    egui::Stroke::new(1.0_f32, text_color),
                );
            }
        }
    }

    // 光标（只在看实时屏幕时画；滚回历史时不画）。
    // 光标在模型里的行号换算成窗口行号，只有落在这一段里才画。
    let cursor_window_row = i64::from(cursor_model_row) - model_row_base;
    if show_cursor
        && !screen.hide_cursor()
        && cursor_window_row >= i64::from(first_row)
        && cursor_window_row < i64::from(first_row) + i64::from(row_count)
    {
        let w = cursor_window_row as u16;
        let view_row = view_row_base + (w - first_row);
        let (_, cc) = screen.cursor_position();
        if cc < cols {
            let wide = screen
                .cell(w, cc)
                .map(|c| c.is_wide())
                .unwrap_or(false);
            let cw_cursor = if wide { cw * 2.0 } else { cw };
            let caret = Rect::from_min_size(
                Pos2::new(
                    rect.left() + cc as f32 * cw,
                    rect.top() + view_row as f32 * ch,
                ),
                Vec2::new(cw_cursor, ch),
            );
            painter.rect_filled(caret, 0.0, style.cursor);
            let content = screen
                .cell(w, cc)
                .map(|c| c.contents().to_string())
                .unwrap_or_default();
            if !content.trim().is_empty() {
                painter.text(
                    caret.min,
                    egui::Align2::LEFT_TOP,
                    content,
                    font.clone(),
                    style.bg,
                );
            }
        }
    }
}

fn cell_colors(cell: &vt100::Cell, style: &TerminalStyle) -> (Color32, Color32) {
    let mut fg = ansi_to_color(cell.fgcolor(), style);
    let mut bg = ansi_to_bg(cell.bgcolor(), style);
    if cell.inverse() {
        std::mem::swap(&mut fg, &mut bg);
    }
    if cell.dim() {
        fg = fg.gamma_multiply(0.65);
    }
    (fg, bg)
}

fn effective_bg(
    row: u16,
    col: u16,
    bg: Color32,
    selection: Option<((u16, u16), (u16, u16))>,
    style: &TerminalStyle,
) -> Color32 {
    if selection.is_some_and(|(a, b)| in_selection(row, col, a, b)) {
        style.selection
    } else {
        bg
    }
}

fn ansi_to_color(color: Color, style: &TerminalStyle) -> Color32 {
    match color {
        Color::Default => style.fg,
        Color::Idx(i) => ansi_color(i, style),
        Color::Rgb(r, g, b) => Color32::from_rgb(r, g, b),
    }
}

fn ansi_to_bg(color: Color, style: &TerminalStyle) -> Color32 {
    match color {
        Color::Default => style.bg,
        Color::Idx(i) => ansi_color(i, style),
        Color::Rgb(r, g, b) => Color32::from_rgb(r, g, b),
    }
}

/// xterm 256 色调色板：0-15 主题色、16-231 6×6×6 色立方、232-255 灰阶。
pub fn ansi_color(i: u8, style: &TerminalStyle) -> Color32 {
    match i {
        0..=15 => style.ansi[i as usize],
        16..=231 => {
            let n = i - 16;
            let r = n / 36;
            let g = (n % 36) / 6;
            let b = n % 6;
            let level = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
            Color32::from_rgb(level(r), level(g), level(b))
        }
        _ => {
            let v = 8 + (i - 232) * 10;
            Color32::from_gray(v)
        }
    }
}

/// 指针位置 → 视图行号 + 列号（越界时夹到边上）。
fn pointer_cell(
    pos: Pos2,
    rect: Rect,
    cw: f32,
    ch: f32,
    cols: u16,
    view_rows: usize,
) -> (usize, u16) {
    let col = ((pos.x - rect.left()) / cw).floor().clamp(0.0, (cols - 1) as f32) as u16;
    let row = if view_rows == 0 {
        0
    } else {
        ((pos.y - rect.top()) / ch)
            .floor()
            .clamp(0.0, (view_rows - 1) as f32) as usize
    };
    (row, col)
}

fn min_cell(a: (u16, u16), b: (u16, u16)) -> (u16, u16) {
    if a <= b {
        a
    } else {
        b
    }
}

fn max_cell(a: (u16, u16), b: (u16, u16)) -> (u16, u16) {
    if a >= b {
        a
    } else {
        b
    }
}

fn in_selection(row: u16, col: u16, start: (u16, u16), end: (u16, u16)) -> bool {
    let p = (row, col);
    p >= start && p <= end
}

/// 选区文本（含端点）。用于 Ctrl+C / Ctrl+Shift+C。
fn selection_text(screen: &Screen, (start, end): ((u16, u16), (u16, u16))) -> String {
    let (_, cols) = screen.size();
    let end_col = (end.1 + 1).min(cols.get());
    let text = screen.contents_between(start.0, start.1, end.0, end_col);
    // 去掉每行尾部空白（复制提示符时更干净，与 Windows Terminal 一致）。
    text.split('\n')
        .map(|line| line.trim_end())
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 拖终端上边缘改高度时，用原始输出在新高度重建，屏幕内容必须连续。
    /// 这是修「42..61 直接跳到 298..300」的回归测试。
    #[test]
    fn conpty_repaint_after_resize_keeps_content_and_coordinates() {
        // 13 行屏幕 + 30 行输出，提示符在最后一行。
        let mut p = Parser::new(nz(13), nz(40), 100);
        for i in 1..=30 {
            p.process(format!("{i}\r\n").as_bytes());
        }
        p.process(b"root@host# ");

        // 面板变高：按 ConPTY 的布局（内容留在顶部，光标不动）。
        grow_screen(&mut p, 40);
        assert_eq!(p.screen().cursor_position().0, 12);

        // ConPTY 随后把旧屏幕整屏重画一遍（真机抓到的原始字节形态：
        // `ESC[?25l` + 制表位 + `ESC[H` + 各行 + `ESC[13;28H` + `ESC[?25h`）。
        let mut repaint = b"\x1b[?25l\x1b[8;40;100t\x1b[H".to_vec();
        for i in 19..=30 {
            repaint.extend_from_slice(format!("{i}\x1b[K\r\n").as_bytes());
        }
        repaint.extend_from_slice(b"root@host#\x1b[K\x1b[13;28H\x1b[?25h");
        p.process(&repaint);

        // 重画后内容必须还在（不能只剩提示符）。
        let row_text = |p: &Parser, row: u16| -> String {
            (0..40)
                .map(|c| {
                    p.screen()
                        .cell(row, c)
                        .map(|cell| cell.contents().to_string())
                        .unwrap_or_default()
                })
                .collect::<String>()
                .trim_end()
                .to_string()
        };
        assert_eq!(row_text(&p, 0), "19");
        assert_eq!(row_text(&p, 12), "root@host#");
        // 光标必须落在 ConPTY 给的位置（第 13 行），两边坐标一致。
        assert_eq!(p.screen().cursor_position().0, 12);

        // 之后 ConPTY 用绝对坐标写内容，必须写到同一行，不能盖掉屏幕中间。
        p.process(b"\x1b[13;1HNEW");
        assert_eq!(row_text(&p, 12), "NEWt@host#");
        assert_eq!(row_text(&p, 20), "", "中间的行不能被改写");
    }

    /// 缩行不能把最新输出裁掉：vt100 会把顶部几行推进滚回区，拉高时再取回来。
    /// 这是「防抖后一次性 resize 不丢内容」的前提。
    #[test]
    fn shrinking_rows_keeps_latest_output_and_restores_on_grow() {
        fn lines(p: &Parser, rows: u16, cols: u16) -> Vec<String> {
            (0..rows)
                .map(|r| {
                    let mut s = String::new();
                    for c in 0..cols {
                        if let Some(cell) = p.screen().cell(r, c) {
                            s.push_str(cell.contents());
                        }
                    }
                    s.trim_end().to_string()
                })
                .collect()
        }
        let mut p = Parser::new(nz(10), nz(20), 1000);
        for i in 0..30 {
            p.process(format!("line{i:02}\r\n").as_bytes());
        }
        let before = lines(&p, 10, 20);
        p.set_size(nz(5), nz(20));
        assert!(p.screen().scrollback_len() >= 5);
        // 光标在底部：缩小时保留的是最新的几行，而不是把底部裁掉。
        assert_eq!(lines(&p, 5, 20), before[5..].to_vec());
        p.set_size(nz(10), nz(20));
        assert_eq!(lines(&p, 10, 20), before);
    }

    /// 网格步进必须等于真实排版步进，否则一长行就会累积漂移（光标落在文字中间）。
    #[test]
    fn cell_width_matches_real_layout_steps() {
        let ctx = egui::Context::default();
        let _ = ctx.run(Default::default(), |_| {});
        let font = style().font.clone();
        let (cw, ch) = ctx.fonts(|f| cell_metrics(f, &font));
        assert!(ch > 0.0);

        ctx.fonts(|f| {
            for n in [1usize, 5, 60, 120] {
                let g = f.layout_no_wrap("M".repeat(n), font.clone(), Color32::WHITE);
                assert!(
                    (cw * n as f32 - g.size().x).abs() < 0.51,
                    "n={n} cw={cw} laid_out={}",
                    g.size().x
                );
            }
            // 用户截图里的提示符：光标列 × cw 必须落在文字末尾。
            let prompt = "root@DESKTOP-86D4EBJ:/home/work/NewLithoTune/candanza# clear";
            let g = f.layout_no_wrap(prompt.to_owned(), font.clone(), Color32::WHITE);
            assert!(
                (cw * prompt.chars().count() as f32 - g.size().x).abs() < 0.51,
                "prompt 末尾漂移：cursor_x={} text_end={}",
                cw * prompt.chars().count() as f32,
                g.size().x
            );
        });
    }

    /// 端到端：长提示符 + 输入内容后，光标块左沿必须贴着最后一个字符的右边。
    #[test]
    fn cursor_block_tracks_the_last_painted_glyph() {
        let ctx = egui::Context::default();
        let _ = ctx.run(Default::default(), |_| {});
        let prompt = "root@DESKTOP-86D4EBJ:/home/work/NewLithoTune/candanza# clear";
        let mut parser = Parser::new(nz(6), nz(80), 0);
        parser.process(prompt.as_bytes());

        let style = style();
        let font = style.font.clone();
        let (cw, ch) = ctx.fonts(|f| cell_metrics(f, &font));
        let out = ctx.run(Default::default(), |ctx| {
            let painter = ctx.layer_painter(egui::LayerId::background());
            let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(cw * 80.0, ch * 6.0));
            paint_screen(
                &painter,
                rect,
                parser.screen(),
                &style,
                &font,
                cw,
                ch,
                80,
                0,
                6,
                0,
                0,
                parser.screen().cursor_position().0,
                true,
                None,
            );
        });

        let caret = out
            .shapes
            .iter()
            .find_map(|cs| match &cs.shape {
                egui::Shape::Rect(r) if r.fill == style.cursor => Some(r.rect),
                _ => None,
            })
            .expect("应画出光标块");
        let (text_pos, galley) = out
            .shapes
            .iter()
            .find_map(|cs| match &cs.shape {
                egui::Shape::Text(t) if t.galley.text().starts_with("root@") => {
                    Some((t.pos, t.galley.clone()))
                }
                _ => None,
            })
            .expect("应画出提示符");

        let last = prompt.chars().count() - 1;
        let glyph = &galley.rows[0].glyphs[last];
        let text_end = text_pos.x + glyph.pos.x + glyph.size.x;
        assert!(
            (caret.left() - text_end).abs() < 1.0,
            "光标没贴在文字末尾：caret={} text_end={text_end}",
            caret.left()
        );
    }

    /// 拖动窗口宽度/终端面板高度时尺寸会连续跳变；只有稳定 `RESIZE_SETTLE` 后
    /// 才允许发给 PTY。否则每一帧 resize 都让 shell 收到 SIGWINCH 重绘，vt100
    /// 也会反复重排，实测会把滚回区和屏幕接错，导致内容丢失/闪烁。
    #[test]
    fn resize_storm_only_applies_the_final_size() {
        let t0 = Instant::now();
        let mut current = (120, 32);
        let mut pending = None;

        // 中间态：窗口被拖到很窄，行数也变了。行列都进入防抖。
        assert!(matches!(
            stable_resize(current, &mut pending, (2, 20), t0, false),
            ResizeTick::Wait(_)
        ));
        let p = pending.expect("2×20 进入防抖");
        assert_eq!((p.cols, p.rows), (2, 20));

        // 行数继续变，计时重新开始。
        let t1 = t0 + Duration::from_millis(60);
        assert!(matches!(
            stable_resize(current, &mut pending, (2, 10), t1, false),
            ResizeTick::Wait(_)
        ));
        let p = pending.expect("仍在防抖");
        assert_eq!((p.cols, p.rows), (2, 10));
        assert_eq!(p.since, t1);

        // 用户把窗口拖回正常宽度：目标又变了，重新计时。
        let t2 = t1 + Duration::from_millis(30);
        assert!(matches!(
            stable_resize(current, &mut pending, (118, 30), t2, false),
            ResizeTick::Wait(_)
        ));
        let p = pending.expect("118×30 进入防抖");
        assert_eq!((p.cols, p.rows), (118, 30));
        assert_eq!(p.since, t2);

        // 还没稳定够：继续等，不能下发中间尺寸。
        assert!(matches!(
            stable_resize(
                current,
                &mut pending,
                (118, 30),
                t2 + Duration::from_millis(119),
                false
            ),
            ResizeTick::Wait(_)
        ));
        // 稳定满 120ms：只下发最终尺寸。
        assert_eq!(
            stable_resize(current, &mut pending, (118, 30), t2 + RESIZE_SETTLE, false),
            ResizeTick::Apply
        );
        assert!(pending.is_none());
        current = (118, 30);
        assert_eq!(
            stable_resize(
                current,
                &mut pending,
                (118, 30),
                t2 + RESIZE_SETTLE,
                false
            ),
            ResizeTick::Idle
        );
    }

    /// 只拖终端面板上边缘（列不变）也要防抖：每帧 resize 会让 shell 反复重绘，
    /// 是内容丢失的直接原因。拖动期间画面靠底边对齐保持稳定。
    #[test]
    fn rows_resize_is_debounced_too() {
        let t0 = Instant::now();
        let mut pending = None;
        assert!(matches!(
            stable_resize((120, 32), &mut pending, (120, 20), t0, false),
            ResizeTick::Wait(_)
        ));
        assert_eq!(pending.expect("行数进入防抖").rows, 20);

        // 继续拖动会重新计时。
        let t1 = t0 + Duration::from_millis(40);
        assert!(matches!(
            stable_resize((120, 32), &mut pending, (120, 40), t1, false),
            ResizeTick::Wait(_)
        ));
        assert_eq!(pending.expect("仍在防抖").since, t1);

        // 稳定满 120ms 才下发。
        assert!(matches!(
            stable_resize(
                (120, 32),
                &mut pending,
                (120, 40),
                t1 + Duration::from_millis(119),
                false
            ),
            ResizeTick::Wait(_)
        ));
        assert_eq!(
            stable_resize(
                (120, 32),
                &mut pending,
                (120, 40),
                t1 + RESIZE_SETTLE,
                false
            ),
            ResizeTick::Apply
        );
        assert!(pending.is_none());
    }

    #[test]
    fn first_layout_applies_immediately() {
        let mut pending = None;
        assert_eq!(
            stable_resize((120, 32), &mut pending, (150, 30), Instant::now(), true),
            ResizeTick::Apply
        );
        assert!(pending.is_none());
    }

    #[test]
    fn same_size_clears_pending_resize() {
        let mut pending = Some(PendingSize {
            cols: 90,
            rows: 20,
            since: Instant::now(),
        });
        assert_eq!(
            stable_resize((120, 32), &mut pending, (120, 32), Instant::now(), false),
            ResizeTick::Idle
        );
        assert!(pending.is_none());
    }

    #[test]
    fn startup_hint_waits_for_two_seconds_and_stops_on_visible_text() {
        assert!(!startup_hint_visible(
            Duration::from_millis(1999),
            false,
            false
        ));
        assert!(startup_hint_visible(STARTUP_HINT_AFTER, false, false));
        // 提示符已经出现过（之后 clear 也不算启动）/ 进程已退出：不再显示
        assert!(!startup_hint_visible(
            STARTUP_HINT_AFTER,
            false,
            true
        ));
        assert!(!startup_hint_visible(STARTUP_HINT_AFTER, true, false));
    }

    #[test]
    fn screen_has_text_ignores_control_sequences_and_blanks() {
        let mut parser = Parser::new(nz(3), nz(20), 0);
        assert!(!screen_has_text(parser.screen()));
        // 纯控制序列 + 空格：屏幕上仍然没有文字
        parser.process(b"\x1b[6n\x1b]0;title\x07   ");
        assert!(!screen_has_text(parser.screen()));
        parser.process(b"root@wsl:~# ");
        assert!(screen_has_text(parser.screen()));
    }

    fn style() -> TerminalStyle {
        TerminalStyle::for_theme(ThemeKind::DarkPlus)
    }

    fn key(k: egui::Key, command: bool, shift: bool) -> egui::Event {
        egui::Event::Key {
            key: k,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers {
                alt: false,
                ctrl: command,
                shift,
                mac_cmd: false,
                command,
            },
        }
    }

    /// egui-winit 把 Ctrl+C 变成 `Event::Copy`；没选区时必须发 SIGINT，
    /// 否则卡在 `~/.bashrc` 里的命令连 Ctrl+C 都停不下来。
    #[test]
    fn copy_event_copies_selection_or_sends_sigint() {
        let mut parser = Parser::new(nz(2), nz(10), 0);
        parser.process(b"hello");
        let events = [egui::Event::Copy];

        let (bytes, copied) = terminal_input(&events, None, parser.screen(), false, false);
        assert_eq!(bytes, vec![0x03], "没有选区时 Ctrl+C 必须发 SIGINT");
        assert!(copied.is_none());

        let selection = Some(((0, 0), (0, 4)));
        let (bytes, copied) = terminal_input(&events, selection, parser.screen(), false, false);
        assert!(bytes.is_empty(), "有选区时 Ctrl+C 只复制，不打断");
        assert_eq!(copied.as_deref(), Some("hello"));
    }

    #[test]
    fn keyboard_encodes_common_control_sequences() {
        assert_eq!(
            event_to_bytes(&key(egui::Key::Enter, false, false), false, false),
            b"\r"
        );
        assert_eq!(
            event_to_bytes(&key(egui::Key::Backspace, false, false), false, false),
            vec![0x7f]
        );
        assert_eq!(
            event_to_bytes(&key(egui::Key::ArrowUp, false, false), false, false),
            b"\x1b[A"
        );
        assert_eq!(
            event_to_bytes(&key(egui::Key::ArrowUp, false, false), true, false),
            b"\x1bOA"
        );
        assert_eq!(
            event_to_bytes(&key(egui::Key::C, true, false), false, false),
            vec![0x03]
        );
        assert_eq!(
            event_to_bytes(&key(egui::Key::D, true, true), false, false),
            vec![0x04]
        );
        assert_eq!(
            event_to_bytes(&key(egui::Key::Tab, false, true), false, false),
            b"\x1b[Z"
        );
        assert_eq!(
            event_to_bytes(&key(egui::Key::Delete, false, false), false, false),
            b"\x1b[3~"
        );
    }

    #[test]
    fn app_shortcuts_do_not_reach_the_shell() {
        for k in [
            egui::Key::P,
            egui::Key::F,
            egui::Key::G,
            egui::Key::S,
            egui::Key::N,
            egui::Key::W,
            egui::Key::T,
            egui::Key::B,
            egui::Key::Comma,
            egui::Key::Backtick,
        ] {
            assert!(
                event_to_bytes(&key(k, true, false), false, false).is_empty(),
                "{k:?} 是 IDE 快捷键，不应写进 shell"
            );
        }
        // 没有 Ctrl 的同一批键仍是普通输入。
        assert_eq!(
            event_to_bytes(&key(egui::Key::T, false, false), false, false),
            Vec::<u8>::new(),
            "Key 事件本身不带字符，字符走 Event::Text"
        );
    }

    #[test]
    fn text_and_paste_events_are_utf8_and_cr_normalized() {
        let ev = egui::Event::Text("echo 你好".into());
        assert_eq!(
            event_to_bytes(&ev, false, false),
            "echo 你好".as_bytes().to_vec()
        );
        let ev = egui::Event::Paste("a\r\nb\n".into());
        assert_eq!(event_to_bytes(&ev, false, false), b"a\rb\r");
        // bracketed paste：前后包上 xterm 序列。
        assert_eq!(
            event_to_bytes(&ev, false, true),
            b"\x1b[200~a\rb\r\x1b[201~"
        );
    }

    #[test]
    fn ansi_palette_matches_xterm_256() {
        let s = style();
        assert_eq!(ansi_color(0, &s), s.ansi[0]);
        assert_eq!(ansi_color(9, &s), s.ansi[9]);
        assert_eq!(ansi_color(16, &s), Color32::from_rgb(0, 0, 0));
        assert_eq!(ansi_color(21, &s), Color32::from_rgb(0, 0, 255));
        assert_eq!(ansi_color(196, &s), Color32::from_rgb(255, 0, 0));
        assert_eq!(ansi_color(231, &s), Color32::from_rgb(255, 255, 255));
        assert_eq!(ansi_color(232, &s), Color32::from_gray(8));
        assert_eq!(ansi_color(255, &s), Color32::from_gray(238));
    }

    #[test]
    fn light_and_dark_palettes_differ() {
        let dark = TerminalStyle::for_theme(ThemeKind::CppDark);
        let light = TerminalStyle::for_theme(ThemeKind::LightPlus);
        assert_ne!(dark.bg, light.bg);
        assert_ne!(dark.fg, light.fg);
        assert_ne!(dark.ansi[2], light.ansi[2]);
    }

    #[test]
    fn selection_normalizes_and_extracts_text() {
        let mut p = Parser::new(nz(6), nz(40), 0);
        p.process(b"hello world\r\nsecond line\r\n");
        let start = min_cell((1, 0), (0, 6));
        let end = max_cell((1, 0), (0, 6));
        assert_eq!(start, (0, 6));
        assert_eq!(end, (1, 0));
        let text = selection_text(p.screen(), (start, end));
        assert_eq!(text, "world\ns");
        // 单行选区包含端点。
        let text = selection_text(p.screen(), ((0, 0), (0, 4)));
        assert_eq!(text, "hello");
    }

    #[test]
    fn pointer_maps_to_clamped_cell() {
        let rect = Rect::from_min_size(Pos2::new(10.0, 20.0), Vec2::new(80.0, 40.0));
        assert_eq!(pointer_cell(Pos2::new(10.0, 20.0), rect, 8.0, 16.0, 10, 3), (0, 0));
        assert_eq!(pointer_cell(Pos2::new(37.0, 51.0), rect, 8.0, 16.0, 10, 3), (1, 3));
        assert_eq!(
            pointer_cell(Pos2::new(-100.0, -100.0), rect, 8.0, 16.0, 10, 3),
            (0, 0)
        );
        assert_eq!(
            pointer_cell(Pos2::new(999.0, 999.0), rect, 8.0, 16.0, 10, 3),
            (2, 9)
        );
    }

    #[test]
    fn terminal_set_never_panics_without_sessions() {
        let mut set = TerminalSet::new();
        assert!(!set.poll());
        set.clear_active();
        set.close_active();
        assert!(set.active().is_none());
        assert!(!set.any_alive());
    }

    #[test]
    fn terminal_answers_cursor_and_device_queries_across_chunks() {
        let mut parser = Parser::new(nz(10), nz(40), 0);
        parser.process(b"\x1b[3;5H");
        let mut tail = Vec::new();
        // DSR 被拆成两个 chunk：第一次只缓存，第二次才应答。
        assert!(scan_terminal_queries(parser.screen(), &mut tail, b"\x1b[6").is_empty());
        assert_eq!(
            scan_terminal_queries(parser.screen(), &mut tail, b"n"),
            b"\x1b[3;5R"
        );
        assert!(tail.is_empty());
        // 设备属性 / 状态报告。
        assert_eq!(
            scan_terminal_queries(parser.screen(), &mut tail, b"\x1b[c"),
            b"\x1b[?1;2c"
        );
        assert_eq!(
            scan_terminal_queries(parser.screen(), &mut tail, b"\x1b[5n"),
            b"\x1b[0n"
        );
        // 普通颜色序列不会触发应答，也不会留尾巴。
        assert!(scan_terminal_queries(parser.screen(), &mut tail, b"\x1b[32mok").is_empty());
        assert!(tail.is_empty());
    }

    /// 真机回归：快速反复拖动面板高度时，终端内容不能丢中间段。
    ///
    /// 复现步骤：跑 `seq 1 300`，然后按「变大→变小」反复 resize PTY，
    /// 最后检查屏幕上 1..300 的数字仍然是连续的。
    ///
    /// `cargo test -p ide-ui real_wsl_resize_keeps_content -- --ignored --nocapture`
    #[test]
    #[ignore = "需要本机 WSL 发行版"]
    fn real_wsl_resize_keeps_content() {
        let distro =
            std::env::var("LITEIDE_WSL_DISTRO").unwrap_or_else(|_| "Ubuntu2404".to_string());
        let mut set = TerminalSet::new();
        set.spawn(&distro, "/tmp", 100, 13).expect("启动 WSL 终端");

        // 等提示符（poll 会自动应答 DSR）。
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline && !set.sessions[0].parser.screen().contents().contains('#')
        {
            set.poll();
            std::thread::sleep(Duration::from_millis(30));
        }
        assert!(
            set.sessions[0].parser.screen().contents().contains('#'),
            "没等到提示符: {:?}",
            set.sessions[0].parser.screen().contents()
        );

        // 跑 seq 1 300，等它输出完。
        set.sessions[0].shell.write(b"seq 1 300\r").expect("写命令");
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline
            && !set.sessions[0]
                .parser
                .screen()
                .contents()
                .contains("\r\n300")
        {
            set.poll();
            std::thread::sleep(Duration::from_millis(20));
        }

        // 模拟用户来回快速拖动面板高度。
        for rows in [40u16, 13, 30, 13, 50, 13, 25, 13] {
            apply_size(&mut set.sessions[0], 100, rows);
            for _ in 0..6 {
                set.poll();
                std::thread::sleep(Duration::from_millis(20));
            }
            let c = set.sessions[0].parser.screen().contents();
            let lines: Vec<&str> = c.lines().collect();
            println!(
                "rows={} 光标={:?} 屏幕前 3 行: {:?}",
                rows,
                set.sessions[0].parser.screen().cursor_position(),
                &lines[..lines.len().min(3)]
            );
        }
        // 让最后一批输出落地。
        for _ in 0..20 {
            set.poll();
            std::thread::sleep(Duration::from_millis(20));
        }

        let contents = set.sessions[0].parser.screen().contents();
        let numbers: Vec<u32> = contents
            .lines()
            .filter_map(|l| l.trim().parse::<u32>().ok())
            .collect();
        println!("屏幕上共 {} 个数字行: {:?}", numbers.len(), &numbers[..numbers.len().min(20)]);
        // 屏幕上的数字必须连续递增（1..300），中间不能缺段。
        for pair in numbers.windows(2) {
            assert_eq!(
                pair[1],
                pair[0] + 1,
                "数字不连续: {:?}\n完整屏幕:\n{}",
                &numbers[..numbers.len().min(40)],
                contents
            );
        }
        assert!(
            numbers.contains(&300),
            "屏幕上没有 300:\n{}",
            contents
        );
    }

    /// 变高后屏幕布局必须和 ConPTY 一致：内容留在顶部、光标不动、下方补空行。
    ///
    /// ConPTY 在 PTY resize 后不会重排内容，只是把旧屏幕原样画在顶部；
    /// vt100 的 `set_size` 反过来会把滚回区拉到顶部、把光标推到底部。
    /// 两者不一致时，ConPTY 后续发来的绝对光标定位就会落到屏幕中间，
    /// 把中间的内容盖掉（用户看到的「301 后面直接跳到 348」）。
    #[test]
    fn grow_keeps_conpty_layout() {
        let mut p = Parser::new(nz(13), nz(40), 100);
        for i in 1..=30 {
            p.process(format!("{i}\r\n").as_bytes());
        }
        p.process(b"root@host# ");
        let cursor_before = p.screen().cursor_position();
        let sb_before = p.screen().scrollback_len();
        assert_eq!(cursor_before.0, 12);
        assert_eq!(sb_before, 18);

        // 变高：模拟 ConPTY 的行为（内容在顶部，光标不动）。
        let moved = grow_screen(&mut p, 40);
        assert_eq!(moved, 18, "滚回区里有多少行就被拉上来多少行");
        let row_text = |p: &Parser, row: u16| -> String {
            (0..40)
                .map(|c| {
                    p.screen()
                        .cell(row, c)
                        .map(|cell| cell.contents().to_string())
                        .unwrap_or_default()
                })
                .collect::<String>()
                .trim_end()
                .to_string()
        };
        assert_eq!(
            p.screen().cursor_position().0,
            cursor_before.0,
            "光标行不能动，否则 ConPTY 的绝对定位会错位"
        );
        assert_eq!(row_text(&p, 0), "19", "顶部应该还是原来的内容");
        assert_eq!(row_text(&p, 12), "root@host#");
        assert_eq!(row_text(&p, 13), "", "下方应该补空行");
        assert_eq!(row_text(&p, 39), "", "下方应该补空行");
        assert_eq!(
            p.screen().scrollback_len(),
            sb_before,
            "滚回区不能因为变高而丢内容"
        );
    }

    /// 探针：把 ConPTY 在 resize 后真正发过来的字节打印出来（需要本机 WSL）。
    ///
    /// `cargo test -p ide-ui probe_conpty_resize_bytes -- --ignored --nocapture`
    #[test]
    #[ignore = "需要本机 WSL 发行版"]
    fn probe_conpty_resize_bytes() {
        let distro =
            std::env::var("LITEIDE_WSL_DISTRO").unwrap_or_else(|_| "Ubuntu2404".to_string());
        let mut term = WslTerminal::spawn(&distro, "/tmp", 100, 13).expect("启动 WSL PTY");
        let mut raw = Vec::new();
        // wsl.exe 启动时先发 DSR（问光标位置）；答得太早会丢，要等 shell 准备好。
        let mut asked_at: Option<Instant> = None;
        let mut replied = false;
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline && !raw.windows(2).any(|w| w == b"# ") {
            let out = term.drain();
            raw.extend_from_slice(&out.bytes);
            if raw.windows(3).any(|w| w == b"\x1b[6n".as_slice()) && asked_at.is_none() {
                asked_at = Some(Instant::now());
            }
            if let Some(t) = asked_at {
                if !replied && t.elapsed() >= Duration::from_secs(3) {
                    term.write(b"\x1b[1;1R").expect("回复 DSR");
                    replied = true;
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(
            raw.windows(2).any(|w| w == b"# "),
            "没等到提示符: {:?}",
            String::from_utf8_lossy(&raw)
        );

        term.write(b"seq 1 60\r").expect("写命令");
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline && !raw.windows(2).any(|w| w == b"60") {
            let out = term.drain();
            raw.extend_from_slice(&out.bytes);
            std::thread::sleep(Duration::from_millis(50));
        }
        println!("--- seq 输出结束，共 {} 字节", raw.len());

        term.resize(100, 40);
        std::thread::sleep(Duration::from_millis(500));
        let out = term.drain();
        println!(
            "--- 变高 13->40 收到 {} 字节:\n{:?}",
            out.bytes.len(),
            String::from_utf8_lossy(&out.bytes)
        );
    }
}
