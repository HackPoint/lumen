# Lumen benchmarks: pre-release-48dbf977a38a

> **Pre-release measurement.** This is the harness run on commit `48dbf977a38a`, before the 1.8.0 release. It is not a 1.8.0 result and must not be quoted as one.

Per-call figures, per file-size bucket and per language. There is no overall "percent saved" here on purpose: how much a person saves overall depends on their usage mix (how many reads are large, how many repeat).

- **MEASURED**: token counts (real `cl100k_base` tokenizer) and latencies (monotonic clock).
- **No dollar figure** (see the end).
- Tokenizer: `cl100k_base` is a proxy for Claude's tokenizer. cl100k_base is not Claude's tokenizer. Counts compare modes with one another; they are not a bill and not the number of tokens Claude would charge.

## Builds

| label | role | commit | profile | per-call identity | reports version |
|---|---|---|---|---|---|
| v1.9.0 | baseline | `cbb8d0fe9d18acb9d7ca1b839dab528fbcb522e7` | release, cargo's default: the root Cargo.toml has no [profile.release] | a hook record per call (PreToolUse on the Lumen tool), claimed by the server through _meta claudecode/toolUseId | 1.9.0 |
| pre-48dbf97 | candidate | `48dbf977a38a42d7f5e9eae8780a04ff72070a6c` | release, cargo's default: the root Cargo.toml has no [profile.release] | a hook record per call (PreToolUse on the Lumen tool), claimed by the server through _meta claudecode/toolUseId | 1.10.0 |

## Machine

- OS: Microsoft Windows Server 2025 Datacenter 10.0.26100 (windows x86_64)
- CPU: AMD EPYC 7763 64-Core Processor, 4 logical cores
- RAM: 16.0 GiB
- Toolchain: rustc 1.99.1 (58cf8f9c3 2026-10-13) / cargo 1.99.1 (5f94df478 2026-08-27)
- Started: 2026-10-26T11:44:19Z
- Load at start: CPU 4% busy (instantaneous); at end: CPU 1% busy (instantaneous)
- probe failed: kernel: no kernel probe on this OS

## Corpus

