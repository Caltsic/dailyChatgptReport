#!/usr/bin/env bash
# Build the Ubuntu member package (.deb) and a runnable install directory.
# The leader end stays Windows-only for now; this script therefore skips --features leader.
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
desktop="$root/desktop"
installers="$root/artifacts/linux-installers"
publish="$root/artifacts/linux-publish"

cd "$desktop"
if [[ ! -d node_modules ]]; then
    npm ci
fi
npm run build

if [[ "${SKIP_TESTS:-0}" != "1" ]]; then
    cargo test --manifest-path src-tauri/Cargo.toml --lib
fi

mkdir -p "$installers" "$publish"
# The base config lists the Windows NSIS target; --bundles overrides it for Linux.
npx tauri build --bundles deb --config src-tauri/tauri.build.conf.json

deb="$(ls -t src-tauri/target/release/bundle/deb/*.deb 2>/dev/null | head -n 1 || true)"
if [[ -z "$deb" ]]; then
    echo "missing .deb output" >&2
    exit 1
fi
cp "$deb" "$installers/CodexDailyReport.Member.deb"

binary="src-tauri/target/release/CodexDailyReport.Member"
if [[ ! -f "$binary" ]]; then
    binary="src-tauri/target/release/codex-daily-report"
fi
if [[ -f "$binary" ]]; then
    cp "$binary" "$publish/CodexDailyReport.Member"
fi

{
    sha256sum "$installers/CodexDailyReport.Member.deb"
    if [[ -f "$publish/CodexDailyReport.Member" ]]; then
        sha256sum "$publish/CodexDailyReport.Member"
    fi
} > "$installers/SHA256.txt"

echo "Linux member build ready: $installers"
