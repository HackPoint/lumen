# Changelog

## [1.4.0] — 2026-07-29

### The headline is dollars now, and the token ratio is demoted to an input

87% fewer tokens per intercepted read was the hero metric, and it flattered the product.
A ratio cannot be wrong in the direction that matters: an intercepted read is a *blocked*
read, so the model spends an extra round calling a Lumen tool instead, and a smaller reply
that forces another round is a loss however good the ratio looks.

The Optimizer screen now leads with the net dollar value — the tokens avoided, priced,
less the rounds they cost — and keeps the token ratio underneath it. The README's 87%
figure is replaced the same way.

The regression test for this is a call that saves 300 of 400 tokens: a 75% ratio, and a
dollar loss. It would have rendered as a success under the old headline.

**Published under a pre-committed rule: whatever the number is, it renders.** A negative
result shows as negative with no softening — asserted by a test that fails if the words
"but", "still", "however", "nonetheless" or "despite" appear near it. A result within a
dollar of zero says *roughly break-even* rather than rounding into a win. And when there
are too few recorded turns to price a round, no figure is claimed at all, because a gross
figure with no cost beside it is the exact overstatement being removed.

Measured on the author's machine: **+$276 over 291 attributable calls, about +$0.95 each.**
The sign is robust across every plausible value of `R`; the magnitude is not, spanning
+$31 to +$368 on that one input, so `R` is shown in the UI rather than hidden. `smart_read`
taken alone is roughly break-even — the surplus comes from `recall_file`.

### Hotspots: where your context actually goes

A new screen answering a question Lumen could always have answered and never did. Top
files by cumulative tokens read, with read counts, share of all context, and how much of
each file's reading found it **unchanged** since the previous read — context re-acquired
rather than retained.

On this repository: `Run.tsx`, **139 reads, 3.83M tokens, 20.8% of everything read**, and
the top ten files account for 40.1% of 18.4M tokens across 1,189 files. Where the numbers
warrant it the screen says what to do — a 3,833-line file read 139 times is a refactor
candidate, and no read optimisation beats splitting it.

It is framed as diagnosis, not savings, and the framing is load-bearing. It costs zero
tokens, intercepts nothing and forces no rounds, so it is the only figure in the product
that cannot come out negative. A test enforces the copy: the screen may not claim to have
saved anything.

The unchanged-read signal is a proxy, and labelled as one. The direct measure would be
"re-read after a compaction", but compaction is recorded in the transcript rather than the
database; `file_mtime` equality answers the same question from data that is present. Rows
predating `file_mtime` are excluded rather than assumed unchanged.

### Notes

- 537 Rust tests, 268 frontend.
- `R` is a measured constant (194) in the UI, not per-call. Per-call `R` needs the
  transcript replay in `scripts/lumen_percall.py`, which cannot run inside a tool call.
- The published figure covers the 291 calls that could be attributed, and is **not scaled
  up** to the 1,470 in the ledger.
- The measurement window has not started: it needs the `Bash` matcher registered (a Setup
  press) and MCP-side `req_key` flowing (a Claude Code restart). Both were still absent at
  release, so no A/B data exists yet and none is claimed.

## [1.3.1] — 2026-07-29

### The budget was a floor mistaken for a target, which disabled the ranking

`budget = full_tokens − S_min` was wrong. `S_min` is a floor on the **saving** — what the
outline must not exceed if the call is to pay for itself. It says nothing about what the
outline should aim for, because net value rises monotonically as the outline shrinks: a
cost formula cannot bound an outline from below.

The consequence was not subtle. On a 39,281-token file the budget came out at 33,897, so
all 222 definitions fitted and `k = n` — the ranking selected nothing, and the A/B would
have compared two unranked outlines. The same file now produces **785 tokens at k=46/222**.
Across the repository the budget binds in **9 of 12** qualifying files; the three where it
does not are files whose entire outline already fits, which is correct.

```
budget = min(full_tokens − S_min, target_outline)     target_outline = 800
```

The real lower bound is sufficiency, which is empirical, not derivable — hence a tuning
parameter, lowered while follow-up rate stays flat and reverted on the first rise. Set it
with `LUMEN_TARGET_OUTLINE`; it is recorded on every row, so rows produced under different
sweep values are never pooled.

**One correction to the target's justification.** 1,439 tokens is the *largest* legacy
outline (`setup.rs`), not a typical one. The legacy median across qualifying files is ~401
tokens and the mean ~550, so 800 starts the sweep *above* the status quo, not below it. That
is still the safe direction — begin generous and come down — but at 800 the ranked arm
costs about 13% more than legacy (7,450 vs 6,595 tokens across 12 files), so the first
sweep step should be expected to close a gap rather than open a lead.

The refusal arm is untouched: `3,216 → 294, refused` is the part that pays.

### Per-call economics, measured jointly — and the follow-up rate

`scripts/lumen_percall.py` computes `(R, round cost)` as a **joint pair per call**, both
from the same position in the same session, which retires the mean-versus-median question:
no average has to be chosen when every call carries its own.

Two findings changed the picture.

