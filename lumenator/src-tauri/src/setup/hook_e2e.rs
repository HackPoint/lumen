//! Setup's hooks, end to end, the way Claude Code runs them.
//!
//! Each test installs Lumen into a temp home with `run_setup_with`, reads the
//! registered `command` back out of the settings.json Setup wrote, and hands it to the
//! shell Claude Code 2.1.270 would use: `/bin/sh -c` on macOS and Linux, Git Bash's
//! `bash -c` on Windows, with a `bash ` prefix when the command names a `.sh`. Stdin is
//! a payload Claude Code really sent (`crates/lumen-mcp/tests/fixtures/hooks/`).
//!
//! `lumen-mcp` is either a stub that records what it was handed — argv, the ledger and
//! spool in its environment, stdin — or the real sidecar: `LUMEN_E2E_MCP`, else the one
//! `build-sidecar.sh` staged under `binaries/`.
//!
//! The environment is built, not inherited. HOME, USERPROFILE and TMPDIR/TMP/TEMP point
//! into the temp dir, and on macOS and Linux PATH holds links to the few tools a test
//! needs and nothing else, so a `lumen-mcp` or `python3` installed on the machine
//! running the suite cannot stand in for one the test did not provide. Windows gets
//! Git Bash's own directories and System32, as Claude Code's hook process does.

use super::*;
use lumen_core::faults::FaultRecord;
use lumen_core::tokenizer::count_tokens;
use rusqlite::{Connection, OpenFlags};
use serde_json::{json, Value};
use std::cell::Cell;
use std::ffi::{OsStr, OsString};
use std::io::Write;
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tempfile::TempDir;

/// `hello.rs` from the capture session, byte for byte.
const HELLO: &str = "fn main() {\n    println!(\"hello\");\n}\n";

/// The session ids inside the captured payloads.
const READ_SESSION: &str = "a2dc779f-f8bf-44c5-9016-7a087d832e3f";
const BASH_SESSION: &str = "7c159cf4-d8a2-4de3-a430-c2b250f22c84";

/// Every file a test reads gets this modification time, so `file_mtime` is checked
/// against the file and not against "some recent second".
const MTIME: i64 = 1_700_000_000;

const OS: &str = std::env::consts::OS;

/// The app executable a 1.5.1 login-item marker recorded.
const APP_EXE: &str = "/Applications/Lumen.app/Contents/MacOS/Lumen";

/// What the 1.6.0 shims run, apart from `lumen-mcp`.
const SHIM_TOOLS: &[&str] = &["bash", "cat", "date"];

/// What the 1.5.1 scripts run, apart from `python3`.
const V151_TOOLS: &[&str] = &[
    "bash", "cat", "date", "mktemp", "rm", "cut", "wc", "awk", "stat", "tr", "grep",
];

/// `read_events` as a 1.5.1 install created it.
const V151_READ_EVENTS: &str = "CREATE TABLE IF NOT EXISTS read_events (
    ts TEXT NOT NULL, tool TEXT NOT NULL, path TEXT NOT NULL, lines INTEGER,
    tokens_returned INTEGER NOT NULL, full_tokens INTEGER NOT NULL, saved_tokens INTEGER NOT NULL,
    routed_via TEXT NOT NULL, channel TEXT NOT NULL DEFAULT 'unknown',
    session_id TEXT, file_mtime INTEGER, req_key TEXT, is_subagent INTEGER NOT NULL DEFAULT 0,
    writer_hook TEXT, token_source TEXT, budget INTEGER, s_min INTEGER, econ_context REAL,
    econ_rounds REAL, econ_output REAL, econ_source TEXT, k_selected INTEGER, n_total INTEGER,
    coeff_version INTEGER, target_outline INTEGER
);";

/// A `lumen-mcp` that writes down what it was handed and does what `STUB_EXIT` and
/// `STUB_STDERR` say. `__NAME__` tells two stubs apart.
const STUB: &str = r#"#!/bin/sh
printf '%s\n' __NAME__ >"$STUB_OUT/name"
for a in "$@"; do printf '%s\n' "$a"; done >"$STUB_OUT/argv"
printf '%s\n' "$LUMEN_DB" "$LUMEN_FAULT_SPOOL" >"$STUB_OUT/env"
cat >"$STUB_OUT/stdin"
if [ -n "$STUB_STDERR" ]; then printf '%s\n' "$STUB_STDERR" >&2; fi
exit "${STUB_EXIT:-0}"
"#;

/// What a hook's environment says about the Pythons on it.
const PYTHON_PROBE: &str = r#"for p in python3 python py; do
    if command -v "$p" >/dev/null 2>&1; then echo "$p: found"; else echo "$p: absent"; fi
done
python3 --version
echo "python3 exit $?"
"#;

/// A `python` or `py` that is Python, as far as `--version` goes.
const PYTHON_STUB: &str = "#!/bin/sh\necho 'Python 3.13.1'\n";

/// The App Execution Alias Windows puts at `python3` when Python is not installed: it
/// says so and exits 9009, which a POSIX shell sees as 49.
const STORE_STUB: &str = "#!/bin/sh\necho 'Python was not found; run without arguments to install from the Microsoft Store, or disable this shortcut from Settings > Manage App Execution Aliases.' >&2\nexit 9009\n";

/// The login item, as a flag.
struct LoginItem(Cell<bool>);

impl LoginItem {
    fn off() -> Self {
        Self(Cell::new(false))
    }
}

impl AutoStart for LoginItem {
    fn is_enabled(&self) -> Result<bool, String> {
        Ok(self.0.get())
    }
    fn enable(&self) -> Result<(), String> {
        self.0.set(true);
        Ok(())
    }
    fn disable(&self) -> Result<(), String> {
        self.0.set(false);
        Ok(())
    }
}

/// What one hook process did.
#[derive(Debug)]
struct Ran {
    code: i32,
    stdout: String,
    stderr: String,
}

#[derive(Debug)]
struct Row {
    tool: String,
    path: String,
    lines: Option<i64>,
    tokens_returned: i64,
    full_tokens: i64,
    saved_tokens: i64,
    routed_via: String,
    channel: String,
    session_id: Option<String>,
    file_mtime: Option<i64>,
    req_key: Option<String>,
    writer_hook: Option<String>,
    token_source: Option<String>,
}

/// What a stub `lumen-mcp` was handed.
#[derive(Debug)]
struct Handed {
    name: String,
    argv: Vec<String>,
    /// `LUMEN_DB` and `LUMEN_FAULT_SPOOL`, in that order.
    env: Vec<String>,
    stdin: Vec<u8>,
}

/// A machine with Claude Code on it and Lumen about to be installed: a home, a project,
/// a temp dir, the tools on PATH, and where Setup will put the ledger.
struct Rig {
    root: TempDir,
    home: PathBuf,
    tmp: PathBuf,
    proj: PathBuf,
    bin: PathBuf,
    /// Where a stub `lumen-mcp` writes down what it was handed.
    out: PathBuf,
    db: PathBuf,
    spool: PathBuf,
}

impl Rig {
    fn new() -> Self {
        Self::with_home("home", SHIM_TOOLS)
    }

    fn with_home(name: &str, tools: &[&str]) -> Self {
        let root = TempDir::new().expect("tempdir");
        let mk = |p: PathBuf| {
            std::fs::create_dir_all(&p).unwrap();
            p
        };
        let home = mk(root.path().join(name));
        mk(home.join(".claude"));
        let data = mk(app_support_dir_in(&home));
        let db = data.join("lumen.db");
        // Isolation asserted, not assumed: a hook that ignored what Setup baked would
        // write the developer's own ledger, and every test here would still pass.
        assert!(
            db.starts_with(std::env::temp_dir()),
            "the test ledger must live under the temp dir, got {}",
            db.display()
        );
        let bin = mk(root.path().join("bin"));
        for tool in tools {
            link_tool(&bin, tool);
        }
        Rig {
            tmp: mk(root.path().join("tmp")),
            proj: mk(root.path().join("proj")),
            out: mk(root.path().join("out")),
            spool: data.join("faults.jsonl"),
            db,
            bin,
            home,
            root,
        }
    }

    /// The PATH a hook gets.
    fn path(&self) -> OsString {
        let mut dirs = vec![self.bin.clone()];
        if cfg!(windows) {
            if let Some(root) = std::env::var_os("SYSTEMROOT") {
                dirs.push(PathBuf::from(root).join("System32"));
            }
        }
        std::env::join_paths(dirs).unwrap()
    }

    /// Setup, as the button runs it, with `mcp` as the sidecar it found.
    fn install(&self, mcp: &Path) -> Vec<SetupStep> {
        self.install_as(mcp, &LoginItem::off())
    }

    fn install_as(&self, mcp: &Path, item: &LoginItem) -> Vec<SetupStep> {
        let steps = run_setup_with(
            &self.home,
            item,
            &self.db.to_string_lossy(),
            &mcp.to_string_lossy(),
        );
        assert!(
            steps
                .iter()
                .all(|s| matches!(s.status, StepStatus::Ok | StepStatus::Warn)),
            "{steps:?}"
        );
        steps
    }

    /// The lumen `command` registered for `matcher` under `phase`, as Setup wrote it.
    fn registered(&self, phase: &str, matcher: &str) -> String {
        registered_in(&global_settings_path_in(&self.home), phase, matcher)
    }

    /// `command` run as Claude Code runs a hook: its shell, `stdin` on stdin, the
    /// project as the working directory.
    fn run(&self, command: &str, stdin: &[u8], env: &[(&str, &str)]) -> Ran {
        let (shell, args, path) = claude_code_shell(command, &self.path());
        let mut cmd = Command::new(&shell);
        cmd.args(&args)
            .env_clear()
            .env("PATH", path)
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env("TMPDIR", &self.tmp)
            .env("TMP", &self.tmp)
            .env("TEMP", &self.tmp)
            .env("STUB_OUT", shell_path(&self.out.to_string_lossy()));
        if cfg!(windows) {
            for k in ["SYSTEMROOT", "SYSTEMDRIVE", "WINDIR", "COMSPEC", "PATHEXT"] {
                if let Some(v) = std::env::var_os(k) {
                    cmd.env(k, v);
                }
            }
        }
        let mut child = cmd
            .envs(env.iter().copied())
            .current_dir(&self.proj)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap_or_else(|e| panic!("spawn {}: {e}", shell.display()));
        // A hook may exit without reading its stdin; that is its business.
        let _ = child.stdin.take().expect("stdin").write_all(stdin);
        let out = child.wait_with_output().expect("wait for the hook");
        Ran {
            code: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }

    /// The PostToolUse hook registered for the payload's tool.
    fn meter(&self, payload: &Value, env: &[(&str, &str)]) -> Ran {
        let tool = payload["tool_name"].as_str().expect("tool_name");
        let command = self.registered("PostToolUse", tool);
        let ran = self.run(&command, &serde_json::to_vec(payload).unwrap(), env);
        // On every path: Claude Code parses a hook's stdout as JSON control output.
        assert_eq!(ran.stdout, "", "the meter printed to stdout: {ran:?}");
        assert_eq!(
            ran.code, 0,
            "the meter must never fail the tool call: {ran:?}"
        );
        ran
    }

    /// The PreToolUse hook registered for Read.
    fn intercept(&self, payload: &Value, env: &[(&str, &str)]) -> Ran {
        let command = self.registered("PreToolUse", "Read");
        let ran = self.run(&command, &serde_json::to_vec(payload).unwrap(), env);
        assert_eq!(ran.stdout, "", "the intercept printed to stdout: {ran:?}");
        ran
    }

    /// A captured payload aimed at this rig. Only `cwd` and, given `file`, the two file
    /// path fields change.
    fn captured(&self, name: &str, file: Option<&Path>) -> Value {
        let mut v: Value = serde_json::from_slice(&fixture_bytes(name)).expect("JSON");
        v["cwd"] = Value::from(self.proj.to_string_lossy().as_ref());
        if let Some(file) = file {
            let s = Value::from(file.to_string_lossy().as_ref());
            v["tool_input"]["file_path"] = s.clone();
            if let Some(f) = v.pointer_mut("/tool_response/file") {
                f["filePath"] = s;
            }
        }
        v
    }

    /// The variables Claude Code sets in a hook's environment, for a CLI session.
    fn claude_env<'a>(&'a self, session: &'a str) -> [(&'a str, &'a str); 3] {
        [
            ("CLAUDE_CODE_ENTRYPOINT", "cli"),
            (
                "CLAUDE_PROJECT_DIR",
                self.proj.to_str().expect("a UTF-8 temp path"),
            ),
            ("CLAUDE_CODE_SESSION_ID", session),
        ]
    }

