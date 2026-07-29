use crate::schema::{DDL, MIGRATIONS};
use rusqlite::{Connection, params};

/// Detect which Claude Code channel spawned this process.
///
/// Signal: CLAUDE_CODE_ENTRYPOINT env var set by the host process.
///   "claude-vscode"          → VS Code extension (hooks don't fire)
///   "cli" | "sdk-cli"        → Terminal CLI (hooks fire)
///   anything else non-empty  → treat as cli (future entrypoint names)
///   unset                    → fall back to VSCODE_PID / VSCODE_CWD presence
pub fn detect_channel() -> &'static str {
    let ep = std::env::var("CLAUDE_CODE_ENTRYPOINT").unwrap_or_default();
    match ep.as_str() {
        "claude-vscode" => "vscode",
        s if s.contains("vscode") => "vscode",
        s if !s.is_empty() => "cli",
        _ => {
            if std::env::var("VSCODE_PID").is_ok() || std::env::var("VSCODE_CWD").is_ok() {
                "vscode"
            } else {
                "unknown"
            }
        }
    }
}

/// Resolve the LUMEN_DB path.
///
/// Priority:
///   1. LUMEN_DB env var (explicit override — set in .mcp.json or by the daemon)
///   2. ~/.lumen_db_path pointer file written by the Tauri app on startup.
///      Allows lumen-mcp to find the same DB without hardcoding paths.
///   3. Binary-relative: current_exe()/../../.. + /lumen.db
///      (covers target/release/lumen-mcp → workspace root for dev use)
///
/// Returns None if none of the above resolves.
pub fn db_path() -> Option<std::path::PathBuf> {
    // HOME on Unix, USERPROFILE on Windows — the pointer file is written to
    // whichever the platform actually uses, so both must be consulted.
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .ok();
    resolve_db_path(std::env::var("LUMEN_DB").ok().as_deref(), home.as_deref())
}

/// Bundle identifier. Must match `identifier` in tauri.conf.json — the GUI, the
/// daemon and the hooks all have to agree on one directory or they meter into
/// different ledgers.
pub const APP_ID: &str = "io.speedata.lumen";

/// The per-OS application data directory.
///
/// Lives here, not in the GUI crate, because every writer needs it: two copies of
/// this logic drifting apart is the same class of bug as the split ledger it
/// prevents. `lumenator`'s `app_support_dir_in` delegates to this.
pub fn app_data_dir_in(home: &std::path::Path) -> std::path::PathBuf {
    #[cfg(target_os = "macos")]
    {
        home.join("Library/Application Support").join(APP_ID)
    }
    #[cfg(target_os = "windows")]
    {
        home.join("AppData").join("Roaming").join(APP_ID)
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        home.join(".local").join("share").join(APP_ID)
    }
}

/// The one database every component should use when nothing overrides it.
pub fn canonical_db_path_in(home: &std::path::Path) -> std::path::PathBuf {
    app_data_dir_in(home).join("lumen.db")
}

/// The precedence policy behind [`db_path`], with the environment passed in.
///
/// Separated so the ordering can be tested directly: mutating LUMEN_DB / HOME in
/// a test is racy across threads and `unsafe` in edition 2024.
pub fn resolve_db_path(lumen_db: Option<&str>, home: Option<&str>) -> Option<std::path::PathBuf> {
    // 1. Explicit env var.
    if let Some(p) = lumen_db
        && !p.is_empty()
    {
        return Some(std::path::PathBuf::from(p));
    }
    // 2. Pointer file written by the Tauri app on startup.
    if let Some(home) = home {
        let pointer = std::path::Path::new(home).join(".lumen_db_path");
        if let Ok(path_str) = std::fs::read_to_string(&pointer) {
            let trimmed = path_str.trim();
            if !trimmed.is_empty() {
                return Some(std::path::PathBuf::from(trimmed));
            }
        }
    }
    // 3. The canonical per-OS location.
    //
    // This used to be current_exe()/../../.. + lumen.db, and that is how the
    // ledger split in two: for a development build at target/release/lumen-mcp,
    // walking up three parents lands on the repository root, so lumen-mcp running
    // without LUMEN_DB wrote to <repo>/lumen.db while everything else wrote to the
    // application data directory. 195 events accumulated in the shadow ledger, 146
    // of them duplicating rows in the real one, and nothing reported a problem —
    // both writes succeeded, they simply went to different files.
    //
    // Resolving to the canonical path instead means a missing LUMEN_DB is no longer
    // a fork in the road. If the home directory cannot be resolved we return None,
    // and the caller logs and skips: losing a row is recoverable, silently writing
    // it somewhere nobody reads is not.
    home.map(|h| canonical_db_path_in(std::path::Path::new(h)))
}

