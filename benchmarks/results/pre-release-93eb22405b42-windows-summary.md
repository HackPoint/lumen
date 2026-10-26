# Lumen benchmarks: pre-release-93eb22405b42

> **Pre-release measurement.** This is the harness run on commit `93eb22405b42`, before the 1.10.0 release. It is not a 1.10.0 result and must not be quoted as one.

Per-call figures, per file-size bucket and per language. There is no overall "percent saved" here on purpose: how much a person saves overall depends on their usage mix (how many reads are large, how many repeat).

- **MEASURED**: token counts (real `cl100k_base` tokenizer) and latencies (monotonic clock).
- **No dollar figure** (see the end).
- Tokenizer: `cl100k_base` is a proxy for Claude's tokenizer. cl100k_base is not Claude's tokenizer. Counts compare modes with one another; they are not a bill and not the number of tokens Claude would charge.

## Builds

| label | role | commit | profile | per-call identity | reports version |
|---|---|---|---|---|---|
| v1.9.0 | baseline | `cbb8d0fe9d18acb9d7ca1b839dab528fbcb522e7` | release, cargo's default: the root Cargo.toml has no [profile.release] | a hook record per call (PreToolUse on the Lumen tool), claimed by the server through _meta claudecode/toolUseId | 1.9.0 |
| pre-93eb224 | candidate | `93eb22405b427cc445daeb78a97337d1431a82b1` | release, cargo's default: the root Cargo.toml has no [profile.release] | a hook record per call (PreToolUse on the Lumen tool), claimed by the server through _meta claudecode/toolUseId | 1.10.0 |

## Machine

- OS: Microsoft Windows Server 2025 Datacenter 10.0.26100 (windows x86_64)
- CPU: AMD EPYC 9V45 96-Core Processor, 4 logical cores
- RAM: 16.0 GiB
- Toolchain: rustc 1.99.1 (58cf8f9c3 2026-10-13) / cargo 1.99.1 (5f94df478 2026-08-27)
- Started: 2026-10-26T12:17:42Z
- Load at start: CPU 2% busy (instantaneous); at end: CPU 6% busy (instantaneous)
- probe failed: kernel: no kernel probe on this OS

## Corpus

