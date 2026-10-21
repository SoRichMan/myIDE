//! 源代码管理（Source Control）侧栏面板。
//!
//! 布局对齐 VS Code 的源代码管理视图：
//!
//! ```text
//! SOURCE CONTROL                     ⋯
//! [筛选更改…]
//! CHANGES
//!   ▾ repo-name        ⑂ master  ↺ ⟳ ✓ ⟳ ⋯
//!     Message (Ctrl+Enter to commit on "master")
//!     [ ✓ 提交                    ▾ ]
//!     ▾ 暂存的更改 (2)
//!         M src/main.cpp
//!     ▾ 更改 (3)
//!         M src/other.cpp
//! GRAPH
//!   ▾ repo-name   ⇣ ⇡ ⟳ ⋯
//!     ● fix something                    Alice
//!     ● another commit                   Bob
//!     ┌ 提交详情卡片 ┐
//! ```

use crate::{draw_codicon, hex, Codicon, RichText};
use ide_git::branch::Branch;
use ide_git::log::{CommitDetail, GitCommit};
use ide_git::porcelain::{GitEntry, GitStatus};
use std::collections::HashSet;

// ---- VS Code Dark+ / Dark Modern 取色 ----
const FG: &str = "#cccccc";
const FG_DIM: &str = "#9d9d9d";
const FG_HEADER: &str = "#bbbbbb";
const HOVER_BG: &str = "#2a2d2e";
const SELECTED_BG: &str = "#37373d";
const ACCENT: &str = "#0078d4";
const ACCENT_TEXT: &str = "#ffffff";
const BORDER: &str = "#3c3c3c";
const CHANGED_ADDED: &str = "#73c991";
const CHANGED_MODIFIED: &str = "#dcdcaa";
const CHANGED_DELETED: &str = "#f48771";
const CHANGED_UNTRACKED: &str = "#73c991";
const REF_FG: &str = "#3794ff";

/// 面板要执行的动作（由 App 层翻译成 git 命令）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    RefreshAll,
    RefreshRepo(usize),
    Stage { repo: usize, path: String },
    Unstage { repo: usize, path: String },
    Discard { repo: usize, path: String },
    StageAll(usize),
    UnstageAll(usize),
    DiscardAll(usize),
    Commit { repo: usize, message: String },
    /// 提交并推送。
    CommitPush { repo: usize, message: String },
    Amend { repo: usize },
    Checkout { repo: usize, branch: String },
    CreateBranch { repo: usize, name: String },
    Push(usize),
    Pull(usize),
    Sync(usize),
    SelectCommit { repo: usize, hash: String },
    OpenDiff { repo: usize, path: String, cached: bool },
    ShowCommitDiff { repo: usize, hash: String },
    /// 取某条提交的详情（悬停卡片要显示改动统计）。
    RequestDetail { repo: usize, hash: String },
    /// 复制提交 ID 到剪贴板。
    CopyHash(String),
    CloseDetail,
    /// 把焦点放到该仓库的提交信息输入框。
    FocusMessage(usize),
}

/// 一个仓库分组的状态（对应 VS Code 多仓库工作区里的一个 CHANGES 分组）。
#[derive(Debug, Clone, Default)]
pub struct RepoState {
    /// 仓库绝对路径。
    pub path: String,
    /// 显示名（目录名）。
    pub name: String,
    /// `git status` 结果。
    pub status: GitStatus,
    /// 分支列表。
    pub branches: Vec<Branch>,
    /// 提交历史（最新在前）。
    pub commits: Vec<GitCommit>,
    /// 提交信息输入框内容。
    pub message: String,
    /// 「暂存的更改」是否展开。
    pub expanded_staged: bool,
    /// 「更改」是否展开。
    pub expanded_unstaged: bool,
    /// GRAPH 列表是否展开。
    pub expanded_graph: bool,
    /// 当前选中的提交（短 SHA）。
    pub selected: Option<String>,
}

impl RepoState {
    /// 新建一个仓库状态。
    pub fn new(path: String, name: String) -> Self {
        Self {
            path,
            name,
            expanded_staged: true,
            expanded_unstaged: true,
            expanded_graph: true,
            ..Default::default()
        }
    }

    /// 当前分支名。
    pub fn branch(&self) -> Option<&str> {
        self.status.branch.as_deref()
    }

    /// 已暂存文件。
    pub fn staged_files(&self) -> Vec<GitEntry> {
        self.status
            .entries
            .iter()
            .filter(|e| !matches!(e.staged, '.' | ' ' | '?' | 'N'))
            .cloned()
            .collect()
    }

    /// 未暂存 + 未跟踪文件。
    pub fn unstaged_files(&self) -> Vec<GitEntry> {
        self.status
            .entries
            .iter()
            .filter(|e| {
                !matches!(e.worktree, '.' | ' ' | 'N') || e.staged == '?' || e.worktree == '?'
            })
            .cloned()
            .collect()
    }
}

/// 悬停在 GRAPH 某条提交上时弹出的信息卡片（对齐 Git Graph / VS Code 的提交详情）。
#[derive(Debug, Clone)]
pub struct HoverCard {
    /// 所属仓库下标。
    pub repo: usize,
    /// 短 SHA。
    pub hash: String,
    /// 作者。
    pub author: String,
    /// 提交时间（ISO）。
    pub date: String,
    /// 标题。
    pub subject: String,
    /// 该行在屏幕上的位置（卡片贴着它弹）。
    pub anchor: egui::Rect,
}

impl Default for HoverCard {
    fn default() -> Self {
        Self {
            repo: 0,
            hash: String::new(),
            author: String::new(),
            date: String::new(),
            subject: String::new(),
            anchor: egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::ZERO),
        }
    }
}

/// 源代码管理面板（整个侧栏视图）。
#[derive(Debug, Default)]
pub struct ScmPanel {
    /// 各仓库分组。
    pub repos: Vec<RepoState>,
    /// 折叠状态：`"CHANGES"` / `"GRAPH"`。
    pub collapsed: HashSet<String>,
    /// 右上角「⋯」菜单是否展开。
    pub menu_open: bool,
    /// 正在展开分支列表的仓库下标。
    pub branch_popup: Option<usize>,
    /// 正在输入新分支名的仓库下标。
    pub new_branch: Option<usize>,
    /// 提交按钮下拉菜单打开的仓库下标。
    pub commit_menu: Option<usize>,
    /// 新分支名输入框内容。
    pub new_branch_name: String,
    /// 选中的提交详情。
    pub detail: Option<CommitDetail>,
    /// 详情对应的仓库下标。
    pub detail_repo: usize,
    /// 选中提交（或文件）的 diff 文本。
    pub detail_diff: String,
    /// 本帧悬停到的提交（GRAPH 列表）。
    pub frame_hover: Option<HoverCard>,
    /// 当前显示中的悬停卡片（指针移到卡片上时保持打开）。
    pub hover_card: Option<HoverCard>,
    /// 上一帧卡片是否被指针悬停（用于「指针移到卡片上不消失」）。
    pub card_hovered: bool,
    /// 工作区根目录（用于发现仓库）。
    pub root: String,
}

impl ScmPanel {
    /// 切换工作区：重新发现仓库并清空旧状态。
    ///
    /// 只看「根 + 一级子目录」——和 VS Code 的 SCM 视图一致：
    /// 工作区里有几个仓库就显示几个分组（不会把 `external/` 里的第三方仓库也列出来）。
    pub fn set_root(&mut self, root: &str) {
        if self.root == root {
            return;
        }
        self.root = root.to_string();
        self.repos = ide_git::repos::discover_repos(root, 1)
            .into_iter()
            .map(|p| {
                let name = ide_git::repos::repo_name(&p);
                RepoState::new(p, name)
            })
            .collect();
        self.detail = None;
        self.detail_diff.clear();
    }

    /// 手动设置仓库列表（测试 / 远程工作区用）。
    #[allow(dead_code)]
    pub fn set_repos(&mut self, repos: Vec<RepoState>) {
        self.repos = repos;
    }

