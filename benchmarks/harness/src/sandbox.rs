//! The sandbox every spawned binary goes through, and the guard that proves it held.
//!
//! A benchmark that spawns a build of Lumen (the current `lumen-mcp`, an old release's daemon,
//! later a competitor's tool) must not read the user's data, write the user's ledger, or be
//! changed by the user's environment. Two things enforce that:
//!
//! - [`Sandbox::apply`] gives a child a scratch home, working directory and ledger and an
//!   environment built from an **allowlist**.
//! - [`RealLedgerGuard`] opens the real ledgers read-only before and after a run and requires
//!   that none holds a row a benchmark session wrote.
//!
//! Why an allowlist and not a blocklist. A blocklist (strip `LUMEN_*` and `CLAUDE*`) is only as
//! good as the day it was written: a proxy variable, an API key, `RUST_LOG`, `NO_COLOR`, a
//! tool's own `*_HOME` or `*_CONFIG` all pass it, and each one can change what a binary does or
//! where it writes, without a word in the results. An allowlist fails the other way: a variable
//! a tool really needs is missing and the tool fails loudly, and the fix is one named line
//! ([`Sandbox::pass`] or [`Sandbox::set`]) that the diff shows. It also removes every inherited
//! `LUMEN_*` and `CLAUDE*` by construction, which a test checks.

use crate::util::{Ctx, Res};
use std::ffi::OsString;
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// Variables a child may inherit: what it needs to find programs and render text, and nothing
/// that names a home, a ledger, a credential or a tool's own configuration.
/// The Windows entries are the ones the loader and the shell need; they point at the system, not
/// at the user.
pub const INHERITED: &[&str] = &[
    "PATH",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "LC_MESSAGES",
    "LC_COLLATE",
    "LC_NUMERIC",
    "LC_TIME",
    "TZ",
    "TERM",
    "SystemRoot",
    "SYSTEMROOT",
    "SystemDrive",
    "windir",
    "WINDIR",
    "COMSPEC",
    "PATHEXT",
    "NUMBER_OF_PROCESSORS",
    "PROCESSOR_ARCHITECTURE",
];

/// Where a run keeps everything it writes, so nothing touches a real ledger, a real home or the
/// real temp directory (the intercept hook's markers live under `temp_dir()`).
#[derive(Clone, Debug)]
pub struct Sandbox {
    pub root: PathBuf,
    pub db: PathBuf,
    pub projects: PathBuf,
    pub tmp: PathBuf,
    /// The scratch `HOME` (and `USERPROFILE`, `APPDATA`, `LOCALAPPDATA`, `XDG_*` beneath it).
    pub home: PathBuf,
    /// The scratch working directory: 1.0.0 writes `lumen.db` to the cwd.
    pub cwd: PathBuf,
    pub spool: PathBuf,
    pub log: PathBuf,
    /// `127.0.0.1:<free port>`, chosen when the sandbox was created.
    pub ws_addr: String,
    /// Added on top of the base environment, in order: a named, reviewable exception list.
    extra: Vec<(String, OsString)>,
    pass: Vec<String>,
}

impl Sandbox {
    /// The directory the ledger is in: where a hook's per-call records go.
    pub fn dir(&self) -> &Path {
        self.db.parent().unwrap_or_else(|| Path::new("."))
    }

