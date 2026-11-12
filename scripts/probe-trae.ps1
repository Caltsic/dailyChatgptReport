# Trace Trae IDE local AI data on this Windows machine without uploading anything.
#
# Trae stores AI conversations in an encrypted SQLCipher database; this probe copies the
# database plus the small VS Code-style state files into a local folder so a maintainer can
# inspect the format and build a parser. It never sends data anywhere and never needs Trae
# to be closed, but run it while Trae is idle to avoid a partially written database.
#
# Usage:
#   pwsh -NoProfile -File .\scripts\probe-trae.ps1
#   pwsh -NoProfile -File .\scripts\probe-trae.ps1 -Output D:\trae-probe -IncludeStateDb
#
# Then export one conversation from Trae (Trae CN supports "导出历史对话"; older builds
# have "导出为 md") and put the exported file in the same folder.
param(
    [string]$Output = (Join-Path (Split-Path -Parent $PSScriptRoot) 'artifacts\trae-probe'),
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
    (Join-Path $env:APPDATA 'Trae CN'),
    (Join-Path $env:APPDATA 'TraeCN'),
    (Join-Path $env:LOCALAPPDATA 'Trae'),
    (Join-Path $env:LOCALAPPDATA 'Trae CN'),
    (Join-Path $env:LOCALAPPDATA 'Programs\Trae'),
    (Join-Path $env:LOCALAPPDATA 'Programs\Trae CN'),
    (Join-Path $env:USERPROFILE '.trae'),
    (Join-Path $env:USERPROFILE '.trae-cn'),
    (Join-Path $env:USERPROFILE '.trae-aicc')
) + $ExtraRoots | Where-Object { $_ -and (Test-Path -LiteralPath $_) }

if (-not $candidates) {
    Write-Warning '没有找到 Trae 数据目录。请在安装了 Trae 的电脑上运行本脚本，或手动提供 Trae 数据目录。'
    exit 1
}

$names = @('database.db')
if ($IncludeStateDb) { $names += @('state.vscdb', 'storage.json') }
$records = New-Item -ItemType Directory -Force -Path (Join-Path $Output 'files')

# Plain SQLite starts with "SQLite format 3\0"; an encrypted SQLCipher database has a
# random-looking header. This classification tells the maintainer whether a plain parser
# is possible before any decryption work.
function Get-DbKind([System.IO.FileInfo]$File) {
    try {
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

Write-Host '=== candidate roots ==='
$report = foreach ($root in $candidates) {
    Write-Host $root
    foreach ($name in $names) {
        Get-ChildItem -LiteralPath $root -Recurse -File -Depth 5 -Filter $name -ErrorAction SilentlyContinue |
            ForEach-Object {
                $relative = $_.FullName.Substring($root.Length).TrimStart('\')
                $safeRoot = ($root -replace '[\\/:*?"<>|]', '_')
                $target = Join-Path (Join-Path $records $safeRoot) $relative
                New-Item -ItemType Directory -Force -Path (Split-Path -Parent $target) | Out-Null
                Copy-Item -LiteralPath $_.FullName -Destination $target -Force
                $kind = Get-DbKind $_
                [pscustomobject]@{
                    root = $root
                    source = $_.FullName
                    file = $name
                    bytes = $_.Length
                    modified = $_.LastWriteTime
                    format = $kind.kind
                    headerHex = $kind.header
                    copied = $target
                }
            }
    }
}

$report | ConvertTo-Json -Depth 3 | Set-Content -LiteralPath (Join-Path $Output 'trae-probe.json') -Encoding utf8
Write-Host ''
Write-Host "探测结果已写入：$Output"
Write-Host '请把整个目录打包发给维护者；如果方便，再从 Trae 导出一份对话记录（md/json）放进同一目录。'
if (-not $IncludeStateDb) {
    Write-Host '默认未复制 state.vscdb（可能包含登录令牌）。如确需排查界面状态，可加 -IncludeStateDb 重跑。'
}