    /// 用一组仓库路径替换当前列表（远程工作区用 WSL 扫描的结果）。
    pub fn set_repo_paths(&mut self, paths: Vec<String>) {
        self.repos = paths
            .into_iter()
            .map(|p| {
                let name = ide_git::repos::repo_name(&p);
                RepoState::new(p, name)
            })
            .collect();
    }

    /// 处理 git 事件。
    pub fn on_event(&mut self, ev: &ide_git::Event) {
        match ev {
            ide_git::Event::Status { root, status } => {
                if let Some(r) = self.repo_mut(root) {
                    r.status = (**status).clone();
                }
            }
            ide_git::Event::Log { root, commits } => {
                if let Some(r) = self.repo_mut(root) {
                    r.commits = commits.clone();
                }
            }
            ide_git::Event::Branches { root, branches } => {
                if let Some(r) = self.repo_mut(root) {
                    r.branches = branches.clone();
                }
            }
            ide_git::Event::CommitDetail { root, detail } => {
                if let Some(idx) = self.repo_index(root) {
                    self.detail_repo = idx;
                    self.detail = Some((**detail).clone());
                }
            }
            ide_git::Event::CommitDiff { text, .. } => {
                self.detail_diff = text.clone();
            }
            ide_git::Event::Diff { text, .. } => {
                self.detail_diff = text.clone();
            }
            ide_git::Event::Done { .. } => {}
        }
    }

    /// 找到仓库下标。
    pub fn repo_index(&self, root: &str) -> Option<usize> {
        self.repos.iter().position(|r| r.path == root)
    }

    fn repo_mut(&mut self, root: &str) -> Option<&mut RepoState> {
        self.repos.iter_mut().find(|r| r.path == root)
    }

    /// 主仓库（第一个）的分支名（状态栏用）。
    pub fn primary_branch(&self) -> Option<String> {
        self.repos.first().and_then(|r| r.branch()).map(|s| s.to_string())
    }

