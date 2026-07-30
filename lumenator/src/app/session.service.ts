import { DestroyRef, Injectable, computed, effect, inject, signal, untracked } from '@angular/core';
import { Observable, scan, startWith, merge, from, filter } from 'rxjs';
import { toSignal } from '@angular/core/rxjs-interop';
import { RATE } from './components';
import { TauriBridge } from './tauri-bridge';
import type { ContextReport, FaultReport, FilingResult, UpdateAvailable } from './components/index';
import type { DaemonMsg, OptimizerReport, SessionMap, SessionState, Turn, UsageReport } from './components';

// Fallback tiers, used ONLY when the model is unrecognised.
const CONTEXT_TIERS = [200_000, 500_000, 1_000_000] as const;

/**
 * Published context windows per model. MIRRORS `MODEL_WINDOWS` in
 * crates/lumen-core/src/rates.rs — keep the two in sync.
 *
 * The window is a property of the MODEL, not of how full the context is.
 * Inferring it from observed fill under-reports systematically: a 1M-window
 * session that has only reached 267K is indistinguishable from a 500K one, which
 * is why the gauge used to read "267,593 / 500,000" on a 1M model.
 *
 * Matched by longest prefix so dated ids (claude-haiku-4-5-20251001) resolve
 * like their alias. Unrecognised models are deliberately absent — falling back
 * to inference is honest; asserting a window we cannot back up is not.
 */
const MODEL_WINDOWS: ReadonlyArray<readonly [string, number]> = [
  // Every current Opus / Sonnet / Fable / Mythos model is 1M.
  ['claude-fable-5', 1_000_000],
  ['claude-mythos-5', 1_000_000],
  ['claude-opus-5', 1_000_000],
  ['claude-opus-4-8', 1_000_000],
  ['claude-opus-4-7', 1_000_000],
  ['claude-opus-4-6', 1_000_000],
  ['claude-sonnet-5', 1_000_000],
  ['claude-sonnet-4-6', 1_000_000],
  // Haiku is the only current model below 1M.
  ['claude-haiku-4-5', 200_000],
];

/** The published window for `model`, or null if unrecognised. */
export function modelWindow(model: string): number | null {
  const m = model.toLowerCase();
  let best: readonly [string, number] | null = null;
  for (const entry of MODEL_WINDOWS) {
    if (m.startsWith(entry[0]) && (best === null || entry[0].length > best[0].length)) {
      best = entry;
    }
  }
  return best?.[1] ?? null;
}

/** Smallest tier that fits `fill`. Only a lower bound — see resolveWindow. */
export function inferWindow(fill: number): number {
  return CONTEXT_TIERS.find((t) => fill <= t) ?? CONTEXT_TIERS[CONTEXT_TIERS.length - 1];
}

/**
 * The window to display. `peakFill` must be the session HIGH-WATER MARK, not the
 * current fill — /compact drops fill sharply and inferring from the momentary
 * value would shrink the window mid-session and make the gauge jump.
 */
export function resolveWindow(model: string, peakFill: number): number {
  const published = modelWindow(model);
  // Never claim a window smaller than a fill we have actually observed.
  return published === null ? inferWindow(peakFill) : Math.max(published, inferWindow(peakFill));
}

// User-selectable window options. null = auto-infer from observed fill.
export const WINDOW_OPTIONS = [
  { label: 'Auto', value: null },
  { label: '200K', value: 200_000 },
  { label: '500K', value: 500_000 },
  { label: '1M', value: 1_000_000 },
] as const;

// Fraction of the window at which Claude Code compacts (~95%).
const COMPACTION_RATIO = 0.95;

// How often to refresh the aggregate Usage & Cost report (rollups change slowly).
const USAGE_REFRESH_MS = 60_000;

/** Parse a raw daemon JSON payload into a typed message, or null. */
function parseDaemon(raw: string): DaemonMsg | null {
  try {
    const payload = JSON.parse(raw);
    if (payload.type === 'snapshot' && Array.isArray(payload.sessions)) {
      return { kind: 'snapshot', sessions: payload.sessions };
    }
    if (payload.type === 'event' && payload.turn) {
      return { kind: 'turn', turn: payload.turn as Turn };
    }
  } catch {
    /* ignore malformed */
  }
  return null;
}

