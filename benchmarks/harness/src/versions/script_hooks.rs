//! The hook series of the versions whose hooks are scripts (1.0.0 to 1.5.1).
//!
//! Up to 1.5.1 Lumen's hooks were bash and python3 scripts committed at `.claude/hooks/*.sh`,
//! which call `lumen-tok` to count tokens. They are timed here as they were shipped:
//!
//! - **The tag's own bytes.** Each script is read from git (`git show <tag>:<path>`), checked
//!   against the sha256 pinned in [`PINS`] and written out unchanged. Never rebuilt, never edited.
//!   A script whose bytes differ from the pin stops the run.
//! - **Interpreted, executed directly**, the way Claude Code runs a hook command: the kernel reads
//!   the shebang. `LUMEN_TOK` points at that version's `lumen-tok` from its release asset and
//!   `LUMEN_MCP_BIN` (read by the 1.5.1 intercept as its liveness probe, never executed) at that
//!   version's `lumen-mcp`.
//! - **In the sandbox.** Everything runs under a [`Sandbox`]: scratch `HOME`, `TMPDIR`, ledger,
//!   fault spool and working directory, and an allowlisted environment. The scripts sit in a
//!   scratch workspace (`<scratch>/ws/.claude/hooks/`), so the paths they derive from their own
//!   location (`WORKSPACE_ROOT`) are scratch paths too. `LUMEN_DEBUG`, the one switch that makes a
//!   script write to the real `/tmp`, is never set: the sandbox strips every inherited `LUMEN_*`.
//! - **The same series as the binary hooks**, with the same ids, payloads and pass rotation (the
//!   caller is `latency::hooks`, once per pass): `hook.floor`, `hook.intercept_pass.spawn`,
//!   `hook.intercept_block.spawn`, `hook.meter_read.spawn` and `hook.meter_bash.spawn`. The last is
//!   `n/a`: `lumen_meter.sh` records only built-in Read events and exits 0 on anything else.
//!   The floor is a harness-written script that reads stdin and exits (bash and nothing else).
//! - **Checked, not assumed.** Every run's exit code is asserted, and so is what each hook should
//!   leave behind: a meter run adds one `Read` row (`builtin_read`, a nonzero `full_tokens`, so
//!   `lumen-tok` really counted), a pass run adds nothing, a block run leaves the 1.5.1 intercept's
//!   per-session marker. The script's own `|| true` swallows every failure, so without the row
//!   check a series could time a script that does nothing.
//! - **Labelled.** Every row says [`LABEL`]. A machine that cannot run them gets `n/a - <reason>`,
//!   never a zero and never a silent skip: Windows ([`NOT_WINDOWS`]), or no `bash`, `python3` (or
//!   `sqlite3`, which the scripts before 1.5.1 call) on `PATH`.
//!
//! What the scripts read outside their own bytes (grep of every pinned tag): `LUMEN_DB`,
//! `LUMEN_TOK`, `LUMEN_MCP_BIN`, `LUMEN_FAULT_SPOOL`, `LUMEN_HOOK_ENABLED`, `LUMEN_LINE_THRESHOLD`,
//! `LUMEN_CAPTURE`, `HOME` (`$HOME/.lumen_db_path`, and the default database path only when
//! `LUMEN_DB` is unset), `TMPDIR`, `PATH`, and the file named in the payload. The only absolute
//! path in them is `/tmp/lumen_hook_dump.json`, behind `LUMEN_DEBUG=1`.

use crate::builds::Build;
use crate::corpus::BUCKETS;
use crate::driver::{Identity, Server};
use crate::latency::{LatencyConfig, Series, Subject, post_read, pre_read, sample, time_spawn};
use crate::sandbox::Sandbox;
use crate::util::{Ctx, Res, sha256_hex};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

/// What every row of these series says about itself.
pub const LABEL: &str = "script hook (bash + python3), interpreted from the tag";

/// The reason on Windows: `n/a - <this>`.
pub const NOT_WINDOWS: &str = "script hooks need bash and python3";

/// The five series, in the order `latency::hooks` writes them.
pub const IDS: [&str; 5] = [
    "hook.floor",
    "hook.intercept_pass.spawn",
    "hook.intercept_block.spawn",
    "hook.meter_read.spawn",
    "hook.meter_bash.spawn",
];

const INTERCEPT: &str = "lumen_read_intercept.sh";
const METER: &str = "lumen_meter.sh";

/// The sha256 of each tag's committed hook scripts: `(tag, intercept, meter)`, taken from
/// `git show <tag>:.claude/hooks/<name>.sh`. 1.0.0 to 1.4.0 shipped the same two files.
pub const PINS: &[(&str, &str, &str)] = &[
    (
        "v1.0.0",
        "d19fcc39c3ff8c3275e2b806a987540a04724e281a2e7aab697b5b8c8f0df3ad",
        "90dabc43e34960eb63f168354c1dc4e287f643dce234a4396a59c19e77701f49",
    ),
    (
        "v1.1.5",
        "d19fcc39c3ff8c3275e2b806a987540a04724e281a2e7aab697b5b8c8f0df3ad",
        "90dabc43e34960eb63f168354c1dc4e287f643dce234a4396a59c19e77701f49",
    ),
    (
        "v1.2.4",
        "d19fcc39c3ff8c3275e2b806a987540a04724e281a2e7aab697b5b8c8f0df3ad",
        "90dabc43e34960eb63f168354c1dc4e287f643dce234a4396a59c19e77701f49",
    ),
    (
        "v1.3.1",
        "d19fcc39c3ff8c3275e2b806a987540a04724e281a2e7aab697b5b8c8f0df3ad",
        "90dabc43e34960eb63f168354c1dc4e287f643dce234a4396a59c19e77701f49",
    ),
    (
        "v1.4.0",
        "d19fcc39c3ff8c3275e2b806a987540a04724e281a2e7aab697b5b8c8f0df3ad",
        "90dabc43e34960eb63f168354c1dc4e287f643dce234a4396a59c19e77701f49",
    ),
    (
        "v1.5.1",
        "791fa95b091ffadb101e6bf82cd4eecb4a8fae65177903b7cf6ed60500cd2b06",
        "cb67198b161754cb69b343d54048c60f29e017ac367a628c4fcec6b5504e6b99",
    ),
];

