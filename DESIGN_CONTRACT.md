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

**`<lumen-savings-headline>`** — selector `lumen-savings-headline`
```
summary = input<SavingsSummary | null>(null)
// SavingsSummary (src/app/savings-summary.ts) = { week, allTime, … }, each a SavingsPeriod:
//   { tokensSaved (signed), measured (the bold "at least" dollars, null = none measured),
//     modelled (shown beside it, never in it), unpriced (calls with tokens and no dollars), … }
```
Null renders nothing. Fed by the savings summary; the headline computes nothing.

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
3. **Full window** — 800×600, tabs Home · Optimizer · Details · Why · Settings
   (Why only while it has a recommendation). Since 1.9.0 its first screen answers
   three questions only — how much did I save, is it working, what should I do:
   - **Home** (route `/`). The hero is the **savings headline**
     (`<lumen-savings-headline>`): tokens saved and $ saved, net and signed, this
     week and all time, both bold. Under it go the "is it working" status and the
     "what to do" recommendation slots. With no data each renders nothing (or a
     neutral sentence once a summary says nothing was recorded), never a zero
     dressed as a figure. The Setup and billing prompts sit above the tabs.
   - **Details** (route `/details`). Uses the **ring gauge** as its hero, with the
     window selector and the alert banner, then the Optimizer's token ratio,
     breakdowns and provenance counts. The ring is the precise, number-annotated
     readout of Claude Code's context fill, which is not a saving, so it is not on
     Home.
   - **Settings** (route `/settings`): billing mode, spend and its alert
     thresholds, the Repeat reads switch, Report a fault, About.

### Ring vs Firefly — recorded design decision

**The split is intentional, not a bug.** The ring gauge and the firefly battery
serve different purposes at different densities:

| Surface | Indicator | Why |
| --- | --- | --- |
| Tray popover (compact) | Firefly battery | Brand mark, glanceable at a glance, no text needed |
| Full window, Home | Savings headline | Home answers what Lumen saved; context fill is not that |
| Full window, Details (spacious) | Ring gauge | Precise %, numeric display, room for the data |
| Menu-bar icon | Firefly silhouette | OS-native size, status color only |

Do not "fix" this inconsistency. It is a deliberate density-appropriate choice.

## 8. Brand direction

Dark, minimal, calm (Linear / Vercel / CleanShot calibre). The savings headline is
Home's hero, bold, monospace and signed: a loss reads as a loss in the alert colour.
The radial gauge is the hero of the context readout on Details. Green/amber/red is
the status language. Motion subtle and purposeful.
Light theme follows `prefers-color-scheme`. Monospace/tabular numerals for money.