**R is bounded by compaction, and that is worth a factor of three.** A saved token stops
paying the moment the context is rebuilt. Counting to the end of the session gave a
call-weighted median R of **658**; bounding it at the next `isCompactSummary` gives
**194–249**. Both are far above the 65 previously assumed, which was a session-length
median rather than a call-weighted one — calls concentrate in long sessions.

**60.4% of `smart_read` calls are followed by a `recall_file` on the same file**, at a
median gap of 3 rounds. The pair multiplier is therefore **1.604 rounds per intercept**, and
the cost side now carries it.

That 60.4% needs reading carefully: outline-then-fetch is Lumen's *documented* workflow —
`smart_read`'s own tool description says to follow it with `recall_file`. So a follow-up is
not by itself a failure, and the A/B question is not "does trimming cause follow-ups" but
"does trimming push the rate above the baseline the two-step design already implies."

### The dollar figure

Net over the 291 attributable calls, with each call's own R and its own round cost, times
the measured pair multiplier:

| route | calls | gross | round cost | **net** | paid for its own round |
|---|---|---|---|---|---|
| `smart_read` | 53 | $39.83 | $14.93 | **+$24.90** | 62.3% |
| `recall_file` | 238 | $346.72 | $95.69 | **+$251.04** | 51.7% |
| | | | | **+$275.93** | |

About **+$0.95 per call**. The sign is robust: positive at every plausible R, from +$31 at
R=65 to +$368 at R=249. The magnitude is not — it spans an order of magnitude on that one
input, and `smart_read` taken alone is break-even to slightly negative at the low end.

### Notes

- **Only 291 of 1,470 ledger calls are attributable.** The transcripts on disk are a subset,
  so this is a per-call figure over what can be attributed, not a total. It is not scaled up.
- **One approximation, stated rather than buried.** Ledger savings and transcript economics
  are the same calls seen from two sides but cannot be joined without `req_key`, so each
  call's saving is matched to a call's economics at the same quantile.
- The compaction marker is a `user` record carrying `isCompactSummary` with no assistant
  message id, so an id lookup finds zero of them. It is located by file order instead — the
  first attempt silently measured no compactions at all.
- 522 Rust tests, 251 frontend.

## [1.3.0] — 2026-07-29

### Ranked, budget-aware outline — implemented, shipped **off**, and measured

`smart_read`'s outline can now be sized by the economics of the call instead of by a fixed
format. An intercepted read costs exactly one extra round, so an outline is worth returning
only when it saves more than that round costs:

```
S_min = (C × 0.5 + O × 25) / (6.25 + 0.5·R)      budget = full_tokens − S_min
```

`C` and `O` come from your own `turns` history when there is enough of it, and the row
records which. Files that cannot clear the bar are refused, with the refusal and the
numbers behind it written to the ledger.

Enable with `LUMEN_RANKED_OUTLINE=on`, or `=ab` to run both arms split by a stable hash of
the path. **Unset — the default — changes nothing.** Anything unrecognised is also off: a
typo must not enable an experiment.

### Read this before enabling it: the ranking is not the win

Measured across this repository, the ranked outline returns **73% more** tokens than the
outline it replaces — 10,759 against 6,205 over the files that qualify. It captures nested
definitions the old outline never did, and the budget is usually generous enough that
nothing is trimmed at all: **52 of 56** definitions included at the measured context, **37
of 37** at 100k. The ranking machinery is close to inert at current economics.

What pays is the **refusal**. Replaying the ledger, declining calls that cannot clear
`S_min` would have moved `smart_read` from **−$24.41 to +$15.09** and `recall_file` from
**+$90.29 to +$159.24** — about **+$108** by not making 831 of 1,470 calls. That is a gate,
not a trimmer, and its proper home is the `PreToolUse` threshold rather than `smart_read`:
by the time `smart_read` runs, the extra round is already spent. That threshold is
deliberately untouched here.

Also worth correcting: the premise that outlines cost ~1,300–1,600 tokens is `recall_file`'s
figure. `smart_read`'s median return is **418** tokens, and it has **zero** net-negative
rows in 421 calls. The losses were dollar-negative — a saving worth less than the round it
forced — not token-negative.

### Upstream tag queries turned out to be the wrong tool

They index symbols for jump-to-definition, not scope for outlining.

- **TypeScript**: upstream captures only declaration forms, so ordinary `class`,
  `function`, `method_definition` and every call yield nothing — measured at **0
  definitions in a 652-line Angular service** and 0 in a 57-line component. A ranked
  outline of the frontend would have been empty.
- **Rust**: `impl` blocks are `@reference.implementation`, leaving every method with no
  container — `fn new` with nothing saying what it constructs.

Both are supplemented by queries authored in-repo rather than copied, which satisfies the
reason upstream was preferred (staying MIT-clean; copying from an Apache-2.0 source is what
that ruled out). That service went from 0 to **56** definitions.

### Fixes and deviations found while building it

- **`Query::new` compiles the pattern set** and cost 9–16 ms of a 10 ms budget, so the
  feature timed out on any file worth outlining. Compiled once per process now: the whole
  pipeline is 2 ms at 421 lines, 7 ms at 1,433, 17 ms at 4,198 — parse-dominated.
