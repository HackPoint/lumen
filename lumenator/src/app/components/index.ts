interface SessionState {
    fill: number;
    /** Monotonic fold counter, used only to break ties on `ts`. Millisecond
     *  timestamps collide when two windows are both active. */
    seq: number;
    /** Session high-water mark. The context window is derived from this, never
     *  from `fill` — /compact drops fill sharply and would shrink the window. */
    peakFill: number;
    model: string;
    /** Short project label, so a user with several editor windows open can tell
     *  which session the gauge is following. Empty when unknown. */
    project: string;
    ts: number;
    startTs: number;        // first turn seen this session
    recentOutput: number[]; // last N output_tokens for velocity
    totals: { input: number; output: number; cacheRead: number; cacheWrite: number };
}
type SessionMap = Record<string, SessionState>;

/** Daemon turn payload (matches the Rust TurnMsg). */
interface Turn {
    session_id: string;
    /** Turn timestamp (ISO-8601 UTC). Ordering sessions by arrival time instead
     *  of this put snapshot and live turns on different clocks. */
    ts?: string;
    model: string;
    cache_read_input_tokens: number;
    cache_creation_input_tokens: number;
    input_tokens: number;
    output_tokens: number;
    /** Subagent turns cost money but carry their own separate context, so they
     *  must not drive the fill gauge. Absent on older daemons. */
    is_subagent?: boolean;
    project?: string | null;
}

/** Opus pricing, USD per token (per-1M ÷ 1e6). */
const RATE = {
    input: 5.0 / 1_000_000,
    output: 25.0 / 1_000_000,
    cacheRead: 0.5 / 1_000_000,     // 0.1× input — 90% cheaper
    cacheWrite: 6.25 / 1_000_000,   // 1.25× input (5-min default)
};


interface CostTotals {
    input: number;
    output: number;
    cacheRead: number;
    cacheWrite: number;
}

type DaemonMsg =
    | { kind: 'snapshot'; sessions: SnapshotSession[] }
    | { kind: 'turn'; turn: Turn };

interface SnapshotSession {
    session_id: string;
    input: number;
    output: number;
    cache_read: number;
    cache_write: number;
    fill: number;
    /** Session peak fill. Absent from daemons predating this field. */
    peak_fill?: number;
    /** Most recent non-null model for the session. Absent on older daemons. */
    model?: string | null;
    /** Short project label derived from the transcript path. */
    project?: string | null;
    ts: string;
}

/**
 * Aggregate consumption over a window (from the Rust `get_usage` command).
 * Token sums only — dollar cost is derived in the frontend via RATE (the
 * single price source), never recomputed here or in SQL.
 */
interface TokenAgg {
    turns: number;
    input: number;
    output: number;
    cacheRead: number;
    cacheWrite: number;
    totalTokens: number;
}

/**
 * Usage & Cost report (CONSUMPTION, not quota). No "% of limit" / "remaining":
 * the plan denominator is server-side and unknown locally. `resetApprox` is a
 * proxy (windowStart + 5h), explicitly approximate.
 */
interface UsageReport {
    rolling5h: TokenAgg;
    windowStart: string | null;
    resetApprox: string | null;
    rolling7dOpus: TokenAgg;
    rolling7dOther: TokenAgg;
    today: TokenAgg;
    thisWeek: TokenAgg;
    allTime: TokenAgg;
}

// ─────────────────────────────────────────────────────────────────────────
// E5 — Optimizer savings (raw token counts; frontend applies RATE.input for USD)
//
// HONEST LABELING:
//   OptimizerReport = tokens CAUSED by Lumen. Exact for Lumen tool calls, which
//   tokenize in-process with no fallback. Built-in Read events are counted by a
//   shell hook that can fall back to bytes/4; token_source records which, and
//   unverifiedProvenanceRows counts the rows that predate that tracking.
//   This is distinct from "Saved by caching" (turns.cache_read * RATE diff),
//   which is REPORTED by Claude Code, not caused by Lumen.
//   Never merge the two numbers in the UI.
// ─────────────────────────────────────────────────────────────────────────

/** Per-channel optimizer breakdown (cli | vscode | unknown). */
interface ChannelBreakdown {
    channel: string;
    calls: number;
    savedTokens: number;
    fullTokens: number;
}

/** Per-tool optimizer breakdown (smart_read | recall_file | compress_logs). */
interface ToolBreakdown {
    tool: string;
    calls: number;
    savedTokens: number;
    fullTokens: number;
}