/// The two scripts of one tag, as bytes: the file is never decoded, so a CRLF or a stray byte
/// survives.
#[derive(Clone, Debug)]
pub struct Scripts {
    pub intercept: Vec<u8>,
    pub meter: Vec<u8>,
}

fn pin_of(tag: &str) -> Res<&'static (&'static str, &'static str, &'static str)> {
    PINS.iter()
        .find(|p| p.0 == tag)
        .ok_or_else(|| format!("{tag} has no pinned hook scripts (script_hooks::PINS)"))
}

/// Refuse bytes whose sha256 is not `want`.
pub fn check_pin(what: &str, bytes: &[u8], want: &str) -> Res<()> {
    let got = sha256_hex(bytes);
    if got == want {
        Ok(())
    } else {
        Err(format!(
            "{what}: the committed script has sha256 {got}, the pin says {want}; refusing to time \
             a script that is not the one pinned"
        ))
    }
}

/// `git show <tag>:<path>`, the blob's bytes exactly (no text conversion).
pub fn git_show(repo: &Path, tag: &str, path: &str) -> Res<Vec<u8>> {
    let spec = format!("{tag}:{path}");
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["show", &spec])
        .output()
        .ctx("run git show")?;
    if !out.status.success() {
        return Err(format!(
            "git show {spec} failed: {} (is the tag in this checkout? `git fetch --tags`)",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(out.stdout)
}

/// Whether `tag` resolves to a commit in this checkout (a shallow clone has no tags).
pub fn tag_in_checkout(repo: &Path, tag: &str) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .args([
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{tag}^{{commit}}"),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// The tag's two scripts, each checked against [`PINS`]. Bytes that differ from the pin are an
/// error, not an `n/a`: that is a different script from the one the series is about.
pub fn from_tag(repo: &Path, tag: &str) -> Res<Scripts> {
    let pin = pin_of(tag)?;
    let intercept = git_show(repo, tag, &format!(".claude/hooks/{INTERCEPT}"))?;
    check_pin(&format!("{tag} {INTERCEPT}"), &intercept, pin.1)?;
    let meter = git_show(repo, tag, &format!(".claude/hooks/{METER}"))?;
    check_pin(&format!("{tag} {METER}"), &meter, pin.2)?;
    Ok(Scripts { intercept, meter })
}

/// Where the scripts were written.
#[derive(Clone, Debug)]
pub struct Laid {
    pub intercept: PathBuf,
    pub meter: PathBuf,
    /// The floor: harness-written, not a tag script.
    pub floor: PathBuf,
}

const FLOOR: &str =
    "#!/usr/bin/env bash\n# bench floor: read stdin, do nothing, exit 0\ncat >/dev/null\nexit 0\n";

fn write_exec(path: &Path, bytes: &[u8]) -> Res<()> {
    let parent = path.parent().ok_or("a script with no directory")?;
    std::fs::create_dir_all(parent).ctx(&format!("create {}", parent.display()))?;
    std::fs::write(path, bytes).ctx(&format!("write {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
            .ctx(&format!("make {} executable", path.display()))?;
    }
    Ok(())
}

/// Write the scripts, byte for byte, under `ws/.claude/hooks/` (the layout the scripts derive
/// `WORKSPACE_ROOT` from), and the floor script beside them.
pub fn lay_out(ws: &Path, s: &Scripts) -> Res<Laid> {
    let hooks = ws.join(".claude").join("hooks");
    let laid = Laid {
        intercept: hooks.join(INTERCEPT),
        meter: hooks.join(METER),
        floor: ws.join("floor.sh"),
    };
    write_exec(&laid.intercept, &s.intercept)?;
    write_exec(&laid.meter, &s.meter)?;
    write_exec(&laid.floor, FLOOR.as_bytes())?;
    Ok(laid)
}

// -- what the machine has -----------------------------------------------------------------------

/// What the machine is and has. Injected, so a test can be Windows or have no python3.
pub struct Tools<'a> {
    pub windows: bool,
    /// Whether a program of this name is on `PATH`.
    pub have: &'a dyn Fn(&str) -> bool,
}

/// Whether `name` is an executable file in a `PATH` directory.
pub fn on_path(name: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|d| {
        let p = d.join(name);
        if !p.is_file() {
            return false;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            p.metadata()
                .is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
        }
        #[cfg(not(unix))]
        {
            true
        }
    })
}

/// The programs these scripts call that are not part of every POSIX box: bash and python3 always,
/// and `sqlite3` where a script writes the ledger with the command-line shell (the meter of
/// 1.0.0 to 1.4.0; from 1.5.1 it writes with python3's sqlite3 module).
pub fn needs(s: &Scripts) -> Vec<&'static str> {
    let mut v = vec!["bash", "python3"];
    let calls_sqlite3 = [&s.intercept, &s.meter].iter().any(|b| {
        String::from_utf8_lossy(b)
            .lines()
            .any(|l| l.trim_start().starts_with("sqlite3 "))
    });
    if calls_sqlite3 {
        v.push("sqlite3");
    }
    v
}

/// Why these scripts cannot run here, or `None`.
pub fn unavailable(t: &Tools, s: &Scripts) -> Option<String> {
    if t.windows {
        return Some(NOT_WINDOWS.to_string());
    }
    let need = needs(s);
    let missing: Vec<&str> = need.iter().copied().filter(|n| !(t.have)(n)).collect();
    if missing.is_empty() {
        None
    } else {
        Some(format!(
            "script hooks need {}; {} not found on PATH",
            need.join(", "),
            missing.join(", ")
        ))
    }
}

// -- the series -------------------------------------------------------------------------------

/// All five rows, `n/a - <reason>`.
fn all_na(why: &str, bash_na: &str) -> Vec<Series> {
    IDS.iter()
        .map(|id| {
            let reason = if *id == "hook.meter_bash.spawn" {
                bash_na
            } else {
                why
            };
            Series::na_because(id, &describe(id), reason)
        })
        .collect()
}

fn describe(id: &str) -> String {
    let what = match id {
        "hook.floor" => {
            "process floor: a script that reads stdin and exits 0 (bash startup; not comparable with a binary hook's floor)"
        }
        "hook.intercept_pass.spawn" => {
            "PreToolUse Read of a file under the 300-line threshold: not intercepted (exit 0)"
        }
        "hook.intercept_block.spawn" => {
            "PreToolUse Read of a file over the threshold, first Read of the session: blocked (exit 2)"
        }
        "hook.meter_read.spawn" => {
            "PostToolUse Read: count the file's tokens with lumen-tok, write one ledger row (exit 0)"
        }
        _ => "PostToolUse Bash",
    };
    format!("{LABEL}. {what}")
}

/// Why `hook.meter_bash.spawn` cannot exist for a script version.
const BASH_NA_FALLBACK: &str = "lumen_meter.sh records only built-in Read events and exits 0 on any other tool; the Bash meter arrived in 1.6.0";

/// What a measurement needs from its caller.
pub struct Job<'a> {
    pub tag: &'a str,
    pub laid: &'a Laid,
    /// This version's `lumen-tok` (read as `LUMEN_TOK`).
    pub tok: &'a Path,
    /// This version's `lumen-mcp` (read as `LUMEN_MCP_BIN`, a liveness probe only).
    pub mcp: &'a Path,
    pub subjects: &'a [Subject],
    pub cfg: &'a LatencyConfig,
    /// The scratch root of this measurement.
    pub work: &'a Path,
    /// Why this version has no Bash meter, from the capability map.
    pub bash_na: Option<String>,
    /// Whether the intercept script leaves a per-session marker under `TMPDIR` when it blocks
    /// (1.5.1): then a block series must leave exactly one per run.
    pub marks_blocks: bool,
}

fn script_cmd(script: &Path, iso: &Sandbox) -> Command {
    let mut c = Command::new(script);
    iso.apply(&mut c);
    c.stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    c
}

/// `time_spawn`, retried while the kernel says the script file is busy: a file just written is
/// briefly "text file busy" if another thread's fork is between its fork and its exec.
fn time_script(cmd: &mut Command, payload: &[u8]) -> Res<(i32, Duration)> {
    let mut tries = 0;
    loop {
        match time_spawn(cmd, payload) {
            Err(e) if e.contains("Text file busy") && tries < 100 => {
                tries += 1;
                std::thread::sleep(Duration::from_millis(20));
            }
            r => return r,
        }
    }
}

/// One timed series: the script's exit code is asserted on every run, warm-up included.
#[allow(clippy::too_many_arguments)]
fn run_series(
    iso: &Sandbox,
    cfg: &LatencyConfig,
    script: &Path,
    id: &str,
    subject: Option<String>,
    expect_exit: i32,
    payload: &dyn Fn(usize) -> Vec<u8>,
) -> Res<Series> {
    let mut cmd = script_cmd(script, iso);
    let ns = sample(cfg, |i| {
        let (code, d) = time_script(&mut cmd, &payload(i))?;
        if code != expect_exit {
            return Err(format!(
                "{id}: the script hook exited {code}, expected {expect_exit} (run {i}); this \
                 series would not be measuring the path it names"
            ));
        }
        Ok(d)
    })?;
    Series::measured(id, &describe(id), subject, &ns)
}

/// `Read` rows by `builtin_read` that carry a token count, and all `Read` rows.
fn read_rows(db: &Path) -> Res<(i64, i64)> {
    let conn = lumen_core::meter::open_read_only(db).ctx("open the ledger")?;
    conn.query_row(
        "SELECT COUNT(*), COALESCE(SUM(routed_via = 'builtin_read' AND full_tokens > 0), 0) \
         FROM read_events WHERE tool = 'Read'",
        [],
        |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
    )
    .ctx("count the meter rows")
}

fn markers(tmp: &Path) -> usize {
    std::fs::read_dir(tmp)
        .map(|d| {
            d.flatten()
                .filter(|e| {
                    e.file_name()
                        .to_string_lossy()
                        .starts_with("lumen_intercept_")
                })
                .count()
        })
        .unwrap_or(0)
}

/// Time the five series. `prep` creates the ledger in the sandbox with this build's own schema
/// (the scripts never create one: they only insert).
pub fn time_scripts(job: &Job, prep: &dyn Fn(&Sandbox) -> Res<()>) -> Res<Vec<Series>> {
    let iso = Sandbox::create(&job.work.join("script-hooks"))?
        .set("LUMEN_TOK", job.tok.as_os_str())
        .set("LUMEN_MCP_BIN", job.mcp.as_os_str());
    let total = job.cfg.warmup + job.cfg.runs;
    let tag: String = job
        .tag
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect();
    let small = job.subjects.iter().find(|s| s.bucket == BUCKETS[0].name);
    let medium = job.subjects.iter().find(|s| s.bucket == BUCKETS[1].name);
    let mut out = Vec::new();

    let floor_payload = medium.map_or_else(
        || pre_read("bench-floor", Path::new("/")),
        |m| post_read("bench-floor", m),
    );
    out.push(run_series(
        &iso,
        job.cfg,
        &job.laid.floor,
        IDS[0],
        None,
        0,
        &|_| floor_payload.clone(),
    )?);

    // The ledger, made by the build under test, before any script inserts into it.
    if medium.or(small).is_some() {
        prep(&iso)?;
        if !iso.db.is_file() {
            return Err(format!(
                "preparing the ledger left no file at {}",
                iso.db.display()
            ));
        }
    }
    let (rows_before, good_before) = if iso.db.is_file() {
        read_rows(&iso.db)?
    } else {
        (0, 0)
    };

    match small {
        Some(s) => {
            out.push(run_series(
                &iso,
                job.cfg,
                &job.laid.intercept,
                IDS[1],
                Some(s.label()),
                0,
                &|i| pre_read(&format!("bench-{tag}-pass-{i}"), &s.abs),
            )?);
            if markers(&iso.tmp) != 0 {
                return Err(format!(
                    "{}: a Read under the threshold left an intercept marker: the series did not \
                     take the pass path",
                    IDS[1]
                ));
            }
        }
        None => out.push(Series::na(
            IDS[1],
            "PreToolUse Read under the threshold",
            "no file under 300 lines in the corpus",
        )),
    }
    match medium {
        Some(m) => {
            out.push(run_series(
                &iso,
                job.cfg,
                &job.laid.intercept,
                IDS[2],
                Some(m.label()),
                2,
                &|i| pre_read(&format!("bench-{tag}-block-{i}"), &m.abs),
            )?);
            let want = if job.marks_blocks { total } else { 0 };
            let got = markers(&iso.tmp);
            if got != want {
                return Err(format!(
                    "{}: {got} intercept markers under the sandbox TMPDIR, expected {want}: the \
                     series did not take the block path on every run",
                    IDS[2]
                ));
            }
            out.push(run_series(
                &iso,
                job.cfg,
                &job.laid.meter,
                IDS[3],
                Some(m.label()),
                0,
                &|i| post_read(&format!("bench-{tag}-mr-{i}"), m),
            )?);
            let (rows, good) = read_rows(&iso.db)?;
            let want = total as i64;
            if rows - rows_before != want || good - good_before != want {
                return Err(format!(
                    "{}: the meter script added {} Read rows ({} with builtin_read and a token \
                     count), expected {want}: a series did not measure a metered call (its own \
                     `|| true` hides a failed insert)",
                    IDS[3],
                    rows - rows_before,
                    good - good_before
                ));
            }
        }
        None => {
            for (id, what) in [
                (IDS[2], "PreToolUse Read over the threshold"),
                (IDS[3], "PostToolUse Read"),
            ] {
                out.push(Series::na(
                    id,
                    what,
                    "no file of 300-999 lines in the corpus",
                ));
            }
        }
    }
    out.push(Series::na_because(
        IDS[4],
        &describe(IDS[4]),
        job.bash_na.as_deref().unwrap_or(BASH_NA_FALLBACK),
    ));
    Ok(out)
}

/// The five rows for `scripts`, measured, or `n/a - <reason>` where the machine cannot run them.
/// `scripts` is asked for only when the machine is not Windows.
pub fn measure(
    t: &Tools,
    scripts: &dyn Fn() -> Res<Result<Scripts, String>>,
    job: &Job,
    prep: &dyn Fn(&Sandbox) -> Res<()>,
) -> Res<Vec<Series>> {
    let bash_na = job.bash_na.as_deref().unwrap_or(BASH_NA_FALLBACK);
    if t.windows {
        return Ok(all_na(NOT_WINDOWS, bash_na));
    }
    let s = match scripts()? {
        Ok(s) => s,
        Err(why) => return Ok(all_na(&why, bash_na)),
    };
    if let Some(why) = unavailable(t, &s) {
        return Ok(all_na(&why, bash_na));
    }
    let laid = lay_out(&job.work.join("ws"), &s)?;
    let job = Job {
        laid: &laid,
        marks_blocks: String::from_utf8_lossy(&s.intercept).contains("lumen_intercept_"),
        ..job.clone_with()
    };
    time_scripts(&job, prep)
}

impl Job<'_> {
    fn clone_with(&self) -> Job<'_> {
        Job {
            tag: self.tag,
            laid: self.laid,
            tok: self.tok,
            mcp: self.mcp,
            subjects: self.subjects,
            cfg: self.cfg,
            work: self.work,
            bash_na: self.bash_na.clone(),
            marks_blocks: self.marks_blocks,
        }
    }
}