| repository | license | commit | measured files | by bucket |
|---|---|---|---:|---|
| [lumen](https://github.com/HackPoint/lumen) | MIT | `3b951e93dbe55e46dc0e15951666b303110861e7` | 96 | <300 53, 300-999 31, 1000-2999 11, 3000+ 1 |

## Latency (MEASURED)

200 recorded runs per series: 5 passes of 40, pooled. In each pass the builds take turns (the order flips every pass), and each series first has 20 warm-up runs that are discarded (per series per pass, run the same way as the recorded runs, so caches, page tables and the ledger file are warm; hook spawns are warm-start, not first launch after boot). Clock: std::time::Instant, a monotonic clock, nanosecond resolution. Release builds, built here. Medians vary with the machine and its load, and this document makes no claim about how closely another run will land. What this run shows about itself: in 48 of 48 series, the median of every one of the 5 passes is within 20% of the series' pooled median (per-pass medians are in the JSON). `benchmarks/run.sh compare <this file>` re-measures on another machine and prints, per series, the ratio to this run, the same ratio after dividing by the process floor, how many series are within 20%, and whether that machine looks like this one.

Percentiles are nearest-rank: the percentile p of n runs is the run at rank ceil(p x n / 100) of the sorted runs, fastest first, with no interpolation. With n = 200 the median is run 100 (of the two middle runs of an even count, the faster), p90 is run 180, and p99 is run 198, which leaves the slowest 2 runs above it. Raw samples: not written. Each series carries `pass_medians_ns`, the median of each pass on its own, in pass order: how far they sit from one another is how much the machine moved while the run was going on.

Subjects (corpus files the series run on):

| bucket | file | lines | bytes | recall item |
|---|---|---:|---:|---|
| <300 | crates/lumen-core/tests/ranked_coverage.rs | 122 | 3754 | `walk` |
| 300-999 | crates/lumen-mcp/tests/protocol.rs | 532 | 17672 | `initialize_returns_a_well_formed_jsonrpc_response` |
| 1000-2999 | crates/lumen-core/tests/silent_fallbacks.rs | 1472 | 52110 | `rust_statement` |

### Hooks, as the harness sees them: process spawn to exit

The whole hook as Claude Code runs it. `hook.floor` is a hook invocation that does no work, so it is the cost of the process itself; the work is what each row adds to it. `hook.per_call_identity` is the PreToolUse hook on a call of one of Lumen's own MCP tools, which records who is calling; a build either has it (the series is timed) or does not (the slot says `n/a`, and why). Which one is decided by running the build, not by reading its usage text.

| series | subject | n | v1.9.0 median | v1.9.0 p90 | v1.9.0 p99 | pre-93eb224 median | pre-93eb224 p90 | pre-93eb224 p99 |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| `hook.floor` | - | 200 | 3.25 ms | 3.37 ms | 3.89 ms | 3.30 ms | 3.38 ms | 3.49 ms |
| `hook.intercept_pass.spawn` | crates/lumen-core/tests/ranked_coverage.rs (122 lines, bucket <300) | 200 | 3.37 ms | 3.51 ms | 4.07 ms | 3.40 ms | 3.64 ms | 3.91 ms |
| `hook.intercept_block.spawn` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | 4.38 ms | 4.60 ms | 5.53 ms | 4.44 ms | 4.75 ms | 7.96 ms |
| `hook.meter_read.spawn` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | 52.54 ms | 55.45 ms | 75.10 ms | 54.06 ms | 57.96 ms | 70.76 ms |
| `hook.meter_bash.spawn` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | 51.91 ms | 55.03 ms | 59.98 ms | 53.44 ms | 58.71 ms | 78.47 ms |
| `hook.per_call_identity` | - | 200 | 3.62 ms | 3.83 ms | 4.24 ms | 3.65 ms | 4.02 ms | 7.40 ms |

### Hooks, work only: `hook::run_with` called in-process, warm (only for a build made from this checkout)

No process start, a warm tokenizer. A fresh hook process pays tokenizer start-up that these calls do not, which is part of the spawn figures above.

| series | subject | n | v1.9.0 median | v1.9.0 p90 | v1.9.0 p99 | pre-93eb224 median | pre-93eb224 p90 | pre-93eb224 p99 |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| `hook.intercept_pass.inproc` | crates/lumen-core/tests/ranked_coverage.rs (122 lines, bucket <300) | 200 | n/a (see notes) | | | 27 us | 28 us | 42 us |
| `hook.intercept_block.inproc` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | n/a (see notes) | | | 538 us | 621 us | 757 us |
| `hook.meter_read.inproc` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | n/a (see notes) | | | 2.39 ms | 2.56 ms | 14.30 ms |
| `hook.meter_bash.inproc` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | n/a (see notes) | | | 1.40 ms | 1.51 ms | 1.81 ms |

### Derived: hook work above the process floor

`median(spawn) - median(floor)`, in each build. A derived figure, not a series of its own.

| hook | v1.9.0 | pre-93eb224 |
|---|---:|---:|
| `hook.intercept_pass.spawn_minus_floor` | 121 us | 110 us |
| `hook.intercept_block.spawn_minus_floor` | 1.13 ms | 1.14 ms |
| `hook.meter_read.spawn_minus_floor` | 49.30 ms | 50.77 ms |
| `hook.meter_bash.spawn_minus_floor` | 48.66 ms | 50.15 ms |
| `hook.per_call_identity.spawn_minus_floor` | 373 us | 353 us |
| `hook.per_call_identity.server_claim` | 91 us | 112 us |

### Server calls, round trip as a client sees it

From just before the request is written to the arrival of the reply line. `server.ping` is the round trip with no work; the ledger row is written after the reply and is not in the interval. For a build with per-call identity every call in these series also carries the hook's record and `_meta` id, so the server's claim of it is inside the interval; `server.identity.placed` against `server.identity.unplaced` (the same call with no record and no id) isolates that claim. Only a build with per-call identity has those two series.

| series | subject | n | v1.9.0 median | v1.9.0 p90 | v1.9.0 p99 | pre-93eb224 median | pre-93eb224 p90 | pre-93eb224 p99 |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| `server.start` | - | 200 | 3.07 ms | 3.17 ms | 3.31 ms | 3.10 ms | 3.18 ms | 3.47 ms |
| `server.ping` | - | 200 | 32 us | 34 us | 43 us | 34 us | 41 us | 59 us |
| `server.smart_read.<300` | crates/lumen-core/tests/ranked_coverage.rs (122 lines, bucket <300) | 200 | 1.09 ms | 1.13 ms | 1.49 ms | 1.10 ms | 1.15 ms | 1.41 ms |
| `server.recall_file.<300` | crates/lumen-core/tests/ranked_coverage.rs (122 lines, bucket <300) | 200 | 1.11 ms | 1.15 ms | 1.53 ms | 1.14 ms | 1.18 ms | 1.55 ms |
| `server.smart_read.300-999` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | 3.89 ms | 4.01 ms | 5.46 ms | 3.98 ms | 4.15 ms | 5.72 ms |
| `server.recall_file.300-999` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | 3.67 ms | 3.87 ms | 4.05 ms | 3.94 ms | 4.09 ms | 5.51 ms |
| `server.smart_read.1000-2999` | crates/lumen-core/tests/silent_fallbacks.rs (1472 lines, bucket 1000-2999) | 200 | 8.82 ms | 9.32 ms | 12.58 ms | 8.97 ms | 9.46 ms | 10.26 ms |
| `server.recall_file.1000-2999` | crates/lumen-core/tests/silent_fallbacks.rs (1472 lines, bucket 1000-2999) | 200 | 8.43 ms | 8.81 ms | 10.94 ms | 8.83 ms | 9.44 ms | 13.51 ms |
| `server.repeat_unchanged.<300` | crates/lumen-core/tests/ranked_coverage.rs (122 lines, bucket <300) | 200 | 1.58 ms | 1.65 ms | 1.88 ms | 1.62 ms | 1.70 ms | 2.18 ms |
| `server.repeat_changed.<300` | crates/lumen-core/tests/ranked_coverage.rs (122 lines, bucket <300) | 200 | 1.62 ms | 1.67 ms | 1.83 ms | 1.65 ms | 1.73 ms | 2.65 ms |
| `server.repeat_unchanged.300-999` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | 4.55 ms | 4.66 ms | 4.99 ms | 4.67 ms | 5.51 ms | 6.90 ms |
| `server.repeat_changed.300-999` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | 4.42 ms | 4.61 ms | 6.17 ms | 4.64 ms | 4.91 ms | 6.96 ms |
| `server.repeat_unchanged.1000-2999` | crates/lumen-core/tests/silent_fallbacks.rs (1472 lines, bucket 1000-2999) | 200 | 9.59 ms | 9.92 ms | 11.90 ms | 9.62 ms | 10.36 ms | 13.46 ms |
| `server.repeat_changed.1000-2999` | crates/lumen-core/tests/silent_fallbacks.rs (1472 lines, bucket 1000-2999) | 200 | 9.37 ms | 9.86 ms | 11.11 ms | 9.46 ms | 10.72 ms | 12.63 ms |
| `server.identity.placed` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | 3.81 ms | 4.01 ms | 5.34 ms | 3.88 ms | 4.21 ms | 5.26 ms |
| `server.identity.unplaced` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | 3.72 ms | 3.90 ms | 4.81 ms | 3.77 ms | 4.03 ms | 5.31 ms |

Notes on series that are not plain measurements:

- `hook.intercept_pass.inproc` on v1.9.0: n/a: the harness links this checkout's hook library, so it can time in-process only a binary built from the checkout; the comparable figure for any other build is spawn minus floor
- `hook.intercept_block.inproc` on v1.9.0: n/a: the harness links this checkout's hook library, so it can time in-process only a binary built from the checkout; the comparable figure for any other build is spawn minus floor
- `hook.meter_read.inproc` on v1.9.0: n/a: the harness links this checkout's hook library, so it can time in-process only a binary built from the checkout; the comparable figure for any other build is spawn minus floor
- `hook.meter_bash.inproc` on v1.9.0: n/a: the harness links this checkout's hook library, so it can time in-process only a binary built from the checkout; the comparable figure for any other build is spawn minus floor

Load on the machine around the whole latency run (every build, every pass):

- before: CPU 2% busy (instantaneous); after: CPU 1% busy (instantaneous)

## Dollars

None: A saving's dollar value is priced at the model of the turn that read the call's result, over the rounds that followed it, and less the extra round the call cost: the savings engine derives these from a ledger's turns. A corpus run has no turns, so this document gives tokens only. Engine: lumen_core::savings (crates/lumen-core/src/savings). Calibration: Token figures are Claude's tokens, estimated from the cl100k_base counts Lumen records (OpenAI's tokenizer: Claude's is not available offline), by the calibration of 2026-10-14, measured through Claude Code 2.1.296 against Claude's own usage data on 60 samples of raw file text. Each count is converted by its file's type and the model of the turn that read it, at the median ratio for them, with the smallest to the largest ratio any sample came out at as its band (not the 10th to 90th percentile: the floor is the band's low end, and must hold call by call); a row with no measured model takes the type's ratio over both tokenizers, weighted alike, with a band that spans them. The ratios are of raw file text: an answer Lumen makes from a file (an outline, a slice, a stub), a Read's line numbers and the tool-result wrapper around any answer, a Read's or Lumen's, are in no row. So an answer Lumen made takes the file's median, with the smallest to the largest ratio of any sample the model was sent as its band (of any sample at all where the model is not known). The dollar floor takes the low end of each saving: the file at its smallest ratio less Lumen's answer at its largest.
