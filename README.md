# LiteIDE

轻量级原生 IDE，面向 Windows + WSL2 (Ubuntu 24.04) 下的 C++ 开发，对标 VSCode Dark+ 体验。
Rust 实现，目标冷启动 < 1.5s、空闲内存 < 300MB、release 二进制 < 15MB。

## 当前状态

- **阶段 1-5 已完成**：编辑器（虚拟滚动 + tree-sitter 高亮）、LSP 集成（clangd）、调试（gdb MI）、搜索（ripgrep）、Git 集成、设置/命令面板/多标签、UI 打磨（空闲内存 159MB）。
- **阶段 6 已完成**：Windows 安装包（Inno Setup）+ 绿色便携包、WSL 一键安装脚本、用户文档（安装/快捷键/配置/故障排查）、工具链缺失守护提示。
- **UI 复核改进**：左侧活动栏 + 标签条限宽 + 状态栏右对齐；编辑器选区/点击定位修复、横向滚动、断点 gutter 点击、缩进参考线、括号匹配；编辑器内查找/替换（Ctrl+F / Ctrl+H）、另存为（Ctrl+Shift+S）；搜索大小写/正则/全词/glob 过滤；Git Stage/Unstage/Discard；大纲与符号结果可点击跳转。
- **UI 复核改进第二轮**：活动栏图标改为矢量绘制（消除方框 tofu）；右侧 minimap 重写为可点击/拖拽的代码地图；Explorer 改为可展开折叠的文件树 + 文件/文件夹图标；滚轮方向对齐 VS Code 并修复闪烁；行/光标垂直居中；调试面板默认隐藏且改为 VS Code 风格分节。
- **光标/文字垂直对齐修复**：绘制的行文本带了行尾换行，导致 `LayoutJob` 多排一行、galley 高度翻倍，正文贴顶而与光标竖线错位；统一裁掉行尾换行后正文与光标/选中高亮垂直居中对齐。
- **状态栏居中 + 侧栏可挤压/动画**：底部状态栏改用 `horizontal_centered` 垂直居中；Explorer 侧栏右侧分割线可拖动挤压编辑器，拖到最左自动滑走收起、展开时平滑滑出（`animate_value_with_time`），手柄悬停高亮 + 横向缩放光标。
- **活动栏选中态修正**：活动栏图标高亮改为跟随“当前选中视图”（VS Code 的 checked 语义），拖分割线收起 / Ctrl+B 隐藏后不再“丢焦点”；收起改在**松开鼠标时**触发（避免拖拽中手柄被回收、松开误落编辑器产生选区）；再次点击已选中图标可收起侧栏。
- **标签激活指示条 + 预览/锁定标签**：激活标签上边缘加 `#007acc` 蓝色细条；单击资源管理器文件 / Ctrl+P / 跳转定义按 VS Code **预览模式**打开（标题斜体，下一次预览打开原地顶替它），双击文件或标签标题即**锁定**（正体）；编辑预览标签自动锁定，未锁定的不会顶替已锁定标签。
- **搜索并入侧栏 + Ctrl+P 文件浏览器 + 滚轮隔离**：全文搜索结果（按文件分组、命中高亮、可点击跳转）改为占据侧栏「搜索」视图，活动栏图标 / Ctrl+Shift+F / 命令面板都能切过去并聚焦输入框；Ctrl+P 变成真正的**文件系统浏览器**，输入框既能敲路径回车跳转（可进入工程以外的目录）、也能当过滤词，带「上一层」/刷新且 Esc 关闭；鼠标停在浮动窗口上滚动时不再带着编辑器一起滚；顺带修掉「内容搜索恒为 0 命中」（rg 无路径参数时去搜了空 stdin）与「异步结果不刷新」（按需重绘）两个真 bug（当时的「文本框里回车无效」是误诊，其补丁已撤销）。
- **进扩展机制 + VSCode C/C++ 配色**：新增 `ide-ext` 扩展宿主（清单/贡献点/注册表/激活隔离/回滚/耗时报告），语法高亮与主题配色都变成**可插拔扩展**（`ext-cpp-highlight` 提供 tree-sitter C++ 查询，`ext-theme-vscode` 提供 Dark+ / Light+ / Dark(Visual Studio - C/C++) 三套配色）；编辑器只持有 `dyn LanguageHighlighter` + 定长颜色数组，按后缀认领语言、逐帧零分配；高亮改走**编辑描述 + tree-sitter 增量解析**（123KB/5470 行文件：首帧 47ms → 打字 6ms，全量重解析为 28ms）。默认主题改为用户 VS Code 用的 "Dark (Visual Studio - C/C++)"。详见 `docs/extensions.md`。
- **中文输入法（IME）修复**：编辑器、面板、对话框此前**全都打不出中文**（第四轮为绕过「回车被吞」每帧把 `platform_output.ime` 清零，等于关掉了整个窗口的输入法通道）。现在：编辑器自己上报光标处的候选窗位置（`IMEOutput`，紧贴光标弹出）+ 处理 `Event::Ime`（`Preedit` 组合串正文内联显示并加下划线、`Commit` 上屏、`Disabled` 取消），内置 `TextEdit` 的输入法照旧由 egui 处理；非组合态下回车/退格/方向键不受影响。详见 `PROGRESS.md` 文末补丁节。
- **中文行光标/选区/点击定位修复**：光标 x 原本按「列号 × 等宽格宽」算，而中文走回退字体（Hack 空格 8.28px vs 微软雅黑汉字 13.64px ≈ 1.65 格），于是打中文时光标停在字中间、选区和点击定位也跟着偏。现在列 → 像素全部按**真实字形宽度**累加（`text_advance` / `col_at_x`），点击定位反查同一套宽度；横向滚动上限与 minimap 用 `char_cells` 粗估（tab = 4 格、CJK = 2 格）。详见 `PROGRESS.md`「中文行里光标不在末尾」节。
- 进度交接以 `PROGRESS.md` 为准。