    /// 绘制整个面板，返回需要执行的动作。
    pub fn draw(&mut self, ui: &mut egui::Ui) -> Vec<Action> {
        let mut actions = Vec::new();
        let mut repos = std::mem::take(&mut self.repos);
        self.frame_hover = None;

        egui::ScrollArea::vertical()
            .id_source("scm_panel")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                self.draw_title(ui, &mut actions);
                ui.add_space(2.0);

                // ---- CHANGES ----
                let changes_open = !self.collapsed.contains("CHANGES");
                section_header(ui, "CHANGES", changes_open, &mut self.collapsed);
                if changes_open {
                    if repos.is_empty() {
                        ui.add_space(4.0);
                        ui.label(
                            RichText::new("未检测到 git 仓库").size(12.0).color(hex(FG_DIM)),
                        );
                    }
                    for idx in 0..repos.len() {
                        self.draw_repo(ui, idx, &mut repos, &mut actions);
                    }
                }

                // ---- GRAPH ----
                ui.add_space(6.0);
                let graph_open = !self.collapsed.contains("GRAPH");
                section_header(ui, "GRAPH", graph_open, &mut self.collapsed);
                if graph_open {
                    for idx in 0..repos.len() {
                        self.draw_graph(ui, idx, &mut repos, &mut actions);
                    }
                    // 悬停信息卡片（弹在侧栏右侧）
                    self.draw_hover_card(ui, &mut actions);
                    self.draw_detail(ui, &mut actions);
                }
            });

        self.repos = repos;
        actions
    }

    // ---- 标题栏 ----

    fn draw_title(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        ui.horizontal(|ui| {
            ui.add_space(4.0);
            ui.label(RichText::new("SOURCE CONTROL").size(11.0).color(hex(FG_HEADER)));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let (rect, resp) = ui.allocate_exact_size(egui::vec2(20.0, 20.0), egui::Sense::click());
                if resp.hovered() {
                    ui.painter()
                        .rect_filled(rect, 3.0, hex("#ffffff").gamma_multiply(0.08));
                }
                draw_codicon(ui.painter(), rect.center(), 16.0, Codicon::Ellipsis, hex(FG));
                if resp.clicked() {
                    self.menu_open = !self.menu_open;
                }
                resp.on_hover_text("更多操作");
            });
        });
        if self.menu_open {
            self.draw_menu(ui, actions);
        }
    }

    fn draw_menu(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let mut close = false;
        egui::Frame::popup(ui.style())
            .fill(hex("#252526"))
            .stroke(egui::Stroke::new(1.0_f32, hex(BORDER)))
            .inner_margin(egui::Margin::same(4.0))
            .show(ui, |ui| {
                ui.set_min_width(160.0);
                if menu_item(ui, "检出分支…") {
                    self.branch_popup = Some(0);
                    close = true;
                }
                if menu_item(ui, "新建分支…") {
                    self.new_branch = Some(0);
                    close = true;
                }
                ui.separator();
                if menu_item(ui, "取消所有暂存") {
                    actions.push(Action::UnstageAll(0));
                    close = true;
                }
                if menu_item(ui, "修补上一次提交") {
                    actions.push(Action::Amend { repo: 0 });
                    close = true;
                }
                ui.separator();
                if menu_item(ui, "拉取") {
                    actions.push(Action::Pull(0));
                    close = true;
                }
                if menu_item(ui, "推送") {
                    actions.push(Action::Push(0));
                    close = true;
                }
                if menu_item(ui, "同步") {
                    actions.push(Action::Sync(0));
                    close = true;
                }
                ui.separator();
                if menu_item(ui, "刷新") {
                    actions.push(Action::RefreshAll);
                    close = true;
                }
            });
        if close {
            self.menu_open = false;
        }
    }

    // ---- 仓库分组（CHANGES） ----

    fn draw_repo(
        &mut self,
        ui: &mut egui::Ui,
        idx: usize,
        repos: &mut [RepoState],
        actions: &mut Vec<Action>,
    ) {
        let branch = repos[idx].branch().unwrap_or("").to_string();

        // ---- 仓库标题行：名字 + 分支 + 动作按钮 ----
        ui.horizontal(|ui| {
            ui.add_space(2.0);
            // 仓库图标（VS Code 用「仓库」图标，分支图标在分支 chip 里）
            let (icon_rect, _) = ui.allocate_exact_size(egui::vec2(16.0, 18.0), egui::Sense::hover());
            draw_codicon(
                ui.painter(),
                icon_rect.center(),
                14.0,
                Codicon::Repo,
                hex(FG),
            );
            ui.label(
                RichText::new(repos[idx].name.clone())
                    .size(12.5)
                    .strong()
                    .color(hex(FG)),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // ⋯
                let (more_rect, more) =
                    ui.allocate_exact_size(egui::vec2(20.0, 20.0), egui::Sense::click());
                if more.hovered() {
                    ui.painter()
                        .rect_filled(more_rect, 3.0, hex("#ffffff").gamma_multiply(0.08));
                }
                draw_codicon(ui.painter(), more_rect.center(), 15.0, Codicon::Ellipsis, hex(FG_DIM));
                if more.clicked() {
                    self.branch_popup = if self.branch_popup == Some(idx) {
                        None
                    } else {
                        Some(idx)
                    };
                }
                more.on_hover_text("仓库操作");

                // 刷新
                let (refresh_rect, refresh) =
                    ui.allocate_exact_size(egui::vec2(20.0, 20.0), egui::Sense::click());
                if refresh.hovered() {
                    ui.painter()
                        .rect_filled(refresh_rect, 3.0, hex("#ffffff").gamma_multiply(0.08));
                }
                draw_codicon(
                    ui.painter(),
                    refresh_rect.center(),
                    15.0,
                    Codicon::Refresh,
                    hex(FG_DIM),
                );
                if refresh.clicked() {
                    actions.push(Action::RefreshRepo(idx));
                }
                refresh.on_hover_text("刷新");

                // 提交（check）
                let (commit_rect, commit) =
                    ui.allocate_exact_size(egui::vec2(20.0, 20.0), egui::Sense::click());
                if commit.hovered() {
                    ui.painter()
                        .rect_filled(commit_rect, 3.0, hex("#ffffff").gamma_multiply(0.08));
                }
                draw_codicon(ui.painter(), commit_rect.center(), 15.0, Codicon::Check, hex(FG_DIM));
                if commit.clicked() {
                    let msg = repos[idx].message.trim().to_string();
                    if msg.is_empty() {
                        actions.push(Action::FocusMessage(idx));
                    } else {
                        actions.push(Action::Commit {
                            repo: idx,
                            message: msg,
                        });
                    }
                }
                commit.on_hover_text("提交");


                // 拉取请求（VS Code 的 ⑂ 位置）
                let (pr_rect, pr) =
                    ui.allocate_exact_size(egui::vec2(20.0, 20.0), egui::Sense::click());
                if pr.hovered() {
                    ui.painter()
                        .rect_filled(pr_rect, 3.0, hex("#ffffff").gamma_multiply(0.08));
                }
                draw_codicon(
                    ui.painter(),
                    pr_rect.center(),
                    15.0,
                    Codicon::GitPullRequest,
                    hex(FG_DIM),
                );
                if pr.clicked() {
                    actions.push(Action::Pull(idx));
                }
                pr.on_hover_text("拉取");

                // 提交并推送（VS Code 的 ⬆ 位置）
                let (push_rect, push) =
                    ui.allocate_exact_size(egui::vec2(20.0, 20.0), egui::Sense::click());
                if push.hovered() {
                    ui.painter()
                        .rect_filled(push_rect, 3.0, hex("#ffffff").gamma_multiply(0.08));
                }
                draw_codicon(
                    ui.painter(),
                    push_rect.center(),
                    15.0,
                    Codicon::CloudUpload,
                    hex(FG_DIM),
                );
                if push.clicked() {
                    actions.push(Action::Push(idx));
                }
                push.on_hover_text("推送");
                // 暂存全部
                let (stage_rect, stage_all) =
                    ui.allocate_exact_size(egui::vec2(20.0, 20.0), egui::Sense::click());
                if stage_all.hovered() {
                    ui.painter()
                        .rect_filled(stage_rect, 3.0, hex("#ffffff").gamma_multiply(0.08));
                }
                draw_codicon(ui.painter(), stage_rect.center(), 15.0, Codicon::Add, hex(FG_DIM));
                if stage_all.clicked() {
                    actions.push(Action::StageAll(idx));
                }
                stage_all.on_hover_text("暂存所有更改");

                // 丢弃全部
                let (discard_rect, discard) =
                    ui.allocate_exact_size(egui::vec2(20.0, 20.0), egui::Sense::click());
                if discard.hovered() {
                    ui.painter()
                        .rect_filled(discard_rect, 3.0, hex("#ffffff").gamma_multiply(0.08));
                }
                draw_codicon(
                    ui.painter(),
                    discard_rect.center(),
                    15.0,
                    Codicon::Discard,
                    hex(FG_DIM),
                );
                if discard.clicked() {
                    actions.push(Action::DiscardAll(idx));
                }
                discard.on_hover_text("丢弃所有更改");

                // 分支名（VS Code 里的分支 chip）
                let chip = egui::Frame::none()
                    .fill(hex("#2d2d2d"))
                    .inner_margin(egui::Margin::symmetric(5.0, 2.0))
                    .rounding(3.0)
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            let (icon_rect, _) =
                                ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
                            draw_codicon(
                                ui.painter(),
                                icon_rect.center(),
                                13.0,
                                Codicon::SourceControl,
                                hex(FG),
                            );
                            ui.label(RichText::new(ellipsize(&branch, 16)).size(11.5).color(hex(FG)));
                        });
                    })
                    .response;
                if chip.clicked() {
                    self.branch_popup = if self.branch_popup == Some(idx) {
                        None
                    } else {
                        Some(idx)
                    };
                }
                chip.on_hover_text("检出分支");
            });
        });

        // ---- 分支下拉 ----
        if self.branch_popup == Some(idx) {
            self.draw_branch_list(ui, idx, repos, actions);
        }
        if self.new_branch == Some(idx) {
            self.draw_new_branch(ui, idx, actions);
        }

        // ---- 提交信息输入框 ----
        self.draw_message_input(ui, idx, repos, &branch, actions);
        ui.add_space(6.0);

        // ---- 提交按钮 ----
        let message = repos[idx].message.trim().to_string();
        self.draw_commit_button(ui, &message, actions, idx);

        // ---- 文件分组 ----
        ui.add_space(4.0);
        let staged = repos[idx].staged_files();
        let unstaged = repos[idx].unstaged_files();

        if !staged.is_empty() {
            let open = repos[idx].expanded_staged;
            let (o, changed) = group_header(ui, "暂存的更改", staged.len(), open);
            repos[idx].expanded_staged = if changed { !open } else { o };
            if repos[idx].expanded_staged {
                self.draw_file_list(ui, idx, &staged, true, actions);
            }
        }
        if !unstaged.is_empty() {
            let open = repos[idx].expanded_unstaged;
            let (o, changed) = group_header(ui, "更改", unstaged.len(), open);
            repos[idx].expanded_unstaged = if changed { !open } else { o };
            if repos[idx].expanded_unstaged {
                self.draw_file_list(ui, idx, &unstaged, false, actions);
            }
        }
        if staged.is_empty() && unstaged.is_empty() {
            ui.horizontal(|ui| {
                ui.add_space(14.0);
                ui.label(RichText::new("工作区干净").size(11.5).color(hex(FG_DIM)));
            });
        }
        ui.add_space(6.0);
    }

    /// 提交信息输入框（VS Code 的 `input.background` 单行框，约 26 px 高）。
    ///
    /// egui 的 `TextEdit::multiline` 即使 `desired_rows(1)` 也会按行高把文字画到框外，
    /// 所以这里改成「单行 TextEdit + 上下内边距」：框高 = 行高 + 2×3px ≈ 26px，
    /// 占位提示垂直居中，和 VS Code 一致。
    fn draw_message_input(
        &mut self,
        ui: &mut egui::Ui,
        idx: usize,
        repos: &mut [RepoState],
        branch: &str,
        actions: &mut Vec<Action>,
    ) {
        let hint = format!("Message (Ctrl+Enter to commit on \"{}\")", branch);
        ui.horizontal(|ui| {
            ui.add_space(10.0);
            let width = ui.available_width() - 14.0;
            let mut msg = repos[idx].message.clone();
            // VS Code：输入框背景 = `input.background`，边框 = `input.border`。
            let old_visuals = ui.visuals().clone();
            let mut visuals = old_visuals.clone();
            visuals.extreme_bg_color = hex("#3c3c3c");
            visuals.widgets.inactive.bg_fill = hex("#3c3c3c");
            visuals.widgets.inactive.weak_bg_fill = hex("#3c3c3c");
            visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0_f32, hex("#3c3c3c"));
            visuals.widgets.inactive.rounding = egui::Rounding::same(2.0);
            *ui.visuals_mut() = visuals;
            let edit = egui::Frame::none()
                .inner_margin(egui::Margin::symmetric(0.0, 5.0))
                .show(ui, |ui| {
                    egui::TextEdit::singleline(&mut msg)
                        .desired_width(width)
                        .hint_text(RichText::new(hint).size(11.5).color(hex(FG_DIM)))
                        .font(egui::TextStyle::Small)
                        .show(ui)
                });
            *ui.visuals_mut() = old_visuals;
            let ctrl_enter = ui.input(|i| i.key_pressed(egui::Key::Enter) && i.modifiers.ctrl);
            if edit.response.changed() {
                repos[idx].message = msg.clone();
            }
            if ctrl_enter {
                let text = msg.trim().to_string();
                if !text.is_empty() {
                    actions.push(Action::Commit {
                        repo: idx,
                        message: text,
                    });
                }
            }
        });
    }

    fn draw_commit_button(
        &mut self,
        ui: &mut egui::Ui,
        message: &str,
        actions: &mut Vec<Action>,
        idx: usize,
    ) {
        let enabled = !message.is_empty();
        ui.horizontal(|ui| {
            ui.add_space(10.0);
            let full = ui.available_width() - 14.0;
            let (rect, _resp) =
                ui.allocate_exact_size(egui::vec2(full, 26.0), egui::Sense::click());
            let painter = ui.painter();
            // VS Code：提交按钮始终是蓝色（没有提交信息时只是变暗）。
            let bg = if enabled { hex(ACCENT) } else { hex("#1f4b78") };
            painter.rect_filled(rect, 3.0, bg);
            let fg = if enabled { hex(ACCENT_TEXT) } else { hex("#9fc6e8") };
            draw_codicon(
                painter,
                egui::pos2(rect.left() + 16.0, rect.center().y),
                14.0,
                Codicon::Check,
                fg,
            );
            painter.text(
                egui::pos2(rect.left() + 28.0, rect.center().y),
                egui::Align2::LEFT_CENTER,
                "提交",
                egui::FontId::proportional(12.0),
                fg,
            );
            // 右侧下拉段
            let split = egui::Rect::from_min_max(
                egui::pos2(rect.right() - 24.0, rect.top()),
                egui::pos2(rect.right(), rect.bottom()),
            );
            painter.vline(
                split.left(),
                split.y_range(),
                egui::Stroke::new(1.0_f32, hex("#ffffff").gamma_multiply(0.25)),
            );
            draw_codicon(
                painter,
                egui::pos2(split.center().x, split.center().y),
                13.0,
                Codicon::ChevronDown,
                fg,
            );
            // 主按钮：提交；右侧小段：下拉菜单（VS Code 的 ▾）
            let main = egui::Rect::from_min_max(
                egui::pos2(rect.left(), rect.top()),
                egui::pos2(split.left(), rect.bottom()),
            );
            let main_resp =
                ui.interact(main, egui::Id::new(("scm_commit_btn", idx)), egui::Sense::click());
            if main_resp.clicked() && enabled {
                actions.push(Action::Commit {
                    repo: idx,
                    message: message.to_string(),
                });
            }
            main_resp.on_hover_text("提交（Ctrl+Enter）");
            let drop_resp =
                ui.interact(split, egui::Id::new(("scm_commit_menu", idx)), egui::Sense::click());
            if drop_resp.clicked() {
                self.commit_menu = if self.commit_menu == Some(idx) {
                    None
                } else {
                    Some(idx)
                };
            }
            drop_resp.on_hover_text("更多提交操作");
        });
        if self.commit_menu == Some(idx) {
            self.draw_commit_menu(ui, idx, message, actions);
        }
        ui.add_space(2.0);
    }

    /// 提交按钮的下拉菜单（VS Code：提交 / 提交并推送 / 修补上一次提交）。
    fn draw_commit_menu(
        &mut self,
        ui: &mut egui::Ui,
        idx: usize,
        message: &str,
        actions: &mut Vec<Action>,
    ) {
        let mut close = false;
        egui::Frame::popup(ui.style())
            .fill(hex("#252526"))
            .stroke(egui::Stroke::new(1.0_f32, hex(BORDER)))
            .inner_margin(egui::Margin::same(4.0))
            .show(ui, |ui| {
                ui.set_min_width(180.0);
                if menu_item(ui, "提交") {
                    if !message.is_empty() {
                        actions.push(Action::Commit {
                            repo: idx,
                            message: message.to_string(),
                        });
                    }
                    close = true;
                }
                if menu_item(ui, "提交并推送") {
                    if !message.is_empty() {
                        actions.push(Action::CommitPush {
                            repo: idx,
                            message: message.to_string(),
                        });
                    }
                    close = true;
                }
                ui.separator();
                if menu_item(ui, "修补上一次提交") {
                    actions.push(Action::Amend { repo: idx });
                    close = true;
                }
            });
        if close {
            self.commit_menu = None;
        }
    }

    fn draw_branch_list(
        &mut self,
        ui: &mut egui::Ui,
        idx: usize,
        repos: &mut [RepoState],
        actions: &mut Vec<Action>,
    ) {
        let branches = repos[idx].branches.clone();
        let current = repos[idx].branch().unwrap_or("").to_string();
        let mut close = false;
        egui::Frame::popup(ui.style())
            .fill(hex("#252526"))
            .stroke(egui::Stroke::new(1.0_f32, hex(BORDER)))
            .inner_margin(egui::Margin::same(4.0))
            .show(ui, |ui| {
                ui.set_min_width(180.0);
                if menu_item(ui, "新建分支…") {
                    self.new_branch = Some(idx);
                    close = true;
                }
                ui.separator();
                for b in &branches {
                    if b.name.starts_with("HEAD") {
                        continue;
                    }
                    let label = if b.current {
                        format!("✓ {}", b.name)
                    } else {
                        b.name.clone()
                    };
                    let resp = ui.add(
                        egui::Label::new(RichText::new(label).size(12.0).color(hex(FG)))
                            .sense(egui::Sense::click()),
                    );
                    if resp.clicked() {
                        if !b.current {
                            actions.push(Action::Checkout {
                                repo: idx,
                                branch: b.name.clone(),
                            });
                        }
                        close = true;
                    }
                }
                if branches.is_empty() {
                    ui.label(
                        RichText::new(format!("(当前分支: {current})"))
                            .size(11.5)
                            .color(hex(FG_DIM)),
                    );
                }
            });
        if close {
            self.branch_popup = None;
        }
    }

    fn draw_new_branch(&mut self, ui: &mut egui::Ui, idx: usize, actions: &mut Vec<Action>) {
        let mut name = self.new_branch_name.clone();
        let mut done = false;
        let mut cancel = false;
        egui::Frame::popup(ui.style())
            .fill(hex("#252526"))
            .stroke(egui::Stroke::new(1.0_f32, hex(BORDER)))
            .inner_margin(egui::Margin::same(6.0))
            .show(ui, |ui| {
                ui.set_min_width(200.0);
                ui.label(RichText::new("新建分支").size(12.0).color(hex(FG)));
                ui.add(
                    egui::TextEdit::singleline(&mut name)
                        .hint_text("分支名")
                        .desired_width(180.0),
                );
                ui.horizontal(|ui| {
                    if ui.button("创建").clicked() {
                        done = true;
                    }
                    if ui.button("取消").clicked() {
                        cancel = true;
                    }
                });
            });
        self.new_branch_name = name;
        if done {
            let n = self.new_branch_name.trim().to_string();
            if !n.is_empty() {
                actions.push(Action::CreateBranch { repo: idx, name: n });
            }
            self.new_branch = None;
            self.new_branch_name.clear();
        } else if cancel {
            self.new_branch = None;
            self.new_branch_name.clear();
        }
    }

    fn draw_file_list(
        &mut self,
        ui: &mut egui::Ui,
        idx: usize,
        files: &[GitEntry],
        staged: bool,
        actions: &mut Vec<Action>,
    ) {
        let row_h = 22.0_f32;
        for e in files {
            let (rect, resp) =
                ui.allocate_exact_size(egui::vec2(ui.available_width(), row_h), egui::Sense::click());
            if resp.hovered() {
                ui.painter().rect_filled(rect, 0.0, hex(HOVER_BG));
            }
            let indent = 18.0;
            let cy = rect.center().y;
            // 状态字母（VS Code 放在行的最右侧）
            let badge = if staged {
                e.staged
            } else if e.worktree == '?' || e.staged == '?' {
                'U'
            } else {
                e.worktree
            };
            let color = badge_color(badge);
            let badge_w = 14.0;
            // 文件图标（VS Code 在最左侧）
            let icon_x = rect.left() + indent;
            draw_codicon(
                ui.painter(),
                egui::pos2(icon_x + 5.0, cy),
                13.0,
                Codicon::File,
                hex(FG_DIM),
            );
            // 文件名在前（正常色，突出），路径在后（灰色）
            let (name, dir) = split_path(&e.path);
            let font = egui::FontId::proportional(12.0);
            let name_w = measure(ui, name, &font);
            let text_x = icon_x + 16.0;
            let avail = rect.right() - badge_w - 8.0 - text_x - name_w;
            let dir_text = if dir.is_empty() {
                String::new()
            } else {
                let mut d = truncate_to_width(ui, dir, &font, avail.max(10.0));
                d.push('/');
                d
            };
            ui.painter().text(
                egui::pos2(text_x, cy),
                egui::Align2::LEFT_CENTER,
                name,
                font.clone(),
                hex(FG),
            );
            if !dir_text.is_empty() {
                ui.painter().text(
                    egui::pos2(text_x + name_w + 4.0, cy),
                    egui::Align2::LEFT_CENTER,
                    &dir_text,
                    font.clone(),
                    hex(FG_DIM),
                );
            }
            // 状态字母（右对齐）
            ui.painter().text(
                egui::pos2(rect.right() - badge_w, cy),
                egui::Align2::LEFT_CENTER,
                badge.to_string(),
                egui::FontId::proportional(12.0),
                color,
            );
            // 悬停操作
            if resp.hovered() {
                let right = rect.right() - badge_w - 6.0;
                if staged {
                    // 取消暂存
                    let r = egui::Rect::from_center_size(
                        egui::pos2(right - 10.0, cy),
                        egui::vec2(18.0, 18.0),
                    );
                    draw_codicon(ui.painter(), r.center(), 14.0, Codicon::Remove, hex(FG_DIM));
                    let hit = ui.interact(
                        r,
                        egui::Id::new(("scm_unstage", idx, &e.path)),
                        egui::Sense::click(),
                    );
                    if hit.clicked() {
                        actions.push(Action::Unstage {
                            repo: idx,
                            path: e.path.clone(),
                        });
                    }
                } else {
                    // 丢弃
                    let r = egui::Rect::from_center_size(
                        egui::pos2(right - 10.0, cy),
                        egui::vec2(18.0, 18.0),
                    );
                    draw_codicon(ui.painter(), r.center(), 14.0, Codicon::Discard, hex(FG_DIM));
                    let hit = ui.interact(
                        r,
                        egui::Id::new(("scm_discard", idx, &e.path)),
                        egui::Sense::click(),
                    );
                    if hit.clicked() {
                        actions.push(Action::Discard {
                            repo: idx,
                            path: e.path.clone(),
                        });
                    }
                    // 暂存
                    let r2 = egui::Rect::from_center_size(
                        egui::pos2(right - 30.0, cy),
                        egui::vec2(18.0, 18.0),
                    );
                    draw_codicon(ui.painter(), r2.center(), 14.0, Codicon::Add, hex(FG_DIM));
                    let hit = ui.interact(
                        r2,
                        egui::Id::new(("scm_stage", idx, &e.path)),
                        egui::Sense::click(),
                    );
                    if hit.clicked() {
                        actions.push(Action::Stage {
                            repo: idx,
                            path: e.path.clone(),
                        });
                    }
                }
            }
            if resp.clicked() {
                actions.push(Action::OpenDiff {
                    repo: idx,
                    path: e.path.clone(),
                    cached: staged,
                });
            }
        }
    }

    // ---- GRAPH ----

    fn draw_graph(
        &mut self,
        ui: &mut egui::Ui,
        idx: usize,
        repos: &mut [RepoState],
        actions: &mut Vec<Action>,
    ) {
        // 仓库行 + 图形工具栏
        ui.horizontal(|ui| {
            ui.add_space(2.0);
            let (icon_rect, _) = ui.allocate_exact_size(egui::vec2(16.0, 18.0), egui::Sense::hover());
            draw_codicon(
                ui.painter(),
                icon_rect.center(),
                14.0,
                Codicon::SourceControl,
                hex(FG),
            );
            let name_resp = ui.add(
                egui::Label::new(
                    RichText::new(repos[idx].name.clone())
                        .size(12.5)
                        .strong()
                        .color(hex(FG)),
                )
                .sense(egui::Sense::click()),
            );
            if name_resp.clicked() {
                repos[idx].expanded_graph = !repos[idx].expanded_graph;
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let (more_rect, more) =
                    ui.allocate_exact_size(egui::vec2(20.0, 20.0), egui::Sense::click());
                draw_codicon(
                    ui.painter(),
                    more_rect.center(),
                    15.0,
                    Codicon::Ellipsis,
                    hex(FG_DIM),
                );
                if more.clicked() {
                    self.branch_popup = if self.branch_popup == Some(idx) {
                        None
                    } else {
                        Some(idx)
                    };
                }
                let (refresh_rect, refresh) =
                    ui.allocate_exact_size(egui::vec2(20.0, 20.0), egui::Sense::click());
                draw_codicon(
                    ui.painter(),
                    refresh_rect.center(),
                    15.0,
                    Codicon::Refresh,
                    hex(FG_DIM),
                );
                if refresh.clicked() {
                    actions.push(Action::RefreshRepo(idx));
                }
                let (fetch_rect, fetch) =
                    ui.allocate_exact_size(egui::vec2(20.0, 20.0), egui::Sense::click());
                draw_codicon(
                    ui.painter(),
                    fetch_rect.center(),
                    15.0,
                    Codicon::Sync,
                    hex(FG_DIM),
                );
                if fetch.clicked() {
                    actions.push(Action::Sync(idx));
                }
                fetch.on_hover_text("同步（拉取 + 变基）");
                let (push_rect, push) =
                    ui.allocate_exact_size(egui::vec2(20.0, 20.0), egui::Sense::click());
                draw_codicon(
                    ui.painter(),
                    push_rect.center(),
                    15.0,
                    Codicon::CloudUpload,
                    hex(FG_DIM),
                );
                if push.clicked() {
                    actions.push(Action::Push(idx));
                }
                push.on_hover_text("推送");
                // 暂存全部
                let (stage_rect, stage_all) =
                    ui.allocate_exact_size(egui::vec2(20.0, 20.0), egui::Sense::click());
                if stage_all.hovered() {
                    ui.painter()
                        .rect_filled(stage_rect, 3.0, hex("#ffffff").gamma_multiply(0.08));
                }
                draw_codicon(ui.painter(), stage_rect.center(), 15.0, Codicon::Add, hex(FG_DIM));
                if stage_all.clicked() {
                    actions.push(Action::StageAll(idx));
                }
                stage_all.on_hover_text("暂存所有更改");

                // 丢弃全部
                let (discard_rect, discard) =
                    ui.allocate_exact_size(egui::vec2(20.0, 20.0), egui::Sense::click());
                if discard.hovered() {
                    ui.painter()
                        .rect_filled(discard_rect, 3.0, hex("#ffffff").gamma_multiply(0.08));
                }
                draw_codicon(
                    ui.painter(),
                    discard_rect.center(),
                    15.0,
                    Codicon::Discard,
                    hex(FG_DIM),
                );
                if discard.clicked() {
                    actions.push(Action::DiscardAll(idx));
                }
                discard.on_hover_text("丢弃所有更改");
                let (pull_rect, pull) =
                    ui.allocate_exact_size(egui::vec2(20.0, 20.0), egui::Sense::click());
                draw_codicon(
                    ui.painter(),
                    pull_rect.center(),
                    15.0,
                    Codicon::CloudDownload,
                    hex(FG_DIM),
                );
                if pull.clicked() {
                    actions.push(Action::Pull(idx));
                }
                pull.on_hover_text("拉取");
            });
        });
        ui.add_space(2.0);

        if !repos[idx].expanded_graph {
            ui.horizontal(|ui| {
                ui.add_space(14.0);
                ui.label(
                    RichText::new(format!("({} commits)", repos[idx].commits.len()))
                        .size(11.0)
                        .color(hex(FG_DIM)),
                );
            });
            return;
        }

        let commits = &repos[idx].commits;
        if commits.is_empty() {
            ui.horizontal(|ui| {
                ui.add_space(14.0);
                ui.label(
                    RichText::new("(没有提交记录)")
                        .size(11.5)
                        .color(hex(FG_DIM)),
                );
            });
            return;
        }
        let selected = repos[idx].selected.clone();
        let row_h = 22.0_f32;
        let count = commits.len();
        egui::ScrollArea::vertical()
            .id_source(("scm_graph", idx))
            .auto_shrink([false, false])
            .max_height(260.0)
            .show_rows(ui, row_h, count, |ui, range| {
                for i in range {
                    let c = &commits[i];
                    let (rect, resp) = ui.allocate_exact_size(
                        egui::vec2(ui.available_width(), row_h),
                        egui::Sense::click(),
                    );
                    let is_sel = selected.as_deref() == Some(c.hash.as_str());
                    if is_sel {
                        ui.painter().rect_filled(rect, 0.0, hex(SELECTED_BG));
                    } else if resp.hovered() {
                        ui.painter().rect_filled(rect, 0.0, hex(HOVER_BG));
                    }
                    let cy = rect.center().y;
                    // 图形：竖线 + 圆点（悬停时画成空心圈，和 Git Graph 一致）
                    let gx = rect.left() + 14.0;
                    let painter = ui.painter();
                    if i + 1 < count {
                        painter.vline(
                            gx,
                            egui::Rangef::new(cy, cy + row_h),
                            egui::Stroke::new(1.5_f32, hex("#4a4a4a")),
                        );
                    }
                    if resp.hovered() {
                        painter.circle_stroke(
                            egui::pos2(gx, cy),
                            4.0,
                            egui::Stroke::new(1.5_f32, hex("#4a9eff")),
                        );
                    } else {
                        painter.circle_filled(egui::pos2(gx, cy), 4.0, hex("#4a9eff"));
                    }
                    let font = egui::FontId::proportional(12.0);
                    let small = egui::FontId::proportional(11.0);
                    let text_x = rect.left() + 26.0;
                    // 作者（右对齐）
                    let author = ellipsize(&c.author, 12);
                    let aw = if author.is_empty() {
                        0.0
                    } else {
                        measure(ui, &author, &small)
                    };
                    // 引用标签（分支名）——先量宽度，标题要给它让位
                    let refs: Vec<String> = c
                        .refs
                        .iter()
                        .map(|r| r.replace("HEAD -> ", ""))
                        .filter(|r| !r.is_empty())
                        // 最多显示两个标签（VS Code 的图形视图也只标主要分支）
                        .take(2)
                        .collect();
                    let mut ref_width = 0.0;
                    for r in &refs {
                        ref_width += measure(ui, r, &small) + 12.0;
                    }
                    // 标题（按剩余宽度截断）
                    let avail = rect.right() - 6.0 - aw - ref_width - text_x;
                    let subject = truncate_to_width(ui, &c.subject, &font, avail);
                    let subject_w = measure(ui, &subject, &font);
                    painter.text(
                        egui::pos2(text_x, cy),
                        egui::Align2::LEFT_CENTER,
                        &subject,
                        font.clone(),
                        hex(FG),
                    );
                    // 引用标签
                    let mut ref_x = text_x + subject_w + 6.0;
                    for r in &refs {
                        let w = measure(ui, r, &small);
                        let chip = egui::Rect::from_min_size(
                            egui::pos2(ref_x, cy - 8.0),
                            egui::vec2(w + 8.0, 16.0),
                        );
                        painter.rect_stroke(
                            chip,
                            3.0,
                            egui::Stroke::new(1.0_f32, hex(REF_FG).gamma_multiply(0.6)),
                        );
                        painter.text(
                            chip.center(),
                            egui::Align2::CENTER_CENTER,
                            r,
                            small.clone(),
                            hex(REF_FG),
                        );
                        ref_x += w + 12.0;
                    }
                    // 作者
                    if !author.is_empty() {
                        painter.text(
                            egui::pos2(rect.right() - 6.0 - aw, cy),
                            egui::Align2::LEFT_CENTER,
                            &author,
                            small.clone(),
                            hex(FG_DIM),
                        );
                    }
                    if resp.clicked() {
                        actions.push(Action::SelectCommit {
                            repo: idx,
                            hash: c.hash.clone(),
                        });
                    }
                    // 悬停 → 记录信息卡片的锚点（卡片本体在 draw() 末尾弹）
                    if resp.hovered() {
                        self.frame_hover = Some(HoverCard {
                            repo: idx,
                            hash: c.hash.clone(),
                            author: c.author.clone(),
                            date: c.date.clone(),
                            subject: c.subject.clone(),
                            anchor: rect,
                        });
                    }
                }
            });
    }

    // ---- 悬停信息卡片（GRAPH 行） ----

    /// 悬停在某条提交上时弹出的信息卡片（对齐 Git Graph 的提交详情）：
    ///
    /// ```text
    /// (👤) fubin  (🕘) 23 hours ago (October 20, 2026 at 4:15 PM)
    /// fragment modify stage 4
    /// 4 files changed, 883 insertions(+), 722 deletions(-)
    /// (◈) 2c6a524   [⧉]
    /// ```
    fn draw_hover_card(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        // 指针在卡片上时保持打开（否则没法点「复制」按钮）。
        if self.card_hovered {
            return;
        }
        let Some(card) = self.frame_hover.clone() else {
            self.hover_card = None;
            return;
        };
        // 换了一条提交 → 重新取详情（统计信息要 `git show --numstat`）。
        let need_detail = self
            .detail
            .as_ref()
            .map(|d| !card.hash.is_empty() && !d.hash.starts_with(&card.hash))
            .unwrap_or(true);
        if need_detail {
            actions.push(Action::RequestDetail {
                repo: card.repo,
                hash: card.hash.clone(),
            });
        }
        self.hover_card = Some(card.clone());

        let anchor = card.anchor;
        let pos = egui::pos2(anchor.right() + 10.0, anchor.top());
        let mut copy = false;
        let detail = self.detail.clone();
        let resp = egui::Area::new(egui::Id::new("scm_hover_card"))
            .fixed_pos(pos)
            .order(egui::Order::Middle)
            // 不做淡入动画：悬停卡片要立刻出现（egui 的 Area 默认淡入会先画 0 帧）。
            .fade_in(false)
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style())
                    .fill(hex("#252526"))
                    .stroke(egui::Stroke::new(1.0_f32, hex(BORDER)))
                    .inner_margin(egui::Margin::same(10.0))
                    .rounding(4.0)
                    .show(ui, |ui| {
                        // 第一行：作者 + 时间
                        ui.horizontal(|ui| {
                            let (icon_rect, _) =
                                ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
                            draw_codicon(
                                ui.painter(),
                                icon_rect.center(),
                                13.0,
                                Codicon::Account,
                                hex(FG_DIM),
                            );
                            ui.add_space(2.0);
                            ui.label(
                                RichText::new(card.author.clone())
                                    .size(12.0)
                                    .strong()
                                    .color(hex(REF_FG)),
                            );
                            ui.add_space(8.0);
                            let (icon_rect, _) =
                                ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
                            draw_codicon(
                                ui.painter(),
                                icon_rect.center(),
                                13.0,
                                Codicon::History,
                                hex(FG_DIM),
                            );
                            ui.add_space(2.0);
                            let abs = format_absolute_time(&card.date);
                            ui.label(
                                RichText::new(if abs.is_empty() {
                                    relative_time(&card.date)
                                } else {
                                    format!("{} ({abs})", relative_time(&card.date))
                                })
                                .size(11.5)
                                .color(hex(FG_DIM)),
                            );
                        });
                        ui.add_space(6.0);
                        // 第二行：标题
                        ui.label(
                            RichText::new(card.subject.clone())
                                .size(13.0)
                                .strong()
                                .color(hex(FG)),
                        );
                        ui.add_space(8.0);
                        // 第三行：改动统计
                        if let Some(d) = &detail {
                            if d.hash.starts_with(&card.hash) {
                                let n = d.files.len();
                                let file_word = if n == 1 { "file" } else { "files" };
                                ui.horizontal(|ui| {
                                    // 紧凑排版：VS Code 是一行 "5 files changed, 96 insertions(+), 29 deletions(-)"
                                    ui.spacing_mut().item_spacing.x = 0.0;
                                    ui.label(
                                        RichText::new(format!("{n} {file_word} changed"))
                                            .size(12.0)
                                            .color(hex(FG)),
                                    );
                                    let ins = d.insertions();
                                    if ins > 0 {
                                        ui.label(
                                            RichText::new(format!(", {ins} insertions(+)"))
                                                .size(12.0)
                                                .color(hex(CHANGED_ADDED)),
                                        );
                                    }
                                    let del = d.deletions();
                                    if del > 0 {
                                        ui.label(
                                            RichText::new(format!(", {del} deletions(-)"))
                                                .size(12.0)
                                                .color(hex(CHANGED_DELETED)),
                                        );
                                    }
                                });
                                ui.add_space(8.0);
                            }
                        }
                        // 第四行：短 SHA + 复制按钮
                        ui.horizontal(|ui| {
                            let (icon_rect, _) =
                                ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
                            draw_codicon(
                                ui.painter(),
                                icon_rect.center(),
                                13.0,
                                Codicon::GitCommit,
                                hex(REF_FG),
                            );
                            ui.add_space(4.0);
                            ui.label(
                                RichText::new(card.hash.clone())
                                    .size(12.0)
                                    .color(hex(REF_FG)),
                            );
                            ui.add_space(8.0);
                            let (btn_rect, btn) =
                                ui.allocate_exact_size(egui::vec2(20.0, 20.0), egui::Sense::click());
                            if btn.hovered() {
                                ui.painter()
                                    .rect_filled(btn_rect, 3.0, hex("#ffffff").gamma_multiply(0.1));
                            }
                            draw_codicon(
                                ui.painter(),
                                btn_rect.center(),
                                14.0,
                                Codicon::Clippy,
                                if btn.hovered() { hex(FG) } else { hex(FG_DIM) },
                            );
                            if btn.clicked() {
                                copy = true;
                            }
                            btn.on_hover_text("复制提交 ID");
                        });
                    });
            });
        // 指针在卡片上时保持打开（否则点不到「复制」按钮）。
        {
            let rect = resp.response.rect;
            self.card_hovered = ui
                .ctx()
                .pointer_hover_pos()
                .map(|p| rect.contains(p))
                .unwrap_or(false);
        }
        if copy {
            actions.push(Action::CopyHash(card.hash.clone()));
        }
    }

    // ---- 提交详情卡片 ----

    fn draw_detail(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let Some(detail) = self.detail.clone() else { return };
        ui.add_space(6.0);
        let frame = egui::Frame::popup(ui.style())
            .fill(hex("#252526"))
            .stroke(egui::Stroke::new(1.0_f32, hex(BORDER)))
            .inner_margin(egui::Margin::same(8.0))
            .rounding(4.0);
        let mut close = false;
        let mut show_diff = false;
        let diff_text = self.detail_diff.clone();
        frame.show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.horizontal(|ui| {
                let (icon_rect, _) =
                    ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
                draw_codicon(
                    ui.painter(),
                    icon_rect.center(),
                    14.0,
                    Codicon::Account,
                    hex(FG_DIM),
                );
                ui.add_space(4.0);
                ui.label(
                    RichText::new(detail.author.clone())
                        .size(12.0)
                        .strong()
                        .color(hex(FG)),
                );
                ui.label(
                    RichText::new(relative_time(&detail.date))
                        .size(11.0)
                        .color(hex(FG_DIM)),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let (close_rect, resp) =
                        ui.allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::click());
                    draw_codicon(
                        ui.painter(),
                        close_rect.center(),
                        14.0,
                        Codicon::Close,
                        hex(FG_DIM),
                    );
                    if resp.clicked() {
                        close = true;
                    }
                });
            });
            ui.add_space(4.0);
            ui.label(RichText::new(detail.subject.clone()).size(12.5).color(hex(FG)));
            if !detail.body.is_empty() {
                ui.add_space(2.0);
                ui.label(RichText::new(detail.body.clone()).size(11.5).color(hex(FG_DIM)));
            }
            ui.add_space(4.0);
            ui.label(RichText::new(detail.summary()).size(11.5).color(hex(FG_DIM)));
            ui.add_space(4.0);
            for f in &detail.files {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(f.path.clone()).size(11.5).color(hex(FG)));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if f.deletions > 0 {
                            ui.label(
                                RichText::new(format!("-{}", f.deletions))
                                    .size(11.0)
                                    .color(hex(CHANGED_DELETED)),
                            );
                        }
                        if f.insertions > 0 {
                            ui.label(
                                RichText::new(format!("+{}", f.insertions))
                                    .size(11.0)
                                    .color(hex(CHANGED_ADDED)),
                            );
                        }
                    });
                });
            }
            if !diff_text.is_empty() {
                ui.add_space(4.0);
                let show = ui
                    .add(
                        egui::Label::new(
                            RichText::new("显示差异")
                                .size(11.5)
                                .color(hex(ACCENT_TEXT))
                                .underline(),
                        )
                        .sense(egui::Sense::click()),
                    )
                    .on_hover_text("显示该提交的 diff");
                if show.clicked() {
                    show_diff = true;
                }
                ui.add_space(2.0);
                egui::ScrollArea::vertical()
                    .max_height(200.0)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for line in ide_git::diff::classify_diff(&diff_text) {
                            let color = match line.kind {
                                ide_git::diff::DiffLineKind::Added => hex(CHANGED_ADDED),
                                ide_git::diff::DiffLineKind::Removed => hex(CHANGED_DELETED),
                                ide_git::diff::DiffLineKind::HunkHeader => hex("#9cdcfe"),
                                ide_git::diff::DiffLineKind::Context => hex("#858585"),
                            };
                            ui.label(
                                RichText::new(line.text.as_str())
                                    .monospace()
                                    .size(10.5)
                                    .color(color),
                            );
                        }
                    });
            }
        });
        if close {
            actions.push(Action::CloseDetail);
        }
        if show_diff {
            actions.push(Action::ShowCommitDiff {
                repo: self.detail_repo,
                hash: self.detail.as_ref().map(|d| d.hash.clone()).unwrap_or_default(),
            });
        }
    }
}

