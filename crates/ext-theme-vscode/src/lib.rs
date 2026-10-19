//! ext-theme-vscode：把 VSCode 的配色作为**主题扩展**贡献出来。
//!
//! 三套色板都来自 VSCode 官方 `theme-defaults` 与 `ms-vscode.cpptools-themes`：
//!
//! | 扩展 id | 外壳色板 | token 色来源 |
//! |---|---|---|
//! | `vscode-dark-plus` | `ThemeKind::DarkPlus` | `theme-defaults/themes/dark_plus.json` + `dark_vs.json` |
//! | `vscode-light-plus` | `ThemeKind::LightPlus` | `theme-defaults/themes/light_plus.json` + `light_vs.json` |
//! | `vscode-cpptools-dark` | `ThemeKind::CppDark` | `cpptools-themes/themes/cpptools_dark_vs_new.json` |
//!
//! TextMate 的解析规则是 **last-match-wins**，所以色值取的是"最终生效"的那条
//! （例如 `keyword.control` 在 C/C++ Themes 里被 VS2019 段覆盖成紫色 `#D8A0DF`）。
//! 每个分组的取值依据都写在下表的注释里，改色请连同 `tests` 里的断言一起改。

use ide_core::theme::ThemeKind;
use ide_ext::{
    Capability, ExtContext, ExtError, Extension, Manifest, ThemeContribution, TokenPalette,
};

/// Dark+（VSCode 默认深色）。
pub const DARK_PLUS: TokenPalette = TokenPalette::from_array([
    "#D4D4D4", // Plain        editor.foreground
    "#569CD6", // Keyword      keyword / storage.type / storage.modifier
    "#C586C0", // KeywordControl keyword.control
    "#569CD6", // Constant     constant.language
    "#4EC9B0", // Type         entity.name.type / support.type
    "#4EC9B0", // Namespace    entity.name.namespace
    "#DCDCAA", // Function     entity.name.function / support.function
    "#9CDCFE", // Variable     variable
    "#9CDCFE", // Parameter    variable.parameter（Dark+ 与变量同色）
    "#9CDCFE", // Property     variable.other.property
    "#569CD6", // Macro        entity.name.function.preprocessor
    "#4FC1FF", // EnumMember   variable.other.enummember
    "#C586C0", // Preproc      keyword.control.directive
    "#CE9178", // String       string
    "#B5CEA8", // Number       constant.numeric
    "#6A9955", // Comment      comment
    "#D4D4D4", // Operator     punctuation / keyword.operator
]);

/// Light+（VSCode 默认浅色）。
pub const LIGHT_PLUS: TokenPalette = TokenPalette::from_array([
    "#000000", // Plain        editor.foreground
    "#0000FF", // Keyword      keyword / storage.type / storage.modifier
    "#AF00DB", // KeywordControl keyword.control
    "#0000FF", // Constant     constant.language
    "#267F99", // Type         entity.name.type / support.type
    "#267F99", // Namespace    entity.name.namespace
    "#795E26", // Function     entity.name.function / support.function
    "#001080", // Variable     variable
    "#001080", // Parameter    variable.parameter
    "#001080", // Property     variable.other.property
    "#0000FF", // Macro        entity.name.function.preprocessor
    "#0070C1", // EnumMember   variable.other.enummember
    "#AF00DB", // Preproc      keyword.control.directive
    "#A31515", // String       string
    "#098658", // Number       constant.numeric
    "#008000", // Comment      comment
    "#000000", // Operator     punctuation / keyword.operator
]);