    /// A file in the project directory, stamped with [`MTIME`].
    fn file(&self, name: &str, body: &str) -> PathBuf {
        let p = self.proj.join(name);
        std::fs::write(&p, body).unwrap();
        std::fs::File::options()
            .write(true)
            .open(&p)
            .unwrap()
            .set_modified(UNIX_EPOCH + Duration::from_secs(MTIME as u64))
            .unwrap();
        p
    }

    /// Every row in the ledger, oldest first. Opened read-only, so looking cannot create
    /// a ledger the hook did not.
    fn rows(&self) -> Vec<Row> {
        let conn = Connection::open_with_flags(&self.db, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap_or_else(|e| panic!("open {}: {e}", self.db.display()));
        let mut stmt = conn
            .prepare(
                "SELECT tool, path, lines, tokens_returned, full_tokens, saved_tokens, \
                 routed_via, channel, session_id, file_mtime, req_key, writer_hook, \
                 token_source FROM read_events ORDER BY rowid",
            )
            .unwrap();
        stmt.query_map([], |r| {
            Ok(Row {
                tool: r.get(0)?,
                path: r.get(1)?,
                lines: r.get(2)?,
                tokens_returned: r.get(3)?,
                full_tokens: r.get(4)?,
                saved_tokens: r.get(5)?,
                routed_via: r.get(6)?,
                channel: r.get(7)?,
                session_id: r.get(8)?,
                file_mtime: r.get(9)?,
                req_key: r.get(10)?,
                writer_hook: r.get(11)?,
                token_source: r.get(12)?,
            })
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
    }

    /// Every record in the fault spool. A missing spool is no faults.
    fn faults(&self) -> Vec<FaultRecord> {
        faults_in(&self.spool)
    }

    /// What the last stub to run was handed, or None if none ran since the last call.
    fn handed(&self) -> Option<Handed> {
        let take = |f: &str| {
            let p = self.out.join(f);
            let bytes = std::fs::read(&p).ok();
            let _ = std::fs::remove_file(&p);
            bytes
        };
        let lines = |b: Option<Vec<u8>>| -> Vec<String> {
            String::from_utf8(b.unwrap_or_default())
                .unwrap()
                .lines()
                .map(String::from)
                .collect()
        };
        let name = take("name")?;
        Some(Handed {
            name: String::from_utf8(name).unwrap().trim_end().to_string(),
            argv: lines(take("argv")),
            env: lines(take("env")),
            stdin: take("stdin").unwrap_or_default(),
        })
    }

    /// `script` in a hook's environment, its raw output printed for the report.
    fn probe(&self, label: &str, script: &str) -> Ran {
        let ran = self.run(script, b"", &[]);
        eprintln!(
            "probe [{OS}] {label}: exit {}\n--- stdout\n{}--- stderr\n{}--- end",
            ran.code, ran.stdout, ran.stderr
        );
        ran
    }

    /// A copy of the real sidecar, under a directory with a space in its name.
    fn real_mcp(&self) -> PathBuf {
        let at = self
            .root
            .path()
            .join("Lumen Test")
            .join("bin")
            .join(format!("lumen-mcp{}", std::env::consts::EXE_SUFFIX));
        std::fs::create_dir_all(at.parent().unwrap()).unwrap();
        std::fs::copy(sidecar(), &at).unwrap();
        at
    }

    /// The scripts a 1.5.1 Setup wrote, where it wrote them: (meter, intercept).
    fn v151_scripts(&self) -> (PathBuf, PathBuf) {
        let dir = lumen_dir_in(&self.home);
        std::fs::create_dir_all(&dir).unwrap();
        let [meter, intercept] = ["lumen_meter.sh", "lumen_read_intercept.sh"].map(|name| {
            let p = dir.join(name);
            std::fs::write(&p, v151_script(name, &self.home)).unwrap();
            set_mode(&p, 0o755).unwrap();
            p
        });
        (meter, intercept)
    }

    /// The first line of `bash --version`, as a hook sees it.
    fn bash_version(&self) -> String {
        let ran = self.run("bash --version", b"", &[]);
        ran.stdout.lines().next().unwrap_or("").to_string()
    }

    /// The Claude Code plugin: this repository's `hooks/hooks.json` and the scripts in
    /// `.claude/hooks/`, copied under a root named `name`, and given `built`, the real
    /// lumen-mcp where a release build of the checkout puts it.
    fn plugin(&self, name: &str, built: bool) -> PathBuf {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let root = self.root.path().join("plugins").join(name);
        for rel in [
            "hooks/hooks.json",
            ".claude/hooks/lumen_meter.sh",
            ".claude/hooks/lumen_read_intercept.sh",
        ] {
            let to = root.join(rel);
            std::fs::create_dir_all(to.parent().unwrap()).unwrap();
            // `copy` keeps the mode, and the scripts are run by path.
            std::fs::copy(repo.join(rel), &to).unwrap();
        }
        if built {
            let at = root
                .join("target/release")
                .join(format!("lumen-mcp{}", std::env::consts::EXE_SUFFIX));
            std::fs::create_dir_all(at.parent().unwrap()).unwrap();
            std::fs::copy(sidecar(), &at).unwrap();
        }
        root
    }

    /// The plugin's hook for the payload's tool under `phase`, run as Claude Code runs
    /// a plugin's: the command as hooks.json has it, unexpanded, with the root in
    /// `CLAUDE_PLUGIN_ROOT` — on Windows in forward slashes, as Claude Code passes it
    /// to Git Bash.
    fn plugin_hook(&self, root: &Path, phase: &str, payload: &Value, env: &[(&str, &str)]) -> Ran {
        let tool = payload["tool_name"].as_str().expect("tool_name");
        let command = registered_in(&root.join("hooks/hooks.json"), phase, tool);
        let root = shell_path(&root.to_string_lossy());
        let mut env = env.to_vec();
        env.push(("CLAUDE_PLUGIN_ROOT", &root));
        let ran = self.run(&command, &serde_json::to_vec(payload).unwrap(), &env);
        assert_eq!(ran.stdout, "", "a plugin hook printed to stdout: {ran:?}");
        if phase == "PostToolUse" {
            assert_eq!(
                ran.code, 0,
                "the meter must never fail the tool call: {ran:?}"
            );
        }
        ran
    }
}

/// A link to the machine's `tool` in `bin`. /bin and /usr/bin first, so a newer copy
/// earlier on the developer's PATH does not stand in for what a hook usually gets.
#[cfg(unix)]
fn link_tool(bin: &Path, tool: &str) {
    let host = std::env::var_os("PATH").unwrap_or_default();
    let found = [PathBuf::from("/bin"), PathBuf::from("/usr/bin")]
        .into_iter()
        .chain(std::env::split_paths(&host))
        .map(|d| d.join(tool))
        .find(|p| p.is_file())
        .unwrap_or_else(|| panic!("{tool} is not installed on this machine"));
    std::os::unix::fs::symlink(&found, bin.join(tool)).unwrap();
}

/// Git Bash brings its own tools; see [`claude_code_shell`].
#[cfg(not(unix))]
fn link_tool(_bin: &Path, _tool: &str) {}

/// The program, arguments and PATH Claude Code 2.1.270 runs a hook `command` with.
///
/// On Windows it runs Git Bash with that bash's directory first on PATH, and when the
/// command's first word names a `.sh` it prefixes `bash `. Git's `bin\bash.exe` then
/// puts its `usr\bin` on PATH, which is where `cat` and `date` come from there.
fn claude_code_shell(command: &str, path: &OsStr) -> (PathBuf, Vec<String>, OsString) {
    if cfg!(windows) {
        let bash = git_bash();
        let command = if hook_command_path(command).ends_with(".sh") {
            format!("bash {command}")
        } else {
            command.to_string()
        };
        let dirs = std::iter::once(bash.parent().unwrap().to_path_buf())
            .chain(std::env::split_paths(path));
        let path = std::env::join_paths(dirs).unwrap();
        (bash, vec!["-c".to_string(), command], path)
    } else {
        (
            PathBuf::from("/bin/sh"),
            vec!["-c".to_string(), command.to_string()],
            path.to_os_string(),
        )
    }
}

/// Git Bash where Claude Code looks for it, in its order.
fn git_bash() -> PathBuf {
    std::env::var_os("CLAUDE_CODE_GIT_BASH_PATH")
        .map(PathBuf::from)
        .into_iter()
        .chain([
            PathBuf::from(r"C:\Program Files\Git\bin\bash.exe"),
            PathBuf::from(r"C:\Program Files (x86)\Git\bin\bash.exe"),
        ])
        .find(|p| p.is_file())
        .expect("Git Bash, where Claude Code looks for it")
}

/// The real `lumen-mcp`. Refused when its sources are newer than it, because a stale
/// sidecar would test last week's hook.
fn sidecar() -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let path = std::env::var_os("LUMEN_E2E_MCP")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            manifest.join("binaries").join(format!(
                "lumen-mcp-{}{}",
                env!("TAURI_ENV_TARGET_TRIPLE"),
                std::env::consts::EXE_SUFFIX
            ))
        });
    let built = std::fs::metadata(&path)
        .and_then(|m| m.modified())
        .unwrap_or_else(|e| {
            panic!(
                "no lumen-mcp at {} ({e}) — run lumenator/build-sidecar.sh, or set LUMEN_E2E_MCP",
                path.display()
            )
        });
    let crates = manifest.join("../../crates");
    if let Some((src, changed)) =
        newest_file(&[crates.join("lumen-mcp/src"), crates.join("lumen-core/src")])
    {
        assert!(
            changed <= built,
            "{} is older than {} — rebuild it with lumenator/build-sidecar.sh",
            path.display(),
            src.display()
        );
    }
    path
}

