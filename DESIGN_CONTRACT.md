# Lumen — Design Contract

**For the UI/UX agent.** Redesign freely *on top of* these. Do **not** change how the
data is computed, where it comes from, or the component input shapes. This is the
firewall: visuals are yours; the data layer is frozen.

---

## 1. What you CAN do

- Restyle every component (layout, color, type, spacing, motion, animation).
- Rebuild the visual structure/markup/CSS of the gauge, cost panel, banner, chips.
- Add new presentational components, views, and layouts.
- Retheme via the design tokens (see §5); define/extend them.
- Reorganize the three surfaces (menu-bar icon / tray popover / full window).

## 2. What you must NOT touch

- The signal **names and types** exposed by `SessionService` (§3).
- The component **input names** (§4).
- `SessionService` data logic, `crates/lumen-daemon`, the WebSocket/snapshot
  plumbing, `request_snapshot`, `get_stats`, and `update_tray`'s data contract.
- The token-accumulation `scan` and the cost math inside `cost`.

If a redesign needs a new piece of data, **request it** — don't invent or recompute.

---

## 3. Data surface — `SessionService` (providedIn: 'root')

A single shared instance feeds every window. Inject it
(`readonly s = inject(SessionService)`) and read signals by calling them.

| member | type | meaning |
|---|---|---|
| `fill()` | `number` | current context fill in tokens (active session) |
| `model()` | `string` | active model, e.g. `claude-opus-4-8` |
| `totals()` | `{ input, output, cacheRead, cacheWrite }` numbers | cumulative tokens, active session |
| `maxContext()` | `number` | **dynamic** context window (inferred tier or user override) |
| `compactionThreshold()` | `number` | tokens at which compaction is imminent (95% of window) |
| `contextOverride()` | `number \| null` | user-chosen window; null = auto-infer |
| `setWindow(v: number \| null)` | method | set the override |
| `windowOptions` | `{ label, value }[]` | chip options: Auto / 200K / 500K / 1M |
| `trayPercent()` | `number` | fill as % of window (for tray title) |
| `trayStatus()` | `'ok'\|'warn'\|'alert'` | status by ratio |
| `notification()` | `{ level:'warn'\|'alert'; text } \| null` | active alert or null |

NOTE: the context window is **variable** — never assume 200,000. Always use
`maxContext()` as the denominator. Status thresholds: warn ≥ 0.80, alert ≥ 0.95
of the window. Keep these meanings; restyle the values.

## 4. Component input contracts (do not rename)

**`<gauge>`** — selector `gauge`
```
fill  = input(0)        // tokens
max   = input(200_000)  // pass s.maxContext()
model = input('')
```
You may rebuild the SVG/markup; keep these three inputs. Internal computeds
(percent/status/color/marker) may be restyled but keep their semantics.

**`<cost>`** — selector `cost`
```
totals = input.required<{ input, output, cacheRead, cacheWrite }>()
```
Exposes cost computeds (totalCost, cacheSavings, cacheHitRate, fmt…). Restyle freely.

---

## 5. Design tokens (theme via these; don't hardcode colors)

```
--lumen-bg            page background
--lumen-bg-secondary  card/tile background
--lumen-track         gauge track (unfilled)
--lumen-text          primary text
--lumen-text-dim      secondary text
--lumen-ok            green   (ratio < 0.80)
--lumen-warn          amber   (0.80–0.95)
--lumen-alert         red     (>= 0.95)
--lumen-save          savings green
```

---

## 6. Technical constraints (hard)

- Angular standalone components + signals; `input()`/`computed()`; OnPush.
- Tailwind v4 (CSS-first, config-less); theme via `--lumen-*`.
- No `localStorage`/`sessionStorage` (Tauri webview); state in signals.
- Modern control flow `@if` / `@for` / `@else`.
- Output every changed file IN FULL.

---

## 7. The three surfaces (same data, three densities)

1. **Menu-bar icon** — small macOS tray icon. Rendered as a colored image
   (`icon_as_template(false)`). The icon is a firefly silhouette drawn in the
   current status color (green/amber/red). Built in `src-tauri/src/lib.rs`
   (`render_tray_icon`). No animation; status color only. `update_tray` keeps its
   existing signature (`percent, status`).
2. **Tray popover** — route `/panel`, 320×400, `decorations:false`, transparent.
   Uses the **firefly battery** as the central hero: body fills bottom-to-top
   proportional to `trayPercent()`, color = `trayStatus()`. This is an intentional
   departure from the old ring gauge — the firefly is glanceable and brand-coherent
   for the compact popover surface.
3. **Full window** — route `/` (Home), 800×600. Uses the **ring gauge** as the
   hero. The ring is the precise, number-annotated readout and is appropriate for
   the full-window context where the user has space to read detail.

### Ring vs Firefly — recorded design decision

**The split is intentional, not a bug.** The ring gauge and the firefly battery
serve different purposes at different densities:

| Surface | Indicator | Why |
| --- | --- | --- |
| Tray popover (compact) | Firefly battery | Brand mark, glanceable at a glance, no text needed |
| Full window (spacious) | Ring gauge | Precise %, numeric display, room for the data |
| Menu-bar icon | Firefly silhouette | OS-native size, status color only |

Do not "fix" this inconsistency. It is a deliberate density-appropriate choice.

## 8. Brand direction

Dark, minimal, calm (Linear / Vercel / CleanShot calibre). The radial gauge is the
hero. Green/amber/red is the status language. Motion subtle and purposeful.
Light theme follows `prefers-color-scheme`. Monospace/tabular numerals for money.
