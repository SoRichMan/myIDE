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

## 语法高亮 / 主题（扩展）

### 某个文件没有语法高亮
LiteIDE 按**文件后缀**向扩展注册表要高亮器，内置语言目前是 C/C++（`c` `cc` `cpp` `cxx`
`c++` `h` `hh` `hpp` `hxx` `h++` `inl` `ipp` `tpp` `tcc` `m` `mm`；完整列表见
`crates/ext-cpp-highlight/src/lib.rs` 的 `EXTENSIONS`）。后缀不在列表里（例如 `.txt`、
无后缀文件）就是纯文本，这是预期行为。想加语言见 `docs/extensions.md`。

### 颜色和 VS Code 对不上
设置面板（Ctrl+,）里确认主题是 `C/C++ Dark`（= VS Code 的
`Dark (Visual Studio - C/C++)`）；`Dark+` / `Light+` 用的是 VSCode 官方默认主题的 token 色，
本来就和 C/C++ Themes 不一样。主题的 token 色由扩展贡献，外壳色（背景/状态栏）由 IDE 内置
色板决定。

### 想确认扩展有没有加载
设置面板最下方的 **Extensions** 列表会列出每个扩展的贡献点数量与激活耗时（µs 级）；
左侧 `✓` 表示激活成功、`✗` 表示失败（激活失败会被回滚并禁用，只影响它自己）。

### 打字时高亮偶尔"跟不上"
高亮是**按帧懒刷新**的（同一帧多次编辑只重算一次），并且走增量解析。2000 行级文件每次
改动约几毫秒；十万行级超大文件建议先关掉 minimap。性能数字与优化点见
`docs/extensions.md`。

## 中文输入（输入法 / IME）

### 打不出中文、切输入法也没反应
早期版本为了绕开「回车被 Windows 输入法吞掉」把整个窗口的输入法通道关掉了（等于全 App 都打不出中文），
该补丁已撤销。当前版本：

- 编辑器和内置文本框都会自己上报输入法候选窗位置，候选窗**紧贴光标**弹出；
- 编辑器自己处理 `ImeEvent::Commit`（上屏）与 `Preedit`（拼音组合串，正文内联 + 下划线）；
- 非组合态下回车 / 退格 / 方向键不受影响。

若仍打不出中文：确认窗口是激活状态，且任务栏输入法处于**中文**模式（微软拼音的 `中/英`，
或 Shift 切换）—— 英文模式下敲字母当然只会出 ASCII。

### 组合时按回车，上屏的是拼音字母
这是输入法本身的行为（微软拼音「回车 = 提交原始串」）。要选候选字按**空格**或数字键。

### 拼音不显示在正文里，只在系统候选窗里
Windows 上 winit 0.29 只在输入法上报光标位置时转发组合串，微软拼音不上报，所以拼音显示在
系统候选窗（贴光标）里；能上报光标位置的 IME / 平台会正文内联显示并加下划线。

### 光标 / 选区 / 点击位置在中文行上偏了
早期版本把「字符列号」直接当「等宽格子数」乘格宽，而中文用的是回退字体（汉字宽 ≈ 1.65 格），
所以中文行上光标会停在字中间、点选也会偏位。当前版本列 → 像素全部按**真实字形宽度**累加
（点击定位是同一套宽度的反查），中文与 ASCII 混排都对齐。

## 性能

| 现象 | 处理 |
|---|---|
| 空闲内存 > 300MB | 关掉大文件夹的资源管理器重开一次；确认没设 `LITEIDE_NO_CJK`。 |
| 首次打开大文件夹卡 | clangd 在建索引，状态栏 LSP 变绿后就流畅了。 |
| 启动慢 | 首次启动要探测 WSL（≤3s）；已配置后冷启动 < 1.5s。 |

## 还要别的帮助

日志在 stdout（GUI 启动看不到时，从 PowerShell 跑 `.\liteide.exe` 即可看到 tracing 输出）。
带上状态栏截图 + `wsl -l -q` 输出提 issue。