- **The wall-clock ceiling is 50 ms, not 10.** What it guards against is returning the
  whole file, which costs the model far more than 50 ms in latency and tokens; 10 ms
  rejected exactly the large files an outline helps most. Ten times that in debug builds,
  whose constant factor is 3–5× and is not what the ceiling is calibrated against.
  Overridable with `LUMEN_RANKED_TIME_BUDGET_MS`: a deadline is a property of the machine,
  not of the code, and without the override the tests declined as `TooSlow` on CI runners
  while passing locally — a test measuring the runner rather than the feature.
- **The A/B split was 100/0.** FNV-1a's lowest bit is close to the XOR of its input bytes,
  so on structured paths `hash % 2` put **every one of 4,000 generated paths in the same
  arm** while reporting a 50/50 design. A splitmix64 finalizer brings it to 0.509, and the
  assignment of four real paths is pinned so a future hash change fails a test instead of
  silently re-randomising an experiment in flight.
- **The inflation guard is unreachable.** `budget = full − S_min` with `S_min > 0` and the
  fit never exceeding budget means `returned < full` by construction. It is kept as a
  backstop against a future change to the budget rule, and the test asserts that invariant
  rather than faking a trigger.
- **The cache key omits the budget**, departing from the specification. Bucketing the
  budget was meant to stop context fluctuations thrashing the cache — but what is cached is
  the tag extraction, which does not depend on the budget at all, so including it would
  cause the very thrashing it was meant to prevent. `mtime` **and** `size` are both in the
  key: this runs inside a tool call where a file can be written and re-read within one
  second, which second-resolution mtime cannot see.

### Notes

- 519 Rust tests (+18 in this release), 251 frontend. Red-then-green on the export prior;
  the budget-guard test initially passed with the guard deleted — `fit_budget` returns k=0
  for a non-positive budget and reports the same verdict — and now asserts the ordering
  property only the guard provides.
- Nine columns added to `read_events`: `budget`, `s_min`, `econ_context`, `econ_rounds`,
  `econ_output`, `econ_source`, `k_selected`, `n_total`, `coeff_version`. NULL on every row
  not produced by this path, including all hook-written rows — a zero would claim a
  decision was made.
- `k_selected = n_total` on a row is the signal that the budget did not bind and the
  ranking had no effect. That is the number to watch if you run the experiment.
- `R` is still the measured default of 65, not derived per call. Doing that needs the
  `(R, round cost)` pairing `req_key` was added to make possible.
- `MIN_USEFUL_OUTLINE = 120` tokens is an unmeasured constant introduced here, and it
  decides the positive-but-too-small band.

## [1.2.4] — 2026-07-29

### Setup reported healthy hooks while the Bash meter was not installed

Verifying 1.2.3 on a real install turned up the gap. `~/.claude/settings.json` still
carried the pre-1.2.1 matcher set — no `Bash`, and all three retired `mcp__lumen__*`
entries — and the Setup screen called it **healthy: "5 hook commands, all present"**.

Two things combined. Hooks are validate-and-report rather than auto-repaired, which is
deliberate: rewriting a user's Claude Code settings without being asked is worse than
telling them to press a button. But that makes the report the entire mechanism, and the
validator only checked that each lumen hook command pointed at a file that exists. It
never compared *which* matchers were registered, so it could not see a stale desired
state — the one thing it needed to see for the button to ever be pressed.

The matcher set now lives in one place that the installer and the validator both read
(`METER_MATCHERS`, `INTERCEPT_MATCHER`, `RETIRED_MATCHERS`). Previously the installer
held the list privately, so changing it in 1.2.1 left the validator measuring the old
contract. The report now names what is missing and what is stale, so the repair is
actionable rather than a bare "unhealthy".

**If you upgraded from 1.2.0 or earlier, open Setup and run it once** to pick up the
`Bash` matcher and drop the retired ones. Everything else self-repairs.

### Notes

- Paired tests keep the two sides from drifting again: one asserts a stale matcher set
  is reported, the other asserts what `step_install_hooks_in` writes validates as
  current. Changing either side alone fails one of them.
- The dangling-path check is unchanged and still covered.
- 465 Rust tests, 251 frontend.

## [1.2.3] — 2026-07-29

### The orphan-daemon fix in 1.2.1 did not work, and its test could not tell

1.2.1 added a watchdog so the daemon exits when the GUI dies, freeing
`127.0.0.1:9999` instead of squatting it across an upgrade. Verified on a real install,
it did not work: after force-killing the app, the daemon was still holding the port
twenty-four seconds later. `lsof` showed its stdin had **no peer** — the pipe was at
EOF and the watchdog's read had already returned.

The watchdog logged before exiting:

```
eprintln!("lumen-daemon: supervisor exited, ...");
std::process::exit(0);
```

The daemon's stderr is a pipe whose read end also belongs to the GUI, so it breaks at
the same instant as stdin. **`eprintln!` panics when the write fails**, and a panic on a
spawned thread unwinds only that thread — so `exit(0)` was never reached. The orphan
survived for exactly the reason the log line existed: to announce itself.

The test could not catch it, because it wired the daemon's stderr to `Stdio::null()`,
where every write succeeds. It reproduced the EOF but not the broken pipe, which is a
strictly easier situation than production. It now closes stdout and stderr alongside
stdin, and with the old code restored it fails — 2 of 3, with the negative control
still passing.