/// The ledger of a build, made the way a session makes it: the build's own `lumen-mcp` answers
/// one `smart_read`, and writes the schema it ships with.
fn make_ledger(build: &Build, s: &Subject, work: &Path, iso: &Sandbox) -> Res<()> {
    let cwd = work.join("ledger-cwd");
    let dest = cwd.join(&s.rel);
    let parent = dest.parent().ok_or("a subject with no directory")?;
    std::fs::create_dir_all(parent).ctx(&format!("create {}", parent.display()))?;
    std::fs::write(&dest, s.src.as_bytes()).ctx(&format!("write {}", dest.display()))?;
    let mut server = Server::start(&build.bin, &cwd, iso, "bench-ledger", false, Identity::Env)?;
    server.set_clamped(build.clamped());
    server.smart_read(&s.rel)?;
    server.finish()
}

/// Replace the five spawn rows `out` already holds for a script-hook build (written `n/a` by
/// `latency::hooks`) with these series. A no-op where there is no subject file at all.
pub fn overlay(
    build: &Build,
    subjects: &[Subject],
    cfg: &LatencyConfig,
    work: &Path,
    out: &mut [Series],
) -> Res<()> {
    let Some(caps) = &build.caps else {
        return Ok(());
    };
    let Some(seed) = subjects
        .iter()
        .find(|s| s.bucket == BUCKETS[1].name)
        .or_else(|| subjects.first())
    else {
        return Ok(());
    };
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let tag = format!("v{}", caps.version);
    let tok = build.bin.with_file_name(if cfg!(windows) {
        "lumen-tok.exe"
    } else {
        "lumen-tok"
    });
    let placeholder = Laid {
        intercept: PathBuf::new(),
        meter: PathBuf::new(),
        floor: PathBuf::new(),
    };
    let job = Job {
        tag: &tag,
        laid: &placeholder,
        tok: &tok,
        mcp: &build.bin,
        subjects,
        cfg,
        work,
        bash_na: caps.na_bash_meter(),
        marks_blocks: false,
    };
    let have = |n: &str| on_path(n);
    let t = Tools {
        windows: cfg!(windows),
        have: &have,
    };
    if !cfg!(windows) && !tok.is_file() {
        return Err(format!(
            "{}: the asset holds no lumen-tok next to lumen-mcp",
            build.label
        ));
    }
    let source = || {
        if !tag_in_checkout(&repo, &tag) {
            return Ok(Err(format!(
                "the tag {tag} is not in this checkout (git fetch --tags), so its committed \
                 hook scripts cannot be read"
            )));
        }
        from_tag(&repo, &tag).map(Ok)
    };
    let rows = measure(&t, &source, &job, &|iso| {
        make_ledger(build, seed, work, iso)
    })?;
    for row in rows {
        match out.iter_mut().find(|s| s.id == row.id) {
            Some(slot) => *slot = row,
            None => return Err(format!("no slot for {} in the hook series", row.id)),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn subject(dir: &Path, bucket: &str, rel: &str, lines: usize) -> Subject {
        let src: String = (0..lines).map(|i| format!("fn f{i}() {{}}\n")).collect();
        let abs = dir.join(rel);
        std::fs::write(&abs, &src).unwrap();
        Subject {
            bucket: bucket.to_string(),
            rel: rel.to_string(),
            lines,
            bytes: src.len(),
            recall_item: "f0".to_string(),
            abs,
            src,
            alt: String::new(),
        }
    }

    fn subjects(dir: &Path) -> Vec<Subject> {
        vec![
            subject(dir, BUCKETS[0].name, "small.rs", 20),
            subject(dir, BUCKETS[1].name, "medium.rs", 400),
        ]
    }

    fn have_all(_: &str) -> bool {
        true
    }

    fn cfg() -> LatencyConfig {
        LatencyConfig { runs: 6, warmup: 2 }
    }

    /// A ledger with the table the stub meter inserts into, made the way a build would make it.
    #[cfg(unix)] // used only by the unix-only series tests
    fn stub_ledger(iso: &Sandbox) -> Res<()> {
        // `connect_db` creates the ledger with lumen-core's schema, as a build would.
        lumen_core::meter::connect_db(&iso.db)
            .map(|_| ())
            .ctx("open the ledger")
    }

    #[cfg(unix)] // used only by the unix-only series tests
    const STUB_INTERCEPT: &str = "#!/usr/bin/env bash\n\
        in=$(cat)\n\
        case \"$in\" in *-block-*) exit 2;; esac\n\
        exit 0\n";

    #[cfg(unix)] // used only by the unix-only series tests
    const STUB_METER: &str = "#!/usr/bin/env bash\n\
        cat >/dev/null\n\
        python3 - \"$LUMEN_DB\" \"$LUMEN_TOK\" <<'PY'\n\
        import sqlite3, sys\n\
        c = sqlite3.connect(sys.argv[1])\n\
        c.execute(\"INSERT INTO read_events(ts,tool,path,tokens_returned,full_tokens,saved_tokens,routed_via) VALUES('t','Read','p',42,42,0,'builtin_read')\")\n\
        c.commit()\n\
        PY\n\
        exit 0\n";

    fn scripts_of(intercept: &str, meter: &str) -> Scripts {
        Scripts {
            intercept: intercept.as_bytes().to_vec(),
            meter: meter.as_bytes().to_vec(),
        }
    }

    #[cfg(unix)] // used only by the unix-only series tests
    fn run_stubs(
        work: &Path,
        s: Scripts,
        t: &Tools,
        prep: &dyn Fn(&Sandbox) -> Res<()>,
    ) -> Res<Vec<Series>> {
        let subs = subjects(work);
        let cfg = cfg();
        let tok = work.join("lumen-tok");
        let placeholder = Laid {
            intercept: PathBuf::new(),
            meter: PathBuf::new(),
            floor: PathBuf::new(),
        };
        let job = Job {
            tag: "v9.9.9",
            laid: &placeholder,
            tok: &tok,
            mcp: &tok,
            subjects: &subs,
            cfg: &cfg,
            work,
            bash_na: Some("no Bash meter".to_string()),
            marks_blocks: false,
        };
        measure(t, &|| Ok(Ok(s.clone())), &job, prep)
    }

    #[cfg(unix)]
    #[test]
    fn stub_scripts_are_timed_with_their_exit_codes_and_rows_asserted() {
        assert!(on_path("python3") && on_path("bash"), "the stubs need both");
        let dir = tempfile::tempdir().unwrap();
        let have = have_all;
        let t = Tools {
            windows: false,
            have: &have,
        };
        let rows = run_stubs(
            dir.path(),
            scripts_of(STUB_INTERCEPT, STUB_METER),
            &t,
            &stub_ledger,
        )
        .unwrap();
        let ids: Vec<&str> = rows.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, IDS);
        let by: BTreeMap<&str, &Series> = rows.iter().map(|s| (s.id.as_str(), s)).collect();
        for id in &IDS[..4] {
            let s = by[id];
            assert_eq!(s.status, "measured", "{id}");
            assert_eq!(s.raw.len(), 6, "{id}: the recorded runs");
            assert!(s.lat.is_some(), "{id}");
            assert!(s.what.starts_with(LABEL), "{id}: {}", s.what);
        }
        let bash = by["hook.meter_bash.spawn"];
        assert!(bash.lat.is_none() && bash.status == "n/a — no Bash meter");
        assert!(bash.what.starts_with(LABEL));
    }

    #[cfg(unix)]
    #[test]
    fn a_script_that_exits_wrong_fails_the_series_loudly() {
        let dir = tempfile::tempdir().unwrap();
        let have = have_all;
        let t = Tools {
            windows: false,
            have: &have,
        };
        // An intercept that never blocks: the block series would time the pass path.
        let never_blocks = "#!/usr/bin/env bash\ncat >/dev/null\nexit 0\n";
        let e = run_stubs(
            dir.path(),
            scripts_of(never_blocks, STUB_METER),
            &t,
            &stub_ledger,
        )
        .unwrap_err();
        assert!(
            e.contains("hook.intercept_block.spawn") && e.contains("exited 0, expected 2"),
            "{e}"
        );
        // A meter that fails: its exit code is asserted too.
        let dir = tempfile::tempdir().unwrap();
        let fails = "#!/usr/bin/env bash\ncat >/dev/null\nexit 3\n";
        let e = run_stubs(
            dir.path(),
            scripts_of(STUB_INTERCEPT, fails),
            &t,
            &stub_ledger,
        )
        .unwrap_err();
        assert!(
            e.contains("hook.meter_read.spawn") && e.contains("expected 0"),
            "{e}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_meter_that_exits_zero_and_writes_nothing_fails_on_the_row_check() {
        // The tag's scripts end in `|| true`: exit 0 is what a failed insert looks like too.
        let dir = tempfile::tempdir().unwrap();
        let have = have_all;
        let t = Tools {
            windows: false,
            have: &have,
        };
        let silent = "#!/usr/bin/env bash\ncat >/dev/null\nexit 0\n";
        let e = run_stubs(
            dir.path(),
            scripts_of(STUB_INTERCEPT, silent),
            &t,
            &stub_ledger,
        )
        .unwrap_err();
        assert!(
            e.contains("hook.meter_read.spawn") && e.contains("added 0 Read rows"),
            "{e}"
        );
        // Rows without a token count (lumen-tok did not count) fail the same check.
        let dir = tempfile::tempdir().unwrap();
        let zero = STUB_METER.replace("'p',42,42,0,", "'p',42,0,0,");
        let e = run_stubs(
            dir.path(),
            scripts_of(STUB_INTERCEPT, &zero),
            &t,
            &stub_ledger,
        )
        .unwrap_err();
        assert!(e.contains("token count"), "{e}");
    }

    #[test]
    fn windows_gives_the_na_row_and_touches_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let have = |_: &str| -> bool { panic!("Windows must not look for a tool") };
        let t = Tools {
            windows: true,
            have: &have,
        };
        let subs = subjects(dir.path());
        let cfg = cfg();
        let p = Path::new("unused");
        let placeholder = Laid {
            intercept: p.into(),
            meter: p.into(),
            floor: p.into(),
        };
        let job = Job {
            tag: "v1.0.0",
            laid: &placeholder,
            tok: p,
            mcp: p,
            subjects: &subs,
            cfg: &cfg,
            work: &dir.path().join("never-created"),
            bash_na: None,
            marks_blocks: false,
        };
        let rows = measure(
            &t,
            &|| panic!("Windows must not read the scripts"),
            &job,
            &|_| panic!("Windows must not make a ledger"),
        )
        .unwrap();
        let ids: Vec<&str> = rows.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, IDS);
        for s in &rows {
            assert!(s.lat.is_none() && s.raw.is_empty(), "{}", s.id);
            assert!(s.what.starts_with(LABEL), "{}", s.id);
            if s.id != "hook.meter_bash.spawn" {
                assert_eq!(
                    s.status, "n/a — script hooks need bash and python3",
                    "{}",
                    s.id
                );
            }
        }
        assert!(!dir.path().join("never-created").exists());
    }

    #[test]
    fn a_missing_python3_or_bash_is_n_a_with_the_reason_not_a_zero() {
        let dir = tempfile::tempdir().unwrap();
        let s = scripts_of("#!/bin/bash\n", "#!/bin/bash\n");
        for gone in ["python3", "bash"] {
            let have = move |n: &str| n != gone;
            let t = Tools {
                windows: false,
                have: &have,
            };
            let why = unavailable(&t, &s).unwrap();
            assert!(
                why.contains(gone) && why.contains("not found on PATH"),
                "{why}"
            );
            let subs = subjects(dir.path());
            let cfg = cfg();
            let p = Path::new("unused");
            let placeholder = Laid {
                intercept: p.into(),
                meter: p.into(),
                floor: p.into(),
            };
            let job = Job {
                tag: "v1.0.0",
                laid: &placeholder,
                tok: p,
                mcp: p,
                subjects: &subs,
                cfg: &cfg,
                work: &dir.path().join("never-created"),
                bash_na: None,
                marks_blocks: false,
            };
            let rows = measure(&t, &|| Ok(Ok(s.clone())), &job, &|_| {
                panic!("no tool, no ledger")
            })
            .unwrap();
            assert_eq!(rows.len(), 5);
            for r in rows.iter().filter(|r| r.id != "hook.meter_bash.spawn") {
                assert!(r.lat.is_none(), "{}", r.id);
                assert!(
                    r.status.starts_with("n/a — ") && r.status.contains(gone),
                    "{}",
                    r.status
                );
            }
        }
        // sqlite3 counts only for the scripts that call it.
        let old = scripts_of("#!/bin/bash\n", "#!/bin/bash\nsqlite3 \"$DB\" \"x\"\n");
        let no_sqlite = |n: &str| n != "sqlite3";
        let t = Tools {
            windows: false,
            have: &no_sqlite,
        };
        assert!(unavailable(&t, &old).unwrap().contains("sqlite3"));
        assert!(unavailable(&t, &s).is_none());
    }

    #[test]
    fn a_tag_missing_from_the_checkout_is_n_a_with_its_reason() {
        let dir = tempfile::tempdir().unwrap();
        let have = have_all;
        let t = Tools {
            windows: false,
            have: &have,
        };
        let subs = subjects(dir.path());
        let cfg = cfg();
        let p = Path::new("unused");
        let placeholder = Laid {
            intercept: p.into(),
            meter: p.into(),
            floor: p.into(),
        };
        let job = Job {
            tag: "v1.0.0",
            laid: &placeholder,
            tok: p,
            mcp: p,
            subjects: &subs,
            cfg: &cfg,
            work: &dir.path().join("never-created"),
            bash_na: None,
            marks_blocks: false,
        };
        let rows = measure(
            &t,
            &|| Ok(Err("the tag v1.0.0 is not in this checkout".to_string())),
            &job,
            &|_| panic!("no scripts, no ledger"),
        )
        .unwrap();
        assert_eq!(rows.len(), 5);
        assert!(
            rows.iter()
                .all(|r| r.lat.is_none() && r.status.starts_with("n/a — "))
        );
        assert!(rows[0].status.contains("not in this checkout"));
    }

    #[test]
    fn the_scripts_are_written_byte_for_byte_and_a_wrong_pin_refuses() {
        let dir = tempfile::tempdir().unwrap();
        // CRLF, a stray byte and no trailing newline survive: nothing decodes or normalises.
        let s = Scripts {
            intercept: b"#!/usr/bin/env bash\r\nexit 2\r\n\xff".to_vec(),
            meter: b"#!/usr/bin/env bash\nexit 0".to_vec(),
        };
        let laid = lay_out(dir.path(), &s).unwrap();
        assert_eq!(std::fs::read(&laid.intercept).unwrap(), s.intercept);
        assert_eq!(std::fs::read(&laid.meter).unwrap(), s.meter);
        assert!(
            laid.intercept
                .ends_with(".claude/hooks/lumen_read_intercept.sh")
        );
        let want = sha256_hex(&s.meter);
        check_pin("meter", &s.meter, &want).unwrap();
        let e = check_pin("meter", &s.meter, &sha256_hex(b"other")).unwrap_err();
        assert!(e.contains("refusing"), "{e}");
    }

    fn repo() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    fn tag_present(tag: &str) -> bool {
        tag_in_checkout(&repo(), tag)
    }

    #[test]
    fn every_script_version_has_a_pin_and_the_pins_name_distinct_real_hashes() {
        let m = crate::manifest::VersionsManifest::load_committed().unwrap();
        for v in &m.meta.versions {
            let e = m.get(v).unwrap();
            let has = PINS.iter().any(|p| p.0 == format!("v{v}"));
            assert_eq!(
                has, !e.hook_cli,
                "{v}: PINS must cover exactly the script versions"
            );
        }
        for (tag, a, b) in PINS {
            for h in [a, b] {
                assert!(
                    h.len() == 64 && h.bytes().all(|c| c.is_ascii_hexdigit()),
                    "{tag}"
                );
            }
            assert_ne!(a, b, "{tag}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn every_pinned_tag_script_is_taken_from_git_byte_for_byte() {
        // CI checks out without tags: the pin is then the guard (a run refuses on any other
        // bytes). Where the checkout has the tag, `git show` must hash to the pin, and the
        // written file must hash to what git printed.
        let mut checked = 0;
        for (tag, intercept, meter) in PINS {
            if !tag_present(tag) {
                eprintln!("script_hooks: {tag} is not in this checkout; the pin guards the run");
                continue;
            }
            let dir = tempfile::tempdir().unwrap();
            let s = from_tag(&repo(), tag).unwrap();
            assert_eq!(&sha256_hex(&s.intercept), intercept, "{tag} intercept");
            assert_eq!(&sha256_hex(&s.meter), meter, "{tag} meter");
            let direct = git_show(&repo(), tag, ".claude/hooks/lumen_meter.sh").unwrap();
            assert_eq!(direct, s.meter, "{tag}");
            let laid = lay_out(dir.path(), &s).unwrap();
            assert_eq!(
                sha256_hex(&std::fs::read(&laid.intercept).unwrap()),
                *intercept,
                "{tag}"
            );
            assert_eq!(
                sha256_hex(&std::fs::read(&laid.meter).unwrap()),
                *meter,
                "{tag}"
            );
            checked += 1;
        }
        eprintln!(
            "script_hooks: {checked} of {} tags checked against git",
            PINS.len()
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_pinned_script_that_differs_from_the_tag_is_refused() {
        if !tag_present("v1.5.1") {
            return;
        }
        // Mutant: the 1.0.0 intercept pin against 1.5.1's bytes.
        let bytes = git_show(&repo(), "v1.5.1", ".claude/hooks/lumen_read_intercept.sh").unwrap();
        assert!(check_pin("x", &bytes, PINS[0].1).is_err());
        assert!(from_tag(&repo(), "v9.9.9").is_err());
    }

    #[test]
    fn the_scripts_read_nothing_outside_the_sandbox() {
        // The sandbox gives a script a scratch HOME, TMPDIR and ledger, and removes every
        // inherited LUMEN_* (LUMEN_DEBUG would write to the real /tmp).
        let dir = tempfile::tempdir().unwrap();
        let iso = Sandbox::create(dir.path())
            .unwrap()
            .set("LUMEN_TOK", "/x/lumen-tok");
        let hostile: Vec<(std::ffi::OsString, std::ffi::OsString)> = [
            ("LUMEN_DEBUG", "1"),
            ("LUMEN_MCP_BIN", "/real/lumen-mcp"),
            ("HOME", "/Users/real"),
            ("TMPDIR", "/real/tmp"),
        ]
        .into_iter()
        .map(|(k, v)| (k.into(), v.into()))
        .collect();
        let env: BTreeMap<String, std::ffi::OsString> = iso.env_for(&hostile).into_iter().collect();
        assert!(!env.contains_key("LUMEN_DEBUG"));
        assert!(
            !env.contains_key("LUMEN_MCP_BIN"),
            "an inherited value must not get through"
        );
        assert_eq!(env["LUMEN_TOK"], std::ffi::OsString::from("/x/lumen-tok"));
        for k in ["HOME", "TMPDIR", "LUMEN_DB", "LUMEN_FAULT_SPOOL"] {
            assert!(
                std::path::Path::new(&env[k]).starts_with(dir.path()),
                "{k} is {:?}, outside the sandbox",
                env[k]
            );
        }
    }

    #[test]
    fn the_only_absolute_paths_in_the_pinned_scripts_are_the_known_ones() {
        // Audited by hand in the module doc; this keeps it true if a pin ever changes.
        let mut seen = 0;
        for (tag, _, _) in PINS {
            if !tag_present(tag) {
                continue;
            }
            let s = from_tag(&repo(), tag).unwrap();
            for (name, bytes) in [(INTERCEPT, &s.intercept), (METER, &s.meter)] {
                for (n, line) in String::from_utf8_lossy(bytes).lines().enumerate() {
                    let code = line.trim_start();
                    if code.starts_with('#') {
                        continue;
                    }
                    for abs in [
                        "/tmp",
                        "/Users",
                        "/home",
                        "/etc",
                        "/var",
                        "/opt",
                        "/usr/local",
                    ] {
                        if code.contains(abs) {
                            let guarded = code.contains("/tmp/lumen_hook_dump.json")
                                || code.contains("${TMPDIR:-/tmp}");
                            assert!(guarded, "{tag} {name}:{}: {line}", n + 1);
                        }
                    }
                }
                seen += 1;
            }
        }
        eprintln!("script_hooks: {seen} scripts scanned");
    }
}
