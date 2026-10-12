# The one command that reproduces the benchmarks on Windows. See benchmarks/README.md.
#
#   benchmarks\run.ps1 --latency-only
#
# Equivalent to: cargo run --release --locked -p lumen-bench -- <options>
$ErrorActionPreference = 'Stop'
Set-Location (Split-Path -Parent $PSScriptRoot)
cargo run --release --locked -p lumen-bench -- @args
exit $LASTEXITCODE
