# 轻量级 C++ IDE 开发提示词文档

## 使用说明

将本文档完整复制给 AI（Claude / GPT / Gemini 等），作为项目启动提示词。AI 应首先输出完整的开发规划，然后按阶段逐步实现，每阶段完成后必须提供验收、测试用例和性能自查报告，等待你确认后再进入下一阶段。

---

## 一、项目目标

开发一个轻量级原生 IDE，用于替代 VSCode 在 WSL2（Ubuntu 24.04）环境下进行 C++ 项目开发。要求：

1. **功能对标 VSCode**：语法高亮、跳转、调试、搜索、Git 管理一应俱全。
2. **资源占用显著低于 VSCode**：冷启动 < 1.5 秒，空闲内存 < 300MB（不含 clangd/gdb 子进程），总体内存低于 VSCode 方案的 50%。
3. **界面风格对标 VSCode Dark+ 主题**：配色、排版、侧边栏、状态栏布局尽量一致，降低迁移成本。
4. **开发语言：Rust**。使用 winit + wgpu（或 egui / iced）构建 GUI，跨平台，便于在 Windows 端显示、通过 WSL 调用工具链。

---

## 二、核心约束

- **不重复造轮子**：语义分析用 clangd（LSP），调试用 gdb（MI 协议），搜索用 ripgrep，Git 用 git CLI。
- **前后端分离**：IDE 前端运行在 Windows，工具链（clangd、gdb、rg、git）运行在 WSL Ubuntu 24.04，通过 stdio 或 TCP 通信。
- **每个阶段必须可独立运行、可验收**，不允许"写完全部再调试"。
- **性能指标必须实测**，用数据说话，不允许估算。
- **所有代码必须包含单元测试**，关键模块要有集成测试和守护用例。
- **脚本执行规则（Windows / PowerShell）**：推荐`.ps1` 脚本 + `-File`的形式，避免截断或拼接异常（除非明显不会有问题）
- **WSL2 环境由 AI 自行配置，不要求用户手动升级或调整 WSL 本身**：
  - 不修改 WSL 版本、内核、`.wslconfig`、发行版注册等系统级配置。
  - AI 只需在现有 WSL2 + Ubuntu 24.04 环境中，通过命令行安装并验证开发所需工具链即可。
  - 需要安装/确认的工具（用 apt 或官方推荐方式）：
    - `build-essential`、`cmake`、`ninja-build`、`pkg-config`
    - `clang`、`clangd`、`lldb`（可选）、`gdb`
    - `ripgrep`、`git`、`bear`（用于生成 compile_commands.json）
    - `gcc`、`g++`（作为 clang 的备选）
  - 安装脚本必须幂等：重复执行不报错，已安装的工具自动跳过。
  - 安装完成后必须输出一份环境自检报告，列出每个工具的路径与版本号。
  - 若某个工具缺失或版本不满足要求，IDE 启动时应在状态栏或欢迎页给出明确提示和一键修复入口，而不是静默失败。

---

## 三、技术选型（已确定，不要更改）

| 模块    | 选型（尽可能选择国内源安装避免安装超时）                                      |
| ----- | --------------------------------------------------------- |
| 语言    | Rust（stable）                                              |
| GUI   | egui + eframe（快速出原型）或 winit + wgpu（更底层）                   |
| 文本编辑  | 自研 rope 缓冲（参考 xi-rope）或直接用 `ropey` crate                  |
| 语法高亮  | tree-sitter + tree-sitter-cpp                             |
| 语义跳转  | clangd（LSP，通过 `lsp-types` + `tower-lsp` 或自研 JSON-RPC 客户端） |
| 调试    | gdb MI 协议（自研解析层，参考 `gdbmi` crate）                         |
| 文件搜索  | ripgrep（调用 CLI，解析 JSON 输出）                                |
| 内容搜索  | ripgrep `--json` 模式                                       |
| Git   | git CLI（解析 `--porcelain` 输出）                              |
| 异步运行时 | tokio                                                     |
| 序列化   | serde + serde_json                                        |
| 日志    | tracing + tracing-subscriber                              |

---

## 四、开发阶段划分与验收标准

### **注意事项**

