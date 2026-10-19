# 架构概览

## 分层

```
┌─────────────────────────────────────────┐
│  ide-ui (eframe/egui)                   │  ← 事件循环、布局、绘制
├─────────────────────────────────────────┤
│  ide-core (主题/错误/领域模型)           │  ← 无 GUI 依赖，纯数据
├─────────────────────────────────────────┤
│  ide-lsp / ide-dap / ide-search / ide-git│  ← 子进程 + JSON-RPC/文本协议
└─────────────────────────────────────────┘
            ▲ stdio / TCP
            │
   WSL2 Ubuntu-24.04: clangd, gdb, rg, git
```

## 关键约束

1. **不阻塞 UI 线程**：所有子进程 IO 在 tokio 任务里，通过 channel 把结果回传 UI。
2. **子进程崩溃不拖死 IDE**：clangd/gdb/rg/git 挂了，UI 只显示错误条，可重启。
3. **Windows 前端 / WSL 工具链**：UI 跑在 Windows，工具链通过 `wsl.exe -d Ubuntu-24.04 ...` 调用。
4. **资源占用**：release profile 开 LTO + `codegen-units=1` + `strip=true` + `panic=abort`。

## Crate 边界

- **ide-core**：不依赖 eframe。主题色、错误类型、未来的文档模型。
- **ide-ui**：唯一的 binary crate（`liteide`），依赖 ide-core 和 eframe。
- **ide-lsp / ide-dap / ide-search / ide-git**：都是纯 Rust 库，各自封装一种外部工具，可独立单测。

## 阶段路线

| 阶段 | 内容 | 状态 |
|---|---|---|
| 0 | 脚手架、主题、空窗口、CI 脚本 | 收尾中 |
| 1 | rope 缓冲、tree-sitter 高亮、编辑器交互 | 未开始 |
| 2 | LSP 客户端、clangd 集成、跳转/hover | 未开始 |
| 3 | gdb MI 调试、断点/单步/变量 | 未开始 |
| 4 | ripgrep 搜索、Git 面板 | 未开始 |
| 5 | UI 打磨、命令面板、分屏 | 未开始 |
| 6 | 打包、WSL 一键脚本、文档 | 未开始 |