    /// Directories under `root`, created. The ledger file is not: the build under test creates it
    /// with its own schema on its first write, so a baseline build never opens a database laid
    /// out by a newer one.
    pub fn create(root: &Path) -> Res<Sandbox> {
        let sb = Sandbox {
            root: root.to_path_buf(),
            db: root.join("ledger.db"),
            projects: root.join("projects"),
            tmp: root.join("tmp"),
            home: root.join("home"),
            cwd: root.join("cwd"),
            spool: root.join("faults.jsonl"),
            log: root.join("lumen.log"),
            ws_addr: format!("127.0.0.1:{}", free_port()?),
            extra: Vec::new(),
            pass: Vec::new(),
        };
        for d in [&sb.projects, &sb.tmp, &sb.home, &sb.cwd] {
            std::fs::create_dir_all(d).ctx(&format!("create {}", d.display()))?;
        }
        for d in [
            sb.home.join("AppData").join("Roaming"),
            sb.home.join("AppData").join("Local"),
            sb.home.join(".config"),
            sb.home.join(".local").join("share"),
            sb.home.join(".local").join("state"),
            sb.home.join(".cache"),
        ] {
            std::fs::create_dir_all(&d).ctx(&format!("create {}", d.display()))?;
        }
        let run = sb.home.join(".run");
        std::fs::create_dir_all(&run).ctx("create the runtime dir")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&run, std::fs::Permissions::from_mode(0o700))
                .ctx("restrict the runtime dir")?;
        }
        Ok(sb)
    }

    /// Also let `name` through from the caller's environment (a tool that needs one named
    /// variable to run at all). Never `LUMEN_*` or `CLAUDE*`: those are refused.
    pub fn pass(mut self, name: &str) -> Sandbox {
        if !is_tainted(name) {
            self.pass.push(name.to_string());
        }
        self
    }

    /// Also set `name` to `value` in every child.
    pub fn set(mut self, name: &str, value: impl Into<OsString>) -> Sandbox {
        self.extra.push((name.to_string(), value.into()));
        self
    }

    /// The whole environment a child gets, given what the caller has. Pure, so a test can hand
    /// it a hostile environment without touching its own.
    pub fn env_for(&self, inherited: &[(OsString, OsString)]) -> Vec<(String, OsString)> {
        let mut out: Vec<(String, OsString)> = Vec::new();
        for (k, v) in inherited {
            let Some(k) = k.to_str() else { continue };
            let allowed = INHERITED.iter().any(|a| a.eq_ignore_ascii_case(k))
                || self.pass.iter().any(|a| a == k);
            if allowed && !is_tainted(k) {
                out.push((k.to_string(), v.clone()));
            }
        }
        let p = |p: &Path| p.as_os_str().to_os_string();
        let appdata = self.home.join("AppData");
        let sandbox: Vec<(&str, OsString)> = vec![
            ("HOME", p(&self.home)),
            ("USERPROFILE", p(&self.home)),
            ("APPDATA", p(&appdata.join("Roaming"))),
            ("LOCALAPPDATA", p(&appdata.join("Local"))),
            ("XDG_CONFIG_HOME", p(&self.home.join(".config"))),
            ("XDG_DATA_HOME", p(&self.home.join(".local").join("share"))),
            ("XDG_STATE_HOME", p(&self.home.join(".local").join("state"))),
            ("XDG_CACHE_HOME", p(&self.home.join(".cache"))),
            ("XDG_RUNTIME_DIR", p(&self.home.join(".run"))),
            ("TMPDIR", p(&self.tmp)),
            ("TMP", p(&self.tmp)),
            ("TEMP", p(&self.tmp)),
            ("LUMEN_DB", p(&self.db)),
            ("LUMEN_PROJECTS_DIR", p(&self.projects)),
            ("LUMEN_FAULT_SPOOL", p(&self.spool)),
            ("LUMEN_LOG", p(&self.log)),
            ("LUMEN_WS_ADDR", OsString::from(&self.ws_addr)),
            // `0` is the only value the update check reads as off.
            ("LUMEN_UPDATE_CHECK", OsString::from("0")),
        ];
        for (k, v) in sandbox {
            set_in(&mut out, k, v);
        }
        for (k, v) in &self.extra {
            set_in(&mut out, k, v.clone());
        }
        out
    }

    /// Make `cmd` a sandboxed child: the environment is replaced by [`Sandbox::env_for`] and the
    /// working directory is the scratch one. Call this first: whatever the caller sets after it
    /// (a session id, a flag under test) stays, and a caller that needs another working
    /// directory sets it after.
    pub fn apply(&self, cmd: &mut Command) {
        let inherited: Vec<(OsString, OsString)> = std::env::vars_os().collect();
        cmd.env_clear();
        for (k, v) in self.env_for(&inherited) {
            cmd.env(k, v);
        }
        cmd.current_dir(&self.cwd);
    }
}

fn set_in(out: &mut Vec<(String, OsString)>, k: &str, v: OsString) {
    out.retain(|(n, _)| !n.eq_ignore_ascii_case(k));
    out.push((k.to_string(), v));
}