> 工具链为手工组装，禁止执行 rustup update。**归档工具链**位置：`C:\rustdl\`
> 
> 如涉及下载和环境安装，尽可能选择国内源安装避免安装超时
> 
> 一次只完成一个阶段，不要跨多个阶段完成任务

### 阶段 0：项目脚手架与基础设施

**任务**

- 创建 Cargo workspace，划分 crate：`ide-core`、`ide-ui`、`ide-lsp`、`ide-dap`（gdb MI）、`ide-search`、`ide-git`。
- 配置 CI（GitHub Actions 或本地 `cargo test` + `cargo clippy`）。
- 建立日志系统、错误处理（`anyhow` + `thiserror`）。
- 编写 README 和架构文档。

**验收标准**

- `cargo build --release` 成功，无 warning。
- `cargo test` 全绿。
- `cargo clippy -- -D warnings` 通过。
- 启动一个空窗口，标题为 "LiteIDE"，主题为 VSCode Dark+ 配色。

**测试守护用例**

- 单元测试：主题颜色常量与 VSCode Dark+ 官方色值一致（`#1e1e1e` 背景、`#d4d4d4` 前景等）。
- 集成测试：启动 → 关闭，无 panic，退出码 0。

**性能指标自查**

- 冷启动时间（进程启动到窗口可见）：< 800ms。
- 空闲内存：< 80MB。
- 二进制体积（release，strip 后）：< 15MB。

---

### 阶段 1：编辑器内核与语法高亮

**任务**

- 实现 rope 文本缓冲，支持 O(log n) 插入/删除。
- 实现光标、选区、多光标（可选）、撤销/重做。
- 集成 tree-sitter-cpp，实现增量语法高亮。
- 实现行号、当前行高亮、括号匹配、缩进参考线。
- 支持 VSCode 常用快捷键：Ctrl+S、Ctrl+Z、Ctrl+Shift+Z、Ctrl+F、Ctrl+H、Ctrl+G。

**验收标准**

- 打开一个 5000 行的 C++ 文件，滚动流畅（≥ 60 FPS）。
- 编辑时高亮实时更新，无闪烁。
- 撤销/重做 100 步无内存泄漏。

**测试守护用例**

- 单元测试：rope 插入/删除/切片在随机操作 10 万次后内容与 `Vec<char>` 一致（property-based test，用 `proptest`）。
- 单元测试：tree-sitter 解析 `int main(){}`，高亮 token 数量与预期一致。
- 集成测试：加载 `testdata/large.cpp`（5000 行），滚动 100 次，无 panic，帧时间 < 16ms。

**性能指标自查**

- 打开 5000 行文件：< 200ms。
- 单次按键到屏幕刷新：< 16ms。
- 内存增量（打开 5000 行文件）：< 30MB。

---

### 阶段 2：LSP 客户端与 clangd 集成

**任务**

- 实现 LSP JSON-RPC 客户端（Content-Length 头 + JSON body）。
- 启动 clangd 子进程（在 WSL 中通过 `wsl.exe -d Ubuntu-24.04 clangd` 调用）。
- 实现 `initialize`、`textDocument/didOpen`、`didChange`、`didSave`、`didClose`。
- 实现跳转：`textDocument/definition`、`textDocument/declaration`、`textDocument/typeDefinition`、`textDocument/implementation`。
- 实现 `textDocument/hover`（悬停提示）。
- 实现 `textDocument/documentSymbol`（大纲视图）。
- 实现快捷键：F12 跳转定义、Alt+Left 返回、Ctrl+T 符号搜索。
- 生成 `compile_commands.json` 的辅助脚本（或调用 `bear` / `cmake -DCMAKE_EXPORT_COMPILE_COMMANDS=ON`）。

**验收标准**

- 打开一个 CMake 项目，点击函数名可跳转到定义。
- Alt+Left 可返回上一位置（维护跳转栈）。
- 悬停显示函数签名和文档注释。
- clangd 崩溃时 IDE 不崩溃，可重启 clangd。

**测试守护用例**

- 单元测试：LSP 消息编解码，`Content-Length` 计算正确，UTF-8 边界处理正确。
- 集成测试：mock clangd（用 Python 脚本模拟），验证请求/响应序列化正确。
- 集成测试：真实 clangd + `testdata/sample_project`，跳转结果行号与预期一致。
- 守护用例：clangd 返回空结果、返回错误、超时三种情况下，IDE 均能优雅降级。

**性能指标自查**

- clangd 首次索引 1000 文件项目：< 60s（受 clangd 本身限制，IDE 侧开销 < 5%）。
- 跳转请求往返：< 150ms（本地项目）。
- hover 响应：< 200ms。

---

### 阶段 3：调试器（GDB MI）

**任务**