Every `eprintln!` in the daemon is now a non-panicking `logline!`. This was a class, not
an instance: fourteen other call sites had the same hazard, and the WebSocket restart
loop reaches one of them every two seconds. A daemon must not die because it could not
describe itself — nor be kept alive by the attempt.

### Notes

- On the install where this was found, the orphan was still present after upgrading
  1.2.0 → 1.2.2. That is expected and not a further bug: the process that must kill the
  daemon is the one being replaced, so an upgrade *away from* a version without the fix
  cannot benefit from it. Upgrades from 1.2.3 onward are covered. To clear a stale one
  now: quit Lumen, `pkill -f 'MacOS/lumen-daemon'`, then reopen it.
- Verified on the real install after this fix, not only in tests: an ordinary quit and a
  `kill -9` of the app both leave zero daemons and free the port.
- 462 Rust tests, 251 frontend.

## [1.2.2] — 2026-07-29

### Linux: the metering hook wrote nothing, in two independent ways

Both bugs arrived with 1.1.5 and neither was visible, because a metering hook must exit
0 so it can never fail the tool call it observes. On Linux that meant no
`builtin_read` and no `bash_output` rows at all. MCP-written rows (`smart_read`,
`recall_file`) were unaffected — those come from the Rust binary, not the shell. The
exposure is 1.1.5 through 1.2.1, all released on the same day.

- **`mktemp -t lumen_bash_out`** — BSD accepts a bare prefix and appends its own
  suffix; **GNU requires at least three X's and refuses.** Verified against coreutils
  9.1: `exit=1`, `too few X's in template`. The temp path came back empty, `python3`
  raised `FileNotFoundError` opening `""`, and a `|| exit 0` swallowed it. Now an
  explicit `$TMPDIR/lumen_bash_out.XXXXXX`, and if it still fails the hook says so on
  stderr instead of vanishing.
- **`stat -f %m`** — on BSD `-f` is "format"; **on GNU `-f` is "display filesystem
  status".** It does not fail, so the `|| stat -c %Y` fallback never ran. It printed six
  lines of filesystem information into a newline-delimited field list, shifting every
  field after it and making the insert throw.

The second is the instructive one. The fallback was ordered on the assumption that the
wrong dialect would *fail*; instead it succeeded with the wrong answer. Both are now
settled by validating the output — the mtime helper requires a pure integer and tries
the other dialect otherwise — rather than by trusting an exit code.

Neither was reachable by the tests that existed before 1.2.1, which only pattern-matched
the script's text. 1.2.1 added tests that execute it, its first CI run went red on Linux
with exactly these, and both bugs were then reproduced and their fixes confirmed against
real GNU coreutils in a container before this release was cut.

### Notes

- The daemon's no-override path resolution is tested on Unix only. Reaching that branch
  means letting the daemon resolve a home directory, and the only way to redirect that
  is `HOME`, which `dirs::home_dir()` honours on Unix but ignores on Windows — there it
  asks the shell for the profile. Run unguarded it would have opened the real ledger, so
  it is gated rather than quietly pointed at production. A companion test covers
  `LUMEN_DB` precedence on every platform.
- 462 Rust tests, 251 frontend.

## [1.2.1] — 2026-07-29

### Read this first: your "missed optimization" number will drop, and the old one was wrong

Half of that figure was never real. Of 6,862,596 tokens attributed to reads that
bypassed Lumen, **3,431,295 came from 117 binary files out of 2,749 rows** — and the
six largest entries in the entire table were screenshots. `lumen-tok` cannot tokenize
a PNG; it crashed, the meter read the crash as a broken tokenizer, and substituted
`bytes ÷ 4`. For a screenshot that overstates the cost by roughly 44x. Three PNGs read
during testing were recorded as 119,921 tokens against approximately 2,750 actual.

That number is now computed without them, for history as well as for new rows, so
expect it to fall by about half. The lower figure is the honest one. Nothing was
deleted to achieve it — the rows are still there, still queryable, and the report
script prints exactly what was excluded.

This also retires the "opportunity" that a non-text interception feature was scoped
against. It did not exist.

### Fixes

- **fix(daemon): an orphaned daemon squatted the WebSocket port across every upgrade,
  so the GUI silently kept reading from the version you replaced.** The app bound the
  spawned daemon to `_child` and dropped it; dropping a `CommandChild` does not
  terminate the process. Every quit therefore left a daemon reparented to launchd and
  still holding `127.0.0.1:9999`. After an upgrade the new app's daemon lost the bind
  and spun in the restart loop that exists to absorb *transient* collisions — it
  cannot tell one from a permanent squatter, so nothing was reported.

  Caught by inode on a real 1.1.4 → 1.2.0 upgrade: the process holding the port was
  inode 89539790 (the old binary, 8,835,200 bytes) while the newly spawned daemon was
  inode 90122126 with **zero TCP descriptors**.

  Two independent guards, because one is not enough. The app now kills the daemon on
  exit, which covers an ordinary quit. The daemon also exits when its stdin reaches
  EOF, which is what a dying parent looks like from the child's side — that covers a
  crash, a force quit, or an installer killing the app outright, where no exit handler
  runs at all. The watchdog is gated on `LUMEN_SUPERVISED=1`, set only by the app, so
  running `lumen-daemon` by hand still behaves.

