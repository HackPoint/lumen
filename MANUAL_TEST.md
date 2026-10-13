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
- [ ] Shows `PreToolUse mcp__lumen__smart_read`, `mcp__lumen__recall_file` and `mcp__lumen__compress_logs` (the same hook, which tells the server whose call it is)
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

## TEST 6 — Conversation Identity (`/clear`, `/resume`, subagents)

The MCP server keeps running across `/clear` and `/resume` and cannot see either. The
PreToolUse hook on `smart_read`, `recall_file` and `compress_logs` is what tells it which
conversation, and which subagent, a call belongs to. This test runs that for real, on a
scratch Claude Code config (`CLAUDE_CONFIG_DIR`) and a scratch Lumen ledger and home, not
yours. It needs `claude`, `python3` and `sqlite3` on the PATH.

### 6.1 Scratch setup

Run the blocks of 6.1, 6.2 and 6.7 in one terminal (terminal 1): they share `SCRATCH` and
`LUMEN_MCP`. `LUMEN_MCP` is the binary under test, by default the one in the installed app.
To test a build from your checkout instead, export `LUMEN_MCP` as the full path of that
build's `lumen-mcp` before the block, for example the one in its `target/release`; the block
keeps a value that is already set.

```bash
export SCRATCH=/private/tmp/lumen_identity
export LUMEN_MCP="${LUMEN_MCP:-/Applications/Lumen.app/Contents/MacOS/lumen-mcp}"
rm -rf "${SCRATCH:?}"; mkdir -p "$SCRATCH/cfg" "$SCRATCH/home" "$SCRATCH/work"
```

The `SCRATCH` path is fixed on purpose: the outline repeats it, so the token counts below
hold only for this one. A source file past the intercept threshold, generated so the numbers
stay put:

```bash
python3 - <<'PY'
import os
lines = ["use std::collections::HashMap;", ""]
for i in range(1, 13):
    lines += [f"/// Step {i} of the pipeline.", f"pub fn step_{i}(input: &HashMap<String, u64>) -> u64 {{", "    let mut total = 0;"]
    lines += [f'    total += input.get("key_{i}_{j}").copied().unwrap_or({i * j});' for j in range(1, 21)]
    lines += ["    total", "}", ""]
open(os.environ["SCRATCH"] + "/work/sample.rs", "w").write("\n".join(lines))
PY
wc -l "$SCRATCH/work/sample.rs"
```

The MCP config and the hook settings, both pointing at the scratch ledger and a scratch home.
The installed hook is a small script that ends in `lumen-mcp hook intercept`; here the binary
is called directly, on the same three tools Setup registers it for:

```bash
python3 - <<'PY'
import json, os
s, mcp = os.environ["SCRATCH"], os.environ["LUMEN_MCP"]
where = f"HOME={s}/home LUMEN_DB={s}/lumen.db"
server = {"type": "stdio", "command": mcp, "args": [], "env": {"HOME": f"{s}/home", "LUMEN_DB": f"{s}/lumen.db"}}
json.dump({"mcpServers": {"lumen": server}}, open(f"{s}/mcp.json", "w"), indent=2)
hook = {"hooks": [{"type": "command", "command": f"{where} {mcp} hook intercept"}]}
tools = ["mcp__lumen__smart_read", "mcp__lumen__recall_file", "mcp__lumen__compress_logs"]
json.dump({"hooks": {"PreToolUse": [{"matcher": t, **hook} for t in tools]}}, open(f"{s}/settings.json", "w"), indent=2)
PY
```

- [ ] `wc -l` printed 313
- [ ] `test -x "$LUMEN_MCP" && echo ok` printed `ok`: the binary exists and is executable
- [ ] `$SCRATCH/settings.json` has three `PreToolUse` entries, one per Lumen tool

### 6.2 Start Claude Code on the scratch config

In terminal 1:

```bash
cd "$SCRATCH/work"
CLAUDE_CONFIG_DIR="$SCRATCH/cfg" claude --mcp-config "$SCRATCH/mcp.json" --strict-mcp-config \
  --settings "$SCRATCH/settings.json" --setting-sources project \
  --allowedTools "mcp__lumen__smart_read mcp__lumen__recall_file mcp__lumen__compress_logs Agent" \
  --model haiku
```

- [ ] The config is new, so Claude Code asks its first-run questions: Enter for the theme,
      Enter past the security notes, then arrow down to "Yes, I trust this folder" (the
      default is "No, exit") and Enter. Sign in if it asks, or have your usual API variables
      exported
- [ ] `/mcp` lists `lumen` with a check mark and `4 tools`; Esc closes it

Stay in this one session for 6.3 to 6.6. A second terminal (terminal 2) does not have
`SCRATCH`: export it there once, then run the `sqlite3` line after every step to see
what the server recorded for each read:

```bash
export SCRATCH=/private/tmp/lumen_identity
sqlite3 -header "$SCRATCH/lumen.db" "select rowid as n, substr(session_id,1,8) as session, substr(agent_id,1,8) as agent, repeat_plan as plan, tokens_returned as returned, full_tokens as full from read_events order by rowid"
```

The prompt for a read, with `WORD` set to ALPHA in the first conversation and BRAVO in the
second, so the transcript tells you which one you are in:

> Call the lumen smart_read tool with path exactly /private/tmp/lumen_identity/work/sample.rs, then reply with only the word WORD.

### 6.3 One conversation: full outline, then the stub

- [ ] Send the prompt with `WORD` = ALPHA
- [ ] **Expected**: row 1: `plan` = `first`, `returned` = 282, `full` = 5263; `session` is set
      and `agent` is empty. Ctrl+O shows what the model received, an outline that starts
      `# 313 lines | 5263 full tokens | 13 items` and lists `function step_1 L4-27` and the
      rest. Ctrl+O again goes back
- [ ] Send the same prompt again
- [ ] **Expected**: row 2: the same `session`, `plan` = `unchanged`, `returned` = 30. Ctrl+O
      shows "Unchanged since an earlier read of this file in this session. If that read is
      not in your context, call smart_read(path, fresh=true)."

### 6.4 `/clear`

- [ ] Type `/clear`, then send the prompt with `WORD` = BRAVO
- [ ] **Expected**: row 3: a NEW `session`, `plan` = `first`, `returned` = 282: the outline
      in full, not the stub

### 6.5 `/resume`

- [ ] Type `/resume`. The picker lists the folder's other conversations, not the one you
      are in, so the earlier one is the only entry (Claude Code titles it; here "Lumen
      smart_read sample.rs"). Press Enter
- [ ] The transcript shows the two ALPHA reads. Send the prompt with `WORD` = ALPHA
- [ ] **Expected**: row 4: the `session` of rows 1 and 2, `plan` = `first`, `returned` = 282:
      the outline in full. The server gave up what it remembered when the conversation
      changed, so it does not claim that the resumed context still holds the outline
- [ ] Send the same prompt again
- [ ] **Expected**: row 5: `plan` = `unchanged`, `returned` = 30

### 6.6 A subagent

The subagent shares its parent's `session`, has an `agent` of its own, and starts with an
empty context. Send:

> Use the Agent tool to start one general-purpose subagent. Tell it to call the lumen smart_read tool with path exactly /private/tmp/lumen_identity/work/sample.rs and reply with only the word DONE. When it has finished, call that smart_read tool yourself with the same path, then reply with only the word DONE.

- [ ] If Claude Code shows a notice about auto mode when the subagent starts, press Enter.
      The subagent runs in the background and the parent waits for it, so row 6 comes first
- [ ] **Expected**: row 6 is the subagent's: the `session` of row 5, an `agent` set to the
      subagent's own id (`<agent-id>`, eight characters), `plan` = `first`, `returned` =
      282. It got the outline although its parent had read the file
- [ ] **Expected**: row 7 is the parent's: the same `session`, an empty `agent`,
      `plan` = `unchanged`, `returned` = 30. The subagent's read did not use up the parent's

### 6.7 Control: no hook

The same read with the hook not registered shows what a failure looks like. Leave Claude
Code with `/exit`, then run this in terminal 1:

```bash
cd "$SCRATCH/work"
echo '{"hooks":{}}' > "$SCRATCH/settings_nohook.json"
CLAUDE_CONFIG_DIR="$SCRATCH/cfg" claude -p "Call the lumen smart_read tool with path exactly /private/tmp/lumen_identity/work/sample.rs, then reply with only the word DONE." \
  --mcp-config "$SCRATCH/mcp.json" --strict-mcp-config --settings "$SCRATCH/settings_nohook.json" \
  --setting-sources project --allowedTools "mcp__lumen__smart_read" --model haiku
```

- [ ] **Expected**: row 8: `session` and `agent` empty, `plan` = `unidentified`, `returned` =
      282. A call the server cannot place is answered in full and remembers nothing

### 6.8 What a failure looks like, and clean-up

- [ ] A stub (`returned` = 30) as the first read after `/clear` or `/resume`, or for the
      subagent: the server answered for a conversation that was not asking. FAIL
- [ ] `plan` = `unidentified` or an empty `session` in rows 1 to 7 (row 8 is that case on
      purpose): the hook did not run or its record was lost. FAIL
- [ ] In terminal 1, `find "$SCRATCH" -maxdepth 1 -name 'lumen_call_*.json'` prints nothing:
      every record was claimed by its call
- [ ] In terminal 1, `rm -rf "${SCRATCH:?}"`

---

## Checklist summary

| # | Test | Status |
|---|------|--------|
| 1 | Clean install — "damaged" dialog, setup screen, all ✓ | [ ] PASS / [ ] FAIL |
| 2 | Optimizer — hook intercepts, Optimizer tab shows saving | [ ] PASS / [ ] FAIL |
| 3 | Monitoring — gauge moves, cost tiles update | [ ] PASS / [ ] FAIL |
| 4 | Soak 3–4h — max(ts) advances, daemon stays alive | [ ] PASS / [ ] FAIL |
| 5 | Uninstall — lumen gone, unrelated preserved, re-install works | [ ] PASS / [ ] FAIL |
| 6 | Identity — full after `/clear` and `/resume`, stub on a repeat, subagent told apart | [ ] PASS / [ ] FAIL |

**Do not publish the release until TEST 4 (soak) is complete.**
