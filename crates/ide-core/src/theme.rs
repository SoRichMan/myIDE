//! VSCode Dark+ / Light+ 主题色值（守护用例基准，勿随意改动）。
//!
//! 色值来源：VSCode 官方 `extensions/theme-defaults/themes/`：
//! - Dark+ = `dark_plus.json`（继承 `dark_vs.json`）
//! - Light+ = `light_plus.json`（继承 `light_vs.json`）
//! - sidebar/activitybar/linenumber/lineHighlight 在官方 JSON 中未显式列出，
//!   取 VSCode 内置默认色（与官方默认主题一致）。
//!
//! **状态栏是刻意的例外**：官方 Dark+/Light+ 的状态栏是蓝底（内置默认 #007ACC），
//! 但用户 VS Code（1.116.0，`workbench.colorTheme` 未设置）用的内置默认主题是
//! `Dark 2026`（`2026-dark.json`，继承 `dark_modern.json`）——状态栏深灰底，
//! 只有「连接」项在连上 WSL/SSH 时才变蓝。按用户要求状态栏统一跟随现代主题
//! （`2026-dark.json` / `2026-light.json`），不再用官方蓝色；C/C++ 主题没有覆盖
//! `statusBar.*`，因此也沿用同一套状态栏色。
pub const BG_EDITOR:      &str = "#1e1e1e";
pub const FG_EDITOR:      &str = "#d4d4d4";
pub const BG_SIDEBAR:     &str = "#252526";
pub const BG_ACTIVITYBAR: &str = "#333333";
pub const FG_LINENUMBER:  &str = "#858585";
pub const BG_CURRENTLINE: &str = "#2a2d2e";
// ---- 状态栏（`2026-dark.json` = VSCode 1.116 的内置默认深色主题）----
pub const BG_STATUSBAR: &str = "#191a1b";
pub const FG_STATUSBAR: &str = "#8c8c8c";
/// 状态栏项悬停 / 按下时整块高亮（`.statusbar-item-label`）。
pub const BG_STATUSBAR_HOVER: &str = "#262728";
pub const BG_STATUSBAR_ACTIVE: &str = "#4b4c4d";
/// 状态栏顶部 1px 分隔线（`statusBar.border`）。
pub const BORDER_STATUSBAR: &str = "#2a2b2c";
/// 「连接」项：连上 WSL/SSH 后的蓝底白字（`2026-dark.json` 未覆盖，继承 `dark_modern.json`）。
pub const BG_STATUSBAR_REMOTE: &str = "#0078d4";
pub const FG_STATUSBAR_REMOTE: &str = "#ffffff";
/// 激活标签顶部的高亮条（`tab.selectedBorderTop`，现代主题里的强调蓝）。
/// 原先借用「蓝色状态栏」色代偿，状态栏改成深灰后必须单独给出。
pub const TAB_ACTIVE_BORDER: &str = "#0078d4";
// 标题栏：dark_vs.json 未定义 `titleBar.activeBackground`，取 VSCode 内置深色默认
// #3c3c3c（前景 #cccccc）；`titleBar.inactiveBackground` 由 dark_vs.json 覆盖为 #323233。
pub const BG_TITLEBAR: &str = "#3c3c3c";
pub const BG_TITLEBAR_INACTIVE: &str = "#323233";
pub const FG_TITLEBAR: &str = "#cccccc";
// 下拉菜单：dark_vs.json 的 `menu.background` / `menu.border` / `menu.selectionBackground`。
pub const BG_MENU: &str = "#252526";
pub const BORDER_MENU: &str = "#454545";
pub const BG_MENU_SELECTION: &str = "#0078d4";

