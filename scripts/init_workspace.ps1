# 幂等初始化 workspace 骨架
$ErrorActionPreference = ''Stop''
$root = ''E:\TestDeepSeek''
Set-Location $root
foreach ($d in ''crates'',''docs'',''scripts'',''testdata'',''.cargo'') {
    if (-not (Test-Path "$root\$d")) { New-Item -ItemType Directory -Path "$root\$d" | Out-Null }
}
Write-Host ''[init_workspace] done'' -ForegroundColor Green