- 实现 gdb MI 协议解析层（`-exec-run`、`-break-insert`、`-exec-next`、`-exec-step`、`-exec-finish`、`-exec-continue`、`-stack-list-variables`、`-var-create`、`-var-list-children`、`-data-evaluate-expression`）。
- 实现断点：行断点、条件断点（`-break-insert -c "i > 10"`）、断点启用/禁用/删除。
- 实现单步：F10 单步跳过、F11 单步进入、Shift+F11 单步跳出、F5 继续。
- 实现变量查看面板：局部变量、监视表达式、调用栈。
- 实现结构体展开：`-var-list-children` 递归展开，显示字段名、类型、值。
- 实现 `ptype /o` 查看结构体大小和内存布局。
- 实现快捷键：F5 运行、Shift+F5 停止、F9 切换断点。
- 实现调试控制台（显示 gdb 原始输出）。

**验收标准**

- F5 启动调试，程序在断点处停下。
- 变量面板显示局部变量，结构体可展开。
- 条件断点生效（如 `i == 5` 时停下）。
- 单步、进入、跳出、继续均正常。
- 调试结束后 gdb 进程被正确清理。

**测试守护用例**

- 单元测试：MI 输出解析（`*stopped`、`=breakpoint-created`、`^done`、`^error` 等）。
- 单元测试：条件断点命令拼接正确，特殊字符转义正确。
- 集成测试：编译 `testdata/debug_target.cpp`，用 gdb 调试，验证在第 10 行停下，变量 `i == 10`。
- 集成测试：结构体 `struct Foo { int a; double b; char c[16]; };` 展开后字段数与预期一致，`sizeof(Foo)` 显示正确。
- 守护用例：gdb 崩溃、断点无法设置、变量不存在三种情况下，IDE 显示友好错误，不 panic。

**性能指标自查**

- 启动调试到首个断点停下：< 2s（不含程序编译时间）。
- 单步命令往返：< 100ms。
- 变量面板展开 100 个字段：< 300ms。

---

### 阶段 4：搜索与 Git

**任务**

- **文件搜索**：调用 `rg --files`，实现模糊匹配（用 `fuzzy-matcher` crate），快捷键 Ctrl+P。
- **内容搜索**：调用 `rg --json`，实现结果高亮、点击跳转，快捷键 Ctrl+Shift+F。
- **Git 状态**：`git status --porcelain=v2`，显示文件修改状态（M/A/D/??）。
- **Git 历史**：`git log --pretty=format:...`，显示提交图、作者、时间、消息。
- **Git 操作**：commit、stage、unstage、discard、branch 切换、pull、push。
- **Git diff**：调用 `git diff` 或 `git diff --cached`，以并排或统一视图显示。

**验收标准**

- Ctrl+P 输入文件名，100ms 内出结果，回车打开文件。
- Ctrl+Shift+F 输入关键字，结果按文件分组，点击跳转到行。
- Git 面板显示当前分支、修改文件列表、可提交。
- 历史视图显示最近 100 条提交，可查看 diff。

**测试守护用例**

- 单元测试：ripgrep JSON 输出解析，处理多行匹配、二进制文件跳过。
- 单元测试：git porcelain 输出解析，处理重命名、冲突状态。
- 集成测试：在 `testdata/git_repo` 中执行 stage → commit，验证 `git log` 出现新提交。
- 守护用例：非 Git 目录、ripgrep 未安装、git 命令失败三种情况下，IDE 显示提示，不崩溃。

**性能指标自查**

- Ctrl+P 在 10 万文件项目中首次搜索：< 500ms。
- Ctrl+Shift+F 在 10 万文件项目中搜索：< 2s。
- Git 状态刷新：< 200ms。

---

### 阶段 5：UI 打磨与体验对标

**任务**

- 实现 VSCode Dark+ 主题：侧边栏 `#252526`、活动栏 `#333333`、编辑区 `#1e1e1e`、状态栏 `#007acc`、行号 `#858585`、选中行 `#2a2d2e`。
- 布局：左侧活动栏 + 侧边栏 + 编辑区 + 底部面板 + 状态栏。
- 字体：Consolas / Cascadia Code，字号 14，行高 1.5。
- 图标：用 `egui` 内置图标或 `lucide-icons`。
- 动画：面板折叠、命令面板弹出，200ms 缓动。
- 命令面板：Ctrl+Shift+P，模糊搜索所有命令。
- 设置界面：JSON 配置文件，热重载。
- 多标签页、分屏、拖拽。

**验收标准**

- 截图与 VSCode Dark+ 并排对比，配色误差 < 5%。
- 所有快捷键可用，无冲突。
- 命令面板覆盖 50+ 命令。

**测试守护用例**

- 单元测试：主题色值与 VSCode 官方 JSON 一致。
- 集成测试：模拟 100 次快捷键操作，无 panic。
- 视觉回归测试：截图对比基准图（用 `image-diff`）。