/// The most recently modified file under `dirs`.
fn newest_file(dirs: &[PathBuf]) -> Option<(PathBuf, SystemTime)> {
    let mut stack = dirs.to_vec();
    let mut newest: Option<(PathBuf, SystemTime)> = None;
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.is_dir() {
                stack.push(entry.path());
            } else if let Ok(t) = meta.modified() {
                if newest.as_ref().is_none_or(|(_, n)| t > *n) {
                    newest = Some((entry.path(), t));
                }
            }
        }
    }
    newest
}

/// A captured payload, every byte as Claude Code sent it.
fn fixture_bytes(name: &str) -> Vec<u8> {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../crates/lumen-mcp/tests/fixtures/hooks")
        .join(name);
    std::fs::read(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

/// A script as 1.5.1 installed it, for a user whose home is `home`.
fn v151_script(name: &str, home: &Path) -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/v1.5.1")
        .join(name);
    std::fs::read_to_string(&p)
        .unwrap_or_else(|e| panic!("{}: {e}", p.display()))
        .replace("__HOME__", &shell_path(&home.to_string_lossy()))
}

/// A stub `lumen-mcp` at `at`, answering to `name`.
fn stub(at: &Path, name: &str) {
    std::fs::create_dir_all(at.parent().unwrap()).unwrap();
    std::fs::write(at, STUB.replace("__NAME__", name)).unwrap();
    set_mode(at, 0o755).unwrap();
}

fn write_json(path: &Path, v: &Value) {
    std::fs::write(path, serde_json::to_string_pretty(v).unwrap()).unwrap();
}

/// The lumen `command` for `matcher` under `phase` in a settings.json or a plugin's
/// hooks.json, which share the shape.
fn registered_in(path: &Path, phase: &str, matcher: &str) -> String {
    let settings: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    settings["hooks"][phase]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|e| e["matcher"] == matcher)
        .flat_map(|e| e["hooks"].as_array().into_iter().flatten())
        .filter_map(|h| h["command"].as_str())
        .find(|c| c.contains("lumen_"))
        .unwrap_or_else(|| panic!("no lumen hook for {phase} {matcher}: {settings}"))
        .to_string()
}

/// Every record in the spool at `path`. A missing spool is no faults.
fn faults_in(path: &Path) -> Vec<FaultRecord> {
    match std::fs::read_to_string(path) {
        Ok(text) => text
            .lines()
            .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("spool line {l:?}: {e}")))
            .collect(),
        Err(_) => Vec::new(),
    }
}

/// `YYYY-MM-DDTHH:MM:SSZ`, the shape `FaultRecord` writes.
fn is_utc_second(ts: &str) -> bool {
    let b = ts.as_bytes();
    b.len() == 20
        && b.iter().enumerate().all(|(i, c)| match i {
            4 | 7 => *c == b'-',
            10 => *c == b'T',
            13 | 16 => *c == b':',
            19 => *c == b'Z',
            _ => c.is_ascii_digit(),
        })
}

fn source_lines(n: usize) -> String {
    (0..n).map(|i| format!("fn f{i}() {{}}\n")).collect()
}

fn missing_line(looked: &Path, consequence: &str) -> String {
    format!(
        "lumen: cannot run lumen-mcp (looked for '{}', then on PATH); {consequence}\n",
        shell_path(&looked.to_string_lossy())
    )
}

const NOT_METERED: &str = "this event was not metered";
const LET_THROUGH: &str = "this read was let through";

// ── The shims, against a stub lumen-mcp ─────────────────────────────────────

/// The PATH a hook gets holds the tools the rig linked and nothing else. Every
/// "lumen-mcp is missing" result below depends on it: with the developer's own
/// `lumen-mcp` on PATH, the fallback would find it.
#[test]
fn the_hook_environment_holds_only_what_the_rig_put_there() {
    let rig = Rig::new();
    let ran = rig.probe(
        "hook environment",
        r#"echo "shell: $0"
bash -c 'echo "bash: $BASH_VERSION OSTYPE=$OSTYPE MSYSTEM=${MSYSTEM-unset}"'
for t in bash cat date lumen-mcp python3 python py; do
    printf '%s: %s\n' "$t" "$(command -v "$t" || echo absent)"
done
printf 'HOME=%s\nTMPDIR=%s\nTMP=%s\nTEMP=%s\nPWD=%s\n' "$HOME" "${TMPDIR-unset}" "${TMP-unset}" "${TEMP-unset}" "$PWD"
"#,
    );
    assert_eq!(ran.code, 0, "{ran:?}");
    for absent in ["lumen-mcp: absent", "python3: absent", "python: absent"] {
        assert!(ran.stdout.lines().any(|l| l == absent), "{absent}: {ran:?}");
    }
    for tool in ["bash", "cat", "date"] {
        let line = ran
            .stdout
            .lines()
            .find(|l| l.starts_with(&format!("{tool}: ")))
            .unwrap_or_else(|| panic!("{tool}: {ran:?}"));
        assert!(!line.ends_with("absent"), "{line}");
    }
}

/// The shims hand Claude Code's payload to `lumen-mcp hook` untouched: the same bytes
/// on stdin, the subcommand for the event, and the baked ledger and spool in the
/// environment. The binary sits under a directory with a space in its name, as it
/// does in `C:\Program Files\Lumen`.
#[test]
fn the_installed_hooks_hand_the_payload_to_lumen_mcp_unchanged() {
    let rig = Rig::new();
    let mcp = rig.root.path().join("Lumen Test/bin/lumen-mcp");
    stub(&mcp, "baked");
    rig.install(&mcp);
    let meter: &[&str] = &["hook", "meter", "--writer", "lumen_meter.sh"];
    let cases = [
        ("PostToolUse", "Read", "read_post.json", meter),
        ("PostToolUse", "Bash", "bash_post.json", meter),
        (
            "PreToolUse",
            "Read",
            "read_pre.json",
            &["hook", "intercept"],
        ),
    ];
    for (phase, matcher, fixture, argv) in cases {
        let payload = fixture_bytes(fixture);
        let ran = rig.run(&rig.registered(phase, matcher), &payload, &[]);
        assert_eq!(
            (ran.code, ran.stdout.as_str(), ran.stderr.as_str()),
            (0, "", ""),
            "{fixture}: {ran:?}"
        );
        let h = rig
            .handed()
            .unwrap_or_else(|| panic!("{fixture}: lumen-mcp never ran"));
        assert_eq!(h.name, "baked", "{fixture}");
        assert_eq!(h.argv, argv, "{fixture}");
        assert_eq!(
            h.env,
            [
                shell_path(&rig.db.to_string_lossy()),
                shell_path(&rig.spool.to_string_lossy())
            ],
            "{fixture}"
        );
        assert!(h.stdin == payload, "{fixture}: stdin changed on the way");
    }
    assert!(!rig.spool.exists(), "{:?}", rig.faults());
}

/// What `lumen-mcp` says and how it exits is what Claude Code sees: the status is the
/// block, and stderr is what the model is told.
#[test]
fn the_status_and_stderr_of_lumen_mcp_come_through_the_shims() {
    let rig = Rig::new();
    let mcp = rig.root.path().join("Lumen Test/bin/lumen-mcp");
    stub(&mcp, "baked");
    rig.install(&mcp);
    let line = "Lumen intercept: a stand-in for the redirect";
    let said = format!("{line}\n");

    let blocked = rig.run(
        &rig.registered("PreToolUse", "Read"),
        &fixture_bytes("read_pre.json"),
        &[("STUB_EXIT", "2"), ("STUB_STDERR", line)],
    );
    assert_eq!(
        (
            blocked.code,
            blocked.stdout.as_str(),
            blocked.stderr.as_str()
        ),
        (2, "", said.as_str())
    );
    let metered = rig.run(
        &rig.registered("PostToolUse", "Read"),
        &fixture_bytes("read_post.json"),
        &[("STUB_STDERR", line)],
    );
    assert_eq!((metered.code, metered.stderr.as_str()), (0, said.as_str()));
}

/// Lumen moved or deleted. Both hooks exit 0, say so on stderr, and leave one fault
/// each, so neither the lost event nor the read let through goes unrecorded.
#[test]
fn a_missing_lumen_mcp_is_said_and_recorded_and_blocks_nothing() {
    let rig = Rig::new();
    let mcp = rig.root.path().join("Lumen Test/bin/lumen-mcp");
    rig.install(&mcp);

    let meter = rig.run(
        &rig.registered("PostToolUse", "Read"),
        &fixture_bytes("read_post.json"),
        &[],
    );
    let intercept = rig.run(
        &rig.registered("PreToolUse", "Read"),
        &fixture_bytes("read_pre.json"),
        &[],
    );
    eprintln!(
        "raw stderr, meter: {:?}\nraw stderr, intercept: {:?}\nraw spool:\n{}",
        meter.stderr,
        intercept.stderr,
        std::fs::read_to_string(&rig.spool).unwrap_or_default()
    );
    assert_eq!((meter.code, meter.stdout.as_str()), (0, ""));
    assert_eq!(meter.stderr, missing_line(&mcp, NOT_METERED));
    assert_eq!((intercept.code, intercept.stdout.as_str()), (0, ""));
    assert_eq!(intercept.stderr, missing_line(&mcp, LET_THROUGH));

    let faults = rig.faults();
    let kinds: Vec<(&str, &str)> = faults
        .iter()
        .map(|f| (f.kind.as_str(), f.variant.as_str()))
        .collect();
    assert_eq!(
        kinds,
        [
            ("meter_write_failed", "lumen_mcp_missing"),
            ("hook_fail_open", "lumen_mcp_missing")
        ]
    );
    for f in &faults {
        assert!(is_utc_second(&f.ts), "{f:?}");
        assert_eq!(
            (
                f.path.as_deref(),
                f.lines,
                f.detail.as_deref(),
                f.session_id.as_deref()
            ),
            (None, None, None, None),
            "{f:?}"
        );
        assert_eq!(f.version.as_deref(), Some(env!("CARGO_PKG_VERSION")));
        assert_eq!(f.channel, "unknown", "no entrypoint, no VS Code variables");
    }
    assert!(!rig.db.exists(), "nothing ran that could write the ledger");
}

/// Something at the baked path that will not run is reported as that, not as missing.
#[test]
fn an_unrunnable_lumen_mcp_is_told_apart_from_a_missing_one() {
    let rig = Rig::new();
    let mcp = rig.root.path().join("Lumen Test/bin/lumen-mcp");
    std::fs::create_dir_all(&mcp).unwrap();
    rig.install(&mcp);
    let ran = rig.run(
        &rig.registered("PostToolUse", "Read"),
        &fixture_bytes("read_post.json"),
        &[],
    );
    eprintln!("raw stderr, a directory at the baked path:\n{}", ran.stderr);
    assert_eq!(ran.code, 0, "{ran:?}");
    assert_eq!(
        ran.stderr.lines().last(),
        Some(missing_line(&mcp, NOT_METERED).trim_end()),
        "{ran:?}"
    );
    let faults = rig.faults();
    assert_eq!(faults.len(), 1, "{faults:?}");
    assert_eq!(
        (faults[0].kind.as_str(), faults[0].variant.as_str()),
        ("meter_write_failed", "lumen_mcp_unrunnable")
    );
}

