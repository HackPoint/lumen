# Lumen benchmarks: pre-release-b8c7c0cd5e2e

> **Pre-release measurement.** This is the harness run on commit `b8c7c0cd5e2e`, before the 1.8.0 release. It is not a 1.8.0 result and must not be quoted as one.

Per-call figures, per file-size bucket and per language. There is no overall "percent saved" here on purpose: how much a person saves overall depends on their usage mix (how many reads are large, how many repeat).

- **MEASURED**: token counts (real `cl100k_base` tokenizer) and latencies (monotonic clock).
- **MODELLED**: every dollar figure. `crates/lumen-core/src/econ.rs (Econ::default) and crates/lumen-core/src/rates.rs`. Not measured, not a bill.
- Tokenizer: `cl100k_base` is a proxy for Claude's tokenizer. cl100k_base is not Claude's tokenizer. Counts compare modes with one another; they are not a bill and not the number of tokens Claude would charge.

## Builds

| label | role | commit | profile | per-call identity | reports version |
|---|---|---|---|---|---|
| v1.7.0 | baseline | `5d1ec0408ad25edcbae43cf1a6ecbc0ddba47c94` | release, cargo's default: the root Cargo.toml has no [profile.release] | CLAUDE_CODE_SESSION_ID, read once when the server starts | 1.7.0 |
| pre-b8c7c0c | candidate | `b8c7c0cd5e2e00f5cae27591c1545592c2dd562c` | release, cargo's default: the root Cargo.toml has no [profile.release] | a hook record per call (PreToolUse on the Lumen tool), claimed by the server through _meta claudecode/toolUseId | 1.7.0 |

## Machine

- OS: Microsoft Windows Server 2025 Datacenter 10.0.26100 (windows x86_64)
- CPU: AMD EPYC 7763 64-Core Processor, 4 logical cores
- RAM: 16.0 GiB
- Toolchain: rustc 1.99.0 (b940084d7 2026-09-28) / cargo 1.99.0 (5f94df478 2026-08-27)
- Started: 2026-10-13T08:38:37Z
- Load at start: CPU 1% busy (instantaneous); at end: CPU 0% busy (instantaneous)
- probe failed: kernel: no kernel probe on this OS

## Corpus

