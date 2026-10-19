//! 扩展宿主：注册表（UI 只读）+ 激活器（计时 / 校验 / 失败隔离）。

use crate::manifest::{Extension, LanguageContribution, ThemeContribution};
use crate::palette::TokenPalette;
use crate::{ExtError, LanguageHighlighter, NullHighlighter};
use ide_core::theme::ThemeKind;
use std::time::{Duration, Instant};

/// 注册表：扩展激活后的全部贡献点。
///
/// 查询都是"少量线性扫描 + 数组下标"级别，发生在**打开文档 / 切换主题**时，
/// 不在逐帧绘制路径上。
#[derive(Debug, Default, Clone)]
pub struct Registry {
    languages: Vec<LanguageContribution>,
    themes: Vec<ThemeContribution>,
}

impl Registry {
    /// 已注册的语言（按注册顺序）。
    pub fn languages(&self) -> &[LanguageContribution] {
        &self.languages
    }

    /// 已注册的主题（按注册顺序）。
    pub fn themes(&self) -> &[ThemeContribution] {
        &self.themes
    }

    /// 按 id 查语言。
    pub fn language(&self, id: &str) -> Option<&LanguageContribution> {
        self.languages.iter().find(|l| l.id == id)
    }

    /// 按文件路径的后缀查语言（大小写不敏感）。
    ///
    /// **先注册者优先**：内置扩展先注册，因此内置语言不会被后来者顶掉。
    pub fn language_for_path(&self, path: &str) -> Option<&LanguageContribution> {
        let ext = extension_of(path)?;
        self.languages
            .iter()
            .find(|l| l.extensions.iter().any(|e| e.eq_ignore_ascii_case(ext)))
    }

    /// 按路径创建高亮器；没有匹配语言时返回 [`NullHighlighter`]（不 panic、无高亮）。
    pub fn create_highlighter(&self, path: &str) -> Box<dyn LanguageHighlighter> {
        match self.language_for_path(path) {
            Some(l) => (l.create)(),
            None => Box::new(NullHighlighter),
        }
    }

    /// 取某个外壳色板对应的 token 配色。
    ///
    /// **后注册者优先**：内置主题先注册，因此第三方扩展可以覆盖内置配色。
    pub fn tokens(&self, kind: ThemeKind) -> Option<&TokenPalette> {
        self.themes.iter().rev().find(|t| t.kind == kind).map(|t| &t.tokens)
    }

    /// 贡献点数量（`语言, 主题`），测试/诊断用。
    pub fn counts(&self) -> (usize, usize) {
        (self.languages.len(), self.themes.len())
    }
}

/// 取文件名后缀（不含点）。无后缀 / 隐藏文件返回 `None`。
fn extension_of(path: &str) -> Option<&str> {
    let file = path.rsplit(['/', '\\']).next().unwrap_or(path);
    let (stem, ext) = file.rsplit_once('.')?;
    if stem.is_empty() || ext.is_empty() {
        return None;
    }
    Some(ext)
}

/// 扩展激活时拿到的注册入口——**扩展唯一可用的 API**（拿不到 UI、文件系统、线程）。
pub struct ExtContext<'a> {
    ext_id: &'static str,
    registry: &'a mut Registry,
    registered: usize,
}

impl ExtContext<'_> {
    /// 当前扩展的 id（诊断用）。
    pub fn extension_id(&self) -> &'static str {
        self.ext_id
    }

    /// 注册语言贡献。id 重复或声明非法返回 `Err`。
    pub fn register_language(&mut self, c: LanguageContribution) -> Result<(), ExtError> {
        if self.registry.languages.iter().any(|l| l.id == c.id) {
            return Err(ExtError::DuplicateId(c.id));
        }
        if c.extensions.is_empty() {
            return Err(ExtError::Invalid("language.extensions 不能为空"));
        }
        self.registry.languages.push(c);
        self.registered += 1;
        Ok(())
    }

    /// 注册主题贡献。id 重复返回 `Err`。
    pub fn register_theme(&mut self, c: ThemeContribution) -> Result<(), ExtError> {
        if self.registry.themes.iter().any(|t| t.id == c.id) {
            return Err(ExtError::DuplicateId(c.id));
        }
        self.registry.themes.push(c);
        self.registered += 1;
        Ok(())
    }

    /// 本次激活已成功注册的贡献点数（宿主用它校验清单）。
    pub fn registered(&self) -> usize {
        self.registered
    }
}

/// 单个扩展的激活结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActivationOutcome {
    /// 正常激活（贡献点已注册）。
    Active,
    /// 扩展返回 `Err`：贡献点已回滚，该扩展被禁用。
    Failed(String),
    /// 扩展 panic：贡献点已回滚（release 档 `panic = "abort"` 时直接终止进程）。
    Panicked(String),
}

