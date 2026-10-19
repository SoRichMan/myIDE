# build_debug_target.ps1 — 编译 testdata/debug_target.cpp（WSL g++，-g -O0）
# 产物：testdata/debug_target（ELF，供 gdb 调试）
$ErrorActionPreference = "Stop"

$distro = if ($env:LITEIDE_WSL_DISTRO) { $env:LITEIDE_WSL_DISTRO } else { "Ubuntu2404" }
$root = Split-Path -Parent $PSScriptRoot
$src = "testdata/debug_target.cpp"
$out = "testdata/debug_target"

Write-Host "编译 $src -> $out (distro=$distro) ..."
wsl.exe -d $distro -- g++ -g -O0 -fno-omit-frame-pointer "$src" -o "$out"
if ($LASTEXITCODE -ne 0) {
    Write-Error "g++ 编译失败 (exit $LASTEXITCODE)"
}

Write-Host "验证产物:"
wsl.exe -d $distro -- file "$out"
wsl.exe -d $distro -- ls -l "$out"