// ---- Light+ ----
pub const L_BG_EDITOR:      &str = "#FFFFFF";
pub const L_FG_EDITOR:      &str = "#000000";
pub const L_BG_SIDEBAR:     &str = "#F3F3F3";
pub const L_BG_ACTIVITYBAR: &str = "#2C2C2C";
pub const L_BG_STATUSBAR: &str = "#fafafd";
pub const L_FG_STATUSBAR: &str = "#606060";
pub const L_BG_STATUSBAR_HOVER: &str = "#dadada4f";
pub const L_BG_STATUSBAR_ACTIVE: &str = "#eeeeee";
pub const L_BORDER_STATUSBAR: &str = "#f0f1f2";
/// Light 2026 未覆盖远程项，继承 `light_modern.json` 的 #005FB8。
pub const L_BG_STATUSBAR_REMOTE: &str = "#005fb8";
pub const L_FG_STATUSBAR_REMOTE: &str = "#ffffff";
pub const L_TAB_ACTIVE_BORDER: &str = "#005fb8";
pub const L_FG_LINENUMBER:  &str = "#237893";
pub const L_BG_CURRENTLINE: &str = "#E8E8E8";
/// Light+ 选区/光标色（官方 editor.selectionBackground = #ADD6FF）。
pub const L_SELECTION: &str = "#ADD6FF";
pub const L_CURSOR:    &str = "#000000";
// 标题栏：内置浅色默认 `titleBar.activeBackground` #dddddd / 前景 #333333；
// `titleBar.inactiveBackground` 取 light_vs.json 的 #e5e5e5。
pub const L_BG_TITLEBAR: &str = "#dddddd";
pub const L_BG_TITLEBAR_INACTIVE: &str = "#e5e5e5";
pub const L_FG_TITLEBAR: &str = "#333333";
// 下拉菜单：light_vs.json 只覆盖了 `menu.border`，其余取内置浅色默认。
pub const L_BG_MENU: &str = "#ffffff";
pub const L_BORDER_MENU: &str = "#d4d4d4";
pub const L_BG_MENU_SELECTION: &str = "#0060c0";

// ---- C/C++ Themes 扩展的 "Dark (Visual Studio - C/C++)" ----
// 色值来源：ms-vscode.cpptools-themes 的 `themes/cpptools_dark_vs_new.json`
// （该主题的 workbench 色只定义了部分，未定义处沿用 VSCode 内置深色默认值）。
pub const C_BG_EDITOR:      &str = "#1E1E1E";
pub const C_FG_EDITOR:      &str = "#DADADA";
pub const C_BG_SIDEBAR:     &str = "#252526";
pub const C_BG_ACTIVITYBAR: &str = "#333333";
/// cpptools 主题没有覆盖 `statusBar.background`，VSCode 1.116 内置默认即 `Dark 2026`。
pub const C_BG_STATUSBAR:   &str = "#191a1b";
pub const C_FG_LINENUMBER:  &str = "#2B91AF";
pub const C_BG_CURRENTLINE: &str = "#2A2D2E";
pub const C_SELECTION:      &str = "#264F78";
pub const C_CURSOR:         &str = "#AEAFAD";
/// C/C++ 主题 JSON 显式覆盖了 `menu.background`：#303031（比内置 Dark+ 的 #252526 亮）。
pub const C_BG_MENU: &str = "#303031";

/// 主题种类（决定"窗口外壳"色板；token 配色由主题扩展单独贡献）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeKind {
    DarkPlus,
    LightPlus,
    /// C/C++ Themes 扩展的 Dark (Visual Studio - C/C++)。
    CppDark,
}

impl ThemeKind {
    /// 全部主题（设置面板按此顺序列出）。
    pub const ALL: [ThemeKind; 3] =
        [ThemeKind::DarkPlus, ThemeKind::LightPlus, ThemeKind::CppDark];

    /// 显示名（设置面板/状态栏用）。
    pub fn name(self) -> &'static str {
        match self {
            ThemeKind::DarkPlus => "Dark+",
            ThemeKind::LightPlus => "Light+",
            ThemeKind::CppDark => "C/C++ Dark",
        }
    }

    /// 从显示名解析；未知名称返回 Dark+。
    pub fn from_name(s: &str) -> ThemeKind {
        match s {
            "Light+" => ThemeKind::LightPlus,
            // 兼容扩展里的官方 label 写法
            "C/C++ Dark" | "Dark (Visual Studio - C/C++)" => ThemeKind::CppDark,
            _ => ThemeKind::DarkPlus,
        }
    }
}

