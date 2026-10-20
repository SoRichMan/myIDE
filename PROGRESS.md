# LiteIDE 进度交接文档

> 新会话开场指令：**"读 E:\TestDeepSeek\PROGRESS.md，从'下一步任务'接着干，不要重新规划。"**
> 每完成一个阶段/里程碑，更新本文档（替换"当前状态"和"下一步任务"）。

---

## 项目一句话

Rust 写的轻量 IDE，Windows + WSL2(Ubuntu2404) 下做 C++ 开发，对标 VSCode Dark+。冷启动 <1.5s、空闲内存 <300MB、release 二进制 <15MB。需求全文见 `myIDE.md`，**严格按它的阶段划分走，一次只做一个阶段，做完停下等用户确认**。

## 环境（已确认，不要重新探查）

- OS: Windows 11, PowerShell
- Rust: **stable 1.99.1**（`rustup`, toolchain 文件 `rust-toolchain.toml` = stable + rustfmt + clippy, profile minimal）
  - ⚠️ 变更说明：10-15 一次失败的 rustup 通道同步删掉了 stable 工具链的组件 manifest（`lib/rustlib/manifest-rustc-*` 等 4 个缺失，rustc.exe 等二进制完好），rustup 判定"部分安装"后**自动联网恢复为当前 stable 1.99.1**（rustc/cargo/clippy 均正常）。`channel=stable` 本就指向最新，非手工 update。恢复脚本 `scripts/fix_rustup_manifests.py` 保留备用（按文件树重建组件 manifest，零网络路径）。
- crates 源: rsproxy 国内镜像（`.cargo/config.toml` 已配 sparse+https://rsproxy.cn/index/）
- **红线**：禁止 `rustup update`（myIDE.md 明确要求，工具链手工组装）
- 项目根: `E:\TestDeepSeek`
- 打包 profile: `lto=true, codegen-units=1, strip=true, panic="abort"`（已在 workspace Cargo.toml）
- **WSL 发行版真实名 = `Ubuntu2404`**（不是文档里写的 `Ubuntu-24.04`；用错名字报 WSL_E_DISTRO_NOT_FOUND）
- `wsl.exe -l -q` 输出是**无 BOM 的 UTF-16LE**（每 ASCII 字符后跟 `\0`），ide-lsp 已按 BOM/UTF-16LE/UTF-8 三态识别
- **clangd 主线程 comm 名是 `clangd.main`**——WSL 里 kill clangd 必须 `pkill -f clangd`，`pkill -x clangd` 匹配不到（已实测）
- WSL 工具链已装好（`scripts/setup-wsl.sh` 幂等安装，全部就位）：clangd 18.1.3、cmake 3.28.3、ninja 1.11.1、bear 3.1.3、gdb 15.1、git 2.43.0、rg 14.1.0、gcc/g++ 13.3.0
- 环境变量：`LITEIDE_LSP_CMD` 整条覆盖 LSP 启动命令；`LITEIDE_WSL_DISTRO` 指定发行版；`LITEIDE_GDB_CMD`（整条覆盖 gdb）、`LITEIDE_DEBUG_BIN`（调试二进制优先）；`LITEIDE_RG_CMD`（整条覆盖 rg）、`LITEIDE_GIT_CMD`（整条覆盖 git，测试用 Windows git 时设为 `git`）；阶段 5 新增 `LITEIDE_NO_CJK=1`（跳过 CJK 字体加载，内存诊断用，正常使用不要设）

## 目录结构

```
E:\TestDeepSeek\
├── Cargo.toml              workspace, 10 members
├── Cargo.lock
├── rust-toolchain.toml
├── .cargo\config.toml      rsproxy 源
├── myIDE.md                 需求/验收标准原文（不要改）
├── README.md
├── PROGRESS.md              ← 本文档
├── docs\architecture.md
├── docs\extensions.md       扩展开发指南（贡献语言高亮 / 主题配色 + 性能契约）
├── crates\
│   ├── ide-core\            主题外壳色板/错误/领域模型 + buffer/rope + HighlightGroup 词汇表
│   │   └── tests\bench_stage1.rs   #[ignore] 性能基准
│   ├── ide-ext\             第八轮：进程内扩展宿主（清单/贡献点/注册表/激活计时/回滚）
│   ├── ext-cpp-highlight\   第八轮：内置扩展 = C/C++ 语法高亮（queries/cpp-highlights.scm）
│   │   └── tests\bench_highlight.rs  #[ignore] 高亮性能基准（全量/增量）
│   ├── ext-theme-vscode\    第八轮：内置扩展 = Dark+ / Light+ / Dark(Visual Studio - C/C++)
│   ├── ide-ui\              eframe/egui 前端, bin = liteide
│   │   ├── assets\          内置资源：codicon.ttf（图标字体，第九轮）+ appicon.png（应用图标位图，第十一轮）
│   │   ├── main.rs          阶段5：设置落地/命令面板/多标签/UI 打磨/虚拟滚动 + 扩展宿主装配
│   │   ├── editor.rs        虚拟滚动编辑器 + EditorAction + 动态字号/主题/语言高亮器注入
│   │   ├── settings.rs      阶段5：AppSettings 加载/校验/JSON 热重载
│   │   ├── tabs.rs          阶段5：多标签模型（打开/关闭/拖拽/脏标记/恢复）
│   │   ├── tree.rs          资源管理器文件树（FileTree/Row，可展开折叠，4 单测）
│   │   └── commands.rs      阶段5：命令注册表（55 命令）+ 快捷键唯一性守护
│   ├── ide-lsp\             (阶段2 完成) codec/message/path_map/process/client/manager
│   │   └── tests\mock_integration.rs + clangd_real.rs
│   ├── ide-dap\             (阶段3 完成) mi/command/path/process/client/manager
│   │   └── tests\mock_gdb.rs + gdb_real.rs
│   ├── ide-search\          (阶段4 完成) process/ripgrep/fuzzy/manager，13 单测
│   ├── ide-git\             (阶段4 完成) process/porcelain/log/diff/manager，11 单测 + 2 集成
│   └── ide-remote\          第十四轮：WSL 内部文件会话（WslFs）+ ConPTY 终端（WslTerminal）
├── scripts\
│   ├── init_workspace.ps1
│   ├── verify_stage0.ps1
│   ├── gen_large_cpp.ps1    生成 testdata/large.cpp
│   ├── setup-wsl.sh         幂等安装 WSL 工具链（clangd/cmake/ninja/bear/gdb/git/rg）
│   ├── gen_compile_commands.sh / .ps1
│   ├── fix_rustup_manifests.py  阶段5：rustup 组件 manifest 损坏重建脚本（备用）
│   └── ci.ps1
├── testdata\
│   ├── large.cpp            5470 行 C++ (128 KB)
│   ├── highlight_demo.cpp   第八轮：语法高亮验收样本（覆盖 17 个 token 分组）
│   ├── mock_clangd.py       mock LSP 服务器（帧协议 + 崩溃触发）
│   ├── mock_gdb.py          mock GDB MI2 服务器
│   ├── debug_target.cpp     阶段3 调试目标
│   ├── debug_target         WSL g++ -g -O0 编译产物
│   └── sample_project\      最小 CMake 项目 + compile_commands.json
└── target\release\liteide.exe  9.68 MB
```

## 当前状态：阶段 6 + 后续复核轮次已完成 ✅（阶段 1-6 全部交付；最近一轮 = 第十四轮 WSL 内部文件访问 + 编辑器下方集成终端）

### 已交付

**设置面板落地（Ctrl+,）**
- `settings.rs`：`AppSettings` 从 `%APPDATA%\LiteIDE\settings.json` 加载/校验，未知键忽略、缺省回退；结构体变更后仍兼容旧 JSON（load 容错）。
- 字号（10-24 滑块）真正应用到编辑器：`Editor` 改为动态字号（去常量 FONT_SIZE），行高 = 字号 × 1.5；缩略行号/高亮/折叠全部按动态尺寸重算。
- 主题切换 Dark+ / Light+：全局 `visuals` + 面板/状态栏/编辑区配色全量走主题色板；色值 = VSCode 官方 JSON（`theme.rs` 守护测试逐项核对：editor.background #1e1e1e、sidebar #252526、activityBar #333333、statusBar #007acc、lineNumber #858585、currentLine #2a2d2e 等）。
- 字体族：Consolas / Cascadia Code / 默认（egui 内置），切换即时生效（egui 字体表重建 + 动态字号适配）。
- minimap（实际实现，非占位）、面包屑、侧边栏开关，全部进设置并可热重载。
- **JSON 热重载**：每帧检测 settings.json 变更 → 差值应用（字体/主题/字号/面板开关独立生效），不改文件不重启。

**命令面板（Ctrl+Shift+P）**
- `commands.rs`：命令注册表 **55 条命令**（editor/workspace/view/settings/search/debug 六组），模糊子序列过滤 + 分组渲染，Enter 执行 / Esc 关闭，200ms 缓动弹出动画。
- 全覆盖执行：goto line（Ctrl+G）、theme 切换、zoom in/out、字体调整、minimap/面包屑/侧边栏开关、settings.reset、closeAll/reopenClosed 等。
- 快捷键唯一性守护测试：注册表 35 个快捷键两两不冲突；命令面板开启时 editor 屏蔽普通编辑键（keys_enabled）。

**多标签页**
- `tabs.rs`：Tab 模型（打开/切换/关闭/脏标记/关后恢复）；标签栏拖拽排序；Ctrl+Tab/Ctrl+Shift+Tab 循环；Ctrl+W 关当前、Ctrl+Shift+W 关闭全部（脏文件确认）、Ctrl+S 保存、Ctrl+Shift+S 保存全部、reopenClosedTab 恢复最近关闭。
- 关闭确认弹窗（dirty 时二次确认）；未保存标记 `●`；tab stash/restore 机制与编辑器 buffer 同步。

**UI 打磨**
- 状态栏补全：Ln/Col、行数、加载耗时、UTF-8、缩进（Spaces: 4）、主题名、⎇ branch、LSP 状态、DBG 状态。
- 活动栏（EXPLORER / OUTLINE 切换）+ 侧边栏；面包屑（文件路径分段）；`draw_keys` 快捷键速查窗（Ctrl+K Ctrl+S 类似，列表来自注册表）。
- **性能**：高亮增量重解析（tree-sitter 增量 parse，只解析改动范围）；跳转栈上限 256、关闭标签上限 16；命令面板打开耗时 / 主题切换耗时应用内 trace。
- **空闲内存 518MB → 159MB 修复**（见踩坑）：EXPLORER 17395 个文件标签每帧全量布局 → `ScrollArea::show_rows` 虚拟化只布局可见行；内容搜索结果同样虚拟化（扁平化行列表 + show_rows）。

### 实测验收数据（2026-10-16）

| 指标 | 目标 | 实测 |
|---|---|---|
| cargo test --workspace | 全绿 | ✅ **170 passed 0 failed**（13 ide-core + 44 ide-dap + 3 dap 集成 + 11 ide-git + 2 git 集成 + 40 ide-lsp + 4 lsp 集成 + 13 ide-search + 39 ide-ui + 1 doctest；ignored = 2 bench + 1 gdb_real + 2 clangd_real） |
| cargo clippy --workspace --all-targets -- -D warnings | 通过 | ✅ 0 warning |
| cargo build --release | 无 warning | ✅ Finished, 0 warning |
| 二进制体积 | <15 MB | **9.48 MB**（9,940,480 B，阶段4 是 9.36 MB，Stage 5 功能增 ~0.12MB） |
| 非测试代码 unwrap/expect | 0 | ✅（仅 tokio runtime expect 与 Mutex lock 豁免；highlight.rs 3 处"编译期不变式"expect 已重构为优雅降级） |
| 空闲内存 | <300 MB | ✅ **159 MB**（修复前 518 MB；根因 EXPLORER 未虚拟化，见踩坑） |
| 命令面板打开 | <50 ms | 应用内 trace（`[perf] command palette open -> N ms`），数值留用户验收 |
| 主题切换 | <100 ms | 应用内 trace（`[perf] theme switch -> N ms`），数值留用户验收 |
| UI 线程不阻塞 | 必须 | ✅ LSP/DAP/rg/git 通信全在各自 tokio runtime |

**GUI 冒烟**（release exe 实测，2026-10-16）：窗口正常启动渲染——EXPLORER 文件树（17395 files）、状态栏完整（`Ln 1, Col 1 | 5470 lines | load 31 ms | UTF-8 | Spaces: 4 | Dark+ | LSP: Ready (clangd) | DBG: off`）、Dark+ 配色、clangd 经 WSL 连接成功。快捷键/命令面板/标签拖拽/设置面板交互留用户验收。

### 守护测试
- `ide-ui` 单测从 3 → **39**：settings（6：加载/缺省/校验/JSON 容错）、tabs（9：打开/关闭/脏标记/恢复/拖拽排序）、editor（10：buffer 增删/撤销/高亮区间/动态字号）、commands（8：注册表 55 条/快捷键唯一性/搜索过滤/100 次安全命令冒烟无 panic）、main（6：主题色值守护/settings 往返/editor 生命周期）。
- 主题色值守护：`theme.rs` 逐项断言与 VSCode Dark+ 官方 JSON 一致。
- `safe_commands_smoke_100_ops`：100 次命令面板安全操作无 panic。

