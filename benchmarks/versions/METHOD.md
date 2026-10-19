# Version over version: method

Every published Lumen version from 1.0.0 to 1.9.0, and no Lumen, measured the same way on the same
inputs. This file and `manifest.toml` are committed before any run.

Versions: 1.0.0, 1.1.5, 1.2.4, 1.3.1, 1.4.0, 1.5.1, 1.6.0, 1.7.0, 1.8.0, 1.9.0, plus **no Lumen**.
(The newest patch release of each minor line, plus 1.0.0.)

## Honesty rule

- The versions, the metrics, the capability map and the rules below are fixed before a run. The
  map is a claim the probes check (see Capability map); a probe that disagrees stops the run and
  the map is corrected in a commit of its own, never silently.
- Every result is published, including every cell that is worse.
- **A newer version that is worse than an older one on any metric is published as worse and opens
  an issue** in HackPoint/lumen with the metric, both values, both versions and the likely cause.
  A row is never left out because it is unflattering. A regression inside the run-to-run noise
  (the min-max ranges overlap) is reported as equal, not as worse.
- In the E2E subset a version that is cheaper but lowers task success does not win the row
  (the row rules of `benchmarks/competitors/METHOD.md`).
- No claim without a row; signed savings (a loss stays negative); no exactness claim about token
  counts, which are cl100k re-counts.

## Assets only, never a rebuild

Each version is the file Lumen published for it (`manifest.toml`: the asset name and its sha256
per OS, from `gh release view v<version> -R HackPoint/lumen --json assets`). The harness
downloads it, refuses on a sha256 mismatch, and extracts the sidecars into a scratch directory;
it never compiles an old tag. Provenance in every result is the asset name and sha256, never a
path.

| OS | asset | how it is opened |
|---|---|---|
| macOS arm64, every tag | `Lumen_<v>_aarch64.dmg` | `hdiutil attach -readonly -nobrowse -mountpoint <scratch>`; copy `Lumen.app/Contents/MacOS/{lumen-mcp,lumen-daemon,lumen-tok}` |
| Linux x86_64, 1.1.5 and later | `lumen_<v>_amd64.deb` | `ar x`, then the data tarball: `usr/bin/*` |
| Windows x64, every tag | `Lumen_<v>_x64-setup.exe` (NSIS) | `7z x`; the installer is never run |

The `lumen` CLI archives hold only the CLI; `lumen-mcp`, `lumen-daemon` and `lumen-tok` are Tauri
sidecars shipped only inside the app assets. **1.0.0 has no Linux app asset**, so every Linux cell
of 1.0.0 is `n/a` with that reason.

Hooks up to 1.5.1 are bash and python3 scripts. They are run as committed at the version's tag
(`.claude/hooks/*.sh`, interpreted, not rebuilt) with `LUMEN_TOK` pointing at the asset's
`lumen-tok`, and labelled "script hooks". From 1.6.0 the hooks are `lumen-mcp hook ...`. On a
binary older than 1.6.0 `lumen-mcp hook intercept` silently starts an MCP server, so the harness
never spawns `hook` where the manifest says `hook_cli = false`.

## Scoring with today's engine

