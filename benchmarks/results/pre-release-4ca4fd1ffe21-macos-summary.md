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

- OS: macOS 26.6.2 (25G83) (macos aarch64)
- CPU: Apple M4, 10 logical cores, 10 physical (4 performance + 6 efficiency)
- RAM: 24.0 GiB
- Power: Now drawing from 'AC Power'
- Toolchain: rustc 1.98.1 (48a229cea 2026-09-01) / cargo 1.98.1 (797e8a9bc 2026-08-05)
- Started: 2026-10-13T11:18:42Z
- Load at start: load average 3.75 5.46 6.33 (1, 5, 15 min); at end: load average 4.89 5.24 6.07 (1, 5, 15 min)

## Corpus

| repository | license | commit | measured files | by bucket |
|---|---|---|---:|---|
| [lumen](https://github.com/HackPoint/lumen) | MIT | `3b951e93dbe55e46dc0e15951666b303110861e7` | 96 | <300 53, 300-999 31, 1000-2999 11, 3000+ 1 |
| [react-hook-form](https://github.com/react-hook-form/react-hook-form) | MIT | `8ac7cc0bcc1f50f887230d50b1929d977b4d3fb4` | 413 | <300 357, 300-999 39, 1000-2999 14, 3000+ 3 |
| [pytest](https://github.com/pytest-dev/pytest) | MIT | `cf470ec0bf7eb89cd97dd56df4859eae5db46447` | 270 | <300 185, 300-999 46, 1000-2999 36, 3000+ 3 |

## Tokens per call (MEASURED)

Median and p90 over files, of the tokens the call returned. `full` is the file's own tokens (what a plain Read puts in context), over every file of the group; `n` is the files the mode applies to (a file with no named item has no recall). The dollar column is MODELLED: the median tokens saved by the call, times the model's value of one saved token. It does not include the one extra round an intercepted Read costs (below).

### By file-size bucket

**outline: `smart_read(path)`, the first read**

| bucket (lines) | n | full med | full p90 | v1.7.0 med | v1.7.0 p90 | v1.7.0 modelled $/call | pre-4ca4fd1 med | pre-4ca4fd1 p90 | pre-4ca4fd1 modelled $/call |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| <300 | 595 | 277 | 1360 | 137 | 244 | $0.0048 | 137 | 244 | $0.0048 |
| 300-999 | 116 | 3947 | 7359 | 307 | 694 | $0.1386 | 307 | 694 | $0.1386 |
| 1000-2999 | 61 | 12609 | 18363 | 767 | 1249 | $0.4547 | 767 | 1249 | $0.4547 |
| 3000+ | 7 | 28012 | 43185 | 1198 | 1734 | $1.0254 | 1198 | 1734 | $1.0254 |

**recall: `recall_file(path, [one representative item])`**

| bucket (lines) | n | full med | full p90 | v1.7.0 med | v1.7.0 p90 | v1.7.0 modelled $/call | pre-4ca4fd1 med | pre-4ca4fd1 p90 | pre-4ca4fd1 modelled $/call |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| <300 | 334 | 277 | 1360 | 97 | 246 | $0.0096 | 97 | 246 | $0.0096 |
| 300-999 | 76 | 3947 | 7359 | 228 | 431 | $0.1435 | 228 | 431 | $0.1435 |
| 1000-2999 | 53 | 12609 | 18363 | 284 | 400 | $0.4879 | 284 | 400 | $0.4879 |
| 3000+ | 6 | 28012 | 43185 | 219 | 479 | $1.0748 | 219 | 479 | $1.0748 |

**repeat-read stub: the second identical `smart_read`, repeat reads on**

| bucket (lines) | n | full med | full p90 | v1.7.0 med | v1.7.0 p90 | v1.7.0 modelled $/call | pre-4ca4fd1 med | pre-4ca4fd1 p90 | pre-4ca4fd1 modelled $/call |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| <300 | 595 | 277 | 1360 | 30 | 30 | $0.0096 | 30 | 30 | $0.0096 |
| 300-999 | 116 | 3947 | 7359 | 30 | 30 | $0.1518 | 30 | 30 | $0.1518 |
| 1000-2999 | 61 | 12609 | 18363 | 30 | 30 | $0.4874 | 30 | 30 | $0.4874 |
| 3000+ | 7 | 28012 | 43185 | 30 | 30 | $1.0843 | 30 | 30 | $1.0843 |

**repeat-read delta: `smart_read` after one small edit to the file, repeat reads on**

| bucket (lines) | n | full med | full p90 | v1.7.0 med | v1.7.0 p90 | v1.7.0 modelled $/call | pre-4ca4fd1 med | pre-4ca4fd1 p90 | pre-4ca4fd1 modelled $/call |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| <300 | 300 | 277 | 1360 | 43 | 96 | $0.0144 | 43 | 96 | $0.0144 |
| 300-999 | 74 | 3947 | 7359 | 44 | 51 | $0.1531 | 44 | 51 | $0.1531 |
| 1000-2999 | 53 | 12609 | 18363 | 46 | 50 | $0.4957 | 46 | 50 | $0.4957 |
| 3000+ | 6 | 28012 | 43185 | 47 | 54 | $1.0836 | 47 | 54 | $1.0836 |


### By language

**outline: `smart_read(path)`, the first read**

| language | n | full med | full p90 | v1.7.0 med | v1.7.0 p90 | v1.7.0 modelled $/call | pre-4ca4fd1 med | pre-4ca4fd1 p90 | pre-4ca4fd1 modelled $/call |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| rust | 58 | 3809 | 17615 | 320 | 838 | $0.1343 | 320 | 838 | $0.1343 |
| python | 274 | 655 | 10796 | 195 | 864 | $0.0159 | 195 | 864 | $0.0159 |
| typescript | 270 | 277 | 1995 | 119 | 208 | $0.0046 | 119 | 208 | $0.0046 |
| tsx | 177 | 461 | 4787 | 164 | 245 | $0.0107 | 164 | 245 | $0.0107 |

**recall: `recall_file(path, [one representative item])`**

| language | n | full med | full p90 | v1.7.0 med | v1.7.0 p90 | v1.7.0 modelled $/call | pre-4ca4fd1 med | pre-4ca4fd1 p90 | pre-4ca4fd1 modelled $/call |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| rust | 57 | 3809 | 17615 | 234 | 370 | $0.1435 | 234 | 370 | $0.1435 |
| python | 223 | 655 | 10796 | 184 | 361 | $0.0386 | 184 | 361 | $0.0386 |
| typescript | 59 | 277 | 1995 | 86 | 172 | $0.0084 | 86 | 172 | $0.0084 |
| tsx | 130 | 461 | 4787 | 80 | 228 | $0.0105 | 80 | 228 | $0.0105 |

**repeat-read stub: the second identical `smart_read`, repeat reads on**

| language | n | full med | full p90 | v1.7.0 med | v1.7.0 p90 | v1.7.0 modelled $/call | pre-4ca4fd1 med | pre-4ca4fd1 p90 | pre-4ca4fd1 modelled $/call |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| rust | 58 | 3809 | 17615 | 30 | 30 | $0.1464 | 30 | 30 | $0.1464 |
| python | 274 | 655 | 10796 | 30 | 40 | $0.0242 | 30 | 40 | $0.0242 |
| typescript | 270 | 277 | 1995 | 30 | 30 | $0.0096 | 30 | 30 | $0.0096 |
| tsx | 177 | 461 | 4787 | 30 | 30 | $0.0167 | 30 | 30 | $0.0167 |

**repeat-read delta: `smart_read` after one small edit to the file, repeat reads on**

| language | n | full med | full p90 | v1.7.0 med | v1.7.0 p90 | v1.7.0 modelled $/call | pre-4ca4fd1 med | pre-4ca4fd1 p90 | pre-4ca4fd1 modelled $/call |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| rust | 57 | 3809 | 17615 | 44 | 55 | $0.1557 | 44 | 55 | $0.1557 |
| python | 188 | 655 | 10796 | 45 | 96 | $0.0658 | 45 | 96 | $0.0658 |
| typescript | 58 | 277 | 1995 | 44 | 142 | $0.0131 | 44 | 142 | $0.0131 |
| tsx | 130 | 461 | 4787 | 43 | 44 | $0.0134 | 43 | 44 | $0.0134 |

### Routes

How the server answered, from its own ledger. A route other than the mode's own is a fall-back the benchmark reports rather than hides: `would_inflate` is the guard returning the file because an outline would not be smaller; `unchanged_not_smaller`, `delta_too_big` and `no_base` are the repeat-read gate declining.

| bucket | mode | v1.7.0 routes | pre-4ca4fd1 routes |
|---|---|---|---|
| <300 | outline | ranked_no_defs 19, smart_read 398, would_inflate 178 | ranked_no_defs 19, smart_read 398, would_inflate 178 |
| <300 | recall | recall_file 291, would_inflate 43 | recall_file 291, would_inflate 43 |
| <300 | stub | ranked_no_defs/unchanged 19, smart_read_unchanged/unchanged 535, would_inflate/unchanged_not_smaller 41 | ranked_no_defs/unchanged 19, smart_read_unchanged/unchanged 535, would_inflate/unchanged_not_smaller 41 |
| <300 | delta | smart_read/no_base 1, smart_read_delta/delta 256, would_inflate/no_base 43 | smart_read/no_base 1, smart_read_delta/delta 256, would_inflate/no_base 43 |
| 300-999 | outline | smart_read 116 | smart_read 116 |
| 300-999 | recall | recall_file 76 | recall_file 76 |
| 300-999 | stub | smart_read_unchanged/unchanged 116 | smart_read_unchanged/unchanged 116 |
| 300-999 | delta | smart_read_delta/delta 74 | smart_read_delta/delta 74 |
| 1000-2999 | outline | smart_read 61 | smart_read 61 |
| 1000-2999 | recall | recall_file 53 | recall_file 53 |
| 1000-2999 | stub | smart_read_unchanged/unchanged 61 | smart_read_unchanged/unchanged 61 |
| 1000-2999 | delta | smart_read_delta/delta 53 | smart_read_delta/delta 53 |
| 3000+ | outline | smart_read 7 | smart_read 7 |
| 3000+ | recall | recall_file 6 | recall_file 6 |
| 3000+ | stub | smart_read_unchanged/unchanged 7 | smart_read_unchanged/unchanged 7 |
| 3000+ | delta | smart_read_delta/delta 6 | smart_read_delta/delta 6 |

Token table digests (SHA-256 of the per-file rows of `*-tokens.tsv` with the build column blanked, so a digest does not depend on a label; equal digests mean equal token figures): v1.7.0 `2d99da988a0f0bbb6dee245e26303f3dfcf525dd935d1cf86d60ef6c5c14abc7`; pre-4ca4fd1 `2d99da988a0f0bbb6dee245e26303f3dfcf525dd935d1cf86d60ef6c5c14abc7`

## Latency (MEASURED)

200 recorded runs per series: 5 passes of 40, pooled. In each pass the builds take turns (the order flips every pass), and each series first has 20 warm-up runs that are discarded (per series per pass, run the same way as the recorded runs, so caches, page tables and the ledger file are warm; hook spawns are warm-start, not first launch after boot). Clock: std::time::Instant, a monotonic clock, nanosecond resolution. Release builds, built here. Medians vary with the machine and its load, and this document makes no claim about how closely another run will land. What this run shows about itself: in 43 of 45 series, the median of every one of the 5 passes is within 20% of the series' pooled median (per-pass medians are in the JSON). `benchmarks/run.sh compare <this file>` re-measures on another machine and prints, per series, the ratio to this run, the same ratio after dividing by the process floor, how many series are within 20%, and whether that machine looks like this one.

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
| `hook.floor` | - | 200 | 2.99 ms | 3.47 ms | 7.40 ms | 2.87 ms | 3.18 ms | 3.47 ms |
| `hook.intercept_pass.spawn` | crates/lumen-core/tests/ranked_coverage.rs (122 lines, bucket <300) | 200 | 3.06 ms | 3.32 ms | 3.64 ms | 3.04 ms | 3.29 ms | 3.52 ms |
| `hook.intercept_block.spawn` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | 2.90 ms | 3.17 ms | 4.31 ms | 5.53 ms | 5.92 ms | 6.50 ms |
| `hook.meter_read.spawn` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | 33.93 ms | 34.78 ms | 38.11 ms | 34.03 ms | 34.77 ms | 36.64 ms |
| `hook.meter_bash.spawn` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | 33.33 ms | 33.99 ms | 36.79 ms | 33.31 ms | 34.13 ms | 53.98 ms |
| `hook.per_call_identity` | - | 200 | n/a (see notes) | | | 4.84 ms | 5.35 ms | 11.63 ms |

Note: `hook.intercept_block.spawn` is 2.63 ms slower on pre-4ca4fd1 than on v1.7.0 (5.53 ms against 2.90 ms, medians as measured on this machine in this run). This run times the hook, it does not trace it, so it does not say why. To find the cause, read what changed in the blocking path of `hook intercept` between the two commits (`git log 5d1ec0408ad2..4ca4fd1ffe21 -- crates/lumen-mcp crates/lumen-core`). The hook that lets a Read through (`hook.intercept_pass.spawn`) is 3.04 ms on pre-4ca4fd1 and 3.06 ms on v1.7.0, which is no slower, so the extra time is in the blocking path rather than in starting the process.

### Hooks, work only: `hook::run_with` called in-process, warm (only for a build made from this checkout)

No process start, a warm tokenizer. A fresh hook process pays tokenizer start-up that these calls do not, which is part of the spawn figures above.

| series | subject | n | v1.7.0 median | v1.7.0 p90 | v1.7.0 p99 | pre-4ca4fd1 median | pre-4ca4fd1 p90 | pre-4ca4fd1 p99 |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| `hook.intercept_pass.inproc` | crates/lumen-core/tests/ranked_coverage.rs (122 lines, bucket <300) | 200 | n/a (see notes) | | | 59 us | 67 us | 95 us |
| `hook.intercept_block.inproc` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | n/a (see notes) | | | 2.51 ms | 3.16 ms | 5.08 ms |
| `hook.meter_read.inproc` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | n/a (see notes) | | | 4.64 ms | 5.57 ms | 6.15 ms |
| `hook.meter_bash.inproc` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | n/a (see notes) | | | 4.71 ms | 5.58 ms | 7.02 ms |

### Derived: hook work above the process floor

`median(spawn) - median(floor)`, in each build. A derived figure, not a series of its own.

| hook | v1.7.0 | pre-4ca4fd1 |
|---|---:|---:|
| `hook.intercept_pass.spawn_minus_floor` | 68 us | 162 us |
| `hook.intercept_block.spawn_minus_floor` | -96 us | 2.66 ms |
| `hook.meter_read.spawn_minus_floor` | 30.93 ms | 31.15 ms |
| `hook.meter_bash.spawn_minus_floor` | 30.33 ms | 30.43 ms |
| `hook.per_call_identity.spawn_minus_floor` | n/a | 1.97 ms |
| `hook.per_call_identity.server_claim` | n/a | 178 us |

### Server calls, round trip as a client sees it

From just before the request is written to the arrival of the reply line. `server.ping` is the round trip with no work; the ledger row is written after the reply and is not in the interval. For a build with per-call identity every call in these series also carries the hook's record and `_meta` id, so the server's claim of it is inside the interval; `server.identity.placed` against `server.identity.unplaced` (the same call with no record and no id) isolates that claim. Only a build with per-call identity has those two series.

| series | subject | n | v1.7.0 median | v1.7.0 p90 | v1.7.0 p99 | pre-4ca4fd1 median | pre-4ca4fd1 p90 | pre-4ca4fd1 p99 |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| `server.start` | - | 200 | 2.67 ms | 2.98 ms | 3.39 ms | 2.88 ms | 3.18 ms | 3.90 ms |
| `server.ping` | - | 200 | 12 us | 13 us | 25 us | 11 us | 13 us | 16 us |
| `server.smart_read.<300` | crates/lumen-core/tests/ranked_coverage.rs (122 lines, bucket <300) | 200 | 658 us | 853 us | 1.19 ms | 926 us | 1.09 ms | 1.57 ms |
| `server.recall_file.<300` | crates/lumen-core/tests/ranked_coverage.rs (122 lines, bucket <300) | 200 | 668 us | 871 us | 1.23 ms | 957 us | 1.25 ms | 1.49 ms |
| `server.smart_read.300-999` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | 2.45 ms | 2.90 ms | 3.09 ms | 2.75 ms | 3.27 ms | 3.51 ms |
| `server.recall_file.300-999` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | 2.34 ms | 2.72 ms | 2.90 ms | 2.63 ms | 3.15 ms | 6.41 ms |
| `server.smart_read.1000-2999` | crates/lumen-core/tests/silent_fallbacks.rs (1472 lines, bucket 1000-2999) | 200 | 5.65 ms | 6.12 ms | 6.38 ms | 6.42 ms | 6.57 ms | 7.24 ms |
| `server.recall_file.1000-2999` | crates/lumen-core/tests/silent_fallbacks.rs (1472 lines, bucket 1000-2999) | 200 | 5.66 ms | 6.24 ms | 6.76 ms | 6.32 ms | 6.48 ms | 7.09 ms |
| `server.repeat_unchanged.<300` | crates/lumen-core/tests/ranked_coverage.rs (122 lines, bucket <300) | 200 | 3.38 ms | 3.96 ms | 5.07 ms | 2.93 ms | 3.33 ms | 3.58 ms |
| `server.repeat_changed.<300` | crates/lumen-core/tests/ranked_coverage.rs (122 lines, bucket <300) | 200 | 4.57 ms | 5.47 ms | 9.01 ms | 3.34 ms | 3.73 ms | 5.11 ms |
| `server.repeat_unchanged.300-999` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | 4.71 ms | 5.20 ms | 5.49 ms | 4.98 ms | 5.62 ms | 16.97 ms |
| `server.repeat_changed.300-999` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | 7.88 ms | 8.92 ms | 10.84 ms | 5.31 ms | 6.02 ms | 6.43 ms |
| `server.repeat_unchanged.1000-2999` | crates/lumen-core/tests/silent_fallbacks.rs (1472 lines, bucket 1000-2999) | 200 | 8.60 ms | 9.23 ms | 16.37 ms | 8.92 ms | 9.60 ms | 10.18 ms |
| `server.repeat_changed.1000-2999` | crates/lumen-core/tests/silent_fallbacks.rs (1472 lines, bucket 1000-2999) | 200 | 13.04 ms | 14.13 ms | 17.29 ms | 9.34 ms | 9.98 ms | 11.09 ms |
| `server.identity.placed` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | - | | | 2.71 ms | 3.26 ms | 4.54 ms |
| `server.identity.unplaced` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | - | | | 2.53 ms | 3.08 ms | 4.62 ms |

Notes on series that are not plain measurements:

- `hook.intercept_pass.inproc` on v1.7.0: n/a: the harness links this checkout's hook library, so it can time in-process only a binary built from the checkout; the comparable figure for any other build is spawn minus floor
- `hook.intercept_block.inproc` on v1.7.0: n/a: the harness links this checkout's hook library, so it can time in-process only a binary built from the checkout; the comparable figure for any other build is spawn minus floor
- `hook.meter_read.inproc` on v1.7.0: n/a: the harness links this checkout's hook library, so it can time in-process only a binary built from the checkout; the comparable figure for any other build is spawn minus floor
- `hook.meter_bash.inproc` on v1.7.0: n/a: the harness links this checkout's hook library, so it can time in-process only a binary built from the checkout; the comparable figure for any other build is spawn minus floor
- `hook.per_call_identity` on v1.7.0: n/a — this build records no per-call identity (a PreToolUse payload for mcp__lumen__smart_read, sent to `hook intercept`, left no record beside the ledger; commit 5d1ec0408ad2)

Load on the machine around the whole latency run (every build, every pass):

- before: load average 4.99 5.50 6.29 (1, 5, 15 min); after: load average 4.89 5.24 6.07 (1, 5, 15 min)

## Dollars (MODELLED)

Every dollar figure above is `modelled`: a model's arithmetic on measured token counts, not a measured cost.

- Source: crates/lumen-core/src/econ.rs (Econ::default) and crates/lumen-core/src/rates.rs
- Rates, USD per million tokens: input 5, output 25, cache read 0.5, cache write 6.25. rates.rs labels these Sonnet 3.5 / 3.7 / 4 pricing. The harness does not check them against a current price list; they are the model's inputs, not a quote.
- Inputs: context 362965 tokens, output 1085 tokens, 65 rounds remaining. measured_defaults: means over 19,161 main-agent turns of the Lumen author's own sessions (econ.rs); another user's profile differs
- Value of one saved token: $0.00003875 (modelled). One extra round for an intercepted Read: $0.2086075 (modelled), which 5383 saved tokens pay for.
- Formula: value of one saved token = (cache_write + cache_read x rounds_remaining) / 1e6 dollars; one intercepted Read costs one extra round = (context x cache_read + output x output_rate) / 1e6 dollars; a dollar figure below is the median saved tokens of a call x the value of one token
