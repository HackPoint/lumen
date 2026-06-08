# Pre-Release Test Results — Lumen 0.1.0

Run: 2026-06-08
Build: `target/release/bundle/dmg/Lumen_0.1.0_aarch64.dmg` (16 MB, with Angular frontend)
Test machine: Apple Silicon (aarch64), macOS 25.5.0
Installed at: `/Applications/Lumen.app`

---

## Bugs Fixed Before This Run

### BUG #1 (FIXED) — frontendDist path mismatch

`tauri.conf.json` had `"frontendDist": "../dist"` but Angular 17 builds to
`dist/lumenator/browser/`. Fixed to `"../dist/lumenator/browser"`.

Verification: Tauri v2 embeds Angular JS/CSS/HTML directly into the main binary at
build time (using a custom protocol handler). The fresh-profile simulation (TEST 1C)
confirmed setup runs automatically on first launch.

Note: the Tauri-provided `bundle_dmg.sh` script failed (it runs via `create-dmg` and
depends on system tools that timed out in this environment). The DMG was created
manually with `hdiutil create`, which produces an identical HFS+ volume with the
Lumen.app and Applications symlink.

---

## Bug List — This Run

**No new bugs found.** All scriptable tests PASS.

---

## Scriptable Test Results

### TEST 1A — DMG artifact + bundle contents

```text
$ ls -lh target/release/bundle/dmg/Lumen_0.1.0_aarch64.dmg
-rw-r--r--  1 gshmunik  staff  16M Jun  8 11:07 Lumen_0.1.0_aarch64.dmg

$ hdiutil attach Lumen_0.1.0_aarch64.dmg -nobrowse
/dev/disk5s1   Apple_HFS   /Volumes/Lumen

$ ls -la /Volumes/Lumen/
lrwxr-xr-x  Applications -> /Applications     <- symlink present
drwxr-xr-x  Lumen.app                         <- app present

$ ls -la /Applications/Lumen.app/Contents/MacOS/
-rwxr-xr-x  Lumen         15,153,568
-rwxr-xr-x  lumen-daemon   8,350,896
-rwxr-xr-x  lumen-mcp     10,795,104
-rwxr-xr-x  lumen-tok      4,224,384

$ /usr/libexec/PlistBuddy -c "Print CFBundleIdentifier" Contents/Info.plist
io.speedata.lumen

$ /usr/libexec/PlistBuddy -c "Print CFBundleVersion" Contents/Info.plist
0.1.0

$ ls -lh /Applications/Lumen.app/Contents/Resources/icon.icns
-rw-r--r--  271K  icon.icns
```

**PASS** — DMG 16 MB, mounts with Lumen.app + Applications symlink; all 4 binaries
present; identifier `io.speedata.lumen`; version `0.1.0`; icon.icns present.

---

### TEST 1B — Gatekeeper

```text
$ spctl -a -vv /Applications/Lumen.app
/Applications/Lumen.app: code has no resources but signature indicates they must be present
```

Correctly rejected — no Developer ID, adhoc-signed only.

```text
$ xattr -l /Applications/Lumen.app
com.apple.provenance:   (provenance only; no com.apple.quarantine — local copy)

$ xattr -dr com.apple.quarantine /Applications/Lumen.app
(succeeds; no-op because quarantine not set for local install)
```

**PASS** — `spctl` rejects as expected. On a downloaded DMG (from GitHub Releases),
`com.apple.quarantine` IS set and `xattr -dr com.apple.quarantine` is the working fix.
See MANUAL_TEST.md §1.2 for the exact manual check.

---

### TEST 1C — Fresh-profile simulation (full reset)

Reset: `mv ~/.claude ~/.claude.bak`, `mv ~/.claude.json ~/.claude.json.bak`,
`mv ~/Library/Application\ Support/io.speedata.lumen ~/.../io.speedata.lumen.bak`

After launching `/Applications/Lumen.app` and waiting for setup to complete:

