# Changelog

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

