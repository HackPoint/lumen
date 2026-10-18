
CREATE TABLE IF NOT EXISTS turns (
    message_id                  TEXT PRIMARY KEY,
    session_id                  TEXT NOT NULL,
    ts                          TEXT NOT NULL,
    model                       TEXT,
    input_tokens                INTEGER NOT NULL,
    output_tokens               INTEGER NOT NULL,
    cache_read_input_tokens     INTEGER NOT NULL,
    cache_creation_input_tokens INTEGER NOT NULL,
    cache_1h                    INTEGER,
    cache_5m                    INTEGER,
    web_search_requests         INTEGER,
    web_fetch_requests          INTEGER,
    service_tier                TEXT,
    source_file                 TEXT,
    -- 1 when the turn came from a subagent transcript. Counted in cost, excluded
    -- from the context-fill gauge (a subagent's context is not the session's).
    is_subagent                 INTEGER NOT NULL DEFAULT 0,
    -- 1.9.0: usage.speed, standard | fast | NULL when not recorded. Last, where the ALTER
    -- in MIGRATIONS puts it on an older ledger, so the two agree on column order.
    speed                       TEXT,
    -- 1.9.0: usage.inference_geo, us | global | not_available | NULL when not recorded.
    -- After speed, in the order the ALTERs in MIGRATIONS add them.
    inference_geo               TEXT
);

CREATE TABLE IF NOT EXISTS calibration (
    message_id      TEXT PRIMARY KEY,
    real_output     INTEGER NOT NULL,
    est_output      INTEGER NOT NULL
);

CREATE VIEW IF NOT EXISTS correction_factor AS
SELECT CAST(SUM(real_output) AS FLOAT) / NULLIF(SUM(est_output), 0) AS factor
FROM calibration;

CREATE INDEX IF NOT EXISTS idx_turns_session ON turns(session_id);
CREATE INDEX IF NOT EXISTS idx_turns_ts      ON turns(ts);

-- Optimizer metering: one row per Read / lumen-tool call.
-- Additive migration: IF NOT EXISTS keeps it idempotent alongside existing tables.
CREATE TABLE IF NOT EXISTS read_events (
    ts              TEXT NOT NULL,
    tool            TEXT NOT NULL,
    path            TEXT NOT NULL,
    lines           INTEGER,
    tokens_returned INTEGER NOT NULL,
    full_tokens     INTEGER NOT NULL,
    saved_tokens    INTEGER NOT NULL,
    routed_via      TEXT NOT NULL,  -- builtin_read | smart_read | recall_file | compress_logs
    channel         TEXT NOT NULL DEFAULT 'unknown',  -- cli | vscode | unknown
    -- Declared in the order the ALTERs in MIGRATIONS add them, so a fresh database
    -- and a migrated one agree on column order as well as on the column set.
    session_id      TEXT,
    file_mtime      INTEGER,
    req_key         TEXT,
    writer_hook     TEXT,
    token_source    TEXT,             -- measured | estimated | unsupported | NULL
    -- 1.3.0 ranked-outline decision. NULL on every row not produced by that path,
    -- including all hook-written rows.
    budget          INTEGER,
    s_min           INTEGER,
    econ_context    REAL,
    econ_rounds     REAL,
    econ_output     REAL,
    econ_source     TEXT,             -- observed | measured_defaults
    k_selected      INTEGER,
    n_total         INTEGER,
    coeff_version   INTEGER,
    target_outline  INTEGER,
    -- 1.7.0 repeat reads. NULL while repeat reads are off.
    repeat_arm      TEXT,             -- treatment | control
    repeat_plan     TEXT,
    -- 1.8.0 conversation identity. The subagent that made the call; NULL for the main agent.
    agent_id        TEXT
);
CREATE INDEX IF NOT EXISTS idx_read_events_ts ON read_events(ts);

-- Compactions and rewinds seen in a session's transcript (kind: compaction | rewind).
-- The daemon re-reads transcripts from the start after a restart, so a row is keyed by the
-- transcript record that caused it and written with INSERT OR IGNORE. Its index is in
-- MIGRATIONS, not here.
CREATE TABLE IF NOT EXISTS session_events (
    session_id  TEXT NOT NULL,
    kind        TEXT NOT NULL,
    uuid        TEXT NOT NULL,
    ts_ms       INTEGER NOT NULL,    -- the record's own timestamp, unix ms
    source_file TEXT,
    PRIMARY KEY (session_id, kind, uuid)
);

-- One row per fault occurrence; `lumen report` aggregates count/first/last at read
-- time, the way it already does for read_events. Writers never come here directly:
-- everything appends to the JSONL spool (see lumen_core::faults) and `lumen report`
-- drains it, so no hook on a blocking path ever waits on this lock.
CREATE TABLE IF NOT EXISTS faults (
    ts         TEXT NOT NULL,
    kind       TEXT NOT NULL,
    -- Sub-kind: a decline route, or which fail-open guard fired. '-' when the kind
    -- has none, matching the sentinel the renderer fingerprints on.
    variant    TEXT NOT NULL DEFAULT '-',
    path       TEXT,
    lines      INTEGER,
    detail     TEXT,
    session_id TEXT,
    -- Version that produced the fault, so a fixed regression stops matching.
    version    TEXT,
    channel    TEXT NOT NULL DEFAULT 'unknown',
    -- 1.9.0: the subagent the hook event belonged to; NULL for the main agent and before.
    -- Declared where the ALTER in MIGRATIONS adds it on an older database.
    agent_id   TEXT
);
CREATE INDEX IF NOT EXISTS idx_faults_kind ON faults(kind, variant);

-- 1.9.0: Reads the intercept blocked, as the transcript of the agent that tried each records it
-- (the hook writes no row for a block). Keyed by the Read's tool_use id and written with INSERT OR
-- IGNORE: the daemon re-reads every transcript from its first line on start. Its index is in
-- MIGRATIONS, not here.
CREATE TABLE IF NOT EXISTS intercept_blocks (
    tool_use_id TEXT PRIMARY KEY,
    session_id  TEXT NOT NULL,
    agent_id    TEXT,              -- the subagent whose transcript holds it; NULL for the main agent
    path        TEXT NOT NULL,     -- as the hook named it, which is what its faults name
    lines       INTEGER,
    kind        TEXT NOT NULL,     -- redirect | log | served (blocked again after a Lumen answer)
    ts_ms       INTEGER NOT NULL,  -- the tool_result record's own timestamp, unix ms
    uuid        TEXT NOT NULL,     -- the tool_result record's uuid
    source_file TEXT
);

-- 1.9.0: transcripts the daemon has read for blocks from their first line (by matcher, the
-- version of lumen_core::intercept that read them): one row per file and session in it, through
-- that session's latest turn read there.
CREATE TABLE IF NOT EXISTS transcript_scans (
    source_file TEXT NOT NULL,
    session_id  TEXT NOT NULL,
    agent_id    TEXT,
    matcher     INTEGER NOT NULL,
    through_ms  INTEGER NOT NULL,
    PRIMARY KEY (source_file, session_id)
);
-- idx_read_events_dedup is created in MIGRATIONS, not here. DDL runs as one batch
-- against a database that may predate E7, where session_id does not yet exist —
-- the index would fail, and because init_schema propagates that with `?`, the
-- whole migration would abort before a single ALTER ran. That is the 1.1.3 defect
-- with a new trigger, and it would have taken the daemon down with it.