/// A variable that is never passed on from the caller: Lumen's own switches and Claude Code's
/// session and settings variables would make two runs measure different things.
fn is_tainted(name: &str) -> bool {
    let up = name.to_ascii_uppercase();
    up.starts_with("LUMEN_") || up.starts_with("CLAUDE")
}

/// A port nothing listens on now, taken by binding port 0 and letting go.
pub fn free_port() -> Res<u16> {
    let l = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).ctx("find a free port")?;
    Ok(l.local_addr().ctx("read the port")?.port())
}

// --- the port 1.0.0's daemon fixes ------------------------------------------------------------

/// The port 1.0.0's daemon binds, whatever the environment says.
pub const OLD_DAEMON_PORT: u16 = 9999;

/// Refuse when something already holds `127.0.0.1:port`. A bind and a connect are both tried: on
/// some systems a second bind to a used port succeeds, and a connect says what is there.
pub fn ensure_port_free(port: u16) -> Res<()> {
    let taken = |why: &str| {
        Err(format!(
            "127.0.0.1:{port} is in use ({why}): quit Lumen first (the app, or any lumen-daemon), then run again. \
             A daemon of 1.0.0 binds this port and cannot be moved, and the running app's window could talk to it."
        ))
    };
    if let Err(e) = TcpListener::bind((Ipv4Addr::LOCALHOST, port)) {
        return taken(&e.to_string());
    }
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    if TcpStream::connect_timeout(&addr, Duration::from_millis(300)).is_ok() {
        return taken("something accepts connections");
    }
    Ok(())
}

/// The preflight before any 1.0.0 daemon is started.
pub fn preflight_old_daemon() -> Res<()> {
    ensure_port_free(OLD_DAEMON_PORT)
}

// --- the real-ledger guard --------------------------------------------------------------------

/// Session-id prefixes the harness's own runs write. `lumen-bench-` is the agreed one; `bench-`
/// is what the token and latency drivers use today. A real session id is a UUID, so neither
/// can match a user's own row.
pub const BENCH_PREFIXES: [&str; 2] = ["lumen-bench-", "bench-"];

/// The data-directory name Lumen uses today, from lumen-core.
const NEXT_APP_ID: &str = lumen_core::meter::APP_ID;

/// The ledgers a run must never write, for a real home: the current data directory's, every
/// Lumen ledger directly under the platform's data root whatever its folder is called (a
/// previous install is found by what it holds, not by its name), and the database
/// `~/.lumen_db_path` points at. Files that do not exist are listed too; [`check_ledger`]
/// skips them and never creates one.
pub fn real_ledger_paths(home: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = Vec::new();
    let mut add = |p: PathBuf| {
        if !v.contains(&p) {
            v.push(p);
        }
    };
    add(lumen_core::meter::canonical_db_path_in(home));
    let dir = lumen_core::meter::app_data_dir_in(home);
    if let Some(root) = dir.parent() {
        add(root.join(NEXT_APP_ID).join("lumen.db"));
        // Any folder one level down that holds a ledger: the install before the identifier
        // changed, under a name this crate never needs to know.
        if let Ok(entries) = std::fs::read_dir(root) {
            let mut found: Vec<PathBuf> = entries
                .flatten()
                .map(|e| e.path().join("lumen.db"))
                .filter(|db| db.is_file())
                .collect();
            found.sort();
            for db in found {
                add(db);
            }
        }
    }
    if let Ok(s) = std::fs::read_to_string(home.join(".lumen_db_path")) {
        let t = s.trim();
        if !t.is_empty() {
            add(PathBuf::from(t));
        }
    }
    v
}