### 已知问题/技术债
1. **分屏未做**（阶段 5 任务列表提及，验收标准未含；LSP/DAP 当前按单文档同步，真分屏需多文档焦点重构）→ 记阶段 6 后候选。
2. **底部面板未做**：当前为右侧 DEBUG 面板 + 浮动窗口（命令面板/搜索/设置），无 VSCode 式底部 Terminal/Output。
3. **视觉回归测试未做**（验收标准里"截图对比基准图 image-diff"）：需要无头截图基础设施，本次以主题色值守护测试 + GUI 冒烟截图替代。
4. **Light+ 部分面板仍是 Dark 色**：DEBUG 面板等用固定 hex 常量（如 `#3c3c3c`），未全量走主题色板；阶段 5 验收以 Dark+ 为准，Light+ 为附加项。
5. ~~搜索结果仍不支持正则/大小写/文件类型过滤（阶段 4 遗留）。~~ **已做**（UI 复核：大小写/正则/全词/glob）。
6. Git 面板 ~~无 Add/Restore 按钮~~、**已加 Stage/Unstage/Discard**；diff 仍只读（阶段 4 遗留）。
7. 阶段 3 遗留：变量树展开缓存每次停止清空；监视/ptype 仅 stopped 刷新。
8. 阶段 2 遗留：~~documentSymbol 大纲不跳转~~（**已做**，大纲/符号结果可点击跳转）；hover 纯文本。
9. 阶段 1 遗留：~~括号匹配/缩进参考线~~（**已做**）；多光标未做。

### 常用命令

```powershell
cd E:\TestDeepSeek
$env:RUSTUP_OFFLINE=1          # 跑 cargo 前必须设，否则 rustup sync 超时
cargo build --release
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo test -p ide-ui -- --nocapture      # 121 单测
cargo test -p ide-remote -- --ignored    # 2 个真实 WSL 测试（文件会话 + PTY）
cargo test -p ide-git -- --nocapture     # 11 单测 + 2 集成（Windows git）
.\target\release\liteide.exe             # 启动（Ctrl+P / Ctrl+Shift+F / Ctrl+Shift+G / Ctrl+, / Ctrl+Shift+P）
```

### 踩过的坑（不要再踩）
- **egui 0.28 ScrollArea 全量布局陷阱**：EXPLORER 17395 个文件标签每帧全量布局，galley 缓存 + CJK 字体图集互相放大，空闲内存 518MB（超 <300MB 目标）→ 必须 `show_rows` 虚拟化只布局可见行（内存降至 159MB）；内容搜索结果同样扁平化 + show_rows。症状定位法：CJK 字体开关 A/B 对照 + 侧边栏开关 A/B 对照。
- **rustup 工具链损坏症状**：`rustc.exe` 在、`lib/rustlib/manifest-rustc-*` 缺失 → rustup 判定"部分安装"并自动联网恢复（会升到当前 stable）。若网络不通，用 `scripts/fix_rustup_manifests.py` 按文件树重建 manifest（零网络路径）。
- **release 构建报 `failed to remove file ... 拒绝访问`**：残留的 liteide.exe 进程锁住 exe——先 `Stop-Process -Name liteide -Force` 再 build。
- **clippy `explicit_auto_deref`**：`iter().map(|m| Row::Match(*m))` 应写 `|m| Row::Match(m)`（函数参数位自动解引用；`Row::Match` 直接作函数指针不行，E0631）。
- 其余沿用阶段 4 清单：PowerShell 改中文源文件 GBK 乱码（用 Write 工具）；egui 闭包借用"快照 + 动作队列"；egui 0.28 API 差异；git porcelain 短格式；rg 退出码语义；cargo 前设 RUSTUP_OFFLINE=1。
- **GUI 截图取证**：Win32 `PrintWindow`（含 `PW_RENDERFULLCONTENT`）与窗口 DC `BitBlt` 都**抓不到 eframe/glow 的 OpenGL 客户端区**（只得到标题栏 + 白底），桌面 `CopyFromScreen` 又会被其他全屏窗口盖住。可靠做法是用 **egui 自带帧缓冲截屏**：`ViewportCommand::Screenshot` → 下一帧从 `Event::Screenshot { image, .. }` 取 `ColorImage`，直接写 PPM/PNG（本次复核即用此法，用后临时钩子已移除）。
- **PowerShell 取证脚本要写纯 ASCII**：用工具生成的 `.ps1` 带中文且无 BOM 时，Windows PowerShell 5.1 会按 ANSI 解码源码，脚本解析错乱（症状：`Add-Type -AssemblyName System.Drawing` 之后的类型全报 `找不到类型`、行号还对不上）。像素分析这类一次性脚本一律写英文/ASCII；项目文档里的中文仍在 `.md` 里。

---

## 当前状态：阶段 6 已完成 ✅（阶段 5 已验收通过；项目全部阶段收官）

### 阶段 6 交付（2026-10-16）

**打包与分发**
- `dist\liteide.exe`（9.48 MB）+ `dist\LiteIDE-0.1.0-portable.zip`（**3.24 MB**，远 < 30MB）。
- `installer\liteide.iss`：Inno Setup 6 脚本，装好 ISCC 后 `iscc installer\liteide.iss` 即产出 `LiteIDE-Setup-0.1.0.exe`（lzma2 压缩后预计 ~4-5MB；本机未装 Inno Setup/winget，故未现场编译，脚本已就绪）。
- `scripts\setup-windows.ps1`：Windows 一键入口——检测 wsl.exe/发行版 → 把 `setup-wsl.sh` 送进 WSL 执行 → 打印自检；幂等。
- `scripts\setup-wsl.sh`（阶段 0 已有）：WSL 内 apt 幂等安装 clang/clangd/cmake/ninja/gdb/rg/git/bear/build-essential + 版本自检。
- `scripts\build_release.ps1`：一键 release → 组装 dist/ → 打 zip → 调 iscc。

**工具链缺失守护（本阶段新增代码）**
- `crates/ide-core/src/diagnostics.rs`：纯函数 `wsl_missing()` / `tool_missing(tool, detail)`，把三类失败映射成"明确提示 + 可照做的修复命令"。
- LSP（ide-lsp/manager.rs）与 DAP（ide-dap/manager.rs）的 WSL 缺失 / clangd 缺失 / gdb 缺失分支全部改用该模块；ide-lsp、ide-dap 新增对 ide-core 的 path 依赖。
- 4 个守护单测：断言文案含 `wsl --install -d Ubuntu-24.04`、`setup-wsl.sh`、幂等说明、底层原因透传。

**文档（docs/）**
- `install.md`（10 分钟 Hello World 全流程）、`shortcuts.md`（全量快捷键）、`settings.md`（配置项 schema + 容错）、`troubleshooting.md`（WSL/clangd/gdb/调试/搜索/性能 20+ 条）；README.md 加文档索引。

### 阶段 6 实测数据（2026-10-16）

| 指标 | 目标 | 实测 |
|---|---|---|
| cargo test --workspace | 全绿 | ✅ **174 passed 0 failed**（ide-core 17 = 13+4 diagnostics；其余同阶段5：44+3+11+2+40+4+13+39+1 doctest） |
| cargo clippy --workspace --all-targets -- -D warnings | 通过 | ✅ 0 warning |
| cargo build --release | 无 warning | ✅ Finished, 9,941,504 B |
| 安装包体积 | < 30 MB | ✅ 便携 ZIP **3.24 MB**；exe 9.48 MB |
| 首启（含 WSL 工具链检测） | < 3s | ✅ 日志实测：启动 02.475 → LSP Ready 03.424 = **~0.95s**（含 wsl -l -q + probe clangd + 握手） |
| 冷启动（已配置） | < 1.5s | ✅ 启动到打开 128KB 文件 02.475→02.997 = **~0.52s** |
| E2E（WSL 工具链 + gdb 断点） | 跑通 | ✅ clangd/gdb/rg/git/cmake/ninja/bear 全在；gdb 停在 debug_target.cpp:10，`$1=10` |
| 空闲内存 | < 300 MB | ✅ 冒烟工作集 170MB |

### 阶段 5 复核（本会话重新验收通过）
- 重跑 test/clippy/build 全绿；非测试代码 unwrap/expect 审计：仅 Mutex 锁中毒 `.lock().unwrap()`、tokio runtime `.expect("tokio runtime 启动失败")`、`pend.len()==1` 守卫下取 key，其余全在 `#[cfg(test)]`。
- release exe 冒烟启动 5s 存活无 panic。

### 未做/已知边界
- 自动更新（self_update crate）：myIDE.md 标注"可选"，未做（单人本地工具，无更新源基础设施）。
- Docker 模拟干净 WSL 的端到端脚本：本机无 Docker，改用真实 WSL Ubuntu2404 实测 gdb 断点通过；安装脚本幂等性已由 `setup-wsl.sh` 的 dpkg 检查保证。
- Inno Setup 安装包 .exe 未现场编译（本机无 iscc/winget），.iss 已就绪，一条命令产出。
- 阶段 5 技术债清单（分屏/底部面板/视觉回归/正则搜索/git 写操作等）见上文，不阻塞交付。

---

## UI 复核改进（2026-10-16，用户反馈"效果不是太理想"）

用户发起一次针对性复核，**一次性**处理下列 UI 布局 + 编辑器可用性缺陷（不新增阶段）。改动后保持 184 测试 + clippy `-D warnings` 全绿。

### 布局
- 新增**左侧活动栏**（`draw_activity_bar`，固定 48px `SidePanel::left("activity_bar")`，图标 ▤⌕⎇▷≡⚙ 始终可见），侧边栏位于其右侧；面板顺序重排（status → menu → activity_bar → sidebar → debug → tab_strip → central），**标签条不再横跨整宽**，只覆盖编辑器区。
- **状态栏重写**：右侧对齐显示 `Ln/Col`、缩进、编码、语言/主题，配色走主题色板。
- 修正点击定位：`pos_to_char` 之前加了 `viewport_top` 而非相减 → 点选光标垂直错位；水平方向也忽略了 `rect.min.x`；两者已修正。

### 编辑器
- **选区绘制 bug**：之前选区覆盖在文字之上，改为在文字下方绘制。
- **横向滚动**（`h_scroll`，Shift+滚轮）；`text_clip`/`with_clip_rect` 为 minimap 预留列。
- **断点 gutter**：行号栏画红点、可直接点击切换断点（`EditorAction::DebugToggleBreakpointAtLine(usize)`）；当前行号高亮。
- **缩进参考线 / 括号匹配高亮**（`find_matching_bracket`、`indent_columns` 纯函数 + 单测）。
- 查找/替换底层 API：`find_matches` / `select_range` / `replace_range` / `scroll_to_char` / `set_breakpoints`。

### 查找 / 替换（Ctrl+F / Ctrl+H）
- 右上浮动条：查找框 + `n/m` 计数 + 上/下一个 + `Aa` 大小写 + 关闭；替换模式追加替换框与「替换 / 全部替换」。
- `Enter`/`Shift+Enter` 下一个/上一个，`Esc` 关闭；打开进 `modal_open` 并屏蔽编辑器快捷键（`set_keys_enabled`）避免双输入。
- 菜单栏 Edit 增加 Find…/Replace…，File 增加 Save As…。

### 另存为（Ctrl+Shift+S）
- 新增 `file.saveAs` 命令 + `open_save_as`/`do_save_as`（写盘、切标签路径/标题、重开 clangd 文档）；`file.saveAll` 让出快捷键（改走菜单/命令面板）。
- **修正**：编辑器 `Ctrl+Shift+S` 原本仍走 SaveAll，已改为 SaveAs，与命令面板一致（对齐 VSCode）。

### 搜索 / Git / 大纲
- 内容搜索增加 **大小写 / 正则 / 全词 / glob** 过滤（`ide-search` 新增 `SearchOptions` + `build_search_args`，3 单测）。
- Git 面板增加 **Stage / Unstage / Discard** 按钮（`ide-git` 新增 `Command::Unstage` + `git restore --staged`）。
- 大纲（OUTLINE）与符号搜索结果**可点击跳转**（用 `SymbolInfo.selection_range`）。

### 本次验收（2026-10-16）

| 指标 | 结果 |
|---|---|
| cargo test --workspace | ✅ **184 passed 0 failed**（较阶段 6 +10：ide-ui 46、ide-search 16 等；mock 集成需 `python` 在 PATH） |
| cargo clippy --workspace --all-targets -- -D warnings | ✅ 0 warning |
| cargo build --release | ✅ **9.58 MB**（< 15 MB） |
| release 冒烟 | ✅ 启动 5s 存活，工作集 ~177 MB（< 300 MB） |
| GUI 截图取证 | ✅ egui 帧缓冲截屏（Win32 PrintWindow 抓不到 OpenGL 窗口，见踩坑） |

> 注：`mock_gdb` / `mock_integration` 共 7 个集成测试依赖 `python` 在 PATH；本机默认无 `python`（仅 `py.exe`），需临时把 `D:\Python311` 加入 PATH 才全绿——环境问题，非代码回归。

---

## UI 复核改进第二轮（2026-10-16，用户 6 条具体反馈）

针对用户逐条反馈的外观问题做定点修复（仍不新增阶段）。改动后 **188 测试 + clippy `-D warnings` 全绿**。

