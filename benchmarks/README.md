# Lumen benchmarks

Per-call token and latency figures for Lumen's read tools and hooks, measured through the real
`lumen-mcp` binary on a pinned corpus, for two builds side by side: the last release (v1.9.0)
and the checkout you run it from. Everything is reproduced by one command.

```sh
benchmarks/run.sh                 # macOS / Linux: tokens and latency, v1.9.0 against this checkout
benchmarks/run.sh compare benchmarks/results/<label>-<os>.json   # latency again, against a committed run
benchmarks\run.ps1 --latency-only # Windows (PowerShell)
cargo run --release --locked -p lumen-bench -- --help
```

`run.sh` is `cargo run --release --locked -p lumen-bench -- "$@"`. The harness is the
`lumen-bench` crate in `benchmarks/harness`. It is a workspace member that nothing shipped
depends on: no runtime dependency was added to `lumen-core`, `lumen-mcp` or any binary the hooks
run, and it is not part of the app bundle. Its fast smoke test (a four-file fixture, no network,
a few seconds) runs with the rest of `cargo test --workspace`; the corpus run does not.

A run needs `git`, a Rust toolchain, network access to fetch the corpus (or a local mirror, see
below), about 1 GiB of disk for the cache (the corpus plus the v1.9.0 build) and another GiB or
so for this checkout's release build, and a clean checkout: the
candidate build refuses uncommitted changes outside `benchmarks/results` (override with
`--allow-dirty` for a throwaway run, which the output then marks as a dirty tree). With the
corpus and the baseline build already cached, a full run took about 3 minutes on the M4 laptop
for 1.9.0 and about 4 for 1.8.0 (most of it the latency series); the first run also fetches the
corpus and makes two release builds, which is the longest part.

## Outputs

| file | content |
|---|---|
| `results/<label>-<os>.json` | everything, machine-readable, stable key order, with an `environment` block |
| `results/<label>-<os>-summary.md` | the tables a person reads |
| `results/<label>-tokens.tsv` | one row per file and build: every token figure, deterministic |

`--label` names them. A label of the form `MAJOR.MINOR.PATCH` is a release result; anything else
is stamped **pre-release** in the JSON (`"pre_release": true`) and at the top of the summary. The
default label is `pre-release-<12 characters of the commit that was built>`, and the candidate
build is labelled `pre-<7 characters>` in the tables, so a result always says which commit it is a
measurement of and never calls it "main" (main moves; the commit does not).

## What is committed here

One measurement per release, each against the release before it, newest first. Every result file
is kept; a later release adds its own and replaces none.

| release | commit measured | against | tokens | latency |
|---|---|---|---|---|
| 1.10.0 | `93eb224` | v1.9.0 | equal | equal on Ubuntu and Windows; on macOS equal once the binary is measured as built ahead (see below) |
| 1.9.0 | `5626660` | v1.8.0 | equal | equal; on Ubuntu one series 2% (30 us) slower, on Windows two 3% faster |
| 1.8.0 | `4ca4fd1` | v1.7.0 | equal | a blocked Read 0.4 to 2.6 ms slower; a new identity hook before every Lumen call |

### 1.10.0: commit 93eb224 against v1.9.0

`results/pre-release-93eb22405b42-*` measure commit `93eb22405b42`, the last commit before the
1.10.0 release that changes anything the harness measures, against v1.9.0 (`cbb8d0f`, the tag as it
stands since the 2026-10-20 history rewrite; the pin before it no longer existed).

| platform | run | machine |
|---|---|---|
| macOS 26.6.2 | tokens and latency, 200 runs per series | Apple M4, 24 GiB, in use: load average 9.6 before the latency series, 6.6 after |
| Ubuntu 24.04 | latency only, 200 runs per series, the Benchmarks workflow (run 40321006240) | GitHub-hosted runner, Intel Xeon Platinum 8370C, 4 logical cores, 15.6 GiB |
| Windows Server 2025 | the same | GitHub-hosted runner, AMD EPYC 9V45, 4 logical cores, 16.0 GiB |

1.10.0 moves the data folder to the new identifier and adds the redirect every hook applies to a
path; what the tools return and what the hooks do are unchanged, so the result expected is equal
tokens and equal latency. Tokens are equal. Medians, v1.9.0 then this commit:

