# Changelog

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