/// 一套完整 UI 色板（编辑器/面板/状态栏共用），全部为 #RRGGBB 字符串。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaletteHex {
    pub bg_editor: &'static str,
    pub fg_editor: &'static str,
    pub bg_sidebar: &'static str,
    pub bg_activitybar: &'static str,
    pub bg_statusbar: &'static str,
    /// 状态栏文字/图标色。
    pub fg_statusbar: &'static str,
    /// 状态栏项悬停 / 按下时的整块底色。
    pub bg_statusbar_hover: &'static str,
    pub bg_statusbar_active: &'static str,
    /// 状态栏顶部 1px 分隔线。
    pub border_statusbar: &'static str,
    /// 「连接」项（连上 WSL/SSH）的蓝底白字。
    pub bg_statusbar_remote: &'static str,
    pub fg_statusbar_remote: &'static str,
    /// 激活标签顶部的高亮条（强调蓝）。
    pub tab_active_border: &'static str,
    pub fg_linenumber: &'static str,
    pub bg_currentline: &'static str,
    pub selection: &'static str,
    pub cursor: &'static str,
    /// 自绘标题栏底色（聚焦 / 未聚焦）。
    pub bg_titlebar: &'static str,
    pub bg_titlebar_inactive: &'static str,
    /// 标题栏文字与图标色。
    pub fg_titlebar: &'static str,
    /// 下拉菜单底色 / 描边 / 选中项底色。
    pub bg_menu: &'static str,
    pub border_menu: &'static str,
    pub bg_menu_selection: &'static str,
}