/// LUMEN_CAPTURE=0 keeps the line on stderr and drops the record.
#[test]
fn capture_off_keeps_the_line_and_drops_the_fault() {
    let rig = Rig::new();
    let mcp = rig.root.path().join("Lumen Test/bin/lumen-mcp");
    rig.install(&mcp);
    let ran = rig.run(
        &rig.registered("PostToolUse", "Read"),
        &fixture_bytes("read_post.json"),
        &[("LUMEN_CAPTURE", "0")],
    );
    assert_eq!(ran.code, 0);
    assert_eq!(ran.stderr, missing_line(&mcp, NOT_METERED));
    assert!(!rig.spool.exists(), "{:?}", rig.faults());
}

/// LUMEN_HOOK_ENABLED=0 switches the intercept off, and an intercept that is off has
/// nothing to report when lumen-mcp is missing. The meter is not the switch's.
#[test]
fn a_switched_off_intercept_is_silent_even_without_lumen_mcp() {
    let rig = Rig::new();
    let mcp = rig.root.path().join("Lumen Test/bin/lumen-mcp");
    rig.install(&mcp);
    let off = [("LUMEN_HOOK_ENABLED", "0")];
    let intercept = rig.run(
        &rig.registered("PreToolUse", "Read"),
        &fixture_bytes("read_pre.json"),
        &off,
    );
    assert_eq!(
        (
            intercept.code,
            intercept.stdout.as_str(),
            intercept.stderr.as_str()
        ),
        (0, "", "")
    );
    assert!(!rig.spool.exists(), "{:?}", rig.faults());

    let meter = rig.run(
        &rig.registered("PostToolUse", "Read"),
        &fixture_bytes("read_post.json"),
        &off,
    );
    assert_eq!(meter.stderr, missing_line(&mcp, NOT_METERED));
    assert_eq!(rig.faults().len(), 1, "{:?}", rig.faults());
}

/// The fault the shell writes is the one `FaultRecord::now_with_env` builds from the
/// same variables, field for field but the second it was taken. `lumen report` reads
/// both, so a difference would split one kind of fault into two.
#[test]
fn the_shell_fault_is_the_binary_fault() {
    let rig = Rig::new();
    let mcp = rig.root.path().join("Lumen Test/bin/lumen-mcp");
    rig.install(&mcp);
    let command = rig.registered("PostToolUse", "Read");
    let proj = rig.proj.to_string_lossy().into_owned();
    let cases: [&[(&str, &str)]; 12] = [
        &[],
        &[("CLAUDE_CODE_ENTRYPOINT", "claude-vscode")],
        &[("CLAUDE_CODE_ENTRYPOINT", "cli")],
        &[("CLAUDE_CODE_ENTRYPOINT", "sdk-ts")],
        &[("CLAUDE_CODE_ENTRYPOINT", ""), ("VSCODE_PID", "")],
        &[("VSCODE_PID", "123")],
        &[("VSCODE_CWD", &proj)],
        &[("LUMEN_CHANNEL", "desktop")],
        &[("LUMEN_CHANNEL", ""), ("CLAUDE_CODE_ENTRYPOINT", "cli")],
        &[("CLAUDE_CODE_SESSION_ID", "s-1")],
        &[
            ("LUMEN_SESSION_ID", "l-1"),
            ("CLAUDE_CODE_SESSION_ID", "s-1"),
        ],
        &[("LUMEN_SESSION_ID", ""), ("CLAUDE_CODE_SESSION_ID", "s-1")],
    ];
    for env in cases {
        let _ = std::fs::remove_file(&rig.spool);
        rig.run(&command, &fixture_bytes("read_post.json"), env);
        let shell = rig
            .faults()
            .pop()
            .unwrap_or_else(|| panic!("{env:?}: no fault"));
        let mut binary =
            FaultRecord::now_with_env("meter_write_failed", "lumen_mcp_missing", |k| {
                env.iter()
                    .find(|(name, _)| *name == k)
                    .map(|(_, v)| v.to_string())
            });
        binary.ts = shell.ts.clone();
        assert_eq!(shell, binary, "{env:?}");
    }
}

/// Where the two part ways, pinned so that changing it is a decision. The shell will
/// not write a value that could break its JSON line, so such a session becomes null;
/// and it takes `LUMEN_CHANNEL` only as one plain word.
#[test]
fn the_shell_fault_leaves_out_what_it_cannot_write_safely() {
    let rig = Rig::new();
    let mcp = rig.root.path().join("Lumen Test/bin/lumen-mcp");
    rig.install(&mcp);
    let command = rig.registered("PostToolUse", "Read");
    let quoted = [("CLAUDE_CODE_SESSION_ID", "a\"b")];
    rig.run(&command, &fixture_bytes("read_post.json"), &quoted);
    let binary = FaultRecord::now_with_env("meter_write_failed", "lumen_mcp_missing", |k| {
        quoted
            .iter()
            .find(|(name, _)| *name == k)
            .map(|(_, v)| v.to_string())
    });
    assert_eq!(binary.session_id.as_deref(), Some("a\"b"));
    assert_eq!(rig.faults().pop().unwrap().session_id, None);

    rig.run(
        &command,
        &fixture_bytes("read_post.json"),
        &[("LUMEN_CHANNEL", "x y"), ("CLAUDE_CODE_ENTRYPOINT", "cli")],
    );
    assert_eq!(rig.faults().pop().unwrap().channel, "cli");
}

/// A home holding every character a shell treats specially. Each baked path is one
/// single-quoted word, so it reaches lumen-mcp as written, and the validator agrees
/// the install is sound. With the binary gone, the fault lands in that home's spool.
#[test]
fn a_home_full_of_shell_metacharacters_runs_the_hooks() {
    // Windows forbids `"` and `\` in a file name.
    let name = if cfg!(windows) {
        "Jane O'Brien $HOME `id` & (1) ; #x"
    } else {
        "Jane O'Brien \"$HOME\" `id` \\ & (1) ; #x"
    };
    let rig = Rig::with_home(name, SHIM_TOOLS);
    let mcp = rig.home.join("Lumen/bin/lumen-mcp");
    stub(&mcp, "baked");
    rig.install(&mcp);
    let command = rig.registered("PostToolUse", "Read");
    eprintln!("registered in an awkward home: {command}");

    let ran = rig.run(&command, &fixture_bytes("read_post.json"), &[]);
    assert_eq!((ran.code, ran.stderr.as_str()), (0, ""), "{ran:?}");
    let h = rig.handed().expect("lumen-mcp never ran");
    assert_eq!(h.name, "baked");
    assert_eq!(
        h.env,
        [
            shell_path(&rig.db.to_string_lossy()),
            shell_path(&rig.spool.to_string_lossy())
        ]
    );
    let health = validate_reported_artifacts_with(
        &rig.home,
        &rig.db.to_string_lossy(),
        &mcp.to_string_lossy(),
    );
    for id in ["scripts", "mcp", "hooks"] {
        let s = health.iter().find(|s| s.id == id).unwrap();
        assert!(s.healthy, "{s:?}");
    }

    std::fs::remove_file(&mcp).unwrap();
    let ran = rig.run(&command, &fixture_bytes("read_post.json"), &[]);
    assert_eq!(ran.stderr, missing_line(&mcp, NOT_METERED));
    assert_eq!(rig.faults().len(), 1, "{:?}", rig.faults());
}

/// Every baked value is a default. LUMEN_MCP_BIN, LUMEN_DB and LUMEN_FAULT_SPOOL from
/// the environment win, and reach lumen-mcp as they were given.
#[test]
fn the_environment_overrides_every_baked_value() {
    let rig = Rig::new();
    let baked = rig.root.path().join("Lumen Test/bin/lumen-mcp");
    stub(&baked, "baked");
    let other = rig.root.path().join("other/lumen-mcp");
    stub(&other, "B");
    rig.install(&baked);
    let db = rig.root.path().join("elsewhere/lumen.db");
    let spool = rig.root.path().join("elsewhere/faults.jsonl");
    let (other, db, spool) = (
        other.to_string_lossy().into_owned(),
        db.to_string_lossy().into_owned(),
        spool.to_string_lossy().into_owned(),
    );
    let ran = rig.run(
        &rig.registered("PostToolUse", "Read"),
        &fixture_bytes("read_post.json"),
        &[
            ("LUMEN_MCP_BIN", &other),
            ("LUMEN_DB", &db),
            ("LUMEN_FAULT_SPOOL", &spool),
        ],
    );
    assert_eq!((ran.code, ran.stderr.as_str()), (0, ""), "{ran:?}");
    let h = rig.handed().expect("lumen-mcp never ran");
    assert_eq!(h.name, "B");
    assert_eq!(h.env, [db, spool]);
}

/// The baked binary first, then `lumen-mcp` on PATH.
#[test]
fn without_the_baked_binary_the_hooks_use_lumen_mcp_on_path() {
    let rig = Rig::new();
    let baked = rig.root.path().join("Lumen Test/bin/lumen-mcp");
    stub(&baked, "baked");
    stub(&rig.bin.join("lumen-mcp"), "P");
    rig.install(&baked);
    let command = rig.registered("PostToolUse", "Read");

    rig.run(&command, &fixture_bytes("read_post.json"), &[]);
    assert_eq!(rig.handed().expect("lumen-mcp never ran").name, "baked");
    std::fs::remove_file(&baked).unwrap();
    let ran = rig.run(&command, &fixture_bytes("read_post.json"), &[]);
    assert_eq!((ran.code, ran.stderr.as_str()), (0, ""), "{ran:?}");
    assert_eq!(rig.handed().expect("lumen-mcp never ran").name, "P");
    assert!(!rig.spool.exists(), "{:?}", rig.faults());
}

// ── C2: the real lumen-mcp ──────────────────────────────────────────────────

/// A Read Claude Code reported, through the hook Setup registered, into the ledger:
/// every column the meter writes.
#[test]
fn a_captured_read_lands_as_a_row_through_the_installed_hook() {
    let rig = Rig::new();
    rig.install(&rig.real_mcp());
    let file = rig.file("hello.rs", HELLO);
    let ran = rig.meter(
        &rig.captured("read_post.json", Some(&file)),
        &rig.claude_env(READ_SESSION),
    );
    assert_eq!(ran.stderr, "");

    let rows = rig.rows();
    eprintln!("C2 row [{OS}] ({}): {rows:?}", rig.bash_version());
    assert_eq!(rows.len(), 1, "{rows:?}");
    let r = &rows[0];
    let path = file.to_string_lossy();
    let tokens = count_tokens(HELLO) as i64;
    assert!(tokens > 0);
    assert_eq!((r.tool.as_str(), r.path.as_str()), ("Read", path.as_ref()));
    assert_eq!(r.lines, Some(3));
    assert_eq!(
        (r.tokens_returned, r.full_tokens, r.saved_tokens),
        (tokens, tokens, 0)
    );
    assert_eq!(r.routed_via, "builtin_read");
    assert_eq!(r.channel, "cli");
    assert_eq!(r.session_id.as_deref(), Some(READ_SESSION));
    assert_eq!(r.file_mtime, Some(MTIME));
    assert_eq!(r.req_key.as_deref(), Some(path.as_ref()));
    assert_eq!(r.writer_hook.as_deref(), Some("lumen_meter.sh"));
    assert_eq!(r.token_source.as_deref(), Some("measured"));
    assert!(!rig.spool.exists(), "{:?}", rig.faults());
}