/** Live stream of daemon messages from the Tauri "daemon" event. */
function liveStream$(bridge: TauriBridge): Observable<DaemonMsg> {
  return new Observable<DaemonMsg>((subscriber) => {
    const sub = bridge.listen$('daemon').subscribe((raw) => {
      const msg = parseDaemon(raw);
      if (msg) subscriber.next(msg);
    });
    return () => sub.unsubscribe();
  });
}

/**
 * One-shot: ask the backend for the cached snapshot (fixes the race where the
 * daemon sent the snapshot before Angular started listening).
 */
function cachedSnapshot$(bridge: TauriBridge): Observable<DaemonMsg> {
  return from(
      bridge.invoke<string | null>('request_snapshot')
          .then((raw) => (raw ? parseDaemon(raw) : null))
          .catch(() => null),
  ).pipe(filter((m): m is DaemonMsg => m !== null));
}

/** Fold counter for `SessionState.seq`. Module-scoped: one reducer per service. */
let seq = 0;

@Injectable({ providedIn: 'root' })
export class SessionService {
  /**
   * Every backend call goes through this seam so the service can be constructed
   * in a test or a plain browser. Declared first: `sessions` below uses it in
   * its own field initialiser, and field order is initialisation order.
   */
  private readonly bridge = inject(TauriBridge);

  /** Cached snapshot (on demand) merged with the live event stream. */
  private readonly sessions = toSignal(
      merge(cachedSnapshot$(this.bridge), liveStream$(this.bridge)).pipe(
          scan<DaemonMsg, SessionMap>((acc, msg) => {
            // Increments on every fold. Two editor windows produce turns in the
            // same millisecond, and `ts` alone then keeps whichever session was
            // seen first — which is the opposite of "follow the active window".
            seq += 1;
            if (msg.kind === 'snapshot') {
              for (const s of msg.sessions) {
                acc[s.session_id] = {
                  fill: s.fill,
                  // Prefer the daemon's peak; fall back to the current fill.
                  peakFill: Math.max(
                      acc[s.session_id]?.peakFill ?? 0,
                      s.peak_fill ?? s.fill,
                      s.fill,
                  ),
                  model: s.model || (acc[s.session_id]?.model ?? ''),
                  project: s.project || (acc[s.session_id]?.project ?? ''),
                  seq,
                  ts: Date.parse(s.ts) || Date.now(),
                  startTs: acc[s.session_id]?.startTs ?? Date.now(),
                  recentOutput: acc[s.session_id]?.recentOutput ?? [],
                  totals: {
                    input: s.input,
                    output: s.output,
                    cacheRead: s.cache_read,
                    cacheWrite: s.cache_write,
                  },
                };
              }
              return { ...acc };
            }

            const t = msg.turn;
            const prev = acc[t.session_id];
            const recentOutput = [...(prev?.recentOutput ?? []), t.output_tokens].slice(-10);
            const pt = prev?.totals ?? { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 };
            // A subagent transcript reuses the parent's sessionId but starts with a
            // fresh, small context. Its tokens are real spend (folded into totals
            // below), but letting it set `fill` made the gauge dip every time one
            // ran — so the gauge keeps the main agent's values.
            const sub = t.is_subagent === true;
            acc[t.session_id] = {
              fill: sub ? (prev?.fill ?? 0) : t.cache_read_input_tokens,
              peakFill: sub
                  ? (prev?.peakFill ?? 0)
                  : Math.max(prev?.peakFill ?? 0, t.cache_read_input_tokens),
              // Keep the last known model when a turn reports none. The snapshot
              // branch above preserves it, the CLI reducer guards the same way
              // (crates/lumen-cli/src/data.rs), and get_sessions picks the most
              // recent NON-NULL model — this branch was the only one that let an
              // empty model blank out a known one. A subagent may run a different
              // model, which must not become the session's.
              model: sub ? (prev?.model ?? '') : t.model || (prev?.model ?? ''),
              project: t.project || (prev?.project ?? ''),
              seq,
              // The turn's own timestamp, so live turns and snapshot rows are
              // ordered on the same clock.
              ts: (t.ts ? Date.parse(t.ts) : NaN) || Date.now(),
              startTs: prev?.startTs ?? Date.now(),
              recentOutput,
              totals: {
                input: pt.input + t.input_tokens,
                output: pt.output + t.output_tokens,
                cacheRead: pt.cacheRead + t.cache_read_input_tokens,
                cacheWrite: pt.cacheWrite + t.cache_creation_input_tokens,
              },
            };
            return { ...acc };
          }, {}),
          startWith({} as SessionMap),
      ),
      { initialValue: {} as SessionMap },
  );