/// Open (or create) the lumen SQLite DB, apply DDL + additive migrations.
fn open_db(path: &std::path::Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open(path)?;
    conn.execute_batch(DDL)?;
    for migration in MIGRATIONS {
        // Ignore "duplicate column" errors — migration already applied.
        let _ = conn.execute_batch(migration);
    }
    Ok(conn)
}

/// Public entry point for external crates (e.g. lumen-cli) that need a DB connection.
pub fn connect_db(path: &std::path::Path) -> rusqlite::Result<Connection> {
    open_db(path)
}

#[allow(clippy::too_many_arguments)]
/// Provenance of a ranked-outline decision, recorded alongside the row.
///
/// All-NULL by default, which is what every other writer produces: the hook and the
/// non-ranked path have no decision to record, and a zero would claim they did.
///
/// Every input is stored, not just the budget, because `S_min` is derived from measured
/// means that will be re-derived once per-call `(R, round cost)` pairs exist. A row
/// carrying only its budget could not be compared against one scored under different
/// coefficients, which is exactly what the A/B has to do.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RankedMeta {
    pub budget: Option<i64>,
    pub s_min: Option<i64>,
    pub econ_context: Option<f64>,
    pub econ_rounds: Option<f64>,
    pub econ_output: Option<f64>,
    pub econ_source: Option<String>,
    /// Definitions included. Equal to `n_total` means the budget never bound and the
    /// ranking had no effect — the distinction between a gate and a trimmer.
    pub k_selected: Option<i64>,
    pub n_total: Option<i64>,
    pub coeff_version: Option<i64>,
    /// What the outline was aiming to cost. Swept downward during the A/B, so a row
    /// without it cannot be placed on the follow-up-rate curve.
    pub target_outline: Option<i64>,
}

/// Thirteen parameters, and the alternative is worse.
///
/// Nine of the columns are already collapsed into [`RankedMeta`]; the rest are the
/// event's own identity. Wrapping those in a struct too would move the argument list to
/// the call site without removing anything, and this has exactly one production caller.
#[allow(clippy::too_many_arguments)]
pub fn insert_read_event(
    path: &str,
    lines: Option<i64>,
    tokens_returned: i64,
    full_tokens: i64,
    saved_tokens: i64,
    routed_via: &str,
    channel: &str,
    tool_name: &str,
    session_id: Option<&str>,
    file_mtime: Option<i64>,
    req_key: Option<&str>,
    meta: &RankedMeta,
) {
    let db = match db_path() {
        Some(p) => p,
        None => {
            eprintln!(
                "lumen-meter: LUMEN_DB not set and binary path resolution failed — skipping DB write"
            );
            return;
        }
    };
    insert_read_event_at(
        &db,
        path,
        lines,
        tokens_returned,
        full_tokens,
        saved_tokens,
        routed_via,
        channel,
        tool_name,
        session_id,
        file_mtime,
        req_key,
        meta,
    );
}

