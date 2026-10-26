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

- OS: Ubuntu 24.04.5 LTS (linux x86_64)
- CPU: Intel(R) Xeon(R) Platinum 8370C CPU @ 2.80GHz, 4 logical cores
- RAM: 15.6 GiB
- Toolchain: rustc 1.99.1 (58cf8f9c3 2026-10-13) / cargo 1.99.1 (5f94df478 2026-08-27)
- Started: 2026-10-26T12:17:05Z
- Load at start: load average 3.77 1.66 0.64 (1, 5, 15 min); at end: load average 1.44 1.47 0.70 (1, 5, 15 min)

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
| `hook.floor` | - | 200 | 729 us | 776 us | 882 us | 736 us | 817 us | 878 us |
| `hook.intercept_pass.spawn` | crates/lumen-core/tests/ranked_coverage.rs (122 lines, bucket <300) | 200 | 767 us | 828 us | 904 us | 772 us | 830 us | 873 us |
| `hook.intercept_block.spawn` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | 1.21 ms | 1.26 ms | 1.32 ms | 1.25 ms | 1.31 ms | 1.37 ms |
| `hook.meter_read.spawn` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | 43.61 ms | 45.60 ms | 49.72 ms | 44.78 ms | 46.68 ms | 48.71 ms |
| `hook.meter_bash.spawn` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | 42.06 ms | 44.09 ms | 46.41 ms | 43.27 ms | 44.70 ms | 46.59 ms |
| `hook.per_call_identity` | - | 200 | 823 us | 872 us | 953 us | 842 us | 908 us | 985 us |

### Hooks, work only: `hook::run_with` called in-process, warm (only for a build made from this checkout)

No process start, a warm tokenizer. A fresh hook process pays tokenizer start-up that these calls do not, which is part of the spawn figures above.

| series | subject | n | v1.9.0 median | v1.9.0 p90 | v1.9.0 p99 | pre-93eb224 median | pre-93eb224 p90 | pre-93eb224 p99 |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| `hook.intercept_pass.inproc` | crates/lumen-core/tests/ranked_coverage.rs (122 lines, bucket <300) | 200 | n/a (see notes) | | | 11 us | 11 us | 20 us |
| `hook.intercept_block.inproc` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | n/a (see notes) | | | 274 us | 293 us | 360 us |
| `hook.meter_read.inproc` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | n/a (see notes) | | | 2.75 ms | 2.89 ms | 3.10 ms |
| `hook.meter_bash.inproc` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | n/a (see notes) | | | 1.33 ms | 1.47 ms | 1.61 ms |

### Derived: hook work above the process floor

`median(spawn) - median(floor)`, in each build. A derived figure, not a series of its own.

| hook | v1.9.0 | pre-93eb224 |
|---|---:|---:|
| `hook.intercept_pass.spawn_minus_floor` | 38 us | 37 us |
| `hook.intercept_block.spawn_minus_floor` | 482 us | 514 us |
| `hook.meter_read.spawn_minus_floor` | 42.88 ms | 44.05 ms |
| `hook.meter_bash.spawn_minus_floor` | 41.34 ms | 42.53 ms |
| `hook.per_call_identity.spawn_minus_floor` | 94 us | 107 us |
| `hook.per_call_identity.server_claim` | 44 us | 67 us |

### Server calls, round trip as a client sees it

From just before the request is written to the arrival of the reply line. `server.ping` is the round trip with no work; the ledger row is written after the reply and is not in the interval. For a build with per-call identity every call in these series also carries the hook's record and `_meta` id, so the server's claim of it is inside the interval; `server.identity.placed` against `server.identity.unplaced` (the same call with no record and no id) isolates that claim. Only a build with per-call identity has those two series.

