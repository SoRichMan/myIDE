//! VSCode Dark+ / Light+ 主题色值（守护用例基准，勿随意改动）。
//!
//! 色值来源：VSCode 官方 `extensions/theme-defaults/themes/`：
//! - Dark+ = `dark_plus.json`（继承 `dark_vs.json`）
//! - Light+ = `light_plus.json`（继承 `light_vs.json`）
//! - sidebar/activitybar/statusbar/linenumber/lineHighlight 在官方 JSON 中未显式
//!   列出，取 VSCode 内置 light 默认色（与官方默认主题一致）。
pub const BG_EDITOR:      &str = "#1e1e1e";
pub const FG_EDITOR:      &str = "#d4d4d4";
pub const BG_SIDEBAR:     &str = "#252526";
pub const BG_ACTIVITYBAR: &str = "#333333";
pub const BG_STATUSBAR:   &str = "#007acc";
pub const FG_LINENUMBER:  &str = "#858585";
pub const BG_CURRENTLINE: &str = "#2a2d2e";

// ---- Light+ ----
pub const L_BG_EDITOR:      &str = "#FFFFFF";
pub const L_FG_EDITOR:      &str = "#000000";
pub const L_BG_SIDEBAR:     &str = "#F3F3F3";
pub const L_BG_ACTIVITYBAR: &str = "#2C2C2C";
pub const L_BG_STATUSBAR:   &str = "#007ACC";
pub const L_FG_LINENUMBER:  &str = "#237893";
pub const L_BG_CURRENTLINE: &str = "#E8E8E8";
/// Light+ 选区/光标色（官方 editor.selectionBackground = #ADD6FF）。
pub const L_SELECTION: &str = "#ADD6FF";
pub const L_CURSOR:    &str = "#000000";

// ---- C/C++ Themes 扩展的 "Dark (Visual Studio - C/C++)" ----
// 色值来源：ms-vscode.cpptools-themes 的 `themes/cpptools_dark_vs_new.json`
// （该主题的 workbench 色只定义了部分，未定义处沿用 VSCode 内置深色默认值）。
pub const C_BG_EDITOR:      &str = "#1E1E1E";
pub const C_FG_EDITOR:      &str = "#DADADA";
pub const C_BG_SIDEBAR:     &str = "#252526";
pub const C_BG_ACTIVITYBAR: &str = "#333333";
pub const C_BG_STATUSBAR:   &str = "#007ACC";
pub const C_FG_LINENUMBER:  &str = "#2B91AF";
pub const C_BG_CURRENTLINE: &str = "#2A2D2E";
pub const C_SELECTION:      &str = "#264F78";
pub const C_CURSOR:         &str = "#AEAFAD";

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
    pub fg_linenumber: &'static str,
    pub bg_currentline: &'static str,
    pub selection: &'static str,
    pub cursor: &'static str,
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
            fg_linenumber: FG_LINENUMBER,
            bg_currentline: BG_CURRENTLINE,
            selection: "#264f78",
            cursor: "#aeafad",
        },
        ThemeKind::LightPlus => PaletteHex {
            bg_editor: L_BG_EDITOR,
            fg_editor: L_FG_EDITOR,
            bg_sidebar: L_BG_SIDEBAR,
            bg_activitybar: L_BG_ACTIVITYBAR,
            bg_statusbar: L_BG_STATUSBAR,
            fg_linenumber: L_FG_LINENUMBER,
            bg_currentline: L_BG_CURRENTLINE,
            selection: L_SELECTION,
            cursor: L_CURSOR,
        },
        ThemeKind::CppDark => PaletteHex {
            bg_editor: C_BG_EDITOR,
            fg_editor: C_FG_EDITOR,
            bg_sidebar: C_BG_SIDEBAR,
            bg_activitybar: C_BG_ACTIVITYBAR,
            bg_statusbar: C_BG_STATUSBAR,
            fg_linenumber: C_FG_LINENUMBER,
            bg_currentline: C_BG_CURRENTLINE,
            selection: C_SELECTION,
            cursor: C_CURSOR,
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
        assert_eq!(BG_STATUSBAR, "#007acc");
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
        assert_eq!(L_BG_STATUSBAR, "#007ACC");
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
}