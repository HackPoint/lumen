# Lumen Pre-Release Manual Test Plan

Run this before every public release. Tick each box as you go.

Automated results (TEST 1–5 scriptable parts) are in the `PRE_RELEASE_TEST_RESULTS.md`
alongside this file.

---

## Prerequisites

- Freshly built DMG at `target/release/bundle/dmg/Lumen_<ver>_aarch64.dmg`
- No prior Lumen install: `/Applications/Lumen.app` absent; `~/.claude.json`
  contains no `lumen` key; `~/.claude/lumen/` absent.
- Fresh-machine simulation requires removing ALL prior state:
  ```bash
  mv ~/.claude ~/.claude.bak
  mv ~/.claude.json ~/.claude.json.bak
  rm -rf ~/Library/Application\ Support/io.speedata.lumen/
  ```

---

## TEST 1 — Clean Install

### 1.1 DMG opens correctly

- [ ] Double-click the `.dmg` file
- [ ] Finder window appears with the Lumen icon and an Applications alias
- [ ] Drag Lumen into Applications

### 1.2 Gatekeeper — "damaged" dialog (macOS 13+)

- [ ] Double-click `/Applications/Lumen.app`
- [ ] **Expected**: macOS shows the alert:
  > "Lumen.app" is damaged and can't be opened. You should move it to the Trash.
- [ ] **Record the exact wording** (it varies by macOS version): _______________
- [ ] Do NOT click Move to Trash
- [ ] In Terminal run:
  ```bash
  sudo xattr -dr com.apple.quarantine /Applications/Lumen.app
  ```
- [ ] Double-click Lumen again — it opens normally

### 1.3 Setup screen appears on first launch

- [ ] App window appears (not blank, not a spinning wheel)
- [ ] Setup screen shows automatically (you are NOT on the main gauge screen)
- [ ] Four step rows are visible: "Detect Claude Code", "Install hook scripts",
      "Register MCP server", "Install hooks"

### 1.4 Setup completes successfully

- [ ] All four rows show ✓ (green checkmark)
  - If any row shows ⚠ Warn, note it: _______________
  - If any row shows ✗ Error, note it: _______________ (stop, file a bug)
- [ ] "Open Lumen" button is visible and clickable
- [ ] Clicking "Open Lumen" navigates to the main gauge screen

### 1.5 Verify what setup wrote

After setup completes (before clicking Open Lumen or in a second Terminal):

```bash
# MCP server registered
python3 -c "import json; d=json.load(open('/Users/$USER/.claude.json')); print(json.dumps(d['mcpServers']['lumen'], indent=2))"
```

- [ ] Output shows `command` pointing to `/Applications/Lumen.app/Contents/MacOS/lumen-mcp`
- [ ] Output shows `LUMEN_DB` env var pointing to `~/Library/Application Support/io.speedata.lumen/lumen.db`

```bash
# Hook scripts present and executable
ls -la ~/.claude/lumen/
```

- [ ] `lumen_meter.sh` present, permissions `-rwxr-xr-x`
- [ ] `lumen_read_intercept.sh` present, permissions `-rwxr-xr-x`
- [ ] `.setup_done` marker file present

```bash
# Hooks registered
python3 -c "import json; d=json.load(open('/Users/$USER/.claude/settings.json')); [print(p, e.get('matcher')) for p,arr in d.get('hooks',{}).items() for e in arr]"
```

- [ ] Shows `PreToolUse Read` (the intercept hook)
- [ ] Shows `PostToolUse Read` (the meter hook)

### 1.6 Restart Claude Code after setup

- [ ] Restart Claude Code (CLI or VS Code extension)
- [ ] `claude mcp list` shows `lumen: /Applications/Lumen.app/... ✓ Connected`

---

## TEST 2 — Optimizer Connects and Saves

### 2.1 Hook intercepts a large-file Read (CLI only)

In a Claude Code CLI session:

- [ ] Open a project that has at least one source file ≥ 300 lines
- [ ] Ask: "What does `<large_file.rs>` do?"
- [ ] **Expected**: Before Claude reads the file, the hook fires and Claude's
      response includes text like:
  ```
  Lumen intercept: <file> is NNN lines.
  Instead of reading the full file, call:
    1. lumen:smart_read(path="<file>")  → structural outline...
  ```
- [ ] Claude then calls `smart_read` (visible in tool use)
- [ ] No raw `Read` on the large file follows (only `smart_read`)

### 2.2 Optimizer screen shows the saving

