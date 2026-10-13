import type { ModelTokens } from '../prices';

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
    /**
     * The same tokens by model, speed and region, keyed by all three. A session can mix models,
     * a subagent on a cheaper one, speeds and regions, so its cost is priced from these, never
     * the totals.
     */
    byModel: Record<string, ModelTokens>;
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
    /**
     * The part of the cache write at the 1-hour rate. Null when the transcript recorded no
     * split by duration, and absent on older daemons: either way the duration is unknown.
     */
    cache_creation_1h_input_tokens?: number | null;
    /** `usage.speed`, 'standard' or 'fast'. Null or absent when not recorded. */
    speed?: string | null;
    /**
     * `usage.inference_geo`, 'us', 'global' or 'not_available'. Null or absent when not
     * recorded.
     */
    inference_geo?: string | null;
    /** Subagent turns cost money but carry their own separate context, so they
     *  must not drive the fill gauge. Absent on older daemons. */
    is_subagent?: boolean;
    project?: string | null;
}

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
    /**
     * The sums above by model. Absent from daemons older than 1.9.0, and null when the daemon
     * could not split them: the sums are then left unpriced, since the session's latest model
     * need not be the one that consumed them.
     */
    by_model?: SnapshotModel[] | null;
}

/** One model's share of a snapshot session, at one speed in one region. */
interface SnapshotModel {
    model: string | null;
    speed?: string | null;
    /** Absent from a daemon older than this field: the region is then not known. */
    inference_geo?: string | null;
    input: number;
    output: number;
    cache_read: number;
    cache_write: number;
    cache_write_1h?: number;
    /** Absent from a daemon that did not send it; the duration is then unknown. */
    cache_write_unknown?: number;
}

/**
 * Aggregate consumption over a window (from the Rust `get_usage` command).
 * Token sums only. Dollars are priced here in the frontend, each model's share at that
 * model's rates from the `lumen_prices` table, never in SQL.
 */
interface TokenAgg {
    turns: number;
    input: number;
    output: number;
    cacheRead: number;
    cacheWrite: number;
    totalTokens: number;
    /** The same sums by model. Optional because a backend older than 1.9.0 does not send it. */
    byModel?: ModelAgg[];
}

/** One model's share of a {@link TokenAgg}, at one speed in one region. */
interface ModelAgg extends ModelTokens {
    turns: number;
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
// E5 — Optimizer savings (raw token counts; no dollars: a read event records no model)
//
// HONEST LABELING:
//   OptimizerReport = tokens CAUSED by Lumen. Exact for Lumen tool calls, which
//   tokenize in-process with no fallback. Built-in Read events were counted until
//   1.6.0 by a shell hook that could fall back to bytes/4; token_source records
//   which, and unverifiedProvenanceRows counts the rows estimated that way or
//   written before that tracking.
//   This is distinct from "Saved by caching" (turns.cache_read priced per model),
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
 * Tokens only: a read event records no model, so there is no price to put on them.
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
    /**
     * Metered events whose count is not known to be measured: estimated by a hook
     * before 1.6.0, or written before 1.1.5, when provenance began to be recorded.
     */
    unverifiedProvenanceRows: number;
    /**
     * Metered events that carry a count, so the UI can say "N of M". Reads of images
     * and binaries carry none and are in neither figure.
     */
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
    /**
     * What repeat reads have done so far. Counted apart from every figure above: the
     * answers it counts are in none of the token totals. Optional because a backend
     * older than 1.8.0 does not send it, and then the page shows nothing about it.
     */
    repeatReads?: RepeatReads;
}

/**
 * Answers served by the repeat-reads feature, from `OptimizerReport.repeatReads`.
 *
 * `tokensNotResent` is counted against reading the whole file again, not against the
 * outline Lumen would have sent with the feature off, and covers measured answers only.
 */
interface RepeatReads {
    /** "Unchanged since an earlier read" answers. */
    unchangedServed: number;
    /** Answers that sent only what an edit changed. */
    deltasServed: number;
    /** Full tokens minus the tokens returned, over the served answers whose count was measured. */
    tokensNotResent: number;
    /** How many of the served answers carry a measured count, so the page can say when the figure covers fewer. */
    measuredAnswers: number;
    /** Full results asked for again with `fresh=true`, in the arm that got the feature. */
    freshRequests: number;
    /** Reads that got a full result because Lumen could not tell which conversation they came from. */
    unidentifiedReads: number;
}

/**
 * The repeat-reads switch as the settings file holds it, from `get_repeat_reads_setting`.
 *
 * This is the file and the default, never the server's effective mode: a
 * `LUMEN_REPEAT_READS` value in a Claude Code MCP config outranks both and the app does
 * not see it.
 */
interface RepeatReadsSetting {
    on: boolean;
    /** Where `on` comes from: no file at all (the default is on), or the file. */
    source: 'default' | 'file';
    /** Why the file could not be used, when it could not. `on` is false then. */
    problem: string | null;
    /** The settings file's path, for the text that names it. */
    file: string;
}

/**
 * Whether the identity hook is registered, from `lumen_identity_hook_status`.
 *
 * The hook is what lets the server tell which conversation a call belongs to, and repeat reads
 * answer in full until it is there. Setup registers it, but Setup does not run again for an
 * install that set Lumen up under 1.7.0 or earlier.
 */
interface IdentityHookStatus {
    /** Claude Code's settings run Lumen's hook before all three Lumen tools. */
    registered: boolean;
    /**
     * Not in the settings file, but a recent call to a Lumen tool was identified all the same:
     * a plugin install registers its hooks from the plugin. Nothing is missing then.
     */
    servedElsewhere: boolean;
}

export type {
    Turn,
    SessionState,
    SessionMap,
    CostTotals,
    DaemonMsg,
    SnapshotSession,
    TokenAgg,
    ModelAgg,
    SnapshotModel,
    UsageReport,
    ChannelBreakdown,
    ToolBreakdown,
    OptimizerReport,
    RepeatReads,
    RepeatReadsSetting,
    IdentityHookStatus,
};

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
    /** Share of every token read, in every project the ledger holds. */
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