/// Write one `read_events` row to the DB at `db`. Split out from
/// [`insert_read_event`] so tests can target a tempdir instead of the ambient
/// LUMEN_DB. Failures are logged and swallowed: metering must never break a tool
/// call that has already answered the client.
#[allow(clippy::too_many_arguments)]
pub fn insert_read_event_at(
    db: &std::path::Path,
    path: &str,
    lines: Option<i64>,
    tokens_returned: i64,
    full_tokens: i64,
    saved_tokens: i64,
    routed_via: &str,
    channel: &str,
    tool_name: &str,
    session_id: Option<&str>,
    file_mtime: Option<i64>,
    req_key: Option<&str>,
    meta: &RankedMeta,
) {
    let conn = match open_db(db) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("lumen-meter: failed to open DB {db:?}: {e}");
            return;
        }
    };

    let ts = {
        use std::time::{SystemTime, UNIX_EPOCH};
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        // Format as ISO-8601 UTC without chrono dependency.
        let s = secs;
        let sec = s % 60;
        let min = (s / 60) % 60;
        let hr = (s / 3600) % 24;
        let days = s / 86400;
        // Days since 1970-01-01 → Gregorian date (accurate for ~200 years)
        let (y, mo, d) = days_to_ymd(days);
        format!("{y:04}-{mo:02}-{d:02}T{hr:02}:{min:02}:{sec:02}Z")
    };

    let lines_val: Option<i64> = lines;

    // token_source is always 'measured' here: this crate tokenizes in-process with
    // no fallback path, unlike the shell hook which can substitute bytes/4. Recording
    // it explicitly is what lets the UI stop qualifying its accuracy claim — a NULL
    // would count as unverified forever and the warning would never clear.
    let result = conn.execute(
        "INSERT INTO read_events(ts,tool,path,lines,tokens_returned,full_tokens,\
         saved_tokens,routed_via,channel,session_id,file_mtime,req_key,is_subagent,\
         writer_hook,token_source,budget,s_min,econ_context,econ_rounds,econ_output,\
         econ_source,k_selected,n_total,coeff_version,target_outline) \
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,0,'lumen-mcp','measured',\
                ?13,?14,?15,?16,?17,?18,?19,?20,?21,?22)",
        params![
            ts,
            tool_name,
            path,
            lines_val,
            tokens_returned,
            full_tokens,
            saved_tokens,
            routed_via,
            channel,
            session_id,
            file_mtime,
            req_key,
            meta.budget,
            meta.s_min,
            meta.econ_context,
            meta.econ_rounds,
            meta.econ_output,
            meta.econ_source.as_deref(),
            meta.k_selected,
            meta.n_total,
            meta.coeff_version,
            meta.target_outline,
        ],
    );

    if let Err(e) = result {
        eprintln!("lumen-meter: INSERT failed: {e}");
    }
}

