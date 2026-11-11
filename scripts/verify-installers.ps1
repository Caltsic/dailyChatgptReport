param([switch]$SkipRuntimeChecks)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$testRoot = Join-Path $root 'artifacts\install-test'
$installRoot = Join-Path $testRoot '自选安装 中文 空格'
$results = [ordered]@{}
foreach ($role in @('Member', 'Leader')) {
    $installer = Join-Path $root "artifacts\installers\CodexDailyReport.$role.Setup.exe"
    $installed = Join-Path $installRoot $role
    $arguments = @('--silent', '--test-root', ('"' + $testRoot + '"'), '--install-dir', ('"' + $installed + '"'))
    $process = Start-Process -FilePath $installer -ArgumentList $arguments -WindowStyle Hidden -PassThru
    if (!$process.WaitForExit(120000)) { throw "Installer timed out: $role" }
    if ($process.ExitCode -ne 0) { throw "Installer failed: $role ($($process.ExitCode))" }
    $published = Join-Path $root "artifacts\publish\$role"
    $files = @(Get-ChildItem -LiteralPath $published -File -Recurse)
    $mismatches = @()
    foreach ($file in $files) {
        $relative = [IO.Path]::GetRelativePath($published, $file.FullName)
        $target = Join-Path $installed $relative
        if (!(Test-Path -LiteralPath $target) -or (Get-FileHash -LiteralPath $file.FullName).Hash -ne (Get-FileHash -LiteralPath $target).Hash) {
            $mismatches += $relative
        }
    }
    $unexpected = @(Get-ChildItem -LiteralPath $installed -File -Recurse | Where-Object { [IO.Path]::GetRelativePath($installed, $_.FullName) -ne 'Uninstall.ps1' -and !(Test-Path -LiteralPath (Join-Path $published ([IO.Path]::GetRelativePath($installed, $_.FullName)))) })
    $results[$role] = @{ files = $files.Count; mismatches = $mismatches; unexpectedFiles = $unexpected.Count }
    if ($mismatches.Count -gt 0 -or $unexpected.Count -gt 0) { throw "Installed files differ: $role" }
    Write-Output "$role installed files verified: $($files.Count)"
    if ($SkipRuntimeChecks) {
        $process = Start-Process -FilePath $installer -ArgumentList $arguments -WindowStyle Hidden -PassThru
        if (!$process.WaitForExit(60000) -or $process.ExitCode -ne 0) { throw "Custom-path upgrade failed: $role" }
        $registration = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\CodexDailyReport.$role"
        $registeredBefore = (Get-ItemProperty -LiteralPath $registration -ErrorAction SilentlyContinue).InstallLocation
        & powershell.exe -NoProfile -ExecutionPolicy Bypass -File (Join-Path $installed 'Uninstall.ps1')
        if ($LASTEXITCODE -ne 0 -or (Test-Path -LiteralPath $installed)) { throw "Custom-path uninstall failed: $role" }
        $registeredAfter = (Get-ItemProperty -LiteralPath $registration -ErrorAction SilentlyContinue).InstallLocation
        if ($registeredBefore -ne $registeredAfter) { throw 'Isolated uninstall changed the real installation registration' }
        $results[$role]['customPathUpgradeAndUninstall'] = $true
    }
}
$acceptance = Join-Path $root 'artifacts\acceptance'
if ($SkipRuntimeChecks) {
    $occupied = Join-Path $testRoot '已有文件'
    New-Item -ItemType Directory -Path $occupied -Force | Out-Null
    $marker = Join-Path $occupied '用户文件.txt'
    [IO.File]::WriteAllText($marker, '保留原文件')
    $process = Start-Process -FilePath (Join-Path $root 'artifacts\installers\CodexDailyReport.Member.Setup.exe') -ArgumentList @('--silent', '--test-root', ('"' + $testRoot + '"'), '--install-dir', ('"' + $occupied + '"')) -WindowStyle Hidden -PassThru
    if (!$process.WaitForExit(60000) -or $process.ExitCode -eq 0 -or [IO.File]::ReadAllText($marker) -ne '保留原文件') { throw 'Installer replaced an unrelated directory' }
    $results['unrelatedFilesPreserved'] = $true
    $results | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath (Join-Path $acceptance 'installer-location-verification.json') -Encoding utf8
    Write-Output 'Custom installation locations, upgrades and uninstalls verified.'
    exit 0
}
$forbidden = @(Get-ChildItem -LiteralPath (Join-Path $installRoot 'Member') -Recurse -File | Where-Object { $_.Name -match 'DailyReport.Server|Playwright|SQLite|prompt|secret' })
if ($forbidden.Count -gt 0) { throw 'Member package contains a forbidden leader dependency' }
if (Test-Path -LiteralPath (Join-Path $installRoot 'Member\agent')) { throw 'Member package must not contain an Agent runtime' }
& (Join-Path $root '.tools\agent-venv\Scripts\python.exe') (Join-Path $root 'agent\smoke_test.py') (Join-Path $installRoot 'Leader\agent\report-agent.exe')
if ($LASTEXITCODE -ne 0) { throw 'Installed Agent could not complete the runtime contract' }
$results['packagedAgent'] = @{ generated = $true; resumed = $true }
$html = Join-Path $acceptance 'packaged-browser-smoke.html'
$pdf = Join-Path $acceptance 'packaged-browser-smoke.pdf'
[IO.File]::WriteAllText($html, '<!doctype html><html lang="zh-CN"><meta charset="utf-8"><style>@page{size:A4;margin:25.4mm}body{font:12pt SimSun}h1{font:20pt "Microsoft YaHei"}</style><h1>安装包 PDF 渲染验证</h1><p>中文工作记录、成员补充说明与版本信息。</p></html>')
if (Test-Path -LiteralPath $pdf) { Remove-Item -LiteralPath $pdf }
$browser = Join-Path $installRoot 'Leader\browsers\chromium\chrome.exe'
$profile = Join-Path $acceptance ('browser-profile-' + [guid]::NewGuid().ToString('N'))
$browserArgs = @('--headless', '--disable-gpu', '--no-first-run', '--no-default-browser-check', '--no-pdf-header-footer', ('--user-data-dir="' + $profile + '"'), ('--print-to-pdf="' + $pdf + '"'), ('"' + ([uri]$html).AbsoluteUri + '"'))
$process = Start-Process -FilePath $browser -ArgumentList $browserArgs -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $acceptance 'packaged-browser-smoke.log')
if (!$process.WaitForExit(60000)) { throw 'Packaged Chromium export timed out' }
if ($process.ExitCode -ne 0 -or !(Test-Path -LiteralPath $pdf) -or (Get-Item -LiteralPath $pdf).Length -lt 1000) { throw 'Packaged Chromium PDF export failed' }
$results['packagedBrowser'] = @{ exported = $true; pdfBytes = (Get-Item -LiteralPath $pdf).Length }
$results | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath (Join-Path $acceptance 'installer-verification.json') -Encoding utf8
Write-Output 'Both installers and packaged Chromium verified.'