| | macOS | Ubuntu | Windows |
|---|---:|---:|---:|
| `hook.floor`: a hook that does nothing | 2.57 to 2.62 ms | 729 to 736 us | 3.25 to 3.30 ms |
| `hook.intercept_pass.spawn`: a small file's Read, let through | 2.47 to 2.39 ms | 767 to 772 us | 3.37 to 3.40 ms |
| `hook.intercept_block.spawn`: a large file's Read, blocked | 5.06 to 4.88 ms | 1.21 to 1.25 ms | 4.38 to 4.44 ms |
| `hook.per_call_identity` | 4.02 to 5.46 ms (see below) | 823 to 842 us | 3.62 to 3.65 ms |
| `hook.meter_read.spawn` | 30.57 to 31.16 ms | 43.61 to 44.78 ms | 52.54 to 54.06 ms |
| `server.start` | 2.49 to 2.91 ms (see below) | 747 to 756 us | 3.07 to 3.10 ms |
| `server.smart_read`, a file under 300 lines | 900 to 895 us | 1.20 to 1.21 ms | 1.09 to 1.10 ms |
| `server.smart_read`, a file of 1000 to 2999 lines | 5.22 to 5.78 ms (see below) | 10.71 to 10.73 ms | 8.82 to 8.97 ms |

Two regressions were found on the way and fixed before this commit. On Windows every hook had
become 4.7 ms slower (a hook that does nothing, 5.67 to 10.37 ms): the redirect asked the shell for
the data folder (`dirs::data_dir`, `SHGetKnownFolderPath`), and while that call was so much as
referenced, the binary imported shell32, which Windows loads into every hook process. The folder
now comes from `%APPDATA%`, and the hook binary no longer names `dirs` on Windows.

On macOS, three series are slower in this run and in no other: the per-call identity hook (+1.4
ms), the server's start and the large `smart_read`. They are slower only when the harness builds
the candidate in the same run. The same binary, the one this run built, at the same path, measured
again as a prebuilt candidate (`--candidate-bin`), gives 4.08 to 3.96 ms for the identity hook,
2.71 to 2.70 ms for the server's start and 5.13 to 5.09 ms for the large `smart_read`:
`results/pre-release-93eb22405b42-macos-prebuilt-*` (its header names it `ab2`). Ubuntu and Windows
show none of the three. Why a candidate built in the same run measures slower on macOS is not
traced yet; the code paths themselves were timed in-process and are unchanged.

### 1.9.0: commit 5626660 against v1.8.0

`results/pre-release-56266602bba5-*` measure commit `56266602bba5cefe3f894ce0318bf19d29c32f66`,
the last commit before the 1.9.0 version bump that changes anything the harness measures, against
v1.8.0 (`12ad305679cf92b23167b032f56412a2a38e72a1`). They carry the pre-release stamp because the
bump is a later commit; the commits after it touch this directory, this file, `README.md`,
`CHANGELOG.md` and the version strings.

| platform | run | machine |
|---|---|---|
| macOS 26.6.2 | tokens and latency, 200 runs per series | Apple M4 (4 performance and 6 efficiency cores), 24 GiB, on mains and in use: load average 7.2 before the latency series, 3.9 after |
| Ubuntu 24.04 | latency only, 200 runs per series, the Benchmarks workflow (run 39118498843) | GitHub-hosted runner, Intel Xeon Platinum 8573C, 4 logical cores, 15.6 GiB, load average 4.0 before, 1.4 after |
| Windows Server 2025 | the same | GitHub-hosted runner, AMD EPYC 7763, 4 logical cores, 16.0 GiB, CPU 2% busy before, 26% after |

1.9.0 changes how Lumen counts what it saved (one savings engine, per-model prices, the
calibration, the Home screen) and how a ledger is upgraded (one transaction, no write on opening a
current ledger), not what the tools return or what the hooks do, so the result expected is equal
tokens and equal latency. Medians, v1.8.0 then this commit:

| | macOS | Ubuntu | Windows |
|---|---:|---:|---:|
| `hook.intercept_block.spawn`: a large file's Read, blocked | 4.59 to 3.54 ms | 1.47 to 1.50 ms | 6.95 to 7.09 ms |
| `hook.intercept_pass.spawn`: a small file's Read, let through | 2.46 to 2.30 ms | 984 to 995 us | 5.58 to 5.60 ms |
| `hook.floor`: a hook that does nothing | 2.42 to 2.27 ms | 934 to 947 us | 5.40 to 5.41 ms |
| `hook.per_call_identity` | 3.03 to 3.12 ms | 1.05 to 1.04 ms | 5.82 to 5.86 ms |
| `server.smart_read`, a file under 300 lines | 635 to 636 us | 1.23 to 1.24 ms | 1.76 to 1.75 ms |
| `server.recall_file`, a file under 300 lines | 688 to 659 us | 1.26 to 1.29 ms | 1.82 to 1.81 ms |
| `server.smart_read`, a file of 1000 to 2999 lines | 5.27 to 5.29 ms | 10.45 to 10.67 ms | 14.58 to 14.18 ms |
| `server.repeat_unchanged`, a file under 300 lines | 2.02 to 2.03 ms | 1.59 to 1.65 ms | 2.52 to 2.49 ms |