- **fix(daemon): the daemon resolved an unset `LUMEN_DB` to the relative path
  `lumen.db`.** This is the same fallback class that previously split the ledger into
  two files accumulating real events in parallel. It now shares
  `meter::resolve_db_path` with every other writer, or refuses to start.

- **fix(hooks): Bash output was never actually measured.** The meter script has had a
  `Bash)` branch since the instrumentation phase, but `Bash` was never registered
  under `PostToolUse`, so the branch was unreachable and `bash_output` had **zero rows
  in 51 days**. The test that covered this asserted the exact four-matcher list, so it
  locked the gap in place rather than catching it.

  Bash is now registered. Command **output** is tokenized; of the command line itself
  only the program and subcommand are stored (`cargo test`, `git status`), with any
  leading `VAR=value` dropped first, so `TOKEN=secret curl …` records `curl`. This is
  observation only: no `PreToolUse` hook on Bash, nothing intercepted or wrapped, and
  Lumen never executes anything from a payload. To opt out, delete the `Bash` entry
  under `PostToolUse` in `~/.claude/settings.json`.

- **fix(hooks): the three `mcp__lumen__*` `PostToolUse` matchers are removed.** Those
  tools meter themselves in-process, so the hook script fell straight through to
  `exit 0` — forking a bash and a python3 on every `smart_read`, `recall_file` and
  `compress_logs` call to do nothing. Waste inside a tool whose purpose is removing
  waste. Removal is surgical: a matcher shared with a hook you added keeps your hook.

- **fix(tok): `lumen-tok` panicked on any input that was not valid UTF-8.** It now
  exits 3, meaning "this is not text and has no token count", which the meter records
  as `token_source = 'unsupported'` with no number. That is deliberately distinct from
  a genuinely broken tokenizer, which still yields a row labelled `estimated` — a real
  file whose count we could not take is not the same as a file that has no count.

- **fix(setup): `LUMEN_DB` and `LUMEN_TOK` are overridable in the generated meter.**
  They were fixed strings, which made the installed hook the one component in the
  pipeline that could not be exercised without writing to your real ledger. It is now
  covered by tests that run the actual generated script.

### Notes

- 24 tests added (460 Rust, 251 frontend). Every fix was falsified before being
  accepted: disabling the watchdog fails two of three supervisor tests while the
  negative control still passes; restoring the old `read_to_string().expect()` fails
  two of six tokenizer tests; removing the metric filter fails the exclusion test but
  not its control; unregistering Bash and re-hardcoding `LUMEN_DB` fails six setup
  tests.
- Historical rows cannot be separated by provenance: before this release a failed
  tokenizer produced a `bytes ÷ 4` value labelled `estimated`, indistinguishable from a
  broken tokenizer on real source. The correction therefore also matches known
  unmeasurable extensions. That is a query-level filter, not a rewrite — the ledger
  stays append-only.
- The extension list is finite and will miss a binary file with an unusual suffix.
  Rows written from this release forward are labelled at write time and do not depend
  on it.

## [1.2.0] — 2026-07-29

### Fixes
- fix(panel): the popover clipped its own cost figures. Measured in a browser at the
  popover's real 320x400: with a seven-digit context, a project label and a
  compaction badge all present, the content needed 413px inside 382px, so the second
  tile ran 7.9px past the bottom edge and `$1134.87` was cut in half.

  Three separate causes, each fixed structurally rather than by nudging a constant:

  - **"SAVED BY CACHING ⓘ" wrapped to two lines.** It needed 107px inside a 106.6px
    column, so the icon fell to its own row and stole 14px from the tiles below. The
    label and its icon are now one `nowrap` flex row, and the panel-scoped type is
    tightened until it fits with slack — so it cannot wrap regardless of translation
    or font fallback.
  - **Nothing could yield when the optional rows appeared.** The project label and
    the badge add 48px between them and are conditional, so the layout was only ever
    correct without them. The firefly is now the designated shrink target
    (`flex: 1 1 auto; min-height: 0`, size tied to viewport height), so growth
    squeezes the illustration instead of clipping the numbers.
  - **Long figures had no way to shrink.** CSS cannot size text by its own character
    count, so the component now picks a size class by length: eight characters is
    where the default stops fitting, ten where the reduced size does.

  Also: the token row and badge no longer wrap, grid items get `min-width: 0` so a
  long figure cannot widen its column, and the panel clips as a last resort.

  Verified at an extreme well past the report — `1,000,000 / 1,000,000`,
  `$123456.78`, a 28-character project name and a dated model id — with zero
  clipping and no horizontal overflow on any element.