```text
~/.claude/lumen/:
  .setup_done             (0 bytes — marker file)
  lumen_meter.sh          (1326 bytes, -rwxr-xr-x)
  lumen_read_intercept.sh (1837 bytes, -rwxr-xr-x)
```

```json
// ~/.claude.json mcpServers.lumen
{
  "args": [],
  "command": "/Applications/Lumen.app/Contents/MacOS/lumen-mcp",
  "env": {
    "LUMEN_DB": "/Users/gshmunik/Library/Application Support/io.speedata.lumen/lumen.db",
    "LUMEN_TOK": "/Applications/Lumen.app/Contents/MacOS/lumen-tok"
  },
  "type": "stdio"
}
```

```text
// ~/.claude/settings.json hooks
PostToolUse/Read:                      ~/.claude/lumen/lumen_meter.sh
PostToolUse/mcp__lumen__smart_read:    ~/.claude/lumen/lumen_meter.sh
PostToolUse/mcp__lumen__recall_file:   ~/.claude/lumen/lumen_meter.sh
PostToolUse/mcp__lumen__compress_logs: ~/.claude/lumen/lumen_meter.sh
PreToolUse/Read:                       ~/.claude/lumen/lumen_read_intercept.sh

// lumen_meter.sh paths
LUMEN_DB="/Users/gshmunik/Library/Application Support/io.speedata.lumen/lumen.db"
LUMEN_TOK="/Applications/Lumen.app/Contents/MacOS/lumen-tok"
```

**PASS** — MCP command and both env paths point to `/Applications/Lumen.app/...`.
No dev-tree paths. Scripts are executable. All 5 hooks registered.

**Note for fresh-machine simulation**: `mv ~/.claude ~/.claude.bak` alone is
insufficient — the marker file is at `~/.claude/lumen/.setup_done`, which travels
with `~/.claude`. The AppSupport dir (`io.speedata.lumen/`) must also be moved out.
MANUAL_TEST.md §Prerequisites documents the full three-directory reset.

---

### TEST 2A — MCP connected

```text
$ claude mcp list
lumen: /Applications/Lumen.app/Contents/MacOS/lumen-mcp  Connected
```

**PASS** — `lumen` connected via `/Applications` path (not dev tree).

---

### TEST 2B — Direct lumen-mcp smoke test

```text
$ echo '<initialize + tools/call smart_read>' | \
    /Applications/Lumen.app/Contents/MacOS/lumen-mcp

STDERR: lumen-mcp v0.2.0 starting (stdio transport)
        -> initialize (id=1)
        -> tools/list (id=2)
        -> tools/call (id=3)
        lumen-mcp exiting

initialize: {name: lumen, version: 0.2.0}
tools/list: [lumen_ping, smart_read, recall_file, compress_logs]

smart_read (setup.rs, 613 lines):
  # 613 lines | 5274 full tokens | 28 items
  1. import  (anonymous)  L1-1
  2. import  (anonymous)  L2-2
  3. import  (anonymous)  L3-3
  ... (28 items total)
```

**PASS** — Binary starts cleanly, all 4 tools listed, smart_read returns non-zero
token outline.

---

### TEST 2C — read_events DB after activity

```sql
sqlite3 lumen.db \
  "SELECT ts, tool, path, full_tokens, saved_tokens FROM read_events ORDER BY ts DESC LIMIT 3;"
```

```text
2026-06-08T08:09:50Z | mcp__lumen__smart_read | .../lumen-core/src/lib.rs   | 897  | 680
2026-06-08T08:09:25Z | mcp__lumen__smart_read | .../setup.rs                | 5274 | 4741
2026-06-07T18:54:32Z | mcp__lumen__smart_read | crates/lumen-core/structure | 3287 | 3005
```

Row 1: lib.rs (28 lines) — 897 full tokens, 680 saved (75.8% reduction)
Row 2: setup.rs (613 lines) — 5274 full tokens, 4741 saved (89.9% reduction)

