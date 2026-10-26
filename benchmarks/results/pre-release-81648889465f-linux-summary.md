# Lumen benchmarks: pre-release-81648889465f

> **Pre-release measurement.** This is the harness run on commit `81648889465f`, before the 1.8.0 release. It is not a 1.8.0 result and must not be quoted as one.

Per-call figures, per file-size bucket and per language. There is no overall "percent saved" here on purpose: how much a person saves overall depends on their usage mix (how many reads are large, how many repeat).

- **MEASURED**: token counts (real `cl100k_base` tokenizer) and latencies (monotonic clock).
- **No dollar figure** (see the end).
- Tokenizer: `cl100k_base` is a proxy for Claude's tokenizer. cl100k_base is not Claude's tokenizer. Counts compare modes with one another; they are not a bill and not the number of tokens Claude would charge.

## Builds

| label | role | commit | profile | per-call identity | reports version |
|---|---|---|---|---|---|
| v1.9.0 | baseline | `cbb8d0fe9d18acb9d7ca1b839dab528fbcb522e7` | release, cargo's default: the root Cargo.toml has no [profile.release] | a hook record per call (PreToolUse on the Lumen tool), claimed by the server through _meta claudecode/toolUseId | 1.9.0 |
| pre-8164888 | candidate | `81648889465f52071abdc01141e78a9ff4d7a6ee` | release, cargo's default: the root Cargo.toml has no [profile.release] | a hook record per call (PreToolUse on the Lumen tool), claimed by the server through _meta claudecode/toolUseId | 1.10.0 |

## Machine

- OS: Ubuntu 24.04.5 LTS (linux x86_64)
- CPU: AMD EPYC 9V45 96-Core Processor, 4 logical cores
- RAM: 15.6 GiB
- Toolchain: rustc 1.99.1 (58cf8f9c3 2026-10-13) / cargo 1.99.1 (5f94df478 2026-08-27)
- Started: 2026-10-26T12:00:48Z
- Load at start: load average 3.77 1.42 0.52 (1, 5, 15 min); at end: load average 1.63 1.36 0.60 (1, 5, 15 min)

## Corpus