// ---- 小组件 ----

/// VS Code 视图标题（CHANGES / GRAPH）。
fn section_header(ui: &mut egui::Ui, title: &str, open: bool, collapsed: &mut HashSet<String>) {
    ui.horizontal(|ui| {
        ui.add_space(2.0);
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(12.0, 18.0), egui::Sense::click());
        draw_codicon(
            ui.painter(),
            rect.center(),
            13.0,
            if open {
                Codicon::ChevronDown
            } else {
                Codicon::ChevronRight
            },
            hex(FG),
        );
        if resp.clicked() {
            if open {
                collapsed.insert(title.to_string());
            } else {
                collapsed.remove(title);
            }
        }
        ui.label(RichText::new(title).size(11.0).strong().color(hex(FG)));
    });
    ui.add_space(2.0);
}

/// 文件分组标题（暂存的更改 / 更改），返回 (展开状态, 是否被点击切换)。
fn group_header(ui: &mut egui::Ui, title: &str, count: usize, open: bool) -> (bool, bool) {
    let mut toggled = false;
    ui.horizontal(|ui| {
        ui.add_space(8.0);
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(12.0, 18.0), egui::Sense::click());
        draw_codicon(
            ui.painter(),
            rect.center(),
            13.0,
            if open {
                Codicon::ChevronDown
            } else {
                Codicon::ChevronRight
            },
            hex(FG),
        );
        if resp.clicked() {
            toggled = true;
        }
        ui.label(
            RichText::new(format!("{title} ({count})"))
                .size(11.5)
                .color(hex(FG)),
        );
    });
    (!toggled && open, toggled)
}

