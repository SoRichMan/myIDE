# 架构概览

## 分层

```
┌──────────────────────────────────────────────┐
│  ide-ui (eframe/egui)                        │  ← 事件循环、布局、绘制
├──────────────────────────────────────────────┤
│  ide-ext (扩展宿主: 清单/贡献点/注册表)        │  ← 无 GUI 依赖，只做注册与查询
│    ▲                        ▲                │
│  ext-cpp-highlight      ext-theme-vscode     │  ← 内置扩展（语法高亮 / token 配色）
├──────────────────────────────────────────────┤
│  ide-core (外壳主题/错误/领域模型/token 词汇表) │  ← 无 GUI 依赖，纯数据
├──────────────────────────────────────────────┤
│  ide-lsp / ide-dap / ide-search / ide-git    │  ← 子进程 + JSON-RPC/文本协议
└──────────────────────────────────────────────┘
            ▲ stdio / TCP
            │
   WSL2 Ubuntu2404: clangd, gdb, rg, git
```

## 关键约束

1. **不阻塞 UI 线程**：所有子进程 IO 在 tokio 任务里，通过 channel 把结果回传 UI。
2. **子进程崩溃不拖死 IDE**：clangd/gdb/rg/git 挂了，UI 只显示错误条，可重启。
3. **Windows 前端 / WSL 工具链**：UI 跑在 Windows，工具链通过 `wsl.exe -d Ubuntu2404 ...` 调用。
4. **资源占用**：release profile 开 LTO + `codegen-units=1` + `strip=true` + `panic=abort`。
5. **扩展只能"注册"，不能"拖慢"**：激活期禁止 IO/重计算（宿主动态计时 + 测试上限），
   逐帧绘制路径不新增分配（配色在切换主题时展开成定长数组，高亮走增量解析）。

## Crate 边界

- **ide-core**：不依赖 eframe。外壳主题色板（`ThemeKind`/`palette`）、token 分组词汇表
  （`HighlightGroup`）、错误类型、文本缓冲。**不认识任何具体语言**。
- **ide-ext**：进程内扩展宿主。`Manifest` + `Capability` + `Registry` + `ExtContext` +
  `ExtensionHost` + `TokenPalette` + `LanguageHighlighter`。不依赖 eframe，也不依赖具体语法。
- **ext-cpp-highlight / ext-theme-vscode**：内置扩展（走和其它扩展完全相同的机制）：
  前者贡献 C/C++ 高亮器（tree-sitter-cpp + 自制查询），后者贡献三套 token 配色。
- **ide-ui**：唯一的 binary crate（`liteide`）。启动时激活内置扩展，之后只通过
  `Registry` 拿"这个文件用什么高亮器""这个主题用哪套配色"。
- **ide-lsp / ide-dap / ide-search / ide-git**：都是纯 Rust 库，各自封装一种外部工具，可独立单测。

新增扩展的完整步骤见 [extensions.md](extensions.md)（含性能契约与测试清单）。

## 阶段路线

| 阶段 | 内容 | 状态 |
|---|---|---|
| 0 | 脚手架、主题、空窗口、CI 脚本 | ✅ |
| 1 | rope 缓冲、tree-sitter 高亮、编辑器交互 | ✅ |
| 2 | LSP 客户端、clangd 集成、跳转/hover | ✅ |
| 3 | gdb MI 调试、断点/单步/变量 | ✅ |
| 4 | ripgrep 搜索、Git 面板 | ✅ |
| 5 | UI 打磨、命令面板、分屏 | ✅ |
| 6 | 打包、WSL 一键脚本、文档 | ✅ |
| 后续 | UI 复核 1-7 轮（见 `PROGRESS.md`）；第八轮：扩展机制 + VSCode C/C++ 配色 | ✅ |
