# 扩展开发指南

LiteIDE 的**语法高亮**和**主题配色**都是扩展，不是硬编码在编辑器里的。
内置扩展用同一套机制（`ide-ext` 宿主 + 清单 + 贡献点），新增一种语言或一套配色
只是**加一个 crate**，不需要改编辑器、不需要改 `ide-ui`。

> 设计动机：功能增长不能拖慢启动与打字。扩展 = 编译期清单 + 激活时"只注册"，
> 稳态（逐帧/逐键）路径上不新增分配、不新增查表。详见下文「性能契约」。

## 目录

- [结构总览](#结构总览)
- [概念：清单 / 贡献点 / 注册表](#概念清单--贡献点--注册表)
- [写一个语言高亮扩展](#写一个语言高亮扩展)
- [写一个主题配色扩展](#写一个主题配色扩展)
- [性能契约](#性能契约)
- [失败隔离与回滚](#失败隔离与回滚)
- [新增一类贡献点](#新增一类贡献点)
- [测试与验收](#测试与验收)

## 结构总览

```
crates/
  ide-core/          HighlightGroup（token 分组词汇表）、ThemeKind（外壳色板）
  ide-ext/           宿主：Manifest / Capability / Registry / ExtContext /
                     ExtensionHost / TokenPalette / LanguageHighlighter / TextEdit
  ext-cpp-highlight/ 内置扩展：C/C++ 语法高亮（tree-sitter-cpp + 自制查询）
  ext-theme-vscode/  内置扩展：Dark+ / Light+ / Dark (Visual Studio - C/C++)
  ide-ui/            前端：启动时激活内置扩展，按后缀向注册表要高亮器与配色
```

数据流（都在**打开文档 / 切换主题**时发生，不是每帧）：

```
启动:  ExtensionHost::activate(vec![Box::new(CppHighlight), Box::new(VscodeThemes)])
           │  每个扩展 activate() 只做 ctx.register_*，宿主计时 + 校验 + 回滚
           ▼
       Registry { languages: [...], themes: [...] }
           │
打开文件: registry.create_highlighter(path) -> Box<dyn LanguageHighlighter>   # 后缀匹配
           │
切换主题: registry.tokens(kind) -> &TokenPalette -> TokenColors -> [Color32; COUNT]
```

编辑器**不认识任何具体语言**：它只持有 `Box<dyn LanguageHighlighter>` 和一张定长颜色表。

## 概念：清单 / 贡献点 / 注册表

| 概念 | 位置 | 说明 |
|---|---|---|
| `Manifest` | `ide-ext::manifest` | 编译期常量：id / 名字 / 版本 / 描述 / **贡献点声明** |
| `Capability` | 同上 | 目前有 `Language(LanguageContribution)` 与 `Theme(ThemeContribution)` |
| `Extension` | 同上 | 实现 `manifest()` + `activate(&mut ExtContext)` |
| `Registry` | `ide-ext::host` | 激活后的只读查询入口（语言 / 配色） |
| `ExtContext` | 同上 | 激活期唯一的写入口：`register_language` / `register_theme` |
| `ExtensionHost` | 同上 | 按顺序激活、计时、校验、失败回滚、记录 `report()` |

查询语义（刻意不对称，理由都写在注释里）：

- **语言：先注册者优先**（`language_for_path`）——内置语言不会被后来者顶掉。
  匹配文件后缀（大小写不敏感），无后缀 / 隐藏文件返回 `None`（→ 纯文本）。
- **主题配色：后注册者优先**（`tokens`）——第三方扩展可以覆盖内置配色。
- 找不到语言 → `NullHighlighter`（无色、不 panic）；找不到配色 → 调用方退回内置 Dark+。

写扩展的 `activate` 有两种写法：

```rust
// 纯声明式：清单里写了什么就注册什么（推荐，声明与实现不分家）
fn activate(&mut self, ctx: &mut ExtContext<'_>) -> Result<(), ExtError> {
    ide_ext::register_declared(ctx, &MANIFEST)
}

// 需要条件注册时自己写（但要保证"声明数 == 注册数"，否则宿主判定激活失败）
fn activate(&mut self, ctx: &mut ExtContext<'_>) -> Result<(), ExtError> {
    if std::env::var("LITEIDE_NO_CPP").is_ok() { return Ok(()); }  // ← 反例：声明了却没注册
    ...
}
```

## 写一个语言高亮扩展

以新增 **Rust 高亮** 为例（照 `ext-cpp-highlight` 抄即可）。

1) 建 crate，加依赖：

```toml
# crates/ext-rust-highlight/Cargo.toml
[package]
name = "ext-rust-highlight"
version.workspace = true
edition.workspace = true

[dependencies]
ide-core = { path = "../ide-core" }   # HighlightGroup / Highlight
ide-ext  = { path = "../ide-ext" }    # 清单 / trait / 上下文
tracing.workspace = true
tree-sitter = "0.25"
tree-sitter-rust = "0.23"
```

2) 写查询文件 `queries/rust-highlights.scm`，把语法节点映射到 `HighlightGroup`：

```scheme
(comment) @comment
(string_literal) @string
[
  "fn" "let" "mut" "impl" "pub"
] @keyword
[
  "if" "else" "for" "while" "match" "return"
] @control
(function_item name: (identifier) @function)
(type_identifier) @type
(identifier) @variable
```

约定：`@变量名` 只是个标签，真正的分组映射写在 `capture_to_group()` 里
（`@field` → `Property`、`@enumMember` → `EnumMember`……）。
**同一个节点叠多条规则时优先级用 `Spec`**：`Fallback < Specific < Override`。

3) 实现扩展与高亮器（要点见注释）：

```rust
use ide_core::{Highlight, HighlightGroup};
use ide_ext::{Capability, ExtContext, ExtError, Extension, LanguageContribution,
               LanguageHighlighter, Manifest, TextEdit};
use std::sync::OnceLock;

/// 进程内共享：Language 与 Query 都只构建一次（激活/打开文档时若重建会明显变慢）。
fn language() -> tree_sitter::Language { /* OnceLock<Language> */ }
fn query() -> Option<&'static tree_sitter::Query> { /* OnceLock<Option<Query>> */ }

pub struct RustHighlighter {
    parser: tree_sitter::Parser,
    /// 上一次解析的语法树：增量解析用（一个文档实例一份，不跨文档共享）。
    tree: Option<tree_sitter::Tree>,
}

impl LanguageHighlighter for RustHighlighter {
    fn highlight(&mut self, text: &str, edit: Option<TextEdit>) -> Vec<Highlight> {
        // 1) 有 TextEdit 且旧树覆盖旧文本 → 克隆旧树 + tree.edit(..) → parser.parse(text, Some(old))
        // 2) 否则全量 parse(text, None)
        // 3) 解析完**必须**校验 root.end_byte() == text.len()，不满足就重解析（宁可慢，不可脏）
        // 4) 把 tree-sitter 捕获映射成 Highlight，按 start 升序返回
        todo!()
    }
    fn reset(&mut self) { self.tree = None; }   // 换文档/外部改动后丢弃增量缓存
}

pub struct RustHighlight;

static MANIFEST: Manifest = Manifest {
    id: "liteide.rust-highlight",
    name: "Rust 语法高亮",
    version: "0.1.0",
    description: "tree-sitter-rust 语法高亮",
    capabilities: &[Capability::Language(LanguageContribution {
        id: "rust",
        name: "Rust",
        extensions: &["rs"],
        create: || Box::new(RustHighlighter::new()),
    })],
};

impl Extension for RustHighlight {
    fn manifest(&self) -> &'static Manifest { &MANIFEST }
    fn activate(&mut self, ctx: &mut ExtContext<'_>) -> Result<(), ExtError> {
        ide_ext::register_declared(ctx, &MANIFEST)
    }
}
```

4) 注册到应用（`crates/ide-ui/src/main.rs` 的 `builtin_extensions()`）与工作区成员
（根 `Cargo.toml` 的 `members`、`ide-ui/Cargo.toml` 的依赖）：

```rust
fn builtin_extensions() -> Vec<Box<dyn ide_ext::Extension>> {
    vec![
        Box::new(ext_cpp_highlight::CppHighlight),
        Box::new(ext_rust_highlight::RustHighlight),   // ← 只加这一行
        Box::new(ext_theme_vscode::VscodeThemes),
    ]
}
```

打开 `.rs` 文件时，`main.rs` 会 `registry.create_highlighter(path)`，
语言就自动认领了——编辑器本身一行都不用改。

**高亮器的两条硬性要求**（不做就会出现"看起来随机"的脏数据）：

1. `highlight()` 返回值必须是**字节偏移 + 升序**，且不能越界（渲染层按字节切片）。
2. 增量解析要么正确、要么退化为全量：绝不返回"过期语法树"。
   历史 bug 参考：把上一版的（更短的）语法树直接交给 `parse()`，会得到只覆盖旧长度的树，
   于是**新追加的文本没有高亮**。修复方式 = 调用方给 `TextEdit` + 解析后校验覆盖全文。

## 写一个主题配色扩展

主题扩展只贡献 **token 配色**；窗口外壳（编辑区背景、行号、状态栏、选区……）
仍由 `ide_core::theme::palette(ThemeKind)` 决定，避免"改一处颜色导致整体不协调"。

```rust
use ide_core::HighlightGroup as G;
use ide_core::theme::ThemeKind;
use ide_ext::{Capability, ExtContext, ExtError, Extension, Manifest, ThemeContribution, TokenPalette};

/// 按 `HighlightGroup::ALL` 的顺序给出**全部 17 个**颜色（编译期常量，零分配）。
/// 少写/多写都会在编译期报错；注释里写清每个色值来源的 VSCode scope 便于日后核对。
pub const MY_DARK: TokenPalette = TokenPalette::from_array([
    "#DADADA", // Plain          editor.foreground
    "#569CD6", // Keyword        keyword / storage / storage.type.primitive
    "#D8A0DF", // KeywordControl keyword.control
    "#569CD6", // Constant       constant.language
    "#4EC9B0", // Type           entity.name.type / support.type
    "#C8C8C8", // Namespace      entity.name.namespace
    "#DCDCAA", // Function       entity.name.function / support.function
    "#9CDCFE", // Variable       variable
    "#9A9A9A", // Parameter      variable.parameter
    "#DADADA", // Property       variable.other.property
    "#BEB7FF", // Macro          entity.name.function.preprocessor
    "#B5CEA8", // EnumMember     variable.other.enummember
    "#9A9A9A", // Preproc        keyword.control.directive
    "#CE9178", // String         string
    "#B5CEA8", // Number         constant.numeric
    "#57A64A", // Comment        comment
    "#B4B4B4", // Operator       punctuation / keyword.operator
]);

static MANIFEST: Manifest = Manifest {
    id: "liteide.theme-mine",
    name: "我的配色",
    version: "0.1.0",
    description: "自定义 token 配色",
    capabilities: &[Capability::Theme(ThemeContribution {
        id: "mine-dark",
        name: "Dark (Mine)",
        kind: ThemeKind::DarkPlus,          // 借用的外壳色板
        tokens: MY_DARK,
    })],
};
```

- `TokenPalette::from_array([...])` 是 `const` 构造（编译期检查长度 = `HighlightGroup::COUNT`）；
  运行期构造可用 `TokenPalette::new(&[(G::Keyword, "#569CD6")], "#DADADA")`，
  未列出的分组回退到第二个参数。
- 「后注册者优先」：想覆盖内置主题的配色，注册一个同 `kind` 的主题即可。
- 新增一套**外壳色板**（背景/状态栏等）要动 `ide-core::theme`（`ThemeKind` + `palette()`），
  属于 core 的改动，不是扩展能做的事——这是刻意的边界。

## 性能契约

> 这是"轻量 IDE"的必要性所在：所有扩展都必须保证性能。

| 阶段 | 允许做的事 | 实测/预算 |
|---|---|---|
| 激活（每个扩展一次） | 只注册；无 IO、无重计算 | 内置两个扩展各 ~2 µs，总计 < 50 ms（测试设上限） |
| 打开文档 | 构造高亮器（`Language`/`Query` 靠 `OnceLock` 共享） | 123 KB C++：47 ms（含全量解析，一次性） |
| 每帧绘制 | **什么都不做**：颜色已展开成 `[Color32; COUNT]`，token 区间来自缓存 | 零分配、零查表 |
| 打字（每次文本变更） | 增量解析 + 重新收集捕获 | 123 KB C++：6 ms（编辑器给了编辑描述）；不给编辑描述 = 全量重解析 28 ms |

细则：

1. **激活只许注册**。宿主动态计时（`ExtensionHost::report()`），设置面板的
   Extension 列表能看到每个扩展的贡献点数与耗时；单元测试对总耗时设了上限。
2. **重资源必须共享**：`Language`/`Query`/正则等用 `OnceLock`。
   每打开一个文档调用一次工厂，工厂要是"重建 grammar"级别的开销就会被用户感知。
3. **颜色只在切换主题时展开**：`TokenPalette`（字符串）→ `TokenColors`（`[Color32; N]`），
   绘制路径上只做一次数组下标。
4. **高亮走编辑描述**：编辑器用 `TextEdit::between(old, new)` 把"改了哪一段"交给扩展，
   tree-sitter 才能复用旧树。没给编辑描述就是全量解析——正确但慢。
5. **测量再优化**：`cargo test -p ext-cpp-highlight --release --test bench_highlight -- --ignored --nocapture`
   会打印真实数字（读取/Buffer/全量高亮/增量高亮/50 次追加的最坏耗时）。

## 失败隔离与回滚

`ExtensionHost::activate` 对每个扩展做三件事：

1. `catch_unwind` 包住 `activate()`；
2. 校验"清单声明的贡献点数 == 实际注册数"（防止声明与实现漂移）；
3. 任何 `Err` / 数量不符 / panic → **原子回滚**该扩展注册的全部贡献点并禁用，
   其余扩展照常工作（`report()` 里能看到 `Failed`/`Panicked` 与原因）。

注意：release 档用 `panic = "abort"`（体积/启动优先），此时 `catch_unwind` 兜不住 panic，
进程会直接终止。因此**扩展代码不能把 panic 当成正常控制流**；测试/调试档会兜住并给出可读信息。

## 新增一类贡献点

比如"命令"（`Capability::Command`）：

1. `ide-ext/src/manifest.rs`：加变体 + `kind()` 分支（给出 id/名字/回调签名）；
2. `ide-ext/src/host.rs`：`Registry` 加存储 + 查询方法，`ExtContext` 加 `register_*`
   （**先查重再 push，并 `self.registered += 1`**），`run_activation` 的回滚数组跟着加一维；
3. `ide-ext/src/lib.rs` 里 `pub use` 出来；
4. 本文件补一节"契约 + 性能预算"；
5. 加测试：注册成功 / id 重复 / 声明数量不符 / panic 回滚。

将来若要做"动态加载"（dylib / WASM），清单与贡献点结构不变，只换加载器与
`Extension` 的宿主边界（现在的 trait 已经把 IO/UI 挡在扩展之外）。

## 测试与验收

| 测试 | 位置 | 覆盖 |
|---|---|---|
| 宿主语义 | `ide-ext/src/host.rs`（9 个） | 注册/查询/后缀匹配/重复 id/数量不符/panic 回滚/激活耗时 |
| `TextEdit::between` | `ide-ext/src/lib.rs` | 追加/删除/替换/多字节边界/行号/未变化 |
| 查询能编译 | `ext-cpp-highlight`（`query_compiles_without_error`） | 写错节点名会让高亮**整体失效**，必须有这个守卫 |
| 分组映射 | `ext-cpp-highlight` | 每个分组至少一处可达、区间合法、确定性 |
| 增量正确性 | `ext-cpp-highlight` | 逐步打字的结果必须与全量解析**逐字节一致** |
| 主题色值 | `ext-theme-vscode` | 17 组 hex 逐项核对 + 色值格式 + 与 `palette(kind).fg_editor` 一致 |
| 端到端 | `ide-ui/src/editor.rs` | 注入扩展 → 有高亮；打字 → 增量结果 == 全量结果 |

人工验收（截图 + 取样）：

```powershell
# 1) 打开 testdata/highlight_demo.cpp（覆盖 17 个分组）
# 2) 截图后按调色板取样，确认渲染用的确实是扩展的配色：
powershell -File …\checkpalette.ps1 -Path <shot.png>   # 13 组色值都应命中，旧色值应为 MISSING
```

### 已知偏差（相对 VSCode + C/C++ Themes）

| 现象 | 原因 |
|---|---|
| 全大写标识符（`MAX_RETRY`）按宏着色 | 语法层分不清"常量变量"与"宏"，用命名约定近似 |
| 同一文件内声明的枚举成员才着色 | 需要语义信息（clangd），tree-sitter 只做局部推断 |
| 全局变量按局部变量着色 | 同上；VSCode 也是靠 clangd 语义高亮细分 `variable.other.global` |
| 字符串内的转义序列不再细分颜色 | 查询不做子节点切分（`\n` 与普通字符同色） |
| `#define` 宏体（`((x) * (x))`）按普通 token 着色 | 宏体不参与语法解析，VSCode 也基本不着色 |
| 字符字面量 `'\t'` 与字符串同色 | 两者都映射到 `String` 分组 |
| 数字后缀 / 浮点指数符号不单独着色 | 保持查询简单、命中率高；如需可在查询里加 `keyword.operator.*` 规则 |