/// Bash output, observed through the installed hook: one `bash_output` row, labelled
/// with the program and subcommand only.
#[test]
fn a_captured_bash_payload_lands_as_a_bash_output_row() {
    let rig = Rig::new();
    rig.install(&rig.real_mcp());
    let payload = rig.captured("bash_post.json", None);
    let stdout = payload["tool_response"]["stdout"]
        .as_str()
        .unwrap()
        .to_string();
    let ran = rig.meter(&payload, &rig.claude_env(BASH_SESSION));
    assert_eq!(ran.stderr, "");

    let rows = rig.rows();
    eprintln!("C2 row [{OS}] ({}): {rows:?}", rig.bash_version());
    assert_eq!(rows.len(), 1, "{rows:?}");
    let r = &rows[0];
    assert_eq!((r.tool.as_str(), r.path.as_str()), ("Bash", "ls -la"));
    assert_eq!(r.lines, None);
    assert_eq!(
        (r.tokens_returned, r.full_tokens, r.saved_tokens),
        (0, count_tokens(&stdout) as i64, 0)
    );
    assert!(r.full_tokens > 0);
    assert_eq!(r.routed_via, "bash_output");
    assert_eq!(r.channel, "cli");
    assert_eq!(r.session_id.as_deref(), Some(BASH_SESSION));
    assert_eq!((r.file_mtime, r.req_key.as_deref()), (None, None));
    assert_eq!(r.token_source.as_deref(), Some("measured"));
    assert!(!rig.spool.exists(), "{:?}", rig.faults());
}

/// The redirect through the installed hook: 299 lines pass, 400 are blocked once with
/// the redirect on stderr, the marker lands in the TMPDIR Claude Code passed, and the
/// retry the message promises goes through and is recorded.
#[test]
fn a_large_read_is_blocked_once_through_the_installed_hook() {
    let rig = Rig::new();
    rig.install(&rig.real_mcp());
    let env = rig.claude_env(READ_SESSION);

    let small = rig.file("small.rs", &source_lines(299));
    let passed = rig.intercept(&rig.captured("read_pre.json", Some(&small)), &env);
    assert_eq!((passed.code, passed.stderr.as_str()), (0, ""), "{passed:?}");

    let big = rig.file("big.rs", &source_lines(400));
    let payload = rig.captured("read_pre.json", Some(&big));
    let path = big.to_string_lossy();
    let first = rig.intercept(&payload, &env);
    eprintln!("C2 block [{OS}] (exit {}):\n{}", first.code, first.stderr);
    assert_eq!(first.code, 2, "{first:?}");
    assert!(
        first
            .stderr
            .starts_with(&format!("Lumen intercept: {path} is 400 lines.\n")),
        "{:?}",
        first.stderr
    );
    assert!(first
        .stderr
        .contains(&format!("lumen:smart_read(path=\"{path}\")")));
    assert!(
        !rig.spool.exists(),
        "a block is not a fault: {:?}",
        rig.faults()
    );
    assert!(
        rig.tmp
            .join(format!("lumen_intercept_{READ_SESSION}"))
            .is_file(),
        "the marker must land in the hook's TMPDIR"
    );

    let retry = rig.intercept(&payload, &env);
    assert_eq!((retry.code, retry.stderr.as_str()), (0, ""), "{retry:?}");
    let faults = rig.faults();
    assert_eq!(faults.len(), 1, "{faults:?}");
    let f = &faults[0];
    assert_eq!(
        (f.kind.as_str(), f.variant.as_str()),
        ("hook_fail_open", "retry_escape_valve")
    );
    assert_eq!(f.path.as_deref(), Some(path.as_ref()));
    assert_eq!(f.lines, Some(400));
    assert_eq!(f.session_id.as_deref(), Some(READ_SESSION));
    assert_eq!(f.channel, "cli");
}

#[test]
fn a_large_log_read_is_sent_to_compress_logs_through_the_installed_hook() {
    let rig = Rig::new();
    rig.install(&rig.real_mcp());
    for name in ["build.log", "task.output"] {
        let log = rig.file(name, &"ok\n".repeat(400));
        let ran = rig.intercept(
            &rig.captured("read_pre.json", Some(&log)),
            &rig.claude_env(READ_SESSION),
        );
        assert_eq!(ran.code, 2, "{name}: {ran:?}");
        assert!(
            ran.stderr.contains(&format!(
                "lumen:compress_logs(path=\"{}\")",
                log.to_string_lossy()
            )),
            "{name}: {ran:?}"
        );
    }
}

/// Fail open, with its negative control: the hooks block a large read while lumen-mcp
/// is there, and once it is deleted they let the next one through, say so, and record
/// it against the session.
#[test]
fn deleting_lumen_mcp_turns_a_block_into_a_reported_pass() {
    let rig = Rig::new();
    let mcp = rig.real_mcp();
    rig.install(&mcp);
    let env = rig.claude_env(READ_SESSION);

    let first = rig.file("first.rs", &source_lines(400));
    let control = rig.intercept(&rig.captured("read_pre.json", Some(&first)), &env);
    assert_eq!(
        control.code, 2,
        "control: with lumen-mcp there, this is blocked"
    );

    std::fs::remove_file(&mcp).unwrap();
    let second = rig.file("second.rs", &source_lines(400));
    let passed = rig.intercept(&rig.captured("read_pre.json", Some(&second)), &env);
    let metered = rig.meter(&rig.captured("read_post.json", Some(&second)), &env);
    eprintln!(
        "C2 fail-open [{OS}]: intercept exit {} stderr {:?}; meter exit {} stderr {:?}",
        passed.code, passed.stderr, metered.code, metered.stderr
    );
    assert_eq!(passed.code, 0);
    assert_eq!(passed.stderr, missing_line(&mcp, LET_THROUGH));
    assert_eq!(metered.stderr, missing_line(&mcp, NOT_METERED));
    let faults = rig.faults();
    let got: Vec<_> = faults
        .iter()
        .map(|f| {
            (
                f.kind.as_str(),
                f.variant.as_str(),
                f.session_id.as_deref(),
                f.channel.as_str(),
            )
        })
        .collect();
    assert_eq!(
        got,
        [
            (
                "hook_fail_open",
                "lumen_mcp_missing",
                Some(READ_SESSION),
                "cli"
            ),
            (
                "meter_write_failed",
                "lumen_mcp_missing",
                Some(READ_SESSION),
                "cli"
            )
        ]
    );
    assert!(!rig.db.exists(), "nothing was metered");
}

/// The real binary, found on PATH once the baked one is gone.
#[test]
fn the_real_lumen_mcp_on_path_meters_when_the_baked_one_is_gone() {
    let rig = Rig::new();
    let mcp = rig.real_mcp();
    rig.install(&mcp);
    std::fs::rename(
        &mcp,
        rig.bin
            .join(format!("lumen-mcp{}", std::env::consts::EXE_SUFFIX)),
    )
    .unwrap();
    let file = rig.file("hello.rs", HELLO);
    let ran = rig.meter(
        &rig.captured("read_post.json", Some(&file)),
        &rig.claude_env(READ_SESSION),
    );
    assert_eq!(ran.stderr, "");
    assert_eq!(rig.rows().len(), 1);
}

// ── Pythons ─────────────────────────────────────────────────────────────────
//
// The layouts the 1.5.1 hooks failed on, built explicitly. The stand-ins are shell
// scripts, not the executables Windows ships; what they reproduce is what a hook's
// `command -v` and exit status see.

/// One layout: `stubs` on PATH, the probe's view of it, then a row and a block from
/// hooks installed on it.
fn python_layout(label: &str, stubs: &[(&str, &str)], want: &[&str]) -> Ran {
    let rig = Rig::new();
    for (name, body) in stubs {
        let p = rig.bin.join(name);
        std::fs::write(&p, body).unwrap();
        set_mode(&p, 0o755).unwrap();
    }
    let probe = rig.probe(label, PYTHON_PROBE);
    for line in want {
        assert!(
            probe.stdout.lines().any(|l| l == *line),
            "{label}: want {line:?}: {probe:?}"
        );
    }

    rig.install(&rig.real_mcp());
    let env = rig.claude_env(READ_SESSION);
    let file = rig.file("hello.rs", HELLO);
    let metered = rig.meter(&rig.captured("read_post.json", Some(&file)), &env);
    assert_eq!(metered.stderr, "", "{label}");
    let rows = rig.rows();
    assert_eq!(rows.len(), 1, "{label}: {rows:?}");
    let big = rig.file("big.rs", &source_lines(400));
    let blocked = rig.intercept(&rig.captured("read_pre.json", Some(&big)), &env);
    eprintln!(
        "layout [{OS}] {label}: row {:?}; 400-line read exit {}",
        rows[0], blocked.code
    );
    assert_eq!(blocked.code, 2, "{label}: {blocked:?}");
    probe
}

/// Layout 1, python.org's Windows install: `python` and `py`, no `python3`.
#[test]
fn python_and_py_without_python3_meter_and_block() {
    python_layout(
        "python + py, no python3",
        &[("python", PYTHON_STUB), ("py", PYTHON_STUB)],
        &[
            "python3: absent",
            "python: found",
            "py: found",
            "python3 exit 127",
        ],
    );
}

/// Layout 2, no Python at all. 1.5.1 wrote nothing and blocked nothing here; the spec
/// expected a fault and a stderr line from the shims, and with the hooks in Rust there
/// is nothing to fail, so the result is a row and a block.
#[test]
fn no_python_at_all_meters_and_blocks() {
    python_layout(
        "no Python",
        &[],
        &[
            "python3: absent",
            "python: absent",
            "py: absent",
            "python3 exit 127",
        ],
    );
}

/// Layout 2 as Windows has it out of the box: `python3` is the Store alias, which runs
/// and fails.
#[test]
fn a_python3_that_only_offers_the_store_meters_and_blocks() {
    let probe = python_layout(
        "python3 is the Store alias",
        &[("python3", STORE_STUB)],
        &[
            "python3: found",
            "python: absent",
            "py: absent",
            "python3 exit 49",
        ],
    );
    assert!(probe.stderr.contains("Python was not found"), "{probe:?}");
}

// ── R2: 1.5.1 against 1.6.0 on a machine with no python3 ────────────────────

