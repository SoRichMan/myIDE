; LiteIDE Inno Setup 脚本（阶段 6）。
; 编译：iscc liteide.iss （Inno Setup 6）。
; 产物：installer\Output\LiteIDE-Setup.exe（< 30MB）。

#define AppName "LiteIDE"
#define AppVersion "0.1.0"
#define AppPublisher "LiteIDE"
; 相对本 .iss 文件的发布产物根：先跑 scripts/build_release.ps1 把 exe/脚本/文档放好
#define SourceRoot "..\dist"

[Setup]
AppId={{8F3A2C11-5B7E-4D9A-A1C2-LITEIDE0001}
AppName={#AppName}
AppVersionName={#AppName} {#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher={#AppPublisher}
DefaultDirName={autopf}\LiteIDE
DefaultGroupName=LiteIDE
DisableProgramGroupPage=yes
OutputDir=Output
OutputBaseFilename=LiteIDE-Setup-{#AppVersion}
Compression=lzma2
SolidCompression=yes
ArchitecturesInstallIn64BitMode=x64
PrivilegesRequired=admin
UninstallDisplayIcon={app}\liteide.exe
WizardStyle=modern

[Languages]
Name: "chinesesimp"; MessagesFile: "compiler:Languages\ChineseSimplified.isl"

[Tasks]
Name: "desktopicon"; Description: "创建桌面快捷图标"; GroupDescription: "附加图标:"

[Files]
; 主程序（release 产物，9.48MB）
Source: "{#SourceRoot}\liteide.exe"; DestDir: "{app}"; Flags: ignoreversion
; WSL/Windows 一键脚本
Source: "{#SourceRoot}\scripts\setup-wsl.sh"; DestDir: "{app}\scripts"; Flags: ignoreversion
Source: "{#SourceRoot}\scripts\setup-windows.ps1"; DestDir: "{app}\scripts"; Flags: ignoreversion
; 文档
Source: "{#SourceRoot}\docs\*.md"; DestDir: "{app}\docs"; Flags: ignoreversion
Source: "{#SourceRoot}\README.md"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\LiteIDE"; Filename: "{app}\liteide.exe"
Name: "{group}\卸载 LiteIDE"; Filename: "{uninstallexe}"
Name: "{commondesktop}\LiteIDE"; Filename: "{app}\liteide.exe"; Tasks: desktopicon

[Run]
; 安装完成后可选地拉起 WSL 工具链准备（不阻塞，用户可取消）
Filename: "powershell.exe"; \
    Parameters: "-ExecutionPolicy Bypass -File ""{app}\scripts\setup-windows.ps1"""; \
    Description: "立即准备 WSL C++ 工具链（clangd/gdb/rg/git）"; \
    Flags: postinstall shellexec skipifsourcedoesntexist; \
    StatusMsg: "正在准备 WSL 工具链..."
Filename: "{app}\liteide.exe"; Description: "立即启动 LiteIDE"; Flags: nowait postinstall skipifsourcedoesntexist