### Notes
- **Non-text file interception was investigated and rejected.** The plan was to block
  reads of images above a size threshold, on the evidence that 87 PNGs accounted for
  4.3M "missed optimization" tokens. Both premises turned out to be wrong.

  Attribution through the transcript chain showed **39 of 45 traceable image reads
  (87%) were the agent reading back a screenshot it had just taken itself** — the
  visual verification loop behind prompts like "run it on local let's see how it
  looks". Blocking those does not remove waste, it blinds the agent on work the user
  asked for. Only 6 reads touched a pre-existing file.

  The token figure was also an artefact of the metering bug 1.1.5 fixed: image costs
  were recorded as `bytes / 4` of binary data by a hook whose tokenizer path was
  dead, or as 0 where the tokenizer panicked on non-UTF-8. Real cost scales with
  pixels, and for these screenshots is roughly 150k tokens — about 22x smaller than
  recorded, on the order of cents across 51 days.

  Not shipped, and not shipped disabled-by-default either: a feature that would break
  a working capability to save a rounding error should not exist. What remains worth
  doing is making `lumen-tok` report honestly on binary input instead of panicking,
  and excluding unparseable file types from the missed-optimization metric — that
  metric is what manufactured this opportunity.

## [1.1.5] — 2026-07-29

### If you installed 0.1.0 or 1.0.0 from the .dmg, your historical optimizer numbers have unverified provenance

Setup baked a tokenizer path pointing **inside the mounted disk image**. Once that
image was ejected the metering hook fell back to a `bytes ÷ 4` estimate **without
saying so**, and no upgrade ever repaired it, because Setup only ever ran once.
Lumen has described these figures as "measured to the token". On affected installs
that claim was wrong.

1.1.5 repairs the path, records provenance on every new row, and marks the
unverified range in the Optimizer screen. It does **not** retroactively correct the
old numbers — recovering them is impossible, and inventing them would be worse than
admitting the gap. Rows written before 1.1.5 are labelled *unknown provenance*
rather than reclassified: a boundary date was inferable from the evidence, but only
2 of 2,549 affected rows were verifiable against git history, so no boundary was
applied.

Unaffected: fresh installs on 1.0.1 or later, and anyone who happened to re-run
Setup after that release.

### Features
- feat: hook scripts now self-repair. They are compared against what the running
  build would generate — with the version stamp excluded, so a release alone
  rewrites nothing — and regenerated when they differ. Previously anything
  reachable only from `run_setup` was unreachable forever once its marker existed,
  which is how three separate bugs shipped: MCP paths (1.0.1), the login item
  (1.1.2), and this tokenizer path.
- feat: `read_events` records `session_id`, `file_mtime`, `req_key`, `writer_hook`
  and `token_source`. Existing rows stay NULL; there is no honest way to backfill
  provenance after the fact.
- feat: Bash output volume is measured (`routed_via='bash_output'`). Observation
  only — no PreToolUse hook on Bash, no interception, no execution wrapper.

### Fixes
- fix: negative savings are recorded instead of clamped to zero. `saturating_sub`
  on `usize` floored at 0 before the `i64` cast, so 170 real events that returned
  **more** than the file contained were logged as saving exactly nothing — hiding
  92,347 tokens of loss and inflating every average built on the column.
- fix: the metering hook detects its channel from `CLAUDE_CODE_ENTRYPOINT` instead
  of writing the literal string `cli` on every row. The "By channel" breakdown has
  been **removed** rather than repaired, because it was plotting a constant.
- fix: `get_optimizer_stats`' "CLI missed reads" filtered `channel = 'cli'`, which
  matched 2,694 of 2,694 rows. The filter is gone and the metric renamed.
- fix: config writes are atomic (temp file, mode preserved with a required floor,
  directory fsynced, symlinks followed rather than replaced). `fs::write` truncates
  in place, so Claude Code could read a half-written `~/.claude.json`.
- fix: the tokenizer fallback logs a warning and marks the row `estimated`. Silence
  was the defect, not the fallback.

### Documentation
- docs: **hooks fire in the VS Code extension.** The README said they fire only in
  the CLI and built a "Full mode vs Soft mode" distinction on it. Measured directly:
  108 built-in `Read` events were recorded during one session whose `entrypoint` was
  `claude-vscode`, and every file over the threshold in that session was intercepted.
  The distinction, and "install the CLI for guaranteed optimization", are removed.
- docs: the "missed optimization" baseline was **66% files Lumen cannot parse** —
  87 PNGs, 37 Markdown files, and other formats `smart_read` has no outline for.
  Reported as 6.53M; the honest residual is 2.20M.

### Notes
- `read_events.is_subagent` exists and migrates, but is **always 0**. Neither writer
  can yet tell whether a read originated inside a subagent — nothing in the hook
  payload or the MCP request context says so. It is a placeholder awaiting a source
  of truth, not a measurement.
- `~/.claude.json` and `~/.claude/settings.json` are **validated and reported, not
  auto-repaired**. They are shared with Claude Code, and corrupting either would be
  a worse failure than the one being fixed. Repair is on an explicit button press.

## [1.1.4] — 2026-07-28

### Fixes
- fix(macos): stop the app executable from overwriting the bundled CLI. The GUI is
  built as `Lumen` and the CLI sidecar was staged as `lumen`; macOS filesystems are
  case-insensitive, so both resolved to the same path inside `Contents/MacOS/` and
  the GUI won. Every macOS build since 1.0.0 therefore shipped **no CLI at all**,
  and Setup's "Install CLI" button symlinked the GUI as the `lumen` command. The
  sidecar is now staged as `lumen-cli`; the command users type is unchanged, since
  that name comes from the symlink rather than the bundle.