/// Before: the hooks 1.5.1 installed. The meter exits 0 having written nothing, and its
/// one word, `python3: command not found`, goes to a stderr Claude Code does not show
/// on exit 0. The intercept lets a 400-line read through in silence. Neither leaves a
/// fault: the fault recorder ran on python3 as well.
///
/// The control, on macOS and Linux: python3 back on PATH, and the same scripts write a
/// row and block, so the silence is python3's absence and nothing else.
///
/// After: Setup from this build on the same machine — a row and a block.
#[test]
fn without_python3_the_1_5_1_hooks_did_nothing_and_these_hooks_work() {
    let rig = Rig::with_home("home", V151_TOOLS);
    let real = rig.real_mcp();
    let (meter, intercept) = rig.v151_scripts();
    // Quoted, unlike 1.5.1's registration, so that the one thing this compares is
    // python3. The bare path is the upgrade test's subject.
    write_json(
        &global_settings_path_in(&rig.home),
        &json!({"hooks": {
            "PreToolUse": [
                {"matcher": "Read", "hooks": [{"type": "command", "command": hook_command(&intercept)}]}
            ],
            "PostToolUse": [
                {"matcher": "Read", "hooks": [{"type": "command", "command": hook_command(&meter)}]},
                {"matcher": "Bash", "hooks": [{"type": "command", "command": hook_command(&meter)}]}
            ]
        }}),
    );
    Connection::open(&rig.db)
        .unwrap()
        .execute_batch(V151_READ_EVENTS)
        .unwrap();
    let db = rig.db.to_string_lossy().into_owned();
    let spool = rig.spool.to_string_lossy().into_owned();
    let mcp = real.to_string_lossy().into_owned();
    // 1.5.1 tokenized with lumen-tok; without it, it still wrote a row, labelled an
    // estimate.
    let tok = rig.root.path().join("no-lumen-tok");
    let tok = tok.to_string_lossy().into_owned();
    let mut env = vec![
        ("LUMEN_DB", db.as_str()),
        ("LUMEN_FAULT_SPOOL", spool.as_str()),
        ("LUMEN_MCP_BIN", mcp.as_str()),
        ("LUMEN_TOK", tok.as_str()),
    ];
    env.extend(rig.claude_env(READ_SESSION));
    let hello = rig.file("hello.rs", HELLO);
    let read = rig.captured("read_post.json", Some(&hello));

    let metered = rig.meter(&read, &env);
    let before = rig.file("before.rs", &source_lines(400));
    let let_through = rig.intercept(&rig.captured("read_pre.json", Some(&before)), &env);
    eprintln!(
        "R2 before [{OS}]: meter exit {} stderr {:?}; intercept exit {} stderr {:?}",
        metered.code, metered.stderr, let_through.code, let_through.stderr
    );
    assert!(rig.rows().is_empty(), "{:?}", rig.rows());
    assert_eq!(let_through.code, 0, "{let_through:?}");
    assert!(!rig.spool.exists(), "{:?}", rig.faults());

    if cfg!(unix) {
        link_tool(&rig.bin, "python3");
        rig.meter(&read, &env);
        let control = rig.file("control.rs", &source_lines(400));
        let blocked = rig.intercept(&rig.captured("read_pre.json", Some(&control)), &env);
        let rows = rig.rows();
        eprintln!(
            "R2 control [{OS}], python3 on PATH: rows {rows:?}; intercept exit {}",
            blocked.code
        );
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].token_source.as_deref(), Some("estimated"));
        assert_eq!(blocked.code, 2, "{blocked:?}");
        std::fs::remove_file(rig.bin.join("python3")).unwrap();
    } else {
        eprintln!("R2 control [{OS}]: skipped — this runner has no python3 to put on PATH");
    }

    let kept = rig.rows().len();
    rig.install(&real);
    let metered = rig.meter(&read, &env);
    let after = rig.file("after.rs", &source_lines(400));
    let blocked = rig.intercept(&rig.captured("read_pre.json", Some(&after)), &env);
    let rows = rig.rows();
    eprintln!(
        "R2 after [{OS}]: meter stderr {:?}; new row {:?}; intercept exit {}",
        metered.stderr,
        rows.last(),
        blocked.code
    );
    assert_eq!(metered.stderr, "");
    assert_eq!(rows.len(), kept + 1, "{rows:?}");
    assert_eq!(rows[kept].token_source.as_deref(), Some("measured"));
    assert_eq!(blocked.code, 2, "{blocked:?}");
}

// ── C3: installs and upgrades ───────────────────────────────────────────────

/// A fresh install: every artifact Setup reports on is healthy, the login item is on,
/// Setup is marked done, and the hooks meter and block.
#[test]
fn a_fresh_install_is_valid_and_its_hooks_work() {
    let rig = Rig::new();
    let mcp = rig.real_mcp();
    let item = LoginItem::off();
    let steps = rig.install_as(&mcp, &item);
    let health = validate_reported_artifacts_with(
        &rig.home,
        &rig.db.to_string_lossy(),
        &mcp.to_string_lossy(),
    );
    eprintln!("C3 fresh [{OS}] steps: {steps:#?}\nC3 fresh [{OS}] health: {health:#?}");
    assert!(item.0.get(), "Setup turns the login item on");
    assert!(marker_path_in(&rig.home).exists());
    let ids: Vec<&str> = health.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids, PERSISTED_ARTIFACTS);
    // `cli` is the machine's own symlink, not the temp home's.
    for s in health.iter().filter(|s| s.id != "cli") {
        assert!(s.healthy, "{s:?}");
    }

    let env = rig.claude_env(READ_SESSION);
    let file = rig.file("hello.rs", HELLO);
    rig.meter(&rig.captured("read_post.json", Some(&file)), &env);
    assert_eq!(rig.rows().len(), 1);
    let big = rig.file("big.rs", &source_lines(400));
    let blocked = rig.intercept(&rig.captured("read_pre.json", Some(&big)), &env);
    assert_eq!(blocked.code, 2, "{blocked:?}");
}

/// An upgrade from 1.5.1, in a home with a space in it.
///
/// The first launch of this build rewrites the two scripts and touches nothing else:
/// settings.json and ~/.claude.json stay byte-identical, and a login item the user
/// switched off stays off. The commands 1.5.1 registered are bare paths, which in this
/// home never ran; the validator says so and asks for Setup. Re-running it quotes them
/// and keeps everything else the user had: settings, other hooks, other MCP servers,
/// and opt-outs that go on working.
#[test]
fn an_upgrade_from_1_5_1_refreshes_the_scripts_and_keeps_what_the_user_set() {
    let rig = Rig::with_home("Jane Doe", SHIM_TOOLS);
    let real = rig.real_mcp();
    let tok = real.with_file_name(format!("lumen-tok{}", std::env::consts::EXE_SUFFIX));
    std::fs::write(&tok, "").unwrap();
    let (meter, intercept) = rig.v151_scripts();
    let native = |p: &Path| p.to_string_lossy().into_owned();
    let (db, mcp) = (native(&rig.db), native(&real));

    let settings = global_settings_path_in(&rig.home);
    let opt_outs = json!({"LUMEN_HOOK_ENABLED": "0", "LUMEN_CAPTURE": "0"});
    let audit = json!({"type": "command", "command": "/usr/local/bin/audit-read"});
    let fmt = json!({"type": "command", "command": "/usr/local/bin/fmt-check"});
    write_json(
        &settings,
        &json!({
            "model": "opus",
            "env": opt_outs,
            "hooks": {
                "PreToolUse": [{"matcher": "Read", "hooks": [
                    audit,
                    {"type": "command", "command": native(&intercept)}
                ]}],
                "PostToolUse": [
                    {"matcher": "Read", "hooks": [{"type": "command", "command": native(&meter)}]},
                    {"matcher": "Bash", "hooks": [{"type": "command", "command": native(&meter)}]},
                    {"matcher": "Edit", "hooks": [fmt]}
                ]
            }
        }),
    );
    let claude_json = claude_json_path_in(&rig.home);
    let github = json!({"type": "stdio", "command": "/usr/local/bin/github-mcp", "args": ["--read-only"], "env": {}});
    write_json(
        &claude_json,
        &json!({
            "numStartups": 42,
            "mcpServers": {
                "lumen": {
                    "type": "stdio", "command": mcp, "args": [],
                    "env": {"LUMEN_DB": db, "LUMEN_TOK": native(&tok)}
                },
                "github": github
            }
        }),
    );
    let marker = autostart_marker_in(&rig.home);
    std::fs::write(&marker, APP_EXE).unwrap();
    let item = LoginItem::off();
    let original = (
        std::fs::read(&settings).unwrap(),
        std::fs::read(&claude_json).unwrap(),
    );

    // 1. The first launch of this build.
    assert_eq!(
        ensure_scripts_fresh_in(&rig.home, &db, &mcp).as_deref(),
        Some("refreshed lumen_meter.sh, lumen_read_intercept.sh")
    );
    for p in [&meter, &intercept] {
        let text = std::fs::read_to_string(p).unwrap();
        assert!(text.contains(&stamp_line()), "{}:\n{text}", p.display());
    }
    assert!(!ensure_autostart_once(&item, &marker, APP_EXE));
    assert!(!item.0.get(), "a login item switched off stays off");
    assert!(
        std::fs::read(&settings).unwrap() == original.0,
        "settings.json changed before Setup was re-run"
    );
    assert!(
        std::fs::read(&claude_json).unwrap() == original.1,
        "~/.claude.json changed before Setup was re-run"
    );

    // 2. What 1.5.1 registered never ran in this home, and the validator says so.
    let hooks = validate_reported_artifacts_with(&rig.home, &db, &mcp)
        .into_iter()
        .find(|s| s.id == "hooks")
        .unwrap();
    let bare = rig.run(&native(&meter), &fixture_bytes("read_post.json"), &[]);
    eprintln!(
        "C3 upgrade [{OS}] hooks before Setup is re-run: {hooks:?}\n\
         C3 upgrade [{OS}] the command 1.5.1 registered: exit {}, stderr {:?}",
        bare.code, bare.stderr
    );
    assert!(
        !hooks.healthy
            && hooks
                .detail
                .contains("will not run as registered (unquoted path)"),
        "{hooks:?}"
    );
    assert_eq!(bare.code, 127, "{bare:?}");
    assert!(!rig.db.exists(), "{bare:?}");

    // The flag is for what breaks, not for the form. Without a space in the home a bare
    // path runs on macOS and Linux, and the validator leaves it be; on Windows Claude
    // Code's scan takes its backslashes as escapes, so it never ran there either.
    let plain = Rig::new();
    let plain_mcp = plain.real_mcp();
    let (plain_meter, plain_intercept) = plain.v151_scripts();
    ensure_scripts_fresh_in(&plain.home, &native(&plain.db), &native(&plain_mcp));
    write_json(
        &global_settings_path_in(&plain.home),
        &json!({"hooks": {
            "PreToolUse": [
                {"matcher": "Read", "hooks": [{"type": "command", "command": native(&plain_intercept)}]}
            ],
            "PostToolUse": [
                {"matcher": "Read", "hooks": [{"type": "command", "command": native(&plain_meter)}]},
                {"matcher": "Bash", "hooks": [{"type": "command", "command": native(&plain_meter)}]}
            ]
        }}),
    );
    let flagged =
        validate_reported_artifacts_with(&plain.home, &native(&plain.db), &native(&plain_mcp))
            .into_iter()
            .find(|s| s.id == "hooks")
            .unwrap();
    let file = plain.file("hello.rs", HELLO);
    let ran = plain.run(
        &native(&plain_meter),
        &serde_json::to_vec(&plain.captured("read_post.json", Some(&file))).unwrap(),
        &[],
    );
    eprintln!(
        "C3 upgrade [{OS}] a bare command, no space in the home: exit {}; {flagged:?}",
        ran.code
    );
    assert_eq!(flagged.healthy, !cfg!(windows), "{flagged:?}");
    assert_eq!(
        flagged.detail.contains("unquoted path"),
        cfg!(windows),
        "{flagged:?}"
    );
    assert_eq!(
        ran.code == 0 && plain.db.exists(),
        !cfg!(windows),
        "{ran:?}"
    );

    // 3. Setup, re-run.
    let steps = rig.install_as(&real, &item);
    let autostart = steps.iter().find(|s| s.id == AUTOSTART_ID).unwrap();
    assert_eq!(
        autostart.detail,
        "Left off, as you set it — the toggle turns it back on"
    );
    assert!(!item.0.get());
    let s: Value = serde_json::from_slice(&std::fs::read(&settings).unwrap()).unwrap();
    assert_eq!(s["model"], "opus");
    assert_eq!(s["env"], opt_outs);
    assert_eq!(
        s["hooks"]["PreToolUse"][0]["hooks"],
        json!([audit, {"type": "command", "command": hook_command(&intercept)}])
    );
    assert_eq!(rig.registered("PostToolUse", "Read"), hook_command(&meter));
    assert_eq!(rig.registered("PostToolUse", "Bash"), hook_command(&meter));
    let edit = s["hooks"]["PostToolUse"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["matcher"] == "Edit")
        .unwrap();
    assert_eq!(edit["hooks"], json!([fmt]));
    let c: Value = serde_json::from_slice(&std::fs::read(&claude_json).unwrap()).unwrap();
    assert_eq!(c["numStartups"], 42);
    assert_eq!(c["mcpServers"]["github"], github);
    assert_eq!(
        c["mcpServers"]["lumen"],
        json!({"type": "stdio", "command": mcp, "args": [], "env": {"LUMEN_DB": db}})
    );
    assert!(std::fs::read(settings.with_extension("json.lumen_bak")).unwrap() == original.0);
    assert!(std::fs::read(claude_json.with_extension("json.lumen_bak")).unwrap() == original.1);
    let health = validate_reported_artifacts_with(&rig.home, &db, &mcp);
    eprintln!("C3 upgrade [{OS}] health after Setup: {health:#?}");
    for st in health.iter().filter(|st| st.id != "cli") {
        assert!(st.healthy, "{st:?}");
    }

    // 4. The opt-outs reach the hooks, as Claude Code applies settings.json's `env`.
    let applied: Vec<(String, String)> = s["env"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(k, v)| (k.clone(), v.as_str().unwrap().to_string()))
        .collect();
    let mut env: Vec<(&str, &str)> = applied
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    env.extend(rig.claude_env(READ_SESSION));
    let hello = rig.file("hello.rs", HELLO);
    let metered = rig.meter(&rig.captured("read_post.json", Some(&hello)), &env);
    assert_eq!(metered.stderr, "");
    assert_eq!(
        rig.rows().len(),
        1,
        "LUMEN_CAPTURE=0 drops faults, not rows"
    );
    let big = rig.file("big.rs", &source_lines(400));
    let passed = rig.intercept(&rig.captured("read_pre.json", Some(&big)), &env);
    assert_eq!((passed.code, passed.stderr.as_str()), (0, ""), "{passed:?}");
    let gone = rig.proj.join("deleted.rs");
    rig.meter(&rig.captured("read_post.json", Some(&gone)), &env);
    assert!(!rig.spool.exists(), "{:?}", rig.faults());

    // Their controls: without the opt-outs the same reads are blocked and recorded.
    let bare_env = rig.claude_env(READ_SESSION);
    let other = rig.file("other.rs", &source_lines(400));
    let blocked = rig.intercept(&rig.captured("read_pre.json", Some(&other)), &bare_env);
    assert_eq!(blocked.code, 2, "{blocked:?}");
    rig.meter(&rig.captured("read_post.json", Some(&gone)), &bare_env);
    assert_eq!(rig.faults().len(), 1, "{:?}", rig.faults());
}

