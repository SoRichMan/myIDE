<#
.SYNOPSIS
    LiteIDE Windows 侧一键环境准备（调用 WSL 内的 setup-wsl.sh）。

.DESCRIPTION
    幂等。检查 wsl.exe 是否可用、是否有发行版，然后把同目录的 setup-wsl.sh
    复制进默认发行版并执行，最后打印工具链自检报告。重复运行不报错。

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File scripts\setup-windows.ps1
#>

$ErrorActionPreference = 'Stop'
$here = Split-Path -Parent $MyInvocation.MyCommand.Definition
$sh = Join-Path $here 'setup-wsl.sh'

Write-Host '== LiteIDE Windows bootstrap ==' -ForegroundColor Cyan

if (-not (Get-Command wsl.exe -ErrorAction SilentlyContinue)) {
    Write-Host '[FAIL] 未找到 wsl.exe。请先在管理员 PowerShell 运行：wsl --install -d Ubuntu-24.04' -ForegroundColor Red
    exit 1
}

# 列出发行版（wsl -l -q 输出是 UTF-16LE，用 PowerShell 直接读会带乱码字符，这里只判空）
$distros = (& wsl.exe -l -q) 2>$null
if (-not $distros -or ($distros -join '') -match '^\s*$') {
    Write-Host '[FAIL] 没有任何 WSL 发行版。请运行：wsl --install -d Ubuntu-24.04，完成后重启电脑。' -ForegroundColor Red
    exit 1
}
Write-Host ("检测到 WSL 发行版: " + (($distros | ForEach-Object { $_.Trim() }) -join ', '))

if (-not (Test-Path $sh)) {
    Write-Host "[FAIL] 未找到 $sh（setup-wsl.sh 应与本脚本同目录）" -ForegroundColor Red
    exit 1
}

# 选一个发行版：环境变量优先，否则第一个非空行
$distro = $env:LITEIDE_WSL_DISTRO
if ([string]::IsNullOrWhiteSpace($distro)) {
    $distro = ($distros | ForEach-Object { $_.Trim().TrimEnd("`0") } | Where-Object { $_ } | Select-Object -First 1)
}
Write-Host "使用发行版: $distro" -ForegroundColor Green

# 把脚本带进 WSL：用 stdin 重定向，避免路径挂载/换行问题
Write-Host '== 在 WSL 内安装/校验工具链 ==' -ForegroundColor Cyan
Get-Content -Raw -Encoding UTF8 $sh | & wsl.exe -d $distro -- bash -s
if ($LASTEXITCODE -ne 0) {
    Write-Host "[FAIL] WSL 工具链安装失败 (exit $LASTEXITCODE)。请看上方 apt 报错。" -ForegroundColor Red
    exit 1
}

Write-Host ''
Write-Host '== 完成。现在可以启动 LiteIDE.exe ==' -ForegroundColor Green
