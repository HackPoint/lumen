-- The ledger as Lumen 1.5.0 left it: its DDL with every one of its migrations applied,
-- generated from crates/lumen-daemon/tests/upgrade_matrix/schema_v1_5_0.rs. 1.5.0 is the
-- first release whose ledger has a faults table.

PRAGMA journal_mode=WAL;

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
    is_subagent                 INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS sessions (
    session_id  TEXT PRIMARY KEY,
    cwd         TEXT,
    model       TEXT,
    started_at  TEXT,
    source_file TEXT
);

CREATE TABLE IF NOT EXISTS calibration (
    message_id      TEXT PRIMARY KEY,
    real_output     INTEGER NOT NULL,
    est_output      INTEGER NOT NULL
);

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
    is_subagent     INTEGER NOT NULL DEFAULT 0,
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
    target_outline  INTEGER
);

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
    channel    TEXT NOT NULL DEFAULT 'unknown'
);

CREATE VIEW IF NOT EXISTS correction_factor AS
SELECT CAST(SUM(real_output) AS FLOAT) / NULLIF(SUM(est_output), 0) AS factor
FROM calibration;

CREATE INDEX IF NOT EXISTS idx_turns_session ON turns(session_id);

CREATE INDEX IF NOT EXISTS idx_turns_ts      ON turns(ts);

CREATE INDEX IF NOT EXISTS idx_read_events_ts ON read_events(ts);

CREATE INDEX IF NOT EXISTS idx_faults_kind ON faults(kind, variant);

CREATE INDEX IF NOT EXISTS idx_read_events_dedup ON read_events(session_id, path, file_mtime);