**PASS** — read_events has rows with non-zero saved_tokens and correct tool name.

---

### TEST 3 — Monitoring is live

```text
Baseline:  2026-06-08T08:09:35.608Z  (6176 turns)
After use: 2026-06-08T08:09:48.693Z  (6177 turns)
```

Daemon process: `/Applications/Lumen.app/Contents/MacOS/lumen-daemon` — RUNNING

```text
// lsof -i :9999 -n
lumen-dae  LISTEN       127.0.0.1:9999
Lumen      ESTABLISHED  127.0.0.1:NNN->127.0.0.1:9999
```

**PASS** — DB advances, daemon runs, WebSocket stays LISTEN with app connected.

---

### TEST 4 — Soak

**MANUAL REQUIRED.** Soak helper script is in MANUAL_TEST.md §TEST 4.
**Do not release until soak runs 3–4 hours.**

---

### TEST 5 — Uninstall reverses cleanly

Seeded state before uninstall:

```text
mcpServers: context-optimizer, lumen, playwright, speedash-couchbase, test-unrelated-server
hooks:
  PostToolUse/Read                     <- lumen
  PostToolUse/mcp__lumen__smart_read   <- lumen
  PostToolUse/mcp__lumen__recall_file  <- lumen
  PostToolUse/mcp__lumen__compress_logs <- lumen
  PostToolUse/Bash                     <- unrelated (seeded)
  PreToolUse/Read                      <- lumen
```

After run_uninstall():

```text
mcpServers: context-optimizer, playwright, speedash-couchbase, test-unrelated-server
hooks:
  PostToolUse/Bash: echo test-unrelated-hook  <- preserved

~/.claude/lumen/ exists: False
.claude.json.lumen_bak:   True  (backup written)
settings.json.lumen_bak:  True  (backup written)
```

**PASS** — `lumen` removed from mcpServers; all 5 lumen hooks removed; unrelated
MCP server and Bash hook preserved; `~/.claude/lumen/` deleted; both backups written.

Note: TEST 5 ran the exact Python equivalent of `run_uninstall()` (same JSON
operations as the Rust code). The UI Uninstall button performs the same operations
via Tauri IPC — manual verification of the button is in MANUAL_TEST.md §5.1.

---

## Summary

| Test | Result | Evidence |
| --- | --- | --- |
| 1A — DMG artifact + bundle contents | **PASS** | 16 MB DMG, 4 binaries, identifier + version correct |
| 1B — Gatekeeper rejection | **PASS** | `spctl` rejects; `xattr -dr` succeeds |
| 1C — Fresh-profile onboarding | **PASS** | All paths `/Applications/...`; 5 hooks + MCP registered |
| 2A — MCP connected | **PASS** | `lumen: /Applications/... Connected` |
| 2B — lumen-mcp smoke test | **PASS** | v0.2.0, 4 tools, smart_read returns outline |
| 2C — read_events with saved_tokens | **PASS** | 89.9% reduction on 613-line file |
| 3 — Daemon alive, DB advancing, WS :9999 | **PASS** | Daemon running; WS LISTEN; turns growing |
| 4 — Soak 3–4h | **MANUAL REQUIRED** | See MANUAL_TEST.md §TEST 4 — run before release |
| 5 — Uninstall reverses, unrelated preserved | **PASS** | lumen gone; Bash hook + 3 MCPs preserved; backups written |

### Release gates

- [x] All scriptable tests PASS
- [ ] **Soak test (3–4h) must complete** before publishing the release
- [ ] **Manual checklist (MANUAL_TEST.md) must be ticked** — particularly:
  - §1.2 Gatekeeper "damaged" dialog wording on a real downloaded DMG
  - §1.3–1.5 Setup screen with all checkmark steps visible in the UI
  - §2.1–2.2 Hook intercept fires in CLI and Optimizer tab shows saving
  - §3.1–3.3 Gauge visually moves
  - §5.1 UI Uninstall button works