- fix(macos): refresh a login item that points at a stale executable. The rename
  above moves the app's binary, and an app moved out of `/Applications` changes path
  too — either way the login item would fail at the one moment it matters. The
  marker now records the path it registered and re-registers when it no longer
  matches, while still leaving a user's opt-out switched off.
- fix(brew): publish the GUI as the `lumen-app` cask. `homebrew/cask` already ships
  an unrelated `lumen` (a screen-brightness tool), so `brew install --cask lumen`
  installed the wrong application and `brew upgrade --cask lumen` offered to replace
  this one with it. Pairs with the `lumen-cli` formula.

### Notes
- **Upgrading from 1.1.3 or earlier requires one manual step**, because the cask
  token changed:

  ```bash
  brew uninstall --cask lumen
  brew trust --cask HackPoint/tap/lumen-app
  brew install --cask lumen-app
  ```

  The `trust` step is required because Homebrew refuses casks from a tap outside
  `homebrew/cask` until they are explicitly trusted.

  Data in `~/Library/Application Support/io.speedata.lumen/` is untouched.

## [1.1.3] — 2026-07-28

### Fixes
- fix: apply schema migrations on the sqlx paths, not only the rusqlite one. The
  daemon and the GUI executed `DDL` alone, which is `CREATE TABLE IF NOT EXISTS`
  and therefore a no-op on a database that already has its tables — so
  `turns.is_subagent`, added in 1.1.0, never reached any existing install. Because
  the daemon binds that column on every insert, **ingest failed on every row and
  the gauge silently froze** on upgrade; only a database created fresh at 1.1.0 or
  later worked. Both now call a shared `init_schema`, which runs the DDL and then
  the additive migrations, including the backfill that classifies existing subagent
  rows. Found by upgrading a real 0.1.0 install and finding the column absent.

## [1.1.2] — 2026-07-28

### Fixes
- fix: register the login item on existing installs. 1.1.1 only did so inside
  `run_setup`, which is skipped entirely once `~/.claude/lumen/.setup_done` exists
  — so anyone who had already set Lumen up got no login item, however many times
  they upgraded. The feature worked for fresh installs only. Registration now also
  happens once at startup, behind its own `.autostart_done` marker so that turning
  the toggle off is not undone by the next launch, and so a failed attempt is
  retried rather than silently abandoned.

## [1.1.1] — 2026-07-28

### Features
- feat: start Lumen at login. Setup registers a login item — a LaunchAgent on
  macOS, a Run key on Windows, an autostart `.desktop` on Linux. Lumen is a tray
  app with no Dock icon and both windows hidden at startup, so previously it did
  nothing until the user remembered to open it, and stayed silent after every
  reboot. The Setup screen carries a toggle, and uninstall removes the login item.
- feat(macos): launch the app once immediately after `brew install --cask`, with
  `open -g` so it appears in the menu bar without taking focus from the terminal.
  The cask now also quits the running app on uninstall and zaps the LaunchAgent.
  No equivalent exists for the `.deb`: postinst runs as root with no user session,
  so the first launch there is manual and autostart takes over afterwards.

### Fixes
- fix(release): update the Homebrew tap. It had been stale at 0.1.0 since June
  because no cross-repo credential was ever configured and a skipped job still
  reports success, so 1.0.0, 1.0.1 and 1.1.0 all shipped with Homebrew users left
  behind. Authentication is now an SSH deploy key scoped to the tap alone rather
  than a `repo`-scoped token with write access to everything; a missing credential
  raises a warning annotation instead of passing quietly.
- fix(release): allow `update-tap` to run on `workflow_dispatch`, so a tap left
  stale can be brought up to date without cutting a new tag. All three hashes are
  now computed from the published assets rather than passed between jobs, which
  also means the formula cannot disagree with what is downloadable.
- fix(release): mark hyphenated tags as prereleases. `v1.1.0-rc.1` would otherwise
  have been published as Latest, pointing every `releases/latest` link and the
  README download badge at a release candidate.
- fix(release): give the Linux GUI job its own target triple. It inherited the
  macOS `TRIPLE` and named its Linux binaries `*-aarch64-apple-darwin`, so
  `tauri_build` found no sidecar and the build failed inside the build script.
  `build-sidecar.sh` now refuses a triple that disagrees with the host.
- fix(daemon): add `LUMEN_PROJECTS_DIR` so the e2e tests are hermetic on Windows.
  They exported `HOME`, but `dirs::home_dir()` reads `%USERPROFILE%` there, so the
  daemon watched the real user profile and all ten timed out.
- fix(cask): use `depends_on macos: :ventura`. Homebrew deprecated the string
  comparison form, which warned on every `brew info` and is slated to become an
  error.
- fix(linux): correct the package description, which called Lumen a "macOS
  menu-bar app" in `apt show`, and stop duplicating the `.deb` dependencies that
  Tauri already derives from the linked libraries.

### Maintenance
- ci: keep `cargo test` on one line — Windows steps default to PowerShell, where a
  trailing backslash is not a line continuation. The command split in two and the
  first half exited 0 having run zero tests, the stray backslash having been taken
  as a test-name filter.

