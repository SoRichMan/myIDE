# LiteIDE 故障排查

状态栏 `LSP:` / `DBG:` 后的颜色与文字就是第一手诊断信息：
绿色 = 正常，灰色 = 进行中，红色 = 出错（括号里是原因，旁边有 `[ Retry ]` 按钮）。

## 启动 / WSL 相关

### `LSP: unavailable (未找到 WSL 发行版…)`
原因：Windows 侧没有任何 WSL 发行版，或 `wsl.exe -l -q` 返回空。
修复（管理员 PowerShell）：
```powershell
wsl --install -d Ubuntu-24.04
# 装完重启电脑，再启动 LiteIDE
```
如果 WSL 装过但命令卡住：通常是残留的 `wsl.exe` 桥进程占用互斥锁，任务管理器结束
所有 `wsl.exe`，或重启一次即可。探测有 10 秒超时兜底，不会无限转圈。

### `LSP: unavailable (WSL 中未找到 clangd…)`
原因：发行版里没装 clangd。修复：
```powershell
powershell -ExecutionPolicy Bypass -File scripts\setup-windows.ps1
```
或手动进 WSL：`sudo apt install clangd`。

### `DBG: unavailable (WSL 中未找到 gdb…)`
原因同上。修复：`sudo apt install gdb`（或跑上面的一键脚本）。

### `LSP: CRASHED (…)`
clangd 进程退出了。点状态栏旁的 `[ Restart clangd ]` 即可重启 IDE 不会崩。
频繁崩溃多半是 `compile_commands.json` 缺失或错误：确认项目根有它，且 `cmake` 配置时
带了 `-DCMAKE_EXPORT_COMPILE_COMMANDS=ON`。

## 调试相关

### 按 F5 后 `无法加载程序 …`
原因：目标 `.exe` 还没编译，或路径里带空格/盘符映射不对。
- 在 WSL 里先 `cmake --build build`。
- Windows 路径 `E:\hello\build\hello` 会被自动转成 `/mnt/e/hello/build/hello`。

### 断点打不住（程序直接跑到底）
- 确认编译时带 `-g -O0`（CMake 项目默认 Debug 带 `-g`）。
- 确认断点所在文件路径与 gdb 加载的源文件路径一致（用 WSL 路径打开）。

### 变量面板是空的
只有程序在断点处停下时变量才会刷新；按 F10 单步一次后看 DEBUG 面板。

## 搜索 / Git

### Ctrl+P 找不到文件
Ctrl+P 是文件系统浏览器：敲完整路径（如 `D:\codes\foo`）回车即进入该目录或打开文件；只敲名字则按当前目录**过滤**。想跳到别处先用 `上一层` 或直接敲盘符路径。文件树按 Ctrl+Shift+P 执行 `Search: Refresh File List` 可刷新资源管理器。

### Ctrl+Shift+F 无结果
搜索跑在 WSL 内的 rg 上，要等它跑完（首次冷启动可能几秒）结果才会出现；确认项目根在 Windows 盘符下（`E:\…`），WSL 通过 `/mnt/e` 访问，且 WSL 里装了 ripgrep（`rg --version`）。

### Git 面板空 / 报错
当前文件夹不是 git 仓库时正常显示空。`git status` 失败多半是 WSL 里没装 git：
`sudo apt install git`。

## 性能

| 现象 | 处理 |
|---|---|
| 空闲内存 > 300MB | 关掉大文件夹的资源管理器重开一次；确认没设 `LITEIDE_NO_CJK`。 |
| 首次打开大文件夹卡 | clangd 在建索引，状态栏 LSP 变绿后就流畅了。 |
| 启动慢 | 首次启动要探测 WSL（≤3s）；已配置后冷启动 < 1.5s。 |

## 还要别的帮助

日志在 stdout（GUI 启动看不到时，从 PowerShell 跑 `.\liteide.exe` 即可看到 tracing 输出）。
带上状态栏截图 + `wsl -l -q` 输出提 issue。
