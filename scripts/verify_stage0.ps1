# scripts/verify_stage0.ps1 — 阶段0 验收 + 性能实测
Set-Location E:\TestDeepSeek
$ErrorActionPreference = 'Continue'

function Invoke-Native {
    param([scriptblock]$Block)
    & $Block 2>&1 | ForEach-Object {
        if ($_ -is [System.Management.Automation.ErrorRecord]) { $_.Exception.Message } else { $_ }
    }
    return $LASTEXITCODE
}

Write-Host '== 1/5 release build (warning 检查) ==' -ForegroundColor Cyan
$buildOut = (& cargo build --release 2>&1 | Out-String)
$buildOut
$warnLines = ($buildOut -split "`n") | Where-Object { $_ -match 'warning' -and $_ -notmatch 'downloading|Updating' }
if ($warnLines) {
    Write-Warning ("发现 warning:`n" + ($warnLines -join "`n"))
} else {
    Write-Host '无 warning' -ForegroundColor Green
}
if ($LASTEXITCODE -ne 0) { throw "cargo build 失败 exit=$LASTEXITCODE" }

Write-Host '== 2/5 cargo test ==' -ForegroundColor Cyan
& cargo test --workspace --no-fail-fast 2>&1 | Out-Host
if ($LASTEXITCODE -ne 0) { throw 'cargo test 失败' }

Write-Host '== 3/5 cargo clippy ==' -ForegroundColor Cyan
& cargo clippy --workspace --all-targets -- -D warnings 2>&1 | Out-Host
if ($LASTEXITCODE -ne 0) { throw 'clippy 不通过' }

Write-Host '== 4/5 二进制体积 ==' -ForegroundColor Cyan
$exe = 'target\release\liteide.exe'
if (-not (Test-Path $exe)) { throw "未找到 $exe" }
$mb = [math]::Round((Get-Item $exe).Length / 1MB, 2)
Write-Host ("liteide.exe = {0} MB (目标 < 15 MB)" -f $mb)

Write-Host '== 5/5 冷启动 & 空闲内存 ==' -ForegroundColor Cyan
$sw = [System.Diagnostics.Stopwatch]::StartNew()
$p = Start-Process -FilePath $exe -PassThru
while ($p.MainWindowHandle -eq 0 -and -not $p.HasExited) { Start-Sleep -Milliseconds 10 }
$sw.Stop()
$launchMs = $sw.ElapsedMilliseconds
if ($p.HasExited) { throw "进程启动后立即退出，exit=$($p.ExitCode)" }
Write-Host ("冷启动(至窗口可见): {0} ms (目标 < 800 ms)" -f $launchMs)

Start-Sleep -Seconds 3
$p.Refresh()
$memMB = [math]::Round($p.WorkingSet64 / 1MB, 2)
Write-Host ("空闲内存(WorkingSet, 3s): {0} MB (目标 < 80 MB)" -f $memMB)

Stop-Process -Id $p.Id -Force
Write-Host ''
Write-Host '阶段0 验收完成' -ForegroundColor Green
Write-Host ("体积 {0} MB | 冷启动 {1} ms | 空闲 {2} MB" -f $mb, $launchMs, $memMB)
