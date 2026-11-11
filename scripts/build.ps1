param([switch]$RebuildAgent,[switch]$SkipTests)
$ErrorActionPreference='Stop'
& (Join-Path $PSScriptRoot 'build-tauri.ps1') -RebuildAgent:$RebuildAgent -SkipTests:$SkipTests
