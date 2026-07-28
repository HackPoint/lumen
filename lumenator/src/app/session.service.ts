import { DestroyRef, Injectable, computed, effect, inject, signal, untracked } from '@angular/core';
import { Observable, scan, startWith, merge, from, filter } from 'rxjs';
import { toSignal } from '@angular/core/rxjs-interop';
import { RATE } from './components';
import { TauriBridge } from './tauri-bridge';
import type { DaemonMsg, OptimizerReport, SessionMap, SessionState, Turn, UsageReport } from './components';

// Known context-window tiers (reference data, not logic).
const CONTEXT_TIERS = [200_000, 500_000, 1_000_000] as const;

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
            if (msg.kind === 'snapshot') {
              for (const s of msg.sessions) {
                acc[s.session_id] = {
                  fill: s.fill,
                  model: acc[s.session_id]?.model ?? '',
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
            acc[t.session_id] = {
              fill: t.cache_read_input_tokens,
              // Keep the last known model when a turn reports none. The snapshot
              // branch above preserves it, the CLI reducer guards the same way
              // (crates/lumen-cli/src/data.rs), and get_sessions picks the most
              // recent NON-NULL model — this branch was the only one that let an
              // empty model blank out a known one.
              model: t.model || (prev?.model ?? ''),
              ts: Date.now(),
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
      if (!latest || s.ts > latest.ts) latest = s;
    }
    return latest;
  });

  readonly fill = computed(() => this.active()?.fill ?? 0);
  readonly model = computed(() => this.active()?.model ?? '');
  readonly totals = computed(() =>
      this.active()?.totals ?? { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
  );

  /** User override for context window. null = infer from observed data. */
  readonly contextOverride = signal<number | null>(null);
  readonly windowOptions = WINDOW_OPTIONS;
  setWindow(v: number | null) {
    this.contextOverride.set(v);
  }

  readonly maxContext = computed(() => {
    const override = this.contextOverride();
    if (override) return override;
    const seen = this.fill();
    return CONTEXT_TIERS.find((t) => seen <= t) ?? CONTEXT_TIERS.at(-1)!;
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

  /** Total tokens saved by Lumen across all time (CAUSED, not reported). */
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