/// C/C++ Themes 扩展的 Dark (Visual Studio - C/C++)——**目标配色**。
///
/// 与本项目此前 Dark+ 配色的两处关键差异：
/// 1. `comment` 是 `#57A64A`（不是 Dark+ 的 `#6A9955`）；
/// 2. `int`/`void` 这类基本类型随 `storage.type` 走**蓝色**，只有 `int32_t`/`vector`
///    这类用户类型才是青色；函数参数是灰色 `#9A9A9A`、成员访问是 `#DADADA`、
///    宏名是淡紫 `#BEB7FF`。
pub const CPPTOOLS_DARK: TokenPalette = TokenPalette::from_array([
    "#DADADA", // Plain        editor.foreground
    "#569CD6", // Keyword      keyword / storage / storage.type.primitive
    "#D8A0DF", // KeywordControl keyword.control（VS2019 覆盖，紫色）
    "#569CD6", // Constant     constant.language（true/false/nullptr）
    "#4EC9B0", // Type         entity.name.type / support.type
    "#C8C8C8", // Namespace    entity.name.namespace / entity.name.scope-resolution
    "#DCDCAA", // Function     entity.name.function / support.function
    "#9CDCFE", // Variable     variable
    "#9A9A9A", // Parameter    variable.parameter（VS2019 覆盖，灰色）
    "#DADADA", // Property     variable.other.property
    "#BEB7FF", // Macro        entity.name.function.preprocessor
    "#B5CEA8", // EnumMember   variable.other.enummember
    "#9A9A9A", // Preproc      keyword.control.directive（#include/#define）
    "#CE9178", // String       string / meta.preprocessor.string
    "#B5CEA8", // Number       constant.numeric
    "#57A64A", // Comment      comment（VS2019 覆盖）
    "#B4B4B4", // Operator     punctuation / keyword.operator
]);

const DARK_PLUS_CONTRIB: ThemeContribution = ThemeContribution {
    id: "vscode-dark-plus",
    name: "Dark+",
    kind: ThemeKind::DarkPlus,
    tokens: DARK_PLUS,
};

const LIGHT_PLUS_CONTRIB: ThemeContribution = ThemeContribution {
    id: "vscode-light-plus",
    name: "Light+",
    kind: ThemeKind::LightPlus,
    tokens: LIGHT_PLUS,
};

const CPPTOOLS_CONTRIB: ThemeContribution = ThemeContribution {
    id: "vscode-cpptools-dark",
    name: "Dark (Visual Studio - C/C++)",
    kind: ThemeKind::CppDark,
    tokens: CPPTOOLS_DARK,
};

static MANIFEST: Manifest = Manifest {
    id: "liteide.theme-vscode",
    name: "VSCode 配色",
    version: "0.1.0",
    description: "Dark+ / Light+ / C/C++ Themes 的 token 配色",
    capabilities: &[
        Capability::Theme(DARK_PLUS_CONTRIB),
        Capability::Theme(LIGHT_PLUS_CONTRIB),
        Capability::Theme(CPPTOOLS_CONTRIB),
    ],
};

/// VSCode 配色扩展。
#[derive(Debug, Default, Clone, Copy)]
pub struct VscodeThemes;

