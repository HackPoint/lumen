#!/usr/bin/env sh
# The one command that reproduces the benchmarks (macOS and Linux). See benchmarks/README.md.
#
#   benchmarks/run.sh                      full run: tokens and latency, v1.9.0 against this checkout
#   benchmarks/run.sh --latency-only       the part a CI job runs
#   benchmarks/run.sh --help               every option
#
# Equivalent to: cargo run --release --locked -p lumen-bench -- <options>
set -eu

here=$(cd "$(dirname "$0")" && pwd)
cd "$here/.."
exec cargo run --release --locked -p lumen-bench -- "$@"
