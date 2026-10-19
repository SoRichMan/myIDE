# LiteIDE 安装指南

LiteIDE 是一个 Windows 原生、面向 WSL2(Ubuntu 24.04) C++ 开发的轻量 IDE。
程序本体跑在 Windows，编译器/语言服务器/调试器（clangd、gdb、rg、git）跑在 WSL 里。

## 前置要求

- Windows 10 22H2 / Windows 11（64 位）
- 已启用 WSL2 并安装 Ubuntu 24.04 发行版
- 不需要装 Visual Studio、不需要装 mingw

> 还没装 WSL？在**管理员 PowerShell** 执行：
> ```powershell
> wsl --install -d Ubuntu-24.04
> ```
> 完成后重启电脑，首次进入 Ubuntu 设置一个用户名密码。

## 方式一：安装包（推荐）

1. 运行 `LiteIDE-Setup-0.1.0.exe`，按提示安装到任意目录。
2. 安装结束的最后一页，勾选 **"立即准备 WSL C++ 工具链"**，它会自动在 WSL 里装好
   clangd / gdb / ripgrep / git / cmake / ninja / bear。
3. 启动 LiteIDE。

## 方式二：绿色便携版

解压 `LiteIDE-0.1.0-portable.zip` 到任意目录，然后在 PowerShell 里跑一次一键脚本：

```powershell
cd 解压目录
powershell -ExecutionPolicy Bypass -File scripts\setup-windows.ps1
.\liteide.exe
```

`setup-windows.ps1` 会检测 WSL、把 `scripts\setup-wsl.sh` 送进默认发行版执行，并打印
每个工具的路径与版本号。**幂等，可重复运行**。

## 10 分钟跑通 Hello World

```powershell
# 1) 建一个最小 C++ 项目（在 Windows 侧，WSL 里通过 /mnt/e 访问）
mkdir E:\hello && cd E:\hello
```

`E:\hello\CMakeLists.txt`:
```cmake
cmake_minimum_required(VERSION 3.20)
project(hello CXX)
set(CMAKE_EXPORT_COMPILE_COMMANDS ON)
add_executable(hello main.cpp)
```

`E:\hello\main.cpp`:
```cpp
#include <iostream>
int main() {
    int a = 3, b = 4;
    std::cout << "sum = " << a + b << std::endl;  // 断点打这一行
    return 0;
}
```

```powershell
# 2) 在 WSL 里配置 + 编译（生成 compile_commands.json，clangd 靠它跳转）
wsl -d Ubuntu2404 -- bash -lc "cd /mnt/e/hello && cmake -S . -B build -G Ninja && cmake --build build"
```

3. 用 LiteIDE 打开 `E:\hello` 文件夹：
   - 状态栏 `LSP: Ready (clangd)` 变绿 → Ctrl+P 输入 `main.cpp` 打开。
   - 光标停在 `a + b` 上按 **F12** 可跳转定义；**Alt+Left** 返回。
4. 在 `std::cout` 那一行按 **F9** 下断点，按 **F5** 开始调试：
   - DEBUG 面板看到程序在断点处停下，变量 `a=3, b=4`。
   - **F10** 单步跳过，**F11** 单步进入，**Shift+F11** 跳出，**Shift+F5** 停止。

## 环境变量（高级，一般不用设）

| 变量 | 作用 |
|---|---|
| `LITEIDE_WSL_DISTRO` | 指定 WSL 发行版名（默认自动探测） |
| `LITEIDE_LSP_CMD` | 整条覆盖 clangd 启动命令（调试/测试用） |
| `LITEIDE_GDB_CMD` | 整条覆盖 gdb 启动命令 |
| `LITEIDE_RG_CMD` / `LITEIDE_GIT_CMD` | 覆盖 rg / git 命令 |
| `LITEIDE_CONFIG_DIR` | 覆盖配置目录（默认 `%APPDATA%\LiteIDE`） |