- [ ] In Lumen, click the Optimizer tab
- [ ] A row appears for the file just read
- [ ] "Lumen optimized" total is non-zero
- [ ] Effectiveness % is displayed (expected: 70–95%)
- [ ] "Saved by caching" section is separately visible (NOT added to Lumen's total)

---

## TEST 3 — Monitoring is Live

### 3.1 Gauge moves

- [ ] Do a Claude Code task (ask a question, run a tool)
- [ ] The context fill gauge in Lumen (ring or firefly) increases
- [ ] The gauge does NOT stay frozen at the same value

### 3.2 Cost tiles update

- [ ] After the task, the session cost tile shows a non-zero amount
- [ ] Input and output token counts are non-zero

### 3.3 Session switches

- [ ] Start a new Claude Code session (`claude` in a different directory)
- [ ] The session label in Lumen updates to the new session within ~30 seconds

---

## TEST 4 — Soak (Durability — Run Before Public Release)

**This test requires 3–4 hours of normal work.** Run the helper script below in a
separate terminal, do your normal Claude Code work, and check back periodically.

### Soak helper script

Save this as `~/lumen-soak.sh` and run `bash ~/lumen-soak.sh` in a separate terminal:

```bash
#!/bin/bash
# Lumen soak monitor — run during 3-4h normal work session
# Prints a status line every 5 minutes.

DB="$HOME/Library/Application Support/io.speedata.lumen/lumen.db"

check() {
    local now
    now=$(date -u +"%Y-%m-%dT%H:%M:%SZ")
    local max_ts
    max_ts=$(sqlite3 "$DB" "SELECT MAX(ts) FROM turns;" 2>/dev/null || echo "DB_ERR")
    local daemon_pid
    daemon_pid=$(pgrep -f "lumen-daemon" | head -1)
    local ws_listen
    ws_listen=$(lsof -i :9999 -n 2>/dev/null | grep LISTEN | awk '{print $2}' | head -1)

    echo "[$now]"
    echo "  max(turns.ts): $max_ts"
    echo "  lumen-daemon pid: ${daemon_pid:-NOT RUNNING}"
    echo "  ws :9999 listen pid: ${ws_listen:-NOT LISTENING}"

    # Warn if max_ts is stale (> 10 min behind now) AND JSONL is growing
    if [[ "$max_ts" != "DB_ERR" ]]; then
        local staleness
        staleness=$(python3 -c "
import datetime, sys
try:
    ts = datetime.datetime.fromisoformat('$max_ts'.replace('Z','+00:00'))
    now = datetime.datetime.now(datetime.timezone.utc)
    diff = (now - ts).total_seconds()
    print(int(diff))
except:
    print(-1)
" 2>/dev/null)
        if [[ "$staleness" -gt 600 ]]; then
            echo "  *** WARNING: max(ts) is ${staleness}s behind now — possible daemon death ***"
        else
            echo "  lag: ${staleness}s  [OK]"
        fi
    fi
    echo ""
}

echo "=== Lumen soak monitor started ==="
echo "=== Let this run for 3-4 hours while you work normally in Claude Code ==="
echo ""

while true; do
    check
    sleep 300   # check every 5 minutes
done
```

### Soak pass criteria

After 3–4 hours of normal work:

- [ ] `max(turns.ts)` advances throughout — the lag from `now` stays under 10 minutes
      (it will be slightly behind because the daemon processes JSONL after Claude writes it)
- [ ] `lumen-daemon` PID stays constant (not restarting repeatedly)
- [ ] WebSocket :9999 stays `LISTEN` the whole time
- [ ] No "WARNING: max(ts) is Ns behind" lines appear (or only brief ones)

### Failure signature to watch for

If `max(ts)` stops advancing while the JSONL session files in `~/.claude/projects/`
continue to grow in size, the daemon has silently died. This is the most dangerous
regression. It causes the gauge to freeze at a stale value without any error message.

Expected visible symptom: gauge stuck, cost tiles not updating, no new turns in DB
despite active Claude Code usage.

---

## TEST 5 — Uninstall Reverses Cleanly

### 5.1 Uninstall via the Setup screen

- [ ] In Lumen, click "Uninstall" (on the Setup screen)
- [ ] Three step rows appear: "Remove MCP entry", "Remove hooks", "Remove scripts"
- [ ] All three show ✓

### 5.2 Verify reversal

```bash
# lumen MCP entry gone
python3 -c "import json; d=json.load(open('/Users/$USER/.claude.json')); print('lumen' not in d.get('mcpServers',{}))"
# expected: True

# lumen hooks gone, unrelated hooks still present
python3 -c "import json; d=json.load(open('/Users/$USER/.claude/settings.json')); [print(p, e.get('matcher')) for p,arr in d.get('hooks',{}).items() for e in arr]"
# expected: any non-lumen hooks still appear; no Read hooks from Lumen

# lumen dir deleted
ls ~/.claude/lumen/ 2>/dev/null || echo "GONE (correct)"

# backup written
ls ~/.claude.json.lumen_bak && echo "backup present"
ls ~/.claude/settings.json.lumen_bak && echo "backup present"
```

- [ ] `lumen` not in mcpServers: True
- [ ] No `Read` hooks from Lumen remain
- [ ] Any other MCP servers and hooks you had before are still present
- [ ] `~/.claude/lumen/` is gone
- [ ] Both `.lumen_bak` files exist (safe to delete after verification)

### 5.3 Re-install works after uninstall

- [ ] Restart Lumen
- [ ] Setup screen appears again (marker deleted by uninstall)
- [ ] Setup completes successfully again
- [ ] `claude mcp list` shows lumen connected

---

## Checklist summary

| # | Test | Status |
|---|------|--------|
| 1 | Clean install — "damaged" dialog, setup screen, all ✓ | [ ] PASS / [ ] FAIL |
| 2 | Optimizer — hook intercepts, Optimizer tab shows saving | [ ] PASS / [ ] FAIL |
| 3 | Monitoring — gauge moves, cost tiles update | [ ] PASS / [ ] FAIL |
| 4 | Soak 3–4h — max(ts) advances, daemon stays alive | [ ] PASS / [ ] FAIL |
| 5 | Uninstall — lumen gone, unrelated preserved, re-install works | [ ] PASS / [ ] FAIL |

**Do not publish the release until TEST 4 (soak) is complete.**
