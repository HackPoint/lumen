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

- OS: macOS 26.6.2 (25G83) (macos aarch64)
- CPU: Apple M4, 10 logical cores, 10 physical (4 performance + 6 efficiency)
- RAM: 24.0 GiB
- Power: Now drawing from 'AC Power'
- Toolchain: rustc 1.98.1 (48a229cea 2026-09-01) / cargo 1.98.1 (797e8a9bc 2026-08-05)
- Started: 2026-10-26T12:22:52Z
- Load at start: load average 9.59 8.40 6.47 (1, 5, 15 min); at end: load average 6.58 6.97 6.23 (1, 5, 15 min)

## Corpus

| repository | license | commit | measured files | by bucket |
|---|---|---|---:|---|
| [lumen](https://github.com/HackPoint/lumen) | MIT | `3b951e93dbe55e46dc0e15951666b303110861e7` | 96 | <300 53, 300-999 31, 1000-2999 11, 3000+ 1 |
| [react-hook-form](https://github.com/react-hook-form/react-hook-form) | MIT | `8ac7cc0bcc1f50f887230d50b1929d977b4d3fb4` | 413 | <300 357, 300-999 39, 1000-2999 14, 3000+ 3 |
| [pytest](https://github.com/pytest-dev/pytest) | MIT | `cf470ec0bf7eb89cd97dd56df4859eae5db46447` | 270 | <300 185, 300-999 46, 1000-2999 36, 3000+ 3 |

## Tokens per call (MEASURED)

Median and p90 over files, of the tokens the call returned. `full` is the file's own tokens (what a plain Read puts in context), over every file of the group; `n` is the files the mode applies to (a file with no named item has no recall).

### By file-size bucket

**outline: `smart_read(path)`, the first read**

| bucket (lines) | n | full med | full p90 | v1.9.0 med | v1.9.0 p90 | pre-93eb224 med | pre-93eb224 p90 |
|---|---:|---:|---:|---:|---:|---:|---:|
| <300 | 595 | 277 | 1360 | 137 | 244 | 137 | 244 |
| 300-999 | 116 | 3947 | 7359 | 307 | 694 | 307 | 694 |
| 1000-2999 | 61 | 12609 | 18363 | 767 | 1249 | 767 | 1249 |
| 3000+ | 7 | 28012 | 43185 | 1198 | 1734 | 1198 | 1734 |

**recall: `recall_file(path, [one representative item])`**

| bucket (lines) | n | full med | full p90 | v1.9.0 med | v1.9.0 p90 | pre-93eb224 med | pre-93eb224 p90 |
|---|---:|---:|---:|---:|---:|---:|---:|
| <300 | 334 | 277 | 1360 | 97 | 246 | 97 | 246 |
| 300-999 | 76 | 3947 | 7359 | 228 | 431 | 228 | 431 |
| 1000-2999 | 53 | 12609 | 18363 | 284 | 400 | 284 | 400 |
| 3000+ | 6 | 28012 | 43185 | 219 | 479 | 219 | 479 |

**repeat-read stub: the second identical `smart_read`, repeat reads on**

| bucket (lines) | n | full med | full p90 | v1.9.0 med | v1.9.0 p90 | pre-93eb224 med | pre-93eb224 p90 |
|---|---:|---:|---:|---:|---:|---:|---:|
| <300 | 595 | 277 | 1360 | 30 | 30 | 30 | 30 |
| 300-999 | 116 | 3947 | 7359 | 30 | 30 | 30 | 30 |
| 1000-2999 | 61 | 12609 | 18363 | 30 | 30 | 30 | 30 |
| 3000+ | 7 | 28012 | 43185 | 30 | 30 | 30 | 30 |

**repeat-read delta: `smart_read` after one small edit to the file, repeat reads on**

| bucket (lines) | n | full med | full p90 | v1.9.0 med | v1.9.0 p90 | pre-93eb224 med | pre-93eb224 p90 |
|---|---:|---:|---:|---:|---:|---:|---:|
| <300 | 300 | 277 | 1360 | 43 | 96 | 43 | 96 |
| 300-999 | 74 | 3947 | 7359 | 44 | 51 | 44 | 51 |
| 1000-2999 | 53 | 12609 | 18363 | 46 | 50 | 46 | 50 |
| 3000+ | 6 | 28012 | 43185 | 47 | 54 | 47 | 54 |


### By language

**outline: `smart_read(path)`, the first read**

| language | n | full med | full p90 | v1.9.0 med | v1.9.0 p90 | pre-93eb224 med | pre-93eb224 p90 |
|---|---:|---:|---:|---:|---:|---:|---:|
| rust | 58 | 3809 | 17615 | 320 | 838 | 320 | 838 |
| python | 274 | 655 | 10796 | 195 | 864 | 195 | 864 |
| typescript | 270 | 277 | 1995 | 119 | 208 | 119 | 208 |
| tsx | 177 | 461 | 4787 | 164 | 245 | 164 | 245 |

**recall: `recall_file(path, [one representative item])`**

| language | n | full med | full p90 | v1.9.0 med | v1.9.0 p90 | pre-93eb224 med | pre-93eb224 p90 |
|---|---:|---:|---:|---:|---:|---:|---:|
| rust | 57 | 3809 | 17615 | 234 | 370 | 234 | 370 |
| python | 223 | 655 | 10796 | 184 | 361 | 184 | 361 |
| typescript | 59 | 277 | 1995 | 86 | 172 | 86 | 172 |
| tsx | 130 | 461 | 4787 | 80 | 228 | 80 | 228 |

**repeat-read stub: the second identical `smart_read`, repeat reads on**

| language | n | full med | full p90 | v1.9.0 med | v1.9.0 p90 | pre-93eb224 med | pre-93eb224 p90 |
|---|---:|---:|---:|---:|---:|---:|---:|
| rust | 58 | 3809 | 17615 | 30 | 30 | 30 | 30 |
| python | 274 | 655 | 10796 | 30 | 40 | 30 | 40 |
| typescript | 270 | 277 | 1995 | 30 | 30 | 30 | 30 |
| tsx | 177 | 461 | 4787 | 30 | 30 | 30 | 30 |

**repeat-read delta: `smart_read` after one small edit to the file, repeat reads on**

| language | n | full med | full p90 | v1.9.0 med | v1.9.0 p90 | pre-93eb224 med | pre-93eb224 p90 |
|---|---:|---:|---:|---:|---:|---:|---:|
| rust | 57 | 3809 | 17615 | 44 | 55 | 44 | 55 |
| python | 188 | 655 | 10796 | 45 | 96 | 45 | 96 |
| typescript | 58 | 277 | 1995 | 44 | 142 | 44 | 142 |
| tsx | 130 | 461 | 4787 | 43 | 44 | 43 | 44 |

### Routes

How the server answered, from its own ledger. A route other than the mode's own is a fall-back the benchmark reports rather than hides: `would_inflate` is the guard returning the file because an outline would not be smaller; `unchanged_not_smaller`, `delta_too_big` and `no_base` are the repeat-read gate declining.

| bucket | mode | v1.9.0 routes | pre-93eb224 routes |
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

Token table digests (SHA-256 of the per-file rows of `*-tokens.tsv` with the build column blanked, so a digest does not depend on a label; equal digests mean equal token figures): v1.9.0 `2d99da988a0f0bbb6dee245e26303f3dfcf525dd935d1cf86d60ef6c5c14abc7`; pre-93eb224 `2d99da988a0f0bbb6dee245e26303f3dfcf525dd935d1cf86d60ef6c5c14abc7`

## Latency (MEASURED)

200 recorded runs per series: 5 passes of 40, pooled. In each pass the builds take turns (the order flips every pass), and each series first has 20 warm-up runs that are discarded (per series per pass, run the same way as the recorded runs, so caches, page tables and the ledger file are warm; hook spawns are warm-start, not first launch after boot). Clock: std::time::Instant, a monotonic clock, nanosecond resolution. Release builds, built here. Medians vary with the machine and its load, and this document makes no claim about how closely another run will land. What this run shows about itself: in 30 of 48 series, the median of every one of the 5 passes is within 20% of the series' pooled median (per-pass medians are in the JSON). `benchmarks/run.sh compare <this file>` re-measures on another machine and prints, per series, the ratio to this run, the same ratio after dividing by the process floor, how many series are within 20%, and whether that machine looks like this one.

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
| `hook.floor` | - | 200 | 2.57 ms | 2.86 ms | 3.57 ms | 2.62 ms | 3.25 ms | 4.10 ms |
| `hook.intercept_pass.spawn` | crates/lumen-core/tests/ranked_coverage.rs (122 lines, bucket <300) | 200 | 2.47 ms | 3.20 ms | 3.91 ms | 2.39 ms | 3.26 ms | 4.12 ms |
| `hook.intercept_block.spawn` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | 5.06 ms | 5.90 ms | 6.81 ms | 4.88 ms | 5.80 ms | 12.13 ms |
| `hook.meter_read.spawn` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | 30.57 ms | 33.18 ms | 36.37 ms | 31.16 ms | 33.75 ms | 37.11 ms |
| `hook.meter_bash.spawn` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | 29.62 ms | 32.39 ms | 39.80 ms | 30.15 ms | 33.42 ms | 40.60 ms |
| `hook.per_call_identity` | - | 200 | 4.02 ms | 4.79 ms | 5.39 ms | 5.46 ms | 6.04 ms | 6.52 ms |

### Hooks, work only: `hook::run_with` called in-process, warm (only for a build made from this checkout)

No process start, a warm tokenizer. A fresh hook process pays tokenizer start-up that these calls do not, which is part of the spawn figures above.

| series | subject | n | v1.9.0 median | v1.9.0 p90 | v1.9.0 p99 | pre-93eb224 median | pre-93eb224 p90 | pre-93eb224 p99 |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| `hook.intercept_pass.inproc` | crates/lumen-core/tests/ranked_coverage.rs (122 lines, bucket <300) | 200 | n/a (see notes) | | | 62 us | 68 us | 91 us |
| `hook.intercept_block.inproc` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | n/a (see notes) | | | 2.79 ms | 3.73 ms | 4.34 ms |
| `hook.meter_read.inproc` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | n/a (see notes) | | | 5.28 ms | 5.96 ms | 6.25 ms |
| `hook.meter_bash.inproc` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | n/a (see notes) | | | 5.55 ms | 8.07 ms | 9.66 ms |

### Derived: hook work above the process floor

`median(spawn) - median(floor)`, in each build. A derived figure, not a series of its own.

| hook | v1.9.0 | pre-93eb224 |
|---|---:|---:|
| `hook.intercept_pass.spawn_minus_floor` | -101 us | -227 us |
| `hook.intercept_block.spawn_minus_floor` | 2.49 ms | 2.26 ms |
| `hook.meter_read.spawn_minus_floor` | 28.00 ms | 28.54 ms |
| `hook.meter_bash.spawn_minus_floor` | 27.05 ms | 27.53 ms |
| `hook.per_call_identity.spawn_minus_floor` | 1.45 ms | 2.84 ms |
| `hook.per_call_identity.server_claim` | 230 us | 223 us |

### Server calls, round trip as a client sees it

From just before the request is written to the arrival of the reply line. `server.ping` is the round trip with no work; the ledger row is written after the reply and is not in the interval. For a build with per-call identity every call in these series also carries the hook's record and `_meta` id, so the server's claim of it is inside the interval; `server.identity.placed` against `server.identity.unplaced` (the same call with no record and no id) isolates that claim. Only a build with per-call identity has those two series.

| series | subject | n | v1.9.0 median | v1.9.0 p90 | v1.9.0 p99 | pre-93eb224 median | pre-93eb224 p90 | pre-93eb224 p99 |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| `server.start` | - | 200 | 2.49 ms | 2.91 ms | 3.50 ms | 2.91 ms | 3.74 ms | 4.28 ms |
| `server.ping` | - | 200 | 11 us | 12 us | 17 us | 11 us | 13 us | 15 us |
| `server.smart_read.<300` | crates/lumen-core/tests/ranked_coverage.rs (122 lines, bucket <300) | 200 | 900 us | 1.19 ms | 2.05 ms | 895 us | 1.21 ms | 2.87 ms |
| `server.recall_file.<300` | crates/lumen-core/tests/ranked_coverage.rs (122 lines, bucket <300) | 200 | 964 us | 1.24 ms | 1.49 ms | 959 us | 1.30 ms | 1.53 ms |
| `server.smart_read.300-999` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | 2.59 ms | 3.10 ms | 3.45 ms | 2.76 ms | 3.02 ms | 4.12 ms |
| `server.recall_file.300-999` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | 2.54 ms | 3.06 ms | 3.79 ms | 2.59 ms | 2.79 ms | 3.04 ms |
| `server.smart_read.1000-2999` | crates/lumen-core/tests/silent_fallbacks.rs (1472 lines, bucket 1000-2999) | 200 | 5.22 ms | 6.26 ms | 7.71 ms | 5.78 ms | 6.49 ms | 14.48 ms |
| `server.recall_file.1000-2999` | crates/lumen-core/tests/silent_fallbacks.rs (1472 lines, bucket 1000-2999) | 200 | 5.18 ms | 6.14 ms | 9.91 ms | 5.69 ms | 6.20 ms | 6.89 ms |
| `server.repeat_unchanged.<300` | crates/lumen-core/tests/ranked_coverage.rs (122 lines, bucket <300) | 200 | 2.81 ms | 3.99 ms | 4.46 ms | 3.15 ms | 3.89 ms | 4.31 ms |
| `server.repeat_changed.<300` | crates/lumen-core/tests/ranked_coverage.rs (122 lines, bucket <300) | 200 | 3.80 ms | 4.28 ms | 4.76 ms | 2.75 ms | 4.31 ms | 4.67 ms |
| `server.repeat_unchanged.300-999` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | 4.96 ms | 6.02 ms | 8.19 ms | 5.61 ms | 6.33 ms | 7.64 ms |
| `server.repeat_changed.300-999` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | 5.74 ms | 6.62 ms | 7.09 ms | 6.11 ms | 7.04 ms | 8.09 ms |
| `server.repeat_unchanged.1000-2999` | crates/lumen-core/tests/silent_fallbacks.rs (1472 lines, bucket 1000-2999) | 200 | 8.19 ms | 8.63 ms | 9.16 ms | 8.35 ms | 8.78 ms | 14.31 ms |
| `server.repeat_changed.1000-2999` | crates/lumen-core/tests/silent_fallbacks.rs (1472 lines, bucket 1000-2999) | 200 | 8.87 ms | 9.52 ms | 10.30 ms | 8.99 ms | 9.84 ms | 13.28 ms |
| `server.identity.placed` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | 2.52 ms | 2.87 ms | 3.83 ms | 2.56 ms | 2.86 ms | 3.66 ms |
| `server.identity.unplaced` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | 2.29 ms | 2.60 ms | 3.50 ms | 2.33 ms | 2.65 ms | 3.19 ms |

Notes on series that are not plain measurements:

- `hook.intercept_pass.inproc` on v1.9.0: n/a: the harness links this checkout's hook library, so it can time in-process only a binary built from the checkout; the comparable figure for any other build is spawn minus floor
- `hook.intercept_block.inproc` on v1.9.0: n/a: the harness links this checkout's hook library, so it can time in-process only a binary built from the checkout; the comparable figure for any other build is spawn minus floor
- `hook.meter_read.inproc` on v1.9.0: n/a: the harness links this checkout's hook library, so it can time in-process only a binary built from the checkout; the comparable figure for any other build is spawn minus floor
- `hook.meter_bash.inproc` on v1.9.0: n/a: the harness links this checkout's hook library, so it can time in-process only a binary built from the checkout; the comparable figure for any other build is spawn minus floor

Load on the machine around the whole latency run (every build, every pass):

- before: load average 6.83 7.82 6.37 (1, 5, 15 min); after: load average 6.58 6.97 6.23 (1, 5, 15 min)

## Dollars

None: A saving's dollar value is priced at the model of the turn that read the call's result, over the rounds that followed it, and less the extra round the call cost: the savings engine derives these from a ledger's turns. A corpus run has no turns, so this document gives tokens only. Engine: lumen_core::savings (crates/lumen-core/src/savings). Calibration: Token figures are Claude's tokens, estimated from the cl100k_base counts Lumen records (OpenAI's tokenizer: Claude's is not available offline), by the calibration of 2026-10-14, measured through Claude Code 2.1.296 against Claude's own usage data on 60 samples of raw file text. Each count is converted by its file's type and the model of the turn that read it, at the median ratio for them, with the smallest to the largest ratio any sample came out at as its band (not the 10th to 90th percentile: the floor is the band's low end, and must hold call by call); a row with no measured model takes the type's ratio over both tokenizers, weighted alike, with a band that spans them. The ratios are of raw file text: an answer Lumen makes from a file (an outline, a slice, a stub), a Read's line numbers and the tool-result wrapper around any answer, a Read's or Lumen's, are in no row. So an answer Lumen made takes the file's median, with the smallest to the largest ratio of any sample the model was sent as its band (of any sample at all where the model is not known). The dollar floor takes the low end of each saving: the file at its smallest ratio less Lumen's answer at its largest.