/// 取指定主题的色板。
pub fn palette(kind: ThemeKind) -> PaletteHex {
    match kind {
        ThemeKind::DarkPlus => PaletteHex {
            bg_editor: BG_EDITOR,
            fg_editor: FG_EDITOR,
            bg_sidebar: BG_SIDEBAR,
            bg_activitybar: BG_ACTIVITYBAR,
            bg_statusbar: BG_STATUSBAR,
            fg_statusbar: FG_STATUSBAR,
            bg_statusbar_hover: BG_STATUSBAR_HOVER,
            bg_statusbar_active: BG_STATUSBAR_ACTIVE,
            border_statusbar: BORDER_STATUSBAR,
            bg_statusbar_remote: BG_STATUSBAR_REMOTE,
            fg_statusbar_remote: FG_STATUSBAR_REMOTE,
            tab_active_border: TAB_ACTIVE_BORDER,
            fg_linenumber: FG_LINENUMBER,
            bg_currentline: BG_CURRENTLINE,
            selection: "#264f78",
            cursor: "#aeafad",
            bg_titlebar: BG_TITLEBAR,
            bg_titlebar_inactive: BG_TITLEBAR_INACTIVE,
            fg_titlebar: FG_TITLEBAR,
            bg_menu: BG_MENU,
            border_menu: BORDER_MENU,
            bg_menu_selection: BG_MENU_SELECTION,
        },
        ThemeKind::LightPlus => PaletteHex {
            bg_editor: L_BG_EDITOR,
            fg_editor: L_FG_EDITOR,
            bg_sidebar: L_BG_SIDEBAR,
            bg_activitybar: L_BG_ACTIVITYBAR,
            bg_statusbar: L_BG_STATUSBAR,
            fg_statusbar: L_FG_STATUSBAR,
            bg_statusbar_hover: L_BG_STATUSBAR_HOVER,
            bg_statusbar_active: L_BG_STATUSBAR_ACTIVE,
            border_statusbar: L_BORDER_STATUSBAR,
            bg_statusbar_remote: L_BG_STATUSBAR_REMOTE,
            fg_statusbar_remote: L_FG_STATUSBAR_REMOTE,
            tab_active_border: L_TAB_ACTIVE_BORDER,
            fg_linenumber: L_FG_LINENUMBER,
            bg_currentline: L_BG_CURRENTLINE,
            selection: L_SELECTION,
            cursor: L_CURSOR,
            bg_titlebar: L_BG_TITLEBAR,
            bg_titlebar_inactive: L_BG_TITLEBAR_INACTIVE,
            fg_titlebar: L_FG_TITLEBAR,
            bg_menu: L_BG_MENU,
            border_menu: L_BORDER_MENU,
            bg_menu_selection: L_BG_MENU_SELECTION,
        },
        ThemeKind::CppDark => PaletteHex {
            bg_editor: C_BG_EDITOR,
            fg_editor: C_FG_EDITOR,
            bg_sidebar: C_BG_SIDEBAR,
            bg_activitybar: C_BG_ACTIVITYBAR,
            bg_statusbar: C_BG_STATUSBAR,
            fg_statusbar: FG_STATUSBAR,
            bg_statusbar_hover: BG_STATUSBAR_HOVER,
            bg_statusbar_active: BG_STATUSBAR_ACTIVE,
            border_statusbar: BORDER_STATUSBAR,
            bg_statusbar_remote: BG_STATUSBAR_REMOTE,
            fg_statusbar_remote: FG_STATUSBAR_REMOTE,
            tab_active_border: TAB_ACTIVE_BORDER,
            fg_linenumber: C_FG_LINENUMBER,
            bg_currentline: C_BG_CURRENTLINE,
            selection: C_SELECTION,
            cursor: C_CURSOR,
            // C/C++ 主题没有覆盖标题栏/菜单，沿用 VSCode 内置深色默认（与 Dark+ 相同）。
            bg_titlebar: BG_TITLEBAR,
            bg_titlebar_inactive: BG_TITLEBAR_INACTIVE,
            fg_titlebar: FG_TITLEBAR,
            bg_menu: BG_MENU,
            border_menu: BORDER_MENU,
            bg_menu_selection: BG_MENU_SELECTION,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dark_plus_colors_match_official() {
        assert_eq!(BG_EDITOR, "#1e1e1e");
        assert_eq!(FG_EDITOR, "#d4d4d4");
        assert_eq!(BG_SIDEBAR, "#252526");
        assert_eq!(BG_ACTIVITYBAR, "#333333");
        // 用户 VS Code 1.116 内置默认 "Dark 2026"（2026-dark.json）实测值。
        assert_eq!(BG_STATUSBAR, "#191a1b");
        assert_eq!(FG_STATUSBAR, "#8c8c8c");
        assert_eq!(BG_STATUSBAR_HOVER, "#262728");
        assert_eq!(BORDER_STATUSBAR, "#2a2b2c");
        assert_eq!(BG_STATUSBAR_REMOTE, "#0078d4");
        assert_eq!(FG_LINENUMBER, "#858585");
        assert_eq!(BG_CURRENTLINE, "#2a2d2e");
    }

    #[test]
    fn light_plus_colors_match_official() {
        // light_plus.json（继承 light_vs.json）实测值 + 官方内置默认色。
        assert_eq!(L_BG_EDITOR, "#FFFFFF");
        assert_eq!(L_FG_EDITOR, "#000000");
        assert_eq!(L_BG_SIDEBAR, "#F3F3F3");
        assert_eq!(L_BG_ACTIVITYBAR, "#2C2C2C");
        // Light 2026（2026-light.json）实测值。
        assert_eq!(L_BG_STATUSBAR, "#fafafd");
        assert_eq!(L_FG_STATUSBAR, "#606060");
        assert_eq!(L_BG_STATUSBAR_HOVER, "#dadada4f");
        assert_eq!(L_BORDER_STATUSBAR, "#f0f1f2");
        assert_eq!(L_BG_STATUSBAR_REMOTE, "#005fb8");
        assert_eq!(L_FG_LINENUMBER, "#237893");
        assert_eq!(L_BG_CURRENTLINE, "#E8E8E8");
        assert_eq!(L_SELECTION, "#ADD6FF");
    }

    #[test]
    fn theme_kind_name_roundtrip() {
        assert_eq!(ThemeKind::from_name("Dark+"), ThemeKind::DarkPlus);
        assert_eq!(ThemeKind::from_name("Light+"), ThemeKind::LightPlus);
        assert_eq!(ThemeKind::from_name("C/C++ Dark"), ThemeKind::CppDark);
        assert_eq!(
            ThemeKind::from_name("Dark (Visual Studio - C/C++)"),
            ThemeKind::CppDark
        );
        assert_eq!(ThemeKind::from_name("???") , ThemeKind::DarkPlus);
        assert_eq!(ThemeKind::DarkPlus.name(), "Dark+");
        assert_eq!(ThemeKind::LightPlus.name(), "Light+");
        assert_eq!(ThemeKind::CppDark.name(), "C/C++ Dark");
        for k in ThemeKind::ALL {
            assert_eq!(ThemeKind::from_name(k.name()), k, "主题名回环失败：{k:?}");
        }
    }

    #[test]
    fn cpp_dark_colors_match_cpptools_theme() {
        // cpptools_dark_vs_new.json（Dark (Visual Studio - C/C++)）
        assert_eq!(C_BG_EDITOR, "#1E1E1E");
        assert_eq!(C_FG_EDITOR, "#DADADA");
        assert_eq!(C_FG_LINENUMBER, "#2B91AF");
        let p = palette(ThemeKind::CppDark);
        assert_eq!(p.bg_editor, C_BG_EDITOR);
        assert_eq!(p.fg_editor, C_FG_EDITOR);
        assert_eq!(p.fg_linenumber, C_FG_LINENUMBER);
        assert_eq!(p.bg_currentline, C_BG_CURRENTLINE);
        assert_eq!(p.selection, C_SELECTION);
        // 与 Dark+ 的差异点：编辑器前景/行号色不同
        assert_ne!(p.fg_editor, FG_EDITOR);
        assert_ne!(p.fg_linenumber, FG_LINENUMBER);
    }

    #[test]
    fn all_themes_use_modern_statusbar() {
        // 三套主题的状态栏都跟随现代主题：深色主题同 Dark 2026，浅色同 Light 2026。
        for kind in [ThemeKind::DarkPlus, ThemeKind::CppDark] {
            let p = palette(kind);
            assert_eq!(p.bg_statusbar, "#191a1b", "{kind:?}");
            assert_eq!(p.fg_statusbar, "#8c8c8c", "{kind:?}");
            assert_eq!(p.bg_statusbar_hover, "#262728", "{kind:?}");
            assert_eq!(p.border_statusbar, "#2a2b2c", "{kind:?}");
            assert_eq!(p.bg_statusbar_remote, "#0078d4", "{kind:?}");
        }
        let p = palette(ThemeKind::LightPlus);
        assert_eq!(p.bg_statusbar, "#fafafd");
        assert_eq!(p.fg_statusbar, "#606060");
        assert_eq!(p.border_statusbar, "#f0f1f2");
        assert_eq!(p.bg_statusbar_remote, "#005fb8");
        // 激活标签高亮条必须是强调蓝，不能跟着状态栏变灰。
        assert_eq!(palette(ThemeKind::DarkPlus).tab_active_border, "#0078d4");
        assert_eq!(p.tab_active_border, "#005fb8");
    }

    #[test]
    fn palette_matches_consts() {
        let d = palette(ThemeKind::DarkPlus);
        assert_eq!(d.bg_editor, BG_EDITOR);
        assert_eq!(d.bg_sidebar, BG_SIDEBAR);
        assert_eq!(d.bg_statusbar, BG_STATUSBAR);
        assert_eq!(d.bg_currentline, BG_CURRENTLINE);
        assert_eq!(d.fg_linenumber, FG_LINENUMBER);
        let l = palette(ThemeKind::LightPlus);
        assert_eq!(l.bg_editor, L_BG_EDITOR);
        assert_eq!(l.bg_sidebar, L_BG_SIDEBAR);
        assert_eq!(l.bg_statusbar, L_BG_STATUSBAR);
        assert_eq!(l.selection, L_SELECTION);
        assert_eq!(l.cursor, L_CURSOR);
    }

    #[test]
    fn titlebar_colors_match_vscode() {
        // dark_vs.json + VSCode 内置深色默认（Dark+）。
        assert_eq!(BG_TITLEBAR, "#3c3c3c");
        assert_eq!(BG_TITLEBAR_INACTIVE, "#323233");
        assert_eq!(FG_TITLEBAR, "#cccccc");
        assert_eq!(BG_MENU, "#252526");
        assert_eq!(BORDER_MENU, "#454545");
        assert_eq!(BG_MENU_SELECTION, "#0078d4");
        // light_vs.json + 内置浅色默认（Light+）。
        assert_eq!(L_BG_TITLEBAR, "#dddddd");
        assert_eq!(L_BG_TITLEBAR_INACTIVE, "#e5e5e5");
        assert_eq!(L_FG_TITLEBAR, "#333333");
        assert_eq!(L_BORDER_MENU, "#d4d4d4");
        // C/C++ 主题不覆盖标题栏/菜单，与 Dark+ 同值。
        let c = palette(ThemeKind::CppDark);
        assert_eq!(c.bg_titlebar, BG_TITLEBAR);
        assert_eq!(c.bg_titlebar_inactive, BG_TITLEBAR_INACTIVE);
        assert_eq!(c.bg_menu, BG_MENU);
        assert_eq!(c.bg_menu_selection, BG_MENU_SELECTION);
        let l = palette(ThemeKind::LightPlus);
        assert_eq!(l.bg_titlebar, L_BG_TITLEBAR);
        assert_eq!(l.fg_titlebar, L_FG_TITLEBAR);
        assert_eq!(l.bg_menu, L_BG_MENU);
    }
}
