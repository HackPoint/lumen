# Lumen

![Lumen firefly mascot](docs/assets/firefly.svg)

See the whole truth about your Claude Code tokens — context fill, real cost, and verifiable optimization.

[![Download](https://img.shields.io/github/v/release/HackPoint/lumen?label=download&color=3fb950)](../../releases/latest)
![Platform](https://img.shields.io/badge/macOS-Apple%20Silicon-lightgrey)
![Platform](https://img.shields.io/badge/Linux-x86__64-lightgrey)
![License](https://img.shields.io/badge/license-MIT-blue)

---

![Lumen panel — context gauge, session cost, and caching savings](docs/assets/screenshot-gauge.png)

---

## What it shows

Lumen is a tray/menu-bar app and terminal dashboard for [Claude Code](https://claude.ai/code) users,
running on macOS, Linux and Windows.
It watches your session files locally and surfaces things Claude Code doesn't show in its own interface:

| Signal | Where it appears |
| --- | --- |
| **Context fill** — tokens used / window size, live | Menu-bar icon · tray popover · main window gauge |
| **Compaction warning** — amber at 80%, red at 95% | Tray popover · OS notification |
| **Session cost** — input + output + cache tokens, priced | Main window cost tiles |
| **Caching savings** — what Claude Code's cache actually saved | Optimizer screen (labeled "reported by Claude Code") |
| **Net value of interception** — tokens saved, priced, less the round they cost | Optimizer screen hero metric |
| **Context hotspots** — which files your context actually goes into | Hotspots screen |

No account, and no telemetry: none of the above leaves your machine. Lumen makes two
network requests in total, both described under [Security & privacy](#security--privacy)
— a once-a-day check for a new release, and filing a fault report, which only happens
when you ask for it.

---

## Platform support

| Platform | GUI | CLI (`lumen`) | Notes |
| --- | --- | --- | --- |
| **macOS** (Apple Silicon) | ✅ `.dmg` + Homebrew cask | ✅ Homebrew | Primary development platform |
| **Linux** (x86_64) | ✅ AppImage + `.deb` | ✅ Homebrew + tarball | Requires WebKitGTK 4.1 — see below |
| **Windows** (x86_64) | ✅ `.exe` installer | ✅ `.zip` | GUI installer is un-signed |
| macOS (Intel) | ❌ | ❌ | Build on request — open an issue |
| Linux (arm64) | ❌ | ❌ | No artifact built yet |

**Requirements**

- **macOS:** 13 Ventura or later · Apple Silicon (aarch64)
- **Linux:** glibc 2.35+ (Ubuntu 22.04 / Debian 12 or newer) · WebKitGTK 4.1 · a
  tray-capable desktop (see [Linux tray support](#linux-tray-support))
- **All platforms:** [Claude Code](https://claude.ai/code) installed

The GUI is not code-signed or notarized on any platform. Per-platform workarounds
are documented in the install sections below.

---

## Install the menu-bar app

### Via Homebrew (recommended — opens with no Gatekeeper prompt)

```bash
brew tap HackPoint/tap
brew trust --cask HackPoint/tap/lumen-app   # required for casks outside homebrew/cask
brew install --cask lumen-app
```

Without the `trust` step Homebrew refuses with *"Refusing to load cask … from
untrusted tap"*. That applies to every third-party cask, not just this one.

> **The cask is `lumen-app`, not `lumen`.** Homebrew already ships an unrelated
> `lumen` (a screen-brightness tool), so that token would install the wrong
> application. The installed app is still **Lumen.app** and the terminal command is
> still `lumen`.
>
> Upgrading from 1.1.3 or earlier, which used the `lumen` token? Move across once:
>
> ```bash
> brew uninstall --cask lumen && brew install --cask lumen-app
> ```
>
> Your data in `~/Library/Application Support/io.speedata.lumen/` is untouched.

Homebrew clears the quarantine flag automatically. Lumen opens normally with no
"damaged" dialog. The app is still un-notarized (proper Apple Developer ID signing
is planned); Homebrew handles the flag for you.

After install, launch Lumen from Spotlight or `/Applications/Lumen.app`.

### Via .dmg (manual — Gatekeeper workaround required)

1. Download **`Lumen_1.1.0_aarch64.dmg`** from the [Releases page](../../releases/latest)
2. Open the .dmg and drag **Lumen** to your Applications folder
3. **⚠️ Before opening — run this once in Terminal:**

   ```bash
   xattr -dr com.apple.quarantine /Applications/Lumen.app
   ```

   macOS will say **"Lumen is damaged and can't be opened."** — this is not actual
   damage. It is the standard block for un-notarized apps. The command removes
   the quarantine flag; double-click Lumen normally after.

   Alternative: try to open it once (blocked), then **System Settings → Privacy &
   Security → Open Anyway**.

   > The permanent fix is Apple Developer ID + notarization ($99/yr) — planned for
   > a later release. Until then, use Homebrew (above) or the xattr command.

4. On first launch, a **Setup** screen appears — click through to register the MCP
   server and hooks. **Restart Claude Code** after setup.

### Linux (x86_64)

Two packages are published per release. Both bundle the daemon, MCP server and CLI
as sidecars, so there is nothing else to install.

**AppImage** — works on any distribution, no root needed:

```bash
VERSION=1.1.0
curl -LO "https://github.com/HackPoint/lumen/releases/download/v${VERSION}/Lumen-${VERSION}-x86_64.AppImage"
chmod +x "Lumen-${VERSION}-x86_64.AppImage"
./"Lumen-${VERSION}-x86_64.AppImage"
```

**.deb** — for Debian, Ubuntu and derivatives:

```bash
VERSION=1.1.0
curl -LO "https://github.com/HackPoint/lumen/releases/download/v${VERSION}/lumen_${VERSION}_amd64.deb"
sudo apt install "./lumen_${VERSION}_amd64.deb"    # apt resolves the dependencies
```

Then launch **Lumen** from your desktop's application menu — the package installs a
`.desktop` entry. From a shell the binary is `Lumen` (capital L, matching the product
name).

`apt install ./file.deb` is deliberate over `dpkg -i`: the package declares
WebKitGTK and app-indicator dependencies, and only `apt` will pull them in.

If WebKitGTK is missing, install it directly:

```bash
# Debian / Ubuntu
sudo apt install libwebkit2gtk-4.1-0 libgtk-3-0 libayatana-appindicator3-1

# Fedora
sudo dnf install webkit2gtk4.1 gtk3 libappindicator-gtk3

# Arch
sudo pacman -S webkit2gtk-4.1 gtk3 libappindicator-gtk3
```

#### Linux tray support

Lumen lives in the system tray, and Linux tray support depends on your desktop:

| Desktop | Works out of the box? |
| --- | --- |
| KDE Plasma, Cinnamon, Budgie, XFCE | ✅ Yes |
| GNOME | ⚠️ Needs the [AppIndicator extension](https://extensions.gnome.org/extension/615/appindicator-support/) — GNOME removed tray icons |
| Sway / i3 / wlroots | ⚠️ Needs a tray-capable bar (Waybar with `tray`, or `i3status-rust`) |

With no tray host running, the app still works and the main window still opens —
you just lose the menu-bar icon. If the window does not appear on Wayland, force
X11: `GDK_BACKEND=x11 ./Lumen-1.1.0-x86_64.AppImage`.

Data lives in `~/.local/share/io.speedata.lumen/` (following the XDG layout),
not in `~/Library` as on macOS.

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

Lumen lives in the **menu bar / system tray**, not in the Dock or taskbar — top-right
on macOS, wherever your desktop puts indicators on Linux and Windows.

| Action | Result |
| --- | --- |
| **Left-click** the tray icon | Quick popover — context gauge, current cost, mode badge |
| **Right-click** → "Open Lumen" | Full window with **Context** and **Optimizer** tabs |

The tray icon pulses with ring animations; its color reflects context fill status
(green → amber at 80% → red at 95%).

---

## How to run the CLI (`lumen`)

The `lumen` terminal command is a **live dashboard** — the same data as the GUI,
rendered in your terminal.

> **Correction.** Earlier versions of this document said hooks fire only in the
> Claude Code CLI and not in the VS Code extension, and built a "Full mode vs Soft
> mode" distinction on it. That is false: hooks fire in the VS Code extension too.
> Measured directly — 108 built-in `Read` events were recorded by the PostToolUse
> hook during a single session whose `entrypoint` was `claude-vscode`, and in the
> same session every file over the line threshold was intercepted and redirected.
> Interception does not depend on which client you use.

### Install

**Via Homebrew:**

```bash
brew tap HackPoint/tap && brew install HackPoint/tap/lumen-cli
```

Works on macOS (Apple Silicon) and Linux (x86_64). On Windows, or without
Homebrew, download the archive for your platform from the
[Releases page](../../releases/latest) and put `lumen` on your `PATH`.

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

**The optimizer's hero metric is dollars, not the token ratio.** The token ratio was the
headline until 1.3.1, and it flattered the product. Returning 87% fewer tokens sounds
decisive, but an intercepted read also *costs* something — the read was blocked, so the
model spends an extra round calling a Lumen tool instead. The honest question is whether
the tokens saved are worth more than the round they cost:

```text
value of saving S tokens = S × (cache_write + cache_read × R) / 1e6
cost of the extra round  = (context × cache_read + output × output_rate) / 1e6 × pairs
```

`R` is how many rounds the saving keeps paying for, bounded by the next compaction. Every
other input comes from the call itself.

**No dollar figure is published here.** This section used to quote one for the author's
machine, and it rested on `R`: not derived per call, but one assumed constant, 65 rounds
(`DEFAULT_ROUNDS_REMAINING` in `crates/lumen-core/src/econ.rs`), and the value side of the
formula is linear in it. Until `R` is derived per call, a dollar total
is that assumption multiplied out. The Optimizer screen still computes the figure from your
own ledger; read it with that in mind. The token ratio beneath it is measured.

**Tool calls and built-in `Read` events are measured to the token.**

`smart_read`, `recall_file` and `compress_logs` count tokens in-process with a BPE
tokenizer (`cl100k_base`, not Claude's own) and have no estimation path, so their figures
are exact in its units.

Since 1.6.0 built-in `Read` events are counted by `lumen-mcp hook meter`, with the same
tokenizer, and it has no estimation path either: a file that is not text is recorded as 0
tokens with provenance `unsupported`, and a meter that cannot write its row records a fault
instead of a guess. Before 1.6.0 a shell hook shelled out to `lumen-tok` and fell back to
`bytes ÷ 4` when it could not; before 1.1.5 it did so **silently**, and on installs set up
from a mounted `.dmg` before 1.0.1 the baked path pointed inside the disk image — so once
it was ejected, every built-in `Read` figure became an estimate while this document claimed
otherwise. Each row records `token_source` (`measured` / `estimated` / `unsupported`), and
rows predating 1.1.5 are marked as unverified provenance rather than reclassified, because
there is no honest way to recover it after the fact.

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

Reads intercepted by the PreToolUse hook, in either client. Interception is
enforced wherever hooks run: a Read of a file over the line threshold is blocked and
redirected before it executes.

> The channel breakdown that used to appear here has been removed rather than
> repaired. Until 1.1.5 the meter wrote the literal string `cli` on every built-in
> `Read` row, so the chart plotted a constant and the "CLI missed reads" metric
> filtered on a value that matched every row. Real channel detection landed in 1.1.5;
> the breakdown returns once enough rows carry a measured channel to make it mean
> something.

#### Not optimized (read in full)

*Both clients.* Reads on files ≥ 300 lines where Claude used the built-in
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

**The CLI is a dashboard, not a precondition for optimization.**

Interception works in both clients, so installing the CLI does not enable or
guarantee anything the extension lacks. The earlier claim that it did was based on
the same false premise as the Full/Soft mode distinction above.
The VS Code extension still gives you the full context gauge, cost tracking, and
caching savings display.

---

## How much you save

The hero metric on the Optimizer screen is the **net dollar value** of interception: what
the tokens Lumen avoided are worth, less what the extra round cost. It is computed from your
own ledger with one assumed input, how many rounds a saving keeps paying for, so no figure
is quoted here; [Effectiveness %](#effectiveness-) says why.

The token ratio — **84% fewer tokens per intercepted read** on the author's machine — is
shown underneath it. It is real and measured to the token, but on its own it is not a
result: a smaller reply that forces a second round is a loss however good the ratio looks.

Every intercepted read reports `full_tokens` vs `returned_tokens`, counted with the
`cl100k_base` BPE tokenizer. That is a real tokenizer but not the one Claude bills with, so
the counts are exact in its units and an approximation of Claude's. No estimation, no
extrapolation, and no scaling up — 96.6% of that saving rests on a real tokenizer count
rather than a bytes/4 guess, which is asserted by a test rather than asserted here.

**→ [Does the optimizer actually save anything?](docs/efficiency.md)** is the full
measurement, including the things that would make the figure above dishonest: **8 of 1,608
eligible reads bypassed** the optimizer, all of them the intercept's deliberate fail-open guards, 3.4M tokens of binary-file noise excluded from the
baseline, and the 17 of 31 files where interception costs more than it saves. No Lumen tool can
return more than the file it was asked about — that is a backstop with a corpus test behind it,
not an observation. Every figure there is produced and asserted by
`cargo test --release -p lumen-mcp --test efficiency -- --nocapture`, so a regression fails CI
instead of ageing into a stale claim.

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
| `smart_read` | Returns a structural outline of a source file — functions, classes, imports with exact line ranges — without reading bodies. Each reply reports its own token count against the full file's, so what it saved is measured per call, not quoted. |
| `recall_file` | Fetches one or more named items (function, class, struct) or an explicit line range, resolved via tree-sitter AST. Use after `smart_read` once you know what you need. |
| `compress_logs` | Collapses repeated lines, stack-trace runs, and blank-line runs in log files and build output into annotated compact form. Deterministic — not LLM summarization. Repeated lines are kept with their count; the middle of a long stack trace is dropped, leaving the count of frames omitted. |

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

Ask Claude to read a large source file. Lumen's hook blocks the read and tells Claude:

```text
Lumen intercept: path/to/file.rs is 420 lines.
Instead of reading the full file, call:
  1. lumen:smart_read(path="path/to/file.rs") → structural outline with line ranges
  2. lumen:recall_file(path="path/to/file.rs", names=["<item>"]) → fetch only what you need
Each call reports the tokens it saved in _meta.saved_tokens.
Use smart_read(mode="full") only if you truly need every line.

If the lumen tools are unavailable to you (server down, permission denied), retry
this exact Read — it will be allowed through. Do not abandon the task.
```

Claude then uses `smart_read` and the Optimizer screen records the event. The message
quotes no savings figure: Claude routes on it, and a typical-case number would bias that
routing on files where it does not hold.

### Troubleshoot: hook not firing

If interception is not happening in the CLI:

```bash
# confirm hooks are registered
grep -n '"command": .*/lumen_' ~/.claude/settings.json
# expected: lumen_read_intercept.sh once (PreToolUse on Read)
#           and lumen_meter.sh twice (PostToolUse on Read and on Bash)
# more meter lines than that: an older Setup also hooked the lumen tools; re-run Setup

# confirm hook scripts exist and are executable
ls -la ~/.claude/lumen/
```

From a checkout of this repository, `./scripts/verify-hooks.sh` goes further: it runs the
installed hooks the way Claude Code does, against a scratch ledger it deletes afterwards,
and says what each one did.

If any check fails, re-run Setup from the Lumen menu (right-click tray → Setup)
and restart Claude Code.

---

## Security & privacy

**Nothing leaves your machine.**

Here is exactly what Lumen's hooks do. Both are short shell scripts in `~/.claude/lumen/`
that hand each event to `lumen-mcp hook`, the binary that also serves the tools. Neither
makes a network call.

`lumen_read_intercept.sh` (PreToolUse, CLI only) — receives the `Read` tool call as JSON
on stdin. If the file is a source or log file at or above the line threshold, it blocks
the read and writes a redirect message to stderr for Claude to act on. **It opens the
file only to count its lines, and keeps nothing of it.**

It writes two things, both local, and both there so that a redirect can never leave
Claude with no way to read the file at all:

- **A session marker** — `lumen_intercept_<session-id>` in the temp directory, one line
  per file already redirected in this session. A file is redirected at most once: if
  Claude comes back to the built-in `Read` for the same file, the Lumen route did not work
  for it and the read is allowed through. Without this the hook can deadlock a session —
  it blocks the built-in `Read` while the replacement is unreachable.
- **A fault record** — one JSON line appended to `faults.jsonl` beside the database when
  a hook lets through a read it would have redirected, or loses an event it should have
  metered. It contains the file path, its line count, what went wrong, and the session id
  — **never file contents**. `lumen report` reads these; nothing is sent anywhere unless
  you explicitly file a report. Set `LUMEN_CAPTURE=0` to keep the hooks and record nothing.

If `lumen-mcp` is gone — Lumen moved or removed — both scripts let the event through, say
so on stderr and record a fault, rather than blocking a read nothing is left to serve.
Set `LUMEN_HOOK_ENABLED=0` to disable interception entirely. `LUMEN_DEBUG=1`, which is off
unless you set it, also writes the payload of each run verbatim to `lumen_hook_dump.json` in
the temp directory, each run overwriting the last, file contents and full command lines
included; it exists to capture test fixtures.

### Checking for a new release

**The only network request Lumen makes without being asked.** Once a day, at most, the app
GETs `https://api.github.com/repos/HackPoint/lumen/releases/latest` and compares the tag to
its own version.

What is sent: nothing beyond what any HTTPS request to GitHub reveals. No credential, no
identifier, no version string, nothing about the machine, the projects on it, or the ledger.
It is a plain unauthenticated read of a public endpoint.

You are notified only for a **minor or major** release. Patch releases are silent — a
notification that fires for every `x.y.Z` gets dismissed reflexively, and then the one that
mattered gets dismissed too. A given version is announced once, and the check state lives
in `update_check.json` beside the database.

Turn it off entirely:

```bash
export LUMEN_UPDATE_CHECK=0
```

With that set, Lumen makes no unprompted network request of any kind.

### Filing a fault report

**→ [Filing a fault report](docs/filing-a-fault-report.md)** walks through it with
screenshots. The short version:

Nothing is sent until you ask for it — via `lumen report --yes`, or the **File issue**
button under **Report a fault** on the Hotspots screen. You see the exact text first,
and it is the text that gets sent; it is never re-generated at send time.

`lumen` comes from the `lumen-cli` formula; a cask-only install leaves it off your `PATH`.
The binary is still in the bundle at
`/Applications/Lumen.app/Contents/MacOS/lumen-cli`, which is the route that works when the
app will not start — see [If `lumen` is not a
command](docs/filing-a-fault-report.md#if-lumen-is-not-a-command).

Three routes are tried in order, and the first that works wins:

| order | route | needs | can comment on an existing issue |
| --- | --- | --- | --- |
| 1 | GitHub CLI — `gh issue create` / `gh issue comment` | `gh` installed and authenticated | yes |
| 2 | REST API — `POST /repos/{repo}/issues` | `GITHUB_TOKEN` or `GH_TOKEN` in the environment | yes |
| 3 | Prefilled browser form | a browser | no — it opens the existing issue instead |

The browser route is a **handoff, not a filing**: it opens GitHub's new-issue form with
the body already filled in, and nothing exists on the tracker until you press Submit.
Lumen says so rather than reporting it as filed.

Why a chain: `gh` is the only route that can post a follow-up comment without a human,
but almost nobody running the app has it. The browser route needs nothing at all, so
there is always a way to report a fault. When a route is skipped, the reason is shown —
a silent fallback would hide that your preferred one is broken.

Reports are deduplicated on a fingerprint of `(kind, variant, version)` carried in the
body as an HTML comment. A second report of the same fault comments on the existing
issue instead of opening a duplicate. The lookup reads the tracker over HTTPS and works
without any credentials on a public repository; a token is only ever needed to *write*.

`lumen_meter.sh` (PostToolUse) — a shell script that fires after a `Read` or a `Bash`
call completes and hands the event to `lumen-mcp hook meter`. It inserts one row into a
local SQLite database and makes no network calls. What it records differs by tool:

- **After a `Read`** it counts the tokens in the file that was just read, with the BPE
  tokenizer built into `lumen-mcp` (local, no network), and stores the file's path, line
  count and modification time. Files that are not text — images, binaries — get a row
  with no token count and a provenance of `unsupported`; Lumen does not guess a number
  for them.
- **After a `Bash`** call it counts the tokens in the output the command already
  produced, to measure how much of your context goes to command output rather than to
  files. This is **observation only**: there is no `PreToolUse` hook on `Bash`, nothing
  is intercepted, blocked, or wrapped, and no command is ever executed by Lumen.

  Of the command line itself, only the program and its subcommand are stored — `cargo
  test`, `git status` — never the full text. Command lines routinely carry credentials
  in flags and URLs, and a leading `VAR=value` assignment is dropped before the label
  is taken, so `TOKEN=secret curl …` is recorded as `curl`. Command **output** is
  tokenized in memory, from the payload Claude Code hands the hook, and unless
  `LUMEN_DEBUG=1` is set it is never written anywhere — only the resulting count is stored.

If you would rather not record `Bash` output at all, set `LUMEN_METER_BASH` to `0` in the
`env` block of `~/.claude/settings.json`:

```json
"env": { "LUMEN_METER_BASH": "0" }
```

It applies from the next Claude Code session. Reads are still metered, and Setup keeps
the `env` block when it runs again or upgrades Lumen. Removing the `Bash` entry under
`PostToolUse` also stops it, but only until the next time Setup runs, because Setup puts
that entry back.

### Experimental: ranked outline (1.3.0, off by default)

`smart_read`'s outline can be sized by the economics of the call rather than by a fixed
format. Set `LUMEN_RANKED_OUTLINE` in the environment Claude Code passes to the MCP
server:

| value | behaviour |
|---|---|
| unset, or anything unrecognised | **off** — the outline that has always shipped |
| `on` | every file uses the ranked outline |
| `ab` | half of files, split by a stable hash of the path, so a given file always takes the same arm |

`LUMEN_RANKED_TIME_BUDGET_MS` overrides the pipeline's wall-clock ceiling (50 ms by
default). Raise it on a slow machine; a file that exceeds it falls back to the ordinary
outline and records `ranked_too_slow`.

An intercepted read costs one extra round, so the outline is only worth returning when it
saves more than that round costs. Lumen computes the minimum saving from your own `turns`
history and refuses files that cannot clear it, recording the refusal and the numbers
behind it. Every decision is written to `read_events` — `budget`, `s_min`, the context,
rounds and output figures used, `k_selected` of `n_total`, and `coeff_version` — so the
two arms can be compared afterwards rather than trusted.

**Measured caveat before you enable it.** On this repository the ranked outline returns
*more* tokens than the current one (10,759 vs 6,205 across the files that qualify),
because it captures nested definitions the old outline never did and because the budget is
usually large enough that nothing gets trimmed. The saving comes from the refusals, not
from the ranking. Treat `on` as an experiment, not an optimisation.

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

**Prerequisites:** Rust (stable), Node 22.22.3+, 24.15+ or 26+ (Angular 22's floor — 24.0–24.14 is too old), pnpm

On Linux, also install the GUI toolkit headers:

```bash
sudo apt install libwebkit2gtk-4.1-dev libgtk-3-dev \
                 libayatana-appindicator3-dev librsvg2-dev patchelf
```

```bash
git clone https://github.com/HackPoint/lumen.git
cd lumen/lumenator

# Build the three helper binaries and stage them for Tauri
./build-sidecar.sh

# Install frontend dependencies
pnpm install

# Development mode
pnpm tauri dev

# Production build → Lumen.app + Lumen_1.1.0_aarch64.dmg
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
  lumen-stats/   SQLite rollups the GUI displays: usage, sessions, optimizer
lumenator/       Tauri application: Angular frontend + Rust backend
```

---

## Troubleshooting

**No menu-bar icon on macOS?** The most common cause is that the icon was ⌘-dragged off the menu
bar at some point — macOS remembers that permanently, and recreates the item hidden on every later
launch. Lumen now clears that state at startup and tells you it did. If the icon is still missing:

```sh
lumen doctor          # names the likely cause and its one-line fix
```

**→ [No menu-bar icon (macOS)](docs/troubleshooting-the-tray.md)** is the full diagnosis path.

If Lumen is running but you cannot reach it, `open -a Lumen` (or double-clicking it in
/Applications) reveals the main window, and `lumen show` does the same from a terminal. The CLI
inside the bundle works even when the GUI does not:

```sh
/Applications/Lumen.app/Contents/MacOS/lumen-cli report --dry-run
```

Logs are at `~/Library/Logs/io.speedata.lumen/Lumen.log`. Set `LUMEN_LOG=debug` for more.
*(Released builds up to and including 1.5.1 wrote no log at all — the logger was registered
only in debug builds.)*

---

## Known limitations

| | |
| --- | --- |
| Unsigned / un-notarized | Workaround documented above. Notarization on the roadmap. |
| No Intel macOS or arm64 Linux build | Build on request — open an issue. |
| Linux tray needs a tray host | GNOME requires the AppIndicator extension; see [Linux tray support](#linux-tray-support). |
| Hooks are CLI-only | VS Code extension API does not support PreToolUse/PostToolUse hooks. Soft mode available. |
| Optimizer requires model cooperation in Soft mode | Full mode (CLI) enforces interception; Soft mode doesn't. |
| Context window comes from a built-in model table | Known models use their published window; unrecognised ones fall back to inferring 200K / 500K / 1M from observed fill. Your actual window may differ by plan tier. |
| Plan limits not visible | Lumen reads consumption from session files but cannot query Anthropic for your plan's token limits. |

---

## License

[MIT](LICENSE) — issues, questions, and PRs welcome.