| repository | license | commit | measured files | by bucket |
|---|---|---|---:|---|
| [lumen](https://github.com/HackPoint/lumen) | MIT | `3b951e93dbe55e46dc0e15951666b303110861e7` | 96 | <300 53, 300-999 31, 1000-2999 11, 3000+ 1 |

## Latency (MEASURED)

200 recorded runs per series: 5 passes of 40, pooled. In each pass the builds take turns (the order flips every pass), and each series first has 20 warm-up runs that are discarded (per series per pass, run the same way as the recorded runs, so caches, page tables and the ledger file are warm; hook spawns are warm-start, not first launch after boot). Clock: std::time::Instant, a monotonic clock, nanosecond resolution. Release builds, built here. Medians vary with the machine and its load, and this document makes no claim about how closely another run will land. What this run shows about itself: in 44 of 48 series, the median of every one of the 5 passes is within 20% of the series' pooled median (per-pass medians are in the JSON). `benchmarks/run.sh compare <this file>` re-measures on another machine and prints, per series, the ratio to this run, the same ratio after dividing by the process floor, how many series are within 20%, and whether that machine looks like this one.

Percentiles are nearest-rank: the percentile p of n runs is the run at rank ceil(p x n / 100) of the sorted runs, fastest first, with no interpolation. With n = 200 the median is run 100 (of the two middle runs of an even count, the faster), p90 is run 180, and p99 is run 198, which leaves the slowest 2 runs above it. Raw samples: not written. Each series carries `pass_medians_ns`, the median of each pass on its own, in pass order: how far they sit from one another is how much the machine moved while the run was going on.

Subjects (corpus files the series run on):

| bucket | file | lines | bytes | recall item |
|---|---|---:|---:|---|
| <300 | crates/lumen-core/tests/ranked_coverage.rs | 122 | 3754 | `walk` |
| 300-999 | crates/lumen-mcp/tests/protocol.rs | 532 | 17672 | `initialize_returns_a_well_formed_jsonrpc_response` |
| 1000-2999 | crates/lumen-core/tests/silent_fallbacks.rs | 1472 | 52110 | `rust_statement` |

### Hooks, as the harness sees them: process spawn to exit

The whole hook as Claude Code runs it. `hook.floor` is a hook invocation that does no work, so it is the cost of the process itself; the work is what each row adds to it. `hook.per_call_identity` is the PreToolUse hook on a call of one of Lumen's own MCP tools, which records who is calling; a build either has it (the series is timed) or does not (the slot says `n/a`, and why). Which one is decided by running the build, not by reading its usage text.

| series | subject | n | v1.9.0 median | v1.9.0 p90 | v1.9.0 p99 | pre-8164888 median | pre-8164888 p90 | pre-8164888 p99 |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| `hook.floor` | - | 200 | 755 us | 834 us | 899 us | 776 us | 859 us | 947 us |
| `hook.intercept_pass.spawn` | crates/lumen-core/tests/ranked_coverage.rs (122 lines, bucket <300) | 200 | 799 us | 878 us | 1.02 ms | 810 us | 901 us | 1.00 ms |
| `hook.intercept_block.spawn` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | 1.28 ms | 1.38 ms | 1.46 ms | 1.31 ms | 1.40 ms | 1.47 ms |
| `hook.meter_read.spawn` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | 39.69 ms | 46.78 ms | 52.75 ms | 35.01 ms | 43.41 ms | 53.02 ms |
| `hook.meter_bash.spawn` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | 33.55 ms | 44.32 ms | 49.06 ms | 33.62 ms | 42.48 ms | 58.59 ms |
| `hook.per_call_identity` | - | 200 | 841 us | 941 us | 1.04 ms | 896 us | 967 us | 1.13 ms |

### Hooks, work only: `hook::run_with` called in-process, warm (only for a build made from this checkout)

No process start, a warm tokenizer. A fresh hook process pays tokenizer start-up that these calls do not, which is part of the spawn figures above.

| series | subject | n | v1.9.0 median | v1.9.0 p90 | v1.9.0 p99 | pre-8164888 median | pre-8164888 p90 | pre-8164888 p99 |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| `hook.intercept_pass.inproc` | crates/lumen-core/tests/ranked_coverage.rs (122 lines, bucket <300) | 200 | n/a (see notes) | | | 9 us | 9 us | 21 us |
| `hook.intercept_block.inproc` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | n/a (see notes) | | | 248 us | 263 us | 285 us |
| `hook.meter_read.inproc` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | n/a (see notes) | | | 2.43 ms | 2.69 ms | 8.10 ms |
| `hook.meter_bash.inproc` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | n/a (see notes) | | | 1.52 ms | 2.70 ms | 5.06 ms |

### Derived: hook work above the process floor

`median(spawn) - median(floor)`, in each build. A derived figure, not a series of its own.

| hook | v1.9.0 | pre-8164888 |
|---|---:|---:|
| `hook.intercept_pass.spawn_minus_floor` | 44 us | 34 us |
| `hook.intercept_block.spawn_minus_floor` | 523 us | 535 us |
| `hook.meter_read.spawn_minus_floor` | 38.93 ms | 34.23 ms |
| `hook.meter_bash.spawn_minus_floor` | 32.80 ms | 32.84 ms |
| `hook.per_call_identity.spawn_minus_floor` | 86 us | 120 us |
| `hook.per_call_identity.server_claim` | 96 us | 114 us |

### Server calls, round trip as a client sees it

From just before the request is written to the arrival of the reply line. `server.ping` is the round trip with no work; the ledger row is written after the reply and is not in the interval. For a build with per-call identity every call in these series also carries the hook's record and `_meta` id, so the server's claim of it is inside the interval; `server.identity.placed` against `server.identity.unplaced` (the same call with no record and no id) isolates that claim. Only a build with per-call identity has those two series.

| series | subject | n | v1.9.0 median | v1.9.0 p90 | v1.9.0 p99 | pre-8164888 median | pre-8164888 p90 | pre-8164888 p99 |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| `server.start` | - | 200 | 698 us | 760 us | 864 us | 709 us | 769 us | 838 us |
| `server.ping` | - | 200 | 23 us | 26 us | 39 us | 24 us | 26 us | 45 us |
| `server.smart_read.<300` | crates/lumen-core/tests/ranked_coverage.rs (122 lines, bucket <300) | 200 | 808 us | 983 us | 1.10 ms | 902 us | 1.20 ms | 1.26 ms |
| `server.recall_file.<300` | crates/lumen-core/tests/ranked_coverage.rs (122 lines, bucket <300) | 200 | 854 us | 1.15 ms | 1.27 ms | 963 us | 1.21 ms | 1.31 ms |
| `server.smart_read.300-999` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | 2.92 ms | 3.28 ms | 3.62 ms | 3.24 ms | 3.63 ms | 3.76 ms |
| `server.recall_file.300-999` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | 2.81 ms | 3.02 ms | 3.25 ms | 3.22 ms | 3.53 ms | 3.64 ms |
| `server.smart_read.1000-2999` | crates/lumen-core/tests/silent_fallbacks.rs (1472 lines, bucket 1000-2999) | 200 | 6.99 ms | 7.70 ms | 8.18 ms | 7.62 ms | 8.09 ms | 8.39 ms |
| `server.recall_file.1000-2999` | crates/lumen-core/tests/silent_fallbacks.rs (1472 lines, bucket 1000-2999) | 200 | 6.82 ms | 7.58 ms | 8.20 ms | 7.52 ms | 7.92 ms | 9.08 ms |
| `server.repeat_unchanged.<300` | crates/lumen-core/tests/ranked_coverage.rs (122 lines, bucket <300) | 200 | 1.14 ms | 1.23 ms | 1.39 ms | 1.26 ms | 1.47 ms | 1.58 ms |
| `server.repeat_changed.<300` | crates/lumen-core/tests/ranked_coverage.rs (122 lines, bucket <300) | 200 | 1.15 ms | 1.25 ms | 1.47 ms | 1.29 ms | 1.54 ms | 2.50 ms |
| `server.repeat_unchanged.300-999` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | 3.55 ms | 3.88 ms | 4.30 ms | 3.80 ms | 4.13 ms | 4.39 ms |
| `server.repeat_changed.300-999` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | 3.62 ms | 3.94 ms | 4.41 ms | 4.05 ms | 4.35 ms | 6.56 ms |
| `server.repeat_unchanged.1000-2999` | crates/lumen-core/tests/silent_fallbacks.rs (1472 lines, bucket 1000-2999) | 200 | 7.90 ms | 9.05 ms | 9.33 ms | 8.72 ms | 9.23 ms | 12.87 ms |
| `server.repeat_changed.1000-2999` | crates/lumen-core/tests/silent_fallbacks.rs (1472 lines, bucket 1000-2999) | 200 | 8.45 ms | 9.15 ms | 9.40 ms | 8.77 ms | 9.22 ms | 9.53 ms |
| `server.identity.placed` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | 3.01 ms | 3.62 ms | 5.06 ms | 3.34 ms | 3.66 ms | 3.79 ms |
| `server.identity.unplaced` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | 2.92 ms | 3.51 ms | 4.75 ms | 3.23 ms | 3.49 ms | 3.65 ms |

Notes on series that are not plain measurements:

- `hook.intercept_pass.inproc` on v1.9.0: n/a: the harness links this checkout's hook library, so it can time in-process only a binary built from the checkout; the comparable figure for any other build is spawn minus floor
- `hook.intercept_block.inproc` on v1.9.0: n/a: the harness links this checkout's hook library, so it can time in-process only a binary built from the checkout; the comparable figure for any other build is spawn minus floor
- `hook.meter_read.inproc` on v1.9.0: n/a: the harness links this checkout's hook library, so it can time in-process only a binary built from the checkout; the comparable figure for any other build is spawn minus floor
- `hook.meter_bash.inproc` on v1.9.0: n/a: the harness links this checkout's hook library, so it can time in-process only a binary built from the checkout; the comparable figure for any other build is spawn minus floor

Load on the machine around the whole latency run (every build, every pass):

- before: load average 3.77 1.42 0.52 (1, 5, 15 min); after: load average 1.63 1.36 0.60 (1, 5, 15 min)

## Dollars

None: A saving's dollar value is priced at the model of the turn that read the call's result, over the rounds that followed it, and less the extra round the call cost: the savings engine derives these from a ledger's turns. A corpus run has no turns, so this document gives tokens only. Engine: lumen_core::savings (crates/lumen-core/src/savings). Calibration: Token figures are Claude's tokens, estimated from the cl100k_base counts Lumen records (OpenAI's tokenizer: Claude's is not available offline), by the calibration of 2026-10-14, measured through Claude Code 2.1.296 against Claude's own usage data on 60 samples of raw file text. Each count is converted by its file's type and the model of the turn that read it, at the median ratio for them, with the smallest to the largest ratio any sample came out at as its band (not the 10th to 90th percentile: the floor is the band's low end, and must hold call by call); a row with no measured model takes the type's ratio over both tokenizers, weighted alike, with a band that spans them. The ratios are of raw file text: an answer Lumen makes from a file (an outline, a slice, a stub), a Read's line numbers and the tool-result wrapper around any answer, a Read's or Lumen's, are in no row. So an answer Lumen made takes the file's median, with the smallest to the largest ratio of any sample the model was sent as its band (of any sample at all where the model is not known). The dollar floor takes the low end of each saving: the file at its smallest ratio less Lumen's answer at its largest.
