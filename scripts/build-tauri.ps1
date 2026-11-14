param([switch]$RebuildAgent,[switch]$SkipTests)
$ErrorActionPreference='Stop'
$root=Split-Path -Parent $PSScriptRoot
$desktop=Join-Path $root 'desktop'
if($RebuildAgent -or !(Test-Path -LiteralPath (Join-Path $root 'artifacts\agent-runtime\report-agent.exe'))) {
    & (Join-Path $PSScriptRoot 'build-agent.ps1')
}
Push-Location -LiteralPath $desktop
try {
    if(!(Test-Path -LiteralPath 'node_modules')) { & npm.cmd ci; if($LASTEXITCODE){throw 'npm ci failed'} }
    & npm.cmd run build
    if($LASTEXITCODE){throw 'Frontend build failed'}
    if(!$SkipTests) {
        & cargo test --manifest-path src-tauri/Cargo.toml --lib
        if($LASTEXITCODE){throw 'Collector regression check failed'}
    }
    $output=Join-Path $root 'artifacts\tauri-installers'
    New-Item -ItemType Directory -Force -Path $output | Out-Null
    foreach($role in @('Member','Leader')) {
        $options=@('run','tauri','--','build','--bundles','nsis','--config','src-tauri/tauri.build.conf.json')
        if($role -eq 'Leader') {$options+=@('--features','leader','--config','src-tauri/tauri.leader.conf.json')}
        & npm.cmd @options
        if($LASTEXITCODE){throw "Tauri $role build failed"}
        $installer=Get-ChildItem -LiteralPath 'src-tauri\target\release\bundle\nsis' -Filter "*$role*setup.exe" | Sort-Object LastWriteTime -Descending | Select-Object -First 1
        if(!$installer){throw "Missing $role installer"}
        Copy-Item -LiteralPath $installer.FullName -Destination (Join-Path $output "CodexDailyReport.$role.Setup.exe") -Force
        $publish=Join-Path $root "artifacts\tauri-publish\$role"
        New-Item -ItemType Directory -Force -Path $publish | Out-Null
        Copy-Item -LiteralPath "src-tauri\target\release\CodexDailyReport.$role.exe" -Destination $publish -Force
        if($role -eq 'Leader') {
            New-Item -ItemType Directory -Force -Path (Join-Path $publish 'agent') | Out-Null
            Copy-Item -Path (Join-Path $root 'artifacts\agent-runtime\*') -Destination (Join-Path $publish 'agent') -Recurse -Force
        }
    }
    Get-ChildItem -LiteralPath $output -Filter '*.exe' | ForEach-Object { "{0}  {1}" -f (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash,$_.Name } | Set-Content -LiteralPath (Join-Path $output 'SHA256.txt') -Encoding utf8
} finally {Pop-Location}
