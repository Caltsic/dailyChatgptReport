# Trace Trae IDE local AI data on this Windows machine without uploading anything.
#
# Trae has two local storage styles:
#  - International builds keep chat data in plaintext VS Code-style `state.vscdb` files
#    (per workspace, under `%APPDATA%\Trae\User\workspaceStorage\<id>\`).
#  - Trae CN keeps AI agent data in a SQLCipher-encrypted
#    `%APPDATA%\Trae CN\ModularData\ai-agent\database.db`.
# This probe copies the relevant small files into one local folder so the format can be
# inspected and parsed. It never sends data anywhere; it only reads and copies files, and
# it never needs Trae to be closed (run it while Trae is idle for a consistent snapshot).
#
# Usage:
#   pwsh -NoProfile -File .\scripts\probe-trae.ps1
#   pwsh -NoProfile -File .\scripts\probe-trae.ps1 -Output D:\trae-probe
#   pwsh -NoProfile -File .\scripts\probe-trae.ps1 -IncludeStateDb      # plus global VS Code state
#
# Then export one conversation from Trae (Trae CN supports "导出历史对话"; the international
# build has an export action in the chat panel) and put the exported file in the same folder.
param(
    [string]$Output = (Join-Path (Split-Path -Parent $PSScriptRoot) 'artifacts\trae-probe'),
    # Also copy global workspace state (may contain login tokens); workspace-scoped
    # state.vscdb files with chat history are copied by default.
    [switch]$IncludeStateDb,
    # Additional data roots (for a non-default install or for testing the probe itself).
    [string[]]$ExtraRoots = @()
)
$ErrorActionPreference = 'Stop'
New-Item -ItemType Directory -Force -Path $Output | Out-Null

Write-Host '=== Trae processes ==='
Get-Process -ErrorAction SilentlyContinue | Where-Object { $_.ProcessName -like '*Trae*' } |
    Select-Object ProcessName, Id, Path | Format-Table -AutoSize | Out-String | Write-Host

Write-Host '=== installed copies ==='
$uninstallRoots = @(
    'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\*',
    'HKLM:\Software\Microsoft\Windows\CurrentVersion\Uninstall\*',
    'HKLM:\Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\*'
)
$installed = foreach ($key in $uninstallRoots) {
    Get-ItemProperty $key -ErrorAction SilentlyContinue | Where-Object { $_.DisplayName -like '*Trae*' } |
        Select-Object DisplayName, DisplayVersion, InstallLocation
}
$installed | Format-Table -AutoSize | Out-String | Write-Host

$candidates = @(
    (Join-Path $env:APPDATA 'Trae'),
    (Join-Path $env:APPDATA 'Trae\User\workspaceStorage'),
    (Join-Path $env:APPDATA 'Trae CN'),
    (Join-Path $env:APPDATA 'Trae CN\ModularData'),
    (Join-Path $env:APPDATA 'TraeCN'),
    (Join-Path $env:LOCALAPPDATA 'Trae'),
    (Join-Path $env:LOCALAPPDATA 'Trae CN'),
    (Join-Path $env:LOCALAPPDATA 'Programs\Trae'),
    (Join-Path $env:LOCALAPPDATA 'Programs\Trae CN'),
    (Join-Path $env:USERPROFILE '.trae'),
    (Join-Path $env:USERPROFILE '.trae-cn'),
    (Join-Path $env:USERPROFILE '.trae-aicc')
) + $ExtraRoots | Where-Object { $_ -and (Test-Path -LiteralPath $_) } | Select-Object -Unique

if (-not $candidates) {
    Write-Warning '没有找到 Trae 数据目录。请在安装了 Trae 的电脑上运行本脚本，或手动提供 Trae 数据目录。'
    exit 1
}

