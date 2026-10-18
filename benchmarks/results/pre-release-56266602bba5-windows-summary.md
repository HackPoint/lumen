# Lumen benchmarks: pre-release-56266602bba5

> **Pre-release measurement.** This is the harness run on commit `56266602bba5`, before the 1.8.0 release. It is not a 1.8.0 result and must not be quoted as one.

Per-call figures, per file-size bucket and per language. There is no overall "percent saved" here on purpose: how much a person saves overall depends on their usage mix (how many reads are large, how many repeat).

- **MEASURED**: token counts (real `cl100k_base` tokenizer) and latencies (monotonic clock).
- **No dollar figure** (see the end).
- Tokenizer: `cl100k_base` is a proxy for Claude's tokenizer. cl100k_base is not Claude's tokenizer. Counts compare modes with one another; they are not a bill and not the number of tokens Claude would charge.

## Builds

| label | role | commit | profile | per-call identity | reports version |
|---|---|---|---|---|---|
| v1.8.0 | baseline | `12ad305679cf92b23167b032f56412a2a38e72a1` | release, cargo's default: the root Cargo.toml has no [profile.release] | a hook record per call (PreToolUse on the Lumen tool), claimed by the server through _meta claudecode/toolUseId | 1.8.0 |
| pre-5626660 | candidate | `56266602bba5cefe3f894ce0318bf19d29c32f66` | release, cargo's default: the root Cargo.toml has no [profile.release] | a hook record per call (PreToolUse on the Lumen tool), claimed by the server through _meta claudecode/toolUseId | 1.8.0 |

## Machine

- OS: Microsoft Windows Server 2025 Datacenter 10.0.26100 (windows x86_64)
- CPU: AMD EPYC 7763 64-Core Processor, 4 logical cores
- RAM: 16.0 GiB
- Toolchain: rustc 1.99.1 (58cf8f9c3 2026-10-13) / cargo 1.99.1 (5f94df478 2026-08-27)
- Started: 2026-10-18T10:19:32Z
- Load at start: CPU 4% busy (instantaneous); at end: CPU 32% busy (instantaneous)
- probe failed: kernel: no kernel probe on this OS

## Corpus

