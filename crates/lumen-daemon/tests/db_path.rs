//! With `LUMEN_DB` unset the daemon must land on the canonical ledger, never on a
//! path relative to wherever it happened to be started.
//!
//! The old fallback was the literal string "lumen.db". A daemon launched from a
//! checkout therefore created a second database beside the source, and both files
//! then accumulated real events: 4,140 rows in one, 195 in the other, 146 of them the
//! same event recorded twice. Reconciling that cost a full detour, so the fallback is
//! now the same `meter::resolve_db_path` every other writer uses.

use std::process::{Command, Stdio};
use std::time::Duration;

#[test]
fn with_no_override_the_daemon_writes_the_canonical_ledger_not_a_relative_one() {
    let home = tempfile::TempDir::new().unwrap();
    let cwd = tempfile::TempDir::new().unwrap();
    let projects = home.path().join("projects");
    std::fs::create_dir_all(&projects).unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_lumen-daemon"))
        // No LUMEN_DB. That is the whole point of the test.
        .env_remove("LUMEN_DB")
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .env("LUMEN_PROJECTS_DIR", &projects)
        .env("LUMEN_WS_ADDR", "127.0.0.1:0")
        .env("LUMEN_SUPERVISED", "1")
        // Started somewhere else entirely, which is what used to decide the answer.
        .current_dir(cwd.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn lumen-daemon");

    std::thread::sleep(Duration::from_millis(2000));
    let running = child.try_wait().unwrap().is_none();
    drop(child.stdin.take());
    let _ = child.kill();
    let _ = child.wait();
    assert!(running, "the daemon exited instead of resolving a path");

    let canonical = lumen_core::meter::canonical_db_path_in(home.path());
    assert!(
        canonical.exists(),
        "expected the canonical ledger at {}",
        canonical.display()
    );
    assert!(
        !cwd.path().join("lumen.db").exists(),
        "a lumen.db appeared in the working directory — this is the split-ledger bug"
    );
}
