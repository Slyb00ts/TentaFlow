$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent $MyInvocation.MyCommand.Path
cargo build --release --target-dir (Join-Path $Root "target") --manifest-path (Join-Path $Root "Cargo.toml")