| repository | license | commit | measured files | by bucket |
|---|---|---|---:|---|
| [lumen](https://github.com/HackPoint/lumen) | MIT | `3b951e93dbe55e46dc0e15951666b303110861e7` | 96 | <300 53, 300-999 31, 1000-2999 11, 3000+ 1 |

## Latency (MEASURED)

200 recorded runs per series: 5 passes of 40, pooled. In each pass the builds take turns (the order flips every pass), and each series first has 20 warm-up runs that are discarded (per series per pass, run the same way as the recorded runs, so caches, page tables and the ledger file are warm; hook spawns are warm-start, not first launch after boot). Clock: std::time::Instant, a monotonic clock, nanosecond resolution. Release builds, built here. Medians vary with the machine and its load, and this document makes no claim about how closely another run will land. What this run shows about itself: in 42 of 48 series, the median of every one of the 5 passes is within 20% of the series' pooled median (per-pass medians are in the JSON). `benchmarks/run.sh compare <this file>` re-measures on another machine and prints, per series, the ratio to this run, the same ratio after dividing by the process floor, how many series are within 20%, and whether that machine looks like this one.

Percentiles are nearest-rank: the percentile p of n runs is the run at rank ceil(p x n / 100) of the sorted runs, fastest first, with no interpolation. With n = 200 the median is run 100 (of the two middle runs of an even count, the faster), p90 is run 180, and p99 is run 198, which leaves the slowest 2 runs above it. Raw samples: not written. Each series carries `pass_medians_ns`, the median of each pass on its own, in pass order: how far they sit from one another is how much the machine moved while the run was going on.

Subjects (corpus files the series run on):

| bucket | file | lines | bytes | recall item |
|---|---|---:|---:|---|
| <300 | crates/lumen-core/tests/ranked_coverage.rs | 122 | 3754 | `walk` |
| 300-999 | crates/lumen-mcp/tests/protocol.rs | 532 | 17672 | `initialize_returns_a_well_formed_jsonrpc_response` |
| 1000-2999 | crates/lumen-core/tests/silent_fallbacks.rs | 1472 | 52110 | `rust_statement` |

### Hooks, as the harness sees them: process spawn to exit

The whole hook as Claude Code runs it. `hook.floor` is a hook invocation that does no work, so it is the cost of the process itself; the work is what each row adds to it. `hook.per_call_identity` is the PreToolUse hook on a call of one of Lumen's own MCP tools, which records who is calling; a build either has it (the series is timed) or does not (the slot says `n/a`, and why). Which one is decided by running the build, not by reading its usage text.

| series | subject | n | v1.8.0 median | v1.8.0 p90 | v1.8.0 p99 | pre-5626660 median | pre-5626660 p90 | pre-5626660 p99 |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| `hook.floor` | - | 200 | 5.40 ms | 5.86 ms | 6.27 ms | 5.41 ms | 6.14 ms | 7.02 ms |
| `hook.intercept_pass.spawn` | crates/lumen-core/tests/ranked_coverage.rs (122 lines, bucket <300) | 200 | 5.58 ms | 5.96 ms | 6.30 ms | 5.60 ms | 6.38 ms | 7.15 ms |
| `hook.intercept_block.spawn` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | 6.95 ms | 7.61 ms | 8.50 ms | 7.09 ms | 8.23 ms | 9.39 ms |
| `hook.meter_read.spawn` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | 73.68 ms | 77.59 ms | 86.77 ms | 75.38 ms | 80.45 ms | 91.38 ms |
| `hook.meter_bash.spawn` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | 72.95 ms | 89.80 ms | 140.98 ms | 74.71 ms | 86.01 ms | 92.89 ms |
| `hook.per_call_identity` | - | 200 | 5.82 ms | 7.16 ms | 8.44 ms | 5.86 ms | 6.76 ms | 8.04 ms |

### Hooks, work only: `hook::run_with` called in-process, warm (only for a build made from this checkout)

No process start, a warm tokenizer. A fresh hook process pays tokenizer start-up that these calls do not, which is part of the spawn figures above.

| series | subject | n | v1.8.0 median | v1.8.0 p90 | v1.8.0 p99 | pre-5626660 median | pre-5626660 p90 | pre-5626660 p99 |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| `hook.intercept_pass.inproc` | crates/lumen-core/tests/ranked_coverage.rs (122 lines, bucket <300) | 200 | n/a (see notes) | | | 49 us | 74 us | 121 us |
| `hook.intercept_block.inproc` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | n/a (see notes) | | | 830 us | 931 us | 1.16 ms |
| `hook.meter_read.inproc` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | n/a (see notes) | | | 4.68 ms | 5.22 ms | 6.73 ms |
| `hook.meter_bash.inproc` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | n/a (see notes) | | | 2.90 ms | 3.38 ms | 5.94 ms |

### Derived: hook work above the process floor

`median(spawn) - median(floor)`, in each build. A derived figure, not a series of its own.

| hook | v1.8.0 | pre-5626660 |
|---|---:|---:|
| `hook.intercept_pass.spawn_minus_floor` | 184 us | 190 us |
| `hook.intercept_block.spawn_minus_floor` | 1.55 ms | 1.69 ms |
| `hook.meter_read.spawn_minus_floor` | 68.29 ms | 69.97 ms |
| `hook.meter_bash.spawn_minus_floor` | 67.55 ms | 69.30 ms |
| `hook.per_call_identity.spawn_minus_floor` | 427 us | 454 us |
| `hook.per_call_identity.server_claim` | 56 us | 54 us |

### Server calls, round trip as a client sees it

From just before the request is written to the arrival of the reply line. `server.ping` is the round trip with no work; the ledger row is written after the reply and is not in the interval. For a build with per-call identity every call in these series also carries the hook's record and `_meta` id, so the server's claim of it is inside the interval; `server.identity.placed` against `server.identity.unplaced` (the same call with no record and no id) isolates that claim. Only a build with per-call identity has those two series.

| series | subject | n | v1.8.0 median | v1.8.0 p90 | v1.8.0 p99 | pre-5626660 median | pre-5626660 p90 | pre-5626660 p99 |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| `server.start` | - | 200 | 4.93 ms | 5.32 ms | 5.91 ms | 4.94 ms | 5.56 ms | 5.85 ms |
| `server.ping` | - | 200 | 61 us | 70 us | 94 us | 73 us | 86 us | 105 us |
| `server.smart_read.<300` | crates/lumen-core/tests/ranked_coverage.rs (122 lines, bucket <300) | 200 | 1.76 ms | 1.88 ms | 2.67 ms | 1.75 ms | 2.70 ms | 2.89 ms |
| `server.recall_file.<300` | crates/lumen-core/tests/ranked_coverage.rs (122 lines, bucket <300) | 200 | 1.82 ms | 1.90 ms | 2.77 ms | 1.81 ms | 2.02 ms | 2.94 ms |
| `server.smart_read.300-999` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | 6.43 ms | 7.41 ms | 10.30 ms | 6.35 ms | 6.57 ms | 8.53 ms |
| `server.recall_file.300-999` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | 6.28 ms | 9.59 ms | 10.70 ms | 6.12 ms | 6.34 ms | 9.47 ms |
| `server.smart_read.1000-2999` | crates/lumen-core/tests/silent_fallbacks.rs (1472 lines, bucket 1000-2999) | 200 | 14.58 ms | 19.60 ms | 24.74 ms | 14.18 ms | 14.86 ms | 20.92 ms |
| `server.recall_file.1000-2999` | crates/lumen-core/tests/silent_fallbacks.rs (1472 lines, bucket 1000-2999) | 200 | 14.21 ms | 17.66 ms | 23.63 ms | 13.89 ms | 14.65 ms | 18.79 ms |
| `server.repeat_unchanged.<300` | crates/lumen-core/tests/ranked_coverage.rs (122 lines, bucket <300) | 200 | 2.52 ms | 2.87 ms | 3.84 ms | 2.49 ms | 2.58 ms | 3.63 ms |
| `server.repeat_changed.<300` | crates/lumen-core/tests/ranked_coverage.rs (122 lines, bucket <300) | 200 | 2.53 ms | 2.74 ms | 3.60 ms | 2.52 ms | 2.82 ms | 4.42 ms |
| `server.repeat_unchanged.300-999` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | 7.31 ms | 7.76 ms | 10.75 ms | 7.25 ms | 9.22 ms | 11.89 ms |
| `server.repeat_changed.300-999` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | 7.26 ms | 8.03 ms | 9.32 ms | 7.40 ms | 10.77 ms | 13.82 ms |
| `server.repeat_unchanged.1000-2999` | crates/lumen-core/tests/silent_fallbacks.rs (1472 lines, bucket 1000-2999) | 200 | 15.61 ms | 16.81 ms | 20.41 ms | 15.48 ms | 16.86 ms | 23.96 ms |
| `server.repeat_changed.1000-2999` | crates/lumen-core/tests/silent_fallbacks.rs (1472 lines, bucket 1000-2999) | 200 | 15.57 ms | 16.83 ms | 22.30 ms | 15.56 ms | 19.42 ms | 23.53 ms |
| `server.identity.placed` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | 6.49 ms | 7.44 ms | 9.90 ms | 6.31 ms | 6.54 ms | 9.50 ms |
| `server.identity.unplaced` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | 6.43 ms | 6.95 ms | 10.33 ms | 6.26 ms | 6.58 ms | 8.59 ms |

Notes on series that are not plain measurements:

- `hook.intercept_pass.inproc` on v1.8.0: n/a: the harness links this checkout's hook library, so it can time in-process only a binary built from the checkout; the comparable figure for any other build is spawn minus floor
- `hook.intercept_block.inproc` on v1.8.0: n/a: the harness links this checkout's hook library, so it can time in-process only a binary built from the checkout; the comparable figure for any other build is spawn minus floor
- `hook.meter_read.inproc` on v1.8.0: n/a: the harness links this checkout's hook library, so it can time in-process only a binary built from the checkout; the comparable figure for any other build is spawn minus floor
- `hook.meter_bash.inproc` on v1.8.0: n/a: the harness links this checkout's hook library, so it can time in-process only a binary built from the checkout; the comparable figure for any other build is spawn minus floor

Load on the machine around the whole latency run (every build, every pass):

- before: CPU 2% busy (instantaneous); after: CPU 26% busy (instantaneous)

## Dollars

None: A saving's dollar value is priced at the model of the turn that read the call's result, over the rounds that followed it, and less the extra round the call cost: the savings engine derives these from a ledger's turns. A corpus run has no turns, so this document gives tokens only. Engine: lumen_core::savings (crates/lumen-core/src/savings). Calibration: Token figures are Claude's tokens, estimated from the cl100k_base counts Lumen records (OpenAI's tokenizer: Claude's is not available offline), by the calibration of 2026-10-14, measured through Claude Code 2.1.296 against Claude's own usage data on 60 samples of raw file text. Each count is converted by its file's type and the model of the turn that read it, at the median ratio for them, with the smallest to the largest ratio any sample came out at as its band (not the 10th to 90th percentile: the floor is the band's low end, and must hold call by call); a row with no measured model takes the type's ratio over both tokenizers, weighted alike, with a band that spans them. The ratios are of raw file text: an answer Lumen makes from a file (an outline, a slice, a stub), a Read's line numbers and the tool-result wrapper around any answer, a Read's or Lumen's, are in no row. So an answer Lumen made takes the file's median, with the smallest to the largest ratio of any sample the model was sent as its band (of any sample at all where the model is not known). The dollar floor takes the low end of each saving: the file at its smallest ratio less Lumen's answer at its largest.
