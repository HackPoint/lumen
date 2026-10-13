# Lumen benchmarks: pre-release-4ca4fd1ffe21

> **Pre-release measurement.** This is the harness run on commit `4ca4fd1ffe21`, before the 1.8.0 release. It is not a 1.8.0 result and must not be quoted as one.

Per-call figures, per file-size bucket and per language. There is no overall "percent saved" here on purpose: how much a person saves overall depends on their usage mix (how many reads are large, how many repeat).

- **MEASURED**: token counts (real `cl100k_base` tokenizer) and latencies (monotonic clock).
- **MODELLED**: every dollar figure. `crates/lumen-core/src/econ.rs (Econ::default) and crates/lumen-core/src/rates.rs`. Not measured, not a bill.
- Tokenizer: `cl100k_base` is a proxy for Claude's tokenizer. cl100k_base is not Claude's tokenizer. Counts compare modes with one another; they are not a bill and not the number of tokens Claude would charge.

## Builds

| label | role | commit | profile | per-call identity | reports version |
|---|---|---|---|---|---|
| v1.7.0 | baseline | `5d1ec0408ad25edcbae43cf1a6ecbc0ddba47c94` | release, cargo's default: the root Cargo.toml has no [profile.release] | CLAUDE_CODE_SESSION_ID, read once when the server starts | 1.7.0 |
| pre-4ca4fd1 | candidate | `4ca4fd1ffe216f47e2c764be80cce4cbb56de6bc` | release, cargo's default: the root Cargo.toml has no [profile.release] | a hook record per call (PreToolUse on the Lumen tool), claimed by the server through _meta claudecode/toolUseId | 1.7.0 |

## Machine

- OS: Ubuntu 24.04.5 LTS (linux x86_64)
- CPU: AMD EPYC 9V74 80-Core Processor, 4 logical cores
- RAM: 15.6 GiB
- Toolchain: rustc 1.99.0 (b940084d7 2026-09-28) / cargo 1.99.0 (5f94df478 2026-08-27)
- Started: 2026-10-13T11:33:37Z
- Load at start: load average 3.06 1.34 0.54 (1, 5, 15 min); at end: load average 1.32 1.24 0.64 (1, 5, 15 min)

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

| series | subject | n | v1.7.0 median | v1.7.0 p90 | v1.7.0 p99 | pre-4ca4fd1 median | pre-4ca4fd1 p90 | pre-4ca4fd1 p99 |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| `hook.floor` | - | 200 | 1.34 ms | 1.43 ms | 1.49 ms | 1.34 ms | 1.47 ms | 1.52 ms |
| `hook.intercept_pass.spawn` | crates/lumen-core/tests/ranked_coverage.rs (122 lines, bucket <300) | 200 | 1.41 ms | 1.52 ms | 1.58 ms | 1.40 ms | 1.52 ms | 1.57 ms |
| `hook.intercept_block.spawn` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | 1.47 ms | 1.59 ms | 1.63 ms | 2.03 ms | 2.14 ms | 2.22 ms |
| `hook.meter_read.spawn` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | 63.10 ms | 66.28 ms | 68.48 ms | 63.52 ms | 67.81 ms | 71.30 ms |
| `hook.meter_bash.spawn` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | 62.69 ms | 66.33 ms | 76.50 ms | 60.26 ms | 64.98 ms | 68.66 ms |
| `hook.per_call_identity` | - | 200 | n/a (see notes) | | | 1.48 ms | 1.56 ms | 1.64 ms |

Note: `hook.intercept_block.spawn` is 560 us slower on pre-4ca4fd1 than on v1.7.0 (2.03 ms against 1.47 ms, medians as measured on this machine in this run). This run times the hook, it does not trace it, so it does not say why. To find the cause, read what changed in the blocking path of `hook intercept` between the two commits (`git log 5d1ec0408ad2..4ca4fd1ffe21 -- crates/lumen-mcp crates/lumen-core`). The hook that lets a Read through (`hook.intercept_pass.spawn`) is 1.40 ms on pre-4ca4fd1 and 1.41 ms on v1.7.0, which is no slower, so the extra time is in the blocking path rather than in starting the process.