| repository | license | commit | measured files | by bucket |
|---|---|---|---:|---|
| [lumen](https://github.com/HackPoint/lumen) | MIT | `3b951e93dbe55e46dc0e15951666b303110861e7` | 96 | <300 53, 300-999 31, 1000-2999 11, 3000+ 1 |

## Latency (MEASURED)

200 recorded runs per series: 5 passes of 40, pooled. In each pass the builds take turns (the order flips every pass), and each series first has 20 warm-up runs that are discarded (per series per pass, run the same way as the recorded runs, so caches, page tables and the ledger file are warm; hook spawns are warm-start, not first launch after boot). Clock: std::time::Instant, a monotonic clock, nanosecond resolution. Release builds, built here. Medians vary with the machine and its load, and this document makes no claim about how closely another run will land. What this run shows about itself: in 45 of 45 series, the median of every one of the 5 passes is within 20% of the series' pooled median (per-pass medians are in the JSON). `benchmarks/run.sh compare <this file>` re-measures on another machine and prints, per series, the ratio to this run, the same ratio after dividing by the process floor, how many series are within 20%, and whether that machine looks like this one.

Percentiles are nearest-rank: the percentile p of n runs is the run at rank ceil(p x n / 100) of the sorted runs, fastest first, with no interpolation. With n = 200 the median is run 100 (of the two middle runs of an even count, the faster), p90 is run 180, and p99 is run 198, which leaves the slowest 2 runs above it. Raw samples: not written. Each series carries `pass_medians_ns`, the median of each pass on its own, in pass order: how far they sit from one another is how much the machine moved while the run was going on.

Subjects (corpus files the series run on):

| bucket | file | lines | bytes | recall item |
|---|---|---:|---:|---|
| <300 | crates/lumen-core/tests/ranked_coverage.rs | 122 | 3754 | `walk` |
| 300-999 | crates/lumen-mcp/tests/protocol.rs | 532 | 17672 | `initialize_returns_a_well_formed_jsonrpc_response` |
| 1000-2999 | crates/lumen-core/tests/silent_fallbacks.rs | 1472 | 52110 | `rust_statement` |

### Hooks, as the harness sees them: process spawn to exit

The whole hook as Claude Code runs it. `hook.floor` is a hook invocation that does no work, so it is the cost of the process itself; the work is what each row adds to it. `hook.per_call_identity` is the PreToolUse hook on a call of one of Lumen's own MCP tools, which records who is calling; a build either has it (the series is timed) or does not (the slot says `n/a`, and why). Which one is decided by running the build, not by reading its usage text.

| series | subject | n | v1.7.0 median | v1.7.0 p90 | v1.7.0 p99 | pre-b8c7c0c median | pre-b8c7c0c p90 | pre-b8c7c0c p99 |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| `hook.floor` | - | 200 | 5.50 ms | 5.89 ms | 6.46 ms | 5.44 ms | 5.63 ms | 6.58 ms |
| `hook.intercept_pass.spawn` | crates/lumen-core/tests/ranked_coverage.rs (122 lines, bucket <300) | 200 | 5.61 ms | 6.03 ms | 6.67 ms | 5.62 ms | 5.78 ms | 6.41 ms |
| `hook.intercept_block.spawn` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | 5.84 ms | 6.13 ms | 6.66 ms | 7.06 ms | 7.53 ms | 8.75 ms |
| `hook.meter_read.spawn` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | 76.05 ms | 80.44 ms | 85.08 ms | 74.48 ms | 77.01 ms | 83.09 ms |
| `hook.meter_bash.spawn` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | 73.96 ms | 77.82 ms | 85.03 ms | 72.77 ms | 75.86 ms | 82.88 ms |
| `hook.per_call_identity` | - | 200 | n/a (see notes) | | | 5.90 ms | 6.18 ms | 6.87 ms |

Note: `hook.intercept_block.spawn` is 1.21 ms slower on pre-b8c7c0c than on v1.7.0 (7.06 ms against 5.84 ms, medians as measured on this machine in this run). This run times the hook, it does not trace it, so it does not say why. To find the cause, read what changed in the blocking path of `hook intercept` between the two commits (`git log 5d1ec0408ad2..b8c7c0cd5e2e -- crates/lumen-mcp crates/lumen-core`). The hook that lets a Read through (`hook.intercept_pass.spawn`) is 5.62 ms on pre-b8c7c0c and 5.61 ms on v1.7.0, which is no slower, so the extra time is in the blocking path rather than in starting the process.

### Hooks, work only: `hook::run_with` called in-process, warm (only for a build made from this checkout)

No process start, a warm tokenizer. A fresh hook process pays tokenizer start-up that these calls do not, which is part of the spawn figures above.

| series | subject | n | v1.7.0 median | v1.7.0 p90 | v1.7.0 p99 | pre-b8c7c0c median | pre-b8c7c0c p90 | pre-b8c7c0c p99 |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| `hook.intercept_pass.inproc` | crates/lumen-core/tests/ranked_coverage.rs (122 lines, bucket <300) | 200 | n/a (see notes) | | | 48 us | 52 us | 95 us |
| `hook.intercept_block.inproc` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | n/a (see notes) | | | 824 us | 885 us | 1.03 ms |
| `hook.meter_read.inproc` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | n/a (see notes) | | | 4.65 ms | 4.95 ms | 7.37 ms |
| `hook.meter_bash.inproc` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | n/a (see notes) | | | 2.89 ms | 3.15 ms | 4.96 ms |

### Derived: hook work above the process floor

`median(spawn) - median(floor)`, in each build. A derived figure, not a series of its own.

| hook | v1.7.0 | pre-b8c7c0c |
|---|---:|---:|
| `hook.intercept_pass.spawn_minus_floor` | 110 us | 175 us |
| `hook.intercept_block.spawn_minus_floor` | 343 us | 1.62 ms |
| `hook.meter_read.spawn_minus_floor` | 70.55 ms | 69.04 ms |
| `hook.meter_bash.spawn_minus_floor` | 68.46 ms | 67.33 ms |
| `hook.per_call_identity.spawn_minus_floor` | n/a | 456 us |
| `hook.per_call_identity.server_claim` | n/a | 72 us |

### Server calls, round trip as a client sees it

From just before the request is written to the arrival of the reply line. `server.ping` is the round trip with no work; the ledger row is written after the reply and is not in the interval. For a build with per-call identity every call in these series also carries the hook's record and `_meta` id, so the server's claim of it is inside the interval; `server.identity.placed` against `server.identity.unplaced` (the same call with no record and no id) isolates that claim. Only a build with per-call identity has those two series.

| series | subject | n | v1.7.0 median | v1.7.0 p90 | v1.7.0 p99 | pre-b8c7c0c median | pre-b8c7c0c p90 | pre-b8c7c0c p99 |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| `server.start` | - | 200 | 4.88 ms | 5.17 ms | 5.63 ms | 4.94 ms | 5.21 ms | 5.69 ms |
| `server.ping` | - | 200 | 62 us | 66 us | 103 us | 62 us | 67 us | 94 us |
| `server.smart_read.<300` | crates/lumen-core/tests/ranked_coverage.rs (122 lines, bucket <300) | 200 | 1.59 ms | 1.64 ms | 2.16 ms | 1.80 ms | 1.97 ms | 2.73 ms |
| `server.recall_file.<300` | crates/lumen-core/tests/ranked_coverage.rs (122 lines, bucket <300) | 200 | 1.63 ms | 1.68 ms | 1.82 ms | 1.82 ms | 1.97 ms | 2.10 ms |
| `server.smart_read.300-999` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | 6.14 ms | 6.33 ms | 6.69 ms | 6.45 ms | 6.97 ms | 10.58 ms |
| `server.recall_file.300-999` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | 5.82 ms | 6.04 ms | 6.62 ms | 6.24 ms | 6.87 ms | 10.07 ms |
| `server.smart_read.1000-2999` | crates/lumen-core/tests/silent_fallbacks.rs (1472 lines, bucket 1000-2999) | 200 | 14.13 ms | 14.72 ms | 16.10 ms | 14.66 ms | 15.58 ms | 18.21 ms |
| `server.recall_file.1000-2999` | crates/lumen-core/tests/silent_fallbacks.rs (1472 lines, bucket 1000-2999) | 200 | 13.75 ms | 14.11 ms | 15.10 ms | 14.58 ms | 15.11 ms | 19.82 ms |
| `server.repeat_unchanged.<300` | crates/lumen-core/tests/ranked_coverage.rs (122 lines, bucket <300) | 200 | 2.30 ms | 2.38 ms | 3.07 ms | 2.59 ms | 2.88 ms | 3.95 ms |
| `server.repeat_changed.<300` | crates/lumen-core/tests/ranked_coverage.rs (122 lines, bucket <300) | 200 | 2.33 ms | 2.40 ms | 2.51 ms | 2.59 ms | 2.86 ms | 3.06 ms |
| `server.repeat_unchanged.300-999` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | 7.07 ms | 7.24 ms | 8.03 ms | 7.49 ms | 8.04 ms | 11.37 ms |
| `server.repeat_changed.300-999` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | 7.03 ms | 7.26 ms | 9.08 ms | 7.47 ms | 8.12 ms | 11.19 ms |
| `server.repeat_unchanged.1000-2999` | crates/lumen-core/tests/silent_fallbacks.rs (1472 lines, bucket 1000-2999) | 200 | 15.31 ms | 15.77 ms | 17.89 ms | 15.85 ms | 16.65 ms | 18.62 ms |
| `server.repeat_changed.1000-2999` | crates/lumen-core/tests/silent_fallbacks.rs (1472 lines, bucket 1000-2999) | 200 | 15.29 ms | 15.80 ms | 16.62 ms | 15.88 ms | 16.97 ms | 20.33 ms |
| `server.identity.placed` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | - | | | 6.44 ms | 6.83 ms | 8.21 ms |
| `server.identity.unplaced` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | - | | | 6.37 ms | 6.75 ms | 9.45 ms |

Notes on series that are not plain measurements:

- `hook.intercept_pass.inproc` on v1.7.0: n/a: the harness links this checkout's hook library, so it can time in-process only a binary built from the checkout; the comparable figure for any other build is spawn minus floor
- `hook.intercept_block.inproc` on v1.7.0: n/a: the harness links this checkout's hook library, so it can time in-process only a binary built from the checkout; the comparable figure for any other build is spawn minus floor
- `hook.meter_read.inproc` on v1.7.0: n/a: the harness links this checkout's hook library, so it can time in-process only a binary built from the checkout; the comparable figure for any other build is spawn minus floor
- `hook.meter_bash.inproc` on v1.7.0: n/a: the harness links this checkout's hook library, so it can time in-process only a binary built from the checkout; the comparable figure for any other build is spawn minus floor
- `hook.per_call_identity` on v1.7.0: n/a — this build records no per-call identity (a PreToolUse payload for mcp__lumen__smart_read, sent to `hook intercept`, left no record beside the ledger; commit 5d1ec0408ad2)

Load on the machine around the whole latency run (every build, every pass):

- before: CPU 2% busy (instantaneous); after: CPU 2% busy (instantaneous)

## Dollars (MODELLED)

Every dollar figure above is `modelled`: a model's arithmetic on measured token counts, not a measured cost.

- Source: crates/lumen-core/src/econ.rs (Econ::default) and crates/lumen-core/src/rates.rs
- Rates, USD per million tokens: input 5, output 25, cache read 0.5, cache write 6.25. rates.rs labels these Sonnet 3.5 / 3.7 / 4 pricing. The harness does not check them against a current price list; they are the model's inputs, not a quote.
- Inputs: context 362965 tokens, output 1085 tokens, 65 rounds remaining. measured_defaults: means over 19,161 main-agent turns of the Lumen author's own sessions (econ.rs); another user's profile differs
- Value of one saved token: $0.00003875 (modelled). One extra round for an intercepted Read: $0.2086075 (modelled), which 5383 saved tokens pay for.
- Formula: value of one saved token = (cache_write + cache_read x rounds_remaining) / 1e6 dollars; one intercepted Read costs one extra round = (context x cache_read + output x output_rate) / 1e6 dollars; a dollar figure below is the median saved tokens of a call x the value of one token
