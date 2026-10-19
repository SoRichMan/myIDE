# LiteIDE: 重新生成 testdata/sample_project/compile_commands.json（在 WSL 中跑 CMake）。
# 用法: powershell -File scripts\gen_compile_commands.ps1
# 依赖: WSL Ubuntu + cmake + ninja（scripts/setup-wsl.sh 可安装）

$ErrorActionPreference = 'Stop'

# 选择发行版：环境变量优先，否则取 wsl -l -q 里第一个 Ubuntu 发行版
$distro = $env:LITEIDE_WSL_DISTRO
if (-not $distro) {
    $raw = & wsl.exe -l -q 2>$null | Out-String
    $line = ($raw -split "`r?`n" | Where-Object { $_ -match 'Ubuntu' } | Select-Object -First 1)
    if (-not $line) { Write-Error '未找到 Ubuntu WSL 发行版，请设置 $env:LITEIDE_WSL_DISTRO' }
    $distro = $line.Trim()
}
Write-Host "Using WSL distro: $distro"

$script = '/mnt/e/TestDeepSeek/scripts/gen_compile_commands.sh'
$out = & wsl.exe -d $distro -- bash $script 2>&1 | Out-String
Write-Host $out
if ($LASTEXITCODE -ne 0) { Write-Error "生成失败 (exit $LASTEXITCODE)" }