### Hooks, work only: `hook::run_with` called in-process, warm (only for a build made from this checkout)

No process start, a warm tokenizer. A fresh hook process pays tokenizer start-up that these calls do not, which is part of the spawn figures above.

| series | subject | n | v1.7.0 median | v1.7.0 p90 | v1.7.0 p99 | pre-4ca4fd1 median | pre-4ca4fd1 p90 | pre-4ca4fd1 p99 |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| `hook.intercept_pass.inproc` | crates/lumen-core/tests/ranked_coverage.rs (122 lines, bucket <300) | 200 | n/a (see notes) | | | 14 us | 14 us | 33 us |
| `hook.intercept_block.inproc` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | n/a (see notes) | | | 411 us | 441 us | 461 us |
| `hook.meter_read.inproc` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | n/a (see notes) | | | 3.30 ms | 3.52 ms | 4.32 ms |
| `hook.meter_bash.inproc` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | n/a (see notes) | | | 1.66 ms | 1.85 ms | 2.73 ms |

### Derived: hook work above the process floor

`median(spawn) - median(floor)`, in each build. A derived figure, not a series of its own.

| hook | v1.7.0 | pre-4ca4fd1 |
|---|---:|---:|
| `hook.intercept_pass.spawn_minus_floor` | 70 us | 56 us |
| `hook.intercept_block.spawn_minus_floor` | 129 us | 684 us |
| `hook.meter_read.spawn_minus_floor` | 61.76 ms | 62.17 ms |
| `hook.meter_bash.spawn_minus_floor` | 61.35 ms | 58.92 ms |
| `hook.per_call_identity.spawn_minus_floor` | n/a | 133 us |
| `hook.per_call_identity.server_claim` | n/a | 158 us |

### Server calls, round trip as a client sees it

From just before the request is written to the arrival of the reply line. `server.ping` is the round trip with no work; the ledger row is written after the reply and is not in the interval. For a build with per-call identity every call in these series also carries the hook's record and `_meta` id, so the server's claim of it is inside the interval; `server.identity.placed` against `server.identity.unplaced` (the same call with no record and no id) isolates that claim. Only a build with per-call identity has those two series.

| series | subject | n | v1.7.0 median | v1.7.0 p90 | v1.7.0 p99 | pre-4ca4fd1 median | pre-4ca4fd1 p90 | pre-4ca4fd1 p99 |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| `server.start` | - | 200 | 1.26 ms | 1.34 ms | 1.39 ms | 1.29 ms | 1.37 ms | 1.47 ms |
| `server.ping` | - | 200 | 31 us | 36 us | 44 us | 32 us | 35 us | 51 us |
| `server.smart_read.<300` | crates/lumen-core/tests/ranked_coverage.rs (122 lines, bucket <300) | 200 | 1.24 ms | 1.34 ms | 1.39 ms | 1.47 ms | 1.58 ms | 1.66 ms |
| `server.recall_file.<300` | crates/lumen-core/tests/ranked_coverage.rs (122 lines, bucket <300) | 200 | 1.31 ms | 1.36 ms | 1.45 ms | 1.50 ms | 1.61 ms | 1.66 ms |
| `server.smart_read.300-999` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | 4.94 ms | 5.47 ms | 5.61 ms | 5.25 ms | 5.42 ms | 5.60 ms |
| `server.recall_file.300-999` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | 4.85 ms | 5.29 ms | 5.45 ms | 5.15 ms | 5.26 ms | 5.37 ms |
| `server.smart_read.1000-2999` | crates/lumen-core/tests/silent_fallbacks.rs (1472 lines, bucket 1000-2999) | 200 | 12.25 ms | 12.82 ms | 13.10 ms | 12.34 ms | 12.55 ms | 13.37 ms |
| `server.recall_file.1000-2999` | crates/lumen-core/tests/silent_fallbacks.rs (1472 lines, bucket 1000-2999) | 200 | 12.11 ms | 12.75 ms | 14.39 ms | 12.28 ms | 12.48 ms | 15.90 ms |
| `server.repeat_unchanged.<300` | crates/lumen-core/tests/ranked_coverage.rs (122 lines, bucket <300) | 200 | 1.71 ms | 1.79 ms | 2.49 ms | 1.99 ms | 2.08 ms | 2.16 ms |
| `server.repeat_changed.<300` | crates/lumen-core/tests/ranked_coverage.rs (122 lines, bucket <300) | 200 | 1.75 ms | 1.85 ms | 2.20 ms | 2.04 ms | 2.14 ms | 2.21 ms |
| `server.repeat_unchanged.300-999` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | 5.69 ms | 6.10 ms | 6.20 ms | 6.02 ms | 6.17 ms | 6.36 ms |
| `server.repeat_changed.300-999` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | 5.84 ms | 6.01 ms | 6.56 ms | 6.11 ms | 6.24 ms | 6.66 ms |
| `server.repeat_unchanged.1000-2999` | crates/lumen-core/tests/silent_fallbacks.rs (1472 lines, bucket 1000-2999) | 200 | 13.30 ms | 13.67 ms | 14.03 ms | 13.53 ms | 13.71 ms | 13.85 ms |
| `server.repeat_changed.1000-2999` | crates/lumen-core/tests/silent_fallbacks.rs (1472 lines, bucket 1000-2999) | 200 | 13.38 ms | 13.78 ms | 14.04 ms | 13.67 ms | 13.85 ms | 14.38 ms |
| `server.identity.placed` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | - | | | 5.20 ms | 5.51 ms | 8.62 ms |
| `server.identity.unplaced` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | - | | | 5.04 ms | 5.41 ms | 6.00 ms |

