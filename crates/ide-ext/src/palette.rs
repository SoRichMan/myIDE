//! token 配色表：按 `HighlightGroup` 索引的十六进制色值。
//!
//! 刻意做成"数组下标"而不是 `HashMap`：配色在**主题切换时**一次性转成
//! `egui::Color32` 表，绘制路径上只剩一次数组访问（稳态零开销）。

use ide_core::HighlightGroup;

/// 一套 token 配色。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokenPalette {
    colors: [&'static str; HighlightGroup::COUNT],
}

impl TokenPalette {
    /// 按 [`HighlightGroup::ALL`] 的顺序直接给出全部颜色——**编译期常量表**，
    /// 主题扩展用它把色值写成 `const`。
    pub const fn from_array(colors: [&'static str; HighlightGroup::COUNT]) -> Self {
        Self { colors }
    }

    /// 用 `(分组, "#RRGGBB")` 列表构造；未列出的分组回退到 `plain`。
    ///
    /// 只在扩展激活时调用一次，之后是只读查表数据。
    pub fn new(pairs: &[(HighlightGroup, &'static str)], plain: &'static str) -> Self {
        let mut colors = [plain; HighlightGroup::COUNT];
        for (group, hex) in pairs {
            colors[group.index()] = hex;
        }
        Self { colors }
    }

    /// 取分组颜色（`#RRGGBB`）。
    pub fn hex(&self, group: HighlightGroup) -> &'static str {
        self.colors[group.index()]
    }

    /// 全部分组颜色（按 `HighlightGroup::ALL` 顺序）；测试/设置面板预览用。
    pub fn pairs(&self) -> impl Iterator<Item = (HighlightGroup, &'static str)> + '_ {
        HighlightGroup::ALL.into_iter().map(move |g| (g, self.hex(g)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unlisted_groups_fall_back_to_plain() {
        let p = TokenPalette::new(&[(HighlightGroup::Keyword, "#123456")], "#abcdef");
        assert_eq!(p.hex(HighlightGroup::Keyword), "#123456");
        assert_eq!(p.hex(HighlightGroup::Comment), "#abcdef");
        assert_eq!(p.hex(HighlightGroup::Plain), "#abcdef");
    }

    #[test]
    fn lookup_covers_every_group_without_allocation() {
        let all: Vec<(HighlightGroup, &'static str)> =
            HighlightGroup::ALL.iter().map(|g| (*g, "#010203")).collect();
        let p = TokenPalette::new(&all, "#010203");
        assert_eq!(p.pairs().count(), HighlightGroup::COUNT);
        for (g, hex) in p.pairs() {
            assert_eq!(p.hex(g), "#010203", "{g:?} 查表失败");
            assert_eq!(hex, "#010203");
        }
    }
}
