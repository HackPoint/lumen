# Changelog

## [1.10.0] — unreleased

**Lumen's data moves to a new folder named after the new identifier, `io.hackpoint.lumen`, the first time 1.10.0 starts, and your history, settings, login-item choice and menu-bar icon position come with it.** The previous folder is kept, read-only, inside the new one as `legacy-backup-<date>` (its files are under `data`). What resets: macOS asks again for notification permission, and may list Lumen as a new login item to approve under System Settings > General > Login Items. Setup runs when Lumen's window first opens after the upgrade, which on Windows and Linux is the first start; then restart Claude Code: a session left open keeps the old MCP server, which can no longer record.

### The data folder moves, and the previous one is kept as a backup

The new folders are `~/Library/Application Support/io.hackpoint.lumen` on macOS, `%USERPROFILE%\AppData\Roaming\io.hackpoint.lumen` on Windows and `~/.local/share/io.hackpoint.lumen` on Linux. Lumen finds the previous install by what a folder holds, never by its name. Its ledger and Lumen's own small files are copied and checked, and then the previous folder is renamed whole to `legacy-backup-<date>` inside the new one (`-2`, `-3` if the name is taken). Nothing is deleted.

If an old app wrote to the previous folder during the move, those rows are merged at every start until Setup has run. Then the backup is frozen: its files are made read-only, and it is never read for merging again. A frozen backup holds nothing Lumen still uses, so you can delete it (see `docs/troubleshooting-the-tray.md`).

### History comes back for people on the re-published 1.9.0, and `lumen import-ledger` brings in the rest

The 1.9.0 build re-published on 2026-10-20 already wrote a ledger in the new folder. 1.10.0 still finds the previous folder and migrates it, so that history comes back. The ledger that was in the new folder is set aside as `lumen.db.pre-legacy` (with a number after it if one is there already), and Lumen says so at start. Bring its rows in with `lumen import-ledger "<path>"`; add `--dry-run` to see what would be added without changing anything. Rows already in the ledger are not added twice.

A previous folder that is re-created later, by an old app started again after the move or by a restored copy, is never merged on its own. Lumen reports it ("An older Lumen wrote to <folder> after the move. Nothing from it was merged. Quit it, then run: lumen import-ledger ..."). Quit that app and run the command it prints; the folder then moves into `legacy-imports` beside the ledger.

### The menu-bar icon keeps its place across the move

The icon's position is carried over with the rest of the data, so it stays where you put it.

### Closing the main window never quits Lumen

While the menu-bar icon is hidden, crowded out by a full menu bar or the notch, closing the window used to be the end of the app. Now the window closes and Lumen keeps running with its Dock icon on macOS, which opens it again; on other systems it minimizes. `lumen show` opens the window from a terminal. Quit from the tray menu, the Dock menu or Cmd-Q.

### The Windows installer removes the previous install's leftovers

After an install, the installer removes a second "Lumen" entry in Apps & features and the previous manufacturer's install-location key. Both are found by what they hold, not by name. The publisher is HackPoint.

### The Homebrew cask's zap removes only the new identifier's paths

`brew uninstall --zap --cask lumen-app` removes the exact paths of the new identifier, with no glob. That includes the backup inside the data folder.

### Homebrew: re-tap if `brew update` stops mid-rebase

The tap's git history was rewritten on 2026-10-20. A tap cloned before that date cannot `brew update`: it stops with "interactive rebase in progress" and unrelated histories, stays on an old formula, and an install from it can fail with a SHA256 mismatch for the 1.9.0 assets. Untapping does not uninstall Lumen:

```bash
brew untap --force hackpoint/tap && brew tap hackpoint/tap
brew upgrade --cask lumen-app
brew upgrade lumen-cli
```

(`brew reinstall` works in place of the upgrades.)

### Linux with `XDG_DATA_HOME` and Windows with a redirected Roaming AppData

1.9.0 put the data where `XDG_DATA_HOME` or the redirect pointed. 1.10.0 keeps it in the home-based folder listed above, and the migration moves what was there.

## [1.9.0] — 2026-10-18

**Home now says in bold what Lumen saved: tokens and dollars, net, this week and all time.** The tokens are Claude's, estimated from Lumen's `cl100k_base` counts by a calibration against Claude's own usage data, so they read "≈". The dollars are a signed floor, the net on record: "at least $0.20 saved, net", or "net loss of at most $0.42". What may be worth more (modelled) and the calls with no price are tags beside the figure, never in it. One engine works the figures out per call from your own ledger, and bare `lumen` and the tray say the same figures in the same words. Every dollar figure in Lumen is now priced at the list price of the model that consumed the tokens, where 1.8.0 priced every model at Opus-class rates, and a figure that could not be priced in full says "at least", and why. How this build compares with 1.8.0 is under "Benchmarks against 1.8.0", at the end of this entry.

**Upgrading from 1.8.0: nothing to run.** The ledger gains its new columns and tables, and loses the unused `sessions` table, the first time any Lumen program opens it, and a 1.8.0 build that opens it afterwards still works. Setup does not need to run again: the hooks are the ones 1.8.0 registered. (From 1.7.0 or earlier, run Setup once and restart Claude Code, as 1.8.0's notes say.) What you will notice: Home asks once whether you pay by subscription or through the API, and until you answer the dollars read "at API prices". The screens moved: the Context tab is Home, the context gauge is on Details, and spend, the alert thresholds, the Repeat reads switch, Report a fault and About are on a new Settings tab; Hotspots is Why, shown only when there is something to act on. In the terminal, bare `lumen` prints one line, the dashboard is `lumen dash`, the fault report is `lumen faults`, and `lumen report` is the savings in detail. The old spellings still work and say so on stderr (`lumen --no-anim`, and `lumen report` with any of the fault report's flags), but a script that ran `lumen report` with no flag, or read bare `lumen`'s piped line, now gets the savings. Turns ingested before 1.9.0 recorded no speed, region or cache-write duration, so a dollar figure that includes them reads "at least"; so does one holding a turn on a 4.6 or later model whose region Claude Code wrote as `not_available`.

### Home says in bold what Lumen saved, and `lumen` and the tray say it in the same words

Nothing in 1.8.0 said what Lumen had saved you. The Context tab showed how full the context was and what the session cost, and the Optimizer's net value rested on assumed constants (see the engine's entry below). Home now opens with the savings headline, in bold and large, from the engine's summary (`lumen_savings_summary`) and nothing else, for every billing mode.

- **Tokens.** "≈ 25,400 tokens saved, net", the central value, in Claude's tokens; a period that
  sent more than it kept out reads "≈ 32 more tokens, net". The band behind it is on the period's
  ⓘ, on hover and on focus.
- **Dollars.** The net on record and nothing else, a floor: "at least $0.20 saved, net". A loss,
  whose floor is the most it can be, reads "net loss of at most $0.42", and one that rounds to no
  cent "net loss under half a cent", in the alert colour. A sign is the value's, never the
  rounding's: only zero reads $0.00. Thousands are grouped ("$12,346").