Notes on series that are not plain measurements:

- `hook.intercept_pass.inproc` on v1.7.0: n/a: the harness links this checkout's hook library, so it can time in-process only a binary built from the checkout; the comparable figure for any other build is spawn minus floor
- `hook.intercept_block.inproc` on v1.7.0: n/a: the harness links this checkout's hook library, so it can time in-process only a binary built from the checkout; the comparable figure for any other build is spawn minus floor
- `hook.meter_read.inproc` on v1.7.0: n/a: the harness links this checkout's hook library, so it can time in-process only a binary built from the checkout; the comparable figure for any other build is spawn minus floor
- `hook.meter_bash.inproc` on v1.7.0: n/a: the harness links this checkout's hook library, so it can time in-process only a binary built from the checkout; the comparable figure for any other build is spawn minus floor
- `hook.per_call_identity` on v1.7.0: n/a — this build records no per-call identity (a PreToolUse payload for mcp__lumen__smart_read, sent to `hook intercept`, left no record beside the ledger; commit 5d1ec0408ad2)

Load on the machine around the whole latency run (every build, every pass):

- before: load average 3.06 1.34 0.54 (1, 5, 15 min); after: load average 1.32 1.24 0.64 (1, 5, 15 min)

## Dollars (MODELLED)

Every dollar figure above is `modelled`: a model's arithmetic on measured token counts, not a measured cost.

- Source: crates/lumen-core/src/econ.rs (Econ::default) and crates/lumen-core/src/rates.rs
- Rates, USD per million tokens: input 5, output 25, cache read 0.5, cache write 6.25. rates.rs labels these Sonnet 3.5 / 3.7 / 4 pricing. The harness does not check them against a current price list; they are the model's inputs, not a quote.
- Inputs: context 362965 tokens, output 1085 tokens, 65 rounds remaining. measured_defaults: means over 19,161 main-agent turns of the Lumen author's own sessions (econ.rs); another user's profile differs
- Value of one saved token: $0.00003875 (modelled). One extra round for an intercepted Read: $0.2086075 (modelled), which 5383 saved tokens pay for.
- Formula: value of one saved token = (cache_write + cache_read x rounds_remaining) / 1e6 dollars; one intercepted Read costs one extra round = (context x cache_read + output x output_rate) / 1e6 dollars; a dollar figure below is the median saved tokens of a call x the value of one token