/// Convert days since Unix epoch to (year, month, day) in the proleptic Gregorian calendar.
fn days_to_ymd(days: u64) -> (u32, u32, u32) {
    // Algorithm from https://howardhinnant.github.io/date_algorithms.html
    let z = days as i64 + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y as u32, m as u32, d as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn epoch_day_zero_is_1970_01_01() {
        assert_eq!(days_to_ymd(0), (1970, 1, 1));
    }

    #[test]
    fn epoch_day_one_is_1970_01_02() {
        assert_eq!(days_to_ymd(1), (1970, 1, 2));
    }

    #[test]
    fn first_day_of_1971() {
        // 1970 is not a leap year: 365 days
        assert_eq!(days_to_ymd(365), (1971, 1, 1));
    }

    #[test]
    fn first_day_of_2000() {
        // 1970..=1999: 7 leap years (72,76,80,84,88,92,96), 23 non-leap
        // 7*366 + 23*365 = 2562 + 8395 = 10957
        assert_eq!(days_to_ymd(10957), (2000, 1, 1));
    }

    #[test]
    fn first_day_of_2024() {
        // 1970..=2023: 13 leap years (72..20), 41 non-leap
        // 13*366 + 41*365 = 4758 + 14965 = 19723
        assert_eq!(days_to_ymd(19723), (2024, 1, 1));
    }

    #[test]
    fn leap_day_2024_02_29() {
        // 2024-01-01 = day 19723, then +31 (Jan) + 28 (Feb 1..28) + 1 = +60 - 1 = day 19782
        // Jan: 31 days (days 19723..19753), Feb 1 = 19754, Feb 29 = 19782
        assert_eq!(days_to_ymd(19782), (2024, 2, 29));
    }

    // ── days_to_ymd, remaining calendar edges ────────────────────────────

    #[test]
    fn last_day_of_a_leap_year() {
        // 2024-12-31: 2024-01-01 is day 19723, +365 days into a 366-day year.
        assert_eq!(days_to_ymd(19723 + 365), (2024, 12, 31));
    }

    #[test]
    fn day_after_a_leap_day_is_march_first() {
        assert_eq!(days_to_ymd(19783), (2024, 3, 1));
    }

    #[test]
    fn a_century_non_leap_year_has_no_february_29() {
        // 1900 was NOT a leap year (divisible by 100, not by 400), so 1900-03-01
        // must follow 1900-02-28. Day 0 is 1970-01-01, so 1900 is negative — use
        // 2100, the next century non-leap, reachable with u64 days.
        // 2100-01-01 = 47482 days after the epoch.
        assert_eq!(days_to_ymd(47482), (2100, 1, 1));
        assert_eq!(days_to_ymd(47482 + 58), (2100, 2, 28));
        assert_eq!(
            days_to_ymd(47482 + 59),
            (2100, 3, 1),
            "2100 is not a leap year, so Feb 29 must not exist"
        );
    }

    #[test]
    fn year_2000_was_a_leap_year() {
        // Divisible by 400, so Feb 29 DOES exist.
        assert_eq!(days_to_ymd(10957 + 59), (2000, 2, 29));
    }

    #[test]
    fn every_day_of_a_year_round_trips_in_order() {
        // Walk 2024 day by day: months must be 1..=12, days 1..=31, and the date
        // must strictly increase. Catches any off-by-one in the era arithmetic.
        let mut previous = (0u32, 0u32, 0u32);
        for offset in 0..366 {
            let (y, m, d) = days_to_ymd(19723 + offset);
            assert_eq!(y, 2024);
            assert!((1..=12).contains(&m), "month {m} out of range");
            assert!((1..=31).contains(&d), "day {d} out of range");
            assert!(
                (y, m, d) > previous,
                "date went backwards at offset {offset}"
            );
            previous = (y, m, d);
        }
    }

    // ── resolve_db_path precedence ───────────────────────────────────────────

    #[test]
    fn lumen_db_wins_over_everything_else() {
        let got = resolve_db_path(Some("/explicit/lumen.db"), Some("/some/home"));
        assert_eq!(got, Some(std::path::PathBuf::from("/explicit/lumen.db")));
    }

    #[test]
    fn an_empty_lumen_db_is_ignored_rather_than_used_as_a_path() {
        // An exported-but-blank LUMEN_DB must not resolve to "".
        assert_eq!(resolve_db_path(Some(""), None), None);
    }

    #[test]
    fn the_pointer_file_is_used_when_lumen_db_is_unset() {
        let home = TempDir::new().unwrap();
        std::fs::write(home.path().join(".lumen_db_path"), "/from/pointer.db\n").unwrap();
        let got = resolve_db_path(None, Some(&home.path().to_string_lossy()));
        assert_eq!(
            got,
            Some(std::path::PathBuf::from("/from/pointer.db")),
            "trailing newline must be trimmed"
        );
    }

    #[test]
    fn a_blank_pointer_file_falls_through_to_the_canonical_path() {
        let home = TempDir::new().unwrap();
        std::fs::write(home.path().join(".lumen_db_path"), "   \n\t ").unwrap();
        let got = resolve_db_path(None, Some(&home.path().to_string_lossy()));
        assert_eq!(
            got,
            Some(canonical_db_path_in(home.path())),
            "a whitespace-only pointer is not a path, but the canonical location still is"
        );
    }

    #[test]
    fn a_missing_pointer_file_falls_through_to_the_canonical_path() {
        let home = TempDir::new().unwrap();
        let got = resolve_db_path(None, Some(&home.path().to_string_lossy()));
        assert_eq!(got, Some(canonical_db_path_in(home.path())));
    }

    #[test]
    fn the_fallback_is_never_relative_to_the_executable() {
        // THE SPLIT-LEDGER REGRESSION. The old third step was
        // current_exe()/../../.. + lumen.db, so a development build at
        // target/release/lumen-mcp resolved to <repo>/lumen.db while every other
        // component used the application data directory. Both writes succeeded,
        // to different files, and nothing reported it: 195 events accumulated in
        // the shadow ledger, 146 duplicating rows in the real one.
        let home = TempDir::new().unwrap();
        let got = resolve_db_path(None, Some(&home.path().to_string_lossy())).unwrap();

        assert!(
            got.starts_with(home.path()),
            "the fallback must live under the home directory, got {got:?}"
        );
        assert!(
            got.to_string_lossy().contains(APP_ID),
            "the fallback must be the canonical per-app location, got {got:?}"
        );
        assert_ne!(
            got,
            home.path().join("lumen.db"),
            "a bare <root>/lumen.db is the shadow-ledger shape that caused the split"
        );
    }

    #[test]
    fn every_component_resolves_to_the_same_file_without_an_override() {
        // Divergence is only impossible if the answer is a pure function of the
        // home directory — no cwd, no executable location, no per-crate copy.
        let home = TempDir::new().unwrap();
        let h = home.path().to_string_lossy().to_string();
        let a = resolve_db_path(None, Some(&h));
        let b = resolve_db_path(None, Some(&h));
        assert_eq!(a, b);
        assert_eq!(a, Some(canonical_db_path_in(home.path())));
    }

    #[test]
    fn nothing_resolves_when_the_home_directory_is_unknown() {
        // Loud failure. insert_read_event logs and skips rather than inventing a
        // location, because losing a row is recoverable and writing it somewhere
        // nobody reads is not.
        assert_eq!(resolve_db_path(None, None), None);
    }

    #[test]
    fn db_path_reads_the_live_environment_without_panicking() {
        // The wrapper's only job is to feed the environment into the policy.
        let _ = db_path();
    }

    // ── open_db / connect_db ─────────────────────────────────────────────────

    #[test]
    fn connect_db_creates_the_file_and_the_schema() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("fresh.db");
        assert!(!path.exists());
        let conn = connect_db(&path).expect("connect");
        assert!(path.exists());
        let tables: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' \
                 AND name IN ('turns','sessions','calibration','read_events')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(tables, 4);
    }

    #[test]
    fn connect_db_applies_the_channel_migration() {
        // MIGRATIONS adds read_events.channel; a fresh DB gets it from the DDL,
        // and re-opening must not fail on the duplicate-column error.
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("m.db");
        drop(connect_db(&path).unwrap());
        let conn = connect_db(&path).expect("second open must tolerate the migration");
        let channel: String = conn
            .query_row(
                "SELECT COALESCE((SELECT channel FROM read_events LIMIT 1),'none')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(channel, "none");
    }

    #[test]
    fn connect_db_fails_on_an_unwritable_path() {
        // A path whose parent does not exist cannot be created.
        let err = connect_db(std::path::Path::new("/nonexistent-dir-xyz/sub/lumen.db"));
        assert!(err.is_err());
    }

    // ── insert_read_event_at ─────────────────────────────────────────────────

    fn count_events(conn: &Connection) -> i64 {
        conn.query_row("SELECT COUNT(*) FROM read_events", [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn a_read_event_is_written_with_every_column() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("m.db");
        drop(connect_db(&path).unwrap());

        insert_read_event_at(
            &path,
            "/src/main.rs",
            Some(420),
            100,
            1_000,
            900,
            "smart_read",
            "cli",
            "mcp__lumen__smart_read",
            None,
            None,
            None,
            &RankedMeta::default(),
        );

        let conn = connect_db(&path).unwrap();
        assert_eq!(count_events(&conn), 1);
        let (p, lines, ret, full, saved, via, chan, tool): (
            String,
            Option<i64>,
            i64,
            i64,
            i64,
            String,
            String,
            String,
        ) = conn
            .query_row(
                "SELECT path,lines,tokens_returned,full_tokens,saved_tokens,routed_via,channel,tool
                 FROM read_events",
                [],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                        r.get(7)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(p, "/src/main.rs");
        assert_eq!(lines, Some(420));
        assert_eq!(ret, 100);
        assert_eq!(full, 1_000);
        assert_eq!(saved, 900);
        assert_eq!(via, "smart_read");
        assert_eq!(chan, "cli");
        assert_eq!(tool, "mcp__lumen__smart_read");
    }

    #[test]
    fn a_read_event_accepts_a_null_line_count() {
        // compress_logs on inline text has no line count to report.
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("m.db");
        drop(connect_db(&path).unwrap());
        insert_read_event_at(
            &path,
            "(inline)",
            None,
            10,
            20,
            10,
            "compress_logs",
            "cli",
            "t",
            None,
            None,
            None,
            &RankedMeta::default(),
        );
        let conn = connect_db(&path).unwrap();
        let lines: Option<i64> = conn
            .query_row("SELECT lines FROM read_events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(lines, None);
    }

    #[test]
    fn the_timestamp_is_iso_8601_utc() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("m.db");
        drop(connect_db(&path).unwrap());
        insert_read_event_at(
            &path,
            "/p",
            None,
            1,
            2,
            1,
            "smart_read",
            "cli",
            "t",
            None,
            None,
            None,
            &RankedMeta::default(),
        );

        let conn = connect_db(&path).unwrap();
        let ts: String = conn
            .query_row("SELECT ts FROM read_events", [], |r| r.get(0))
            .unwrap();
        // The aggregate queries compare with datetime(ts), so the shape matters.
        assert_eq!(ts.len(), 20, "expected YYYY-MM-DDTHH:MM:SSZ, got {ts}");
        assert!(ts.ends_with('Z'), "must be marked UTC: {ts}");
        assert_eq!(&ts[4..5], "-");
        assert_eq!(&ts[10..11], "T");
        // And SQLite must be able to parse it.
        let parsed: Option<String> = conn
            .query_row("SELECT datetime(ts) FROM read_events", [], |r| r.get(0))
            .unwrap();
        assert!(parsed.is_some(), "SQLite could not parse {ts}");
    }

    #[test]
    fn several_read_events_accumulate() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("m.db");
        drop(connect_db(&path).unwrap());
        for i in 0..5 {
            insert_read_event_at(
                &path,
                &format!("/p{i}.rs"),
                Some(i),
                1,
                2,
                1,
                "smart_read",
                "cli",
                "t",
                None,
                None,
                None,
                &RankedMeta::default(),
            );
        }
        let conn = connect_db(&path).unwrap();
        assert_eq!(count_events(&conn), 5);
    }

    #[test]
    fn a_failed_write_is_swallowed_rather_than_panicking() {
        // Metering must never break a tool call that already answered the client.
        insert_read_event_at(
            std::path::Path::new("/nonexistent-dir-xyz/sub/lumen.db"),
            "/p",
            None,
            1,
            2,
            1,
            "smart_read",
            "cli",
            "t",
            None,
            None,
            None,
            &RankedMeta::default(),
        );
    }
}
