//! With `LUMEN_DB` unset the daemon must resolve an absolute, canonical ledger — never
//! a path relative to wherever it happened to be started.
//!
//! The old fallback was the literal string "lumen.db". A daemon launched from a
//! checkout therefore created a second database beside the source, and both files then
//! accumulated real events: 4,140 rows in one, 195 in the other, 146 of them the same
//! event recorded twice. Reconciling that cost a full detour, so the fallback is now
//! the same `meter::resolve_db_path` every other writer uses.
//!
//! Assertions are made against the path the daemon *reports*, parsed from its own
//! startup log, so the test does not have to recompute the answer.
//!
//! No test here may open the real ledger. That constrains how the fallback can be
//! exercised: reaching it means letting the daemon resolve a home directory, and the
//! only way to redirect that is `HOME`, which `dirs::home_dir()` honours on Unix only —
//! on Windows it queries the shell for the profile and ignores the environment. So the
//! fallback test is Unix-gated rather than run everywhere against the developer's own
//! database, and the precedence test below, which needs no home at all, covers every
//! platform.

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// The daemon prints this once, with the path it resolved.
const MARKER: &str = "lumen-daemon using db: ";

/// Read the daemon's stderr until it reports its database path.
fn resolved_db_path(child: &mut Child) -> Result<String, String> {
    let reader = BufReader::new(child.stderr.take().expect("piped stderr"));
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut log = Vec::new();
    for line in reader.lines() {
        let Ok(line) = line else { break };
        log.push(line.clone());
        if let Some((_, rest)) = line.split_once(MARKER) {
            return Ok(rest.trim().to_string());
        }
        if Instant::now() > deadline {
            break;
        }
    }
    Err(log.join("\n"))
}

fn spawn(dir: &std::path::Path, envs: &[(&str, &std::ffi::OsStr)]) -> Child {
    let projects = dir.join("projects");
    std::fs::create_dir_all(&projects).unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_lumen-daemon"));
    cmd.env_remove("LUMEN_DB")
        .env("LUMEN_PROJECTS_DIR", &projects)
        .env("LUMEN_WS_ADDR", "127.0.0.1:0")
        .env("LUMEN_SUPERVISED", "1")
        // Started somewhere else entirely, which is what used to decide the answer.
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    for (k, v) in envs {
        cmd.env(k, v);
    }
    cmd.spawn().expect("spawn lumen-daemon")
}

fn shut_down(child: &mut Child) {
    drop(child.stdin.take());
    let _ = child.kill();
    let _ = child.wait();
}

/// `LUMEN_DB` wins, on every platform. Proves the daemon resolves through
/// `meter::resolve_db_path` rather than carrying its own convention.
#[test]
fn an_explicit_lumen_db_is_used_verbatim() {
    let dir = tempfile::TempDir::new().unwrap();
    let want = dir.path().join("explicit.db");
    let mut child = spawn(dir.path(), &[("LUMEN_DB", want.as_os_str())]);
    let got = resolved_db_path(&mut child);
    shut_down(&mut child);

    let got = got.unwrap_or_else(|log| panic!("no path reported.\nstderr:\n{log}"));
    assert_eq!(
        std::path::Path::new(&got),
        want,
        "an explicit LUMEN_DB must be used exactly as given"
    );
}

/// The fallback itself. Unix only — see the module note on `HOME`.
#[cfg(unix)]
#[test]
fn with_no_override_the_daemon_resolves_an_absolute_ledger_not_a_relative_one() {
    let home = tempfile::TempDir::new().unwrap();
    let cwd = tempfile::TempDir::new().unwrap();

    let mut child = spawn(cwd.path(), &[("HOME", home.path().as_os_str())]);
    let got = resolved_db_path(&mut child);
    shut_down(&mut child);

    let got = got.unwrap_or_else(|log| panic!("no path reported.\nstderr:\n{log}"));
    let p = std::path::Path::new(&got);

    // Isolation is asserted, not assumed: if HOME were ignored the daemon would have
    // opened the developer's real ledger, and this test must fail rather than pass
    // quietly having done so.
    assert!(
        p.starts_with(home.path()),
        "the daemon resolved {got:?}, which is outside the injected home {} — it may \
         have opened the real ledger",
        home.path().display()
    );
    assert!(
        p.is_absolute(),
        "a relative path is how the ledger split in two"
    );
    assert_ne!(
        p,
        cwd.path().join("lumen.db"),
        "the daemon resolved the ledger relative to its working directory"
    );
    assert!(
        !cwd.path().join("lumen.db").exists(),
        "a lumen.db appeared in the working directory — this is the split-ledger bug"
    );
    assert_eq!(
        p,
        lumen_core::meter::canonical_db_path_in(home.path()),
        "and it is the canonical location every other writer uses"
    );
}