/// How many rows of `db` belong to a benchmark session: across every table with a `session_id`
/// column, the rows whose session starts with one of [`BENCH_PREFIXES`]. `Ok(None)` when there
/// is no file. The database is opened with a `?mode=ro` URI: it cannot be created or written.
pub fn check_ledger(db: &Path) -> Res<Option<i64>> {
    if !db.is_file() {
        return Ok(None);
    }
    let conn = lumen_core::meter::open_read_only(Path::new(&ro_uri(db)))
        .ctx(&format!("open {} read-only", shown(db)))?;
    let tables: Vec<String> = {
        let mut st = conn
            .prepare(
                "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
            )
            .ctx("list the tables")?;
        st.query_map([], |r| r.get::<_, String>(0))
            .ctx("list the tables")?
            .collect::<Result<_, _>>()
            .ctx("list the tables")?
    };
    let mut hits = 0i64;
    for t in tables {
        let q = |s: &str| s.replace('"', "\"\"");
        let has: i64 = conn
            .query_row(
                &format!(
                    "SELECT COUNT(*) FROM pragma_table_info('{}') WHERE name = 'session_id'",
                    t.replace('\'', "''")
                ),
                [],
                |r| r.get(0),
            )
            .ctx(&format!("read the columns of {t}"))?;
        if has == 0 {
            continue;
        }
        for p in BENCH_PREFIXES {
            let n: i64 = conn
                .query_row(
                    &format!(
                        "SELECT COUNT(*) FROM \"{}\" WHERE substr(session_id, 1, {}) = '{p}'",
                        q(&t),
                        p.len()
                    ),
                    [],
                    |r| r.get(0),
                )
                .ctx(&format!("count benchmark rows in {t}"))?;
            hits += n;
        }
    }
    Ok(Some(hits))
}

/// `file:<path>?mode=ro`, the path percent-encoded where SQLite would read it as syntax.
fn ro_uri(db: &Path) -> String {
    let mut s = String::from("file:");
    for c in db.to_string_lossy().chars() {
        match c {
            '%' => s.push_str("%25"),
            '?' => s.push_str("%3f"),
            '#' => s.push_str("%23"),
            c => s.push(c),
        }
    }
    s.push_str("?mode=ro");
    s
}

/// A path as a result or a message may show it: never the user's home.
fn shown(p: &Path) -> String {
    p.file_name()
        .map(|n| format!("the ledger {}", n.to_string_lossy()))
        .unwrap_or_else(|| "a ledger".into())
}

/// The real ledgers, checked before and after a run.
pub struct RealLedgerGuard {
    paths: Vec<PathBuf>,
    /// Failures of an after-check that ran while a panic unwound ([`GuardScope`]'s `Drop`): the
    /// panic carries its own message, so these are printed and kept for whoever can still look.
    drop_failures: std::sync::Mutex<Vec<String>>,
}

/// An open guard: the before-check passed, and the after-check runs when this ends, by
/// [`GuardScope::finish`] or, if the run panics, by `Drop`. A guard whose after-check a panic skips
/// would let a run that wrote a real ledger look like a run that only crashed.
pub struct GuardScope<'a> {
    guard: &'a RealLedgerGuard,
    done: bool,
}

impl GuardScope<'_> {
    /// The after-check, as a result.
    pub fn finish(mut self) -> Res<()> {
        self.done = true;
        self.guard.check("after")
    }
}

impl Drop for GuardScope<'_> {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        if let Err(e) = self.guard.check("after") {
            eprintln!("{e}");
            self.guard
                .drop_failures
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push(e);
        }
    }
}

impl RealLedgerGuard {
    /// For the home this process runs under (`HOME`, else `USERPROFILE`). Without one the guard
    /// cannot say where the real ledgers are, so it refuses.
    pub fn new() -> Res<RealLedgerGuard> {
        let home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .filter(|h| !h.is_empty())
            .ok_or("the real-ledger guard needs HOME or USERPROFILE to find the ledgers")?;
        Ok(RealLedgerGuard::with_paths(real_ledger_paths(Path::new(
            &home,
        ))))
    }