/// 弹出菜单里的一项。
fn menu_item(ui: &mut egui::Ui, label: &str) -> bool {
    let resp = ui.add(
        egui::Label::new(RichText::new(label).size(12.0).color(hex(FG)))
            .sense(egui::Sense::click()),
    );
    if resp.hovered() {
        ui.painter()
            .rect_filled(resp.rect, 2.0, hex("#0e639c").gamma_multiply(0.9));
    }
    resp.clicked()
}

/// 文本宽度（egui 0.28 的 `Painter` 没有 `text_size`）。
fn measure(ui: &egui::Ui, text: &str, font: &egui::FontId) -> f32 {
    ui.fonts(|f| {
        text.chars()
            .map(|c| f.glyph_width(font, c))
            .sum::<f32>()
    })
}

/// 按像素宽度截断文本（超出加省略号）。
fn truncate_to_width(ui: &egui::Ui, text: &str, font: &egui::FontId, max_w: f32) -> String {
    if max_w <= 0.0 {
        return String::new();
    }
    if measure(ui, text, font) <= max_w {
        return text.to_string();
    }
    let mut out = String::new();
    let mut w = 0.0;
    let ellipsis_w = measure(ui, "…", font);
    for c in text.chars() {
        let cw = measure(ui, &c.to_string(), font);
        if w + cw + ellipsis_w > max_w {
            break;
        }
        out.push(c);
        w += cw;
    }
    out.push('…');
    out
}

