$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$output = Join-Path $root 'artifacts\installers'
New-Item -ItemType Directory -Force -Path $output | Out-Null
$files = @('.gitignore', '.gitattributes', 'LICENSE', 'README.md', 'VALIDATION.md', 'CONTRIBUTING.md', 'SECURITY.md', 'CODE_OF_CONDUCT.md', 'CHANGELOG.md', 'VERSION', 'Directory.Build.props', 'global.json', 'scripts/build.ps1','scripts/build-tauri.ps1','scripts/build-agent.ps1','scripts/package-source.ps1') | ForEach-Object { Get-Item -LiteralPath (Join-Path $root $_) }
if (Test-Path -LiteralPath (Join-Path $root '.github')) {
    $files += Get-ChildItem -LiteralPath (Join-Path $root '.github') -Recurse -File
}
foreach ($folder in @('desktop', 'prompts', 'agent')) {
    $files += Get-ChildItem -LiteralPath (Join-Path $root $folder) -Recurse -File | Where-Object {
        $relative = [IO.Path]::GetRelativePath($root, $_.FullName)
        $relative -notmatch '(^|[\\/])(bin|obj|node_modules|target|dist|gen|__pycache__|\.cache|\.venv|venv|tokenizer-cache|feedback-conflicts)([\\/]|$)' -and $_.Extension -notin '.enc', '.pyc'
    }
}
$temporary = Join-Path $output ('source-' + [guid]::NewGuid().ToString('N') + '.tmp')
try {
    $archive = [IO.Compression.ZipFile]::Open($temporary, [IO.Compression.ZipArchiveMode]::Create)
    try {
        foreach ($file in $files) {
            $relative = [IO.Path]::GetRelativePath($root, $file.FullName).Replace('\', '/')
            [IO.Compression.ZipFileExtensions]::CreateEntryFromFile($archive, $file.FullName, $relative, [IO.Compression.CompressionLevel]::Optimal) | Out-Null
        }
    } finally { $archive.Dispose() }
    Move-Item -LiteralPath $temporary -Destination (Join-Path $output 'CodexDailyReport.Source.zip') -Force
} finally { if (Test-Path -LiteralPath $temporary) { Remove-Item -LiteralPath $temporary } }
Get-ChildItem -LiteralPath $output -File | Where-Object Extension -In '.exe', '.zip' | Sort-Object Name | ForEach-Object {
    $hash = Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256
    "$($hash.Hash)  $($_.Name)"
} | Set-Content -LiteralPath (Join-Path $output 'SHA256.txt') -Encoding utf8
Write-Output "Source package created: $($files.Count) files."