| series | subject | n | v1.9.0 median | v1.9.0 p90 | v1.9.0 p99 | pre-93eb224 median | pre-93eb224 p90 | pre-93eb224 p99 |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| `server.start` | - | 200 | 747 us | 802 us | 907 us | 756 us | 809 us | 855 us |
| `server.ping` | - | 200 | 32 us | 38 us | 57 us | 29 us | 34 us | 49 us |
| `server.smart_read.<300` | crates/lumen-core/tests/ranked_coverage.rs (122 lines, bucket <300) | 200 | 1.20 ms | 1.24 ms | 1.30 ms | 1.21 ms | 1.23 ms | 1.29 ms |
| `server.recall_file.<300` | crates/lumen-core/tests/ranked_coverage.rs (122 lines, bucket <300) | 200 | 1.24 ms | 1.27 ms | 1.30 ms | 1.25 ms | 1.27 ms | 1.29 ms |
| `server.smart_read.300-999` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | 4.53 ms | 4.56 ms | 4.60 ms | 4.55 ms | 4.61 ms | 4.67 ms |
| `server.recall_file.300-999` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | 4.38 ms | 4.42 ms | 4.47 ms | 4.39 ms | 4.42 ms | 4.50 ms |
| `server.smart_read.1000-2999` | crates/lumen-core/tests/silent_fallbacks.rs (1472 lines, bucket 1000-2999) | 200 | 10.71 ms | 10.81 ms | 19.23 ms | 10.73 ms | 10.82 ms | 12.98 ms |
| `server.recall_file.1000-2999` | crates/lumen-core/tests/silent_fallbacks.rs (1472 lines, bucket 1000-2999) | 200 | 10.56 ms | 10.66 ms | 10.87 ms | 10.54 ms | 10.63 ms | 11.64 ms |
| `server.repeat_unchanged.<300` | crates/lumen-core/tests/ranked_coverage.rs (122 lines, bucket <300) | 200 | 1.56 ms | 1.59 ms | 1.61 ms | 1.58 ms | 1.61 ms | 1.65 ms |
| `server.repeat_changed.<300` | crates/lumen-core/tests/ranked_coverage.rs (122 lines, bucket <300) | 200 | 1.59 ms | 1.63 ms | 1.69 ms | 1.60 ms | 1.63 ms | 1.65 ms |
| `server.repeat_unchanged.300-999` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | 5.05 ms | 5.10 ms | 5.18 ms | 5.08 ms | 5.12 ms | 5.27 ms |
| `server.repeat_changed.300-999` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | 5.10 ms | 5.16 ms | 5.36 ms | 5.12 ms | 5.18 ms | 5.26 ms |
| `server.repeat_unchanged.1000-2999` | crates/lumen-core/tests/silent_fallbacks.rs (1472 lines, bucket 1000-2999) | 200 | 11.54 ms | 11.64 ms | 11.73 ms | 11.57 ms | 11.68 ms | 12.19 ms |
| `server.repeat_changed.1000-2999` | crates/lumen-core/tests/silent_fallbacks.rs (1472 lines, bucket 1000-2999) | 200 | 11.61 ms | 11.72 ms | 12.57 ms | 11.64 ms | 11.77 ms | 12.10 ms |
| `server.identity.placed` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | 4.52 ms | 4.57 ms | 4.64 ms | 4.55 ms | 4.59 ms | 5.36 ms |
| `server.identity.unplaced` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | 4.47 ms | 4.53 ms | 4.58 ms | 4.48 ms | 4.52 ms | 4.70 ms |

Notes on series that are not plain measurements:

- `hook.intercept_pass.inproc` on v1.9.0: n/a: the harness links this checkout's hook library, so it can time in-process only a binary built from the checkout; the comparable figure for any other build is spawn minus floor
- `hook.intercept_block.inproc` on v1.9.0: n/a: the harness links this checkout's hook library, so it can time in-process only a binary built from the checkout; the comparable figure for any other build is spawn minus floor
- `hook.meter_read.inproc` on v1.9.0: n/a: the harness links this checkout's hook library, so it can time in-process only a binary built from the checkout; the comparable figure for any other build is spawn minus floor
- `hook.meter_bash.inproc` on v1.9.0: n/a: the harness links this checkout's hook library, so it can time in-process only a binary built from the checkout; the comparable figure for any other build is spawn minus floor

Load on the machine around the whole latency run (every build, every pass):

- before: load average 3.77 1.66 0.64 (1, 5, 15 min); after: load average 1.44 1.47 0.70 (1, 5, 15 min)

## Dollars

None: A saving's dollar value is priced at the model of the turn that read the call's result, over the rounds that followed it, and less the extra round the call cost: the savings engine derives these from a ledger's turns. A corpus run has no turns, so this document gives tokens only. Engine: lumen_core::savings (crates/lumen-core/src/savings). Calibration: Token figures are Claude's tokens, estimated from the cl100k_base counts Lumen records (OpenAI's tokenizer: Claude's is not available offline), by the calibration of 2026-10-14, measured through Claude Code 2.1.296 against Claude's own usage data on 60 samples of raw file text. Each count is converted by its file's type and the model of the turn that read it, at the median ratio for them, with the smallest to the largest ratio any sample came out at as its band (not the 10th to 90th percentile: the floor is the band's low end, and must hold call by call); a row with no measured model takes the type's ratio over both tokenizers, weighted alike, with a band that spans them. The ratios are of raw file text: an answer Lumen makes from a file (an outline, a slice, a stub), a Read's line numbers and the tool-result wrapper around any answer, a Read's or Lumen's, are in no row. So an answer Lumen made takes the file's median, with the smallest to the largest ratio of any sample the model was sent as its band (of any sample at all where the model is not known). The dollar floor takes the low end of each saving: the file at its smallest ratio less Lumen's answer at its largest.
