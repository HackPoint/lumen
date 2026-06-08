# Lumen v0.1.0

**See the whole truth about your Claude Code tokens — context fill, real cost, and verifiable optimization.**

Lumen is a macOS menu-bar app and terminal dashboard for [Claude Code](https://claude.ai/code).
It watches your local session files and shows what Claude Code doesn't: live context fill,
real cost per session, prompt-caching savings (clearly labeled as reported, not caused by Lumen),
and a token-accurate optimizer that intercepts large-file reads and routes them through
smarter, cheaper alternatives.

Everything runs locally. No account. No telemetry. Your data never leaves your machine.

---

## Platform

**macOS Apple Silicon (aarch64) only.** Intel Macs, Windows, and Linux are planned but not yet available.

---

## Install

### Menu-bar app — Homebrew (recommended, no Gatekeeper prompt)

```bash
brew tap HackPoint/tap
brew install --cask HackPoint/tap/lumen
```

Homebrew clears the quarantine flag automatically — Lumen opens normally with no
"Lumen is damaged" dialog. The app is un-notarized (Apple Developer ID signing
planned for a later release); Homebrew handles the quarantine flag for you.

### Menu-bar app — manual .dmg (fallback)

1. Download **`Lumen_0.1.0_aarch64.dmg`** below.
2. Open the .dmg and drag Lumen to `/Applications`.
3. **⚠️ Before opening, run once in Terminal:**

   ```bash
   xattr -dr com.apple.quarantine /Applications/Lumen.app
   ```

   macOS will say **"Lumen is damaged and can't be opened."** — not actual damage.
   Standard block for un-notarized apps. The command removes the quarantine flag.
   Alternative: open once (blocked) → **System Settings → Privacy & Security → Open Anyway**.

4. On first launch, a Setup screen registers the MCP server and hooks. **Restart Claude Code** after setup.

### CLI (`lumen` terminal dashboard)

```bash
brew tap HackPoint/tap && brew install HackPoint/tap/lumen-cli
```

Or use the **Install CLI** button inside the app (symlinks the bundled binary to your PATH).

---

## Modes

| Mode | When | What's enforced |
| --- | --- | --- |
| **Full mode** | Claude Code CLI | Every large-file Read is intercepted before it runs |
| **Soft mode** | VS Code extension | Lumen tools available; interception not enforced |

Use the CLI (`claude`) for guaranteed, measurable optimization.

---

## What it shows

- **Context gauge** — live fill % with green/amber/red thresholds (80% / 95%)
- **Session cost** — input + output + cache tokens priced at the published Anthropic rate table
- **Saved by caching** — reported by Claude Code (not caused by Lumen, always labeled clearly)
- **Optimizer effectiveness** — % fewer tokens per intercepted read; measured to the token, never estimated
- **Usage history** — rolling 5h / 7d windows; today / this week / all-time calendar rollups

---

## Known limitations

- Unsigned / un-notarized (use `xattr` workaround above; notarization on the roadmap)
- Apple Silicon only for now
- Hook interception is CLI-only (VS Code extension API limitation)
- Context window size is inferred from the model name, not authoritative

---

## Uninstall

Use the **Uninstall** button on the Setup screen (removes MCP entry, hooks, and scripts).
To also wipe the local database:

```bash
rm -rf /Applications/Lumen.app
rm -rf ~/Library/Application\ Support/io.speedata.lumen
```