**性能指标自查**

- 命令面板打开：< 50ms。
- 主题切换：< 100ms。
- 空闲内存：< 300MB（含所有子系统）。

---

### 阶段 6：打包、分发与文档

**任务**

- Windows 端：生成 `.exe` + 安装包（Inno Setup 或 WiX）。
- WSL 端：提供一键安装脚本，安装 clangd、gdb、ripgrep、git。
- 文档：安装指南、快捷键速查、配置说明、故障排查。
- 自动更新：可选，用 `self_update` crate。

**验收标准**

- 全新 Windows + WSL 环境，按文档 10 分钟内跑通 Hello World 项目。
- 安装包 < 30MB。
- 文档覆盖所有功能。

**测试守护用例**

- 端到端测试：Docker 模拟干净 WSL 环境，执行安装脚本，运行 IDE，打开示例项目，调试通过。
- 守护用例：WSL 未安装、clangd 未安装、gdb 未安装三种情况下，IDE 给出明确提示和修复建议。

**性能指标自查**

- 安装包体积：< 30MB。
- 首次启动（含 WSL 工具链检测）：< 3s。
- 冷启动（已配置）：< 1.5s。

---

## 五、每个阶段的输出格式要求

AI 每完成一个阶段，必须按以下格式输出：

```
## 阶段 N 完成报告

### 交付物
- 文件列表（路径 + 一句话说明）
- 关键代码片段（不超过 200 行）

### 验收结果
- [ ] 验收项 1：实测数据
- [ ] 验收项 2：实测数据
- ...

### 测试用例
- 单元测试：N 个，全绿
- 集成测试：N 个，全绿
- 守护用例：N 个，全绿
- 覆盖率：XX%（用 tarpaulin 或 llvm-cov）

### 性能自查
| 指标 | 目标 | 实测 | 结论 |
|------|------|------|------|
| ... | ... | ... | 达标/不达标 |

### 已知问题
- ...

### 下一阶段计划
- ...
```

---

## 六、质量红线（不达标必须返工）

1. **不允许 panic 逃逸到用户界面**：所有 `unwrap`/`expect` 必须在测试代码或已证明不可能失败的场景。
2. **不允许阻塞主线程**：所有 IO、子进程通信必须在 tokio 任务中。
3. **不允许内存泄漏**：每阶段结束用 `valgrind` 或 `heaptrack` 检查。
4. **不允许未处理的外部进程崩溃**：clangd、gdb、rg、git 崩溃时 IDE 必须存活。
5. **不允许跳过测试**：每个公开函数至少一个单元测试。
6. **提供 `scripts/setup-wsl.sh`**：一键安装/校验所有 WSL 侧工具链，幂等可重跑，输出环境自检报告。

---

## 七、协作规则

1. **先规划后编码**：AI 收到本提示词后，首先输出完整的阶段规划（含每阶段预估工作量、依赖关系、风险点），等用户确认后再开始阶段 0。
2. **一次一阶段**：每个阶段完成后停下来，等用户验收确认，再进入下一阶段。
3. **遇到阻塞主动提问**：技术选型、API 设计有歧义时，列出 2~3 个方案让用户选，不要擅自决定。
4. **代码必须可运行**：不允许出现"伪代码"、"TODO 占位"、"自行补充"。
5. **性能数据必须实测**：用 `hyperfine`、`cargo bench`、`/usr/bin/time -v` 等工具，不允许估算。

---

## 八、最终交付目标

一个可执行文件 `liteide.exe`，在 Windows + WSL Ubuntu 24.04 环境下：

- 打开 C++ 项目，语法高亮正确。
- F12 跳转定义，Alt+Left 返回。
- F5 启动调试，断点、单步、变量查看、条件断点正常。
- Ctrl+P 文件搜索，Ctrl+Shift+F 内容搜索。
- Git 面板显示状态、历史、可提交。
- 界面与 VSCode Dark+ 高度一致。
- 冷启动 < 1.5s，空闲内存 < 300MB，二进制 < 15MB。

**达成以上目标，即视为项目成功。**

---

## 九、启动指令

请 AI 收到本提示词后，按以下步骤响应：

1. 复述项目目标和核心约束（确认理解无误）。
2. 输出完整的阶段规划表（阶段、任务、预估工作量、依赖、风险）。
3. 列出你需要的用户输入（如：GUI 框架最终选 egui 还是 winit？是否需要多光标？）。
4. 等待用户确认后，开始阶段 0。

**现在，请开始。**