impl Extension for VscodeThemes {
    fn manifest(&self) -> &'static Manifest {
        &MANIFEST
    }

    fn activate(&mut self, ctx: &mut ExtContext<'_>) -> Result<(), ExtError> {
        ide_ext::register_declared(ctx, &MANIFEST)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ide_core::HighlightGroup;

    /// 按分组名取色，便于用"读得懂"的方式写断言。
    fn hex_of(p: &TokenPalette, group: HighlightGroup) -> &'static str {
        p.hex(group)
    }

    #[test]
    fn cpptools_dark_matches_source_theme() {
        // 逐条对齐 ms-vscode.cpptools-themes/themes/cpptools_dark_vs_new.json。
        use HighlightGroup::*;
        let expect: &[(HighlightGroup, &str, &str)] = &[
            (Plain, "#DADADA", "editor.foreground"),
            (Keyword, "#569CD6", "keyword/storage/storage.type.primitive"),
            (KeywordControl, "#D8A0DF", "keyword.control (VS2019)"),
            (Constant, "#569CD6", "constant.language"),
            (Type, "#4EC9B0", "entity.name.type"),
            (Namespace, "#C8C8C8", "entity.name.namespace"),
            (Function, "#DCDCAA", "entity.name.function"),
            (Variable, "#9CDCFE", "variable"),
            (Parameter, "#9A9A9A", "variable.parameter (VS2019)"),
            (Property, "#DADADA", "variable.other.property"),
            (Macro, "#BEB7FF", "entity.name.function.preprocessor"),
            (EnumMember, "#B5CEA8", "variable.other.enummember"),
            (Preproc, "#9A9A9A", "keyword.control.directive"),
            (String, "#CE9178", "string"),
            (Number, "#B5CEA8", "constant.numeric"),
            (Comment, "#57A64A", "comment (VS2019)"),
            (Operator, "#B4B4B4", "punctuation/keyword.operator"),
        ];
        for (group, want, scope) in expect {
            assert_eq!(
                hex_of(&CPPTOOLS_DARK, *group),
                *want,
                "{} 应取自 `{scope}`",
                group.name()
            );
        }
        // 与 Dark+ 的关键差异必须真实存在（防止"改了名字没改数据"）
        assert_ne!(
            hex_of(&CPPTOOLS_DARK, Comment),
            hex_of(&DARK_PLUS, Comment),
            "C/C++ Themes 的注释色应与 Dark+ 不同"
        );
        assert_ne!(
            hex_of(&CPPTOOLS_DARK, Parameter),
            hex_of(&DARK_PLUS, Parameter),
            "C/C++ Themes 的形参色应与 Dark+ 不同"
        );
    }

    #[test]
    fn dark_plus_and_light_plus_match_official_themes() {
        use HighlightGroup::*;
        let dark: &[(HighlightGroup, &str)] = &[
            (Plain, "#D4D4D4"),
            (Keyword, "#569CD6"),
            (KeywordControl, "#C586C0"),
            (Type, "#4EC9B0"),
            (Function, "#DCDCAA"),
            (Variable, "#9CDCFE"),
            (Comment, "#6A9955"),
            (String, "#CE9178"),
            (Number, "#B5CEA8"),
            (EnumMember, "#4FC1FF"),
        ];
        for (g, want) in dark {
            assert_eq!(hex_of(&DARK_PLUS, *g), *want, "Dark+ {} 不符", g.name());
        }
        let light: &[(HighlightGroup, &str)] = &[
            (Plain, "#000000"),
            (Keyword, "#0000FF"),
            (KeywordControl, "#AF00DB"),
            (Type, "#267F99"),
            (Function, "#795E26"),
            (Variable, "#001080"),
            (Comment, "#008000"),
            (String, "#A31515"),
            (Number, "#098658"),
            (EnumMember, "#0070C1"),
        ];
        for (g, want) in light {
            assert_eq!(hex_of(&LIGHT_PLUS, *g), *want, "Light+ {} 不符", g.name());
        }
    }

    #[test]
    fn every_palette_covers_every_group_with_valid_hex() {
        for p in [&DARK_PLUS, &LIGHT_PLUS, &CPPTOOLS_DARK] {
            for (group, hex) in p.pairs() {
                assert_eq!(hex.len(), 7, "{} 色值长度不对：{hex}", group.name());
                assert!(hex.starts_with('#'), "{} 缺 #：{hex}", group.name());
                assert!(
                    hex[1..].chars().all(|c| c.is_ascii_hexdigit()),
                    "{} 不是合法十六进制：{hex}",
                    group.name()
                );
            }
        }
    }

    #[test]
    fn plain_token_color_equals_editor_foreground() {
        // 未着色文本必须和外壳前景色一致，否则会有"半截亮半截暗"的观感问题。
        for (kind, palette) in [
            (ThemeKind::DarkPlus, &DARK_PLUS),
            (ThemeKind::LightPlus, &LIGHT_PLUS),
            (ThemeKind::CppDark, &CPPTOOLS_DARK),
        ] {
            let ui = ide_core::theme::palette(kind);
            assert_eq!(
                hex_of(palette, HighlightGroup::Plain).to_ascii_lowercase(),
                ui.fg_editor.to_ascii_lowercase(),
                "{:?} 的 Plain 与 fg_editor 不一致",
                kind
            );
        }
    }

    #[test]
    fn activation_registers_three_themes_lookupable_by_kind() {
        let host = ide_ext::ExtensionHost::activate(vec![Box::new(VscodeThemes)]);
        assert_eq!(host.active_ids(), vec!["liteide.theme-vscode"]);
        let reg = host.registry();
        assert_eq!(reg.themes().len(), 3);
        for (kind, want) in [
            (ThemeKind::DarkPlus, "#C586C0"),
            (ThemeKind::LightPlus, "#AF00DB"),
            (ThemeKind::CppDark, "#D8A0DF"),
        ] {
            let tokens = reg.tokens(kind).unwrap_or_else(|| panic!("{kind:?} 无配色"));
            assert_eq!(
                tokens.hex(HighlightGroup::KeywordControl),
                want,
                "{kind:?} 控制流关键字色不符"
            );
        }
    }
}