## 用户文档

- [安装指南](docs/install.md) — 10 分钟跑通 Hello World
- [快捷键速查](docs/shortcuts.md)
- [配置说明](docs/settings.md)
- [故障排查](docs/troubleshooting.md)
- [扩展开发指南](docs/extensions.md) — 贡献语言高亮 / 主题配色
- [架构设计](docs/architecture.md)

## 目录结构

```
crates/
  ide-core/          核心抽象、token 分组、外壳主题色板、错误类型
  ide-ext/           进程内扩展宿主（清单/贡献点/注册表/激活记录）
  ext-cpp-highlight/ 内置扩展：C/C++ 语法高亮（tree-sitter 查询）
  ext-theme-vscode/  内置扩展：VSCode 三套 token 配色
  ide-ui/            eframe/egui 前端入口（liteide bin）
  ide-lsp/           LSP JSON-RPC 客户端（阶段2）
  ide-dap/           gdb MI 调试协议（阶段3）
  ide-search/        ripgrep 文件/内容搜索（阶段4）
  ide-git/           git CLI 封装（阶段4）
docs/          架构与开发文档
scripts/       PowerShell 验收/CI 脚本
testdata/      测试用例资源
```

## 构建

```powershell
# 依赖已走 rsproxy 国内源（.cargo/config.toml），无需改源
cargo build --release
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings

# 阶段 0 完整验收（build/test/clippy/体积/冷启动/内存）
powershell -ExecutionPolicy Bypass -File scripts\verify_stage0.ps1
```

## 技术选型

| 模块 | 选型 |
|---|---|
| GUI | egui + eframe 0.28 |
| 异步 | tokio |
| 日志 | tracing + tracing-subscriber |
| 错误 | thiserror + anyhow |
| 文本缓冲 | ropey（阶段1） |
| 语法高亮 | 扩展 `ext-cpp-highlight`（tree-sitter-cpp + 自制查询） |
| 主题配色 | 扩展 `ext-theme-vscode`（VSCode 官方主题色 JSON） |
| 语义 | clangd via LSP（阶段2） |
| 调试 | gdb MI（阶段3） |
| 搜索 | ripgrep CLI（阶段4） |
| Git | git CLI（阶段4） |

详见 [docs/architecture.md](docs/architecture.md)。