| repository | license | commit | measured files | by bucket |
|---|---|---|---:|---|
| [lumen](https://github.com/HackPoint/lumen) | MIT | `3b951e93dbe55e46dc0e15951666b303110861e7` | 96 | <300 53, 300-999 31, 1000-2999 11, 3000+ 1 |

## Latency (MEASURED)

200 recorded runs per series: 5 passes of 40, pooled. In each pass the builds take turns (the order flips every pass), and each series first has 20 warm-up runs that are discarded (per series per pass, run the same way as the recorded runs, so caches, page tables and the ledger file are warm; hook spawns are warm-start, not first launch after boot). Clock: std::time::Instant, a monotonic clock, nanosecond resolution. Release builds, built here. Medians vary with the machine and its load, and this document makes no claim about how closely another run will land. What this run shows about itself: in 47 of 48 series, the median of every one of the 5 passes is within 20% of the series' pooled median (per-pass medians are in the JSON). `benchmarks/run.sh compare <this file>` re-measures on another machine and prints, per series, the ratio to this run, the same ratio after dividing by the process floor, how many series are within 20%, and whether that machine looks like this one.

Percentiles are nearest-rank: the percentile p of n runs is the run at rank ceil(p x n / 100) of the sorted runs, fastest first, with no interpolation. With n = 200 the median is run 100 (of the two middle runs of an even count, the faster), p90 is run 180, and p99 is run 198, which leaves the slowest 2 runs above it. Raw samples: not written. Each series carries `pass_medians_ns`, the median of each pass on its own, in pass order: how far they sit from one another is how much the machine moved while the run was going on.

Subjects (corpus files the series run on):

| bucket | file | lines | bytes | recall item |
|---|---|---:|---:|---|
| <300 | crates/lumen-core/tests/ranked_coverage.rs | 122 | 3754 | `walk` |
| 300-999 | crates/lumen-mcp/tests/protocol.rs | 532 | 17672 | `initialize_returns_a_well_formed_jsonrpc_response` |
| 1000-2999 | crates/lumen-core/tests/silent_fallbacks.rs | 1472 | 52110 | `rust_statement` |

### Hooks, as the harness sees them: process spawn to exit

The whole hook as Claude Code runs it. `hook.floor` is a hook invocation that does no work, so it is the cost of the process itself; the work is what each row adds to it. `hook.per_call_identity` is the PreToolUse hook on a call of one of Lumen's own MCP tools, which records who is calling; a build either has it (the series is timed) or does not (the slot says `n/a`, and why). Which one is decided by running the build, not by reading its usage text.

| series | subject | n | v1.9.0 median | v1.9.0 p90 | v1.9.0 p99 | pre-48dbf97 median | pre-48dbf97 p90 | pre-48dbf97 p99 |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| `hook.floor` | - | 200 | 5.67 ms | 6.40 ms | 7.04 ms | 10.37 ms | 11.54 ms | 12.66 ms |
| `hook.intercept_pass.spawn` | crates/lumen-core/tests/ranked_coverage.rs (122 lines, bucket <300) | 200 | 5.94 ms | 6.48 ms | 6.82 ms | 10.57 ms | 12.04 ms | 13.75 ms |
| `hook.intercept_block.spawn` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | 7.84 ms | 8.74 ms | 9.43 ms | 15.88 ms | 18.21 ms | 20.86 ms |
| `hook.meter_read.spawn` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | 87.63 ms | 100.44 ms | 137.74 ms | 98.39 ms | 110.53 ms | 139.61 ms |
| `hook.meter_bash.spawn` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | 85.47 ms | 95.12 ms | 107.30 ms | 97.82 ms | 115.39 ms | 155.44 ms |
| `hook.per_call_identity` | - | 200 | 6.18 ms | 7.14 ms | 7.99 ms | 15.10 ms | 17.06 ms | 19.15 ms |

Note: `hook.intercept_block.spawn` is 8.03 ms slower on pre-48dbf97 than on v1.9.0 (15.88 ms against 7.84 ms, medians as measured on this machine in this run). This run times the hook, it does not trace it, so it does not say why. To find the cause, read what changed in the blocking path of `hook intercept` between the two commits (`git log cbb8d0fe9d18..48dbf977a38a -- crates/lumen-mcp crates/lumen-core`). The hook that lets a Read through (`hook.intercept_pass.spawn`) is 10.57 ms on pre-48dbf97 and 5.94 ms on v1.9.0, so starting the process, or what every hook does, may be part of it.

### Hooks, work only: `hook::run_with` called in-process, warm (only for a build made from this checkout)

No process start, a warm tokenizer. A fresh hook process pays tokenizer start-up that these calls do not, which is part of the spawn figures above.

| series | subject | n | v1.9.0 median | v1.9.0 p90 | v1.9.0 p99 | pre-48dbf97 median | pre-48dbf97 p90 | pre-48dbf97 p99 |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| `hook.intercept_pass.inproc` | crates/lumen-core/tests/ranked_coverage.rs (122 lines, bucket <300) | 200 | n/a (see notes) | | | 52 us | 68 us | 113 us |
| `hook.intercept_block.inproc` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | n/a (see notes) | | | 881 us | 1.26 ms | 1.44 ms |
| `hook.meter_read.inproc` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | n/a (see notes) | | | 5.36 ms | 6.69 ms | 9.33 ms |
| `hook.meter_bash.inproc` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | n/a (see notes) | | | 3.26 ms | 4.01 ms | 4.97 ms |

### Derived: hook work above the process floor

`median(spawn) - median(floor)`, in each build. A derived figure, not a series of its own.

| hook | v1.9.0 | pre-48dbf97 |
|---|---:|---:|
| `hook.intercept_pass.spawn_minus_floor` | 273 us | 202 us |
| `hook.intercept_block.spawn_minus_floor` | 2.17 ms | 5.51 ms |
| `hook.meter_read.spawn_minus_floor` | 81.95 ms | 88.01 ms |
| `hook.meter_bash.spawn_minus_floor` | 79.80 ms | 87.45 ms |
| `hook.per_call_identity.spawn_minus_floor` | 511 us | 4.73 ms |
| `hook.per_call_identity.server_claim` | 201 us | 611 us |

### Server calls, round trip as a client sees it

From just before the request is written to the arrival of the reply line. `server.ping` is the round trip with no work; the ledger row is written after the reply and is not in the interval. For a build with per-call identity every call in these series also carries the hook's record and `_meta` id, so the server's claim of it is inside the interval; `server.identity.placed` against `server.identity.unplaced` (the same call with no record and no id) isolates that claim. Only a build with per-call identity has those two series.

| series | subject | n | v1.9.0 median | v1.9.0 p90 | v1.9.0 p99 | pre-48dbf97 median | pre-48dbf97 p90 | pre-48dbf97 p99 |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| `server.start` | - | 200 | 5.03 ms | 5.92 ms | 7.44 ms | 12.32 ms | 14.45 ms | 16.88 ms |
| `server.ping` | - | 200 | 67 us | 87 us | 143 us | 82 us | 93 us | 132 us |
| `server.smart_read.<300` | crates/lumen-core/tests/ranked_coverage.rs (122 lines, bucket <300) | 200 | 2.22 ms | 2.46 ms | 3.45 ms | 2.47 ms | 2.85 ms | 3.41 ms |
| `server.recall_file.<300` | crates/lumen-core/tests/ranked_coverage.rs (122 lines, bucket <300) | 200 | 2.30 ms | 2.62 ms | 3.37 ms | 2.56 ms | 3.00 ms | 3.55 ms |
| `server.smart_read.300-999` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | 7.54 ms | 8.42 ms | 11.56 ms | 8.06 ms | 10.02 ms | 12.07 ms |
| `server.recall_file.300-999` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | 7.16 ms | 7.89 ms | 12.95 ms | 7.55 ms | 8.26 ms | 11.06 ms |
| `server.smart_read.1000-2999` | crates/lumen-core/tests/silent_fallbacks.rs (1472 lines, bucket 1000-2999) | 200 | 17.02 ms | 18.52 ms | 22.79 ms | 17.33 ms | 18.73 ms | 24.12 ms |
| `server.recall_file.1000-2999` | crates/lumen-core/tests/silent_fallbacks.rs (1472 lines, bucket 1000-2999) | 200 | 16.34 ms | 18.66 ms | 24.30 ms | 17.12 ms | 18.68 ms | 24.41 ms |
| `server.repeat_unchanged.<300` | crates/lumen-core/tests/ranked_coverage.rs (122 lines, bucket <300) | 200 | 3.20 ms | 3.49 ms | 4.40 ms | 3.43 ms | 4.07 ms | 4.85 ms |
| `server.repeat_changed.<300` | crates/lumen-core/tests/ranked_coverage.rs (122 lines, bucket <300) | 200 | 3.27 ms | 3.55 ms | 4.63 ms | 3.41 ms | 3.96 ms | 4.83 ms |
| `server.repeat_unchanged.300-999` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | 8.70 ms | 9.66 ms | 12.94 ms | 9.04 ms | 10.24 ms | 13.55 ms |
| `server.repeat_changed.300-999` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | 8.62 ms | 9.40 ms | 13.59 ms | 9.13 ms | 9.99 ms | 12.87 ms |
| `server.repeat_unchanged.1000-2999` | crates/lumen-core/tests/silent_fallbacks.rs (1472 lines, bucket 1000-2999) | 200 | 18.37 ms | 20.32 ms | 25.83 ms | 18.87 ms | 20.66 ms | 27.59 ms |
| `server.repeat_changed.1000-2999` | crates/lumen-core/tests/silent_fallbacks.rs (1472 lines, bucket 1000-2999) | 200 | 18.18 ms | 19.71 ms | 24.32 ms | 18.30 ms | 20.21 ms | 28.27 ms |
| `server.identity.placed` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | 7.60 ms | 8.30 ms | 10.31 ms | 7.93 ms | 8.72 ms | 11.54 ms |
| `server.identity.unplaced` | crates/lumen-mcp/tests/protocol.rs (532 lines, bucket 300-999) | 200 | 7.40 ms | 8.17 ms | 9.71 ms | 7.32 ms | 8.36 ms | 11.23 ms |

Notes on series that are not plain measurements:

- `hook.intercept_pass.inproc` on v1.9.0: n/a: the harness links this checkout's hook library, so it can time in-process only a binary built from the checkout; the comparable figure for any other build is spawn minus floor
- `hook.intercept_block.inproc` on v1.9.0: n/a: the harness links this checkout's hook library, so it can time in-process only a binary built from the checkout; the comparable figure for any other build is spawn minus floor
- `hook.meter_read.inproc` on v1.9.0: n/a: the harness links this checkout's hook library, so it can time in-process only a binary built from the checkout; the comparable figure for any other build is spawn minus floor
- `hook.meter_bash.inproc` on v1.9.0: n/a: the harness links this checkout's hook library, so it can time in-process only a binary built from the checkout; the comparable figure for any other build is spawn minus floor

Load on the machine around the whole latency run (every build, every pass):

- before: CPU 2% busy (instantaneous); after: CPU 4% busy (instantaneous)

## Dollars

None: A saving's dollar value is priced at the model of the turn that read the call's result, over the rounds that followed it, and less the extra round the call cost: the savings engine derives these from a ledger's turns. A corpus run has no turns, so this document gives tokens only. Engine: lumen_core::savings (crates/lumen-core/src/savings). Calibration: Token figures are Claude's tokens, estimated from the cl100k_base counts Lumen records (OpenAI's tokenizer: Claude's is not available offline), by the calibration of 2026-10-14, measured through Claude Code 2.1.296 against Claude's own usage data on 60 samples of raw file text. Each count is converted by its file's type and the model of the turn that read it, at the median ratio for them, with the smallest to the largest ratio any sample came out at as its band (not the 10th to 90th percentile: the floor is the band's low end, and must hold call by call); a row with no measured model takes the type's ratio over both tokenizers, weighted alike, with a band that spans them. The ratios are of raw file text: an answer Lumen makes from a file (an outline, a slice, a stub), a Read's line numbers and the tool-result wrapper around any answer, a Read's or Lumen's, are in no row. So an answer Lumen made takes the file's median, with the smallest to the largest ratio of any sample the model was sent as its band (of any sample at all where the model is not known). The dollar floor takes the low end of each saving: the file at its smallest ratio less Lumen's answer at its largest.