/// 激活记录（耗时 + 结果）。
#[derive(Debug, Clone)]
pub struct ActivationRecord {
    pub id: &'static str,
    pub name: &'static str,
    /// 成功注册的贡献点数量（清单声明数量应与之相等）。
    pub registered: usize,
    pub duration: Duration,
    pub outcome: ActivationOutcome,
}

impl ActivationRecord {
    /// 是否激活成功。
    pub fn is_active(&self) -> bool {
        self.outcome == ActivationOutcome::Active
    }
}

/// 扩展宿主：持有扩展实例与注册表。
pub struct ExtensionHost {
    extensions: Vec<Box<dyn Extension>>,
    registry: Registry,
    report: Vec<ActivationRecord>,
    total: Duration,
}

impl ExtensionHost {
    /// 构造并**立即激活**全部扩展（一个扩展失败不影响其它扩展）。
    ///
    /// 激活会做两件事作为契约检查：
    /// 1. 清单声明了几个贡献点，就必须成功注册几个（否则视为激活失败并回滚）；
    /// 2. 记录每个扩展的激活耗时，[`Self::report`] 可查（测试对总耗时设上限）。
    pub fn activate(extensions: Vec<Box<dyn Extension>>) -> Self {
        let mut host = Self {
            extensions,
            registry: Registry::default(),
            report: Vec::new(),
            total: Duration::ZERO,
        };
        host.run_activation();
        host
    }

    fn run_activation(&mut self) {
        let t_all = Instant::now();
        for ext in self.extensions.iter_mut() {
            let manifest = ext.manifest();
            let (langs_before, themes_before) = self.registry.counts();
            let t0 = Instant::now();

            let mut ctx = ExtContext {
                ext_id: manifest.id,
                registry: &mut self.registry,
                registered: 0,
            };
            // catch_unwind：dev/test 档能兜住 panic；release 档 panic=abort 会先终止进程。
            let result =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| ext.activate(&mut ctx)));
            let duration = t0.elapsed();

            let outcome = match result {
                Ok(Ok(())) => {
                    let declared = manifest.capabilities.len();
                    if declared == ctx.registered() {
                        ActivationOutcome::Active
                    } else {
                        ActivationOutcome::Failed(
                            ExtError::CapabilityMismatch {
                                declared,
                                registered: ctx.registered(),
                            }
                            .to_string(),
                        )
                    }
                }
                Ok(Err(e)) => ActivationOutcome::Failed(e.to_string()),
                Err(payload) => ActivationOutcome::Panicked(panic_message(&payload)),
            };
            let registered = ctx.registered();

            if !matches!(outcome, ActivationOutcome::Active) {
                // 原子回滚：失败的扩展不留半个贡献点。
                self.registry.languages.truncate(langs_before);
                self.registry.themes.truncate(themes_before);
                tracing::warn!(
                    ext = manifest.id,
                    err = %outcome_message(&outcome),
                    "扩展激活失败，已回滚并禁用"
                );
            } else {
                tracing::debug!(
                    ext = manifest.id,
                    us = duration.as_micros() as u64,
                    "扩展已激活"
                );
            }

            self.report.push(ActivationRecord {
                id: manifest.id,
                name: manifest.name,
                registered,
                duration,
                outcome,
            });
        }
        self.total = t_all.elapsed();
    }

    /// 只读注册表（UI 用它查语言与配色）。
    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    /// 每个扩展的激活记录（顺序 = 注册顺序）。
    pub fn report(&self) -> &[ActivationRecord] {
        &self.report
    }

    /// 全部扩展激活总耗时（测试里的性能上限断言）。
    pub fn total_activation(&self) -> Duration {
        self.total
    }

    /// 激活成功的扩展 id。
    pub fn active_ids(&self) -> Vec<&'static str> {
        self.report
            .iter()
            .filter(|r| r.is_active())
            .map(|r| r.id)
            .collect()
    }
}

fn outcome_message(outcome: &ActivationOutcome) -> String {
    match outcome {
        ActivationOutcome::Active => "ok".to_string(),
        ActivationOutcome::Failed(m) | ActivationOutcome::Panicked(m) => m.clone(),
    }
}