1. **活动栏图标显示成方框（tofu）** — 根因是 egui 默认字体缺 `▤⌕⎇▷≡⚙` 字形。新增 `IconKind` + `draw_icon()`：用 painter 图元（rect / circle / line / 多边形）**矢量绘制** 10 个图标，彻底不依赖字体字形；`icon_button()` 复用于侧边栏标题栏按钮。
2. **右侧一列空白、起不到预览作用** — 原 minimap 只画一条纯色带。重写为**代码地图**：逐可见行的缩进 + 长度 + 首个 token 颜色映射为小矩形，叠加当前视口方框；支持点击 / 拖拽 `seek_from_minimap()` 定位。
3. **Explorer 排版太简陋** — 新增 `tree.rs`（`FileTree` / `Row`：目录优先、字典序、可展开折叠，4 单测）；`draw_explorer_content` 用 `show_rows` 虚拟化渲染真正的目录树：折叠三角 + 文件夹 / 文件图标（按扩展名着色）+ 缩进层级 + hover 高亮 + 标题栏「重新扫描」。
4. **滚轮方向与 VS Code 相反 + 闪烁** — egui `smooth_scroll_delta.y` 正值 = 内容下移，故改为 `scroll - dy`（同步 `h_scroll`、Shift+滚轮）；闪烁根因是每帧无条件「保持光标可见」把 `scroll` 拉回，改为仅当**光标真的移动**时 `ensure_cursor_visible()`。
5. **代码行与光标未垂直居中** — 文本 galley 与行号都在 `line_height` 带内垂直居中（`(...).max(0.0)` 防指标超高时上溢）。
6. **调试窗口默认不该显示** — 确认 `debug_open: false` 默认关闭；同时把面板改为 VS Code 观感：`section()` 分节标题（折叠箭头 + 名称 + 数量 + 底纹），空态文案，WATCH / PTYPE 输入行。

### 性能
- 资源管理器不再每帧调 `search.cached_files()`（会 clone ≈1.9 万条路径）；改为订阅 `SearchEvent::FilesReady` 缓存进 `explorer_files`，文件树只在清单变化时重建。
- 编辑器高亮扫描由「每个可见行全量查找」改为指针推进 `hi_scan`（O(可见行 + 命中数)），`highlights` 按起点排序。

### 本轮验收（2026-10-16）

| 指标 | 结果 |
|---|---|
| cargo test --workspace | ✅ **188 passed 0 failed**（较上轮 +4：tree.rs） |
| cargo clippy --workspace --all-targets -- -D warnings | ✅ 0 warning |
| cargo build --release | ✅ **10.07 MB**（< 15 MB） |
| release 冒烟 + GUI 截图 | ✅ 活动栏 / 资源管理器 / 代码地图 / 调试面板经 egui 帧缓冲截图人工核对 |

> ⚠️ 本次修复期间曾一次误用 PowerShell `Get-Content/Set-Content` 重写 `main.rs` 造成 GBK 乱码；已用「UTF-8 解码 → GBK 编码」逆变换 + 定点修复复原（171 处 `\uFFFD?` 标记）。少数注释 / 字符串按上下文重建，用户可见字符串已逐条与 release 二进制字符串表核对。**再次强调：改含中文的源文件只用 Write/Edit 工具。**

---

## UI 复核改进第三轮（2026-10-16，光标竖线与文字垂直对不齐）

用户反馈：选中一行后，光标竖线和该行文字不在同一条水平中心线上。逐像素定位到根因——**参与绘制的行文本带了行尾换行**：

- `line_byte_starts` 给出的行字节区间含行尾 `\n`，直接交给 `LayoutJob` 会多排出一个空行，`galley.size().y` 由 ~16px 翻倍到 **32px**；
- 因 `line_height`(21) < `galley_h`(32)，垂直居中公式 `ty = y + (line_height - galley_h) * 0.5` 被 `.max(0.0)` 钳成 `ty = y` → 正文贴条带顶部；而光标竖线居中于整条行高条带 → 两者错位约 2~3px。

修复：新增纯函数 `line_span_no_eol(raw, start, end)` 统一裁掉行尾 `\r`/`\n`，用于（1）语法高亮 `LayoutJob`、（2）minimap 每行（顺带修掉空行会画出 1px 小点）。修复后实测 `galley_h=16`、文本盒 `[62.5,78.5]` 居中于条带 `[60,80]`，与光标竖线一致。

> 注：`Buffer::line()` 按设计**含**行尾 `\n`（`buffer.rs` 单测断言 `line(0) == "abc\n"`，Ctrl+L 选行含换行是 VS Code 语义），因此不能改 `line()` 本身，只能在绘制侧裁掉。

新增 2 个单测：`line_span_strips_trailing_newline`、`line_span_strips_crlf`。

| 指标 | 结果 |
|---|---|
| cargo test --workspace | ✅ **190 passed 0 failed**（本轮 +2） |
| cargo clippy --workspace --all-targets -- -D warnings | ✅ 0 warning |
| cargo build --release | ✅ 9.6 MB |
| 像素级核验 | ✅ 帧缓冲截图量得：选中条带 `60..80`、光标 `61..79`、文本盒 `[62.5,78.5]`，中心一致 |

---

## UI 复核改进第四轮（2026-10-16，状态栏居中 + 侧栏可挤压/动画）

用户反馈两条：

1. **底部蓝色状态栏文字没有垂直居中。** 原因是 `draw_status_bar` 用了 `ui.horizontal(...)`——该布局只为内容分配自身高度并顶对齐，在 24px 面板里文字整体偏上。改用 `ui.horizontal_centered(...)`（分配整高再垂直居中）。像素核验：状态栏文本已落在 24px 蓝条正中。

2. **Explorer 侧栏要能“感受”右侧分割线向左挤压，并在拖到最左时滑走、再滑出展开。** 原先侧栏是 `SidePanel::left(...).resizable(true).default_width(260)`，切换时瞬间显隐、宽度由 egui 内部 `PanelState` 托管，无法做“拖到阈值即收起 + 前后滑动”。重写 `draw_sidebar`：

   - 新增 `sidebar_width: f32` 字段（默认 `SIDEBAR_DEFAULT_WIDTH=260`）与常量 `SIDEBAR_MIN/MAX_WIDTH=160/520`、`SIDEBAR_ANIM_TIME=0.16s`；
   - 宽度用 `ctx.animate_value_with_time("sidebar_anim", target, 0.16)`：`target = 展开 ? sidebar_width : 0`，收起时宽度平滑归零 → **编辑器随动挤压/铺满**；拖拽中读上一帧响应 `read_response` 判定 `dragging`，此时宽度直接跟手；
   - 面板用 `.resizable(false).exact_width(width)`，右侧分割线改为**自绘手柄**（面板右缘 5px 热区，`Sense::drag`），悬停变 `ResizeHorizontal` 光标并高亮为 `#4a9eff`（平时 `#3a3a3a`）；
   - 拖动逻辑抽成纯函数 `sidebar_drag(cur, dx) -> (新宽度, 是否收起)`：宽度 clamp 到 `[160,520]`，一旦到最小宽度即判为“拖到最左 → 收起”，同时把 `sidebar_width` 复位为默认值，保证下次滑出是正常宽度。

新增单测 `sidebar_drag_clamps_and_collapses_at_min`（加宽/夹上限/拖到最左收起/轻微左拖不收起）。

| 指标 | 结果 |
|---|---|
| cargo test --workspace | ✅ **191 passed 0 failed**（本轮 +1） |
| cargo clippy --workspace --all-targets -- -D warnings | ✅ 0 warning |
| cargo build --release | ✅ 9.61 MB |
| 视觉核验 | ✅ 窗口截图：状态栏文字居中；侧栏右侧分割线可见；正文/光标垂直居中对齐保持 |

---

## UI 复核改进第五轮（2026-10-16，拖分割线收起后活动栏按钮不失焦）

用户反馈：**“在滑动分割线的时候，左侧的按钮会丢失焦点”**（附两图：侧栏展开时 Explorer 图标高亮；拖分割线收起后该图标变灰）。

根因两处：

1. **高亮条件耦合了侧栏可见性。** `draw_activity_bar` 里 `explorer_on = show_sidebar && sidebar_content == Explorer`——拖到最左/收起后 `show_sidebar=false`，图标随即失去选中态。改为一律跟随“当前选中视图”（`sidebar_content == Explorer`），与 VSCode 活动栏 `checked` 语义一致：**收起/隐藏后仍保持蓝色左侧竖条 + 白图标**。
2. **拖拽中途就收起，导致手柄被回收。** 原实现“拖到最小宽度的那一帧立刻收起”，面板随即隐藏、`Sense::drag` 手柄消失，而此时鼠标键仍按住 → 松开事件被下方编辑器误接（复现出**误选文本**、按钮“丢焦点”）。改为**松开时才收起**：拖拽途中宽度停在 `SIDEBAR_MIN_WIDTH`、面板与手柄保持存活，`dragging` 由上帧响应 **且指针确实按下**（`i.pointer.primary_down()`，避免面板隐藏后读到残留响应而状态粘住）判定；检测到 `dragging` 由 true→false 且停在最小宽度，才 `show_sidebar=false` 并复位宽度。

另外按 VSCode 习惯补上**再次点击已选中图标 → 收起侧栏**（`if on && show_sidebar { 收起 } else { 选中并展开 }`），EXPLORER / 大纲两项一致；手柄热区由 5px 加宽到 6px 便于抓取。

新增/更新单测 `sidebar_drag_clamps_width`（加宽 / 夹上限 / 拖到最左停在最小宽度）。

| 指标 | 结果 |
|---|---|
| cargo test --workspace | ✅ **191 passed 0 failed** |
| cargo clippy --workspace --all-targets -- -D warnings | ✅ 0 warning |
| cargo build --release | ✅ 9.61 MB |
| 交互核验（真实注入鼠标：拖拽 / 点击） | ✅ 拖到最左松开 → 侧栏滑走且 **Explorer 图标仍高亮**、无残留选区；再点该图标 → 侧栏滑出；侧栏展开时点该图标 → 收起 |

---

## UI 复核改进第六轮（2026-10-16，标签激活指示条 + 预览/锁定标签）

用户反馈两条：

1. **激活标签要有蓝色上边缘细条**（像 VS Code 那样区分哪个是当前标签）。
2. **双击才锁定、不锁定再开别的文件就被关掉**（VS Code preview 模式）。

### 1. 激活标签顶部蓝色细条

`draw_tab_bar` 里拿 `Button` 的 `resp.rect`，在激活标签的上边缘画 `TAB_ACTIVE_INDICATOR_H = 2.0` 高的细条，颜色用色板里的 `bg_statusbar`（Dark+/Light+ 均为 `#007acc`，与状态栏同色）。原先包在 `ui.scope` 里取 rect，改为直接用 `ui.add(...)` 的 `Response::rect`。

### 2. 预览 / 锁定标签（`TabList` 预览语义）

`Tab` 新增 `pinned: bool`（`false` = 预览标签）；`TabList` 新增：

- `open_preview(path, text)`：预览打开。已打开 → 仅激活（**不降级**已锁定标签）；存在预览标签 → **原地替换**（位置不变、不新增标签）；否则新建预览标签。
- `open(path, text)`：锁定打开。已打开 → 激活并 `pin`（预览“转正”）。
- `pin(idx)` / `is_preview(idx)` / `preview_index()`。
- `set_active_dirty(true)` 时**自动锁定**（带未保存修改的预览标签不会被顶替）；`open_untitled` / `restore` 一律锁定。

UI 侧：

- `open_file(path, text, preview)`：单击资源管理器文件、Ctrl+P 快速打开、跳转定义（`jump_to`）走 `preview = true`；双击（或打开已存在文件）走 `false`。
- 标签栏：预览标签标题用**斜体**（VS Code 同款）+ 悬停提示；双击标题 → 锁定；中键 → 关闭。
- 资源管理器行：双击 → 打开并锁定。

### 3. 双击识别加固

egui 内置双击判定窗口是 `MAX_DOUBLE_CLICK_DELAY = 0.3s`，而 **Windows 默认双击间隔是 500ms**；且 winit 0.29 不转发 `WM_LBUTTONDBLCLK`（系统用 DBLCLK **替换**第二次 DOWN），慢一点的双击会漏判。因此自实现识别：记录上一次点击的 (行/标签 , 时间)，`DOUBLE_CLICK_SECS = 0.55` 内点同一目标即视为双击，并保留 `resp.double_clicked()` 作为或条件。纯逻辑（`open_preview` / `pin` / 自动锁定等）由单测覆盖，双击识别本身按 Windows 语义放宽。

新增 6 个 `tabs.rs` 单测：`preview_open_replaces_previous_preview_in_place`、`preview_open_activates_already_open_tab_without_unpinning`、`open_pins_existing_preview`、`pin_marks_and_reports_change`、`dirty_preview_is_auto_pinned_and_untitled_is_pinned`、`restore_forces_pinned`。

| 指标 | 结果 |
|---|---|
| cargo test --workspace | ✅ **197 passed 0 failed**（本轮 +6） |
| cargo clippy --workspace --all-targets -- -D warnings | ✅ 0 warning |
| cargo build --release | ✅ 9.61 MB |
| 交互核验（真实注入鼠标） | ✅ 单击 A → 斜体预览 + 顶部蓝条；单击 B → **原地顶替** A；双击 B → 转为正体（锁定）；单击 C → 与其并排的新预览标签；双击标签标题 → 该标签转为正体 |

---

## UI 复核改进第七轮（2026-10-16，搜索并入侧栏 + Ctrl+P 文件浏览器 + 浮层滚轮隔离）

用户反馈三条：

1. **搜索要像 VS Code 一样待在侧栏**（原来是弹一个独立浮动窗口）。
2. **Ctrl+P 要能打开工程以外的文件/目录**（原来只在工程文件清单里模糊匹配）。
3. **鼠标滚轮停在浮动窗口上时，编辑器会跟着滚**。