## [1.1.0] — 2026-07-28

### Features
- feat(linux): ship the GUI on Linux (x86_64) as an AppImage and a `.deb`. Both
  bundle the daemon, MCP server and CLI as sidecars. The `.deb` declares its
  WebKitGTK and app-indicator dependencies so `apt install ./lumen_*.deb` pulls
  them in.
- feat(linux): offer the `lumen` CLI on Linux via Homebrew. The release pipeline
  had built an x86_64 Linux binary since 1.0.0, but the formula never referenced
  it, so `brew install lumen-cli` failed on Linux with no bottle.

### Fixes
- fix(gui): store data in the per-OS location instead of hard-coding macOS's.
  `~/Library/Application Support/` was used on every platform, so on Linux the
  database landed in a literal `~/Library/…` directory. Linux now uses
  `~/.local/share/io.speedata.lumen/` (XDG) and Windows `%APPDATA%`.
- fix(gui): gate the legacy `com.tauri.dev` database migration to macOS. It
  probed a macOS-only path, which could never match elsewhere.
- fix(core): fall back to `USERPROFILE` when `HOME` is unset, so the database
  path resolves on Windows.
- fix(build): derive the sidecar target triple from the toolchain rather than
  hard-coding `aarch64-apple-darwin`. On any other host the build produced files
  Tauri could not find and failed on a missing sidecar.
- fix(release): stamp each platform's own sha256 into the Homebrew formula. One
  `sed` matched every `sha256` line, so adding the Linux entry would have written
  the macOS tarball's hash to it and broken every Linux install's checksum.
- fix(release): copy the in-repo formula templates into the tap. The job only
  rewrote the version and sha256 of whatever was already in the tap, so no
  structural change — including the new Linux block — could ever reach users.
- fix(daemon): wrap WebSocket text payloads in `Utf8Bytes` for tungstenite 0.30.
- fix(cli): bound `run_loop` on `io::Error: From<B::Error>` now that ratatui 0.30
  gives `Backend` an associated error type.
- fix(gui): tighten `fetch_agg`'s WHERE clause to `&'static str` so sqlx 0.9's
  `SqlSafeStr` audit is satisfied structurally rather than by comment.

### Maintenance
- ci: test `lumen-cli` and `lumen-stats`, which no platform tested before, and
  add a Linux job that builds, tests and lints the Tauri crate — previously the
  GUI crate was never linted at all.
- ci: run the frontend suite (`pnpm test`) with its coverage thresholds enforced.
- chore(deps): update the whole dependency tree to latest. Rust: sqlx 0.8 → 0.9,
  rusqlite 0.31 → 0.39, tiktoken-rs 0.6 → 0.12, ratatui 0.29 → 0.30,
  crossterm 0.28 → 0.29, notify 6 → 8, tungstenite/tokio-tungstenite 0.24 → 0.30,
  dirs 5 → 6, plus every semver-compatible bump in `Cargo.lock`. Frontend:
  Angular 20 → 22, TypeScript 5.8 → 6.0, zone.js 0.15 → 0.16, Tailwind 4.3.3,
  and the `@tauri-apps/*` packages to 2.11.x.
- chore(deps): pull in the sqlx 0.8.1+ fix for the SQLite binary-protocol
  integer overflow (RUSTSEC-2024-0363), which the old 0.8.0 lock pin blocked.

### Notes
- Angular 22 raises the Node floor to **22.22.3** (or 24.15+/26+). CI's
  `node-version: '22'` resolves to a new enough 22.x; local toolchains below
  22.22.3 need updating before `pnpm build` will run.
- TypeScript stays on 6.0.x — Angular 22's compiler pins `typescript >=6.0 <6.1`,
  so 7.x is not yet available to this project.
- rusqlite is capped at 0.39: it and sqlx-sqlite both link `sqlite3`, and
  sqlx 0.9 accepts `libsqlite3-sys >=0.30.1 <0.38`. Bump the two together.

## [1.0.1] — 2026-06-17

### Fixes
- fix(setup): register MCP sidecars from a stable path when the app runs from a
  DMG or App Translocation mount. Previously the `lumen-mcp`/`lumen-tok` paths
  written to `~/.claude.json` pointed inside the ejected `/Volumes/…` mount, so
  Claude Code failed to launch the server with
  `ENOENT … posix_spawn '/Volumes/…/lumen-mcp'`. The sidecars are now copied to
  `~/Library/Application Support/io.speedata.lumen/bin/` and registered there.

## [1.0.0] — 2026-06-09

### Features
- feat: add homebrew cask for GUI; rename CLI formula to lumen-cli

### Fixes
- fix(release): use POSIX [Yy] glob instead of bash-4 ${answer,,}
- fix(release): brace ${TAG} so bash 3.2 parses it before the ellipsis
- fix: skip tap update steps when TAP_PUSH_TOKEN is unavailable
- fix: make update-tap resilient when token secret is unset
- fix: skip update-tap job when TAP_PUSH_TOKEN is missing
- fix: discover DMG by find rather than hard-coding tag-derived filename

### Maintenance
- chore: harden tap token availability check

### Other
- Merge pull request #1 from HackPoint/copilot/fix-update-homebrew-tap-job
- Initial plan