/// 把 panic payload 转成可读文本（`panic!("{}", s)` / `panic!("literal")` 两种都覆盖）。
fn panic_message(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "panic（无文本负载）".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{Capability, Manifest};
    use ide_core::HighlightGroup;

    const EMPTY: &[Capability] = &[];

    /// 测试用最小色板（静态上下文里只能用 const 构造）。
    const NO_TOKENS: TokenPalette = TokenPalette::from_array(["#000000"; HighlightGroup::COUNT]);

    static OK_MANIFEST: Manifest = Manifest {
        id: "test.ok",
        name: "Ok",
        version: "0.1.0",
        description: "注册一个语言 + 一个主题",
        capabilities: &[
            Capability::Language(LanguageContribution {
                id: "testlang",
                name: "Test",
                extensions: &["tst"],
                create: || Box::new(NullHighlighter),
            }),
            Capability::Theme(ThemeContribution {
                id: "testtheme",
                name: "Test Theme",
                kind: ThemeKind::DarkPlus,
                tokens: NO_TOKENS,
            }),
        ],
    };

    struct OkExt;
    impl Extension for OkExt {
        fn manifest(&self) -> &'static Manifest {
            &OK_MANIFEST
        }
        fn activate(&mut self, ctx: &mut ExtContext<'_>) -> Result<(), ExtError> {
            ctx.register_language(LanguageContribution {
                id: "testlang",
                name: "Test",
                extensions: &["tst"],
                create: || Box::new(NullHighlighter),
            })?;
            ctx.register_theme(ThemeContribution {
                id: "testtheme",
                name: "Test Theme",
                kind: ThemeKind::DarkPlus,
                tokens: TokenPalette::new(&[(ide_core::HighlightGroup::Keyword, "#111111")], "#000"),
            })
        }
    }

    /// 声明 1 个贡献点却注册 2 个 → 应判定失败并回滚。
    static MISMATCH_MANIFEST: Manifest = Manifest {
        id: "test.mismatch",
        name: "Mismatch",
        version: "0.1.0",
        description: "清单与实现不一致",
        capabilities: EMPTY,
    };

    struct MismatchExt;
    impl Extension for MismatchExt {
        fn manifest(&self) -> &'static Manifest {
            &MISMATCH_MANIFEST
        }
        fn activate(&mut self, ctx: &mut ExtContext<'_>) -> Result<(), ExtError> {
            ctx.register_theme(ThemeContribution {
                id: "ghost",
                name: "Ghost",
                kind: ThemeKind::LightPlus,
                tokens: TokenPalette::new(&[], "#fff"),
            })
        }
    }

    static DUPE_MANIFEST: Manifest = Manifest {
        id: "test.dupe",
        name: "Dupe",
        version: "0.1.0",
        description: "重复 id",
        capabilities: EMPTY,
    };

    struct DupeExt;
    impl Extension for DupeExt {
        fn manifest(&self) -> &'static Manifest {
            &DUPE_MANIFEST
        }
        fn activate(&mut self, ctx: &mut ExtContext<'_>) -> Result<(), ExtError> {
            // 先占住 id，第二次同 id 注册必须报错。
            ctx.register_language(LanguageContribution {
                id: "dup",
                name: "Dup",
                extensions: &["dup"],
                create: || Box::new(NullHighlighter),
            })?;
            ctx.register_language(LanguageContribution {
                id: "dup",
                name: "Dup",
                extensions: &["dup"],
                create: || Box::new(NullHighlighter),
            })
        }
    }

    static PANIC_MANIFEST: Manifest = Manifest {
        id: "test.panic",
        name: "Panic",
        version: "0.1.0",
        description: "激活时 panic",
        capabilities: EMPTY,
    };

    struct PanicExt;
    impl Extension for PanicExt {
        fn manifest(&self) -> &'static Manifest {
            &PANIC_MANIFEST
        }
        fn activate(&mut self, ctx: &mut ExtContext<'_>) -> Result<(), ExtError> {
            ctx.register_theme(ThemeContribution {
                id: "half",
                name: "Half",
                kind: ThemeKind::CppDark,
                tokens: TokenPalette::new(&[], "#123456"),
            })?;
            panic!("boom")
        }
    }

    #[test]
    fn activates_and_exposes_language_and_theme() {
        let host = ExtensionHost::activate(vec![Box::new(OkExt)]);
        assert_eq!(host.active_ids(), vec!["test.ok"]);
        assert_eq!(host.report()[0].registered, 2, "应记录成功注册的贡献点数");
        let reg = host.registry();
        assert_eq!(reg.counts(), (1, 1));
        assert_eq!(reg.language("testlang").map(|l| l.name), Some("Test"));
        // 后缀匹配大小写不敏感
        assert_eq!(reg.language_for_path("a/b/c.TST").map(|l| l.id), Some("testlang"));
        let tokens = reg.tokens(ThemeKind::DarkPlus).expect("主题应已注册");
        assert_eq!(tokens.hex(ide_core::HighlightGroup::Keyword), "#111111");
    }

    #[test]
    fn unknown_path_falls_back_to_null_highlighter() {
        let host = ExtensionHost::activate(vec![Box::new(OkExt)]);
        let mut h = host.registry().create_highlighter("main.cpp");
        assert!(h.highlight("int main(){}", None).is_empty(), "未知语言应为空高亮");
        // 已注册语言走工厂
        let mut h2 = host.registry().create_highlighter("x.tst");
        assert!(h2.highlight("whatever", None).is_empty(), "工厂产物也应是空高亮");
    }

    #[test]
    fn manifest_mismatch_is_rolled_back() {
        let host = ExtensionHost::activate(vec![Box::new(MismatchExt)]);
        assert!(host.active_ids().is_empty());
        assert_eq!(host.registry().counts(), (0, 0), "失败的扩展必须回滚干净");
        assert_eq!(host.report()[0].registered, 1, "仍应记录它尝试注册了几个");
        assert!(matches!(
            host.report()[0].outcome,
            ActivationOutcome::Failed(_)
        ));
    }

    #[test]
    fn duplicate_id_is_rejected() {
        let host = ExtensionHost::activate(vec![Box::new(DupeExt)]);
        assert!(host.active_ids().is_empty());
        assert_eq!(host.registry().counts(), (0, 0));
        let msg = match &host.report()[0].outcome {
            ActivationOutcome::Failed(m) => m.clone(),
            other => panic!("期望 Failed，得到 {other:?}"),
        };
        assert!(msg.contains("重复"), "错误信息应说明重复：{msg}");
    }

    #[test]
    fn a_broken_extension_does_not_break_the_others() {
        let host = ExtensionHost::activate(vec![
            Box::new(MismatchExt),
            Box::new(OkExt),
            Box::new(DupeExt),
        ]);
        assert_eq!(host.active_ids(), vec!["test.ok"]);
        assert_eq!(host.registry().counts(), (1, 1));
        assert_eq!(host.report().len(), 3);
    }

    #[test]
    fn later_theme_registration_wins() {
        // 后注册者覆盖：这里用两个扩展注册同一 kind 验证。
        struct OverrideExt;
        static M: Manifest = Manifest {
            id: "test.override",
            name: "Override",
            version: "0.1.0",
            description: "覆盖同 kind 的配色",
            capabilities: &[Capability::Theme(ThemeContribution {
                id: "override",
                name: "Override",
                kind: ThemeKind::DarkPlus,
                tokens: NO_TOKENS,
            })],
        };
        impl Extension for OverrideExt {
            fn manifest(&self) -> &'static Manifest {
                &M
            }
            fn activate(&mut self, ctx: &mut ExtContext<'_>) -> Result<(), ExtError> {
                ctx.register_theme(ThemeContribution {
                    id: "override",
                    name: "Override",
                    kind: ThemeKind::DarkPlus,
                    tokens: TokenPalette::new(
                        &[(ide_core::HighlightGroup::Keyword, "#ff0000")],
                        "#000000",
                    ),
                })
            }
        }
        let host = ExtensionHost::activate(vec![Box::new(OkExt), Box::new(OverrideExt)]);
        assert_eq!(host.active_ids().len(), 2);
        let t = host.registry().tokens(ThemeKind::DarkPlus).expect("应有配色");
        assert_eq!(t.hex(ide_core::HighlightGroup::Keyword), "#ff0000");
        // 未被覆盖的 kind 仍取内置
        assert!(host.registry().tokens(ThemeKind::LightPlus).is_none());
    }

    #[test]
    fn panic_in_activation_is_isolated_and_rolled_back() {
        let host = ExtensionHost::activate(vec![Box::new(PanicExt), Box::new(OkExt)]);
        let rec = &host.report()[0];
        match &rec.outcome {
            ActivationOutcome::Panicked(m) => assert!(m.contains("boom"), "panic 信息应被保留：{m}"),
            other => panic!("期望 Panicked，得到 {other:?}"),
        }
        assert_eq!(host.active_ids(), vec!["test.ok"]);
        assert_eq!(host.registry().counts(), (1, 1), "panic 的扩展不能留下贡献点");
        assert!(host.registry().tokens(ThemeKind::CppDark).is_none());
    }

    #[test]
    fn extension_paths_without_suffix_do_not_match() {
        let host = ExtensionHost::activate(vec![Box::new(OkExt)]);
        let reg = host.registry();
        assert!(reg.language_for_path("noext").is_none());
        assert!(reg.language_for_path(".gitignore").is_none());
        assert!(reg.language_for_path("src/main.").is_none());
        assert!(reg.language_for_path("readme.md").is_none());
    }

    #[test]
    fn activation_is_within_budget() {
        // 性能硬约束：激活只做注册，应该远低于 5ms（这里留足 debug 档余量）。
        let host = ExtensionHost::activate(vec![
            Box::new(OkExt),
            Box::new(MismatchExt),
            Box::new(DupeExt),
            Box::new(PanicExt),
        ]);
        let total = host.total_activation();
        assert!(total < Duration::from_millis(50), "激活总耗时 {total:?} 超预算");
        for r in host.report() {
            assert!(
                r.duration < Duration::from_millis(20),
                "扩展 {} 激活耗时 {:?} 超预算",
                r.id,
                r.duration
            );
        }
    }
}