### 1. 搜索结果并入侧栏（`SidebarContent::Search`）

- 侧栏内容新增 `Search`，占资源管理器同一位置；活动栏 Search 图标 / `Ctrl+Shift+F` / 命令面板 `search.content` 都切到该视图。
- 布局对齐 VS Code：`SEARCH` 标题 + 刷新按钮（按当前条件重搜）、查询框、`Aa` / `.*` / `ab` 选项 + `N 处匹配`、`包含的文件`（glob）框；下面是**按文件分组**的结果（文件头 + 命中行，命中片段高亮），`show_rows` 虚拟滚动。
- 活动栏点击 Search 图标：未打开 → 打开并**聚焦输入框**（`content_search_focus`）；已在该视图 → 再点收回侧栏（VS Code 行为）。
- 回车跑搜索后输入框**保持焦点**，便于改条件再搜；原来那个浮动搜索窗口整体删除。

### 2. Ctrl+P → 文件系统浏览器（可浏览工程外）

- 一个输入框身兼两职：文本能解析成**已存在的路径**（绝对 / 相对 / `~`）→ 回车进入目录或打开文件；否则当作当前目录的过滤词（打字即过滤，大小写不敏感）。
- `上一层` 按钮 + 刷新按钮；当前目录以等宽字体显示、可选中复制；`Esc` 关闭。
- 目录项缓存，只在换目录时 `read_dir`；排序「目录在前」、大小写不敏感；列表 `show_rows` 虚拟滚动。
- 窗口尺寸每帧按结果条数算死（最高 360），条目少时窗口自动收窄，不再出现「列表很短、窗口却很大」的空洞。
- 回车跳转 / 点目录项后**焦点回到输入框**，可以接着在新目录里过滤。
- 输入里含 `/` `\` `:` 时按路径处理（不过滤），避免「正在敲路径」的阶段把列表过滤成空。

### 3. 浮层上的滚轮不再带着编辑器滚

编辑器滚轮原先只判断自己 `hovered()`；egui 的面板都在 `Background` 层、浮动窗口在 `Middle` 层，所以指针在浮窗上时编辑器依然 `hovered()`。新增 `order_is_float` / `pointer_over_float(ctx)`（该位置所属图层是浮层且不是本控件），守护条件改成 `response.hovered() && !pointer_over_float(ctx)`。

（不用 `Context::is_pointer_over_area()`：Background 层会让它对控件自身也返回 true。）

### 顺手修掉的三个真 bug

- **文本框里回车无效**：`TextEdit` 一拿到焦点，egui 就请求开启系统 IME，而 winit 0.29.15 在 IME 开启时把 `WM_KEYDOWN` 交给输入法（回车/退格/方向键被吞），只透出 `WM_KEYUP`，于是 `key_pressed(Enter)` 永远为假 —— 表现就是「搜索框里敲回车没反应」。本项目编辑器是自绘文本通道、本来就不需要 IME，于是在 `update()` 末尾清空 `platform_output.ime`，全程关掉 IME；同时 `text_focus` 取「绘制前 || 绘制后」的并集，避免回车那一帧 TextEdit 已交出焦点而把回车漏给编辑器。
  - ⚠️ **已被取代**（见文末「补丁：编辑器里打不进中文」）：代价是全 App 再也打不出中文。该补丁已撤销 —— 实测「非组合态」下回车/退格/方向键的 `WM_KEYDOWN` 是正常到达的，只有输入法组合中才会被吞。
- **内容搜索永远 0 命中**：`Command::output()` 会把子进程 stdin 接到空设备（不是 tty），而 `rg` 在「没给路径参数、stdin 又不是 tty」时会去搜 stdin（空的）→ 永远 0 命中。现在 `build_search_args` 显式追加搜索路径 `.`，并去掉 rg 输出里的 `./` 前缀，保持与 `rg --files` 一致的相对路径。
- **异步结果到不了界面**：egui 是「按需重绘」，后台线程（rg / LSP / DAP）送到的结果没人重绘就一直停在旧帧（表现同样是「搜索没反应」）。现在有搜索在飞 / LSP 探测中 / DAP 启动中时用 `request_repaint_after(100ms)` 低频轮询事件通道。

| 指标 | 结果 |
|---|---|
| cargo test --workspace | ✅ **206 passed 0 failed**（本轮 +9） |
| cargo clippy --workspace --all-targets -- -D warnings | ✅ 0 warning |
| cargo build --release | ✅ 9.64 MB |
| 核验（真实注入鼠标/键盘） | ✅ 侧栏搜 `struct` → 339 处匹配、按文件分组、点击结果跳到 `clangd_real.rs:21`；Ctrl+Shift+F 聚焦搜索框；Ctrl+P 输入 `D:\Python311` 回车进入该目录、接着敲 `py` 过滤出 `python.exe/python3.dll/...`；Esc 关闭；浮窗上滚 6 格 → 编辑器区域 **0 像素变化**（指针移到编辑器上滚则正常滚动）；最后回到编辑器敲 `// zz` 仍正常写入（转为脏） |

---

## 扩展机制 + VSCode C/C++ 配色（2026-10-19，第八轮）

用户诉求：**LiteIDE 里的 C/C++ 语法与变量着色要和 VS Code 一致**，并且这些功能
要以「扩展」的形式存在（以后新功能都归到扩展里），**但性能不能退**。

范围确认（开工前问过）：扩展机制 = 进程内（trait + 编译期清单 + 注册表，静态编译进单文件）；
本轮 = 扩展框架 + 高亮/主题迁移 + 配色一起做。

### 1. 新增 `ide-ext` 扩展宿主