// ── The plugin's copies ─────────────────────────────────────────────────────
//
// The Claude Code plugin registers `hooks/hooks.json`, which runs the scripts in
// `.claude/hooks/`: Setup's templates with nothing baked in (`setup::plugin_hooks`).
// These run the checked-in copies the way Claude Code runs a plugin's hooks, on a
// machine where Setup never ran. A plugin's root is wherever Claude Code put it, so
// this one has a space, a `$`, a backtick and quotes in its name.

const PLUGIN: &str = "lumen $HOME `x` 'q' (1.6.0)";
/// What the plugin's meter writes in `writer_hook`.
const PLUGIN_WRITER: &str = "repo:.claude/hooks/lumen_meter.sh";

/// A Read and a Bash through the plugin's meter land as rows in the ledger lumen-mcp
/// resolves for itself, and a large read is blocked, with no Setup anywhere.
#[test]
fn the_plugin_hooks_meter_and_block_without_setup() {
    let rig = Rig::new();
    let root = rig.plugin(PLUGIN, true);
    assert!(
        !global_settings_path_in(&rig.home).exists(),
        "Setup never ran"
    );

    let file = rig.file("hello.rs", HELLO);
    let read = rig.plugin_hook(
        &root,
        "PostToolUse",
        &rig.captured("read_post.json", Some(&file)),
        &rig.claude_env(READ_SESSION),
    );
    let bash = rig.plugin_hook(
        &root,
        "PostToolUse",
        &rig.captured("bash_post.json", None),
        &rig.claude_env(BASH_SESSION),
    );
    assert_eq!((read.stderr.as_str(), bash.stderr.as_str()), ("", ""));

    let rows = rig.rows();
    eprintln!("C2 plugin rows [{OS}] ({}): {rows:?}", rig.bash_version());
    let path = file.to_string_lossy();
    let got: Vec<_> = rows
        .iter()
        .map(|r| {
            (
                r.tool.as_str(),
                r.path.as_str(),
                r.routed_via.as_str(),
                r.channel.as_str(),
                r.session_id.as_deref(),
                r.writer_hook.as_deref(),
                r.token_source.as_deref(),
            )
        })
        .collect();
    assert_eq!(
        got,
        [
            (
                "Read",
                path.as_ref(),
                "builtin_read",
                "cli",
                Some(READ_SESSION),
                Some(PLUGIN_WRITER),
                Some("measured")
            ),
            (
                "Bash",
                "ls -la",
                "bash_output",
                "cli",
                Some(BASH_SESSION),
                Some(PLUGIN_WRITER),
                Some("measured")
            ),
        ]
    );
    assert_eq!(rows[0].tokens_returned, count_tokens(HELLO) as i64);
    assert!(rows[1].full_tokens > 0, "{:?}", rows[1]);

    let big = rig.file("big.rs", &source_lines(400));
    let blocked = rig.plugin_hook(
        &root,
        "PreToolUse",
        &rig.captured("read_pre.json", Some(&big)),
        &rig.claude_env(READ_SESSION),
    );
    eprintln!(
        "C2 plugin block [{OS}] (exit {}):\n{}",
        blocked.code, blocked.stderr
    );
    assert_eq!(blocked.code, 2, "{blocked:?}");
    assert!(
        blocked.stderr.starts_with(&format!(
            "Lumen intercept: {} is 400 lines.\n",
            big.to_string_lossy()
        )),
        "{blocked:?}"
    );
    assert!(!rig.spool.exists(), "{:?}", rig.faults());
}

/// Without a build in the checkout, the plugin's hooks use lumen-mcp on PATH.
#[test]
fn without_a_build_the_plugin_hooks_use_lumen_mcp_on_path() {
    let rig = Rig::new();
    let root = rig.plugin(PLUGIN, false);
    std::fs::copy(
        sidecar(),
        rig.bin
            .join(format!("lumen-mcp{}", std::env::consts::EXE_SUFFIX)),
    )
    .unwrap();
    let file = rig.file("hello.rs", HELLO);
    let ran = rig.plugin_hook(
        &root,
        "PostToolUse",
        &rig.captured("read_post.json", Some(&file)),
        &rig.claude_env(READ_SESSION),
    );
    assert_eq!(ran.stderr, "");
    let rows = rig.rows();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].writer_hook.as_deref(), Some(PLUGIN_WRITER));
}

/// Fail open through the plugin, with its negative control: a large read is blocked
/// while the checkout's build is there; once it is gone and nothing is on PATH, the
/// next one goes through, both hooks say where they looked, and the faults carry the
/// session and the version stamped on the script.
#[test]
fn without_lumen_mcp_the_plugin_hooks_say_so_record_it_and_block_nothing() {
    let rig = Rig::new();
    let root = rig.plugin(PLUGIN, true);
    let env = rig.claude_env(READ_SESSION);

    let first = rig.file("first.rs", &source_lines(400));
    let control = rig.plugin_hook(
        &root,
        "PreToolUse",
        &rig.captured("read_pre.json", Some(&first)),
        &env,
    );
    assert_eq!(
        control.code, 2,
        "control: with the build there, this is blocked"
    );

    std::fs::remove_file(
        root.join("target/release")
            .join(format!("lumen-mcp{}", std::env::consts::EXE_SUFFIX)),
    )
    .unwrap();
    let second = rig.file("second.rs", &source_lines(400));
    let passed = rig.plugin_hook(
        &root,
        "PreToolUse",
        &rig.captured("read_pre.json", Some(&second)),
        &env,
    );
    let metered = rig.plugin_hook(
        &root,
        "PostToolUse",
        &rig.captured("read_post.json", Some(&second)),
        &env,
    );
    eprintln!(
        "C2 plugin fail-open [{OS}]: intercept exit {} stderr {:?}; meter exit {} stderr {:?}",
        passed.code, passed.stderr, metered.code, metered.stderr
    );
    // The script names the binary by the path it was run by, the root as Claude Code
    // passed it.
    let looked = PathBuf::from(format!(
        "{}/.claude/hooks/../../target/release/lumen-mcp",
        shell_path(&root.to_string_lossy())
    ));
    assert_eq!(passed.code, 0);
    assert_eq!(passed.stderr, missing_line(&looked, LET_THROUGH));
    assert_eq!(metered.stderr, missing_line(&looked, NOT_METERED));
    let faults = rig.faults();
    let got: Vec<_> = faults
        .iter()
        .map(|f| {
            (
                f.kind.as_str(),
                f.variant.as_str(),
                f.session_id.as_deref(),
                f.version.as_deref(),
                f.channel.as_str(),
            )
        })
        .collect();
    let version = Some(env!("CARGO_PKG_VERSION"));
    assert_eq!(
        got,
        [
            (
                "hook_fail_open",
                "lumen_mcp_missing",
                Some(READ_SESSION),
                version,
                "cli"
            ),
            (
                "meter_write_failed",
                "lumen_mcp_missing",
                Some(READ_SESSION),
                version,
                "cli"
            )
        ]
    );
    assert!(!rig.db.exists(), "nothing was metered");
}