/**
 * Lifetime optimizer savings from `get_optimizer_stats`.
 *
 * Dollar cost = lifetimeOptimizedTokens * RATE.input  (input reads saved).
 * DO NOT mix with UsageReport.allTime.cacheRead (that's reported caching, not caused).
 *
 * missedCalls / missedFullTokens: CLI-only reads that bypassed Lumen.
 * Label as "not optimized (read in full)". Never count as savings.
 */
interface OptimizerReport {
    lifetimeOptimizedTokens: number;
    lifetimeFullTokens: number;
    todaySavedTokens: number;
    thisWeekSavedTokens: number;
    byChannel: ChannelBreakdown[];
    byTool: ToolBreakdown[];
    /** Channel of the most recent read_events row — proxy for active context. */
    currentChannel: string;
    missedCalls: number;
    missedFullTokens: number;
    /**
     * Built-in Reads of files with no token count — images, binaries. Excluded from
     * missedCalls because there was no optimization available to miss, and carried
     * separately so the exclusion stays visible. Optional: a 1.2.0 or earlier backend
     * does not send it.
     */
    unmeasurableCalls?: number;
    /** Metered events with no recorded token provenance (rows predating 1.1.5). */
    unverifiedProvenanceRows: number;
    /** Total metered events, so the UI can say "N of M". */
    provenanceTotalRows: number;
    /**
     * Net dollar value of interception: the avoided tokens priced, less the extra rounds
     * they cost. The headline from 1.4.0 on.
     *
     * The token ratio it replaces flattered the product — a smaller reply that forces
     * another round is a loss however good the ratio looks. Optional so a 1.3.x backend
     * still renders.
     */
    netValueUsd?: number;
    grossValueUsd?: number;
    roundCostUsd?: number;
    /** Rounds a saving keeps paying for. Surfaced because the result is most sensitive to it. */
    valueRounds?: number;
    /** Rounds each intercept costs — 1.604 measured. */
    pairMultiplier?: number;
}

export type {
    Turn,
    SessionState,
    SessionMap,
    CostTotals,
    DaemonMsg,
    SnapshotSession,
    TokenAgg,
    UsageReport,
    ChannelBreakdown,
    ToolBreakdown,
    OptimizerReport,
};
export { RATE };

/**
 * One file that a meaningful share of the project's context has gone into.
 *
 * Diagnosis, not savings. Nothing here claims to have saved anything, which is why it
 * is the one figure in the product that cannot be net-negative: it costs no tokens,
 * intercepts nothing and forces no rounds.
 */
export interface FileHotspot {
    path: string;
    /** Basename, so the view need not split paths. */
    name: string;
    reads: number;
    totalTokens: number;
    /** Share of every token this project has read. */
    sharePct: number;
    lines: number | null;
    /**
     * Re-reads where the file had not changed since the previous read of it — context
     * re-acquired rather than retained.
     */
    unchangedRereads: number;
    /** Present only where the numbers warrant one. */
    recommendation: string | null;
}

export interface ContextReport {
    totalTokensRead: number;
    distinctFiles: number;
    topFiles: FileHotspot[];
    /** Share of all tokens read that sits in the ten largest files. */
    top10SharePct: number;
    totalUnchangedRereads: number;
}

/**
 * A newer release worth telling the user about.
 *
 * Only ever populated for a minor or major bump — the backend returns nothing for a patch,
 * so the UI cannot notify for one even by accident.
 */
export interface UpdateAvailable {
    current: string;
    latest: string;
    /** `minor` or `major`. */
    bump: string;
    url: string;
}

/**
 * What filing did, and which route did it.
 *
 * `handoff` is the field that matters. The browser route opens a prefilled form and
 * nothing exists on the tracker until the user submits it, so the UI must not report a
 * handoff as filed — that would claim something that has not happened.
 */
export interface FilingResult {
    url: string;
    /** `gh`, `api` or `browser`. */
    route: string;
    handoff: boolean;
    commented: boolean;
    /** Why each earlier route was passed over. Surfaced so a degraded setup is visible. */
    fellBack: string[];
}

/**
 * A rendered fault report, ready to show and then file.
 *
 * `body` is the whole issue text, already redacted by the backend: metadata only, no file
 * contents, no absolute paths. It is passed back to `file_fault_report` unchanged so the
 * text the user approved is byte-for-byte the text that gets filed.
 */
export interface FaultReport {
    body: string;
    title: string;
    /** Dedupe key. A second filing of the same fingerprint comments instead of duplicating. */
    fingerprint: string;
    /** Distinct (kind, variant) groups, for a badge that need not parse the body. */
    kinds: number;
    occurrences: number;
    repo: string;
}