  /** Most recently active session. */
  private readonly active = computed<SessionState | null>(() => {
    const map = this.sessions();
    let latest: SessionState | null = null;
    for (const s of Object.values(map)) {
      // Strictly-greater on ts alone kept the FIRST session seen whenever two
      // windows produced turns in the same millisecond. Falling back to seq makes
      // the most recently updated session win, which is what "active" means.
      if (latest === null || s.ts > latest.ts || (s.ts === latest.ts && s.seq > latest.seq)) {
        latest = s;
      }
    }
    return latest;
  });

  readonly fill = computed(() => this.active()?.fill ?? 0);
  readonly model = computed(() => this.active()?.model ?? '');
  /** Project the gauge is currently following. Empty when unknown. */
  readonly project = computed(() => this.active()?.project ?? '');
  /** How many sessions have been seen — >1 means the label matters. */
  readonly sessionCount = computed(() => Object.keys(this.sessions()).length);
  readonly totals = computed(() =>
      this.active()?.totals ?? { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
  );

  /** User override for context window. null = infer from observed data. */
  readonly contextOverride = signal<number | null>(null);
  readonly windowOptions = WINDOW_OPTIONS;
  setWindow(v: number | null) {
    this.contextOverride.set(v);
  }

  /** Session high-water mark, for window resolution. */
  readonly peakFill = computed(() => this.active()?.peakFill ?? 0);

  readonly maxContext = computed(() => {
    const override = this.contextOverride();
    if (override) return override;
    // Model first, peak second. Using the momentary fill here is what made a
    // 1M-window session read as 500K, and made the window shrink after /compact.
    return resolveWindow(this.model(), this.peakFill());
  });

  readonly compactionThreshold = computed(() => this.maxContext() * COMPACTION_RATIO);

  readonly trayPercent = computed(() => {
    const max = this.maxContext();
    return max > 0 ? Math.round((this.fill() / max) * 100) : 0;
  });

  readonly trayStatus = computed<'ok' | 'warn' | 'alert'>(() => {
    const max = this.maxContext();
    const r = max > 0 ? this.fill() / max : 0;
    if (r >= 0.95) return 'alert';
    if (r >= 0.80) return 'warn';
    return 'ok';
  });

  // ─────────────────────────────────────────────────────────────────────────
  // D4 — Cost signals derived from existing queries; no new DB calls.
  // ─────────────────────────────────────────────────────────────────────────

  /** Active-session cost from the live totals signal. */
  readonly costSession = computed(() => {
    const t = this.totals();
    return t.input * RATE.input + t.output * RATE.output +
           t.cacheRead * RATE.cacheRead + t.cacheWrite * RATE.cacheWrite;
  });

  /** Today's cost from the usage rollup signal (0 until usage loads). */
  readonly costToday = computed(() => {
    const u = this.usage();
    if (!u) return 0;
    const t = u.today;
    return t.input * RATE.input + t.output * RATE.output +
           t.cacheRead * RATE.cacheRead + t.cacheWrite * RATE.cacheWrite;
  });

  // ─────────────────────────────────────────────────────────────────────────
  // D4 — Cost-threshold alerts.
  //
  // Thresholds are in-memory signals (no localStorage — Tauri webview).
  // Defaults: $5 daily, $2 per session.  Set to 0 to disable that alert.
  //
  // _dailyFired / _sessionFired flip once per threshold crossing and reset
  // when spend drops back below.  D5 (below) watches these flags to fire a
  // native toast exactly once per crossing.
  // ─────────────────────────────────────────────────────────────────────────

  /** Daily spend limit in USD. 0 = disabled. */
  readonly dailySpendLimit = signal(5);
  /** Per-session spend limit in USD. 0 = disabled. */
  readonly sessionSpendLimit = signal(2);

  setDailyLimit(v: number): void { this.dailySpendLimit.set(Math.max(0, v)); }
  setSessionLimit(v: number): void { this.sessionSpendLimit.set(Math.max(0, v)); }

  // Edge-trigger flags — private state; flip once per crossing direction.
  private readonly _dailyFired = signal(false);
  private readonly _sessionFired = signal(false);

  // Active cost alert text (set by edge-trigger effect, read by notification()).
  private readonly _costAlert = signal<{ level: 'warn' | 'alert'; text: string } | null>(null);

  // ─────────────────────────────────────────────────────────────────────────
  // D4 — notification() — single active in-app banner.
  // Priority: compaction (alert) → daily cost (alert) → context 80% (warn)
  //           → session cost (warn) → velocity (warn) → long session (warn).
  // ─────────────────────────────────────────────────────────────────────────
  readonly notification = computed<{ level: 'warn' | 'alert'; text: string } | null>(() => {
    const s = this.active();
    const max = this.maxContext();
    const compaction = this.compactionThreshold();

    // 1. Compaction — most time-critical
    if (s && s.fill >= compaction) {
      return {
        level: 'alert',
        text: 'Context full — compaction imminent. Consider wrapping up or starting fresh.',
      };
    }

    const costNote = this._costAlert();

    // 2. Daily cost (alert level)
    if (costNote?.level === 'alert') return costNote;

    // 3. Context approaching limit (warn)
    if (s) {
      const ratio = s.fill / max;
      if (ratio >= 0.8) {
        return { level: 'warn', text: `Context at ${Math.round(ratio * 100)}% — approaching compaction.` };
      }
    }

    // 4. Session cost (warn level)
    if (costNote?.level === 'warn') return costNote;

    // 5–6. Velocity / duration (require active session)
    if (s) {
      const out = s.recentOutput;
      if (out.length >= 5) {
        const avg = out.reduce((a, b) => a + b, 0) / out.length;
        if (avg > 4000) {
          return {
            level: 'warn',
            text: `High output rate (~${Math.round(avg).toLocaleString()} tokens/turn). Burning context fast.`,
          };
        }
      }

      const minutes = (Date.now() - s.startTs) / 60000;
      if (minutes > 120) {
        return { level: 'warn', text: `Long session (${Math.round(minutes)} min). Context may be drifting.` };
      }
    }

    return null;
  });

  // ─────────────────────────────────────────────────────────────────────────
  // D5 — Native notification toggle + delivery.
  //
  // nativeNotify is in-memory (no localStorage); defaults on.
  // _compactionFired mirrors the compaction edge so the native-notification
  // effect has the same clean boolean-edge signal as D4's cost flags.
  //
  // Plain boolean fields (_native*Sent) track whether we've already sent a
  // notification for the current active crossing so we don't resend on every
  // effect re-run while the alert is still live.
  // ─────────────────────────────────────────────────────────────────────────

  /** Enable/disable native OS notifications. Resets to true on restart. */
  readonly nativeNotify = signal(true);
  setNativeNotify(v: boolean): void { this.nativeNotify.set(v); }

  private readonly _compactionFired = signal(false);

  // Per-alert dedup guards (plain booleans — only ever read/written inside the
  // D5 effect; don't need to be reactive).
  private _nativeCompactionSent = false;
  private _nativeDailySent = false;
  private _nativeSessionSent = false;

  /**
   * Aggregate Usage & Cost report (rolling windows + calendar rollups +
   * lifetime caching). Fetched on demand from get_usage and refreshed on a
   * slow timer. Null until the first fetch resolves.
   */
  readonly usage = signal<UsageReport | null>(null);

  /** Re-fetch the aggregate report. */
  refreshUsage(): void {
    this.bridge.invoke<UsageReport>('get_usage')
        .then((u) => this.usage.set(u))
        .catch(() => { /* not in Tauri / db not ready — ignore */ });
  }

  // ─────────────────────────────────────────────────────────────────────────
  // E5 — Optimizer savings (CAUSED by Lumen).
  //
  // HONEST LABELING — two separate numbers, never merged:
  //   "Lumen optimized" (here) = caused = saved_tokens in read_events.
  //   "Saved by caching" (D2)  = reported = cache_read in turns table.
  //
  // Dollar conversion: lifetimeOptimizedTokens * RATE.input — optimizer
  // saves input token reads, so the input rate is correct.
  //
  // Mode banner:
  //   cli     → "Full mode — reads intercepted; savings + missed both tracked."
  //   vscode  → "Soft mode — tools available but not enforced; only optimized
  //              reads tracked (bypassed built-in reads invisible here)."
  // ─────────────────────────────────────────────────────────────────────────

  /** Raw report from get_optimizer_stats. Null until first fetch resolves. */
  readonly optimizerStats = signal<OptimizerReport | null>(null);

  /** Re-fetch the optimizer report. */
  refreshOptimizerStats(): void {
    this.bridge.invoke<OptimizerReport>('get_optimizer_stats')
        .then((r) => this.optimizerStats.set(r))
        .catch(() => { /* not in Tauri / db not ready — ignore */ });
  }

  /** Net dollar value of interception. 0 when the backend cannot price a round. */
  readonly netValueUsd = computed(() => this.optimizerStats()?.netValueUsd ?? 0);
  readonly grossValueUsd = computed(() => this.optimizerStats()?.grossValueUsd ?? 0);
  readonly roundCostUsd = computed(() => this.optimizerStats()?.roundCostUsd ?? 0);
  readonly valueRounds = computed(() => this.optimizerStats()?.valueRounds ?? 0);
  /** True once the backend has enough turns to price a round at all. */
  readonly netValuePriced = computed(() => (this.optimizerStats()?.roundCostUsd ?? 0) > 0);

  // ── Context diagnostics ───────────────────────────────────────────────────
  //
  // Where the project's context has actually gone. Kept separate from the optimizer
  // signals on purpose: those make a savings claim and this one does not.

  /** Raw report from get_context_report. Null until first fetch resolves. */
  readonly contextReport = signal<ContextReport | null>(null);

  /** Re-fetch the context diagnostics. */
  refreshContextReport(): void {
    this.bridge.invoke<ContextReport>('get_context_report')
        .then((r) => this.contextReport.set(r))
        .catch(() => { /* not in Tauri / db not ready — ignore */ });
  }

  // ── Fault reporting ───────────────────────────────────────────────────────
  //
  // Two steps on purpose. Rendering is free and local; filing publishes to a public
  // tracker and cannot be undone, so it never happens as a side effect of looking.

  /** Rendered report, or null for "not fetched yet". */
  readonly faultReport = signal<FaultReport | null>(null);
  /** True once a fetch has resolved with nothing to report. */
  readonly faultsNone = signal(false);
  readonly faultReportLoading = signal(false);
  readonly faultFiling = signal(false);
  /**
   * Outcome of the last filing attempt, or null if it has not been tried.
   *
   * The whole result, not just a URL: the browser route only opens a prefilled form, so
   * the view has to know whether anything was actually published.
   */
  readonly faultFiled = signal<FilingResult | null>(null);
  readonly faultError = signal<string | null>(null);

  /**
   * Faults waiting, for the nav badge and the tray panel.
   *
   * Its own signal rather than derived from {@link faultReport}: the badge has to be
   * current on every screen, and rendering a whole issue body to show a number would
   * make navigation expensive. Ranked declines are excluded by the backend — hundreds of
   * them would keep the badge permanently lit.
   */
  readonly faultCount = signal(0);

  refreshFaultCount(): void {
    this.bridge.invoke<number>('get_fault_count')
        .then((n) => this.faultCount.set(n ?? 0))
        .catch(() => { /* not in Tauri / db not ready — leave the badge dark */ });
  }

  // ── Update notice ─────────────────────────────────────────────────────────
  //
  // Minor and major releases only. The backend returns nothing for a patch bump, for an
  // already-announced version, or when the check is disabled — so this signal is
  // populated only when there is genuinely something to say.

  readonly updateAvailable = signal<UpdateAvailable | null>(null);

  /**
   * Ask the backend whether a newer release exists, and notify once if so.
   *
   * The notification is sent from here rather than the backend so it goes through the
   * same permission-aware path as every other notification, and so a browser or test
   * build simply records it instead of needing a native notification service.
   */
  checkForUpdate(): void {
    this.bridge.invoke<UpdateAvailable | null>('check_for_update')
        .then((u) => {
          if (!u) return;
          this.updateAvailable.set(u);
          void this.notifyUpdate(u);
        })
        .catch(() => { /* offline, or not in Tauri — the next launch tries again */ });
  }

  private async notifyUpdate(u: UpdateAvailable): Promise<void> {
    try {
      let granted = await this.bridge.isPermissionGranted();
      if (!granted) granted = (await this.bridge.requestPermission()) === 'granted';
      if (!granted) return;
      this.bridge.sendNotification({
        title: `Lumen ${u.latest} is available`,
        body: `You are on ${u.current}. This is a ${u.bump} release.`,
      });
    } catch {
      // A refused or unavailable notification service is not worth surfacing; the notice
      // is still on screen.
    }
  }

  /**
   * Ask the tray popover to fit `height` logical pixels.
   *
   * The backend clamps it. Fire-and-forget: a popover that is briefly the wrong size is a
   * cosmetic problem, and surfacing an error for it would be worse than the symptom.
   */
  resizePanel(height: number): void {
    this.bridge.invoke<void>('resize_panel', { height }).catch(() => { /* not in Tauri */ });
  }

  /** Reveal the main window. The tray panel has no navigation of its own. */
  openMainWindow(): void {
    this.bridge.invoke<void>('show_main_window').catch(() => { /* not in Tauri */ });
  }

  /** Render the current fault report. Local only — nothing leaves the machine. */
  refreshFaultReport(): void {
    this.faultReportLoading.set(true);
    this.faultError.set(null);
    this.faultFiled.set(null);
    this.bridge.invoke<FaultReport | null>('get_fault_report')
        .then((r) => {
          this.faultReport.set(r ?? null);
          this.faultsNone.set(r === null);
        })
        .catch((e: unknown) => this.faultError.set(this.message(e)))
        .finally(() => this.faultReportLoading.set(false));
  }

  /**
   * File the report that is currently on screen.
   *
   * Sends the body already rendered rather than asking the backend to re-render: the user
   * approved a specific text, and re-rendering could file a different one.
   */
  fileFaultReport(): void {
    const report = this.faultReport();
    // Guard, not an assertion: the button is disabled without a report, and a filing call
    // that slipped through anyway must publish nothing.
    if (!report || this.faultFiling()) return;

    this.faultFiling.set(true);
    this.faultError.set(null);
    this.bridge.invoke<FilingResult>('file_fault_report', {
          body: report.body,
          title: report.title,
          fingerprint: report.fingerprint,
          repo: report.repo,
        })
        .then((r) => this.faultFiled.set(r))
        .catch((e: unknown) => this.faultError.set(this.message(e)))
        .finally(() => this.faultFiling.set(false));
  }

  /** Clear a rendered report without filing it. */
  dismissFaultReport(): void {
    this.faultReport.set(null);
    this.faultsNone.set(false);
    this.faultError.set(null);
    this.faultFiled.set(null);
  }

  private message(e: unknown): string {
    return e instanceof Error ? e.message : String(e);
  }

  readonly contextTotalTokens = computed(() => this.contextReport()?.totalTokensRead ?? 0);
  readonly contextFiles = computed(() => this.contextReport()?.topFiles ?? []);
  readonly contextDistinctFiles = computed(() => this.contextReport()?.distinctFiles ?? 0);
  readonly contextTop10Share = computed(() => this.contextReport()?.top10SharePct ?? 0);
  readonly contextUnchangedRereads = computed(
      () => this.contextReport()?.totalUnchangedRereads ?? 0);

  /** Total tokens saved by Lumen across all time (CAUSED, not reported). */
  /**
   * Metered events whose token provenance was never recorded.
   *
   * Rows predating 1.1.5 carry no `token_source`, and on installs whose baked
   * tokenizer path had died the hook substituted bytes/4 silently. While any
   * remain, the Optimizer screen qualifies its accuracy claim instead of asserting
   * an exactness the data cannot support.
   */
  readonly unverifiedProvenanceRows = computed(
      () => this.optimizerStats()?.unverifiedProvenanceRows ?? 0,
  );
  readonly provenanceTotalRows = computed(
      () => this.optimizerStats()?.provenanceTotalRows ?? 0,
  );

  readonly lifetimeOptimizedTokens = computed(
      () => this.optimizerStats()?.lifetimeOptimizedTokens ?? 0,
  );

  /**
   * Lifetime optimizer savings in USD.
   * Uses RATE.input: optimizer saves input reads; this is the correct rate.
   * Distinct from cacheSavings (D2) which uses RATE.input − RATE.cacheRead.
   */
  readonly lifetimeOptimizedUsd = computed(
      () => this.lifetimeOptimizedTokens() * RATE.input,
  );

  /** Per-channel breakdown (cli | vscode | unknown). */
  readonly optimizedByChannel = computed(
      () => this.optimizerStats()?.byChannel ?? [],
  );

  /** Per-tool breakdown (smart_read | recall_file | compress_logs). */
  readonly optimizedByTool = computed(
      () => this.optimizerStats()?.byTool ?? [],
  );

  /** Tokens saved today (local calendar day). */
  readonly optimizedToday = computed(
      () => this.optimizerStats()?.todaySavedTokens ?? 0,
  );

  /** Tokens saved this ISO week (Mon–Sun, local time). */
  readonly optimizedThisWeek = computed(
      () => this.optimizerStats()?.thisWeekSavedTokens ?? 0,
  );

  /**
   * Overall optimizer effectiveness: saved / full over all lumen routes (0–100).
   * 0 when no lumen calls recorded yet.
   */
  readonly effectivenessPct = computed(() => {
    const r = this.optimizerStats();
    if (!r || r.lifetimeFullTokens === 0) return 0;
    return (r.lifetimeOptimizedTokens / r.lifetimeFullTokens) * 100;
  });

  /**
   * CLI-only: reads that bypassed Lumen (model used built-in Read instead).
   * Labeled "not optimized (read in full)". Never counted as savings.
   * missedCalls=0 and missedFullTokens=0 in VS Code (hook doesn't fire).
   */
  readonly missedReads = computed(() => ({
    calls: this.optimizerStats()?.missedCalls ?? 0,
    fullTokens: this.optimizerStats()?.missedFullTokens ?? 0,
  }));

  /**
   * Channel of the active context, derived from the most recent read_events row.
   * "cli"     → Full mode (interception on, missed reads tracked).
   * "vscode"  → Soft mode (tools available, not enforced, missed reads invisible).
   * "unknown" → No optimizer events recorded yet.
   */
  readonly currentChannel = computed(
      () => this.optimizerStats()?.currentChannel ?? 'unknown',
  );

  constructor() {
    // ── Tray redraw on every fill/status change ──────────────────────────
    effect(() => {
      const percent = this.trayPercent();
      const status = this.trayStatus();
      this.bridge.invoke('update_tray', { percent, status }).catch(() => {});
    });

    // ── D4: cost threshold edge-trigger ──────────────────────────────────
    // Runs whenever costToday, costSession, or either limit changes.
    // _dailyFired / _sessionFired are read via untracked() to prevent this
    // effect from tracking its own writes.
    effect(() => {
      const daily = this.costToday();
      const session = this.costSession();
      const dailyLimit = this.dailySpendLimit();
      const sessionLimit = this.sessionSpendLimit();

      const dailyFired = untracked(() => this._dailyFired());
      const sessionFired = untracked(() => this._sessionFired());

      const dailyOver = dailyLimit > 0 && daily > dailyLimit;
      const sessionOver = sessionLimit > 0 && session > sessionLimit;

      // Flip flags once per crossing; reset on drop-below.
      if (dailyOver !== dailyFired) this._dailyFired.set(dailyOver);
      if (sessionOver !== sessionFired) this._sessionFired.set(sessionOver);

      // Active cost alert text updates on every run (keeps spend figure current).
      // Daily takes priority over session.
      if (dailyOver) {
        this._costAlert.set({
          level: 'alert',
          text: `Today's spend $${daily.toFixed(2)} exceeded your $${dailyLimit.toFixed(0)} daily limit.`,
        });
      } else if (sessionOver) {
        this._costAlert.set({
          level: 'warn',
          text: `Session cost $${session.toFixed(2)} exceeded your $${sessionLimit.toFixed(0)} per-session limit.`,
        });
      } else {
        this._costAlert.set(null);
      }
    });

    // ── D5: compaction crossing tracker ─────────────────────────────────
    // Provides a clean boolean-edge signal for the native notification effect
    // without coupling it to the full notification() computed.
    effect(() => {
      const s = this.active();
      const compaction = this.compactionThreshold();
      const compFired = untracked(() => this._compactionFired());
      const compOver = s !== null && s.fill >= compaction;
      if (compOver !== compFired) this._compactionFired.set(compOver);
    });

    // ── D5: native notification delivery ────────────────────────────────
    // Re-runs when _compactionFired, _dailyFired, _sessionFired, or
    // nativeNotify changes.  Plain boolean guards prevent resending while an
    // alert is still active (distinct from the D4 in-app banner which stays
    // visible — a toast fires once then dismisses on its own).
    effect(() => {
      if (!this.nativeNotify()) {
        // Disable path: reset guards so re-enabling doesn't suppress alerts.
        this._nativeCompactionSent = false;
        this._nativeDailySent = false;
        this._nativeSessionSent = false;
        return;
      }

      const compFired = this._compactionFired();
      const dailyFired = this._dailyFired();
      const sessionFired = this._sessionFired();

      // Compaction — most urgent
      if (compFired && !this._nativeCompactionSent) {
        this._nativeCompactionSent = true;
        void this.fireNativeNotification(
            'Lumen — Context Alert',
            'Context full — compaction imminent. Consider wrapping up or starting fresh.',
        );
      } else if (!compFired) {
        this._nativeCompactionSent = false;
      }

      // Daily cost — alert level; read _costAlert untracked to avoid extra dep.
      if (dailyFired && !this._nativeDailySent) {
        this._nativeDailySent = true;
        const note = untracked(() => this._costAlert());
        void this.fireNativeNotification(
            'Lumen — Daily Spend Alert',
            note?.text ?? "Today's spend exceeded your daily limit.",
        );
      } else if (!dailyFired) {
        this._nativeDailySent = false;
      }

      // Session cost — warn level; skip if daily is also active (avoid spam).
      if (sessionFired && !this._nativeSessionSent && !dailyFired) {
        this._nativeSessionSent = true;
        const note = untracked(() => this._costAlert());
        void this.fireNativeNotification(
            'Lumen — Session Alert',
            note?.text ?? 'Session cost exceeded your limit.',
        );
      } else if (!sessionFired) {
        this._nativeSessionSent = false;
      }
    });

    // Aggregate reports: fetch once now, then refresh slowly.
    this.refreshUsage();
    this.refreshOptimizerStats();
    // Tied to the injector lifetime: an uncleared interval outlives the service
    // and, in tests, leaks across cases.
    const refreshTimer = setInterval(() => {
      this.refreshUsage();
      this.refreshOptimizerStats();
    }, USAGE_REFRESH_MS);
    inject(DestroyRef).onDestroy(() => clearInterval(refreshTimer));
  }

  /**
   * Request OS notification permission (once) then fire a native toast.
   * Wrapped in try/catch so it silently no-ops outside the Tauri context
   * (e.g. browser dev server) or when the user has denied permission.
   */
  private async fireNativeNotification(title: string, body: string): Promise<void> {
    try {
      let granted = await this.bridge.isPermissionGranted();
      if (!granted) {
        const perm = await this.bridge.requestPermission();
        granted = perm === 'granted';
      }
      if (granted) {
        this.bridge.sendNotification({ title, body });
      }
    } catch {
      // Outside Tauri context, or permission permanently denied — skip silently.
    }
  }
}