/// 状态字母配色（VS Code `gitDecoration`）。
fn badge_color(badge: char) -> egui::Color32 {
    match badge {
        'M' => hex(CHANGED_MODIFIED),
        'A' => hex(CHANGED_ADDED),
        'D' => hex(CHANGED_DELETED),
        'R' => hex(CHANGED_MODIFIED),
        'U' => hex(CHANGED_UNTRACKED),
        '?' => hex(CHANGED_UNTRACKED),
        _ => hex(FG),
    }
}

/// 拆成 (文件名, 目录)。
fn split_path(path: &str) -> (&str, &str) {
    match path.rfind('/') {
        Some(i) => (&path[i + 1..], &path[..i]),
        None => (path, ""),
    }
}

/// 超长文本截断（加省略号）。
fn ellipsize(s: &str, max: usize) -> String {
    let count = s.chars().count();
    if count <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// 绝对时间（`October 20, 2026 at 4:15 PM`）——VS Code / Git Graph 的写法。
pub fn format_absolute_time(iso: &str) -> String {
    if iso.len() < 19 {
        return String::new();
    }
    let Ok(year) = iso[0..4].parse::<i64>() else {
        return String::new();
    };
    let Ok(month) = iso[5..7].parse::<usize>() else {
        return String::new();
    };
    let Ok(day) = iso[8..10].parse::<usize>() else {
        return String::new();
    };
    let Ok(hour) = iso[11..13].parse::<usize>() else {
        return String::new();
    };
    let Ok(minute) = iso[14..16].parse::<usize>() else {
        return String::new();
    };
    const MONTHS: [&str; 12] = [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ];
    if !(1..=12).contains(&month) {
        return String::new();
    }
    let ampm = if hour < 12 { "AM" } else { "PM" };
    let h12 = match hour % 12 {
        0 => 12,
        h => h,
    };
    format!(
        "{} {}, {} at {}:{:02} {}",
        MONTHS[month - 1],
        day,
        year,
        h12,
        minute,
        ampm
    )
}

/// 相对时间（"22 hours ago"），与 VS Code 的 `humanize` 风格一致。
pub fn relative_time(iso: &str) -> String {
    let Some(ts) = parse_iso_epoch(iso) else {
        return iso.to_string();
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    humanize_seconds((now - ts).max(0))
}

/// 把秒数变成 "x minutes ago" 之类的英文短语（VS Code 风格）。
pub fn humanize_seconds(secs: i64) -> String {
    fn plural(n: i64, unit: &str) -> String {
        if n == 1 {
            format!("1 {unit} ago")
        } else {
            format!("{n} {unit}s ago")
        }
    }
    if secs < 60 {
        plural(secs.max(1), "second")
    } else if secs < 3600 {
        plural(secs / 60, "minute")
    } else if secs < 86_400 {
        plural(secs / 3600, "hour")
    } else if secs < 2_592_000 {
        plural(secs / 86_400, "day")
    } else if secs < 31_536_000 {
        plural(secs / 2_592_000, "month")
    } else {
        plural(secs / 31_536_000, "year")
    }
}

/// 解析 ISO 8601 时间（`2026-10-20T14:12:00+08:00`）为 Unix 秒。
pub fn parse_iso_epoch(iso: &str) -> Option<i64> {
    if iso.len() < 19 {
        return None;
    }
    let year: i64 = iso.get(0..4)?.parse().ok()?;
    let month: i64 = iso.get(5..7)?.parse().ok()?;
    let day: i64 = iso.get(8..10)?.parse().ok()?;
    let hour: i64 = iso.get(11..13)?.parse().ok()?;
    let min: i64 = iso.get(14..16)?.parse().ok()?;
    let sec: i64 = iso.get(17..19)?.parse().ok()?;
    let days = days_from_civil(year, month, day);
    let mut ts = days * 86_400 + hour * 3600 + min * 60 + sec;
    // 时区偏移
    let tail = &iso[19..];
    if let Some(rest) = tail.strip_prefix('+').or_else(|| tail.strip_prefix('-')) {
        let sign = if tail.starts_with('-') { -1 } else { 1 };
        if rest.len() >= 5 {
            let oh: i64 = rest[0..2].parse().ok()?;
            let om: i64 = rest[3..5].parse().ok()?;
            ts -= sign * (oh * 3600 + om * 60);
        }
    }
    Some(ts)
}

/// 1970-01-01 起的天数（Howard Hinnant 的 civil 算法）。
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn humanize_uses_vscode_wording() {
        assert_eq!(humanize_seconds(30), "30 seconds ago");
        assert_eq!(humanize_seconds(60), "1 minute ago");
        assert_eq!(humanize_seconds(3600), "1 hour ago");
        assert_eq!(humanize_seconds(3600 * 22), "22 hours ago");
        assert_eq!(humanize_seconds(86_400 * 3), "3 days ago");
    }

    #[test]
    fn parses_iso_with_offset() {
        assert_eq!(parse_iso_epoch("1970-01-01T00:00:00+00:00"), Some(0));
        assert_eq!(parse_iso_epoch("1970-01-01T08:00:00+08:00"), Some(0));
    }

    #[test]
    fn ellipsize_shortens() {
        assert_eq!(ellipsize("abc", 5), "abc");
        assert_eq!(ellipsize("abcdef", 4), "abc…");
    }

    #[test]
    fn splits_path() {
        assert_eq!(split_path("src/main.cpp"), ("main.cpp", "src"));
        assert_eq!(split_path("main.cpp"), ("main.cpp", ""));
    }

    #[test]
    fn repo_state_splits_staged_and_unstaged() {
        let mut r = RepoState::new("E:/x".into(), "x".into());
        r.status = ide_git::porcelain::parse_porcelain(
            "1 M. N... 100644 100644 100644 a b c staged.cpp\n\
             1 .M N... 100644 100644 100644 a b c unstaged.cpp\n\
             ? new.cpp\n",
        );
        assert_eq!(r.staged_files().len(), 1);
        assert_eq!(r.unstaged_files().len(), 2);
    }

    #[test]
    fn badge_colors_follow_git_decoration() {
        assert_eq!(badge_color('M'), hex(CHANGED_MODIFIED));
        assert_eq!(badge_color('A'), hex(CHANGED_ADDED));
        assert_eq!(badge_color('D'), hex(CHANGED_DELETED));
    }
}