Every version's replies are scored by the same engine, the one in the checkout under test
(`lumen-core`, `savings::Ledger`): per flow a ledger is built from `CallRow`s of the measured
replies (the full file and the returned text both re-counted with today's cl100k counter) and
`TurnRow`s of a reference session, then `summarize(..., &Calibrated)` gives tokens saved, value in
dollars, the extra-round cost and the net. So an old version is not scored by the arithmetic it
shipped with: a version's own self-reported saving is recorded where it exists and not used. For
1.0.0, whose `saved_tokens` is clamped at zero, the signed saving is recomputed from the
re-counted reply. A build is identified by its asset sha256, not by what it reports
(`serverInfo.version` of 1.0.0 says 0.2.0).

**Reference sessions** (labelled MODELLED in every cell; `reference_session.toml`):
- **Primary:** `claude-sonnet-5-5` at the prices in `crates/lumen-core/src/rates.rs` (`PRICES`, as of
  2026-10-13: $2 input, $10 output, $2.50 five minute cache write, $0.10 cache read per million),
  with the context and output sizes lumen-core measured on 19,161 main-agent turns
  (`econ::DEFAULT_CONTEXT_TOKENS` = 362,965 and `DEFAULT_OUTPUT_TOKENS` = 1,085, read from
  lumen-core, not copied). The prompt is split 10 uncached, a cache write (five minute) equal to the
  prior round's output (1,085; this write is modelled here, since `econ.rs` has no write term, and
  it adds about $0.0027 a round on Sonnet 5.5, against Lumen) and the rest read from the cache
  (361,870), as in `Econ::round_cost`. The primary is measured, not chosen, and it is the
  conservative choice for Lumen: an extra round costs what a whole prompt of that size costs, about
  $0.05 on Sonnet 5.5 (0.049770), so a saving has to pay for the rounds it causes.
- **Sensitivities, always shown beside the primary:** Opus 5.5 and Haiku 5.5 at the primary sizes,
  and a **small prompt** (60,000-token prompt, 400 output tokens, 400 written, 59,590 read: a round of
  about $0.011). The small prompt flatters Lumen and is labelled so; it is never the headline.
- The session closed more than an hour before the clock, with `events_from` earlier than its
  start, so the cache term is not a lower bound.
- Two values of R (the reread term): R = 0, the floor, and R = the median observed in the no-Lumen
  E2E arm once it exists. Haiku 5.5 has no price in the engine today, so its dollar cells are
  `n/a`, never 0.

Flows: outline; outline then recall (the engine charges the follow-up round); outline then stub;
edit then delta; for files of 300 lines or more, a blocked Read then `smart_read`, with a recorded
block row so the block's round is on evidence.

## Metrics and how each is measured

All through the real binaries of the asset, on the committed corpus of the existing harness
(`benchmarks/README.md`), in a sandbox (below).

| group | metric | measured |
|---|---|---|
| tokens | `smart_read` outline, `recall_file`, `compress_logs` | tokens returned against tokens of the full file, today's cl100k, per file and in total; signed |
| tokens | stub and delta | tokens of the repeat-read stub and of an edit's delta against a full re-read |
| tokens | `server.repeat_*` | repeat reads, the flow above |
| dollars | per flow | `value_usd`, `extra_round_usd`, `net_usd` from the engine, for the reference session; R = 0 and R = measured |
| hooks | `hook.intercept_block.spawn`, `hook.intercept_pass.spawn`, `hook.floor` | wall time of one spawned hook process, a large file's Read blocked, a small file's let through, and a hook that does nothing |
| hooks | `hook.meter_bash` | the Bash meter hook |
| hooks | `hook.per_call_identity` | the identity hook before a Lumen call |
| server | `server.smart_read`, `server.recall_file` (by file size class), `server.identity.*` | request to reply on a running `lumen-mcp`, over stdio |
| server | `*.inproc` | not measured for any prebuilt version (it needs the library linked) |
| daemon | ingest throughput | a seeded synthetic Claude Code transcript set (about 40 MB; the generator is committed, no real transcripts). The daemon starts on an empty projects directory, is ready, the set is moved in, and `COUNT(*) FROM turns` is polled every 50 ms until it equals the set's turn count (or, for a version that drops or adds turns, until unchanged for 10 s; that run is marked incomplete). Bytes per second, turns per second, rows per table; 5 fresh runs, median and min-max |
| startup | cold start, 30 runs each | daemon spawn to port accepting and schema present; `lumen-mcp` spawn to `initialize` replied; first hook spawn. Linux: a true cold start with the page cache dropped (`sudo -n`); macOS and Windows: labelled "warm file cache" |
| resources | idle CPU and memory | the daemon empty and after ingest, and an idle `lumen-mcp`: 30 s to settle, then once a second for 120 s (and on until 60 samples are in); CPU seconds per minute, RSS median and peak |
| E2E | success, dollars, rounds | the subset below |

