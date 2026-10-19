<#
.SYNOPSIS
    阶段 6 打包：构建 release → 组装 dist/ → 便携 ZIP → (可选) Inno Setup 安装包。
#>
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $MyInvocation.MyCommand.Definition | Split-Path -Parent
Set-Location $root
$env:RUSTUP_OFFLINE = 1

Write-Host '== [1/4] cargo build --release ==' -ForegroundColor Cyan
cargo build --release
if ($LASTEXITCODE -ne 0) { throw 'release build failed' }

$exe = Join-Path $root 'target\release\liteide.exe'
$dist = Join-Path $root 'dist'
if (Test-Path $dist) { Remove-Item -Recurse -Force $dist }
New-Item -ItemType Directory -Force -Path "$dist\scripts", "$dist\docs" | Out-Null

Write-Host '== [2/4] 组装 dist/ ==' -ForegroundColor Cyan
Copy-Item $exe "$dist\liteide.exe"
Copy-Item "$root\scripts\setup-wsl.sh" "$dist\scripts\"
Copy-Item "$root\scripts\setup-windows.ps1" "$dist\scripts\"
Copy-Item "$root\docs\*.md" "$dist\docs\"
Copy-Item "$root\README.md" "$dist\"

Write-Host '== [3/4] 便携 ZIP ==' -ForegroundColor Cyan
$zip = Join-Path $root 'dist\LiteIDE-0.1.0-portable.zip'
if (Test-Path $zip) { Remove-Item -Force $zip }
Compress-Archive -Path "$dist\*" -DestinationPath $zip -CompressionLevel Optimal
$zipSize = (Get-Item $zip).Length

Write-Host '== [4/4] Inno Setup（如可用）==' -ForegroundColor Cyan
$iscc = Get-Command iscc.exe -ErrorAction SilentlyContinue
if (-not $iscc) {
    $cand = 'C:\Program Files (x86)\Inno Setup 6\ISCC.exe'
    if (Test-Path $cand) { $iscc = $cand }
}
if ($iscc) {
    & $iscc "$root\installer\liteide.iss"
} else {
    Write-Host 'iscc.exe 未安装：已产出便携 ZIP。安装包请装 Inno Setup 6 后跑 iscc installer\liteide.iss' -Yellow
}

Write-Host ''
Write-Host '== 产物 ==' -ForegroundColor Green
Get-ChildItem $dist -Recurse -File | ForEach-Object {
    '  {0,12:N0} B  {1}' -f $_.Length, $_.FullName.Substring($root.Length+1)
}
'  portable zip: {0:N2} MB' -f ($zipSize/1MB)