/// The plugin's script, unlike Setup's, has no spool baked in, so when it reports it
/// resolves the one lumen-mcp would have used. Each case runs both under the same
/// environment — lumen-mcp from a built root, the script from a root without a build
/// — and finds both faults in the one file the rule names, and nothing anywhere else.
/// The cases add one rule each, so each also shows the rule before it losing.
#[test]
fn the_plugin_script_reports_where_lumen_mcp_would_have() {
    let rig = Rig::new();
    let built = rig.plugin(PLUGIN, true);
    let unbuilt = rig.plugin("lumen unbuilt", false);
    let dir = |name: &str| {
        let d = rig.root.path().join(name);
        std::fs::create_dir_all(&d).unwrap();
        d
    };
    let pointed = dir("pointed db").join("lumen.db");
    let set = dir("env db").join("lumen.db");
    let spool = dir("spool dir").join("faults here.jsonl");
    let beside = |db: &Path| db.with_file_name("faults.jsonl");
    // Native paths, as the app writes the pointer and a user sets a variable.
    let native = |p: &Path| p.to_string_lossy().into_owned();
    let pointer = rig.home.join(".lumen_db_path");
    let candidates = [
        rig.spool.clone(),
        beside(&pointed),
        beside(&set),
        spool.clone(),
        rig.proj.join("faults.jsonl"),
    ];
    let points = format!(" \t{}\t \r\n", native(&pointed));
    let cases: [(&str, Option<&str>, Vec<(&str, String)>, &Path); 5] = [
        ("the per-OS ledger", None, vec![], &rig.spool),
        ("a blank pointer", Some(" \t\r\n"), vec![], &rig.spool),
        ("the pointer", Some(&points), vec![], &beside(&pointed)),
        (
            "LUMEN_DB over the pointer",
            Some(&points),
            vec![("LUMEN_DB", native(&set))],
            &beside(&set),
        ),
        (
            "LUMEN_FAULT_SPOOL over both",
            Some(&points),
            vec![
                ("LUMEN_DB", native(&set)),
                ("LUMEN_FAULT_SPOOL", native(&spool)),
            ],
            &spool,
        ),
    ];
    let big = rig.file("big.rs", &source_lines(400));
    for (i, (label, pointing, extra, want)) in cases.iter().enumerate() {
        for c in &candidates {
            let _ = std::fs::remove_file(c);
        }
        match pointing {
            Some(text) => std::fs::write(&pointer, text).unwrap(),
            None => {
                let _ = std::fs::remove_file(&pointer);
            }
        }
        let session = format!("spool-case-{i}");
        let mut payload = rig.captured("read_pre.json", Some(&big));
        payload["session_id"] = Value::from(session.as_str());
        let mut env = rig.claude_env(&session).to_vec();
        env.extend(extra.iter().map(|(k, v)| (*k, v.as_str())));

        let blocked = rig.plugin_hook(&built, "PreToolUse", &payload, &env);
        assert_eq!(blocked.code, 2, "{label}: {blocked:?}");
        let binary = rig.plugin_hook(&built, "PreToolUse", &payload, &env);
        let shell = rig.plugin_hook(&unbuilt, "PreToolUse", &payload, &env);
        assert_eq!(
            (binary.code, shell.code),
            (0, 0),
            "{label}: {binary:?} {shell:?}"
        );

        let landed: Vec<(&Path, Vec<(String, String, Option<String>)>)> = candidates
            .iter()
            .filter(|c| c.exists())
            .map(|c| {
                let faults = faults_in(c)
                    .into_iter()
                    .map(|f| (f.kind, f.variant, f.session_id))
                    .collect();
                (c.as_path(), faults)
            })
            .collect();
        eprintln!("spool [{OS}] {label}: {landed:?}");
        let fault = |variant: &str| {
            (
                "hook_fail_open".to_string(),
                variant.to_string(),
                Some(session.clone()),
            )
        };
        assert_eq!(
            landed,
            [(
                *want,
                vec![fault("retry_escape_valve"), fault("lumen_mcp_missing")]
            )],
            "{label}"
        );
    }
}

// ── scripts/verify-hooks.sh ──────────────────────────────────────────────────

/// What `scripts/verify-hooks.sh` runs, apart from the hooks and `sqlite3`.
const VERIFY_TOOLS: &[&str] = &[
    "bash", "cat", "date", "grep", "ln", "mkdir", "mktemp", "rm", "sed", "tr",
];

/// `scripts/verify-hooks.sh --expect <this build> <dir>`, as a user runs it: by path
/// on macOS and Linux, through Git Bash on Windows, with the rig's home and temp dir.
/// PATH holds the tools it needs and `sqlite3` if this machine has one, so a
/// `lumen-mcp` installed here cannot stand in for the one Setup baked. Returns the
/// exit code, each result line as (PASS|FAIL|SKIP, text), and the raw output.
fn verify_hooks(rig: &Rig, dir: &Path) -> (i32, Vec<(String, String)>, String) {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/verify-hooks.sh");
    let mut cmd = if cfg!(windows) {
        let mut c = Command::new(git_bash());
        c.arg(shell_path(&script.to_string_lossy()));
        c
    } else {
        Command::new(&script)
    };
    let tools = rig.root.path().join("verify-bin");
    std::fs::create_dir_all(&tools).unwrap();
    #[cfg(unix)]
    {
        for tool in VERIFY_TOOLS {
            link_tool(&tools, tool);
        }
        let host = std::env::var_os("PATH").unwrap_or_default();
        if let Some(found) = std::env::split_paths(&host)
            .map(|d| d.join("sqlite3"))
            .find(|p| p.is_file())
        {
            std::os::unix::fs::symlink(found, tools.join("sqlite3")).unwrap();
        }
    }
    // Windows: Git Bash puts its own directories first, as it does for a hook.
    let mut path = vec![tools];
    if let Some(root) = std::env::var_os("SYSTEMROOT") {
        path.push(PathBuf::from(root).join("System32"));
    }
    let out = cmd
        .args(["--expect", env!("CARGO_PKG_VERSION")])
        .arg(shell_path(&dir.to_string_lossy()))
        .env("PATH", std::env::join_paths(path).unwrap())
        .env("HOME", &rig.home)
        .env("USERPROFILE", &rig.home)
        .env("TMPDIR", &rig.tmp)
        .env("TMP", &rig.tmp)
        .env("TEMP", &rig.tmp)
        .stdin(Stdio::null())
        .output()
        .expect("run scripts/verify-hooks.sh");
    let raw = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let results = raw
        .lines()
        .filter_map(|line| {
            let plain = without_colour(line);
            let line = plain.trim_start();
            ["PASS", "FAIL", "SKIP"].iter().find_map(|status| {
                let text = line.strip_prefix(status)?.strip_prefix("  ")?;
                Some((status.to_string(), text.to_string()))
            })
        })
        .collect();
    (out.status.code().unwrap_or(-1), results, raw)
}

/// `line` without its ANSI colour codes.
fn without_colour(line: &str) -> String {
    let mut out = String::new();
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            chars.by_ref().find(|&c| c == 'm');
        } else {
            out.push(c);
        }
    }
    out
}

/// The texts of the results with `status`.
fn with_status<'a>(results: &'a [(String, String)], status: &str) -> Vec<&'a str> {
    results
        .iter()
        .filter(|(s, _)| s == status)
        .map(|(_, t)| t.as_str())
        .collect()
}

/// The verify-hooks scratch directories left in the rig's temp dir.
fn scratch_left(rig: &Rig) -> Vec<PathBuf> {
    std::fs::read_dir(&rig.tmp)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("lumen-verify-hooks."))
        })
        .collect()
}

/// What Setup installs passes every check, and checking it touched nothing of the
/// rig's: the hooks ran against the script's own scratch ledger, which it removed.
#[test]
fn verify_hooks_passes_the_hooks_setup_installs() {
    let rig = Rig::new();
    rig.install(&rig.real_mcp());
    let (code, results, raw) = verify_hooks(&rig, &lumen_dir_in(&rig.home));
    eprintln!("verify-hooks [{OS}] Setup's hooks: exit {code}\n{raw}");
    assert_eq!(results.len(), 13, "{raw}");
    assert_eq!(with_status(&results, "FAIL"), Vec::<&str>::new(), "{raw}");
    // The rows are read with sqlite3, and only its absence may skip that.
    for skipped in with_status(&results, "SKIP") {
        assert!(
            skipped.ends_with("there is no sqlite3 here to read its rows"),
            "{raw}"
        );
    }
    assert_eq!(code, 0, "{raw}");
    assert!(!rig.spool.exists(), "{:?}", rig.faults());
    assert!(
        !rig.db.exists() || rig.rows().is_empty(),
        "{:?}",
        rig.rows()
    );
    assert_eq!(scratch_left(&rig), Vec::<PathBuf>::new());
}

/// Negative control. The 1.5.1 hooks fail by name and are not run: nothing they
/// would have written exists, and they would have needed a python3 PATH lacks.
#[test]
fn verify_hooks_fails_the_hooks_1_5_1_installed_without_running_them() {
    let rig = Rig::new();
    rig.v151_scripts();
    let (code, results, raw) = verify_hooks(&rig, &lumen_dir_in(&rig.home));
    eprintln!("verify-hooks [{OS}] 1.5.1's hooks: exit {code}\n{raw}");
    let failed = with_status(&results, "FAIL");
    assert_eq!(failed.len(), 2, "{raw}");
    for (text, name) in failed
        .iter()
        .zip(["lumen_meter.sh", "lumen_read_intercept.sh"])
    {
        assert!(
            text.starts_with(&format!(
                "{name}, written by Lumen 1.5.1, predates `lumen-mcp hook`"
            )),
            "{raw}"
        );
    }
    assert_eq!(
        results.last().map(|(s, t)| (s.as_str(), t.as_str())),
        Some(("SKIP", "not run — fix the failures above first")),
        "{raw}"
    );
    assert_eq!(code, 1, "{raw}");
    assert!(!rig.db.exists() && !rig.spool.exists());
    assert_eq!(scratch_left(&rig), Vec::<PathBuf>::new());
}

/// Lumen moved or removed. The hooks still exit 0 and every guard is still in the
/// script for a grep to find; the check this replaced passed them. Run, they meter
/// nothing and block nothing, and verify-hooks says so.
#[test]
fn verify_hooks_fails_hooks_whose_lumen_mcp_is_gone() {
    let rig = Rig::new();
    let mcp = rig.real_mcp();
    rig.install(&mcp);
    std::fs::remove_file(&mcp).unwrap();
    let (code, results, raw) = verify_hooks(&rig, &lumen_dir_in(&rig.home));
    eprintln!("verify-hooks [{OS}] lumen-mcp gone: exit {code}\n{raw}");
    let failed = with_status(&results, "FAIL");
    for check in [
        "meter: a Read gave exit 0, stdout [], stderr [lumen: cannot run lumen-mcp",
        "intercept: a 400-line file gave exit 0, stdout [], stderr [lumen: cannot run lumen-mcp",
    ] {
        assert!(
            failed.iter().any(|t| t.starts_with(check)),
            "{check}\n{raw}"
        );
    }
    // What the hooks did do, they did as they should.
    let passed = with_status(&results, "PASS");
    for check in ["intercept without lumen-mcp", "meter without lumen-mcp"] {
        assert!(
            passed.iter().any(|t| t.starts_with(check)),
            "{check}\n{raw}"
        );
    }
    assert_eq!(code, 1, "{raw}");
    assert!(!rig.spool.exists(), "{:?}", rig.faults());
    assert_eq!(scratch_left(&rig), Vec::<PathBuf>::new());
}
