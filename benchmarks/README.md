# Lumen benchmarks

Per-call token and latency figures for Lumen's read tools and hooks, measured through the real
`lumen-mcp` binary on a pinned corpus, for two builds side by side: the last release (v1.7.0)
and the checkout you run it from. Everything is reproduced by one command.

```sh
benchmarks/run.sh                 # macOS / Linux: tokens and latency, v1.7.0 against this checkout
benchmarks\run.ps1 --latency-only # Windows (PowerShell)
cargo run --release --locked -p lumen-bench -- --help
```

`run.sh` is `cargo run --release --locked -p lumen-bench -- "$@"`. The harness is the
`lumen-bench` crate in `benchmarks/harness`. It is a workspace member that nothing shipped
depends on: no runtime dependency was added to `lumen-core`, `lumen-mcp` or any binary the hooks
run, and it is not part of the app bundle. Its fast smoke test (a four-file fixture, no network,
a few seconds) runs with the rest of `cargo test --workspace`; the corpus run does not.

A run needs `git`, a Rust toolchain, network access to fetch the corpus (or a local mirror, see
below), about 1 GiB of disk for the cache (the corpus plus the v1.7.0 build) and another GiB or
so for this checkout's release build, and a clean checkout: the
candidate build refuses uncommitted changes outside `benchmarks/results` (override with
`--allow-dirty` for a throwaway run, which the output then marks as a dirty tree). The full run
takes roughly 10 to 15 minutes on an idle laptop, most of it the v1.7.0 build and the latency
series.

## Outputs

| file | content |
|---|---|
| `results/<label>-<os>.json` | everything, machine-readable, stable key order, with an `environment` block |
| `results/<label>-summary.md` | the tables a person reads |
| `results/<label>-tokens.tsv` | one row per file and build: every token figure, deterministic |

`--label` names them. A label of the form `MAJOR.MINOR.PATCH` is a release result; anything else
(the default is `current-main`) is stamped **pre-release** in the JSON (`"pre_release": true`) and
at the top of the summary. The files committed here under `current-main-*` are a measurement of
main before 1.8.0 and are not a 1.8.0 result.

## MEASURED and MODELLED

| | what | how |
|---|---|---|
| MEASURED | token counts | the reply text of the real server, counted with the meter's tokenizer |
| MEASURED | latencies | a monotonic clock around the real binary |
| MODELLED | every dollar figure | the economics model in `crates/lumen-core/src/econ.rs`, applied to measured token counts |

A dollar figure is never a measurement and is always labelled `modelled`. The model's rates are
`rates.rs`: USD per million tokens, input 5, output 25, cache read 0.5, cache write 6.25, which
that file labels "Sonnet 3.5 / 3.7 / 4 pricing". The harness does not check them against a
current price list. The model's other inputs (context size, output per round, rounds remaining)
are `Econ::default()`, averages over the author's own sessions; another person's profile differs.
The value of one saved token is `(cache_write + cache_read x rounds_remaining) / 1e6` dollars,
and a "$/call" cell is the median saved tokens of that call times that value.

## The tokenizer is a proxy

Tokens are counted with `lumen_core::tokenizer::count_tokens`: `cl100k_base`, the same tokenizer
the meter uses. It is not Claude's tokenizer. Counts compare one way of reading a file with
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
| outline | `smart_read(path)` | the first read, repeat reads off (the shipped default) |
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

### How repeat-read identity works today

The repeat-read store is in memory, keyed by (session, request). The session is
`CLAUDE_CODE_SESSION_ID`, read once from the server's environment when it is spawned and fixed
for its life. The harness therefore gives every server it starts its own session id, set in one
function, `set_session_identity` in `benchmarks/harness/src/driver.rs`. If identity moves to a
live id handed over by a hook before each call, that is the only function to adapt (its doc
comment says how), and the "per-call identity hook overhead" latency slot (below) then gets a
figure. A stub does not replace the stored base; a delta does.

## Method: latency

Each series is at least 100 recorded runs (default 200; `--runs N`), timed with
`std::time::Instant` (monotonic). Reported: median, p90, p99, min, max, in the JSON in
nanoseconds. A server that has not answered in two minutes stops the run.

The recorded runs are not taken in one block. The run is 5 passes over every series of both
builds; in each pass the builds take turns (the order flips every pass), and each series first
does `--warmup N` runs (default 20) that are executed the same way as the recorded ones and
discarded. The samples of the five passes are pooled per series. The reason is the machine: a
burst of other work (a build, a browser, another agent) on a laptop that is not idle used to
land on one series of one build and move its median by tens of percent; spread over passes, it
lands a little on every series of both builds. The v1.7.0-against-this-checkout comparison is
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
| `hook.per_call_identity` | placeholder for task T1's PreToolUse hook on Lumen MCP tools: `n/a at <sha>` while the code path does not exist |

Process-spawn cost is separated from in-process cost: `*.spawn` includes the operating system
starting a process, `*.inproc` does not, and the summary also prints `spawn - floor`, which is
what the hook itself adds for a build whose hook cannot be called in-process (the baseline: the
harness links only the candidate's hook code, so `*.inproc` is `n/a` for v1.7.0). Every hook run
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

Latency varies. A reviewer should accept a re-run within 20% on the same machine at similar
load, and should not compare a laptop with a CI runner. Measured repeatability: two full latency
runs back to back on the Apple M4 laptop of the first run, while other work kept its load
average at 6 to 9, agreed within 20% on 38 of the 42 series medians. Every process-spawn series
(`hook.*.spawn`, `hook.floor`, `server.start`) and `server.ping` agreed within 7%. The four that
did not were the longer server calls and one in-process hook
(`server.smart_read.1000-2999` on v1.7.0, `server.repeat_changed.1000-2999` and
`server.repeat_unchanged.300-999` on the current build, `hook.intercept_block.inproc`); which
series move differs from run to run. On an idle machine expect better; under load, compare
medians only between runs whose recorded load is alike.

## Reading a result

- **Tokens**: compare `builds[].tokens.digest` and `*-tokens.tsv`. Equal digests, equal figures.
- **Latency**: compare medians (p90 and p99 move with load). The load average before and after
  the whole latency run is recorded; a run under heavy load is not comparable with one without.
- **Builds**: `builds[]` records the label, commit, profile, and the version the binary reports
  for itself. Both builds are made by the harness from source with the same toolchain: v1.7.0 is
  `git archive`d from commit `5d1ec0408ad25edcbae43cf1a6ecbc0ddba47c94` into the cache and built
  there; the candidate is built from the checkout. The repository has no `[profile.release]`, so
  both are cargo's default release profile, and the JSON says so.
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
- The dollar figures are a model of the author's usage with unverified rates; they say what a
  saved token is worth under that model, not what anyone was charged.
- Windows: the latency part is meant to run there (`run.ps1`, or the Benchmarks workflow); the
  committed first run is from macOS.

## The workflow

`.github/workflows/benchmarks.yml` is `workflow_dispatch` only (no push or pull-request trigger,
not a required check). It runs `--latency-only` on `ubuntu-latest` and `windows-latest` and
uploads each `results` directory as an artifact. It commits nothing back.
