$ErrorActionPreference = ''Stop''
Set-Location E:\TestDeepSeek
cargo fmt --all -- --check
cargo build --release
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
Write-Host ''CI OK'' -ForegroundColor Green
