//! 命令注册表 + 命令面板数据（阶段 5）。
//!
//! 命令面板（Ctrl+Shift+P）对所有命令做模糊搜索，回车执行。
//! `safe` 标记：仅翻转内存状态、不产生子进程/磁盘写入的命令，供守护测试
//! 「模拟 100 次快捷键操作无 panic」使用。
//!
//! 快捷键唯一性由 `shortcuts_unique` 守护用例保证（同一动作的多条命令中
//! 只给主命令分配快捷键，其余走命令面板）。

/// 一条命令。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandSpec {
    pub id: &'static str,
    pub title: &'static str,
    /// 中文标题（命令面板支持中文查询）。
    pub zh: &'static str,
    pub category: &'static str,
    pub shortcut: &'static str,
    /// 是否安全（仅内存操作，无子进程/磁盘 IO）——守护测试用。
    pub safe: bool,
}

/// 全量命令表。执行分发在 `LiteIdeApp::run_command`（main.rs）。
pub fn registry() -> &'static [CommandSpec] {
    &[
        // ---- File ----
        CommandSpec { id: "file.newFile", title: "New File", zh: "新建文件", category: "File", shortcut: "Ctrl+N", safe: true },
        CommandSpec { id: "file.openFile", title: "Open File…", zh: "打开文件", category: "File", shortcut: "Ctrl+P", safe: false },
        CommandSpec { id: "file.save", title: "Save File", zh: "保存文件", category: "File", shortcut: "Ctrl+S", safe: false },
        CommandSpec { id: "file.saveAs", title: "Save As…", zh: "另存为", category: "File", shortcut: "Ctrl+Shift+S", safe: false },
        CommandSpec { id: "file.saveAll", title: "Save All Files", zh: "全部保存", category: "File", shortcut: "", safe: false },
        CommandSpec { id: "file.close", title: "Close Tab", zh: "关闭标签", category: "File", shortcut: "Ctrl+W", safe: true },
        CommandSpec { id: "file.closeAll", title: "Close All Tabs", zh: "关闭全部标签", category: "File", shortcut: "", safe: true },
        CommandSpec { id: "file.reopenClosed", title: "Reopen Closed Tab", zh: "重新打开已关闭标签", category: "File", shortcut: "", safe: true },
        CommandSpec { id: "file.reload", title: "Reload from Disk", zh: "从磁盘重新加载", category: "File", shortcut: "", safe: false },
        CommandSpec { id: "file.copyPath", title: "Copy File Path", zh: "复制文件路径", category: "File", shortcut: "", safe: true },
        // ---- Edit ----
        CommandSpec { id: "edit.undo", title: "Undo", zh: "撤销", category: "Edit", shortcut: "Ctrl+Z", safe: true },
        CommandSpec { id: "edit.redo", title: "Redo", zh: "重做", category: "Edit", shortcut: "Ctrl+Y", safe: true },
        CommandSpec { id: "edit.selectAll", title: "Select All", zh: "全选", category: "Edit", shortcut: "Ctrl+A", safe: true },
        CommandSpec { id: "edit.cut", title: "Cut", zh: "剪切", category: "Edit", shortcut: "Ctrl+X", safe: true },
        CommandSpec { id: "edit.copy", title: "Copy", zh: "复制", category: "Edit", shortcut: "Ctrl+C", safe: true },
        CommandSpec { id: "edit.paste", title: "Paste", zh: "粘贴", category: "Edit", shortcut: "Ctrl+V", safe: true },
        CommandSpec { id: "edit.deleteLine", title: "Delete Line", zh: "删除行", category: "Edit", shortcut: "Ctrl+Shift+K", safe: true },
        CommandSpec { id: "edit.selectLine", title: "Select Line", zh: "选中行", category: "Edit", shortcut: "Ctrl+L", safe: true },
        CommandSpec { id: "edit.toggleComment", title: "Toggle Line Comment", zh: "切换行注释", category: "Edit", shortcut: "Ctrl+/", safe: true },
        CommandSpec { id: "edit.duplicateLine", title: "Duplicate Line", zh: "复制行", category: "Edit", shortcut: "Ctrl+Shift+D", safe: true },
        CommandSpec { id: "edit.find", title: "Find", zh: "查找", category: "Edit", shortcut: "Ctrl+F", safe: true },
        CommandSpec { id: "edit.replace", title: "Replace", zh: "替换", category: "Edit", shortcut: "Ctrl+H", safe: true },
        // ---- View ----
        CommandSpec { id: "view.toggleSidebar", title: "Toggle Sidebar", zh: "切换侧边栏", category: "View", shortcut: "Ctrl+B", safe: true },
        CommandSpec { id: "view.toggleMinimap", title: "Toggle Minimap", zh: "切换迷你地图", category: "View", shortcut: "", safe: true },
        CommandSpec { id: "view.toggleBreadcrumbs", title: "Toggle Breadcrumbs", zh: "切换面包屑", category: "View", shortcut: "", safe: true },
        CommandSpec { id: "view.showExplorer", title: "Show Explorer", zh: "显示资源管理器", category: "View", shortcut: "", safe: true },
        CommandSpec { id: "view.showOutline", title: "Show Outline", zh: "显示大纲", category: "View", shortcut: "", safe: true },
        CommandSpec { id: "view.zoomIn", title: "Zoom In", zh: "放大字号", category: "View", shortcut: "Ctrl+=", safe: false },
        CommandSpec { id: "view.zoomOut", title: "Zoom Out", zh: "缩小字号", category: "View", shortcut: "Ctrl+-", safe: false },
        CommandSpec { id: "view.resetFontSize", title: "Reset Font Size", zh: "重置字号", category: "View", shortcut: "Ctrl+0", safe: false },
        CommandSpec { id: "view.switchTheme", title: "Switch Theme (Dark+/Light+)", zh: "切换主题", category: "View", shortcut: "", safe: false },
        // ---- Go ----
        CommandSpec { id: "go.definition", title: "Go to Definition", zh: "转到定义", category: "Go", shortcut: "F12", safe: false },
        CommandSpec { id: "go.back", title: "Go Back", zh: "返回上一位置", category: "Go", shortcut: "Alt+Left", safe: true },
        CommandSpec { id: "go.line", title: "Go to Line…", zh: "跳转到行", category: "Go", shortcut: "Ctrl+G", safe: true },
        CommandSpec { id: "go.symbol", title: "Go to Symbol…", zh: "跳转到符号", category: "Go", shortcut: "Ctrl+T", safe: true },
        // ---- Search ----
        CommandSpec { id: "search.file", title: "Find File", zh: "查找文件", category: "Search", shortcut: "", safe: false },
        CommandSpec { id: "search.content", title: "Find in Files", zh: "在工作区中查找", category: "Search", shortcut: "Ctrl+Shift+F", safe: true },
        CommandSpec { id: "search.symbol", title: "Find Symbol", zh: "查找符号", category: "Search", shortcut: "", safe: true },
        CommandSpec { id: "search.refreshFiles", title: "Refresh File List", zh: "刷新文件列表", category: "Search", shortcut: "", safe: false },
        // ---- Debug ----
        CommandSpec { id: "debug.start", title: "Start / Continue Debugging", zh: "开始/继续调试", category: "Debug", shortcut: "F5", safe: false },
        CommandSpec { id: "debug.stop", title: "Stop Debugging", zh: "停止调试", category: "Debug", shortcut: "Shift+F5", safe: false },
        CommandSpec { id: "debug.toggleBreakpoint", title: "Toggle Breakpoint", zh: "切换断点", category: "Debug", shortcut: "F9", safe: true },
        CommandSpec { id: "debug.stepOver", title: "Step Over", zh: "单步跳过", category: "Debug", shortcut: "F10", safe: false },
        CommandSpec { id: "debug.stepInto", title: "Step Into", zh: "单步进入", category: "Debug", shortcut: "F11", safe: false },
        CommandSpec { id: "debug.stepOut", title: "Step Out", zh: "单步跳出", category: "Debug", shortcut: "Shift+F11", safe: false },
        CommandSpec { id: "debug.addWatch", title: "Add to Watch", zh: "添加到监视", category: "Debug", shortcut: "Ctrl+Shift+W", safe: true },
        // ---- Git ----
        CommandSpec { id: "git.openPanel", title: "Git: Open Panel", zh: "打开 Git 面板", category: "Git", shortcut: "Ctrl+Shift+G", safe: false },
        CommandSpec { id: "git.refresh", title: "Git: Refresh Status", zh: "刷新 Git 状态", category: "Git", shortcut: "", safe: false },
        CommandSpec { id: "git.commit", title: "Git: Commit…", zh: "提交更改", category: "Git", shortcut: "", safe: false },
        CommandSpec { id: "git.diffActive", title: "Git: Diff Active File", zh: "查看活动文件差异", category: "Git", shortcut: "", safe: false },
        CommandSpec { id: "git.showHistory", title: "Git: Show History", zh: "查看提交历史", category: "Git", shortcut: "", safe: false },
        // ---- Settings ----
        CommandSpec { id: "settings.open", title: "Settings", zh: "打开设置", category: "Settings", shortcut: "Ctrl+,", safe: true },
        CommandSpec { id: "settings.increaseFont", title: "Increase Font Size", zh: "增大字号", category: "Settings", shortcut: "Ctrl+Plus", safe: false },
        CommandSpec { id: "settings.decreaseFont", title: "Decrease Font Size", zh: "减小字号", category: "Settings", shortcut: "Ctrl+Minus", safe: false },
        CommandSpec { id: "settings.reset", title: "Reset Settings", zh: "重置设置", category: "Settings", shortcut: "", safe: false },
        // ---- Help ----
        CommandSpec { id: "help.keybindings", title: "Keyboard Shortcuts", zh: "键盘快捷键", category: "Help", shortcut: "", safe: true },
        CommandSpec { id: "help.about", title: "About LiteIDE", zh: "关于 LiteIDE", category: "Help", shortcut: "", safe: true },
    ]
}