- **Tokens** (macOS only; they do not depend on the machine): equal for every file and mode. Both
  builds' digests are `2d99da988a0f...`, the digest of 1.8.0's own committed run.
- **macOS: no difference.** For every series, the five pass medians of one build overlap the
  other's. The medians run from 0.77 to 1.23 of v1.8.0's on a laptop in use; the widest pass
  medians, as in `hook.intercept_block.spawn` (3.8 to 5.1 ms for v1.8.0 alone), are wider than
  the difference.
- **Ubuntu: one loss.** `server.recall_file.<300` takes 30 us longer (1.26 to 1.29 ms, 2%), and the
  slowest of v1.8.0's five pass medians is faster than the fastest of this commit's. Every other
  series overlaps; their medians are within 0.99 to 1.05 of v1.8.0's, most of the server series 1
  to 3% higher. Since v1.8.0, `lumen-mcp`'s own code changed only in the intercept hook (`hook.rs`)
  and a comment in `tok.rs`; `lumen-core`, which it links, changed more, including the ledger
  schema and how it is opened. Which change, if any, is the cost was not traced, and the same
  series did not separate on macOS or Windows. It is published as a loss (#62).
- **Windows: two gains.** `server.smart_read.1000-2999` (14.58 to 14.18 ms) and
  `server.identity.placed` (6.49 to 6.31 ms) are 3% faster, with every pass median of this commit
  below every one of v1.8.0's. Every other series overlaps; medians within 0.97 to 1.20 of
  v1.8.0's (1.20 is `server.ping`, 61 to 73 us).
- **An earlier run of the same release** measured `02baa69`, before the upgrade change (Benchmarks
  run 39112297823, whose Ubuntu and Windows files are on its `bench-results/*-39112297823` branches;
  none of that run is kept here). On Ubuntu it separated two other series,
  `server.repeat_unchanged.<300` and `server.repeat_changed.<300`, by 40 us (2%), which this run
  does not; tokens were equal and nothing separated on macOS or Windows. The runners drew other
  CPUs (Ubuntu an AMD EPYC 7763 there, Windows an EPYC 9V45). Read a 2% difference from a shared
  runner as one draw, not as a constant.

### 1.8.0: commit 4ca4fd1 against v1.7.0

`results/pre-release-4ca4fd1ffe21-*` measure commit `4ca4fd1ffe216f47e2c764be80cce4cbb56de6bc`,
the last code commit before the 1.8.0 version bump, against v1.7.0. They carry the pre-release
stamp because the bump is a later commit. Nothing after it changes what the harness measures: the
commits that follow touch this directory, this file, the version strings (in `Cargo.lock`, only the
workspace crates' own lines) and `CHANGELOG.md`.

| platform | run | machine |
|---|---|---|
| macOS 26.6.2 | tokens and latency, 200 runs per series | Apple M4 (4 performance and 6 efficiency cores), 24 GiB, on mains and in use: load average 5.0 before the latency series, 4.9 after |
| Ubuntu 24.04 | latency only, 200 runs per series, the Benchmarks workflow (run 38389263356) | GitHub-hosted runner, AMD EPYC 9V45, 4 logical cores, 15.6 GiB, load average 3.1 before, 1.6 after |
| Windows Server 2025 | the same | GitHub-hosted runner, AMD EPYC 9V74, 4 logical cores, 16.0 GiB, CPU 1% busy before, 4% after |

Medians, v1.7.0 then this commit. Every figure is in the summaries and there is no overall figure
(see "No headline"). Compare a platform's two builds with each other, not one platform's latency
with another's.

| | macOS | Ubuntu | Windows |
|---|---:|---:|---:|
| `hook.intercept_block.spawn`: a large file's Read, blocked | 2.90 to 5.53 ms | 1.01 to 1.40 ms | 4.63 to 5.66 ms |
| `hook.intercept_pass.spawn`: a small file's Read, let through | 3.06 to 3.04 ms | 0.98 to 0.97 ms | 4.42 to 4.54 ms |
| `hook.floor`: a hook that does nothing | 2.99 to 2.87 ms | 0.93 to 0.92 ms | 4.28 to 4.31 ms |
| `hook.per_call_identity`: new in this commit, 1.7.0 has none | 4.84 ms | 1.02 ms | 4.69 ms |
| `server.smart_read`, a file under 300 lines | 658 to 926 us | 671 to 724 us | 1.26 to 1.56 ms |
| `server.smart_read`, a file of 1000 to 2999 lines | 5.65 to 6.42 ms | 6.17 to 6.21 ms | 11.43 to 12.30 ms |

- **Tokens** (macOS only; they do not depend on the machine): equal for every file and mode. The
  two builds' digests are the same, `2d99da988a0f...` (see "Reading a result").
- **A blocked Read costs more than in 1.7.0**, on all three platforms and in every pass: the
  slowest of 1.7.0's five pass medians is faster than the fastest of this commit's. Since #42 a
  blocked Read opens the ledger read-only and runs up to two SELECTs on `read_events`
  (`lumen_served` in `crates/lumen-mcp/src/hook.rs`, which calls `meter::lumen_answered`) to decide
  whether the retry goes to `recall_file`; 1.7.0 did not touch the ledger there. A reviewer
  confirmed on macOS that this lookup is the cost: a scratch build with `lumen_served` made to
  return false at once blocked a Read in 1.7.0's time (within about 0.1 ms), where the unmodified
  build took 2.3 to 2.7 ms longer; a second reviewer repeated it over eight runs and got the same.
  Ubuntu and Windows were not tried. A Read that is let through costs what it did, within 3% on all
  three platforms in the first run.
- **A hook runs before every Lumen call.** 1.7.0 read the session id once, from the environment,
  when the server started. This commit names the conversation per call, so that `/clear` and
  `/resume` get a full result (#32). `hook.per_call_identity` is that hook: above the process floor,
  about 1 to 2 ms on the macOS laptop (1.97 ms in the committed run), 0.10 ms on Ubuntu and 0.38 ms
  on Windows. The server's claim of its record (`hook.per_call_identity.server_claim`: 178 us,
  39 us, 191 us) is inside every server call.
- **Server calls** (`server.smart_read`, `server.recall_file`) take longer in every bucket: by 0.27
  to 0.77 ms on macOS, 0.04 to 0.06 ms on Ubuntu and 0.30 to 0.90 ms on Windows. The identity
  claim is part of it; the harness does not trace the rest.
- **Close to 1.7.0**: the meter hooks (`hook.meter_*`, 0.98 to 1.02 of its median on every
  platform) and the repeat-read calls (`server.repeat_*`: 1.00 to 1.14 on Ubuntu and Windows; on
  macOS 0.86 to 1.06, except the next bullet).
- **Faster than 1.7.0**: on macOS `server.repeat_changed.*` takes 0.67 to 0.73 of 1.7.0's median
  (a reviewer's re-run: 0.67 to 0.74). The cause was not traced, and it does not show on Ubuntu or
  Windows.

A second run of the same workflow on the same commit (run 38390782413) agreed with the first in
direction on every latency claim above, and not in size. The runners drew other CPUs (Ubuntu an AMD
EPYC 9V74 where the first run had a 9V45, Windows an EPYC 7763 where it had a 9V74), every median
was 1.2 to 2.3 times the first run's, and the differences moved: a blocked Read cost 0.56 ms more
than in 1.7.0 on Ubuntu and 1.85 ms more on Windows, and the identity hook sat 0.13 ms and 0.82 ms
above the floor. A let-through Read on Windows cost 7.6% more than in 1.7.0 (2.7% in the first
run), and the floor was 4.0% apart in that run (0.5% in the first), so that runner was the
noisier one. Read the sizes from a shared runner as one draw, not as a constant.

## What is measured

| | what | how |
|---|---|---|
| MEASURED | token counts | the reply text of the real server, counted with the meter's tokenizer |
| MEASURED | latencies | a monotonic clock around the real binary |

No dollar figure. Until 1.9.0 each token table had a "modelled $/call" column, priced at
Opus-class rates with a fixed rounds-remaining figure from the author's own sessions. What a
saved token is worth depends on the turn that read the call's result, on its model, and on how
many rounds followed it before a compaction; `lumen_core::savings` derives all three from a
ledger's turns, and a corpus run has none. The results' `savings` section says so, and whether
the engine's token counts are calibrated to Claude's.

## The tokenizer is a proxy

Tokens are counted with `lumen_core::tokenizer::count_tokens`: `cl100k_base`, the same tokenizer
the meter uses. It is not Claude's tokenizer: on raw file text the models measured from Claude
Opus 4.7 on (Opus 4.7, Opus 4.8, Opus 5, Sonnet 5, Fable 5, Opus 5.5, Sonnet 5.5 and Haiku 5.5)
count about 1.59× as many tokens (1.44–1.70×), and the earlier models measured (Opus 4.6, Sonnet
4.6, Opus 4.5 and Haiku 4.5) about 1.24× as many (1.14–1.31×), from the 10th to the 90th
percentile of 60 samples measured against Claude's own usage data
([calibration](calibration/README.md), calibration of 2026-10-14; a `Read`'s line numbers and
the tool-result wrapper are not in these ratios). Counts compare one way of reading a file with
another; they are not a bill and not the number of tokens Claude would charge. The harness also
re-counts every reply it receives and stops if the server's own figure disagrees, and it
cross-checks each reply against the row the server wrote to its ledger.

## No headline

There is deliberately no overall "X% saved". Figures are per call and per file-size bucket.
How much a person saves overall depends on their usage mix: how many of their reads are large,
how many are repeats, how many are of files under the threshold.

## Corpus

Three repositories, each pinned by the full 40-character commit SHA. The harness fetches exactly
that commit (`git fetch --depth 1 <url> <sha>`), checks that the object that arrives is that
commit, and stops loudly otherwise. The corpus is cached outside the repository (default
`<system temp>/lumen-bench-cache`; `--cache DIR`; a cache inside the checkout is refused) and is
never committed.

| repository | license | commit | note |
|---|---|---|---|
| [lumen](https://github.com/HackPoint/lumen) | MIT | `3b951e93dbe55e46dc0e15951666b303110861e7` | this repository: Rust, plus the app's TypeScript; main before the benchmarks landed |
| [react-hook-form](https://github.com/react-hook-form/react-hook-form) | MIT | `8ac7cc0bcc1f50f887230d50b1929d977b4d3fb4` | TypeScript and TSX, tag v7.90.0 |
| [pytest](https://github.com/pytest-dev/pytest) | MIT | `cf470ec0bf7eb89cd97dd56df4859eae5db46447` | Python, tag 9.1.1 |

Measured files are `.rs`, `.py`, `.ts`, `.tsx` (not `.d.ts`), outside `node_modules`, `target`,
`dist`, `build`, `vendor`, `third_party`, `__pycache__`, `.yarn`, and valid UTF-8 (the others are
listed as skipped in the JSON). Files are read out of git's object store byte for byte
(`git cat-file --batch`), not from a working-tree checkout, so `core.autocrlf` and
`.gitattributes` cannot change a count: **there is no line-ending conversion anywhere**. Each
corpus has a fingerprint (SHA-256 over every measured file's path and git blob id) in the JSON.

`LUMEN_BENCH_SOURCE_<NAME>` (`LUMEN_BENCH_SOURCE_REACT_HOOK_FORM`, ...) names a URL or local path
to fetch from instead, for a mirror or an offline copy. What it serves must still be the pinned
commit.

## Method: tokens

The harness starts the real `lumen-mcp` binary, speaks newline-delimited JSON-RPC to it over
stdio (`initialize`, then `tools/call`), and for every file records the tokens of the reply in
five modes:

| mode | call | what it is |
|---|---|---|
| full | none | the file's own tokens: what a plain Read puts in context |
| outline | `smart_read(path)` | the first read, repeat reads off (`LUMEN_REPEAT_READS=off`; on is the default from 1.8.0) |
| recall | `recall_file(path, [one item])` | one representative item |
| stub | the second identical `smart_read(path)`, `LUMEN_REPEAT_READS=on` | "unchanged since you read it" |
| delta | a third `smart_read(path)` after one small edit, repeat reads on | only what changed |

Tool-result tokens are the reply text only. The JSON-RPC envelope and `_meta` are not counted,
exactly as the meter does not count them, so a real session pays a few dozen tokens more per
call, in every mode alike. Paths are sent relative to the corpus root (an outline repeats the
path, so an absolute path would make the figures depend on where the cache lives).

Per call the tables give the **median and p90 over files**, for each size bucket and each
language, for each mode, for both builds. Buckets are by line count (newlines, the count the
intercept hook compares to its 300-line threshold): `<300`, `300-999`, `1000-2999`, `3000+`.
Below 300 the hook lets a Read through, so that bucket shows what a `smart_read` costs on a file
that would not have been intercepted; for the shortest files the outline is longer than the file
and the server returns the file itself (route `would_inflate`). The routes tables show every
such fall-back; none is hidden.

Two selection rules, each a single deterministic rule:

- **Representative item** (recall): among a file's top-level functions, classes, structs, enums,
  traits, interfaces and named consts that span at least three lines and whose name (compared
  case-insensitively, as `recall_file` does) no other item shares, the one in the middle by line
  span (the lower median; ties by position). A file with no such item has no recall and no
  delta figure, so `n` differs between modes.
- **Small edit** (delta): a trailing comment (`// lumen-bench edit`, `# ...` in Python) on one
  line of that item, the line nearest its middle that still parses to the same outline. The
  edit is applied to a private copy of the file; the corpus cache is never written.

The first read with repeat reads on must be word for word the repeat-off outline, or the run
stops: otherwise stub and delta would not be comparable to the outline figures.

The per-file table (`*-tokens.tsv`) is byte-for-byte deterministic: given the corpus SHAs it is
identical on every run and every OS (the tokenizer, the files, the paths and the selection rules
do not vary), and the JSON carries a SHA-256 of each build's rows (`builds[].tokens.digest`),
computed with the `build` column held at `-` so a digest does not depend on a label. **Two runs
agree exactly when those digests are equal**, and two builds give the same token figures exactly
when their digests are equal. Everything else in the results (the `environment` block, the
latency figures, timestamps) varies.

### How repeat-read identity works, and what the harness does about it

The repeat-read store is in memory, keyed by (session, request). How a build learns the session
differs, so the harness does not assume it: before measuring, it **runs each build and asks**
(`driver::probe_identity`). It sends `hook intercept` a PreToolUse payload for
`mcp__lumen__smart_read` with a `tool_use_id` and a `session_id`, and looks for a record the build
left beside its ledger. No record: the build reads its session from `CLAUDE_CODE_SESSION_ID` when it
is spawned (v1.7.0 and older), and the harness gives every server it starts its own id
(`Identity::Env`, set in `Server::start`). A record: the build identifies each call from a record
written by the PreToolUse hook (the design of PR #45, `lumen_call_<tool_use_id>.json` beside the
ledger), and the harness plays Claude Code's part exactly: before every placed tool call it runs
the real `hook intercept` with a fresh `tool_use_id`, then sends the call with
`_meta["claudecode/toolUseId"]` set to that same id, so the server claims the record
(`Identity::Hook`). The probe is of behaviour, not of usage text: a build whose `--help` mentions
the hook but which records nothing is an Env build, and a build that records is a Hook build
whatever it prints. Which one each build was is in the JSON (`builds[].identity`) and in the
Builds table of the summary.

Two slots exist for the hook design, and both say what they are:

- `hook.per_call_identity`: the cost of the hook that records the identity: spawn `hook intercept`
  with that payload, wait for exit 0, require the record. For a build that records nothing, the
  slot says `n/a — this build records no per-call identity` and names the commit asked.
- `server.identity.placed` / `server.identity.unplaced`: the server's side of it, the cost of a
  `smart_read` with a record to claim against one with none (the unplaced call gets the full
  answer and remembers nothing), so the difference is the claim. Only Hook builds have them.

Both feed `hook.per_call_identity.spawn_minus_floor` and `.server_claim` in the derived figures.
A stub does not replace the stored base; a delta does.

## Method: latency

Each series is at least 100 recorded runs (default 200; `--runs N`), timed with
`std::time::Instant` (monotonic). Reported: median, p90, p99, min, max, in the JSON in
nanoseconds, plus the median of each pass on its own (`pass_medians_ns`, how much the machine
moved during the run). The percentiles are **nearest-rank**: of n sorted samples, the p-th
percentile is sample number ceil(p x n / 100), counting from 1, with no interpolation; the median
of an even n is the lower of the two middle samples. With the default 200 runs that makes the
median sample 100, the p90 sample 180 and the p99 sample 198, so the p99 is the third-slowest run
and one stray slow run does not move it. The raw samples are not written (a few hundred KB per
run); the five pass medians per series are. A server that has not answered in two minutes stops
the run.

The recorded runs are not taken in one block. The run is 5 passes over every series of both
builds; in each pass the builds take turns (the order flips every pass), and each series first
does `--warmup N` runs (default 20) that are executed the same way as the recorded ones and
discarded. The samples of the five passes are pooled per series. The reason is the machine: a
burst of other work (a build, a browser, another agent) on a laptop that is not idle used to
land on one series of one build and move its median by tens of percent; spread over passes, it
lands a little on every series of both builds. The baseline-against-this-checkout comparison is
therefore made in the same minutes, under the same conditions.

| series | what is timed |
|---|---|
| `hook.floor` | spawn the binary with a subcommand that does nothing, write the payload to stdin, wait for exit: the process cost every hook pays |
| `hook.intercept_pass.spawn` | PreToolUse Read of a file under 300 lines (exit 0), through `lumen-mcp hook intercept` |
| `hook.intercept_block.spawn` | the same for a file over the threshold, first Read of a session (exit 2) |
| `hook.meter_read.spawn`, `hook.meter_bash.spawn` | PostToolUse Read and Bash through `lumen-mcp hook meter`: count tokens, write one ledger row |
| `hook.*.inproc` | the same four hooks as function calls inside one warm process (`hook::run_with`) |
| `server.start`, `server.ping` | spawn plus `initialize`; a JSON-RPC round trip that does no work (the floor under every server series) |
| `server.smart_read.<bucket>`, `server.recall_file.<bucket>` | request written to reply line received |
| `server.repeat_unchanged.<bucket>` | `smart_read`, repeat reads on, file unchanged: the stub |
| `server.repeat_changed.<bucket>` | the same after one item changed: the delta (the file alternates between two contents, writes untimed) |
| `hook.per_call_identity` | the PreToolUse hook that records which session a Lumen MCP call is from: `hook intercept` on an `mcp__lumen__*` payload, record required. `n/a — this build records no per-call identity` for a build that does not |
| `server.identity.placed`, `server.identity.unplaced` | Hook-identity builds only: `smart_read` with a record to claim, and without one |

Process-spawn cost is separated from in-process cost: `*.spawn` includes the operating system
starting a process, `*.inproc` does not, and the summary also prints `spawn - floor`, which is
what the hook itself adds for a build whose hook cannot be called in-process (the harness links
only this checkout's hook code, so `*.inproc` exists only for a build made from the checkout, and
is `n/a` for the baseline and for any `--candidate-bin`). Every hook run
uses a fresh session id (the intercept keeps a once-per-session marker) and its exit code is
asserted; the ledger row counts are checked afterwards, so a series cannot time a call that did
not do its work. Server series use the ledger's routes the same way: a series whose calls took a
route other than the one named is marked `measured, mixed routes`. An untimed `tools/list`
follows each timed call, because the server writes its ledger row after it replies and would
otherwise bleed into the next sample.

The latency subjects are one Rust file per bucket below 3000 lines from the Lumen corpus: the
lower-median file by line count among those with a representative item and an edit. They are
listed in the JSON and the summary. Hooks only ever see source files, and the repository is
Rust, so latency does not depend on the other corpora.

Latency varies, and on a shared machine it varies more than 20%. Token counts are the same on
every run; latency is not, and this section says how far. All runs below are the full latency run (42 series medians
across both builds, 45 in 1.8.0's run) on one Apple M4 laptop that other work was using, so
its load average ranged from about 4 to 25 across them ("the committed run" is the first run
committed here, from the harness's development; 1.8.0's run replaced it):

| pair of runs (same code, one machine) | series medians within 20% |
|---|---|
| two latency-only runs 2 minutes apart, load 6 to 9 | 38 of 42 |
| two full runs 3 minutes apart, load 4 to 6 | 29 of 42 |
| a full run against a latency-only run 18 minutes earlier | 18 of 42 |
| the committed run against that same run, 21 minutes earlier | 15 of 42 |
| `compare` (100 runs per series) against the committed run, a few minutes later, 1-minute load 3.6 against the committed run's 7.4 | 39 of 42 as measured, 32 of 34 after dividing by the floor; the floors were within 5%, and `compare` said the machine matched |
| `compare` against 1.8.0's committed macOS run, two reviewers' re-runs, one about half an hour after the committed run started and one about an hour and a quarter after; 1-minute load 3.8 to 5.4 in the first and 5.0 to 8.0 in the second | 45 of 45 in the first and 42 of 45 in the second as measured (the three misses were in-process hooks that the re-run measured 21 to 23% lower; their pass medians span 13 to 27% of the median in the committed run); 37 of 37 after dividing by the floor in both; `compare` said the machine matched both times |

In the back-to-back pair every process-spawn series (`hook.*.spawn`, `hook.floor`,
`server.start`) and `server.ping` agreed within 7%; the four that did not were longer server
calls and one in-process hook. In the other pairs whole families moved together, including
`hook.floor` (a process that does nothing) and `server.ping` (a round trip that does nothing).
That is the sign that the machine changed between the runs (power and thermal state, the
operating system's security scans, other builds), not the code. A longer warm-up did not help;
pooling over passes (above) did. So: compare two latency runs only when their `hook.floor` and
`server.ping` medians agree, take them on the same machine at similar recorded load, and do not
compare a laptop with a CI runner. No claim is made here about how closely a re-run on a quiet
machine would agree: it has not been measured, and the table above is the only evidence.

### Comparing a fresh run with a committed one

```sh
benchmarks/run.sh compare benchmarks/results/<label>-<os>.json
```

measures the latency series again (the committed run's `--runs` and `--warmup`, the same builds
and subjects, on the Lumen corpus alone) and prints, per series, the median now against the
committed one:

- the **ratio** now / committed;
- the **floor-normalized ratio**: each median divided by its own run's floor first (`hook.floor`
  for the hooks and `server.start`, `server.ping` for the other server series), so a machine that
  is uniformly faster or slower drops out. The floors and the in-process hooks have no floor of
  their own kind and are compared as measured only;
- how many series are **within 20% both ways** (ratio 0.80 to 1.20), as measured and normalized;
- whether the **machine matches**: `hook.floor` and `server.ping` each within 15% of the committed
  run's, in every build. It says so in words, and when it does not it tells the reader to use the
  normalized ratios and not to read a raw difference as a difference in the code.

The load average at the start and end of each run is printed alongside. Series are matched by
build role and id, so a candidate built from a different commit is still compared with the
committed candidate; the output says both commits.

## Reading a result

- **Tokens**: compare `builds[].tokens.digest` and `*-tokens.tsv`. Equal digests, equal figures.
- **Latency**: compare medians (p90 and p99 move with load). The load average before and after
  the whole latency run is recorded; a run under heavy load is not comparable with one without.
- **Builds**: `builds[]` records the label, commit, profile, and the version the binary reports
  for itself. Both builds are made by the harness from source with the same toolchain: the baseline
  (v1.8.0, unless `--baseline-sha` names another) is `git archive`d from its commit
  (`12ad305679cf92b23167b032f56412a2a38e72a1`) into the cache and built there; the candidate is
  built from the checkout, or is a prebuilt binary given with
  `--candidate-bin PATH --candidate-sha <40 characters>` (the checkout is then not built; the
  result records the file's name, never where it is, and the commit is the caller's word). The
  repository has no `[profile.release]`, so both are cargo's default release profile, and the JSON
  says so.
- **Machine**: the `environment` block holds the OS and version, CPU model, logical and physical
  cores, RAM, power source, rustc and cargo versions, start time and load. A probe that fails is
  recorded as `null` with the reason in `probe_errors`, never as a default.

### Describing a machine when you quote a result

Give the OS and version, the CPU model and core count, the RAM, the build profile and the
commit that was built, say whether the machine was otherwise idle (the load averages are in the
file), and say whether it ran on mains or battery. Do not quote a latency figure without them.

## Limits

- Buckets are by line count only; a 2,000-line file of one-line statements and one of long
  functions share a bucket.
- One representative item per file, chosen by one rule. A different rule would give different
  recall and delta figures.
- Medians over files describe the corpus, not any one session. Three repositories in four
  languages are a sample, not the population of code.
- The outline, recall, stub and delta figures count the reply text only. Per-call envelope
  overhead is outside every figure.
- cl100k_base is a proxy (see above).
- Latency is warm-start: the ledger file, the page cache and the tokenizer's tables are warm.
  The first call after boot is slower.
- Ubuntu and Windows: only the latency part runs there (`--latency-only`: `run.ps1` on Windows,
  or the Benchmarks workflow). The committed runs are from GitHub-hosted runners, which are shared
  virtual machines with 4 logical cores. Compare a platform's two builds with each other, not one
  platform's latency with another's.

## The workflow

`.github/workflows/benchmarks.yml` is `workflow_dispatch` only (no push or pull-request trigger,
not a required check). It runs `--latency-only` on `ubuntu-latest` and `windows-latest`, uploads
each `results` directory as an artifact, and commits the result files (`<label>-<os>.json` and
`<label>-<os>-summary.md`) to a **new branch** `bench-results/<os>-<run id>`, as HackPoint, with
`permissions: contents: write` for that one push. A re-run of the same workflow run (GitHub's
"Re-run jobs") has the same run id, so from the second attempt on the branch is
`bench-results/<os>-<run id>-<attempt>` (`-2`, `-3`, ...); the first attempt has no suffix. That
keeps a re-run from colliding with the first attempt's branch. It never pushes to `main` or to the branch it
was dispatched from, and a push made with the workflow's own token starts no other workflow, so
nothing loops. Nothing is merged: the owner opens a pull request from that branch if the numbers
are worth keeping. The label defaults to `pre-release-<12 characters of the commit>`.
