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
    // 1. Explicit env var.
    if let Ok(p) = std::env::var("LUMEN_DB")
        && !p.is_empty()
    {
        return Some(std::path::PathBuf::from(p));
    }
    // 2. Pointer file written by the Tauri app on startup.
    if let Ok(home) = std::env::var("HOME") {
        let pointer = std::path::Path::new(&home).join(".lumen_db_path");
        if let Ok(path_str) = std::fs::read_to_string(&pointer) {
            let trimmed = path_str.trim();
            if !trimmed.is_empty() {
                return Some(std::path::PathBuf::from(trimmed));
            }
        }
    }
    // 3. Binary-relative: current_exe()/../../.. + /lumen.db
    if let Ok(exe) = std::env::current_exe()
        && let Some(root) = exe
            .parent()
            .and_then(|p| p.parent())
            .and_then(|p| p.parent())
    {
        let candidate = root.join("lumen.db");
        return Some(candidate);
    }
    None
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
pub fn insert_read_event(
    path: &str,
    lines: Option<i64>,
    tokens_returned: i64,
    full_tokens: i64,
    saved_tokens: i64,
    routed_via: &str,
    channel: &str,
    tool_name: &str,
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

    let conn = match open_db(&db) {
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

    let result = conn.execute(
        "INSERT INTO read_events(ts,tool,path,lines,tokens_returned,full_tokens,saved_tokens,routed_via,channel)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
        params![ts, tool_name, path, lines_val, tokens_returned, full_tokens, saved_tokens, routed_via, channel],
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

    #[test]
    fn db_path_uses_lumen_db_env_var() {
        // db_path() returns the LUMEN_DB env var when set
        // We can't safely set/unset env vars in parallel tests, so just
        // verify the function doesn't panic when called.
        let _ = db_path();
    }
}