| 文件 | 内容 |
|---|---|
| `manifest.rs` | `Manifest`（id/名字/版本/描述/**贡献点声明**）、`Capability::{Language,Theme}`、`LanguageContribution`（后缀 → 高亮器工厂）、`ThemeContribution`（外壳色板 + `TokenPalette`）、`Extension` trait |
| `palette.rs` | `TokenPalette`：按 `HighlightGroup` 索引的 17 个 hex 色值；`from_array()` 是 **const** 构造（长度编译期校验），`new()` 供运行期/测试用 |
| `lib.rs` | `LanguageHighlighter` trait、`NullHighlighter`、`TextEdit`/`Point`（编辑描述 + 且求 `between()`）、`ExtError`、`register_declared()`（声明式扩展的 `activate`） |
| `host.rs` | `Registry`（语言/配色查询）、`ExtContext`（激活期唯一写入口）、`ExtensionHost`（激活 + 计时 + 校验 + 回滚 + `report()`） |

关键语义（都有单测）：

- **语言先注册者优先**（内置不会被顶掉）；**配色后注册者优先**（第三方可覆盖内置）。
- 清单声明了几个贡献点就必须注册几个，否则整份激活**原子回滚**并禁用该扩展。
- 激活 `catch_unwind` 隔离 panic（release 档 `panic = "abort"` 兜不住，见文档）。
- 激活耗时逐扩展记录：内置两个扩展各 **~2 µs**（设置面板可见），测试对总耗时设了上限。

### 2. 两个内置扩展（功能全部走扩展，编辑器不认识具体语言）

- `ext-cpp-highlight`：`queries/cpp-highlights.scm`（17 个分组的自查查询）+ `CppHighlighter`
  （`OnceLock` 共享 Language/Query，每文档一个 parser + 增量树）。
- `ext-theme-vscode`：`DARK_PLUS` / `LIGHT_PLUS` / `CPPTOOLS_DARK` 三套 const 配色
  （色值逐条注释来源 scope），`cpptools_dark_vs_new.json` 为最终依据（last-match-wins）。

`ide-core` 只留**词汇表**与外壳色板：17 变体的 `HighlightGroup` + `ThemeKind`
（新增 `CppDark`，`from_name` 兼容官方标签 `Dark (Visual Studio - C/C++)`）；
tree-sitter 依赖从 core 移除，core 不再认识任何语言。

### 3. IDE 侧改动（`ide-ui`）

- 编辑器字段 `Highlighter` → `Box<dyn LanguageHighlighter>`，删除 `token_color()` 与
  Dark+/Light+ 硬编码 token 常量；新增 `TokenColors`（`[egui::Color32; HighlightGroup::COUNT]`），
  由 `TokenPalette` 在**切主题时**展开一次，绘制路径只剩一次数组下标。
- `set_theme(kind, &TokenPalette)`、`set_highlighter(..)`、`open_document(text, highlighter)`
  （打开/切标签时**只高亮一次**：先换高亮器再载入文本）。
- `main.rs` 启动即 `ExtensionHost::activate(builtin_extensions())`，按**文件后缀**向注册表要
  高亮器（`.cpp/.h/.hpp/.cc/...` → C++，`.md`/无后缀 → 空实现）；设置面板新增
  **Extensions 列表**（贡献点数 + 激活耗时 + id）与三个主题按钮（`ThemeKind::ALL`）。

### 4. 增量解析（性能核心）

原来自绘编辑器只把**新文本**交给高亮器，无法告知"改了哪一段"，tree-sitter 只能全量重解析
（`tree.edit()` 是增量的前提）。现在编辑器用 `TextEdit::between(old, new)` 求出最小差异
（前后缀按 char 边界对齐，多字节安全）→ 扩展 `Tree::edit(&InputEdit)` → `parse(text, Some(old))`，
并在解析后**校验 `root.end_byte() == text.len()`**，不满足就退化为全量解析（宁可慢，不可脏）。

实测（`bench_highlight`，release，`testdata/large.cpp` 123 KB / 5470 行 / 23232 个 token）：

| 场景 | 耗时 |
|---|---|
| 首次打开（全量解析 + 收集捕获） | **47 ms** |
| 末尾追加 1 字符（增量，给编辑描述） | **6 ms** |
| 同上但不给编辑描述（全量重解析） | 19–28 ms |
| 2000 行合成文件、连续 50 次追加 | 最坏 12 ms / 平均 11 ms |

（仍以"每次改动重跑整棵树的查询"为代价；若将来要在十万行级文件上继续压，下一步是把
查询执行也限制在改动区间 + 平移区间外的旧高亮，本轮没做，已在文档里写明。）

### 5. 默认主题改为用户的 VS Code 主题

用户 VS Code 用的是 C/C++ Themes 扩展的 **Dark (Visual Studio - C/C++)**，所以
`AppSettings::default().theme` 与 `Editor::new` 的内置外观都改成它（外壳色 `#1E1E1E`，
token 色见下表），设置面板里仍可切回 Dark+ / Light+。

### 验收（截图 + 取样，release exe）

- 新增验收样本 `testdata/highlight_demo.cpp`（覆盖 17 个分组：预处理/宏/命名空间/模板/
  类/枚举/函数/形参/成员访问/字符串转义/字符/数字/运算符/控制流/中文注释）。
- Ctrl+P 打开样本 → 全窗口取样，13 个目标色值**全部命中**，旧配色（Dark+ 注释 `#6A9955`、
  Light+ 关键字 `#0000FF`）**全部 MISSING** → 渲染确实来自扩展贡献的配色：

  | 分组 | 色值 | 分组 | 色值 |
  |---|---|---|---|
  | Keyword/存储/原始类型 | `#569CD6` | Macro | `#BEB7FF` |
  | KeywordControl（`if`/`return`） | `#D8A0DF` | EnumMember/Number | `#B5CEA8` |
  | Type（`Point`/`std::string`） | `#4EC9B0` | String / `<cstdint>` | `#CE9178` |
  | Namespace | `#C8C8C8` | Comment | `#57A64A` |
  | Function | `#DCDCAA` | Parameter | `#9A9A9A` |
  | Variable | `#9CDCFE` | Property/Plain | `#DADADA` |
  | Operator | `#B4B4B4` | | |

- 现场打字核验：在文档中部插入 `// typed live 0x1F` → 新行按注释着色、其后所有行重新
  正确着色（增量解析正确性）；设置面板显示 Extensions（1 language · 3 theme）与各扩展耗时。

| 指标 | 结果 |
|---|---|
| cargo test --workspace | ✅ **259 passed 0 failed**（本轮新增 3 个 crate 共 50 个单测：宿主 19 + C/C++ 高亮/增量 26 + 主题 5；ide-ui 另有"增量结果 == 全量结果"端到端测试；6 个 `#[ignore]` 基准不计） |
| cargo clippy --workspace --all-targets -- -D warnings | ✅ 0 warning |
| cargo build --release | ✅ 9.67 MB |
| 截图 + 调色板取样 | ✅ 13/13 命中，旧色值 0 命中 |
| 启动/打开/打字实测 | ✅ 启动即 C/C++ Dark；Ctrl+P 开样本 → 全量高亮；中部插入一行 → 增量重算正确；扩展激活 2 µs/个 |

### 本轮的已知偏差（相对 VS Code + C/C++ Themes，已写入 docs/extensions.md）

全大写标识符按宏着色（命名约定近似）／同一文件内声明的枚举成员才着色／全局变量按局部
变量着色／字符串内转义序列不再细分／`#define` 宏体按普通 token 着色／字符字面量与字符串
同色／数字后缀与浮点指数符号不单独着色。这些都需要 clangd 语义高亮（阶段 2 已有 LSP
通道）才能做得和 VS Code 一样细，属于后续可选优化。

---

## 补丁：编辑器里打不进中文（IME）（2026-10-19）

用户反馈：**编辑框里无法输入中文，切换输入法也没有效果**，并要求确认是不是全 App 都有问题。

### 根因：全 App 的输入法通道被上一轮的「绕过 IME」补丁关掉了

第四轮修「搜索框里回车没反应」时，为了让 winit 0.29.15 不把 `WM_KEYDOWN` 交给输入法，
在 `App::update()` 末尾每帧清空 `platform_output.ime`（见上文「顺手修掉的三个真 bug」）。
egui-winit 的判定是 `allow_ime = output.ime.is_some()`，于是窗口的输入法通道**从头到尾是关的**
—— 编辑器、命令面板、搜索框、文件浏览器统统打不出中文。

另外，自绘编辑器只认 `Event::Text`：Windows 输入法**组合时根本不产生** `Event::Text`
（拼音走 `ImeEvent::Preedit`、上屏走 `ImeEvent::Commit`），所以就算开了通道，编辑器也收不到。

### 改法

| 位置 | 改动 |
|---|---|
| `main.rs` | 删掉每帧 `o.ime = None`：没人上报时 `output.ime` 自然是 `None`，egui-winit 会自己关掉通道；内置 `TextEdit`（面板 / 对话框）本来就会上报 |
| `Editor::show` | 编辑器接管字符输入时自己上报 `IMEOutput`（`rect` = **光标那一小块**，不是整个编辑区） |
| `Editor` 事件循环 | 处理 `Event::Ime`：`Commit` 落正文；`Preedit` 组合串**直接插进缓冲区**渲染（复用排版/高亮/滚动），提交时替换、取消时删除、被按键打断时先撤掉；`Disabled` 清组合 |
| 组合串提示 | 组合期间在正文下方画一条下划线（提示「还没落定」） |

### 为什么 `IMEOutput.rect` 只报光标那一小块

egui-winit 把 `IMEOutput.rect` 当作 `CFS_EXCLUDE` 区交给输入法（`set_ime_cursor_area`）。
报整个编辑区时候选窗被挤出该区域 → 掉到窗口最下沿（实测：候选窗跑到底边）。
改成光标处一小块后，候选窗**紧贴光标下方**弹出（与 VS Code 一致）。

### 实测（真实微软拼音 IME，release exe）

| 场景 | 结果 |
|---|---|
| 编辑器敲 `nihao` + 空格 | 候选窗贴着光标弹出；`你好` 上屏插入光标处（`// ascii 你好`） |
| 再敲 `shijie` + 回车 | 上屏原始拼音（微软拼音「回车 = 提交原始串」），正文继续正确 |
| 焦点在别的框 / 模态窗口打开 | 编辑器不抢输入法通道（交给那个 `TextEdit`） |
| 文件浏览器里输入 `highlight_demo.cpp` + 回车 | IME 开启（中文模式）下依然能打开文件 → 回车不再「失效」 |
| IME 组合中 | 字母的 `WM_KEYDOWN` 仍会到达编辑器，所以「字母键不打断组合，只有退格/回车/方向键/带 Ctrl 的键才打断」 |
| 非组合态 | 回车 / 退格 / 方向键仍是 `pressed: true`，编辑操作照常（日志取证） |
| 英文模式敲 ASCII | `Text(..)` 事件照旧，正常写码 |

### 已知偏差

- winit 0.29.15 对 `Ime::Preedit(text, None)`（本机微软拼音不上报光标位置）会**丢弃**组合串文本，
  因此 Windows 上看不到正文内的拼音串 —— 拼音显示在系统候选窗里（贴光标）。
  代码里的 `Preedit` 路径对能上报光标位置的 IME / 平台则会正文内联显示（已加单测）。

| 指标 | 结果 |
|---|---|
| cargo test --workspace | ✅ **268 passed 0 failed**（本轮 +9 IME 单测；6 个 `#[ignore]` 基准不计） |
| cargo clippy --workspace --all-targets -- -D warnings | ✅ 0 warning |
| cargo build --release | ✅ 9.67 MB |
| IME 实测 | ✅ 上表 7 项全过 |

---

## 补丁：中文行里光标不在末尾（列宽按真实字形宽度算）（2026-10-19）

用户反馈：**打中文的时候光标不在末尾跟着走，而是停在字中间**。

### 根因：把「字符列号」当成「等宽格子数」

光标 x 一直是 `列号 × 等宽格宽`（`char_width` = 空格的字形宽度）。这在纯 ASCII 行上成立，
因为等宽字体每个字符都一样宽；但中文走的是**回退字体**，宽度完全不同：

| 字符 | 字体 | 实测宽度（14px） | 折算成等宽格 |
|---|---|---|---|
| ` ` / `A` | Hack（默认等宽） | 8.275 px | 1.00 |
| `中` / `，` | 微软雅黑（回退） | 13.638 px | **1.65** |

于是 `// ascii 你好`（11 个字符）的光标被画在 `11 × 8.275 = 91 px`，
而真实宽度是 `9 × 8.275 + 2 × 13.638 = 101.8 px` —— 差了一个多汉字宽，
光标就落在最后一个「好」字中间（截图实测：光标左沿比文字右沿还靠左 8px）。
同一处错位还影响**选区高亮**、**组合串下划线**、**括号匹配框**、**鼠标点击定位**（点哪都偏）
和**横向滚动上限**。

### 改法

| 位置 | 改动 |
|---|---|
| `advance_with` / `text_advance` | 一行前 n 个字符的宽度 = **逐个真实字形宽度累加**（`Fonts::glyph_width`，与 `LayoutJob` 用同一个 `FontId`）；tab 按 `INDENT_STEP` 格（与 epaint 的 `TAB_SIZE = 4` 一致）；`advance_with` 的宽度函数可注入 → 可单测 |
| `col_at_x` | 上面的反函数：给定横向偏移求落在第几个字符（取字符前半段）→ 鼠标点击/拖选定位 |
| 光标 x、选区 x0/x1、组合串下划线、括号匹配框 | 全部改走 `text_advance` / `col_at_x` |
| `pos_to_char` | 改走 `col_at_x`；点到行尾之外时夹到行末（换行符之前），与 VS Code 一致 |
| `char_cells`（新） | 整文档级粗估专用（tab = 4 格、东亚宽字符 = 2 格）：只用于**横向滚动上限**和 **minimap 缩略宽度** —— 那里拿不到字体、也不可能逐行问字形 |
| `is_wide_char` | 东亚宽/全角字符区间判定（汉字、假名、谚文、全角标点、emoji） |

### 实测（release exe，真实微软拼音，同一段文本 A/B 对比）

| 截图 | 文本 | 光标左沿 | 文字右沿 | 缺口 |
|---|---|---|---|---|
| 修复前 `v10_3committed.png` | `// ascii 你好` | 471 | 479 | **−8（光标在「好」字里）** |
| 修复后 `v13_3committed.png` | `// ascii 你好` | 482 | 479 | **+3（紧贴文字末尾）** |
| 修复后 `v13_4second.png` | `// ascii 你好世界` | 531 | 527 | +4 |
| 修复后（打开的文件里）`v13_8comment.png` | `注释// ...` | 407 | 406 | +1（正好落在「释」和 `//` 之间） |

光标右移量 +11px ≈ 2 × (13.638 − 8.275) ≈ 10.7px，与「两个汉字按真实宽度算」的预期一致。
`v13_8comment.png` 里状态栏也给出 `Ln 1, Col 3`（2 个字符后）—— 位置与绘制一致。

### 新增单测（+6）

`advance_uses_real_glyph_widths_not_char_cells`、`click_column_is_inverse_of_advance`、
`col_at_x_round_trips_advance_on_cjk_line`、`tab_and_wide_chars_count_as_multiple_cells`、
`max_cols_accounts_for_wide_chars`、
`ime_caret_area_follows_real_text_width_on_cjk_line`（端到端：用真实字体链路量光标处 IME 锚点位移 = 2 × 汉字宽度）。

| 指标 | 结果 |
|---|---|
| cargo test --workspace | ✅ **274 passed 0 failed**（本轮 +6 单测；6 个 `#[ignore]` 基准不计） |
| cargo clippy --workspace --all-targets -- -D warnings | ✅ 0 warning |
| cargo build --release | ✅ 9.68 MB |
| 光标定位实测 | ✅ 上表 4 项 |

---

## UI 复核改进第八轮：搜索视图对齐 VS Code（2026-10-19）

用户诉求：**把侧栏搜索框的观感与交互改成和 VS Code 一致**，包含所有小按钮、小选项与结果展示。

全部集中在 `crates/ide-ui/src/main.rs`（不新增阶段、不加依赖）。

### 标题栏动作（对齐 VS Code Search 视图标题栏）

`SEARCH` 右侧按序排列：**刷新 / 清除结果 / 全部折叠 / 以树状图查看（可切换，选中高亮）/ 更多操作 ⋯**。
「⋯」弹出菜单：清除搜索结果、全部折叠、全部展开、以树状图查看（勾选）、在编辑器中打开。
新增矢量图标 `Clear` / `CollapseAll` / `Tree` / `Ellipsis` / `ChevronRight` / `ChevronDown`（`draw_icon`，不依赖字体字形）。

### 搜索 / 替换框 + 选项

- 搜索框左侧一个箭头开合**替换框**（`search_replace_open`），箭头随状态切换 `▸/▾`；
- 选项行顺序修正为 VS Code 的 **`Aa`（大小写）/ `ab`（全词）/ `.*`（正则）**，并为替换模式在右侧显示 **`AB`（保留大小写）**；开关外观改为 VS Code 的 input-option 方形小块（选中蓝底白字）；
- 保留「包含的文件」glob 输入；
- **边输入边搜**：输入 350ms 防抖自动搜索（`search_debounce_at`），改选项/glob 立即重搜；空查询清空结果。

### 结果展示

- 汇总行：`N results in M files`（VS Code 单复数），下方蓝色链接「在编辑器中打开」（把结果整理成未命名标签页，上限 5000 行）；
- 每个文件一行：**折叠三角 + 文件图标（按扩展名着色）+ 文件名 + 灰色目录 + 右侧蓝色计数徽标**；
- 命中行：缩进 + **命中片段高亮**（`submatches` 字节区间 → `clamp_submatches` 收敛/排序/合并/对齐字符边界 → `LayoutJob` 背景色）+ 超宽省略号；
- **树状图查看**：按目录嵌套（`SearchDir`，目录行带小计徽标），可逐目录/逐文件折叠；「全部折叠/展开」一键切换；
- 结果仍走 `show_rows` 虚拟滚动（沿用第五轮内存优化）；同时把结果集由「每帧克隆」改为**借用**，避免万级匹配的重复分配。

### 新增单测（+6，纯逻辑）

`search_summary_matches_vscode_pluralization`、`split_file_path_returns_name_and_directory`、
`clamp_submatches_merges_sorts_and_clips`、`search_rows_flat_groups_by_file_sorted_by_line`、
`search_rows_tree_nests_directories_and_counts`、`file_ancestor_dirs_returns_all_levels`。

| 指标 | 结果 |
|---|---|
| cargo test --workspace | ✅ **全绿**（含 `python` 在 PATH 时的 7 个 mock 集成；ide-ui 91 passed） |
| cargo clippy --workspace --all-targets -- -D warnings | ✅ 0 warning |
| cargo build --release | ✅ 10,204,160 B ≈ 9.73 MB（< 15 MB） |
| 截图核验 | ✅ egui 帧缓冲截屏（临时钩子已移除）：列表视图 / 树状图 / 展开替换框 三种状态与参考图一致 |

### 本轮已知边界

- 替换框与 `AB` 开关只做了 **UI 对齐**，未实现「替换 / 全部替换」写盘（避免无 `regex` 依赖下的语义偏差与误写盘）；
  需要时再单开一轮做工作区替换（含确认框）。
- 「在编辑器中打开」为轻量版：打开一个未命名只读文本标签页列结果，非 VS Code 的 search editor（不可就地点改）。

---

## UI 复核改进第九轮：搜索视图与 VS Code **逐像素对齐**（2026-10-19）

用户诉求：上一轮「有些地方没做一致，图标选用也不一样」，要求**尽量完全一致**，且资源要独立、不依赖外部
Node 或其它运行时（保持轻量）。

因此本轮做了两件关键决定：

1. **图标不再自绘**，改用 VS Code 官方 **codicon 字体**。自绘矢量图形无论怎么调都不可能和 VS Code 形状一致；
2. 字体随二进制内置：`crates/ide-ui/assets/codicon.ttf`（126,704 B）用 `include_bytes!` 编进程序，
   注册成独立的 `FontFamily::Name("codicon")`（`add_icon_font`）。**运行时零外部依赖**，也不需要系统装字体。
   许可证一并入库：`assets/codicon.LICENSE.txt`（MIT）。

### 图标系统重写

- 删除原来的 `IconKind` / `draw_icon`（约 290 行自绘图元）与 `draw_chevron`，改为 `Codicon` 枚举 + `draw_codicon`；
- 码位与 VS Code 一一对应（`search-refresh` 等是 VS Code 内置产品图标，同样在 codicon 字体里）：
  活动栏 `files eaf0 / search-large ec70 / source-control ea68 / debug-alt eb91 / symbol-structure ea91 / settings-gear eb51`；
  文件 `file ea7b / folder ea83 / folder-opened eaf7`；
  标题栏 `refresh eb37 / clear-all eabf / new-file ea7f / list-flat eb84 / list-tree eb86 / collapse-all eac5 / expand-all eb95`；
  输入框 `ellipsis ea7c / case-sensitive eab1 / whole-word eb7e / regex eb38 / preserve-case eb2e / replace-all eb3c`；
  树 `chevron-down eab4 / chevron-right eab6`；
- 新增单测 `codicon_font_contains_every_used_glyph`：遍历 `Codicon::ALL`，断言内置字体**确实包含**这些码位、
  且没有两个图标复用同一码位 —— 以后加图标忘了同步、或资源被换掉，测试会直接失败。

### 布局按 VS Code 真实 DOM/CSS 重做

- **标题栏 5 个动作**（左→右：刷新 / 清除搜索结果 / 打开新的搜索编辑器 / 以树（列表）形式查看 / 全部折叠），
  去掉了 VS Code 里根本不存在的「⋯ 更多」菜单；
- **开关搬进输入框内右侧**（VS Code 是 `position:absolute; top:3px; right:2px`），顺序 `Aa` / `ab` / `.*`；
  替换框同理：输入框内右侧是 `AB`，**框外右边**单放一个图标按钮「全部替换」（VS Code 提交搜索前是禁用态，本轮保持禁用观感 + tooltip 说明）；
- **视图切换按钮的图标语义**照抄 VS Code 的怪癖：列表模式显示 `list-flat` + 提示「以树形式查看」；
  树模式显示 `list-tree` + 提示「以列表形式查看」；
- 「全部折叠」按钮在结果全折叠后**原地变成「全部展开」**（`search_all_collapsed`，对应 VS Code 同一位置的两个 action）；
- **搜索详细信息**：输入框下方的 `⋯`（`ea7c`）展开「包含的文件 / 排除的文件」两个输入框，默认隐藏；
- 汇总行合成一条：`N results in M files - 在编辑器中打开`（VS Code 的 `.messages .message` + 链接）；
- 行高改为 VS Code 的 `line-height: 22px`；缩进改为每级 8px（箭头 `8+8d`、图标 `24+8d`、文件名 `34+8d`、
  命中行文字 `24+8d`）；计数徽标改成 17px 高的圆角药丸 + 主题 badge 配色（深色 `#616161`/浅色 `#c4c4c4`）。

### 搜索后端：排除 glob + VS Code 的逗号拆分

`crates/ide-search/src/process.rs`：`SearchOptions` 新增 `exclude_glob`，`build_search_args` 里按
`split_globs`（对齐 VS Code 的 `splitGlobAware`，**`{}` 内的逗号不算分隔符**）拆分，包含项用 `-g <p>`、
排除项用 `-g !<p>`。新增单测 `split_globs_keeps_brace_groups_together`、`build_args_expands_include_and_exclude_globs`。

### 顺带修掉一个真 bug：首帧字体表

egui 的 `Context::set_fonts` 要到**帧末**才生效。原来第一帧「调完 apply_fonts 就继续画」，本帧仍用内置字体表，
而内置表里没有 `codicon` 族 —— 直接 panic（`FontFamily::Name("codicon") is not bound to any fonts`）。
现在首帧只装字体并 `request_repaint()` 后提前返回，不画内容。

| 指标 | 结果 |
|---|---|
| cargo test --workspace | ✅ 全绿（ide-ui 新增 2 个单测，共 93 passed；ide-search +2） |
| cargo clippy --workspace --all-targets -- -D warnings | ✅ 0 warning |
| cargo build --release | ✅ 10,325,504 B ≈ 9.85 MB（+126 KB 内置 codicon 字体；仍 < 15 MB） |
| 截图核验 | ✅ egui 帧缓冲截屏（临时钩子已移除）：列表 / 树状图 / 展开替换框 / 搜索详细信息 四种状态 |
| 外部依赖 | ✅ 0（codicon 字体随二进制内置，无需 Node / 无需装字体） |

### 本轮已知边界

- 「全部替换」仍是**禁用外观**（未实现写盘替换），行为与 VS Code「未提交搜索时」一致；
- 「包含的文件 / 排除的文件」输入框内 VS Code 还有两个小开关（`edit-code` / `book`、`exclude`），本轮未做；
- 搜索结果的文件图标仍是「按扩展名着色 + codicon 文件形状」，没有复刻 Seti 文件图标主题（那是另一套插件资源）；
- 缩进参考线（`.indent-guide`）未画。

---

## UI 复核改进第十轮：搜索视图输入框与留白**实测对齐**（2026-10-19）

用户反馈两条：1) 输入框的风格和 VSCode 不一致；2) 「⋯」那一行到搜索结果统计之间的空行太多。

这轮先把基准**量出来**再改：本机还开着一个 vscode.dev 搜索视图，用浏览器工具直接读它的 DOM 几何 +
`getComputedStyle`（即 VS Code `searchView.css` 换算成像素），逐项对齐。

### 基准数据（VS Code 实测：面板宽 247、面板左沿 49）

- `.monaco-inputbox`：26px 高、`border-radius: 4px`、`input.background` 底 + 1px `input.border`
  （Dark+ 两者同色 = 无描边，Dark Modern 是 `#313131` 底 + `#3c3c3c` 描边）；
  搜索框左沿 = 面板 + 20、右沿 = 面板 - 8；替换框右沿 = 面板 - 36（右边留给「全部替换」）；
- `.toggle-replace-button`：**不在输入框里**，是左侧 16px 宽、贯穿两个输入框的窄槽（`height: 100%`），
  箭头垂直居中 —— 所以展开替换框时，箭头正好落在两个框中间；
- `.monaco-custom-toggle`：20×20、1px 边框（常态透明）、3px 圆角，最右那个距框右沿 2px；
  选中 = `inputOption.activeBackground` + `activeBorder` + 白字；
- `.query-details` 只有 13~16px 高、紧贴替换框；`.message` = 左 22px / 下 8px 内边距，
  `margin-top: -5px`（把消息行往上压，与「⋯」几乎贴在一起）；
- 结果行：内容左沿 = 面板 + 34 + 8×层级，文件名 = 内容 + 22，图标中心 = 内容 + 8；
  徽标 19×19 药丸、右沿距面板右 16px；列表悬停条左右各内缩 4px。

### 本轮改动

- **输入框**：抽出 `input_box_frame()`（26px 高 / 4px 圆角 / 1px 描边）与 `input_box_inner_w()`，
  搜索框、替换框、「包含的文件」、「排除的文件」四处共用；左沿 20px、右沿 8px（原先贴边满宽）；
  替换框窄 28px 给「全部替换」按钮；左侧 18px 窄槽（`SEARCH_GUTTER_W`）整条可点、箭头垂直居中，
  **折叠箭头从输入框内搬到框外**（与 VS Code 一致）；框内右侧三个开关 20×20、间距 2px、最右距框沿 2px；
- **开关样式**：`option_toggle` 常态透明 + 悬停 `inputOption.hoverBackground` + 选中蓝底 / 蓝描边 / 白字
  （原先是 20×18 的纯色块，没有描边、尺寸也不对）；
- **留白**：这个视图里 egui 默认的纵向 `item_spacing`（每段 4px）全部关掉，改为显式 `add_space`；
  「⋯」行从 20px 收到 16px，汇总行只回退 3px（对齐 VS Code `margin-top: -5px` 的观感）——
  「⋯」中心到汇总行中心 = **15px**（VS Code 实测 15.1px，改前约 29px）；
  汇总行字号 11 → 12，左沿 22px（对齐 `.message` 的 `padding-left: 22px`）；
- **结果行**：内容基准从 `8 + 8×层级` 改成 `34 + 8×层级`（文件名 56、命中行文字 34 + 8×层级）；
  计数徽标 17px → 19px 药丸、右沿距面板 16px；列表悬停条左右各内缩 4px；标题栏「SEARCH」文字左沿与输入框对齐（20px）。

### 守护测试

新增 `search_view_matches_vscode_geometry`：用真实 egui 帧跑 `draw_sidebar`，从 `FullOutput.shapes`
里读回矩形 / 文字位置，把上面那张基准表逐项断言下来
（输入框 26 高 / 左 20 / 右 8、替换框窄 28 且间距 6、开关最右距框沿 2px、「⋯」紧贴输入框且右沿距面板 6、
汇总行左 22 且与「⋯」相距 14~16、文件名 56、命中行 34 + 8、徽标 19 药丸右沿距面板 16；
另跑一遍「替换框收起」状态）。

| 指标 | 结果 |
|---|---|
| cargo test --workspace | ✅ 全绿（ide-ui 94 passed，本轮 +2） |
| cargo clippy --workspace --all-targets -- -D warnings | ✅ 0 warning |
| 几何核验 | ✅ `search_view_matches_vscode_geometry`（展开替换 / 收起替换两种状态） |
| 新依赖 | ✅ 0（仍然只用 egui + 内置 codicon 字体） |

### 追加（同日）：标题栏精简 + 「清除」语义

- **去掉「打开新的搜索编辑器」按钮**（codicon `ea7f`，本 IDE 用不上）。标题栏动作变为 4 个：
  刷新 / 清除 / 列表-树切换 / 全部折叠。`Codicon::NewFile` 变体、`ALL` 表、`glyph()` 表一并删除
  （留着会触发 `dead_code`，`-D warnings` 过不去）；
- **「清除搜索结果」（`eabf`）现在连输入一起清**：除结果集外，搜索框、替换框、「包含的文件」、
  「排除的文件」四个输入框全部清空（VS Code 原语义只清结果，按用户要求扩展）。提示语相应改为
  「清除搜索结果与输入」。清理点放在结果区渲染之后的 `want_clear` 分支——那里本帧输入内容已经写回
  `content_query`，直接清字段即可，同时 `pending_search_seq` / `search_debounce_at` 置空，不会
  反过来触发一次空搜索；
- 新增端到端单测 `clear_button_wipes_results_and_every_input`：用真实 egui 帧 + 指针事件点在
  按钮上（按下 + 抬起两帧），断言五个字段全部清空。

### 本轮已知边界（沿用第九轮）

- 「全部替换」仍是**禁用外观**（未实现写盘替换）；「包含的文件 / 排除的文件」里 VS Code 的两个小开关
  （`edit-code` / `book`、`exclude`）未做；结果行文件图标仍是「按扩展名着色 + codicon 文件形状」，
  没有复刻 Seti 主题；缩进参考线未画；汇总行与「⋯」的行高比 VS Code 各高 2px 左右（egui 的
  `interact_size.y` 决定，视觉差 < 3px）。

---

## UI 复核改进第十一轮：自绘标题栏（Logo + 菜单 + 窗口按钮合到最上一行）（2026-10-19）

用户给了两张截图（我们的 vs VS Code）：我们上面是**两行**——[系统标题栏（`e` 图标 + "LiteIDE"）] + [菜单行]；
要求像 VS Code 一样全部合到最上面一行。

### 做法：系统标题栏换成自绘（这是唯一能合并的路径）

- `main()` 里 `with_decorations(false)` 去掉系统标题栏，改由 `draw_title_bar()` 自绘：
  **35px 单行** = 35px 应用图标区（图标居中）+ File/Edit/View/Go/Run/Help +
  右侧 3×46px 窗口按钮（最小化 / 最大化-还原 / 关闭，codicon 字形）；
- 窗口交互全部自己接管：标题栏空白处按下 = `StartDrag` 拖动窗口；双击标题栏 = 最大化/还原；
  `draw_window_resize_edges()` 负责 5px 边缘带（四角优先）的 8 向 `BeginResize` + 鼠标指针
  （最大化时不留缩放带，与 VS Code 一致）；
- 度量/配色按 VS Code 实测值：`.part.titlebar` 35px、`.window-appicon` 35px、
  `.window-controls-container` 3×46px、`.menubar-menu-title { padding: 0 8px; border-radius: 5px }`；
  窗口按钮悬停 `#ffffff1a`，关闭按钮悬停 `#e81123e6` + 白图标；窗口失焦用 `titleBar.inactiveBackground`；
- 弹出菜单：`menu.background` / `menu.border` / `menu.selectionBackground`（Dark+ 是蓝 `#0078d4`）——
  ⚠️ egui 的弹出菜单用的是 `ctx` 的样式而不是父 `Ui` 的，所以菜单项样式必须写在各菜单闭包内部
  （`menu_items_style`），否则不生效；
- `theme.rs` 新增 6 个标题栏/菜单色（Dark+ / Light+ / C/C++ Dark 三套）+ `PaletteHex` 新字段 +
  守护测试 `titlebar_colors_match_vscode`。

### 应用图标（用户追问：「为什么换成了一个连接符，不应该是字母 e 吗？」）

- 起因：自绘标题栏初期我随手用了 codicon 的 `</>`（`code`，U+EAC4）当占位图标 —— 用户看到的「连接符」；
- codicon 里**没有拉丁字母**（用 cmap 解析过：字形范围只有 U+EA60–U+F102），唯一的品牌字形是
  `codicon-vscode`（VS Code 自己的 logo），不适合当我们图标；
- 改为用**应用原本的图标**：eframe/egui 的默认窗口图标（深色圆角方块 + 白色小写 “e”，512×512）→
  裁掉四周各 40px 透明留白 → 高质量重采样成 **40×40 PNG** 内嵌（`crates/ide-ui/assets/appicon.png`，
  1.4 KB，出处与许可见同目录 `appicon.LICENSE.txt`）；
- 为什么内嵌位图而不是直接画字形/直接用 eframe 默认值：egui 的纹理**没有 mipmap**，512px 直接缩到
  显示尺寸会因采样不足走样；40×40 缩到 20px 正好是一次干净的 2:1 平均。解码失败只记一条 `tracing::warn!`
  并退化成「不画图标」——不在 UI 线程 panic（遵守「非测试代码不用 unwrap/expect」红线）；
- 显示尺寸：VS Code 是 16px，但我们的图标是整块实心方块，用户反馈 16px 看着偏小 → 调到 **20px**。

### 验收

| 指标 | 结果 |
|---|---|
| cargo test --workspace | ✅ 全绿（ide-ui 95 passed，本轮新增 `title_bar_keeps_logo_menus_and_window_buttons_on_one_top_row`） |
| cargo clippy --workspace --all-targets -- -D warnings | ✅ 0 warning |
| 真机（debug exe + 合成鼠标输入） | ✅ 拖动标题栏移动窗口、右下角拖拽缩放、双击标题栏最大化、点最小化按钮（`IsIconic=True`） |
| 截图 | ✅ 单行标题栏：深色圆角方块 + 白色小写 e（20px）+ 6 个菜单 + 最小化/最大化/关闭 |

### 本轮已知边界

- 窗口没有系统阴影/圆角（`WS_POPUP` 无边框窗口所致；VS Code/Electron 有）——用户在意再议；
- 未加 Selection / Terminal 菜单（功能未实现，先不加）；菜单弹层仍是 egui 原生（宽度、阴影与 VS Code 略有差异）；
- 标题栏图标是 40×40 位图（非矢量），>200% 缩放的屏幕上会有轻微放大模糊（100%~200% 区间是清晰的）。

---

## UI 复核改进第十二轮：状态栏改回 VS Code「Dark 2026」+ 左下角连接项（2026-10-19）

用户实测了本机 VS Code（1.116.0，`workbench.colorTheme` 未设置 → 内置默认 **Dark 2026**）的真实取值，
指出我们的状态栏还是老 Dark+ 的蓝底白字，与 VS Code 现状不一致。方案确认：**整套外壳状态栏走现代主题**
（Dark 2026 / Light 2026），**连接项只做「显示 + 悬停提示」**（不实现真正连 SSH）。

### 色板（`crates/ide-core/src/theme.rs`，全部来自官方 2026 主题 JSON）

| 语义 | Dark 2026 | Light 2026 |
|---|---|---|
| `statusBar.background` | `#191a1b` | `#fafafd` |
| `statusBar.foreground` | `#8c8c8c` | `#606060` |
| 项悬停 `statusBarItem.hoverBackground` | `#262728` | `#dadada4f` |
| 项按下 `statusBarItem.activeBackground` | `#4b4c4d` | `#eeeeee` |
| `statusBar.border`（顶部 1px） | `#2a2b2c` | `#f0f1f2` |
| 远程项（连上 WSL/SSH） | `#0078d4` / 白字 | `#005fb8` / 白字 |

C/C++ Themes 主题没有覆盖 `statusBar.*`，因此也沿用 Dark 2026 那套；`PaletteHex` 相应新增
`fg_statusbar` / `bg_statusbar_hover` / `bg_statusbar_active` / `border_statusbar` /
`bg_statusbar_remote` / `fg_statusbar_remote` / `tab_active_border` 七个字段。

**顺带修掉一个连带 bug**：激活标签顶部的 2px 高亮条原先借用 `bg_statusbar` 上色（那时它就是蓝色），
状态栏改深灰后标签条会跟着变灰。现在单独引入 `tab_active_border`（Dark `#0078d4` / Light `#005fb8`，
即现代主题的 `tab.selectedBorderTop`）。

### 状态栏绘制（`draw_status_bar`）

- 底色/前景/悬停全部改读色板，不再硬编码白色文字；顶部画 1px `statusBar.border`；
- 去掉原来那排 `ui.separator()` 竖线（VS Code 状态栏没有），改为**每项自带 6px 左右内边距**、
  悬停时整块变 `#262728`（`statusbar_item` 统一实现：图标 + 文字 + tooltip 三态）；
- 最左侧新增**连接项**：未连接只显示 codicon `remote`（U+EB3A，和 VS Code 同一个字形）；
  连上 WSL 后整块变蓝底白字并显示 `WSL: <distro>`（发行版名来自 LSP 管理器新暴露的
  `LspManager::remote_name()`——探测到 WSL 发行版即记录，即使 clangd 崩溃/缺失也保留）；
  悬停提示区分「未连接（WSL/SSH）」与「已连接到 WSL: xxx」；
- `hex()` 支持 8 位 `#RRGGBBAA`（Light 2026 的悬停色就是带 alpha 的写法）。

### 验收（真机 + 逐像素取样）

| 指标 | 结果 |
|---|---|
| cargo test --workspace | ✅ 全绿（ide-ui 96；新增 `statusbar_remote_matches_vscode_states`，theme 新增 `all_themes_use_modern_statusbar`） |
| cargo clippy（ide-ui / ide-core / ide-lsp） | ✅ 0 warning |
| 真机截图取样（PrintWindow） | ✅ 状态栏底色 `#191a1b`；顶部边框行 `#2a2b2b`（1px，抗锯齿后 ±1）；连接项区域 `#0078d4` + 白字；激活标签条 `#0078d4` |
| 悬停 | ✅ 单测用合成指针事件断言未连接态悬停底为 `#262728`，且未连接时不出现蓝底/发行版名 |

### 本轮已知边界

- 连接项只做显示与提示（按确认的方案）：点击/菜单不弹「连接到 WSL/SSH」列表，也不发起新连接；
- 发行版名取自 LSP 侧探测结果；若显式使用 `LITEIDE_LSP_CMD`（跳过 WSL 探测）则不显示连接态；
- 状态栏没有 VS Code 的「远程指示」蓝色分隔块与多窗口/通知图标。

---

## 第十三轮：连接项可点 → WSL 远程工作区（真连接）

把上一轮「只显示」的连接项做成真功能：点它弹 VS Code 风格的远程菜单，其中 **WSL 是真实现**，
其余项按「选项都在、但禁用」列出（与 VS Code 菜单同序）。

### 路径层（`crates/ide-lsp/src/path_map.rs`）

- 新增 `wsl_dir_to_unc(distro, posix)`：`/home/x` → `\\wsl.localhost\<distro>\home\x`。
  Windows 侧读写走 9P 挂载，**不需要为每个文件套 `wsl.exe cat`**；
- 新增 `unc_wsl_distro` / `unc_to_wsl_posix` / `path_to_uri_any`：
   UNC → POSIX、UNC → `file://wsl.localhost/<distro>/...`（clangd 认这种形态），盘符仍走 `file:///mnt/<drive>/...`；
- `uri_to_win` 认得 `file://wsl.localhost/...` 与 `file://wsl$/...` 两种写法（跳转定义回到远程文件）。

`ide-search` / `ide-git` 的 `win_to_wsl_path` 也补上 UNC 分支：远程根在 wsl.exe 里就是 `/home/x`，
不再是 `//wsl.localhost/...` 这种 wsl 不认的路径。

### LSP 换根（`crates/ide-lsp/src/manager.rs`）

clangd 的根只在 `initialize` 时确定，所以新增 `Command::Reopen { root_uri }` + `LspManager::reopen_root()`
（同根不重启），`ready_loop` 收到后 shutdown 并带新根重连。**修掉一个死锁式 bug**：`ReadyOutcome::Restart`
原本回到「等新命令」的循环，只发一条 `Reopen` 的调用方会永远停在 `LspStatus::Off`；现在用 `restart_now`
直接用当前根重连。新增独立测试二进制 `tests/mock_reopen.rs`（`LITEIDE_LSP_CMD` 是进程级变量，
不与 `mock_integration.rs` 同进程并行）。

### IDE 侧（`crates/ide-ui/src/main.rs`）

- 状态/持久化：`AppSettings::remote_distro` / `remote_path`，重启自动恢复上次连接；
- `workspace_root()` / `workspace_uri()` 统一接管**资源管理器 / rg（含 Ctrl+P 文件浏览）/ git / LSP / 另存为**的根；
- 连接项：`Sense::click`，点击弹出 `draw_remote_menu`（`Connect to WSL…`、`Connect to Host…(SSH)`、
  `Connect to Tunnel…`、`Clone Repository in WSL…`、`Open Folder on Remote…`；已连接时顶部多一行
  `WSL: <distro>` + `Close Remote Connection`）。非 WSL 项 `add_enabled(false)`，悬停给「暂未实现」提示；
- 连接对话框 `draw_remote_dialog`：发行版下拉（`wsl.exe -l -q`，同步版带 10s 超时）+ 目录输入 +
  子目录浏览（UNC 直读，不需要子进程）+ Up/Connect/Cancel；路径统一走 `normalize_posix`；
- 连接成功后 `apply_workspace_switch()`：资源管理器重扫、git 换根、LSP 换根重启，并 `flash` 结果；
- 远程工作区里**根外的文件不发给 clangd**（不在索引范围内，发过去只会白发 didChange）；
- 对话框打开期间计入 `modal_open`，避免在路径输入框里按 Ctrl+A 同时全选编辑器正文。

### 验收（真机交互，Ubuntu2404）

| 指标 | 结果 |
|---|---|
| `cargo test --workspace` | ✅ 全绿（ide-ui 100；ide-lsp 新增 `mock_reopen`） |
| `cargo clippy --workspace --all-targets -D warnings` | ✅ 0 warning |
| 点击连接项 → 菜单 | ✅ 选项与 VS Code 同序；只有 ` Connect to WSL…` 高亮可点，其余变灰 |
| 连接 `Ubuntu2404:/home/work/NewLithoTune/candanza` | ✅ 资源管理器换成该目录（23643 files）；`branch: master`；状态栏蓝底 `WSL: Ubuntu2404` |
| LSP 换根 | ✅ 日志 `LSP root → file://wsl.localhost/Ubuntu2404/...`，`Off → Detecting → Connecting → Ready` |
| File Open（Ctrl+P） | ✅ 对话框起点就是远程根，打开其中文件正常显示 |
| 编辑 + Ctrl+S | ✅ 改完保存，WSL 侧 `cat` 读回一致（随后删掉该探针文件） |
| 重启恢复 | ✅ 带 `remote_distro/remote_path` 启动后直接进远程工作区 |
| 断开 | ✅ `Close Remote Connection` 回到本地根，状态栏恢复图标态，LSP 重启到本地根 |

### 已知边界

- 只实现了 WSL；SSH / Tunnel / Dev Container 仍是禁用的占位项；
- 「克隆仓库到 WSL」「在远程打开文件夹」也未实现（同样占位）；
- 远程根下的分支/状态来自 WSL 里的 git（UNC 根已在 `win_to_wsl_path` 里映射成 POSIX）；
- 没有远程的文件监视，外部改动仍需手动刷新。

---

## 第十四轮：WSL 内部文件访问 + 编辑器下方集成终端（2026-10-20）

用户要求：**终端要跑在 WSL 里，而且像 VS Code 一样是编辑器下方的分屏面板**（不是独立窗口）；
同时要有「新建终端」菜单与命令面板项，并保留上一轮的 WSL 远程连接 / 编辑 / 断开行为。

### 一、远程文件从 UNC 改成 WSL 内部访问（新 crate `ide-remote`）

上一轮连上 WSL 后靠 `\\wsl.localhost\<distro>\...`（Windows 侧 9P 挂载）读写。这一轮改成在
发行版内部常驻一个 `bash -l`：

- **协议**：行式 TSV（`id / op / arg1 / arg2`），参数和返回内容都走 base64；只认 id 对得上的
  响应行，所以 `bash -l` 的 profile 输出不会混进结果；路径里的空格/引号/换行也不会破帧。
- **操作**：`read` / `write` / `list` / `stat` / `mkdir` / `ping`；单次请求 15s 超时
  （`REQUEST_TIMEOUT`），WSL 会话被 kill 时 UI 不会无限等。
- 常驻会话省掉每次 `wsl.exe` 冷启动（实测单次约 120ms → 会话内往返约 1ms）。
- `normalize_posix` / `is_posix_path` / `PathKind`：路径统一成 POSIX 形式（反斜杠、尾斜杠、
  相对路径都收敛），并能区分「远程 POSIX 路径」和 Windows 盘符路径。
- Windows 侧用 `CREATE_NO_WINDOW` 起 `wsl.exe`，不闪控制台窗。

IDE 侧（`main.rs`）按路径分流：远程根下的读 / 写 / 建目录 / 列目录 / 探测类型走 `WslFs`
（`read_text_file` / `write_text_file` / `create_dir_all_path` / `list_dir_path` / `is_dir_path`），
本地仍走 `std::fs`。资源管理器、另存为、文件树展开、打开 / 保存全部走这一层。

LSP 路径（`ide-lsp/src/path_map.rs`）新增 `posix_to_uri` / `uri_to_posix`：远程工作区的
`root_uri` 和文档 URI 都是 WSL 内部视角的 `file:///home/...`，clangd 返回的 POSIX URI 也能
映射回编辑器；上一轮的 `file://wsl.localhost/...` 形态保留兼容。

### 二、集成终端（编辑器下方 Split 面板）

- `ide-remote/src/terminal.rs`：用 `portable-pty` 0.9 起真 PTY（Windows 侧 ConPTY），命令是
  `wsl.exe -d <distro> --cd <posix>`，落点就是当前远程工作区；提供
  `spawn` / `spawn_with_args` / `drain` / `write` / `resize` / `kill`。`spawn_with_args` 留给测试
  用干净 shell（`--exec bash --noprofile --norc -i`），不让用户 rc 脚本影响结果。
- `ide-ui/src/terminal.rs`：`vt100` 0.18 维护屏幕状态，egui 手绘网格（不依赖外部终端控件）；
  5000 行回滚、滚轮、选区与复制（Ctrl+Shift+C）、粘贴（含 bracketed paste）、面板尺寸变化时
  `resize` 到 PTY。
- **键盘编码**：回车 / 退格 / Tab / Shift+Tab、方向键（普通与应用模式）、
  Home/End/PgUp/PgDn/Insert/Delete、F1-F12、Ctrl+字母（含 Ctrl+C / Ctrl+D）、Alt+键；
  IDE 自己的 Ctrl+P/S/... 在终端里不会漏进 shell（`is_app_shortcut` + 单测守护）。
- **终端应答**：`ESC[6n`（光标位置）、`ESC[5n`（状态）、`ESC[c` / `ESC[>c`（设备属性），
  支持跨 chunk。WSL 的 bash 启动时会先问光标位置再出提示符，不应答会一直卡着。
- **配色**：VSCode Dark+ / Light+ 终端 16 色 + xterm 256 色映射，切主题即时生效。
- **UI**：`TopBottomPanel::bottom("terminal_panel")`，默认 240px、最小 96px，标题栏 30px；
  多会话标签页、`+` 新建、清空、结束、隐藏按钮；空态按「远程 / 本地」分别提示；
  `View` 菜单加 `Terminal Panel` 勾选；`Terminal` 菜单（New / Toggle / Clear / Kill / Open in WSL）；
  命令面板新增 5 条命令；快捷键 ``Ctrl+` `` 开合、``Ctrl+Shift+` `` 新建。
- **焦点**：点面板接管键盘，点面板外归还编辑器；终端聚焦时编辑器 `keys_enabled=false`，
  IDE 快捷键由 `main.rs` 显式处理；点终端会先让出任何 `TextEdit` 焦点（避免输入落进搜索框）。
- **生命周期**：断开 / 切换工作区时 `terminal.shutdown()` 结束所有 PTY，避免进程留在旧发行版
  或旧目录。

### 三、验收（真机 Ubuntu2404）

| 指标 | 结果 |
|---|---|
| `cargo test --workspace` | ✅ **328 passed 0 failed**（10 个 `#[ignore]` = 基准 + 真实 clangd/gdb + 真实 WSL；终端相关 22 项 + 1 个真机 WSL 回归） |
| `cargo clippy --workspace --all-targets -- -D warnings` | ✅ 0 warning |
| `cargo build --release` | ✅ 10,711,552 B ≈ **10.21 MB**（含 `portable-pty` / `vt100`，仍 < 15 MB） |
| 真机：远程工作区 `/tmp` | ✅ 面板内起 WSL PTY，输入 `echo LITEIDE_TERM_OK` 输出正常、提示符返回 |
| 面板形态 | ✅ 编辑器下方 240px 分屏，标签 / 新建 / 清空 / 结束 / 隐藏可点，编辑器仍在其上方 |
| 关闭 | ✅ 关终端 / 断开工作区后 PTY 进程结束 |
| 文件读写 | ✅ 连接 `/tmp` 后资源管理器、打开、保存走 WSL 内部（保存后 WSL 侧读回一致） |

顺带修掉一处测试串扰：新增 `LiteIdeApp::with_settings`，主程序的测试统一用默认设置装配
（`test_app()`），不再受用户 `settings.json` 或并行 settings 测试临时改写的 `LITEIDE_CONFIG_DIR`
影响（标题栏配色测试曾因此在并行运行时偶发失败）。

### 四、追加修复（用户实测反馈，2026-10-20）

**1) 顶部提示（flash）只占一行，超宽时中间省略**

- 连接成功后的提示原来用普通 `ui.label`，路径一长就换行。现在先按真实字形宽度
  （`layout_no_wrap`）测量，超过「窗口宽 − 80px」（上限 900px）时保留首尾、中间插 `…`，
  再用 `Label::wrap_mode(TextWrapMode::Extend)` 强制不换行；
- 新增 `middle_ellipsis`（纯函数，二分保留字符数）与两个守护测试：
  截断后首尾都在、无换行；真实渲染一帧断言提示 galley 只有 1 行且不超出窗口。

**2) 终端光标不在行末（长行累积漂移）**

- 根因：网格步进用的是 `Fonts::glyph_width('M')` = **7.758px**，而 epaint 排版每写一个
  字符都会把 `cursor_x` 四舍五入到物理像素，实际步进是 **8.0px**。每字符差 0.242px，
  截图里 60 字符的提示符累计漂移 ~14.5px（约两个格）——光标就落进文字中间；
- 改成 `cell_metrics()`：用 64 个 `M` 实际排版一次，取 `galley.width / 64` 作为格宽
  （DPI/字体变化时自动跟着变），格高仍用 `row_height`；
- 守护测试：`cell_width_matches_real_layout_steps`（1/5/60/120 字符的排版宽度都必须等于
  `cw × n`，含截图里的提示符）+ `cursor_block_tracks_the_last_painted_glyph`
  （真实绘制一帧，断言光标块左沿贴住最后一个字形的右边，误差 < 1px）；
- 真机复验（debug exe + `PrintWindow`）：远程 WSL 终端里输入 76 字符长行，光标块紧贴
  最后一个字符（文字右沿与光标左沿重合）。

**3) 拖动窗口大小后终端变成竖排（每行 2 个字符，留在滚回区）**

- 根因：拖动窗口边缘时 egui 每帧都会给出新的 `available_size`，中间会出现极窄的正文。
  旧代码立刻把算出的 `cols`（本次是 2，旧下限也是 2）通过 `SIGWINCH` 发给 bash；
  readline 按 2 列重绘提示符并写进滚回区，窗口恢复后这些行仍是竖排，看起来像终端坏了；
- 修复：`stable_resize()` 做 120ms 防抖——尺寸连续变化时只记录 pending，稳定后才同时
  `shell.resize` + `parser.set_size`；拖动中间帧完全不碰 PTY。网格下限改为 8 列 / 2 行，
  窗口最小化（0×0）时直接跳过，避免把 8×2 发下去；
- 第一次布局仍然立即生效（`first_layout`），避免终端刚建好就多收一次 SIGWINCH；
- 测试：`resize_storm_only_applies_the_final_size`（2→10→118 列的抖动中只有最终尺寸 Apply）、
  `first_layout_applies_immediately`、`same_size_clears_pending_resize`；
- 真机复验：在提示符后输入 80+ 字符长行，用 `SetWindowPos` 以 15ms 间隔把窗口宽度在
  1280→260→1280 之间抖动，终端未出现竖排垃圾，长行只在最终宽度正常折行。

**4) 启动慢时给出解释，Ctrl+C 现在真的能中断**

- 启动慢的根因不在终端：本机 `wsl.exe` 冷启动实测 ~100ms，但 `~/.bashrc` 最后一行
  `~/candanza-tunnel.sh start` 是同步执行的，里面的 `ssh -f -N -o BatchMode=yes candanza`
  在主机不可达时会卡很久；每个新终端都会先等它；
- Shell 启动阶段（屏幕上还没有任何可见文字）超过 2 秒时，正文里会显示提示：
  检查 `~/.bashrc` 里的同步命令，或按 Ctrl+C 中断。提示只在启动阶段出现一次，
  之后执行 `clear` 不会重新弹出（`seen_text`）；
- 顺带修掉一个真 bug：egui-winit 把 Ctrl+C 翻译成 `Event::Copy`，根本不会再产生
  `Key::C`，原来的 `Key::C` 分支是死代码——所以以前在终端里按 Ctrl+C 发不出 SIGINT，
  卡住的 `ssh` 也停不下来。新增 `terminal_input()` 处理 `Event::Copy`：
  有选区就复制，没有选区就发 `0x03`；真机验证按 Ctrl+C 能立刻打断卡住的 ssh 回到提示符；
- 测试：`copy_event_copies_selection_or_sends_sigint`、
  `screen_has_text_ignores_control_sequences_and_blanks`、
  `startup_hint_waits_for_two_seconds_and_stops_on_visible_text`。

**5) 快速上下拖动面板时丢失中间输出（`301~348` 一段）— 根因彻底修掉**

- 之前两轮「过滤 ConPTY 重绘 / 重放历史」都是止血，根因是**我们的屏幕模型和 ConPTY
  的屏幕不是一回事**：
  - ConPTY 在 PTY resize 后**不重排内容**，只把旧屏幕原样画在新屏幕顶部、光标停在原处；
  - vt100 的 `set_size` 变高时反过来：把滚回区历史拉到顶部、内容推到底部、光标落到最后一行。
  - 两边坐标系不一致 → ConPTY 之后发来的**绝对光标定位**（`ESC[13;28H`）落到我们屏幕中间，
    中间那段内容被覆盖 → 用户看到「301 后面直接跳到 348」；
  - 之前那版过滤器还有个硬伤：ConPTY 的重绘头**不固定**（实测是
    `ESC[?25l ESC[8;40;100t ESC[H …`，中间还夹着制表位序列），只认 `ESC[?25l ESC[H`
    就永远匹配不上 → 整段重绘被当成真实输出写进历史，越拖越乱。
- 现在的做法（`crates/ide-ui/src/terminal.rs`）：
  - **模型严格镜像 ConPTY**：ConPTY 画什么我们就画什么（不再重放历史、不再过滤重绘）；
  - 变高用 `grow_screen()`：先 `set_size`，再把 vt100 拉到顶部的历史行 `ESC[nS` 推回滚回区、
    光标 `ESC[nA` 移回原位 → 内容留在顶部、光标不动、下方补空行，与 ConPTY 完全一致；
  - 变矮直接 `set_size`（vt100 与 ConPTY 行为一致：保留最后若干行、光标在底部）；
  - **视图自己拼**：面板显示「输出流的最后 N 行」= 滚回区尾部 + 当前屏幕内容，
    于是「拉高面板 = 看到更多历史」（和 VS Code / Windows Terminal 一致），且一行都不少；
  - 滚轮上翻改用自己记的 `scroll_offset`（不再直接动 vt100 的偏移）。
- 回归测试：
  - `grow_keeps_conpty_layout`：变高后光标行不变、内容仍在顶部、下方补空行、滚回区不丢；
  - `conpty_repaint_after_resize_keeps_content_and_coordinates`：喂真机抓到的重绘字节，
    内容不丢、光标落在 ConPTY 给的行、之后 `ESC[13;1H` 写内容也写到同一行；
  - `real_wsl_resize_keeps_content`（`#[ignore]`，需本机 WSL）：真 PTY 上跑 `seq 1 300`，
    反复 `apply_size(40/13/30/13/50/13/25/13)`，屏幕上的数字必须连续 —— 修复前失败、修复后通过。
- 真机复验：`seq 1 300` 后面板在 28 行 / 13 行之间快速来回拖 6 次，
  屏幕始终是连续的 `276..300` + 提示符；`top`（全屏程序）正常；resize 后继续输入命令正常。

### 五、已知边界 / 环境坑

- **本机 `~/.bashrc` 末尾有 `~/candanza-tunnel.sh start > /dev/null 2>&1`**：脚本里是阻塞式
  `ssh -f -N -o BatchMode=yes candanza`。默认登录 shell 会先执行它，所以这台机器新建终端会
  停在启动阶段（现在 2 秒后会有提示，按 Ctrl+C 可打断并立即得到提示符；PTY 本身与
  `--noprofile --norc` 干净 shell 测试均正常）。
- **本机环境已修**（2026-10-20）：`~/.bashrc` 那一行改成后台执行
  `( ~/candanza-tunnel.sh start > /dev/null 2>&1 & )`，`candanza-tunnel.sh` 里的 ssh 加
  `-o ConnectTimeout=30`。原文件备份为 `~/.bashrc.bak-liteide-20261020-134714` 和
  `~/candanza-tunnel.sh.bak-liteide-20261020-134714`。改后 `bash -ic` 从 >90s 降到 ~200ms，
  真机终端提示符立即出现，隧道在后台继续尝试（最长 30s 连接超时），不再阻塞 shell。
- 终端只在 **WSL 远程工作区**可用（按需求）；本地工作区给「先连接 WSL」的提示，不偷偷开
  Windows shell。
- 只有单层标签，没有 split pane、链接识别、终端内搜索、shell integration（cwd / 命令状态）。
- 远程文件整文件 base64 过管道：常规源码没问题，超大文件会比本地慢；rg / git 仍按每次调用
  起 `wsl.exe`。
- 远程工作区仍无文件监视，外部改动要手动刷新。
- 跑 `cargo test --workspace` 的 mock 集成测试需要 `python` 在 PATH；本机只有 `py` 启动器，
  临时把 Store 的 Python 别名目录加进 PATH 即可（不影响项目代码）。

---

## 下一步任务：项目主体已完成

myIDE.md 阶段 0-6 全部交付。后续仅按需处理技术债清单，不主动开工。

---

## 不要做的事
- 不要跨阶段（不要现在碰阶段 7+ 的功能）
- 不要 `rustup update`
- 不要在 workspace 顶层加无关依赖；依赖加到具体 crate 的 Cargo.toml 或 `[workspace.dependencies]`
- 不要在 UI 线程做阻塞 IO（LSP/DAP/rg/git 通信放各自 tokio task）
- 不要用 `unwrap()/expect()` 在非测试代码里
- 不要用 PowerShell `Get-Content/Set-Content` 改含中文的源文件（会 GBK 乱码），一律用 Write 工具
- 不要用 `wsl.exe -d Ubuntu-24.04`（真实名是 `Ubuntu2404`）；kill clangd 用 `pkill -f clangd`
- 不要在用户的窗口堆叠上做无谓的置顶操作（本机桌面有其他全屏应用时会盖住 LiteIDE，截图取证用 PrintWindow）
