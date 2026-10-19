//! 扩展清单与贡献点：等价于 VSCode 扩展 `package.json` 里的 `contributes`。
//!
//! 清单是**编译期常量**（`&'static`），所以加载/校验不产生任何分配。

use crate::palette::TokenPalette;
use crate::{ExtContext, ExtError, LanguageHighlighter};
use ide_core::theme::ThemeKind;

/// 扩展清单。
#[derive(Debug, Clone, Copy)]
pub struct Manifest {
    /// 唯一 id，约定 `liteide.<名字>`，如 `liteide.cpp-highlight`。
    pub id: &'static str,
    /// 展示名（设置面板"扩展"页用）。
    pub name: &'static str,
    pub version: &'static str,
    pub description: &'static str,
    /// 声明式贡献点：声明了几个，激活时就必须成功注册几个（否则整份激活回滚）。
    pub capabilities: &'static [Capability],
}

/// 扩展能提供的一类东西。
///
/// 新增一类功能（命令、视图、格式化器……）就是：加一个变体 + 在 [`crate::Registry`]
/// 上加一个查询方法 + 在 `docs/extensions.md` 里写清楚契约与性能预算。
// 清单是编译期常量（每个扩展 1~3 条），变体尺寸差无关紧要；
// 刻意不 Box：`TokenPalette` 保持 Copy，激活时零分配。
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, Copy)]
pub enum Capability {
    /// 语言贡献：文件后缀 → 语法高亮器工厂。
    Language(LanguageContribution),
    /// 主题贡献：一套 token 配色。
    Theme(ThemeContribution),
}

impl Capability {
    /// 贡献点类别名（诊断输出用）。
    pub fn kind(&self) -> &'static str {
        match self {
            Capability::Language(_) => "language",
            Capability::Theme(_) => "theme",
        }
    }
}

/// 语言贡献。
#[derive(Debug, Clone, Copy)]
pub struct LanguageContribution {
    /// 唯一 id，如 `cpp`。
    pub id: &'static str,
    /// 展示名，如 `C++`。
    pub name: &'static str,
    /// 文件后缀（**不含点**，大小写不敏感），如 `["c", "h", "cpp", "hpp"]`。
    pub extensions: &'static [&'static str],
    /// 高亮器工厂：每打开一个文档调用一次，**必须足够便宜**——语言、查询等重资源
    /// 要在扩展内部用 `OnceLock` 共享，不能每次重建。
    pub create: fn() -> Box<dyn LanguageHighlighter>,
}

/// 主题贡献：只负责 token 配色；窗口外壳（背景/行号/选区……）仍由 core 的
/// [`ThemeKind`] 决定，避免扩展改一处色导致整体不协调。
#[derive(Debug, Clone, Copy)]
pub struct ThemeContribution {
    /// 唯一 id，如 `vscode-cpptools-dark`。
    pub id: &'static str,
    /// 展示名，如 `Dark (Visual Studio - C/C++)`。
    pub name: &'static str,
    /// 对应的外壳色板。
    pub kind: ThemeKind,
    pub tokens: TokenPalette,
}

/// 一个扩展。
///
/// 实现者只允许在 [`Extension::activate`] 里做"注册"，禁止文件/网络 IO 与重计算：
/// 宿主会计时并记录（见 [`crate::ExtensionHost::report`]），测试里对总耗时设了上限。
pub trait Extension: Send + Sync {
    /// 清单（`&'static`，可在构造时确定为常量）。
    fn manifest(&self) -> &'static Manifest;

    /// 激活：把清单里声明的贡献点注册进 [`ExtContext`]。
    ///
    /// 返回 `Err` 表示该扩展被禁用：它已注册的贡献点会被**原子回滚**，不影响其它扩展。
    fn activate(&mut self, ctx: &mut ExtContext<'_>) -> Result<(), ExtError>;

    /// 卸载（当前仅保留语义，UI 侧不调用）。
    fn deactivate(&mut self) {}
}
