# Lumen

![Lumen firefly mascot](docs/assets/firefly.svg)

See the whole truth about your Claude Code tokens — context fill, real cost, and verifiable optimization.

[![Download](https://img.shields.io/github/v/release/speedata/lumen?label=download&color=3fb950)](../../releases/latest)
![Platform](https://img.shields.io/badge/macOS-Apple%20Silicon-lightgrey)
![License](https://img.shields.io/badge/license-MIT-blue)

---

> **Screenshot needed** — `docs/assets/screenshot-gauge.png`
> Capture: the Lumen main window showing the ring gauge, cost tiles, and caching savings strip.

---

## What it shows

Lumen is a macOS menu-bar app and terminal dashboard for [Claude Code](https://claude.ai/code) users.
It watches your session files locally and surfaces things Claude Code doesn't show in its own interface:

| Signal | Where it appears |
| --- | --- |
| **Context fill** — tokens used / window size, live | Menu-bar icon · tray popover · main window gauge |
| **Compaction warning** — amber at 80%, red at 95% | Tray popover · OS notification |
| **Session cost** — input + output + cache tokens, priced | Main window cost tiles |
| **Caching savings** — what Claude Code's cache actually saved | Optimizer screen (labeled "reported by Claude Code") |
| **Optimizer effectiveness** — % fewer tokens per intercepted read | Optimizer screen hero metric |

None of this leaves your machine. No account, no telemetry. See [Security & privacy](#security--privacy).

---

## Platform support

**macOS (Apple Silicon) today.**

Lumen is built and tested on aarch64 macOS only. Intel Macs, Windows, and Linux are
planned but not yet available. There are no install instructions for those platforms
because there is nothing to install yet — an honest "not yet" beats a fake guide.

**Requirements:** macOS 13 Ventura or later · Apple Silicon (aarch64) · [Claude Code](https://claude.ai/code) installed

---

## Install the menu-bar app (.dmg)

1. Download **`Lumen_0.1.0_aarch64.dmg`** from the [Releases page](../../releases/latest)
2. Open the .dmg and drag **Lumen** to your Applications folder
3. Read the Gatekeeper note below before you try to open it

---

## ⚠️ Gatekeeper — read this before opening

macOS will say **"Lumen is damaged and can't be opened."**

This is expected. The build is unsigned (no Apple Developer ID yet — notarization is on
the roadmap). It is not malware. Here is how to open it:

### Terminal (most reliable)

```bash
sudo xattr -dr com.apple.quarantine /Applications/Lumen.app
```

Then double-click Lumen normally.

### System Settings

Try to open Lumen once (it will be blocked). Then open **System Settings → Privacy & Security**,
scroll to the bottom, and click **"Open Anyway"** next to the Lumen entry.

> Right-click → Open does not bypass quarantine for adhoc-signed apps on macOS 13+.
> Use one of the two options above.

---

## How to run the menu-bar app

> **Screenshot needed** — `docs/assets/screenshot-setup.png`
> Capture: the Setup screen showing the four step rows with checkmarks.

### First launch — setup

The first time Lumen launches, a **Setup** screen appears automatically. It does three things:

1. Writes hook scripts to `~/.claude/lumen/` — a read-intercept script and a token meter
2. Registers the `lumen` MCP server globally in `~/.claude.json`
3. Merges Lumen's hooks into `~/.claude/settings.json`

Setup is **non-destructive**: it merges Lumen's entries alongside any existing MCP servers
and hooks you have. Nothing existing is removed or overwritten.

**Uninstall** (also on the Setup screen) reverses all three steps cleanly.

After setup, **restart Claude Code** for the MCP server and hooks to activate.

### Opening the main window

Lumen lives in the **menu bar** (top-right of your screen), not in the Dock.

| Action | Result |
| --- | --- |
| **Left-click** the tray icon | Quick popover — context gauge, current cost, mode badge |
| **Right-click** → "Open Lumen" | Full window with **Context** and **Optimizer** tabs |

The tray icon pulses with ring animations; its color reflects context fill status
(green → amber at 80% → red at 95%).

---

## How to run the CLI (`lumen`)

The `lumen` terminal command is a **live dashboard** — the same data as the GUI,
rendered in your terminal. It is also the path to **Full mode** optimization:
hooks fire only in the Claude Code CLI, not the VS Code extension.

### Install

**Via Homebrew (recommended):**

```bash
brew tap speedata/tap && brew install lumen
```

**Via the app:**

Open Lumen → click **"Install CLI"** — this symlinks the bundled binary into your PATH.

### Run

```bash
lumen          # live terminal dashboard
# Press q or Ctrl-C to quit
```

The dashboard reads from the same local SQLite database as the GUI. It works best
with the Lumen daemon running (the GUI starts the daemon automatically); if the daemon
is not running, the CLI falls back to polling the database directly.

> **Note:** The `lumen` CLI is a monitoring and dashboard tool, not a replacement for
> Claude Code. Keep Claude Code running normally — `lumen` watches it.

---

## What the numbers mean

> **Screenshot needed** — `docs/assets/screenshot-numbers.png`
> Capture: the main window with the context tab visible and all tiles labeled.

Every number in Lumen comes from your local session files or is computed locally.
Here is what each one means, where it comes from, and any honesty caveat.

### Context tab

#### Context gauge (the ring)

The large ring shows **how full your current context window is**. The fill is calculated
from the most recent turn's `cache_read` token count divided by the inferred window size.

| Color | Meaning |
| --- | --- |
| Green | Below 80% — plenty of room |
| Amber | 80–95% — compaction is approaching |
| Red | Above 95% — compaction is imminent; Claude Code will soon summarize prior context |

**Honesty caveat:** the window size is *inferred* from the model name (200K for most
models, 500K or 1M for models that support it). Lumen cannot read your actual plan
tier — your real limit may differ. The inferred tiers are listed in
[Known limitations](#known-limitations).

#### "X / Y tokens"

**X** — tokens currently filling the window (from the latest turn's token counts).  
**Y** — the inferred window size for your current model.

These are the raw numbers behind the gauge ring.

#### Model name

The model identifier from the **most recent turn** in the active session (e.g.,
`claude-sonnet-4-6`, `claude-opus-4-8`). Lumen reads this from the JSONL session
file — it reflects what Claude Code is actually using, not a preference or setting.

#### Session cost

The running **dollar total for the active session**, computed locally from:

- Output tokens × output price
- Fresh input tokens × input price
- Cache-read tokens × cache-read price
- Cache-write tokens × cache-write price

Prices are hard-coded to the published Anthropic rate table per model. Lumen cannot
see your negotiated pricing or credits.

#### Cost breakdown (output / fresh input / cache read / cache write)

Four sub-tiles showing the **per-category dollar contribution** to the session total.
Useful for understanding where your spend is going — typically output tokens dominate
for code generation, while cache reads dominate for long-running agentic sessions.

#### Saved by caching

The dollar value of tokens that hit Claude Code's prompt cache, calculated as:
`cache_read_tokens × (input_price − cache_read_price)`.

**This is reported by Claude Code, not caused by Lumen.** The label always reads
"Saved by caching (reported by Claude Code)". Lumen displays it for completeness —
it does not take credit for it, and it is never added to "Lumen optimized."
See [How much you save](#how-much-you-save) for why these two numbers are kept separate.

#### Used in last 5h / 7d

**Rolling consumption windows** — total tokens spent across all sessions in the last
5 hours and 7 days respectively. Each window shows an approximate reset time and an
Opus-vs-other model split.

**Honesty caveat:** these are *consumption* totals, not "percentage of your plan limit."
Lumen cannot read plan limits from Anthropic's API — it only sees what you've actually
used, which it can measure precisely.

#### Today / This week / All-time

**Calendar rollup totals** — spending grouped by calendar day, ISO week (Monday start),
and all recorded history. These use local time for day boundaries and the current locale's
Monday-based week.

### Optimizer tab

> **Screenshot needed** — `docs/assets/screenshot-optimizer.png`
> Capture: the Optimizer tab showing effectiveness %, Lumen optimized, Saved by caching, and a by-tool breakdown row.

#### Effectiveness %

**The optimizer's hero metric.** For every read that Lumen intercepted, this is the
ratio of tokens saved to tokens that a full read would have cost:

```text
effectiveness = 1 − (returned_tokens / full_tokens)
```

On the author's machine this sits around 87%. Your number varies with your codebase —
larger files with more structure produce higher ratios. Effectiveness is computed over
*all intercepted reads*, so it stabilizes after a few sessions.

**This is measured to the token, never estimated.** Both `full_tokens` and
`returned_tokens` are counted by `lumen-tok`, a local BPE tokenizer, at the moment
of each read.

#### Lumen optimized (caused)

`SUM(saved_tokens)` across all `smart_read`, `recall_file`, and `compress_logs` calls
that Lumen actually made. This number starts small and grows with every session.

**This is the only number Lumen claims credit for.** It is small, verifiable, and
derived directly from the database — not estimated, not extrapolated.

#### Saved by caching (reported)

Shown alongside "Lumen optimized" for context, but in a clearly separate row.
See [Saved by caching](#saved-by-caching) above. The two numbers are **never added together.**

#### By tool

A breakdown of Lumen-optimized tokens by which tool produced them:
`smart_read`, `recall_file`, or `compress_logs`. Useful for understanding which
reads are being intercepted and which file types are generating the most savings.

#### By channel

**Full mode (CLI)** — reads intercepted by the PreToolUse hook in the Claude Code CLI.
Interception is enforced: every large-file Read call is blocked and redirected before
it runs.

**Soft mode (VS Code)** — reads that Claude routed through a Lumen tool opportunistically,
without hook enforcement. See [CLI vs VS Code](#cli-vs-vs-code--full-mode-vs-soft-mode)
for why interception is unavailable in the VS Code extension.

#### Not optimized (read in full)

*CLI / Full mode only.* Reads on files ≥ 300 lines where Claude used the built-in
`Read` tool instead of a Lumen tool — i.e., the hook fired but Claude did not follow
the redirect, or the file was excluded. These are tracked as context (never as savings)
so you can see the true adoption rate. A high "not optimized" count in Full mode
suggests Claude is bypassing the redirect; see [Verify it's working](#verify-its-working).

#### Mode banner (Full / Soft)

A persistent badge on the Optimizer tab showing which mode the current (or most recent)
session ran in. **Full** = Claude Code CLI with hooks active. **Soft** = VS Code extension
(tools available; interception not enforced).

---

## CLI vs VS Code — Full mode vs Soft mode

How much Lumen can do depends on how you run Claude Code.

### Full mode — Claude Code CLI

```bash
npm i -g @anthropic-ai/claude-code   # install if needed
claude                                # open a session
```

In the CLI, Lumen's **PreToolUse hook intercepts every `Read` call** on a large file
(≥ 300 lines) before it runs and redirects Claude to use `lumen:smart_read` instead.
This guarantees the cheaper read path is taken. Reads that bypass Lumen are also tracked
("not optimized — read in full") so you can see the true adoption rate.

### Soft mode — VS Code extension

The VS Code extension [does not fire PreToolUse/PostToolUse hooks](https://github.com/anthropics/claude-code/issues)
(known upstream limitation). Lumen's MCP tools are available and Claude can use them,
but interception is not enforced — Claude routes to optimized reads opportunistically,
not on every large-file read. Only reads that actually went through a Lumen tool appear
on the Optimizer screen.

**Use the CLI for guaranteed, measurable optimization.**
The VS Code extension still gives you the full context gauge, cost tracking, and
caching savings display.

---

## How much you save

The hero metric on the Optimizer screen is **effectiveness %**: on average, every read
that Lumen intercepts returns that many fewer tokens than reading the full file would.
On the author's machine this sits around 87%. Your number varies with your codebase.

Every intercepted read reports `full_tokens` vs `returned_tokens`, measured by the same
BPE tokenizer Claude uses. No estimation. No extrapolation.

The Optimizer screen shows two clearly separated numbers:

| Label | What it is | Caused by |
| --- | --- | --- |
| **Lumen optimized** | `SUM(saved_tokens)` over `smart_read`, `recall_file`, `compress_logs` calls | Lumen |
| **Saved by caching** | Cache-read tokens × (input price − cache-read price) | Claude Code's prompt cache |

These are **never added together.** The caching number is reported by Claude Code; Lumen
displays it for completeness but does not claim credit for it. The "Lumen optimized"
figure starts small and grows with every session. Small and verified beats large and invented.

---

## The optimizer tools

Three MCP tools ship with Lumen. Claude uses them automatically when interception is
active (Full mode), or on-demand in Soft mode:

| Tool | What it does |
| --- | --- |
| `smart_read` | Returns a structural outline of a source file — functions, classes, imports with exact line ranges — without reading bodies. Typically 5–10% of the token cost of reading the full file. |
| `recall_file` | Fetches one or more named items (function, class, struct) or an explicit line range, resolved via tree-sitter AST. Use after `smart_read` once you know what you need. |
| `compress_logs` | Collapses repeated lines, stack-trace runs, and blank-line noise in log files and build output into annotated compact form. Deterministic — not LLM summarization, no information loss. |

Languages supported by `smart_read` / `recall_file`: Rust, Python, TypeScript, TSX.
`compress_logs` works on any text.

---

## Verify it's working

### Check MCP is connected

```bash
claude mcp list
# lumen: /Applications/Lumen.app/Contents/MacOS/lumen-mcp  ✓ Connected
```

Or inside a Claude Code session:

```text
/mcp
```

The `lumen` server should appear as Connected.

### The Optimizer screen shows data

After any session where Claude reads a large file via a Lumen tool, the effectiveness
ratio and token counts appear on the Optimizer tab.

### Trigger an interception (Full mode / CLI)

Ask Claude to read a large source file. Lumen's hook intercepts it and prints:

```text
Lumen intercept: path/to/file.rs is 420 lines.
Instead of reading the full file, call:
  1. lumen:smart_read(path="path/to/file.rs")  → structural outline, ~5-10% token cost
  2. lumen:recall_file(path="path/to/file.rs", names=["<item>"])
```

Claude then uses `smart_read` and the Optimizer screen records the event.

### Troubleshoot: hook not firing

If interception is not happening in the CLI:

```bash
# confirm hooks are registered
python3 -c "import json; d=json.load(open('/Users/$USER/.claude/settings.json')); \
  [print(p, e.get('matcher')) for p,arr in d.get('hooks',{}).items() for e in arr]"
# expected: PreToolUse Read  and  PostToolUse Read

# confirm hook scripts exist and are executable
ls -la ~/.claude/lumen/
```

If either check fails, re-run Setup from the Lumen menu (right-click tray → Setup)
and restart Claude Code.

---

## Security & privacy

**Nothing leaves your machine.**

Here is exactly what Lumen's hooks do:

`lumen_read_intercept.sh` (PreToolUse, CLI only) — a shell script that receives the
`Read` tool call as JSON on stdin, checks the file extension and line count, and if the
file is large, writes a redirect message to stderr for Claude to act on. It reads no
file contents, writes nothing to disk, and makes no network calls.

`lumen_meter.sh` (PostToolUse, CLI only) — a shell script that fires after a `Read`
completes. It counts the tokens in the file using `lumen-tok` (a local BPE tokenizer,
no network) and inserts one row into a local SQLite database.

### The database

All session and usage data is stored locally at:

```text
~/Library/Application Support/io.speedata.lumen/lumen.db
```

It is a plain SQLite file. You can open it with `sqlite3`, inspect it, or delete it
at any time. Deleting it resets all history; Lumen recreates it empty on next launch.

`lumen-daemon` — the background process Lumen launches — watches `~/.claude/projects/`
for new JSONL session files and reads token usage from them, writing to the same local DB.
It makes no network calls.

### Full uninstall

Use the **Uninstall** button on the Setup screen. It removes:

- The `lumen` MCP server entry from `~/.claude.json`
- Lumen's hooks from `~/.claude/settings.json`
- The `~/.claude/lumen/` directory

To remove everything including the database:

```bash
rm -rf /Applications/Lumen.app
rm -rf ~/Library/Application\ Support/io.speedata.lumen
rm -f ~/.lumen_db_path
```

---

## Build from source

**Prerequisites:** Rust (stable), Node 20+, pnpm

```bash
git clone https://github.com/speedata/lumen.git
cd lumen/lumenator

# Build the three helper binaries and stage them for Tauri
./build-sidecar.sh

# Install frontend dependencies
pnpm install

# Development mode
pnpm tauri dev

# Production build → Lumen.app + Lumen_0.1.0_aarch64.dmg
pnpm tauri build
# Artifacts at: target/release/bundle/macos/ and target/release/bundle/dmg/
```

Crate layout:

```text
crates/
  lumen-core/    shared types: Record parser, schema, tokenizer, structurer, compressor
  lumen-daemon/  file watcher + SQLite ingester + WebSocket server
  lumen-mcp/     MCP stdio server (smart_read, recall_file, compress_logs, lumen_ping)
                 also builds lumen-tok (standalone BPE tokenizer)
lumenator/       Tauri application: Angular frontend + Rust backend
```

---

## Known limitations

| | |
| --- | --- |
| Unsigned / un-notarized | Workaround documented above. Notarization on the roadmap. |
| Apple Silicon only | Intel (x86_64) build on request — open an issue. Windows/Linux not yet available. |
| Hooks are CLI-only | VS Code extension API does not support PreToolUse/PostToolUse hooks. Soft mode available. |
| Optimizer requires model cooperation in Soft mode | Full mode (CLI) enforces interception; Soft mode doesn't. |
| Context window is inferred, not authoritative | Lumen infers 200K / 500K / 1M from the model name. Your actual window may differ by plan tier. |
| Plan limits not visible | Lumen reads consumption from session files but cannot query Anthropic for your plan's token limits. |

---

## License

[MIT](LICENSE) — issues, questions, and PRs welcome.