    pub fn with_paths(paths: Vec<PathBuf>) -> RealLedgerGuard {
        RealLedgerGuard {
            paths,
            drop_failures: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// The before-check, and a scope whose after-check also runs on panic.
    pub fn begin(&self) -> Res<GuardScope<'_>> {
        self.check("before")?;
        Ok(GuardScope {
            guard: self,
            done: false,
        })
    }

    /// Failures the after-check found while unwinding from a panic.
    pub fn take_drop_failures(&self) -> Vec<String> {
        std::mem::take(&mut *self.drop_failures.lock().unwrap_or_else(|p| p.into_inner()))
    }

    /// Stop with an error when a real ledger holds a benchmark row. `when` is "before" or
    /// "after".
    pub fn check(&self, when: &str) -> Res<()> {
        for p in &self.paths {
            if let Some(n) = check_ledger(p)?
                && n > 0
            {
                return Err(format!(
                    "REFUSED: {} holds {n} row(s) from a benchmark session ({when} the run). \
                     A benchmark has written to a real ledger, or left rows in it; nothing it measured can be trusted.",
                    shown(p)
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn osv(v: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
        v.iter().map(|(k, v)| ((*k).into(), (*v).into())).collect()
    }

    fn get(env: &[(String, OsString)], k: &str) -> Option<String> {
        env.iter()
            .find(|(n, _)| n == k)
            .map(|(_, v)| v.to_string_lossy().into_owned())
    }

    #[test]
    fn hostile_environment_is_replaced_by_the_sandbox() {
        let t = tempfile::tempdir().unwrap();
        let sb = Sandbox::create(t.path()).unwrap();
        let env = sb.env_for(&osv(&[
            ("PATH", "/usr/bin"),
            ("HOME", "/Users/real"),
            ("USERPROFILE", "C:\\Users\\real"),
            ("LUMEN_DB", "/real/lumen.db"),
            ("LUMEN_REPEAT_READS", "on"),
            ("LUMEN_UPDATE_CHECK", "1"),
            ("CLAUDE_CODE_SESSION_ID", "real"),
            ("CLAUDECODE", "1"),
            ("ANTHROPIC_API_KEY", "sk-secret"),
            ("XDG_CONFIG_HOME", "/Users/real/.config"),
            ("RUST_LOG", "trace"),
        ]));
        assert_eq!(get(&env, "PATH").as_deref(), Some("/usr/bin"));
        for (k, _) in &env {
            assert!(
                !k.starts_with("CLAUDE") && k != "ANTHROPIC_API_KEY" && k != "RUST_LOG",
                "{k} leaked"
            );
        }
        let home = sb.home.to_string_lossy().into_owned();
        assert_eq!(get(&env, "HOME").as_deref(), Some(home.as_str()));
        assert_eq!(get(&env, "USERPROFILE").as_deref(), Some(home.as_str()));
        assert!(get(&env, "XDG_CONFIG_HOME").unwrap().starts_with(&home));
        assert!(get(&env, "APPDATA").unwrap().starts_with(&home));
        assert!(get(&env, "LOCALAPPDATA").unwrap().starts_with(&home));
        assert_eq!(get(&env, "LUMEN_UPDATE_CHECK").as_deref(), Some("0"));
        let root = t.path().to_string_lossy().into_owned();
        for k in [
            "LUMEN_DB",
            "LUMEN_PROJECTS_DIR",
            "LUMEN_FAULT_SPOOL",
            "LUMEN_LOG",
        ] {
            assert!(get(&env, k).unwrap().starts_with(&root), "{k} not scratch");
        }
        // Every LUMEN_* a child sees is one the sandbox set.
        let ours = [
            "LUMEN_DB",
            "LUMEN_PROJECTS_DIR",
            "LUMEN_FAULT_SPOOL",
            "LUMEN_LOG",
            "LUMEN_WS_ADDR",
            "LUMEN_UPDATE_CHECK",
        ];
        for (k, _) in &env {
            if k.starts_with("LUMEN_") {
                assert!(ours.contains(&k.as_str()), "{k} inherited");
            }
        }
        assert!(
            get(&env, "LUMEN_WS_ADDR")
                .unwrap()
                .starts_with("127.0.0.1:")
        );
    }

    #[test]
    fn pass_never_lets_lumen_or_claude_through() {
        let t = tempfile::tempdir().unwrap();
        let sb = Sandbox::create(t.path())
            .unwrap()
            .pass("LUMEN_FLAG")
            .pass("CLAUDE_X")
            .pass("MY_TOOL_HOME");
        let env = sb.env_for(&osv(&[
            ("LUMEN_FLAG", "1"),
            ("CLAUDE_X", "1"),
            ("MY_TOOL_HOME", "/x"),
        ]));
        assert_eq!(get(&env, "LUMEN_FLAG"), None);
        assert_eq!(get(&env, "CLAUDE_X"), None);
        assert_eq!(get(&env, "MY_TOOL_HOME").as_deref(), Some("/x"));
    }

    /// A real child process, not a model of one: it prints what it was given.
    #[test]
    fn a_stub_child_sees_no_inherited_lumen_or_claude_and_a_scratch_home() {
        let t = tempfile::tempdir().unwrap();
        let sb = Sandbox::create(t.path()).unwrap();
        let mut cmd = if cfg!(windows) {
            let mut c = Command::new("cmd");
            c.args(["/C", "set"]);
            c
        } else {
            Command::new("env")
        };
        // Set before `apply`, as a caller's own environment would be.
        cmd.env("LUMEN_REPEAT_READS", "on")
            .env("CLAUDE_CODE_SESSION_ID", "real-session")
            .env("HOME", "/Users/real")
            .env("LUMEN_UPDATE_CHECK", "1");
        sb.apply(&mut cmd);
        let out = cmd.output().unwrap();
        assert!(out.status.success());
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        assert!(!text.contains("real-session"), "{text}");
        assert!(!text.contains("LUMEN_REPEAT_READS"), "{text}");
        assert!(!text.contains("/Users/real"), "{text}");
        assert!(text.contains("LUMEN_UPDATE_CHECK=0"), "{text}");
        let home = sb.home.to_string_lossy().into_owned();
        assert!(
            text.lines()
                .any(|l| l.eq_ignore_ascii_case(&format!("HOME={home}"))),
            "{text}"
        );
    }

    #[test]
    fn the_child_runs_in_the_scratch_cwd() {
        if cfg!(windows) {
            return;
        }
        let t = tempfile::tempdir().unwrap();
        let sb = Sandbox::create(t.path()).unwrap();
        let mut cmd = Command::new("pwd");
        sb.apply(&mut cmd);
        let out = String::from_utf8(cmd.output().unwrap().stdout).unwrap();
        assert_eq!(
            Path::new(out.trim()).canonicalize().unwrap(),
            sb.cwd.canonicalize().unwrap()
        );
    }

    fn plant(db: &Path, session: &str) {
        let c = lumen_core::meter::connect_db(db).unwrap();
        c.execute_batch(&format!(
            "INSERT INTO session_events (session_id, kind, uuid, ts_ms) VALUES ('{session}', 'k', 'u', 1)"
        ))
        .unwrap();
    }

    #[test]
    fn a_planted_bench_row_trips_the_guard() {
        let t = tempfile::tempdir().unwrap();
        let db = t.path().join("lumen.db");
        plant(&db, "0f8e2c1a-real-session");
        let g = RealLedgerGuard::with_paths(vec![db.clone()]);
        g.check("before").unwrap();
        assert_eq!(check_ledger(&db).unwrap(), Some(0));
        plant(&db, "lumen-bench-planted");
        let e = g.check("after").unwrap_err();
        assert!(e.contains("REFUSED") && e.contains("after"), "{e}");
        assert!(!e.contains(t.path().to_str().unwrap()), "no path in {e}");
        // The harness's own prefix counts too.
        let db2 = t.path().join("two.db");
        plant(&db2, "bench-on-x");
        assert!(
            RealLedgerGuard::with_paths(vec![db2])
                .check("before")
                .is_err()
        );
    }

    /// Each prefix the harness's runs write is named here, apart from the constant: dropping one
    /// from `BENCH_PREFIXES` must turn this red, because a run that writes under it would pass the
    /// guard unseen.
    #[test]
    fn every_session_prefix_a_benchmark_writes_trips_the_guard() {
        let t = tempfile::tempdir().unwrap();
        for (i, prefix) in ["lumen-bench-", "bench-"].iter().enumerate() {
            assert!(
                BENCH_PREFIXES.contains(prefix),
                "{prefix} is missing from BENCH_PREFIXES"
            );
            let db = t.path().join(format!("p{i}.db"));
            plant(&db, &format!("{prefix}run-1"));
            let g = RealLedgerGuard::with_paths(vec![db]);
            let e = g.check("after").unwrap_err();
            assert!(e.contains("REFUSED"), "{prefix}: {e}");
        }
    }

    #[test]
    fn the_after_check_runs_when_the_run_panics() {
        let t = tempfile::tempdir().unwrap();
        let db = t.path().join("lumen.db");
        plant(&db, "0f8e2c1a-real-session");
        let g = RealLedgerGuard::with_paths(vec![db.clone()]);
        let died = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _scope = g.begin().unwrap();
            plant(&db, "lumen-bench-planted");
            panic!("the run died");
        }));
        assert!(died.is_err());
        let seen = g.take_drop_failures();
        assert_eq!(
            seen.len(),
            1,
            "the after-check must run on a panic: {seen:?}"
        );
        assert!(
            seen[0].contains("REFUSED") && seen[0].contains("after"),
            "{seen:?}"
        );
        // A clean run of the same scope reports nothing on drop and returns its error from finish.
        let clean = RealLedgerGuard::with_paths(vec![t.path().join("none.db")]);
        clean.begin().unwrap().finish().unwrap();
        assert!(clean.take_drop_failures().is_empty());
        let e = g.begin().err().expect("a planted row refuses before");
        assert!(e.contains("before"), "{e}");
    }

    #[test]
    fn the_guard_never_creates_a_ledger_and_cannot_write() {
        let t = tempfile::tempdir().unwrap();
        let missing = t.path().join("nope").join("lumen.db");
        assert_eq!(check_ledger(&missing).unwrap(), None);
        RealLedgerGuard::with_paths(vec![missing.clone()])
            .check("before")
            .unwrap();
        assert!(!missing.exists() && !missing.parent().unwrap().exists());
        // The connection it uses refuses a write.
        let db = t.path().join("lumen.db");
        plant(&db, "real");
        let c = lumen_core::meter::open_read_only(Path::new(&ro_uri(&db))).unwrap();
        assert!(c.execute_batch("DELETE FROM session_events").is_err());
        // A path with URI syntax in it still opens the right file.
        let odd = t.path().join("a b#c%d");
        std::fs::create_dir(&odd).unwrap();
        let db = odd.join("lumen.db");
        plant(&db, "bench-x");
        assert_eq!(check_ledger(&db).unwrap(), Some(1));
    }

    #[test]
    fn real_ledger_paths_cover_every_ledger_under_the_data_root_and_the_pointer() {
        let t = tempfile::tempdir().unwrap();
        std::fs::write(t.path().join(".lumen_db_path"), "/somewhere/else.db\n").unwrap();
        let parent = lumen_core::meter::app_data_dir_in(t.path());
        let parent = parent.parent().unwrap().to_path_buf();
        // A previous install under a name of its own, and a folder with no ledger.
        std::fs::create_dir_all(parent.join("io.previous.lumen")).unwrap();
        std::fs::write(parent.join("io.previous.lumen").join("lumen.db"), b"").unwrap();
        std::fs::create_dir_all(parent.join("some.other.app")).unwrap();
        let v = real_ledger_paths(t.path());
        assert!(v.contains(&PathBuf::from("/somewhere/else.db")));
        assert!(v.contains(&lumen_core::meter::canonical_db_path_in(t.path())));
        assert!(v.contains(&parent.join("io.previous.lumen").join("lumen.db")));
        assert!(!v.contains(&parent.join("some.other.app").join("lumen.db")));
        assert!(v.contains(&parent.join(NEXT_APP_ID).join("lumen.db")));
        let mut d = v.clone();
        d.dedup();
        assert_eq!(d.len(), v.len(), "no duplicates: {v:?}");
    }

    #[test]
    fn a_taken_port_refuses_with_quit_lumen_first() {
        let l = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = l.local_addr().unwrap().port();
        let e = ensure_port_free(port).unwrap_err();
        assert!(e.contains("quit Lumen first"), "{e}");
        drop(l);
        // Other tests bind and release ports in parallel, so a port can be busy for a moment:
        // a free one passes within a few tries.
        let ok = (0..20).any(|_| {
            std::thread::sleep(Duration::from_millis(50));
            ensure_port_free(port).is_ok()
        });
        assert!(ok, "a released port is reported free");
    }
}