/// 命令面板模糊过滤：按 `ide_search::fuzzy` 分数降序，取前 `limit` 条。
pub fn filter_commands(query: &str, limit: usize) -> Vec<&'static CommandSpec> {
    let mut scored: Vec<(u32, &'static CommandSpec)> = registry()
        .iter()
        .filter_map(|c| {
            // 分类 + 英文标题 + 中文标题一起参与匹配，支持中英文查询。
            let hay = format!("{} {} {}", c.category, c.title, c.zh);
            ide_search::fuzzy::fuzzy_score(query, &hay).map(|s| (s, c))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.title.cmp(b.1.title)));
    scored.into_iter().take(limit).map(|(_, c)| c).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn registry_has_at_least_50_commands() {
        let n = registry().len();
        assert!(n >= 50, "命令面板需覆盖 50+ 命令，当前 {n}");
        assert!(n <= 80);
    }

    #[test]
    fn ids_are_unique() {
        let ids: HashSet<&str> = registry().iter().map(|c| c.id).collect();
        assert_eq!(ids.len(), registry().len());
    }

    #[test]
    fn shortcuts_are_unique() {
        let mut seen = HashSet::new();
        for c in registry() {
            if !c.shortcut.is_empty() {
                assert!(
                    seen.insert(c.shortcut),
                    "快捷键冲突: {} 被 {} 和另一命令共用",
                    c.shortcut,
                    c.title
                );
            }
        }
    }

    #[test]
    fn categories_are_covered() {
        let cats: HashSet<&str> = registry().iter().map(|c| c.category).collect();
        for want in ["File", "Edit", "View", "Go", "Search", "Debug", "Git", "Settings", "Help"] {
            assert!(cats.contains(want), "缺少分类 {want}");
        }
    }

    #[test]
    fn fuzzy_filters_git_commands() {
        let hits = filter_commands("git", 20);
        assert!(!hits.is_empty());
        // "git" 是子序列，可能顺带命中 "Go: Definition"；Git 命令必须出现且排在最前。
        assert!(hits.iter().any(|c| c.category == "Git"));
        assert_eq!(hits[0].category, "Git");
    }

    #[test]
    fn fuzzy_filters_by_chinese_title_and_category() {
        let hits = filter_commands("设置", 20);
        assert!(hits.iter().any(|c| c.id == "settings.open"));
        let hits = filter_commands("保存", 20);
        assert!(hits.iter().any(|c| c.id == "file.save"));
    }

    #[test]
    fn empty_query_returns_top_commands() {
        let hits = filter_commands("", 5);
        assert_eq!(hits.len(), 5);
    }

    #[test]
    fn safe_marker_distinguishes_process_commands() {
        // 会拉起子进程的命令必须标记 safe=false；
        // 仅内存操作（无会话时只翻转内存状态/队列消息）的调试命令可标记 safe。
        for c in registry() {
            if c.category == "Debug" || c.category == "Git" {
                let memory_only =
                    matches!(c.id, "debug.toggleBreakpoint" | "debug.addWatch");
                assert_eq!(c.safe, memory_only, "{} 的 safe 标记与预期不符", c.id);
            }
        }
        assert!(registry().iter().any(|c| c.id == "git.openPanel"));
        assert!(registry().iter().all(|c| c.id != "no.such"));
    }
}