Latency series report the median and the min-max of the runs. "Worse" and "equal" use that
range: overlapping ranges are equal.

## What is worse

A newer version is compared with the version immediately before it in the manifest (never with
no Lumen, which is a reference and not an earlier release). Both cells must hold a figure: an
`n/a` cell, a failed cell, or a pair that is not comparable (a script hook against a binary hook)
is never compared. "Worse" is defined per metric, so noise is not called a regression:

| metric | worse when |
|---|---|
| tokens returned per call, per tool and size bucket | the median over the files is higher |
| net dollars per call, per tool and size bucket | the median over the files is lower (for a repeat read and a blocked Read the unit is the flow, see below) |
| latency p50 (hooks and server) | the runs separate: every pass median of the newer build is above every pass median of the older build (the release bench's rule). p90 and p99 are shown with their changes and not judged: only the median has pass-level evidence |
| ingest throughput (bytes and turns per second) | the whole min-max of the newer runs is below the whole min-max of the older runs |
| cold start | the whole min-max of the newer runs is above that of the older runs |
| idle RSS | the newer median is above the older peak (the run's own median-to-peak range does not overlap) |
| idle CPU | one window per process has no range, so a fixed margin: higher by more than 25% and by more than 0.05 CPU seconds a minute |

Anything else is `equal` (overlapping ranges, a change inside the margin) or `better`. An ingest
in which any run did not finish is a failed cell, `incomplete: N of M turns` for the run with the
fewest turns, and is never compared. A hook figure of a script-hook version (up to 1.5.1) is
labelled `(script)` and is compared only with another script-hook version.

The engine prices a repeat-read stub and an edit delta against what Lumen returned the first
time, so its line for them moves with the outline. They are therefore scored as the flow (the
outline and the repeat read, two calls), as is a blocked Read followed by an outline, so that a
better outline is never shown as a worse stub. The outline and `recall_file` rows are one call.
The delta of the edit flow is scored with the file's own full count, not the edited file's (a
few tokens apart).

Every regression is listed in the report with the metric, both versions, both values, the
evidence and where to look for the cause (a pointer: the run times and counts, it does not trace),
and the `gh issue create -R HackPoint/lumen ...` command that files it is printed in the report
and by the command. The harness never runs `gh`: the issues are filed after review.

## The one command and the table

`benchmarks/versions/run.sh` (`run.ps1` on Windows), which is `lumen-bench versions all`:

1. fetches the pinned asset of every version for this OS (sha256 checked, never rebuilt), and
   refuses on a mismatch;
2. measures every version and no Lumen one after the other, in the sandbox, under the real-ledger
   guard: the token figures, the latency series (binary hooks, script hooks, server), and the
   daemon (ingest, cold start, idle CPU and memory);
3. writes `results/<os>.json` (the measurements, the only input of the report) and
   `results/<os>.md` (the version x metric table rendered from that JSON, then the regressions
   and the issue commands). `versions report FILE` renders a JSON again, byte for byte.

`--dry-run` prints what would be fetched and measured and writes nothing. A setting other than
the default (`--runs`, `--ingest-runs`, `--idle-window`, ...) or a subset of the versions is
written into the results as a note, so a smoke run is never mistaken for the benchmark.

The table has one row per metric and one column per version, no Lumen first. Each figure carries
its change against the previous version and against no Lumen (dollars as a difference, the rest as
a percentage); `n/a - <reason>` cells carry no change. Dollars are the primary MODELLED reference
session at R = 0 and, once the no-Lumen E2E arm has run, at the R it measured (`--measured-r`);
until then the report says "not available yet". `compress_logs` is not measured by the token
runner and has no row.

## n/a rules

A cell that cannot exist shows `n/a - <reason>`, never 0, and never enters a delta or a ranking:

- `hook.meter_bash` before 1.6.0 (no Bash meter, no `would_inflate`);
- `hook.per_call_identity` and `server.identity.*` before 1.8.0 (identity is an environment value
  in 1.6.0 and 1.7.0 and absent before);
- stub and delta tokens and `server.repeat_*` before 1.7.0; 1.7.0 is measured with repeat reads on
  (`LUMEN_REPEAT_READS`) and labelled "opt-in";
- script hooks on Windows (bash and python3 scripts);
- `*.inproc` for every prebuilt build;
- every Linux cell of 1.0.0 (no asset);
- 1.0.0's daemon when port 9999 is taken (it binds `127.0.0.1:9999` fixed) - the run refuses
  and tells the user to quit Lumen;
- for no Lumen: hooks and the daemon do not exist, tokens are the full file (with the Read cap),
  and dollars are 0 by definition, shown as the baseline and not as a saving.

What is not n/a: before 1.6.0 an outline longer than the file is returned as is. That is a
measured loss and is published as a negative saving.

## Capability map

`manifest.toml` records, per version, the claims the harness relies on: whether `saved_tokens` is
clamped or signed, the hook kind, whether the binary has the `hook` subcommand, the Bash meter,
repeat reads (none, opt-in, default), identity (none, env, hook), and which `LUMEN_*` variables the
daemon reads. These are checked, not trusted: the identity probe runs only where the map says the
version has `hook`; a probe that disagrees with the map stops the run; the ledger cross-check
queries only the columns `PRAGMA table_info` shows.

## Sandbox and safety

- Every binary runs with a scratch `HOME`, `USERPROFILE`, `APPDATA`, `XDG_*` and a scratch working
  directory (1.0.0 puts `lumen.db` in the cwd by default). `LUMEN_DB`, `LUMEN_PROJECTS_DIR`,
  `LUMEN_FAULT_SPOOL` and `LUMEN_LOG` point into scratch, `LUMEN_WS_ADDR` to a free port,
  `LUMEN_UPDATE_CHECK=0` (the only value that disables the check), and inherited `LUMEN_*` and
  `CLAUDE*` variables are removed. The environment is an allowlist (`PATH`, locale, and on
  Windows the system variables), not a blocklist: a proxy, a key or a tool's own `*_HOME` cannot
  slip through by not matching a pattern. `harness/src/sandbox.rs` is the one place that does it.
- **Real-ledger guard:** before and after a run the real ledgers (the legacy data
  directory, which is the app identifier before 1.10.0, the current data directory, and
  `~/.lumen_db_path`) are opened read-only and must hold zero rows whose session starts with
  `lumen-bench-` (or the harness's own `bench-`; a real session id is a UUID). A hit stops the
  run. The ledgers are opened with a `?mode=ro` URI and a missing one is skipped, never created. The legacy path is taken from lumen-core's
  `legacy::LEGACY_APP_ID`, never written as a literal in the harness or in these files.
- 1.0.0's daemon reads `$HOME/.claude/projects` and its fixed port can collide with a running
  Lumen app, whose GUI could then talk to the bench daemon: the port is checked free first.
- A downloaded asset is untrusted: verified by sha256, unpacked into a fresh directory, never run
  from inside it, installers never executed. No telemetry, nothing sent from a run.
- Public results carry no home paths, usernames, employer names or identifiers.

## The E2E subset

Four Lumen arms and the baseline, a subset of the shared design of
`benchmarks/competitors/METHOD.md` (same tasks, same runs, same row rules, same sandbox):

| arm | note |
|---|---|
| no Lumen | shared with the competitor report |
| Lumen 1.1.5 | stands in for 1.0.0, which has no Linux build; labelled in every table |
| Lumen 1.6.0 | the first version with `lumen-mcp hook` |
| Lumen 1.8.0 | identity hook |
| Lumen 1.9.0 | shared with the competitor report; the version under release |

The no-Lumen and 1.9.0 runs are done once and cited by both reports. The other versions have no
E2E arm; their rows in that group are `n/a - not in the E2E subset`, never 0.
