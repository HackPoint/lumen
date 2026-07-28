# Changelog

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

