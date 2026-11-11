param([switch]$SkipInstall)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$python = Join-Path $root '.tools\agent-venv\Scripts\python.exe'
if (!(Test-Path -LiteralPath $python)) {
    python -m venv (Join-Path $root '.tools\agent-venv')
    if ($LASTEXITCODE -ne 0) { throw 'Agent Python environment creation failed' }
}
if (!$SkipInstall) {
    & $python -m pip install -r (Join-Path $root 'agent\requirements.txt')
    if ($LASTEXITCODE -ne 0) { throw 'Agent dependency installation failed' }
}
$tokenizerCache = Join-Path $root '.tools\tokenizer-cache'
New-Item -ItemType Directory -Force -Path $tokenizerCache | Out-Null
$previousTokenizerCache = $env:TIKTOKEN_CACHE_DIR
try {
    $env:TIKTOKEN_CACHE_DIR = $tokenizerCache
    & $python -c "import tiktoken; tiktoken.get_encoding('o200k_base'); tiktoken.get_encoding('cl100k_base')"
    if ($LASTEXITCODE -ne 0) { throw 'Tokenizer data preparation failed' }
} finally { $env:TIKTOKEN_CACHE_DIR = $previousTokenizerCache }
& $python -m PyInstaller --noconfirm --clean --onedir --name report-agent --distpath (Join-Path $root 'artifacts\agent-build') --workpath (Join-Path $root 'artifacts\agent-pyinstaller') --specpath (Join-Path $root 'artifacts\agent-pyinstaller') --collect-all langchain --collect-all langgraph --collect-all langchain_openai --collect-all tiktoken --hidden-import tiktoken_ext.openai_public --add-data "$tokenizerCache;tokenizer-cache" --copy-metadata langchain --copy-metadata langgraph --copy-metadata langchain-core --copy-metadata langchain-openai --copy-metadata langgraph-checkpoint --copy-metadata langgraph-checkpoint-sqlite (Join-Path $root 'agent\runner.py')
if ($LASTEXITCODE -ne 0) { throw 'Agent packaging failed' }
$target = [IO.Path]::GetFullPath((Join-Path $root 'artifacts\agent-runtime'))
$boundary = [IO.Path]::GetFullPath((Join-Path $root 'artifacts')) + [IO.Path]::DirectorySeparatorChar
if (!$target.StartsWith($boundary, [StringComparison]::OrdinalIgnoreCase)) { throw 'Unexpected Agent output directory' }
if (Test-Path -LiteralPath $target) { Remove-Item -LiteralPath $target -Recurse -Force }
Move-Item -LiteralPath (Join-Path $root 'artifacts\agent-build\report-agent') -Destination $target
Write-Output "Agent runtime ready: $target\report-agent.exe"