# Plain SQLite starts with "SQLite format 3\0"; an encrypted SQLCipher database has a
# random-looking header. This classification tells the maintainer whether a plain parser
# is possible before any decryption work.
function Get-DbKind([System.IO.FileInfo]$File) {
    try {
        if ($File.Extension -eq '.json') {
            return @{ kind = 'json-text'; header = '' }
        }
        $stream = [System.IO.File]::OpenRead($File.FullName)
        try {
            $buffer = New-Object byte[] 16
            $read = $stream.Read($buffer, 0, 16)
        } finally { $stream.Dispose() }
        $header = ($buffer[0..([Math]::Max(0, $read - 1))] | ForEach-Object { $_.ToString('X2') }) -join ' '
        $ascii = -join ($buffer[0..([Math]::Max(0, $read - 1))] | ForEach-Object { if ($_ -ge 32 -and $_ -lt 127) { [char]$_ } else { '.' } })
        if ($read -ge 16 -and $ascii.StartsWith('SQLite format 3')) {
            return @{ kind = 'sqlite-plaintext'; header = $header }
        }
        return @{ kind = 'encrypted-or-unknown'; header = $header }
    } catch {
        return @{ kind = 'unreadable'; header = '' }
    }
}

$records = New-Item -ItemType Directory -Force -Path (Join-Path $Output 'files')
$seen = New-Object 'System.Collections.Generic.HashSet[string]'
$report = New-Object 'System.Collections.Generic.List[object]'
$copied = 0

foreach ($root in $candidates) {
    Write-Host "scanning: $root"
    $files = @(Get-ChildItem -LiteralPath $root -Recurse -File -Depth 6 -ErrorAction SilentlyContinue |
        Where-Object { $_.Name -in @('database.db', 'state.vscdb', 'storage.json') })
    foreach ($file in $files) {
        if (-not $seen.Add($file.FullName)) { continue }
        $relative = $file.FullName.Substring($root.Length).TrimStart('\')
        $isGlobalState = $file.Name -ne 'database.db' -and ($relative -match '(?i)(^|\\)globalStorage(\\|$)')
        $skipCopy = ($file.Name -eq 'storage.json' -or $isGlobalState) -and -not $IncludeStateDb
        $kind = Get-DbKind $file
        $entry = [ordered]@{
            root = $root
            source = $file.FullName
            relative = $relative
            file = $file.Name
            bytes = $file.Length
            modified = $file.LastWriteTime
            format = $kind.kind
            headerHex = $kind.header
            copied = $null
            copiedCompanions = @()
            skipped = $skipCopy
        }
        if (-not $skipCopy) {
            $safeRoot = ($root -replace '[\\/:*?"<>|]', '_')
            $target = Join-Path (Join-Path $records $safeRoot) $relative
            New-Item -ItemType Directory -Force -Path (Split-Path -Parent $target) | Out-Null
            Copy-Item -LiteralPath $file.FullName -Destination $target -Force
            $entry.copied = $target
            $copied++
            # A SQLite database plus its WAL/SHM siblings is a consistent snapshot.
            foreach ($suffix in @('-wal', '-shm')) {
                $companion = [System.IO.FileInfo]::new($file.FullName + $suffix)
                if ($companion.Exists) {
                    Copy-Item -LiteralPath $companion.FullName -Destination ($target + $suffix) -Force
                    $entry.copiedCompanions += ($target + $suffix)
                }
            }
        }
        $report.Add([pscustomobject]$entry)
    }
}

$report | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath (Join-Path $Output 'trae-probe.json') -Encoding utf8
$summary = [ordered]@{
    generatedAt = (Get-Date).ToString('o')
    copiedMainFiles = $copied
    copiedDatabase = @($report | Where-Object { $_.file -eq 'database.db' -and $_.copied }).Count
    copiedWorkspaceState = @($report | Where-Object { $_.file -eq 'state.vscdb' -and $_.copied }).Count
    skippedForPrivacy = @($report | Where-Object { $_.skipped }).Count
    encryptedDatabases = @($report | Where-Object { $_.format -eq 'encrypted-or-unknown' }).Count
    plaintextDatabases = @($report | Where-Object { $_.format -eq 'sqlite-plaintext' }).Count
}
$summary | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $Output 'summary.json') -Encoding utf8
$summary | Format-Table -AutoSize | Out-String | Write-Host

Write-Host ''
Write-Host "探测结果已写入：$Output"
Write-Host '请把整个目录打包发给维护者；如果方便，再从 Trae 导出一份对话记录（md/json）放进同一目录。'
if (-not $IncludeStateDb) {
    Write-Host '工作区的 state.vscdb（国际版聊天记录）已复制；全局 state.vscdb/storage.json 可能含登录令牌，默认未复制。'
}
