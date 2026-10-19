#!/usr/bin/env sh
# The one command that reproduces the version-over-version benchmark on this OS (macOS and Linux).
# See benchmarks/versions/METHOD.md.
#
#   benchmarks/versions/run.sh              fetch the pinned assets, measure every version and no
#                                           Lumen, write benchmarks/versions/results/<os>.json
#                                           and <os>.md
#   benchmarks/versions/run.sh --dry-run    print the plan, fetch and start nothing
#   benchmarks/versions/run.sh --help       every option
#
# The gh issue command of every regression is printed, never run.
#
# Equivalent to: cargo run --release --locked -p lumen-bench -- versions all <options>
set -eu

here=$(cd "$(dirname "$0")" && pwd)
cd "$here/../.."
exec cargo run --release --locked -p lumen-bench -- versions all "$@"
