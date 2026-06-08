# Lumen

**macOS menu-bar app that tracks your Claude Code context window in real time.**

> Download the latest release → drag to Applications → right-click Open (see note below).

---

## What it does

Lumen watches your Claude Code session files and shows a live gauge of how full your
context window is — so you know when to `/compact` before things slow down or a
compaction happens mid-thought.

It also ships a context-optimizer MCP server (`lumen-mcp`) that intercepts large file
reads and routes them through cheaper structural tools, measurably reducing how fast
your context fills.

**Two modes:**
- **CLI** (`claude` in terminal) — full optimization: Read intercept hook + token meter
- **VS Code extension** — MCP optimizer only (hooks don't fire in the extension)

---

## Install

1. Download `Lumen_0.1.0_aarch64.dmg` from the [latest release](../../releases/latest)
2. Open the .dmg, drag **Lumen** to Applications
3. **Right-click → Open → Open** (required once because the app is unsigned — see below)
4. Lumen appears in the menu bar and runs the first-time setup automatically

### Gatekeeper note (unsigned build)

macOS will block the first launch because this build is not signed or notarized.
The workaround:

```
# Option A — right-click method (GUI)
Right-click Lumen.app → Open → click Open in the dialog

# Option B — if macOS shows "damaged and can't be opened"
sudo xattr -dr com.apple.quarantine /Applications/Lumen.app
# then double-click normally
```

Signing/notarization is on the roadmap.

---

## Build from source

**Prerequisites:** Rust (stable), Node 20+, pnpm, Xcode Command Line Tools

```bash
git clone https://github.com/<org>/lumen
cd lumen/lumenator
./build-sidecar.sh          # builds lumen-daemon, lumen-mcp, lumen-tok
pnpm install
pnpm tauri dev              # development mode
pnpm tauri build            # produces .app + .dmg in target/release/bundle/
```

---

## License

MIT — see [LICENSE](LICENSE)
