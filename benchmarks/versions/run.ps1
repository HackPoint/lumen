# The one command that reproduces the version-over-version benchmark on Windows.
# See benchmarks/versions/METHOD.md.
#
#   benchmarks\versions\run.ps1 --dry-run
#
# Equivalent to: cargo run --release --locked -p lumen-bench -- versions all <options>
$ErrorActionPreference = 'Stop'
Set-Location (Split-Path -Parent (Split-Path -Parent $PSScriptRoot))
cargo run --release --locked -p lumen-bench -- versions all @args
exit $LASTEXITCODE