- **On a subscription**, before you have said how you pay, or when the settings file cannot be
  read, the dollars are "at API prices", since a plan is not billed per token, after the share of
  tokens Lumen kept out of context: "≈ 92% fewer tokens from file and tool reads this week" ("≈
  5.0% more …" the other way).
- **Beside the bold figure, never added to it**, as smaller tags: "+$0.01 modelled", "1 call
  unpriced", and "219 blocked Reads" where blocked Reads are all the period holds.
- **No zero that was not recorded.** A period with no Lumen calls says "No Lumen activity this
  week.", an empty ledger "No Lumen activity recorded yet.", a period with no dollars on record "No
  dollars on record this week.", and a partial one "Partial data:" and the engine's reason.
- **Status and one step.** Under the figures, one status line in `lumen report`'s words ("Status:
  working", "Status: not working:" and the reason, "Status: no data") and at most one recommendation with
  its step: Run Setup when the identity hook does not reach the server, "In a terminal: lumen
  doctor" for the others. The floor is a floor, so a losing week whose band reaches above zero, or
  whose high end is no bound, says "Lumen may have cost more than it saved: net loss of at most $X
  this week"; only a bounded band that is a loss throughout says "Lumen cost more than it saved".
- **How it's measured**, a screen reached from the headline and from Details: the summary's rules,
  the price table with its date and source, the calibration, every figure with its band and every
  count for both periods, and the per-call ledger (the five calls that gained and lost most, and
  the newest 200) as tables.
- **Refresh.** The summary is fetched when the app starts, every minute after, when the window gets
  focus, when Home opens again and once a billing choice is saved. A fetch that fails keeps the
  last good summary on screen, marked "Not up to date: the latest refresh failed", with why, and the
  time the figures are as of, in UTC; with none before it, the headline says the savings could not be
  read and shows no figure. A late answer from an overtaken fetch is dropped, and a summary of
  another schema, or one holding a figure that is not a finite number, is not shown.
- **Screen readers** are given each figure in words ("at least 105 dollars saved, net, all time, at
  API prices"), with the symbols hidden; the headline, each period and the status have headings.
  The two period cards stack below 480 px, so a long figure never pushes one off a narrow window.

The terminal and the tray say the same:

- **Bare `lumen`** prints one line: "Lumen this week: ≈ 92% fewer tokens from file and tool reads;
  ≈ 25,400 tokens saved, net; at least $0.20 saved, net at API prices · all time: … · working
  (lumen report for detail, lumen dash for the live dashboard)", and "no Lumen activity recorded
  yet" for an empty ledger.
- **The tray** shows a savings line, disabled, under its version line: on the API "Saved at least
  $X this week" or "Net loss of at most $X this week", otherwise "This week: …", then the status.
  With no data there is no line, rather than an empty one. It is worked out again every two
  minutes, off the startup path.
- **`lumen report`** prints each period with its bands, the method, the counts and the calls that
  gained and lost most; `--json` prints the summary itself. A ledger that does not exist yet is
  read as an empty one, not created.
- **One wording.** `lumen_core::savings::text` makes the terminal's and the tray's lines and
  `savings-text.ts` the headline's, phrase for phrase, held together by one case file that the Rust
  and the TypeScript tests both read. It caught the app rounding $0.125 to $0.13 where the terminal
  said $0.12; both now round half to even.

The WebDriver e2e (Linux, Windows) writes a fixture ledger, holds every piece of Home to the built
`lumen`'s line from the same ledger, follows How it's measured to a row per call, and keeps a
screenshot; the macOS launch check does the same under a temporary `HOME`. Both refuse a path where
a ledger or settings file already is, and on Windows and macOS they run only on GitHub-hosted
runners, so no ledger of a real profile is read or written.

### Home keeps three questions; Details, Settings and Why take the rest

1.8.0's main window put the context gauge, the session cost, the daily usage, the alert
thresholds, the Repeat reads switch and About on one screen, the Context tab, beside Optimizer and
Hotspots. Home now answers how much Lumen saved, whether it is working and what to do. Nothing was
deleted; the tabs are Home, Optimizer, Details, Why (when it has something) and Settings.

- **Home** keeps the header, the startup banners, the Run Setup prompt for the identity hook and
  the tabs, and gains the savings headline and, once, the billing question.
- **Details** has the context gauge with its Auto/200K/500K/1M selector and the alert banner: they
  describe how full Claude Code's context is, which is neither a saving nor something Lumen did.
  Native notifications still fire whatever screen is open. It also has the Optimizer's token
  ratio, the by-tool and by-channel breakdowns and the provenance counts. Its effectiveness is the
  summary's share, losses included, and a negative share reads as more tokens, not as zero.
- **Settings**, new: Billing mode; Spend (the session cost tiles and the daily usage report, beside
  the thresholds they are compared with); the alert thresholds with the native notifications
  toggle; the Repeat reads switch; Report a fault, moved from Hotspots, so the fault badge is now on
  the Settings tab; and About.
- **Why** is what Hotspots was (`/hotspots` redirects), with "What to do" first. It is in the tabs
  only while a reported file is something to act on: at least 1,000 lines read at least 20 times,
  or at least 10 reads with half of them unchanged, with advice written for it. A file that holds
  10% of everything read does not count: on a young ledger nearly every file does.
- **Optimizer.** The net value hero is gone, with its dollar figure, which rested on an assumed
  number of rounds remaining; the "Lumen optimized" card has no dollar line, since a read event
  records no model to price it by. The token cards, the provenance sentence under them, the
  caching card and the Repeat reads card stay.
- **The billing question**: "How do you pay for Claude Code?", Subscription (Pro/Max) or API (pay
  per token). Home asks it once, after Setup and only once Setup is known to be done, never on the
  Setup screen or in the tray popover; Settings changes it. It goes once answered, stays to say why
  when a choice was not saved, and is not shown over a settings file that cannot be used, which
  Settings explains. The choice is kept as `billing_mode` in `lumen_settings.json` beside
  `repeat_reads`, read with the same 64 KiB cap, and only `"subscription"` or `"api"` is accepted
  (a bad value is repeated back in short); absent means not asked yet. A bad `billing_mode` leaves
  repeat reads alone, and the other way round.
- **The settings file's writers take turns.** Each reads the file, changes its key and writes the
  object back, and both used one temporary file name per process, so two at once could each write
  back the other's old value or rename the other's file away: in 300 rounds, 62 lost
  `repeat_reads: false` (repeat reads silently back on). A lock now spans the read and the rename,
  and each temporary file is numbered. The billing writer keeps every other key and refuses to
  replace a file that is not a JSON object: such a file turns repeat reads off, and one holding only
  `billing_mode` would turn them back on.

The CLI:

- **Bare `lumen`** prints the one line above; it was the dashboard. The dashboard is `lumen dash`
  (`--no-anim` kept; piped, it prints its one-shot line as before).
- **`lumen report`** is the savings in detail. **`lumen faults`** is the fault report, with all its
  flags.
- **The old spellings still work**, and each says so in one line on stderr: `lumen report` with any
  of the fault report's flags (`--dry-run`, `--yes`, `--repo`, `--include-source`, `--faults`) runs
  `lumen faults`, and bare `lumen --no-anim` opens the dashboard. `--no-anim` given to any other
  command says it does nothing there. `lumen report` with no flag, which previewed the fault report,
  now prints the savings.
- **The Claude Code plugin's `/lumen-report`** runs `lumen faults --dry-run`, and `lumen report`
  in its place on a CLI older than 1.9.0. `lumen doctor`'s remedies, the README, the fault-report
  doc and the issue template name the new commands.

The macOS launch check (Issue 5) read About's version off Home; it now presses the Settings tab and
reads it there. A tab the runner cannot press is reported as a limitation, not a failure, since the
WebDriver e2e on Linux and Windows checks About on Settings too.

### One engine prices every saving per call, from the turns that read it

What Lumen saved was worked out in three places, and none of them knew the call it was pricing.
lumen-stats priced a net value at Opus-class rates with a fixed 194 rounds remaining and a fixed
1.604 rounds per call, the benchmark harness priced a "$/call" column with the outline sizing's
fixed 65 rounds, and bare `lumen`, the tray and Home had nothing behind them. Each figure was one
set of sessions multiplied out. `lumen_core::savings` is now the one place a saving is worked out.
It is pure: the ledger's rows in, one summary out. The CLI reads the ledger through rusqlite and
the app through sqlx with the same SQL, and a test holds `lumen report --json` and the app's loader
to the same bytes on one realistic ledger.

Per call, from the ledger:

- **The turn that read the result** is the first turn of the call's own conversation at least one
  second after the call (the meter records to the second, and the turn that made the call can share
  it). A subagent's call is read in its own transcript, `<session>/subagents/agent-<id>.jsonl`,
  under either path separator; a subagent with no turns in the ledger has tokens only.
- **`R`** is the turns after that one, `<synthetic>` messages left out, up to the first of: a
  compaction or rewind recorded in the same transcript; a collapse, the first later turn whose
  prompt is smaller than the one before it, since a prompt only grows until context is taken out
  (labelled inferred: it catches the compactions the ledger never recorded); or the session's end.
  A conversation that began before the daemon's first recorded event and shows no collapse has
  nothing to vouch for its end, and its `R` is labelled so; a session whose last turn is under an
  hour old may still be running, and its `R` is a lower bound. The daemon now records a compaction
  in a subagent's transcript as `subagent_compaction`, which ends that subagent's `R` and nothing of
  the main conversation's; the repeat-read gate does not read it.
- **Each turn is priced** at its model, its recorded speed and its recorded region (the two pricing
  entries below). Where a turn recorded no speed, no usable region or no cache-write duration, its
  saving is priced at the lower rate and its extra round at the higher, so the net is a floor, and
  the summary gives the reasons.
- **Against what.** A first read is counted against the built-in Read, which returns at most
  25,000 tokens of a file (Claude Code's documented default), so a larger file counts as 25,000 and
  the line says so. A repeat read's stub or delta is counted against Lumen's earlier answer to the
  same request in the same conversation, not against the whole file; with no earlier answer in the
  ledger it counts as none. A `fresh=true` call after a stub asks again for what the stub stood in
  for: it is a loss, not a second saving.
- **The value** of saving S tokens is S times the one-time price the reading turn paid for new
  context (the cache write at the duration it recorded, the input price if it wrote no cache, the
  5-minute price as a floor where it recorded no split) plus a cache read on each of the `R` turns,
  at each turn's own model and speed. A cache read on every later round is the least a saved token
  would have cost, so this is a floor.
- **Extra rounds**, each the reading turn priced as it ran (a write of unrecorded duration at the
  1-hour rate, so the cost is not understated): none for a repeat-read stub or delta; one, on
  evidence, for a `recall_file` or `smart_read(mode="full")` after an outline of the same file in
  the same conversation, for a `fresh=true` call after a stub, for `compress_logs` on inline text
  (which saves nothing, since the text stays in context, and also pays for writing it out), and for
  each Read the intercept blocked (the blocked Reads entry below); one, modelled, for any other
  call, which may have followed a blocked Read.
- **Net** is value less extra rounds, signed, never clamped.

A dollar is the price of the model that consumed the tokens, so each period's dollars come in three
parts that no surface adds together:

- **On record**, the bold "at least" figure: every call priced at the turn that read it, less every
  extra round it cost, on evidence or modelled. Where `R` may be too long, a saving is taken at
  `R = 0` (the one-time price alone) and a loss at `R`, the least either can be; the rounds are
  charged in full.
- **Modelled**: what those calls are worth beyond that, the cache reads over an `R` that may be too
  long. Shown beside the figure, labelled, never in it.
- **Tokens only**: rows with no turn to price them by (no session id, as on most rows before 1.1.5;
  no turn after the call; a subagent not in the ledger; a time that does not parse; a compaction
  before any turn read the result) or whose model has no price. Their tokens count; their dollars
  are not guessed from other calls'.

Every token and dollar figure is a band, low, value and high; the central value is the conservative
one, and where an `R` is a lower bound or may be too long the high end is said to be no bound. The
summary holds the billing mode; the ISO week (Monday 00:00 local time to now) and all time, each
with the tokens saved, the tokens the files held, the share, the three parts, what the priced turns
did not record, the counts and the data quality; a status (working, not working with its reason, no
data) and at most one recommendation; how each figure is made; and the per-call ledger, the newest
200 calls and the five that gained and lost most.

- **One test for the identity hook.** Setup's validator, the app's prompt, `lumen doctor` and the
  engine all call `lumen_core::identity_hook`: a Lumen command under PreToolUse whose matcher
  selects each of the three tools, with the ledger's newest 50 calls that carry a repeat plan
  overruling `settings.json` either way. Until now the terminal counted any Lumen command on any
  matching entry and the window only an exact matcher, so the two could disagree about one file. A
  `mcp__lumen__.*` matcher, which Claude Code reads as selecting all three, now counts in the window
  too; another tool's command on the three names counts in neither.
- **The hook records the subagent on each escape-valve fault** (`agent_id`, in the spool line and a
  new `faults` column; a main agent's line is unchanged). The backfill every opener runs now also
  marks subagent rows written with backslashes, as the daemon already did.
- **Speed.** On a synthetic ledger ten times a heavy user's (999,000 turns, 150,000 `read_events`,
  2,500 compactions and rewinds) a summary takes 0.42 s for the tray, its pool kept open, and 0.56 s
  for `lumen report`, migrations included; at the heavy user's own size, 39 and 53 ms. The tray works
  it out again only when the newest row of a table it reads, or the fault spool, has changed, or
  fifteen minutes have passed: 0.14 ms otherwise.
- **Gone**: lumen-stats' net value and its fields, the app's copies of them,
  `scripts/lumen_rounds.py` and `scripts/lumen_percall.py` (transcript replays priced at Opus-class
  rates), and the harness's dollar column: a corpus run has no turns to price a saving by, so its
  results say "none" and why. A test fails if the 194 or 1.604 figures, or their names, come back.

Not changed: what the MCP tools return and what the hooks decide. The ranked arm's outline sizing
(`econ.rs`, behind `LUMEN_RANKED_OUTLINE`, off by default) keeps its fixed inputs, 65 rounds
included: over the pinned corpus every other value tried made a file-size bucket's replies larger
(at 1,000 rounds the 300–999 line bucket returned 48,150 tokens against 43,577).

### Each Read the intercept blocked is counted from the transcript that records it

The hook writes no ledger row when it blocks a Read, only the marker file it keeps for itself, so
the escape valve's faults were the only record of a block, and they name the agent only from 1.9.0
on. Claude Code's transcripts record every block: the assistant's `tool_use` (a Read, with its
`file_path`), then a `tool_result` flagged `is_error` whose text holds the hook's message, in the
transcript of the agent that tried (`<session>.jsonl`, or `<session>/subagents/agent-<id>.jsonl`).
The daemon now reads them.

- **One definition of the message.** `lumen_core::intercept` builds the first line of every block
  message (`Lumen intercept: <path> is <n> lines` and one of three tails) and reads it back from
  anywhere in a text, since Claude Code puts its own words first. The hook's three messages are
  built from it, byte for byte what they were. Every first line a hook has written since 0.1.0
  parses, the eight-column `wc -l` count of the hooks Setup wrote up to 1.4.x included. Not a block:
  a Read that went through of a file holding a block message, a Bash error quoting one, another
  file's Read, any other error of a Read.
- **The daemon** matches each result to the Read it answers (the last 64 Reads of a transcript are
  kept) and writes a row of a new `intercept_blocks` table, keyed by the Read's `tool_use` id:
  session, agent, the path as the hook named it, lines, kind, and the result's time. A new
  `transcript_scans` table records which transcripts have been read for blocks from their first
  line, and through which turn. A tail read that lost its tracker has lost the Reads waiting for a
  result, so it moves nothing on until the file is read from its first line again.
- **No separate backfill.** The daemon's startup pass already reads every transcript on disk from
  byte 0, so the first start of 1.9.0 records every block the transcripts still hold, in the
  daemon's own process, never the app's window; later starts write nothing for a row that has not
  changed. On 252 MB of synthetic transcripts (600 files, 78,600 lines, 614 blocks) the scanner
  adds 9 to 15 ms to the 130 ms of one of the daemon's per-line parses, and the startup pass took
  1.48 s before and 1.40 s after (median of 11 alternating runs on a loaded machine).
- **Trackers that race, and files rewritten in place.** The notify and poll paths can read one file
  at once, and the first could put back a tracker that had fallen behind the offset; such a tracker
  is now dropped, not trusted, and a read that starts behind its tracker no longer feeds it lines
  twice (a prompt fed twice used to read as a rewind). A transcript rewritten in place, shorter than
  its offset or holding other bytes in the 64 before where its tracker stopped, is read again from
  its first line, with a resync. A file deleted and written again with the same lines and more is
  still tailed as if it never left.
- **The engine** counts a recorded block as one extra round, on evidence, priced at the first turn
  of the blocked agent's own chain after it, the turn that read the message; a path blocked twice
  in one context is two. The faults on the same session, path and kind are its later Reads,
  counted beside it and as no more blocks.
- **Where no transcript records a block** (it was deleted before the daemon read it), the faults
  stand for it: one block per session, agent and path, a fault that names no agent being attributed
  by the meter's `builtin_read` row for the same Read. Faults nothing attributes stand for at most
  one block per agent that could have read the path then (the main agent, each subagent with a turn
  within five minutes, any agent the meter named on the path), never more than the faults: the first
  on evidence, the rest bounded and modelled.
- **A Lumen call loses its modelled round** ("may have followed a blocked Read") where a transcript
  read through the call records a block of the same file in the same conversation before it that no
  earlier call was answered by: that block is the call's round. A conversation counts as read for
  blocks only as far as its own transcript was read.
- **Counts.** `blockedReadsFromTranscripts`, `blockedReadsFromFaults` and `blockedReadsBounded`,
  which `blockedReads` sums. Where blocked Reads are all a period holds and they are not all of one
  kind, `lumen`, the tray and Home say where they come from: "219 blocked Reads: 200 recorded, 7
  from faults, 12 bounded". How it's measured lists the three counts and names each recorded block
  and the call it answered. A ledger with neither table filled, before the daemon has run, gives
  the summary it would have given without them.

Not changed: the hook writes nothing new for this. Limits: the repository holds captured hook
payloads but no captured transcript of a blocked Read, so the shape of its `tool_result`
(`is_error` true, the hook's message inside) is an assumption, and the tests say so. A transcript
rewritten by another program at the same length, with the same last 64 bytes before the offset, is
not seen as rewritten; Claude Code only appends to transcripts.

### Each model's tokens are priced at that model's list price, and a floor is shown as one

Every dollar figure in the CLI and the app priced every token at one set of Opus-class rates ($5
input, $25 output per million), whatever model consumed it, so a Sonnet 4.6 session read about 1.7
times its cost and a Haiku 4.5 one five times. The app's rates were a TypeScript copy of the Rust
constants, kept in step by a comment.

- **A price table.** `lumen_core::rates` holds, per model, input, output, 5-minute and 1-hour cache
  writes and cache reads, in USD per million tokens, from Anthropic's pricing page on 2026-10-13,
  each row naming the page row it came from, and the fast-mode prices of the three models that have
  it (Opus 5.5, Opus 5, Opus 4.8). A dated id or a `[1m]` suffix resolves to its alias; anything
  else must match exactly, not by prefix, so a model the table has not heard of gets no price rather
  than an older model's. `<synthetic>`, Bedrock ids, Claude Haiku 5.5 (priced by each request's
  prompt length) and Claude Sonnet 4.5 (whose long-context premium the page no longer gives) have
  none.
- **No figure is shown as more complete than it is.** A model with no price adds no dollars; its
  tokens are still counted. A cache write that recorded no duration is priced at the 5-minute rate,
  the lower of the two, and a turn that recorded no speed at the standard rate on a model whose fast
  mode costs more. Every turn ingested before 1.9.0 is both. A figure holding any of these is a
  floor: the app tags it "at least" and the tag's tooltip says why, `lumen dash` prints "at least
  $…" with a line for each reason, and a figure with nothing priced in it shows a dash, tagged "no
  price", or a note that the prices have not loaded yet. Each figure is judged on its own: a write
  of unknown duration makes the session cost and its cache-write part a floor, not its output cost.
- **Spend alerts.** A floor over its limit fires the alert. A floor under it cannot settle whether
  the limit was crossed, so the alert stays quiet and Settings says so under the thresholds.
- **The daemon records** what the transcript says about both: the cache write's split by duration,
  into `cache_5m` and `cache_1h` (in the schema from the start, never filled), and `usage.speed`,
  into a new `turns.speed` column. A turn that recorded neither is stored as NULL, not as zero or
  "standard", and no row is backfilled.
- **Sums split by model.** `get_usage` returns `byModel` on every aggregate, and the daemon's
  snapshot sends `by_model` with each session. The snapshot is now read in one transaction, and a
  live turn it already counted is held back: a turn ingested while a client connected was in both
  the snapshot and the live stream, and was counted twice. A snapshot with no split is left unpriced
  rather than priced at the session's latest model, which a subagent may not share.
- **`lumen dash`** shows a sum it could not read as a dash, where it showed zero on a first failure
  and the previous figure, possibly another session's, after that. Its TODAY line is read from the
  ledger while the daemon is connected; it stayed at $0.0000.
- **The app holds no price of its own.** It gets the table from a new `lumen_prices` command, and a
  test fails on a rate constant, a per-token literal or a model's prices written into the frontend.

Not changed: MCP replies and hook decisions.

### A US-only turn is priced at 1.1 times, and a turn whose region is not known as a floor

Every dollar figure priced a turn at the global-routing rate, and the ledger did not record where a
turn ran. Anthropic's pricing and data residency pages, read on 2026-10-14: for Claude 4.6 and
later models, `inference_geo` `"us"` is 1.1 times every token price (input, output, cache writes
and cache reads, on top of fast mode's prices too) and `"global"`, the default, the standard price;
Opus 4.5, Sonnet 4.5, Haiku 4.5 and earlier always cost the standard price. Neither page defines
`"not_available"`, which Claude Code writes in `usage.inference_geo`, and a request that names no
region takes the workspace's default, which can be `"us"`. So a turn with `"not_available"` or no
region is not known to have cost the standard price.

- The daemon records `usage.inference_geo` as written, into a new `turns.inference_geo` column. A
  turn that recorded none is NULL, and no row is backfilled. A 1.8.0 build that writes to the ledger
  afterwards names its columns, and its turns get NULL too.
- `"us"` costs 1.1 times on a model that offers it, `"global"` the price, and no region or
  `"not_available"` the global price, flagged as a floor ("at least", with the reason, in the app
  and the CLI). A region this build does not know has no price, as an unknown speed has none. An
  older model costs its price wherever it ran.
- Wherever Claude Code writes `"not_available"` (the real transcript line in Lumen's tests, an Opus
  4.8 turn, carries it), a figure holding 4.6-or-later turns is now marked "at least": those turns
  may have cost 1.1 times what is shown.
- Spend, the snapshot's `by_model`, `get_usage`'s `byModel` and the app's per-model tally are split
  by model, speed and region; `lumen_prices` sends the multiplier, and the frontend price guard
  fails on a hard-coded 1.1. The app keeps an empty speed or region apart from an unrecorded one:
  an empty one has no price.

### Lumen's token counts are calibrated against Claude's, and no longer called exact

Lumen counts tokens with `cl100k_base`, OpenAI's tokenizer, because Claude's is not available
offline. The README and the Optimizer's footer claimed for those counts an exactness they have only
in `cl100k_base`'s own units, and nothing said how far those units are from what Claude counts.

- **How it is measured.** `benchmarks/calibration` needs no API key: it goes through Claude Code's
  own usage data. The same prompt is sent with and without a sample (`claude -p --output-format
  json`, no tools, no MCP servers, no hooks, no settings, no session persistence), and the
  difference of the two prompt totals is Claude's count of the sample. Each model was validated
  first: five sends of the empty prompt gave one total, two samples gave the same delta twice, a
  one-character sample cost two tokens, and two samples sent together cost what they cost apart. In
  the run every sample was sent twice and every pair agreed. No calibration call left a transcript
  under `~/.claude/projects`, where the daemon would have ingested it as your own use; the runner
  checks after every call and at the end of a run.
- **The corpus.** Sixty samples from ten public repositories, pinned by commit and git blob id:
  Rust, Python, TypeScript/JavaScript, Go, shell, JSON, logs and prose, all of it raw file text.
- **Two tokenizers.** Twelve models were measured through Claude Code 2.1.296 on 2026-10-13 and
  2026-10-14. Opus 4.7 and every later model measured (Opus 4.8, Opus 5, Sonnet 5, Fable 5, Opus
  5.5, Sonnet 5.5 and Haiku 5.5) counted every sample alike: 1.34 to 1.79 times as many tokens as
  `cl100k_base`, 1.59 at the median (10th percentile 1.44, 90th 1.70). Opus 4.6, Sonnet 4.6, Opus
  4.5 and Haiku 4.5 counted every sample alike too: 1.06 to 1.34 times, 1.24 at the median (1.14,
  1.31). Fable 5 stopped on one sample on safety grounds and Claude Code answered with another model,
  so that sample is logged as a fallback and dropped for it.
- **`lumen_core::calibration`** embeds the table, keyed by model and content type, with its dates.
  A model it does not have, or a file type it does not have, gets a fallback row with a band widened
  over every row it stands in for. Tests hold it to `results/table.tsv`.
- **The text follows.** The README, `docs/efficiency.md`, `benchmarks/README.md`, the messaging
  contract and the Optimizer tab say what counted a figure and how far that is from Claude's count
  on raw file text, for both tokenizers, and a test holds each quote to the table. The Optimizer's
  repeat reads card no longer calls a tokenizer count measured. `tests/proxy_count_claims.rs` fails
  on user-facing text that calls a proxy count exact (the READMEs, the contracts, `docs/`,
  `commands/`, this changelog's newest entry, the app's templates and TypeScript, and every string
  literal in the crates' and the tray's Rust); it found 18 such phrases before this change and none
  after.

Not changed: what the hooks and the MCP tools return. Limits: the ratios are of raw file text; a
Read's line numbers and the tool-result wrapper are in no row. Every sample came out higher in
Claude's count than in `cl100k_base`'s, on every model measured, but the samples are English and
include no CSS, HTML, YAML, SQL or non-Latin text. A later Claude Code or model can move every row,
so the table carries its date wherever it is quoted.

### Every token figure is Claude's, through the calibration, and the floor takes the file at its smallest ratio and Lumen's answer at its largest

`cl100k_base` counts alone would leave every token figure short (on the models from Opus 4.7 on,
Claude counts the same text about 1.59 times as many tokens), and the 25,000 tokens a Read returns
at most are Claude's tokens, not `cl100k_base`'s. The engine now converts every count it uses (the
file's, the answer's, the earlier answer a repeat read stands in for) through
`lumen_core::calibration`, by the file's content type (its extension) and the model of the turn that
read the result.

- **A file** is converted at its row's median ratio, with the band from the smallest to the largest
  ratio any one sample of the row came out at. Not the 10th to 90th percentile: the dollar floor is
  the band's low end, and it must hold for each call, not only on the average of many.
- **An answer Lumen made** (an outline, a slice, a stub, a delta) is a kind of text the corpus never
  measured, so it takes the file's median and the widest span measured for the model, over every
  content type: 1.335 to 1.794 on the models from Opus 4.7 on, 1.056 to 1.344 on Opus 4.6, Sonnet
  4.6, Opus 4.5 and Haiku 4.5, and 1.056 to 1.794 where the model is not known.
- **A row with no consuming turn, or one read by a model the table does not have**, takes the
  type's ratio as the mean of the two tokenizers' medians, with a band that spans both. Weighing
  every row alike counted the tokenizers eight models to four: Rust is now 1.421 where it would
  have been 1.543, and a file of no known type 1.415 where it would have been 1.503.
- **A saving** is the file less the answer, two kinds of text whose ratios nothing ties together:
  its low end is the file at its low end less the answer at its high end, its high end the other
  way, and its value the medians'. The same holds for a repeat read (the earlier answer less the
  stub or delta), for a stub whose earlier answer is not in the ledger, and for a loss against
  nothing (a `fresh=true` ask after a stub, `compress_logs` on inline text), which takes the answer
  at its high end. The 25,000 cap holds each end of the file's band on its own.
  Whether a line is capped (`readCapped`) is decided at the median, and a line's `savedCl100k`, the
  proxy's own figure, is taken before the cap.
- **The floor.** The bold figure values the saved tokens at the low end of their band (for a loss,
  the largest loss), never at the median ratio, and the band's high end takes their high end. An
  extra round is the turn's own recorded usage, the provider's count, and is not converted. The one
  cost Lumen counts itself, writing inline text out for `compress_logs`, is taken at the high end of
  its band.
- **"On record", not "measured".** The provider's usage data is measured; the bold figure also rests
  on calibrated estimates. So `lumen` and the tray say "no dollars on record", Home "No dollars on
  record this week.", and How it's measured "Net on record, at least (the bold figure)". The
  summary's field names keep the old word (`measured`), documented as meaning on record, and a test
  fails on user-facing text that calls the floor measured.
- **What each surface says.** The token figure stays "≈ N tokens saved, net", now in Claude's
  tokens. The period's ⓘ gives `band L to H of Claude's tokens, estimated from cl100k_base counts by
  the calibration of <date>; the dollars take its low end`. `lumen report` and How it's measured
  print the calibration: its date, the Claude Code version it was measured through, and for each
  tokenizer and for the rows with no measured model the table's ratio and band, the calls converted
  at it all time, their files in `cl100k_base` tokens and the ratio those files came out at.
- **The summary is schema 2**, since its token figures changed meaning; the app reads 2 and refuses
  any other.
- **`calibrate.py`** stops a run past one safety fallback per model, or one per content type over
  every model, and a test holds the committed run to the same caps.

Not changed: what the hooks and the MCP tools return. `lumen report --json` took the same time in a
release build, within 1% (median of 15 alternating runs): the ratios are worked out once, and the
lookups allocate nothing.

### Upgrading a ledger is one transaction, and opening a current one writes nothing

Every program that opens the ledger (daemon, app, hooks, MCP server, CLI) brings it up to date. Each
step of that upgrade was a write transaction of its own, so upgrading a ledger from 1.5.1 took 9
commits and one from 1.1.4 took 26. This release adds 8 steps. A backfill also asked for the write
lock on every open, even when no row needed it. With several programs opening an old ledger at the
same moment, each waits for every one of those locks, and on a slow disk the wait could run past the
two seconds an opener retries a busy ledger. The upgrade tests saw this in one CI run on a Windows
runner: nine openers at once, and in two of the upgrade cases one gave up with "database is locked".

- The upgrade now runs in one transaction. It takes the write lock only when a step changes
  something, and a ledger that is already up to date is opened without taking it at all. If the
  ledger is busy, the whole upgrade is rolled back and tried again for up to two seconds, as
  before, and the next open runs it again if that runs out.
- A backfill reads first and writes only when a row needs it, so opening a current ledger writes
  nothing.
- The schema and the rows are unchanged, the same on all three code paths. Opening a current
  ledger of 50,000 turns takes the same time as before (6.6 ms).

### The unused `sessions` table is dropped

Every DDL from the first commit to 1.8.0 created `sessions` (`session_id`, `cwd`, `model`,
`started_at`, `source_file`), and nothing ever inserted a row into it or selected from it: a session
is the `session_id` its turns carry. The table only looked like the place to find sessions (#13).

- The DDL no longer creates it, and the migrations end with `DROP TABLE IF EXISTS sessions`, so the
  daemon, the app and the rusqlite path (hooks, the MCP server, the CLI) all drop it through the
  shared schema code. It is a no-op on a fresh ledger and on one where it already ran. A ledger that
  somehow holds rows there loses them; no Lumen build ever wrote one.
- A 1.8.0 build opening the ledger afterwards creates it again, empty; no other row or column
  changes, nothing in 1.8.0 reads it, and the next open by 1.9.0 drops it again. A test runs that
  round trip with 1.8.0's own schema code, through each of the three openers.
- The upgrade matrix gains a 1.8.0 ledger: the long-lived ledger is now written by 1.5.0, 1.6.0,
  1.7.0 and 1.8.0 in turn, and every cell asserts that `sessions` is gone after the upgrade, that the
  ledger matches a fresh one, and that every row of every other table is kept.

The release's other schema changes, each made by the shared migrations on every opener, with
nothing to run: `turns.speed`, `turns.inference_geo` and `faults.agent_id` (NULL where nothing was
recorded, no row backfilled), the `cache_5m` and `cache_1h` split now filled on ingest, and the
`intercept_blocks` and `transcript_scans` tables.

### Benchmarks against 1.8.0

The benchmark harness now measures against the last release, v1.8.0, where it measured against
v1.7.0. It was run on commit `5626660`, the last commit before this bump that changes anything it
measures: tokens and latency on macOS (Apple M4), latency on Ubuntu 24.04 and Windows Server 2025
through the Benchmarks workflow (run 39118498843), 200 runs per series. The results are in
`benchmarks/results/pre-release-56266602bba5-*`, and `benchmarks/README.md` sets them beside 1.8.0's.

This release changes how Lumen counts what it saved and how a ledger is upgraded, not what its tools
return or what its hooks do, and the benchmark agrees:

- **Tokens:** equal for every file and every mode (outline, recall, stub, delta). The two builds'
  token digests are the same.
- **Latency on macOS:** no difference. For every series, the two builds' pass medians overlap.
- **Latency on Ubuntu:** `recall_file` on a file under 300 lines takes 30 µs longer (1.26 to 1.29 ms,
  2%), and that held in every pass. Every other series overlaps. The cause was not traced (#62).
- **Latency on Windows:** `smart_read` on a file of 1,000 to 2,999 lines and a call that claims its
  identity record are 3% faster in every pass. Every other series overlaps.

## [1.8.0] — 2026-10-13

**Repeat reads are on by default.** A second `smart_read` or `recall_file` of the same thing in a conversation is answered with a short stub when the file has not changed since, and with only what changed when it has. To turn them off, use the Repeat reads switch in the app, put `{"repeat_reads": false}` in `lumen_settings.json` in the Lumen data directory, or set `LUMEN_REPEAT_READS=off` (the variable wins over the file). The app now shows which version it is, under the tagline in the main window and in an About section. Measurements on a pinned corpus, 1.7.0 beside this build: [benchmarks/README.md](https://github.com/HackPoint/lumen/blob/v1.8.0/benchmarks/README.md#what-is-committed-here). They show a cost in time: a Read that Lumen blocks takes 0.4 to 2.6 ms longer than in 1.7.0, and the hook that names the conversation adds about 0.1 to 2 ms to each Lumen tool call, depending on the machine.

**Upgrading from 1.7.0 or earlier: run Setup once, then restart Claude Code.** Repeat reads need the hook that tells Lumen which conversation a call belongs to, and Setup is what registers it. An install set up under 1.7.0 or earlier does not have it, and Setup does not run again by itself, so until you run it every read is answered in full, as before. The Context and Optimizer tabs say "Repeat reads are on, but not working yet", with a Run Setup button, until the hook is registered.

### A `recall_file` line range the file cannot satisfy no longer kills the server

`recall_file` with a `start_line` five or more lines past the end of the file, or one far enough
after its `end_line`, panicked on a slice out of range. The server exited (status 101) and answered
nothing after that, so one bad guess, such as a line number from before the file shrank, took every
Lumen tool away for the rest of the session.

- A range the file cannot satisfy is now an error reply (invalid params, `-32602`) that says how many
  lines the file has and what was asked for. An `end_line` past the last line is clamped to it, and a
  number too big for the platform's integer no longer overflows the arithmetic.
- A range the file can satisfy is answered exactly as before, byte for byte, and `start_line` 0 is
  still read from the first line. Three requests that used to be answered now get the error instead,
  because the answer was not what they asked for: a `start_line` one to four lines past the end (the
  last lines of the file, or nothing, under the range asked for), a `start_line` slightly after the
  `end_line` (a window around lines nobody named), and a range that starts at line 1 or later on an
  empty file (a reply that cost tokens for a file that had none).
- A panic anywhere in a request is now caught per request: the call is answered with an error
  (`-32603`), the server keeps serving, and a `request_panic` fault is recorded with the tool's name
  and the file and line it panicked at. The panic message is in the error reply and on stderr but not
  in the fault, since it can quote the text being read and a report holds no file contents. `lumen
  report` ranks it just below a fired fail-open guard. What the call had remembered for repeat reads
  is dropped, since the model never received it. `LUMEN_CAPTURE=0` keeps the fault out of the spool,
  as it does for the hooks.

### A transcript deleted before the daemon read it is not a fault

The daemon opens a transcript after it has noticed it, from a watcher event or a directory
listing, and the file can be deleted in between. The open failed with `No such file or directory`
and was filed as an `ingest_failed` fault, on the `notify`, `poll` and `init` paths alike. A path
that is not there is now nothing to ingest: the saved offset and the transcript's tracker are left
as they were, so if the same path comes back it is tailed, or resynced, exactly as before. Only a
missing file is excused. A transcript that cannot be read for any other reason (permissions, a
directory with its name, bytes that are not text) is still recorded.

### Two programs opening the ledger at the same moment no longer fail with "database is locked"

A ledger that is not in WAL mode yet (a first run, or one last written by an old build) is
switched by whichever program opens it first, and SQLite does not let a second opener wait for
that: it refuses at once, whatever the busy timeout says, because the two could otherwise wait on
each other for good. The daemon and a hook, or the app and the daemon, starting together could so
fail an open that a moment's wait would have let through (seen on Windows in the upgrade tests).
Every opener (hooks, MCP server, CLI, daemon, app) now tries again for up to two seconds before it
reports the error.

A migration refused for the same reason used to be skipped and counted as applied, which could
leave a column or an index missing. A busy ledger is now an error, and the next open runs the
migration again. The one exception is the step that marks subagent turns: it rewrites rows, not
the schema, and every open runs it, so an open that finds the ledger busy for it goes on without
it, as before. Any other migration error (a column that already exists) is still read as applied.

### The app shows which version it is

The version was only in a startup log line, so someone filing a report could not say which build
they had without opening the log.

- The main window's header reads `Lumen <version>` under the tagline, and an About section at the
  bottom of the same window lists it under `Version`. Lumen has no separate settings window, so
  that is where its settings are and where About went.
- The tray menu starts with the same line, `Lumen <version>`, greyed out. It is disabled, so no
  platform delivers a click on it, and the menu's handler would ignore it if one did.
- Both read the app's own `PackageInfo`, which Tauri fills from `tauri.conf.json` when the app is
  built (the window through `getVersion()`, the tray menu directly). There is no version string of
  Lumen's own in the frontend or in Rust to keep in step.
- A test now fails when the version the app was built as differs from `tauri.conf.json`, the
  Tauri crate's `Cargo.toml` or any crate's, which covers the sidecars the app bundles. The app
  e2e (Linux and Windows) and the Issue 5 launch check (macOS) read the version from those files
  and assert the window's text.

### The app says when repeat reads are waiting for Setup

Repeat reads are on by default from 1.8.0, but they need the hook that tells Lumen which
conversation a call belongs to: a PreToolUse hook on `smart_read`, `recall_file` and `compress_logs`
in `~/.claude/settings.json` (#32, below). Setup registers it, and Setup runs by itself only while
`~/.claude/lumen/.setup_done` is missing. An install set up under 1.7.0 or earlier has that file, so
the app never sent it back: the switch said on, every read was still answered in full, and nothing
in the window said why. **Anyone upgrading from 1.7.0 or earlier has to run Setup once, and restart
Claude Code, for repeat reads to start working.** The app now says so until Setup has done it.

- **A prompt at the top of the Context and Optimizer tabs**, in the look of the startup-health
  banner: "Repeat reads are on, but not working yet", what is missing, and a Run Setup button. The
  button opens Setup, which runs its steps when it opens. It is announced as a status, the button
  takes the keyboard, and it is not on the Setup screen or in the menu-bar panel.
- **Only while there is something to do.** Not while Setup is still to run (a first launch opens it
  by itself), not once the hook is registered, and not when the settings file turns repeat reads
  off. Not before the setting has been read, so it does not flash and withdraw. Not when the app
  cannot tell: a check that fails, or a `settings.json` that is not a JSON object (Setup refuses
  that file too), shows nothing, since an unknown is not an accusation.
- **It clears when the hook is registered.** The window asks again each time one of those tabs
  opens, which includes coming back from Setup, and when the window gets focus while the prompt is
  showing. Nothing polls.
- **One test for "registered".** The prompt and the hook validator behind `lumen_artifact_health`
  read `settings.json` through the same function, so one cannot say the hook is there while the
  other says it is not: a command of Lumen's under PreToolUse, on each of the three tool names as
  Setup writes them. Another tool's hook on the same name does not count, nor does one left on
  PostToolUse, which runs after the tool has answered. A hand-written pattern that happens to select
  the tools (`mcp__lumen__.*`) is not recognised here, though `lumen doctor` recognises it.
- **The Claude Code plugin.** It registers the hooks from its own `hooks/hooks.json`, so
  `settings.json` lacking them proves nothing there. The prompt stays away when the ledger shows the
  hook working: any of the newest fifty Lumen tool calls that carry a repeat plan was identified,
  the rule `lumen doctor` uses. The ledger is opened read-only.
- **What it does not do.** It is in the window, and the window is not opened for it: someone who
  never opens it sees nothing, and `lumen doctor` says the same from a terminal. It does not fire
  when the hook is registered and calls are still unidentified, since running Setup would not
  fix that.

### Repeat reads are on by default

1.7.0 shipped repeat reads (a repeat `smart_read` or `recall_file` of an unchanged file answered
with one sentence, an edited one with a delta) behind `LUMEN_REPEAT_READS=on`. They are now on
unless turned off. Fresh installs and upgrades from 1.7.0 both get them.

- **Turning it off.** `LUMEN_REPEAT_READS=off` (also `0`, `false`) in the environment the MCP
  server gets, which applies from the next Claude Code session; or the persisted setting, a
  `lumen_settings.json` holding `{"repeat_reads": false}` in the Lumen data directory beside the
  ledger (macOS: the legacy data directory under `~/Library/Application Support/`), read on every Lumen call
  (about 30 microseconds), so it applies to the next call with no restart. The environment
  variable wins over the file when both say something. `ab` is still accepted for the
  experiment, by the environment variable only. With it off the answers are byte-identical to
  1.7.0's off, nothing is remembered, and `fresh` is not advertised.
- **A switch that cannot be read is off, and says so.** An unrecognised `LUMEN_REPEAT_READS`, or a
  settings file that is not valid JSON (a byte-order mark, UTF-16 and an empty file included), not
  an object, has a `repeat_reads` that is not a boolean, or cannot be read, answers every read in
  full (it does not fall back to the other source) and is recorded as a `repeat_reads_config`
  fault, at most one row every five minutes, with one line on the server's stderr. `lumen doctor`
  names the value or file and what to write instead. Previously an unrecognised value was
  silently the same as unset. A mistyped key in the file is just an unknown key: ignored, on.
- **Identity or base uncertain: the full result**, as in #32 below. Nothing about the default
  changes that.
- **Upgrade, by install.** The new behaviour needs the PreToolUse hooks of #32 (below), which 1.7.0
  never registered, so after updating until they are registered every call is answered in full,
  the same as with the switch off. Lumen app: Setup registers them and runs by itself only while
  `~/.claude/lumen/.setup_done` is missing; launching the app never edits `settings.json`. To run
  it again: quit Lumen, delete that file, open Lumen, restart Claude Code (there is no menu item
  for it; the Setup screen's hook check now says this, with the path). Claude Code plugin:
  `hooks/hooks.json` already carries them; update or reinstall the plugin and restart Claude Code.
  Homebrew `lumen-cli` alone: it installs `lumen` only, with no MCP server and no hooks, so there
  is nothing to update; install the app or the plugin.
- **`lumen doctor`.** The `repeat reads` check is silent when repeat reads are off, however they
  were turned off, and says nothing is wrong. When they are on but the hooks are not registered it
  says so, names what that costs, and says what to run for each install. It now prints the state
  (on or off, and whether the default, the file or the environment decided) and the settings
  file's path.
- **Cost.** A two-read scenario (the `repeat_reads` fixture, an unchanged file read twice):
  1.7.0 as shipped (default off) sends the outline twice, the second read 743 tokens; now the
  second read is 30 tokens (two reads: 1,486 tokens, now 773; the first read is the same bytes
  either way). Offering `fresh` on `smart_read` and `recall_file` adds 174 bytes to the tool
  list a session loads (about 43 tokens at 4 bytes a token, an estimate), and nothing with
  repeat reads off; on the fixture one repeat read saves 713 tokens. The `optimizer_bench`
  baselines are untouched.
- **The benchmark harness** (`benchmarks/`, #46) now sets `LUMEN_REPEAT_READS=off` in its
  repeat-reads-off arms instead of relying on the default; left alone, every "off" figure would
  have measured the gate.

### The app can turn repeat reads off, and shows what they have done

Turning repeat reads off meant an environment variable or a hand-edited JSON file, and nothing
showed what they had done. Nothing the MCP server sends changes: no tool's output, `tools/list` or
hook.

- **A Repeat reads switch** on the Context tab of the main window, on by default. It writes the
  `lumen_settings.json` described above (atomically, readable by its owner only, the other keys of
  a valid file kept), so a change applies to the next Lumen read in every running Claude Code
  session with no restart, and it survives a restart, unlike the alert thresholds beside it.
- **It shows the settings file, not the server.** The window shows what the file says, or the
  default when there is no file. It does not read `LUMEN_REPEAT_READS`: a value in a Claude Code
  MCP config outranks the file and the app cannot see it, nor a `LUMEN_DB` there that makes the
  server read another ledger's settings file. Its text says both.
- **A file that cannot be used is shown as off, with the reason**: the same off the server
  applies, which it records as a `repeat_reads_config` fault. Switching on rewrites the file. A
  setting that cannot be read shows neither state and offers Retry; a change that cannot be saved
  puts the switch back and says why. Nothing is shown as saved that was not.
- **`lumen doctor` and the server's message about an unusable settings file** name the switch as
  the way to repair it, where they said only to edit or delete the file.
- **A Repeat reads card on the Optimizer tab**: the unchanged answers and the deltas served, the
  tokens they did not re-send, and how often the full result was asked for again with `fresh=true`,
  as n of m (m is the unchanged answers, the deltas and the `fresh` requests together; only
  requests made while repeat reads were on are counted, so it stops moving when they are off).
  It is counted apart from every total on the tab: in none of the token totals, the by-tool and
  by-channel breakdowns or the net value (the provenance counts do include these rows). It shows
  no dollar figure and no percentage. The tokens are counted against reading the whole file again,
  which is more than the outline a read returns with repeat reads off, so the card does not call
  them a saving and says what they are counted against. The figure covers only answers whose token
  count was measured, says how many when that is fewer than all of them, and is a dash when none
  was. The card says when the settings file turns repeat reads off, and counts the reads answered in
  full because Lumen could not tell which conversation they came from, with a link to Setup (the
  app install's remedy; on the plugin, update or reinstall it, as the upgrade notes above say).
- **Stats.** The optimizer report gains `repeatReads`; there is no schema change. A backend that
  does not send it gets no card.

### Repeat reads follow the conversation, not the process (#32)

`/clear` and `/resume` keep `lumen-mcp` running, and it read `CLAUDE_CODE_SESSION_ID` once at
spawn. After a `/clear` the repeat-read gate still held the old conversation's bases and answered
"unchanged" to a model whose context was empty, and the ledger filed the call under the wrong
session. Nothing in the MCP request says which conversation it is from (`_meta` carries only
`claudecode/toolUseId` and a progress token), so the identity now comes from a PreToolUse hook.

- The intercept hook is also registered on `smart_read`, `recall_file` and `compress_logs`. On
  those it blocks nothing: it writes a small record, named by the call's `tool_use_id`, beside the
  ledger, holding the session and, inside a subagent, the agent. The server claims (reads and
  deletes) the record by that id. Bases and ledger rows are keyed by it, and a change of
  conversation drops every base. A subagent shares its parent's session id, so it is keyed by
  session and agent and is never told the main agent's read is in its context.
- No record, a stale or malformed one, an unwritable directory, a call the hook never saw: the
  answer is the full result, the ledger's session is empty and the plan is `unidentified`. The
  stale environment variable is never used to decide anything.
- Records of calls that never arrived (a denied permission, a crash) are pruned by age and count,
  by the hook and when the server starts. The hook records `identity_unrecorded` when it cannot
  write one.
- Upgrade: an install from 1.6.0 or 1.7.0 has the intercept on `Read` alone, so the validator
  reports it stale and Setup registers the three new matchers. Until Setup is re-run (or the
  plugin reinstalled) on such an install, Lumen calls cannot be attributed to a conversation:
  repeat reads give the full result and the ledger's session column stays empty. The validator
  and `lumen doctor` say so and say to re-run Setup. The redirect after a working `smart_read`
  (#36, below) still works there: it also accepts a row with no session for the same path from
  the last hour, so the retry is redirected as it was before this change. A row from a known
  other conversation never counts. The plugin registers the same hook under its own tool names
  (`mcp__plugin_lumen_lumen__*`); with the plugin and Setup both installed a call to
  `mcp__lumen__*` runs the hook twice, as `Read` already did, and the two write the same record.
- A record is written to a temporary file and renamed into place, readable by its owner only, and
  one dated more than a minute ahead of the clock is treated as unknown, like an old one.
- `read_events` gains `agent_id` (a plain ALTER through `init_schema`, nothing to migrate).
- Cost. Live, on a 331-line file after a `/clear`: the full 256-token outline (2,457 tokens as a
  plain read) where the server used to send a 30-token stub the model could not use. Repeats inside
  one conversation are unchanged on the `repeat_reads` fixture: 30 tokens for an unchanged file
  against 743, a 107-token delta against a 603-token outline. The hook and the server's claim
  each add about 1–2 ms to a Lumen call (medians over 200 runs).

### The retry escape valve no longer hands back a whole file after a working `smart_read` (#36)

The intercept lets a second built-in Read of a large file through, for a model that cannot reach
the Lumen tools. It also let through a model that could: `smart_read` worked, the model read the
file anyway, and the whole file came back, with Claude Code's own unchanged-file dedup out of
reach because the first Read was blocked and never entered its cache.

- When `smart_read` or `recall_file` has answered for the path in this session (the ledger says
  so), the retry is redirected once more, to `recall_file`. A model that needs the file whole,
  to Edit it for example, retries again and gets it, so no model is ever stranded.
- With no successful `smart_read` or `recall_file` recorded for the path, nothing changes: the
  retry is released at once. Any trouble reading the ledger counts as "none recorded".
- A release after a working route is recorded as `retry_escape_valve` with detail
  `after_lumen_read`, so it can be counted apart from the valve's intended use.
- On a 1,442-line fixture the scenario costs 444 tokens (a 211-token redirect and a 233-token
  `recall_file`) against 13,807 for the released Read.

### The settings file is read with a limit, and a bad value is repeated back in short

`lumen_settings.json` is read on every tool call, and it was read whole, whatever it was: a very
large file was read into memory each time, and a named pipe or a device where the file should be
could block every call. A file larger than 64 KiB, or one that is not a regular file, is now
reported as unreadable: repeat reads are off, as for any unreadable file, and the problem says
why. A wrong-typed value in the file, and a `LUMEN_REPEAT_READS` that is not `on`, `off` or `ab`,
are repeated back in the fault row and on stderr as written when that is at most 64 characters,
and otherwise as its first 64 characters and an ellipsis. A valid file is read exactly as before.

### A rewind the daemon could not see is now treated as one

When the daemon tailed a transcript from a byte offset without that transcript's tracker (an ingest
that failed part-way, or the notify and poll paths racing on one file), it started from an empty
state and recorded nothing, so a rewind in the lines that followed went unseen and a repeat read
could be answered with a stub or delta whose base was no longer in the model's context. It now
records a `resync` event stamped now, which the repeat-read gate answers with the full result, the
same direction an event with no usable timestamp already took. A restart is unaffected: its initial
pass reads every transcript from byte 0, so it neither loses a tracker nor writes a resync.

### A release is never published with another version's notes

The release workflow took a release's notes from its `## [<version>]` block in `CHANGELOG.md`. When
there was no such block it published `RELEASE_NOTES.md` instead, which still held the v0.1.0 text,
and generated notes if that file was gone, so a tag pushed before its changelog block existed
would have been released under 0.1.0's notes with nothing failing. A block with only blank lines
under its header was published as empty notes.

- `scripts/release-notes.sh <version>` prints a version's block. It prints nothing and fails when
  there is no block, when it is empty, and when the file cannot be read. The version is matched
  literally: `1.8.0` no longer matches a header such as `## [1x8y0]`.
- The workflow's first job runs it on a tag push, so a tag without a block stops the run before
  any build starts, and the release step runs it again and publishes its output as the notes.
  `RELEASE_NOTES.md` is deleted and `--generate-notes` is gone: there is no other source. A
  prerelease tag (`v1.8.0-rc.1`) needs a `## [1.8.0-rc.1]` block of its own.
- `scripts/test-release-notes.sh` runs in CI on Linux and macOS, the two systems the release
  workflow runs it on. It covers a missing, an empty, a blank-only, a CRLF and a last block and
  lookalike headers, parses the repository's newest release block, and fails if
  `RELEASE_NOTES.md` comes back or the workflow names another source of notes.

### A log that compresses badly is returned as it is, and its cost is counted as sent

`compress_logs` compared the body it compressed with the log and left out the header it adds, so a
log that compressed by less than the header cost came back costing more than the log, under a
header that said it saved tokens. On 56 synthetic logs, by path and inline (112 replies), 31 cost
more than the log and 11 of those headers said they saved tokens.

- The reply is counted whole, header and body. When that is not cheaper than the log, the log
  itself comes back with no note, recorded as `would_inflate`, the route the other tools use when
  they return the file instead. A log with nothing visible in it keeps its header: answering it
  with itself would send an empty text block, which a client may reject.
- A log that does compress is answered with the same text as before, and what is recorded for it
  is lower: `_meta` and the ledger now include the header, so `saved_tokens` falls by its cost.
  That is 31 to 36 tokens on the optimizer benchmark's four compressed logs, more for a longer
  path, and `optimizer_baseline.json` moves to match (tokens returned 590 to 621, 1,062 to 1,097,
  1,180 to 1,215, 804 to 840). The header's own `saved N` still counts the body only, so it reads
  higher than `_meta` by that amount. The benchmark's incompressible log cost 9,507 tokens while
  being recorded as 9,472; it now costs 9,472.
- Each call counts the reply once more, about 0.13 ms per 1,000 tokens of reply in a release build:
  0.1 to 0.2 ms on the optimizer benchmark's four compressed logs, 1.2 ms on its incompressible
  one, whose reply is as long as the log.
- With `LUMEN_RANKED_OUTLINE=on`, a file the ranked arm declined was answered with its outline even
  when the outline cost more than the file, which a small file's can. It is now answered with the
  file, recorded as saving nothing. A blank file keeps the outline.

### The guarantee that no tool returns more than the file is worded as far as it is checked

1.6.0 put its backstop "at the single point every metered reply passes through" and gave the worst
overage as 27 tokens, 0.33% of the file. There is no such point, and `compress_logs` was outside
the guard until the entry above. The guard sits where each reply is built: in `metered_guarded`
(the `smart_read` outline and every `recall_file` branch), on the ranked arm's decline, in the
fallback for a file with no structure, and in a check of `compress_logs`'s own. `smart_read` with
`mode="full"` has none: the whole file is what it was asked for.

A corpus test, in `crates/lumen-mcp/tests/efficiency.rs`, counts the text each reply sent instead
of trusting what the reply says about itself, and holds it to the file plus what labels it. It asks
70 files of this repository (every UTF-8 file of 300 lines or more that fits in 50,000 tokens, of
any extension), 8 small files and 56 synthetic logs, in every request shape: 1,134 calls on the 70
files. What a reply may cost above the file:

- the file returned instead of a view that would have cost as much, behind a one-line note: at most
  80 tokens (27 at most seen);
- a file with no structure, behind a note that carries its path: the note;
- `smart_read(mode="full")`, behind a header that carries the path: the header;
- `compress_logs`: nothing, the log itself when its reply cannot cost less;
- a file or log with nothing visible in it: no bound, because the reply must not be an empty text
  block, which a client may reject.

Not checked: error replies, the answers to repeat reads, the JSON-RPC envelope and `_meta`, a file
that is not UTF-8 or is over 50,000 tokens, and a `recall_file` request whose start line is after
its end line or far beyond the end of the file, which the test does not generate. The README and
`docs/efficiency.md` say the same and carry the re-measured figures: interception costs more than
it saves on 31 of the 61 files of 300 lines or more in this repository, not 17 of 31, and an
outline costs 7.4% of the file, not 6.2%.

### The startup claims of 1.6.0 are held to the source

1.6.0 said nothing in the tray or sidecar startup uses `?` or `expect`. Its one stated exception,
`Builder::build`'s `expect`, stands. The rest was too broad: `build_tray_menu_items`,
`write_atomic` and `stable_binary` use `?`, each handing a failure back to a caller that deals with
it, so none of them can abort startup, but "nothing uses `?`" was not true of the code.

`crates/lumen-core/tests/startup_scan.rs` now reads the app's four production files (comments,
strings and test modules blanked out; standard library only, nothing new in the tool path) and
fails when:

- the `.setup` closure, or any function of the crate that it reaches by name, uses `?` other than
  those three, or one of them stops using `?`, or its caller stops dealing with the error;
- production code has an `expect` other than `Builder::build`'s in `run`.

`unwrap` is not claimed. Five remain in production code (four on a `Mutex` lock, one on a JSON
value the lines before have made an array); the scan lists them and does not pin them. Also not
checked: calls through trait objects or function pointers, code outside the crate, and a `?` in a
function startup does not name.

### Release logging is held to the source, and not to a launch

1.6.0 said logging is registered unconditionally, at `Warn` in release. The same scan fails when
the log plugin is registered anywhere but the top level of the `.setup` closure, or behind a `cfg`,
a condition or an earlier exit; when it loses its file target (`TargetKind::LogDir`) or takes its
level from anything but `log_level_from`, whose release default a unit test pins to `Warn`
(`the_log_level_defaults_differ_by_profile_but_are_never_off`); when the `STARTUP:` line that
follows it is gone; when a second logger appears; and when a `Cargo.toml` in the workspace caps the
`log` level with a `max_level_*` feature or makes the plugin optional.

Not checked: that a release binary writes `Lumen.log`. Nothing launches a release build of the app
and looks for the file: the app e2e and the Issue 5 launch check run debug builds, and
`scripts/verify-install.sh` only checks that an installed app is running. A `release_max_level_*`
feature switched on by a dependency cannot be read from a manifest.

### A release dispatch builds the tag's own commit, and never main

Run by hand for an existing tag, the release workflow built the head of `main` and attached the
result to that tag's release, so a re-run could ship code that was never in the version. It took the
commit from `github.sha`, which on a dispatch is the branch head, and pasted the tag input into a
shell script's text, so an input such as `v1.0.0"; touch pwned; "` ran as a command.

- The first job turns the tag into the commit it points at, once (an annotated tag is peeled), and
  every job that checks out this repository does so at that SHA: the macOS build, both CLI builds,
  both GUI builds and the Homebrew tap's formula templates. Nothing reads `github.sha` or the ref to
  decide what to build, and a tag moved after the run started cannot change it.
- Before anything is built the run refuses a tag that is not `vN.N.N`, `vN.N.N-pre` or
  `vN.N.N+build`, a tag that does not exist, and one that does not point at a commit. The input
  reaches the script through `env`, not through its text.
- When the tag's source does not build, the run fails with `<tag> (<sha>) does not build. A dispatch
  never builds from main: ship a patch release (a new tag) instead.` as an annotation and in the job
  summary. It never falls back to main.
- `scripts/test-resolve-release.sh` runs the workflow's own script in a throwaway repository whose
  `main` is past every tag, in CI on Linux and macOS. It covers annotated, lightweight and nested
  tags, a tag push, malformed, missing and non-commit tags, injection-shaped input and the job that
  reports a failed build, and fails if a job checks out anything but the resolved commit.

Not checked: a dispatch itself. The test runs the step against a local repository, so the fetch with
the job's token and the conditions of the jobs that follow run only on GitHub. A dispatch for a tag
before v1.1.4 now fails in `update-tap`, because that tag has no cask template to copy, and leaves
the tap as it was.

## [1.7.0] — 2026-10-11

### Repeat reads: a second `smart_read` or `recall_file` of the same thing no longer costs the same again

Ships behind a flag, default off. `LUMEN_REPEAT_READS=on` (or `ab`, split by a hash of the path)
makes Lumen remember, in memory, what it returned for each `(session, request)`. An unchanged
file is answered with one sentence, an edited one with a delta of the changed symbols, and
anything it cannot vouch for with the full result. The built-in Read is not touched.

- **Unchanged first.** Content-addressed (git blob hash computed in-process, CRLF read as LF; never
  mtime). The answer is about 30 tokens against 743 for the fixture's outline; on 60 real edits
  the unchanged answers total 1,800 tokens against 44,306 for the outlines they replace.
- **Deltas** name `changed [body]`, `changed [signature]`, `added`, `removed` and `moved`
  symbols with current line ranges. A symbol shifted by an insertion above it is `moved`, not
  changed. `recall_file` of a changed symbol returns a diff scoped to it. On 60 real edits a delta
  is median 16.6% and p90 31.5% of the full outline (1.7.0's first cut: 41.7% / 59.0%); the p90
  is above 25% because small outlines pay a fixed ~40 tokens for the sentence that tells the
  model how to ask again. That figure is per delta served, not an overall saving.
- **Is the base still in context?** The daemon records one `session_events` row per compaction
  and per rewind from the transcript; after either the answer is the full result. A subagent
  sharing the session, or an event not yet ingested, cannot be seen, so every repeat answer says
  it refers to an earlier read and to call again with `fresh=true`. `fresh=true` on both tools
  always returns the full result and is recorded: its rate is the error rate of the feature.
- `read_events` gains `repeat_arm` and `repeat_plan`, and a new `session_events` table with its
  index, through the shared `init_schema` on all three code paths; tested from a 1.6.0 database.
- With the flag off, `optimizer_bench` is unchanged to the token and the baseline is not
  re-blessed.

## [1.6.0] — 2026-10-08

Three things this release is really about: the hooks, which on a machine with no `python3` did
nothing and said nothing, and now run in `lumen-mcp`; the menu-bar icon that never appeared for
one user; and the discovery that two of the three "honesty" figures published in 1.5.1's
efficiency report were themselves wrong.

### The hooks did nothing without `python3`, and said nothing

**On a machine with no `python3`, 1.5.1's hooks were inert.** Both were bash scripts that ran
`python3` to write a row or decide a block, and the recorder that would have reported a failure ran
on `python3` too. Where PATH has no `python3`, which is the case with Python from python.org's
classic Windows installer (it adds `python` and `py`) or with no Python at all, the meter exited 0
having written nothing. Its one word, `python3: command not found`, went to a stderr that Claude
Code does not show on exit 0. The intercept let a 400-line read through, and neither left a fault.
It was the same on CI's Windows runner with `python3` as the App Execution Alias Windows puts there
when Python is not installed: `command -v` finds it, and nothing is written or blocked. With a real
Python 3 as `python3`, the same scripts write the row and block the read, so the silence was
`python3`'s absence and nothing else.

**The hooks are now shims over `lumen-mcp hook`.** Metering, the intercept's decision and the fault
records moved into `lumen-mcp`, the binary the MCP server already runs as, so the hooks need no
Python and count tokens in-process. What is left in shell needs bash, cat and date, and does one
thing. When `lumen-mcp` cannot be found or run, it says so on stderr and in the fault spool, as
`lumen_mcp_missing` or `lumen_mcp_unrunnable`, and lets the tool call through: without that binary
the MCP server is not running either, so a block would send the model to tools it cannot call.

Tests now run the commands Setup registers the way Claude Code runs them, with payloads Claude
Code really sent and the real `lumen-mcp`, in CI on macOS, Ubuntu and Windows. The shell is
`/bin/sh -c` on macOS and Linux and Git Bash on Windows. A Read lands as a row, a Bash command as a
`bash_output` row, and a large read is blocked once. With `lumen-mcp` deleted, the same read is let
through and reported. With no Python, with `python` and `py` only, and with a `python3` that
behaves as the Store alias does, the hooks meter and block. Before anything is built, a probe
prints what a hook's shell finds on each runner, unedited.

**Faults say where they came from.** The intercept's recorder wrote `"channel": "cli"` and
`"version": null` as literals. A fault raised under VS Code said `cli`, and an issue `lumen report`
filed from a hook fault carried no build version. The Rust recorder read its channel from
`LUMEN_CHANNEL`, which nothing sets, so its faults said `unknown`. Faults now take their channel
from `CLAUDE_CODE_ENTRYPOINT`, as the meter's rows do, and carry the version of the build that
raised them. The shims write their own fault when there is no binary to do it; it follows the same
channel rules and reads the version from the stamp in the script.

**Registered commands are quoted.** Setup registered each hook as a bare path. In a home with a
space in it, the shell split the path at the space. On Windows an unquoted backslash is an escape,
both to bash and to Claude Code's scan of the command, so
`C:\Users\Jane\.claude\lumen\lumen_meter.sh` arrived as `C:UsersJane.claudelumenlumen_meter.sh`.
Those hooks never ran. Each command is now the script's path as one single-quoted word, with forward
slashes on Windows. Setup's validator reports a bare path that holds anything the shell does not
take literally, and asks for Setup to be run.

**An upgrade keeps what the user set.** The upgrade test installs 1.5.1's own scripts and
registration in a home named `Jane Doe`. The first launch of this build rewrites the two scripts.
It leaves `settings.json` and `~/.claude.json` byte for byte as they were, and a login item the user
switched off stays off. Setup then quotes the commands and keeps everything else: the user's other
hooks and MCP servers, and `LUMEN_HOOK_ENABLED=0` and `LUMEN_CAPTURE=0`, which go on working.

**The plugin's hooks are Setup's.** The scripts in the Claude Code plugin's `.claude/hooks/` were
`python3` scripts of their own, and still inserted `is_subagent`. They are now generated from
Setup's templates with nothing baked in, and a test fails if the committed copies differ by a byte
(`LUMEN_BLESS_HOOKS=1` regenerates them). The plugin's `hooks.json` now quotes the plugin root and
registers the meter on `Bash`, as Setup does. It drops the three `mcp__lumen__*` matchers that
Setup retired in 1.2.1.

**`verify-install` runs the hooks instead of grepping them.** It checked the intercept for the
names of its fail-open guards. That passed 1.5.1's hooks and failed this release's, whose guards
are in `lumen-mcp`. It also passed hooks whose `lumen-mcp` was gone, which meter and block nothing.
`scripts/verify-hooks.sh` runs the installed hooks as Claude Code does, against a scratch ledger,
and judges them by the rows, blocks and faults they leave. Both verify-install scripts call it, the
PowerShell one under Git Bash. Hooks from before `lumen-mcp hook` are reported and not run: nothing
in them promises to write to the scratch ledger rather than the real one.

Three faults in verify-install itself are fixed:

- `--bin-dir` was the first place it looked rather than the only one, so the app bundle's binaries
  answered for the build being checked.
- A flag given without its value hung it. It now exits 2.
- On Windows, `-BinDir` passed the GUI check falsely: the build's `lumen.exe` answered for
  `Lumen.exe` on a case-insensitive filesystem.

### The menu-bar icon, and why nothing was logged

Issue #5 reported Lumen running with two processes and no menu-bar icon on macOS 26 — and because
both windows start hidden and the tray is the whole interface, no popover, no window, and no way
to reach the fault reporter to report it. It did not reproduce here.

**The diagnostic asked for did not exist.** `tauri_plugin_log` was registered under
`if cfg!(debug_assertions)`, and no other logger exists in the crate, so in a released build every
`log::error!` went to a no-op sink and `Lumen.log` in the legacy log directory was never
created. The 1.5.1 CHANGELOG and a comment on the issue both asked the reporter for a line from
that file. Logging is now registered unconditionally, at `Warn` in release, with `LUMEN_LOG` to
raise it and `Stdout` retained so running the binary from a terminal shows its output.

**The likely cause needed no error at all.** ⌘-dragging a status item off the menu bar makes macOS
persist `NSStatusItem Visible <autosave>` = false, permanently: from then on AppKit creates the
item and immediately hides it. `build()` returns `Ok`, the app logs success, and the fallback added
in 1.5.1 never fires — the app is exactly as unreachable as before, while looking healthy from the
inside. It is per-user preference state, which is why it does not reproduce on another machine.

Lumen now clears that preference before building the tray, and says so rather than silently
overriding a choice someone may have made deliberately: on the launch that repairs it, the window
opens once and says what hid the icon, that hiding it never stopped Lumen, and that **Quit Lumen**
in the icon's menu does. `TrayIcon::set_visible(true)` is *not* the fix — it only re-creates a
missing item and never calls `NSStatusItem::setVisible`.

**"Built" is not "visible", and is no longer treated as such.** After the event loop starts, the
status item's rect is checked at +500ms, +1.5s and +4s, and classified `Present` / `Absent` /
`OffScreen` (a full menu bar or a notch clip — which Lumen cannot fix, and now says so instead of
pretending). On `Absent` it asks AppKit directly to show the item. The build itself is not retried:
its only macOS error paths are deterministic, so a second attempt cannot succeed.

**The check runs only where it can answer.** Only macOS gives a status item a place to judge
against the menu bar; elsewhere the check can only say "unknown". As first written for this
release, three unknowns counted as an absent tray, so on Linux every launch logged the tray as not
visible and opened its window six seconds in. Off macOS the tray is now left "unknown (not yet
verified)" and the window stays closed. That has a cost: a Linux desktop with no tray host, such as
GNOME without an AppIndicator extension, is not detected, and since `lumen show` and re-opening the
app work only on macOS, Lumen is as unreachable there as it was in 1.5.1. Where the checks do give
up, the window they open now says why. Home had read the startup health once, at load, so that
window opened with an empty banner.

**Nothing in the tray or sidecar startup uses `?` or `expect` any more.** Three `?` on tray-menu
construction and two `expect`s on the daemon sidecar could abort startup outright, and 1.5.1's
fallback covered none of them. Every step of `setup` now either succeeds or records a degradation,
startup ends with one decision about reachability, and the app says so in a banner instead of
presenting itself as healthy. One `expect` is left, on `Builder::build` itself: if Tauri cannot
build the app at all there is no window, no tray and no event loop to degrade to, and that failure
still ends the process with a panic, as it did in 1.5.1. A latent
trap came with it: `DaemonChild` was only managed on the success path while the exit handler called
`state::<DaemonChild>()`, which panics if unmanaged — so degrading past a failed spawn would have
traded a startup panic for a shutdown panic.

**And on macOS there are now ways in that do not involve the tray.** `open -a Lumen` or
double-clicking the app reveals the window (previously it did nothing at all). `lumen show` asks a
running instance to surface. `lumen doctor` prints what a bug report needs in one paste — status-item preferences across
both domains, processes, menu-bar managers, log and database paths — with the likely cause named
and its one-line fix. It reads the visibility flag in both forms macOS writes,
`NSStatusItem Visible …` and `NSStatusItem VisibleCC …`, as the app's own repair does. The
fallback also switches to `Regular` activation while degraded, because an Accessory process has
no Dock icon and a window it "shows" has nothing to bring it forward.

### Two of the three published honesty figures were wrong

**"64.9% of reads never reached a Lumen tool" was a bad denominator.** It divided every recorded
read of every file type by the total, so reads that were never eligible for interception were
counted as bypasses — it measured the file mix of one machine, mostly how many screenshots its
author had looked at. On the honest denominator, **0 of 1,573 eligible reads leaked**. Coverage on
what the hook claims to cover is complete, and a test now asserts that equality rather than a
ratio, alongside a *scope* table that prices what is genuinely not covered (`.md` 250k tokens,
`.scss` 180k, `.js` 98k) instead of hiding it inside a percentage.

**Half the missed-optimization baseline was screenshots.** 3,431,295 of 6,931,572 tokens came from
124 reads of binary files, whose `full_tokens` is a bytes/4 estimate this codebase already
documents as overstating a PNG by ~40×. `lumen-stats` had excluded these since 1.2.1 using a
constant that already existed; the efficiency test aggregated raw. Both now read one list from one
place. 28 duplicate rows were also collapsed — two meter hooks are registered on a development
machine and both fire.

Five places independently decided "would Lumen have handled this file?", and they disagreed. That
is now one module, and the disagreement it hid included a real bug: `mts`/`cts` were accepted by
the ranked language detector and not by the structural one, so `smart_read` on a `.mts` file
produced a one-item whole-file "outline" and metered it as a ~95% saving of a file it never looked
inside.

### The model was told figures nobody measured

The model routes on what it is told. Every block told it that an outline costs "~5-10%" of the
file and "typically saves 80-93% of context", or for a log "typically 40-80%". This repository's
`CLAUDE.md` said each `smart_read` of a large file saves "3000–4500 tokens". `compress_logs`
described itself as "fully reversible, no information loss". None of it came from the ledger, and
the last was false: the middle of a long stack trace is dropped and only its frame count is kept.

The tool descriptions, the intercept's message, `CLAUDE.md`, `MESSAGING_CONTRACT.md` and the
README now carry no figure, since every tool reply already reports what that call saved.
`compress_logs` says what it drops. `model_copy.rs` fails on any percentage, approximate number,
token count or "typically" in the tool list or `CLAUDE.md`, and the intercept's message is
checked the same way. The dollar figures that rested on the assumed rounds-remaining constant `R`,
in the README and `docs/efficiency.md`, are withdrawn until `R` is derived per call.

### The Optimizer called screenshots unverified, and priced what it could not

Provenance counted every row not marked `measured` as unverified, image reads included. Those rows
carry no count and say so, as `unsupported`. So the first screenshot read turned "never estimated"
into "partly unverified", with a note that the events predated provenance tracking. Image reads are
now on neither side of "N of M". On the ledger where this was found, 3,192 of 7,800 became 2,584
of 7,192; the 608 removed were all PNG and JPEG reads. The note no longer says that every
unverified row predates tracking, since `estimated` rows were tracked.

An unpriced net value rendered "+$0.00, roughly break-even" beside "no net figure is claimed". It
now renders a dash. The tests read the rendered page, and each one failed with its condition
removed.

### No tool can return more than the file it was asked about

170 recorded calls had cost more than a plain read — 92,347 tokens, and that is a floor, because
the JSON-RPC envelope is never counted. 96% of it came from one mechanism: `recall_file` prefixed
every line with `{lineno:>5}: `, about three tokens of gutter, and applied it across essentially
whole files because name matching used `contains()` while the tool's own description promised exact
matching. `names: ["e"]` selected nearly every item in a file.

Exact matching now comes first, with substring as a labelled fallback capped at five items; the
gutter is `123|`; overlapping and adjacent items merge so shared lines are emitted once; the
no-selector branch returns the outline rather than the file plus a header; and `mode="full"` has its
own route so handing over a whole file cannot pool with outline savings.

Then a backstop at the single point every metered reply passes through: **a reply that would cost
more than the file returns the file instead**, with one line saying so, routed `would_inflate`. Not
an error, which would leave the model with nothing and burn a round; not a truncation, which would
silently return less than was asked for. A corpus test asserts it against every file in this
repository at or above the threshold, for every shape a caller can ask for. Worst overage across the
corpus: 27 tokens, 0.33% of the file.

### The developer meter hook laundered unsupported files as measurements

It did `FULL_TOKENS=$("$LUMEN_TOK" < "$f" 2>/dev/null || echo 0); TOKEN_SOURCE="measured"`,
discarding the exit code — and `lumen-tok` exits 3 for a file that is not valid UTF-8. A PNG was
recorded as `full_tokens=0, token_source='measured'`: an unsupported file laundered as a
measurement, in the one column that exists to tell those apart. The installed copy was correct. The
drift test compared column *names*, which is why it passed.

The hook is now a shim over `lumen-mcp hook meter`, as the installed one is, so there is one meter
and no copy to drift. A file that is not UTF-8 is recorded as 0 `unsupported`, and
`a_file_that_is_not_utf8_is_recorded_as_unsupported_with_no_count` fails if it is recorded as
`measured`. That test and two others were missing until just before release. The shell meter's
tests went with the shell meter, and three rules they held were tested nowhere else: this one, no
row for a command that printed nothing, and no second count for Lumen's own tools. Each was checked
by breaking it with the whole `lumen-mcp` suite running, and each time exactly one test failed, on a
line this change added.

### A Read the ledger refused is a fault the app shows

**1.5.1 dropped it without a trace.** The installed meter ran its INSERT as
`python3 -c '…' 2>/dev/null || true`, so a ledger that would not take the row — read-only,
locked past the timeout, no `python3` — lost the Read with no row, no fault and no message.
Metering now runs in `lumen-mcp hook meter`, and a refused INSERT is a `meter_write_failed`
record in the fault spool, carrying the file, its line count and SQLite's error, plus a line on
stderr.

**The app did not show it.** Three defects stood between that record and the screen:

- The badge opened the ledger with `connect_db`, which creates and migrates it, on every
  navigation, and a ledger that would not open put the badge out while faults sat in the spool.
  It now opens the ledger read-only and counts the spool whatever the ledger does; a ledger that
  exists and cannot be read counts as one fault, so the badge leads to the report that names it.
- The report drained the spool into the ledger that had just refused the row. The batch stayed in
  `faults.jsonl.draining`, which nothing counted or listed: the first visit to the report put the
  badge out, and the report showed "spool drain: attempt to write a readonly database" and never
  the Read that was lost. A refused drain now puts the batch back, and the report lists what the
  spool holds beside what the ledger does, on every visit.
- A batch left aside was read as UTF-8 or not at all: one torn multi-byte character and the whole
  batch was never read, and the next drain deleted it after a merge whose failure it ignored. It
  is now read lossily, line by line, and a merge that fails leaves the batch where it is.

**Restoring the ledger was not enough to take writes again.** SQLite creates `lumen.db-wal` and
`lumen.db-shm` with the ledger's own mode. A Read metered while the ledger was read-only, with no
`-shm` there already, left a 0444 `-shm` behind; once the ledger's permissions were restored,
every insert was still refused with "attempt to write a readonly database", against a writable
ledger. Every writer now gives the two sidecars the write bits the ledger has before opening it
(Unix; Windows keeps no mode to copy). The library test of this case had switched the ledger to a
rollback journal, so it never ran the journal mode the ledger uses.

**The report tests drained the spool of whatever home they ran under.** `cargo test` on a
developer machine emptied that machine's fault spool into a throwaway ledger: a canary fault in a
scratch home's spool was gone after `cargo test -p lumen-core --lib db_reader`. The tests now name
their spool, and the same probe leaves the canary in place through the whole `lumen-core` suite.

### `read_events.is_subagent` is gone

Every writer inserted a literal 0 into it, so every row said "main agent", and a
`GROUP BY is_subagent` looked exactly like a measurement. The 1.1.5 notes called it a placeholder
awaiting a source of truth. None exists for every writer: Claude Code hands hooks an `agent_id`
only inside a subagent, and an MCP tool call carries no agent identity at all. Nothing read the
column, so a migration drops it, and the inserts and queries no longer name it. Nothing is lost,
since every stored value was the literal. `a_populated_1_5_1_database_loses_is_subagent_and_keeps_every_value`
runs the migration on a 1.5.1 table holding rows. Where the drop cannot run, an insert still lands,
because the column has a default and the insert never names it. `turns.is_subagent`, which the
daemon derives from the transcript's path, stays.

**A test run changed an installed ledger's schema.** `crates/lumen-mcp/tests/efficiency.rs`
measures the ledger of the machine it runs on. It opened that ledger with `connect_db`, which
creates and migrates. While this release was being made, it ran on a machine with 1.5.1 installed
and applied this migration to the live ledger. 1.5.1's own migration then added the column back, at
the end of the table. No value was lost, because the column held only the literal. Tests that read
a ledger they do not own now open it with `meter::open_read_only`, which creates, migrates and
writes nothing.

### A fallback that hides a failure now fails a test

**The guard asked for in 1.2.1 was never written.** A failure replaced by a value that looks real
has shipped four times: the DMG tokenizer, 1.1.3's ingest, the meter's `|| echo 0`, and the INSERT
above. A test now scans the production Rust and every shell script, Setup's hook templates
included, for `|| echo <number>`, `|| true`, `unwrap_or(<number>)`, `unwrap_or_default()` and
writes whose result is thrown away, and fails on any that its allowlist does not name with a
reason. It finds 57 today, under 56 entries, each saying why that one is not a measurement; an
entry that matches nothing fails the test as stale. Added in a scratch commit,
`lines=$(wc -l < "$1" 2>/dev/null || echo 0)` in the meter template failed it by name:
`setup.rs:1071 [|| echo <number>] in const METER_TEMPLATE`.

Where the honest reason would have described something wrong that the user can see, the code was
fixed instead:

- **The issue reporter's token file.** The curl config carrying the GitHub token was written at
  the umask's mode and chmodded afterwards, the chmod's failure dropped, and written through
  whatever already sat at its predictable name — on Linux, in a `/tmp` every user can write to.
  It and the issue body are now created owner-only by the call that creates them, and an
  existing name, a symlink included, is never written through.
- **Setup and Uninstall rewrote a config whose backup had failed.** The copy to `.lumen_bak` was
  `let _`, so the one undo might not exist and nothing said so. The step now fails and leaves the
  file as it was.
- **App startup dropped three errors.** Creating the data directory, the copy that moves the
  ledger over from `com.tauri.dev` — which logged "Migrated" whatever the copy did — and writing
  `~/.lumen_db_path` are now logged as errors, so they reach `Lumen.log` in a release build.
- **A poisoned lock emptied the degraded banner.** After a panic while the startup health was
  locked, its reads returned no degradations and an unchecked tray, which is a healthy launch, so
  no banner. The locks are now read through the poisoning.
- **Calibration rows were dropped with `let _`.** A refused row is now logged, once per process.
  It is not filed as a fault: the rows feed only `correction_factor`, which nothing in the app or
  the CLI shows.
- **A login-item marker that could not be written is logged.** Without it the next launch takes
  the item for never registered, and turns it back on if the user had turned it off.

The scan reads text, and what it does not read is listed at the top of the test: PowerShell,
workflow YAML, `${VAR:-N}` defaults, the frontend, and any fallback spelled another way.

### Opting out of Bash metering did not survive Setup

Through 1.5.1 the README said to stop recording Bash output by deleting the `Bash` entry under
`PostToolUse` in `~/.claude/settings.json`. Setup's validator then reported the hooks unhealthy,
"PostToolUse not registered: Bash", and the Setup run it asked for put the entry back, so
command output was recorded again. Upgrading to this release asks for that Setup run in any home
with a space in its path, and in every home on Windows, because the commands 1.5.1 registered
never ran there.

`LUMEN_METER_BASH=0` in the `env` block of `~/.claude/settings.json` now stops it, and Setup
keeps that block. Reads are still metered. Setup also recognises a deleted entry, a Lumen meter
on `Read` with none on `Bash` and no matcher retired in 1.2.1, which no release's Setup ever
wrote, and carries it over as `LUMEN_METER_BASH=0` instead of recording again.

### The app, launched and driven, before release

Until now the tests ran functions, binaries, and pages against a stand-in for the backend; none
launched the app. Two CI jobs now do.

**App e2e, on Linux and Windows.** `tauri-driver` launches the debug build over WebDriver and the
tests read what it shows: a tray that cannot be built leaves the window open, saying so legibly;
the Optimizer and Hotspots screens show what a ledger written by the real `lumen-mcp` holds,
checked against SQLite directly; an empty ledger shows the empty states; a healthy launch keeps
its window closed past the tray checks; a tray reported absent opens the window once they give
up, saying why. macOS has no WebDriver for its web view, so the app is not driven there.

**Issue #5, made on purpose, on macOS.** The preference behind issue #5 existed on nobody's
machine but the reporter's, so a job now creates it: both forms set false, the built app
launched, then the preferences, the icon, the window and what the window says are checked; a
second launch with nothing set must do none of it. It writes the app's preferences as the
logged-in user, so the script refuses to run outside CI.

Their first runs found three faults, all in this release's own unreleased changes, and confirmed
a fourth that had been fixed shortly before:

- Off macOS every launch opened its window six seconds in (above).
- A tray that failed to build was reported again at the first redraw as having "disappeared
  after startup", replacing the real reason in the banner and opening the window a second time.
- The window the tray checks open had an empty banner (above).
- The degraded banner named CSS variables that nothing defines, so in the light theme its bold
  text measured 1.03:1 against its background and the rest 2.21:1 and 2.69:1. They are now
  14.17:1, 4.26:1 and 3.98:1. The last is the app's secondary-text colour everywhere, still
  short of the 4.5:1 WCAG AA asks of body text, and not changed here.

Each test was shown failing with its fix reverted and passing with it restored.

### Frontend coverage is enforced per file

The coverage thresholds applied to the total. It passed while eight files sat under them, so
`pnpm test` exited 0: `home.html` at 79.82% of statements and 0% of functions, `setup.html` at 0%
of functions, and `panel.ts` at 83.33%. The thresholds now hold for each file. Files that no spec
loads are measured too; before, they were left out of the report altogether (`app.component.ts`,
`app.config.ts`, `app.routes.ts`).

Tests were written for what that exposed:

- the rendered controls on Home, Setup and the panel;
- the Optimizer's per-channel rows and missed reads;
- the tooltip's placement;
- the session warnings and refresh timer;
- the app shell's routes.

The Optimizer's `provenanceTip`, unused since 1.4.0, is gone. What is still excluded is listed in
`ci.yml` with the reason.

### Hotspots gave files their largest size, and called every project "this project"

The line count on the Hotspots screen is documented as the file's size at its most recent read.
Since the screen arrived in 1.4.0 it was the largest size any read had recorded, so a file that
shrank kept its old size there, and with it any advice to split it, after it was split. Running
the report over this repository's own reads found it: `setup.rs`, in the worktree this release
was made in, was read at 4,748 lines, then at 4,348 and 4,392 once a change here had shortened
it, and the screen still said 4,748. It now gives the latest read's count; a read that recorded
no count is passed over rather than taken for an empty file.

The ledger has no notion of a project, so the report covers every project it holds, but its
advice called a file's share of all of them its share of "everything this project has read",
and its empty state spoke of "this project" too. Both now say what is measured. The file that
1.4.0's entry gave as this repository's heaviest has never been in this repository; those
figures were the whole ledger's.

The report also reads the ledger three times rather than four. Release builds of 1.5.1's report
and this one ran on the same copy of a real ledger, 7,833 reads of 2,307 files: the median of 21
calls went from 8.81 and 8.67 ms to 7.76 and 8.47 ms over two rounds, and with the same reads 25
times over, from 218 and 204 ms to 138 and 137 ms. Every figure but the line counts, and the
advice that quotes them, came out the same.

### Maintenance

- build: `release.sh` read the commit subjects as `git log … 2>/dev/null | grep … || true`, so a
  `git log` that failed — a bad range, say — wrote a release entry with no notes in it. `git log`
  now runs once and unguarded and stops the release; `|| true` covers only grep finding nothing.
- chore(deps): Tauri 2.11.5 → 2.12.1, moved as one group: `tauri-build` 2.7.1, the autostart
  2.7.0, log 2.10.0, notification 2.5.1, positioner 2.4.0 and shell 2.4.0 plugins, and
  `@tauri-apps/api`/`cli` 2.12.1. The pinned `tray-icon` goes 0.24.2 → 0.25.1, the version Tauri
  2.12 uses, not the newer 0.26.
- build: every crate declares `rust-version` 1.94.0, inherited from the workspace. That is
  sqlx 0.9's floor, and every crate reaches sqlx through lumen-core; the GUI crate alone
  declared one before, 1.90, Tauri 2.12's floor, which the build could not honour. A new CI
  job, `msrv`, reads the version from `Cargo.toml`, checks every crate declares it, and builds
  the whole workspace, the Tauri crate included, on exactly that compiler.
- chore(deps): tree-sitter 0.26 → 0.27, sha2 0.10 → 0.11, dirs 6 → 7, plus every
  semver-compatible bump in `Cargo.lock`. A known-answer test now pins the SHA-256 output: the
  fingerprint that deduplicates filed issues is built on it, so a digest change would re-file
  every known fault.
- chore(deps): Angular 22.0 → 22.2, vitest 4 → 5, zone.js 0.16.3, jsdom 30.1, postcss 8.5.29,
  and a refresh of the npm lockfile. `@tauri-apps/plugin-opener` and the npm
  `@tauri-apps/plugin-shell` are removed: nothing imported them, and the sidecar is spawned from
  Rust.
- ci: `actions/checkout` and `actions/setup-node` v4 → v7 and `pnpm/action-setup` v4 → v6, off the
  deprecated Node 20 action runtime; the frontend now builds on Node 24 LTS.
- ci: two new jobs, `App e2e` on `ubuntu-22.04` and `windows-latest`, and `Issue 5 launch check
  (macOS)` on `macos-14`, described above. `tauri-driver` is pinned to 2.1.0 and installed with
  `--locked`. On Windows the job fetches the msedgedriver that matches the runner's WebView2
  runtime, and falls back to the one the runner image ships. There the tests run unelevated,
  through gsudo 2.6.1 (pinned, its checksum checked) at medium integrity. The first Windows run was
  elevated, as the runner's shell is, and failed all five tests with "DevToolsActivePort file
  doesn't exist": from WebView2 150 an elevated app's web view ignores
  `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS`, which msedgedriver opens its debugging port through
  (tauri-apps/wry#1782). The runtime and the driver were the same version, 153.0.4234.48.
- ci: the `test` job runs on macOS as well as Ubuntu and Windows, and runs every crate and target.
  It used to leave out the Tauri crate, where Setup and the hooks live. The hook end-to-end tests
  run first, with their output shown. Captured hook payloads are checked out byte for byte as Claude
  Code sent them (`.gitattributes`).
- ci: a compiler warning fails the `test` job on all three platforms, as clippy fails one in `check`,
  which runs on macOS only. Every Linux and Windows build had warned that two constants of
  `lumen doctor`, used only on macOS, were never used, and no job failed on it. They are compiled
  on macOS only now. In a Linux container, the job's flags fail the build with the constants as
  they were, and pass it with them gated.
- test: the first Windows run failed three tests that wrote `~/.claude.json` by splicing a path into
  a JSON string with `format!`. A Windows temp path made `\U`, an invalid escape. They now write the
  file through serde.
- test: a `lumen-daemon` test failed once on macOS, in the run meant to gate this release. Its
  daemons start in parallel, and on macOS std creates a child's pipes and marks them close-on-exec
  in two steps, so a daemon another test started in between inherited the supervised daemon's
  stdin. Holding it open, it kept back the EOF the test waits for, and the supervised daemon
  outlived its 10 s grace. The tests in `supervisor_exit.rs` now spawn one daemon at a time. A
  scratch test that gave a copy of that stdin to a second process failed the same way, and its
  daemon exited 57 ms after the second process did.
- ci: the `Verify install` workflow runs `verify-hooks.sh` on each OS, against the plugin's hooks in
  a scratch home, with the build's `lumen-mcp`.
- fix(daemon): an assistant record with `usage: null` is skipped. The daemon unwrapped the usage and
  relied on `is_billable`, in `lumen-core`, to have checked it, so an edit there would have
  panicked the daemon at ingest. The test was shown failing with `is_billable` edited to stop
  checking, and passing with the unwrap replaced.
- chore: removed `lumenator/src-tauri/Cargo.lock`. `src-tauri` is a workspace member, so cargo
  and the Tauri CLI both read the root lockfile; this one was unused and had been stale since
  0.1.0.

### Notes

- Fault reports filed from the app claimed `channel: "cli"`, because `Environment::collect`
  hardcoded it. They now say `gui`, and carry the tray state and any startup degradations — so a
  user who *can* reach the app files a report that already contains the answer.
- `LUMEN_SIMULATE_TRAY=err|absent|offscreen` is honoured in release builds. The tray failure paths
  need a live app and a real menu bar, so they cannot be unit-tested at all; this is what makes them
  reachable deliberately instead of by breaking your menu bar.
- Test fixtures shaped like toys were hiding behind the guard: `"fn alpha() {}\nfn beta() {}"` is
  eight tokens, so any reply about it legitimately costs more than reading it. Fifteen tests were
  asserting the inflation guard rather than the behaviour they were written for.
- The hook tests run the hooks as Claude Code 2.1.270 spawns them: the shell, the `bash ` prefix on
  Windows, the environment it builds. That is read from Claude Code, not run: no test starts Claude
  Code itself.
- A 1.5.1 process still running after the upgrade, such as an MCP server that Claude Code started
  before it, re-adds `is_subagent` each time it opens the ledger, and 1.6.0 drops it again. 1.6.0's
  inserts land either way, which is tested. 1.5.1's add the column back before inserting, so they
  land too, unless a drop falls between the two. That was read from 1.5.1's code, not tested.

## [1.5.1] — 2026-07-31

A patch release, and the reason to install it is that 1.5.0's Linux CLI does not run.

### The Linux CLI required a glibc most distributions do not have

`lumen-v1.5.0-x86_64-unknown-linux-gnu.tar.gz` links against `GLIBC_2.39` and exits
immediately on anything older:

| distribution | glibc | 1.5.0 |
| --- | --- | --- |
| Ubuntu 24.04 | 2.39 | runs |
| Debian 12 | 2.36 | `version 'GLIBC_2.39' not found` |
| Ubuntu 22.04 LTS | 2.35 | `version 'GLIBC_2.39' not found` |

The release workflow built it on `ubuntu-latest`, which GitHub moved to 24.04, so the
binary took that runner's glibc as its floor. The GUI job had been pinned to `ubuntu-22.04`
for a related reason; the CLI job never was. Anyone installing the CLI on a stable
distribution — including through the Homebrew formula on Linux — received a binary that
could not start. Every test passed the whole time: nothing running in-process can see a
dynamic-link failure.

Pinned to `ubuntu-22.04`, which lowers the floor to 2.35.

### Install verification, per platform, before release rather than after

`scripts/verify-install.sh` and `verify-install.ps1` check that an install *works*: the MCP
server is driven over stdio with a real JSON-RPC handshake and asked for its tool list, the
tokenizer is asked to count, the CLI is asked to refuse filing without `--yes`, and the
intercept is checked for both fail-open guards. Every install defect found so far was
invisible to a file-existence check — a bundled MCP server two days older than its UI, a
June hook script with no guards, and now a binary that cannot link.

A CI workflow runs them on macOS, Linux and Windows, plus a job that runs the freshly built
Linux CLI inside `debian:12` and `ubuntu:22.04` containers. That job is the one that would
have caught the break above: it deliberately runs the binary on distributions older than
the one that built it. All ten checks are now required before a merge.

### The fault button was rendered off the bottom of the tray popover

The popover is a fixed 320x400 window whose card was stretched to it, with `overflow:
hidden` turning overflow into silent clipping. The rows below the gauge are all
conditional, and each one pushed the next further down: measured at 308px with none of
them and 396px with a recorded fault and an update notice both showing, in a window with
392px of usable height. The button was present, styled and impossible to click.

The card sizes to its content now and the window follows it.

### The Windows browser filing route could never succeed

`cmd /C start "" <url>` passed the URL as an ordinary argument, and `cmd.exe` treats `&`
as a command separator. Every prefilled issue URL contains `&body=`, so cmd tried to run
the second half as a command and exited 1. The browser route exists so that someone
without the GitHub CLI can still report a fault; on Windows it could not.

### A failed tray left no way into the app

Reported from the field on macOS 26 (#5): Lumen running with two processes, no menu-bar
icon, and — because both windows start hidden — no popover, no main window, and no way to
reach the fault reporter to report it. The tray build result was logged and otherwise
ignored.

The main window is now shown when the tray cannot be built. That is not a fix for whatever
makes the tray fail, which remains undiagnosed and does not reproduce here on the same OS
version; it is the difference between a degraded app and an unusable one.

**Correction.** This entry originally said the identifying line was `TRAY: build failed:` in
`Lumen.log` in the legacy log directory. That file does not exist in a released build:
the logger was registered only under `cfg!(debug_assertions)`, so every `log::error!` in the
shipped binary went to a no-op sink. The instruction was wrong when it was written, and it
was also sent to the reporter of #5. Release logging is fixed in the next version; until
then the diagnostics that do work are in
[docs/troubleshooting-the-tray.md](docs/troubleshooting-the-tray.md).

The same report caught the cask uninstalling a LaunchAgent that does not exist: it
unloaded the app's bundle identifier, but `launchctl` matches on the agent's Label and the plugin
writes `Lumen.plist` with Label `Lumen`. Every uninstall left a login item behind, still
trying to launch a deleted app at each boot.

### The developer meter hook recorded nothing

`.claude/hooks/lumen_meter.sh` resolved its database as `<workspace>/lumen.db` — a file
with no schema — so every insert failed with "no such table" and `|| true` discarded the
error. It had also drifted to nine columns where the installed copy writes fifteen, losing
`token_source` (making a bytes/4 estimate indistinguishable from a real count), `req_key`,
`is_subagent`, `session_id` and `file_mtime`. Same shape as the intercept bug two releases
ago: two copies of one script with nothing asserting they agree. There is a test now.

### Notes

- A notifier for new releases, for minor and major versions only. Patch releases are
  silent: a notice that fires for every `x.y.Z` gets dismissed reflexively, and then the
  one that mattered gets dismissed too. **This adds the only unprompted network request
  Lumen makes** — an unauthenticated read of a public endpoint, no credential and no
  identifier — documented under Security & privacy and disabled by `LUMEN_UPDATE_CHECK=0`.
- Efficiency measurement on a real corpus: `cargo test --release -p lumen-mcp --test
  efficiency -- --nocapture`. Measured on this repository, an outline costs **6.4%** of a
  full read. The same harness reports where interception does *not* pay, and asserts it
  rather than omitting it.
- Print macros in the daemon could kill it. Its rule is "never `eprintln!` in this process"
  because the GUI owns that pipe, but the rule named only stderr and four `println!` calls
  survived it — one on the main thread during startup, where a panic ends the process.
- Four classes were styled nowhere at all, and `.mcp.json` declared no servers, so a fresh
  clone got none of the tools the Read intercept routes to.

## [1.5.0] — 2026-07-30

### The Read intercept could deadlock a session

The intercept blocked every Read of a ≥300-line source or log file and redirected the
model to the Lumen MCP tools — without ever checking that those tools could run. When the
MCP server was absent or its tools were denied, the model had no way to read the file at
all: the built-in Read was blocked and the replacement was unreachable. The documented
escape, `LUMEN_HOOK_ENABLED=0`, has to be set before Claude Code launches, so nothing
inside a running session could recover.

Observed in the field as an agent that retried the same routing about fifteen times and
then abandoned the task, reporting that it could not read the files it needed.

Two fail-open guards now make blocking conditional on the redirect being viable. If
`lumen-mcp` is not on the machine, the hook does not block. And a file is redirected at
most once per session — a model coming back to the built-in Read has already been told to
use Lumen, so if it is asking again the Lumen route failed for it, and the Read is
released. The block message now says so explicitly, which is what breaks the retry loop:
*"If the lumen tools are unavailable to you, retry this exact Read — it will be allowed
through."*

**The first version of this fix shipped to nobody.** It landed in the developer copy of
the hook, `.claude/hooks/lumen_read_intercept.sh`. Setup installs a different script,
generated from a string constant in `setup.rs`, and that one was untouched — so the
repository looked fixed while every real install kept the deadlock, and the staleness
check correctly reported no drift because the constant it compares against had not
changed. Found by installing the build rather than reading it. Six tests now cover the
shipped script, five of them by running it, and one compares guard markers between the
two copies.

Also fixed: `smart_read`'s tool description advertised "files ≥100 lines" while the hook
and `CLAUDE.md` both said 300. The model was reading one threshold and meeting another.

### `lumen report` — faults become a filed issue

A fired fail-open guard is the highest-signal fault Lumen has: routing degraded on a real
machine and nothing recorded it. `lumen report` now renders that and five other fault
kinds as a GitHub issue, and files it.

Writers append to a JSONL spool; nobody writes to SQLite. The intercept sits on the path
that decides whether the model may read a file, so a lock acquisition there is not
acceptable — `lumen report` does the database work later, draining the spool into a new
`faults` table. A drain renames before reading, so a hook writing mid-drain lands in the
fresh spool rather than being lost, and an interrupted drain is recovered next run.
Retry-loop callers are throttled: a permanently failed WS bind retries every two seconds
and would otherwise write about 43,000 lines a day.

Captured: both intercept guards, the daemon's three ingest-error paths (until now logged
and dropped — the silent loss that froze the gauge in 1.1.0), and its WS restart loop.
Ranked declines needed no writer; they were already in `read_events.routed_via`. Schema
drift is checked live. The daemon's supervisor-exit path is deliberately **not** captured:
it is documented as exit-first because logging before exit is what left an orphan daemon
holding the port in 1.2.2.

Filing tries three routes and takes the first that works — the GitHub CLI, then the REST
API with a token, then a prefilled browser form. The browser route is a *handoff, not a
filing*: nothing exists on the tracker until you press Submit, and both the CLI and the UI
say so rather than claiming an issue was created. Every skipped route is reported, because
a chain that silently degrades hides that the preferred one is broken. Reports are
deduplicated on a fingerprint of `(kind, variant, version)` carried in the body, so
re-running comments on the existing issue instead of opening a duplicate; the lookup works
without credentials on a public repository, since a token is only needed to write.

**The tracker is public and Lumen reads your source files, so redaction is the feature.**
Bodies are metadata-only: extension, line count, and a content hash that identifies a file
without shipping it. Paths outside the workspace lose even their basename — `acme_client_billing.ts`
identifies a client on its own. Path-valued environment overrides render as `<path>`,
because collapsing `$HOME` still leaves `clients/acme` visible.

That property did not hold on Windows. `Path::is_relative` answers for the host, and
`/Users/me/clients/acme` carries no drive letter, so a report rendered on Windows
classified every Unix-style absolute path as workspace-local and published it verbatim —
while the `$HOME` scrub made the output *look* sanitised. The same call appeared in three
places, so `--include-source` would also have embedded a file from outside the workspace.
Absoluteness is now decided from the string rather than the host. Caught by CI on
windows-latest, on a push made before tagging; a release cut an hour earlier would have
shipped a public-tracker leak to every Windows user.

### Reporting faults from the app

The report lives under **Report a fault** on the Hotspots screen, with a count badge on
the nav and a row in the tray popover — which hands off to the main window rather than
filing, because a 320×400 popover cannot show you the body first. Two clicks throughout:
one renders locally, the other publishes, and the text shown is the text sent.

Its first version was reachable only through a nav the tray popover does not have, and
below a ten-row list at that. Fixing the placement exposed three older layout bugs on that
screen: `.home`, the tab nav and the centred column were all styled only in the Home
component, so Hotspots painted no background at all — near-white text on a white window —
had no navigation styling, and sat flush against the window edge. The app shell is now
defined once, globally.

### `release.sh` has been failing halfway through since July

The script bumps `Casks/lumen.rb`. That file was renamed to `Casks/lumen-app.rb` on
28 July — to stop an unqualified `brew install --cask lumen` fetching an unrelated
brightness tool — and the script was not updated. The `sed` then failed under `set -e`
and the script aborted partway: after the Cargo manifests and the formula, before the
changelog, the commit or the tag. v1.3.0, v1.3.1 and v1.4.0 were cut some other way.

So the in-repo cask template drifted to `1.2.0`, and `crates/lumen-stats` — absent from
the script's list of crates entirely — was never bumped by any release and only stayed in
step when someone noticed by hand. Both are fixed, and `update_ruby_version` now refuses
a missing file or one with no version line instead of failing mid-bump. A run that does
most of its job and stops is how this went unnoticed.

**Correction.** An earlier draft of this entry, and the commit message of `30be13c`,
claimed the stale template meant `brew install --cask lumen-app` had been fetching
`Lumen_1.2.0_aarch64.dmg` for three releases. That is wrong, and no user was affected.
The tap is generated, not authored: CI copies these files into `HackPoint/homebrew-tap`
and then stamps the version from the resolved tag and the sha256 from the published
assets. The tap's own history shows a correct bump for every release — v1.2.4, v1.3.0,
v1.3.1, v1.4.0 — so the in-repo drift never reached anyone. The bug was real; the
consequence asserted was not.

### Notes

- Every fault kind is checked against the renderer by a test. The first report ever filed
  from the app read "Impact: Unclassified fault" because the daemon's kinds were added
  without teaching the renderer about them, while a kind that is never emitted had prose
  ready and waiting.
- `docs/filing-a-fault-report.md` walks through filing with screenshots of each state.
- `tauri build` does not rebuild sidecars. `build-sidecar.sh` must run first or the bundle
  ships whatever binaries were last staged — during this work it shipped a two-day-old
  MCP server alongside a current UI.

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

On this repository: the heaviest file, **139 reads, 3.83M tokens, 20.8% of everything read**, and
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

  Data in the legacy data directory is untouched.

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
  the legacy data directory under `~/.local/share/` (XDG) and Windows `%APPDATA%`.
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
  `bin/` in the legacy data directory and registered there.

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

