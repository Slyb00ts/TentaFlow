$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent $MyInvocation.MyCommand.Path
cargo build --release --target-dir (Join-Path $Root "target") --manifest-path (Join-Path $Root "Cargo.toml")
Copy-Item (Join-Path $Root "target\release\tentaflow-coding-agent-bridge.exe") (Join-Path $Root "server.exe") -Force
