//! The daemon must not outlive the app that spawned it.
//!
//! When it does, it stays bound to 127.0.0.1:9999 after being reparented to launchd.
//! The next app launch cannot bind, its own daemon spins in the restart loop, and the
//! GUI keeps reading from the previous build's daemon — verified in the field after a
//! 1.1.4 -> 1.2.0 upgrade, where the port holder was the *old* binary (different
//! inode, 0 TCP fds on the new one) and nothing reported a problem.
//!
//! Both tests point the daemon at temp directories. Nothing here may touch the real
//! ledger under ~/Library/Application Support, and nothing may bind the real port —
//! a developer's running daemon must survive `cargo test`.

use std::io::Write;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// How long to allow for shutdown. The watchdog reacts as soon as the read returns,
/// so this is generous by two orders of magnitude.
const GRACE: Duration = Duration::from_secs(10);

struct Fixture {
    _dir: tempfile::TempDir,
    child: Child,
}

fn spawn(supervised: bool) -> Fixture {
    let dir = tempfile::TempDir::new().unwrap();
    let db = dir.path().join("ledger.db");
    let projects = dir.path().join("projects");
    std::fs::create_dir_all(&projects).unwrap();

    // Guard the isolation rather than trusting it: a regression that ignored these
    // env vars would otherwise quietly write to the developer's real database.
    assert!(
        db.starts_with(std::env::temp_dir()),
        "the test database must live under the temp dir, got {}",
        db.display()
    );

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_lumen-daemon"));
    cmd.env("LUMEN_DB", &db)
        .env("LUMEN_PROJECTS_DIR", &projects)
        // Port 0 so the OS assigns an unused one; binding 9999 would fight a daemon
        // the developer is running.
        .env("LUMEN_WS_ADDR", "127.0.0.1:0")
        .env_remove("LUMEN_SUPERVISED")
        .stdin(Stdio::piped())
        // Piped, not null. This mirrors the app, which reads the daemon's stderr to
        // forward it into its own log, and it is load-bearing: with `Stdio::null()`
        // every write to stderr succeeds, so an earlier version of this test passed
        // while the shipped daemon did not shut down at all. The watchdog logged
        // before exiting, `eprintln!` panicked on the broken pipe once the supervisor
        // was gone, the panic unwound only the watchdog thread, and the orphan lived.
        // Against /dev/null that failure mode cannot occur.
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if supervised {
        cmd.env("LUMEN_SUPERVISED", "1");
    }

    let mut child = cmd.spawn().expect("spawn lumen-daemon");

    // Let it get past startup, so an exit below is attributable to the closed pipe
    // and not to the daemon never having run.
    std::thread::sleep(Duration::from_millis(1500));
    assert!(
        child.try_wait().unwrap().is_none(),
        "the daemon exited during startup; the rest of this test would be vacuous"
    );

    Fixture { _dir: dir, child }
}

/// Simulate the supervisor dying: close every pipe it owned.
///
/// Order matters and mirrors a real death. The app holds the write end of the
/// daemon's stdin *and* the read end of its stdout/stderr, and a killed process loses
/// all of them at once — so the daemon faces EOF on stdin and a broken pipe on stderr
/// simultaneously. Dropping only stdin would leave the daemon able to log, which is a
/// strictly easier situation than production and is precisely the gap that let a
/// broken watchdog pass its own test.
///
/// These are dropped only after startup, because while the app is alive it *does* read
/// the daemon's stderr; breaking that pipe from the beginning would have the daemon
/// panicking on its ordinary startup logging instead of on shutdown.
fn supervisor_dies(child: &mut Child) {
    drop(child.stdin.take().expect("piped stdin"));
    drop(child.stdout.take());
    drop(child.stderr.take());
}

/// Wait for exit, returning whether it happened inside `GRACE`.
fn exited_within_grace(child: &mut Child) -> bool {
    let deadline = Instant::now() + GRACE;
    while Instant::now() < deadline {
        if child.try_wait().unwrap().is_some() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

#[test]
fn a_supervised_daemon_exits_when_its_stdin_closes() {
    let mut f = spawn(true);

    supervisor_dies(&mut f.child);

    let exited = exited_within_grace(&mut f.child);
    if !exited {
        let _ = f.child.kill();
    }
    assert!(
        exited,
        "a supervised daemon must exit when its supervisor's pipe closes, or an \
         orphan keeps holding the port across every upgrade"
    );
}

/// Negative control. Without `LUMEN_SUPERVISED` there is no supervisor to follow, and
/// a bare `lumen-daemon < /dev/null &` must keep running. This is also what proves
/// the test above measures the watchdog: if the daemon simply exited on a closed
/// stdin regardless, this test would fail.
#[test]
fn an_unsupervised_daemon_ignores_a_closed_stdin() {
    let mut f = spawn(false);

    supervisor_dies(&mut f.child);

    let exited = exited_within_grace(&mut f.child);
    let _ = f.child.kill();
    let _ = f.child.wait();
    assert!(
        !exited,
        "an unsupervised daemon must survive a closed stdin; exiting would break \
         running the daemon by hand or from a service manager"
    );
}

/// A supervisor that writes something must not be mistaken for one that died. Only
/// EOF ends the daemon.
#[test]
fn traffic_on_stdin_does_not_end_a_supervised_daemon() {
    let mut f = spawn(true);

    {
        let stdin = f.child.stdin.as_mut().expect("piped stdin");
        stdin.write_all(b"ping\n").unwrap();
        stdin.flush().unwrap();
    }
    std::thread::sleep(Duration::from_secs(2));
    assert!(
        f.child.try_wait().unwrap().is_none(),
        "bytes on stdin are not EOF and must not trigger shutdown"
    );
    supervisor_dies(&mut f.child);

    let exited = exited_within_grace(&mut f.child);
    if !exited {
        let _ = f.child.kill();
    }
    assert!(exited, "EOF after traffic must still shut the daemon down");
}
