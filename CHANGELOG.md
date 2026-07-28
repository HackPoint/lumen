# Changelog

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

