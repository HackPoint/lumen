//! The steps of `.github/workflows/upgrade.yml` that run inside the test binary: an install a
//! previous release left on the runner's REAL profile, and the checks after this build is
//! installed over it with the platform's own installer and launched.
//!
//! The previous releases' binaries are gone, so the previous install is simulated under the
//! neutral folder name [`PREVIOUS`]: its ledger is built from that release's schema, its shims
//! from `tests/fixtures/v<version>`. The start finds it by what it holds, which is what makes the
//! neutral name a valid stand-in.
//!
//! These helpers write the real home, so they refuse to run anywhere but a GitHub-hosted runner
//! with `LUMEN_UPGRADE_JOB=1` ([`refusal`]): the runner is a throwaway VM. They are `#[ignore]`d,
//! and the workflow runs each by its exact name. State passes between steps through
//! `LUMEN_UPGRADE_STATE`.

use super::*;
use crate::upgrade::freeze_after_setup;
use lumen_core::legacy::commit::backup_dir;
use lumen_core::legacy::freeze::is_frozen;
use lumen_core::legacy::marker::{self, Seen};

const DDL_180: &str =
    include_str!("../../../../../crates/lumen-core/tests/fixtures/ledger_schema_v1.8.0.sql");
const DDL_190: &str =
    include_str!("../../../../../crates/lumen-core/tests/fixtures/ledger_schema_v1.9.0.sql");

/// Rows the previous install holds before its hooks add one.
const SEEDED: i64 = 20;
/// Rows the re-created folder holds.
const RECREATED: i64 = 5;
/// The previous install's menu-bar position, carried by the start (bug 5 of the 1.9.0 report).
#[cfg(target_os = "macos")]
const POSITION: &str = "612";
/// A vendor that is not this build's, for the previous manufacturer key.
#[cfg(windows)]
const OLD_VENDOR: &str = "ExampleVendor";

/// Why the helpers may not run here, or `None` on a GitHub-hosted upgrade job.
fn refusal(get: &dyn Fn(&str) -> Option<String>) -> Option<String> {
    let is = |k: &str, v: &str| get(k).as_deref() == Some(v);
    if !is("GITHUB_ACTIONS", "true") || !is("RUNNER_ENVIRONMENT", "github-hosted") {
        return Some("these helpers write the real home: GitHub-hosted runners only".into());
    }
    if !is("LUMEN_UPGRADE_JOB", "1") {
        return Some("LUMEN_UPGRADE_JOB=1 is set only by the upgrade workflow".into());
    }
    None
}

#[test]
fn the_helpers_refuse_to_run_off_a_github_hosted_upgrade_job() {
    let env = |pairs: &'static [(&'static str, &'static str)]| {
        move |k: &str| {
            pairs
                .iter()
                .find(|(key, _)| *key == k)
                .map(|(_, v)| v.to_string())
        }
    };
    assert!(refusal(&env(&[])).is_some(), "a developer's machine");
    assert!(
        refusal(&env(&[
            ("GITHUB_ACTIONS", "true"),
            ("LUMEN_UPGRADE_JOB", "1")
        ]))
        .is_some(),
        "a self-hosted runner"
    );
    assert!(
        refusal(&env(&[
            ("GITHUB_ACTIONS", "true"),
            ("RUNNER_ENVIRONMENT", "github-hosted")
        ]))
        .is_some(),
        "another job on a hosted runner"
    );
    assert!(refusal(&env(&[
        ("GITHUB_ACTIONS", "true"),
        ("RUNNER_ENVIRONMENT", "github-hosted"),
        ("LUMEN_UPGRADE_JOB", "1")
    ]))
    .is_none());
}

/// The real home, the new data folder, the previous one and the state folder, once the gate
/// has let the helper through.
struct Job {
    home: PathBuf,
    n: PathBuf,
    previous: PathBuf,
    state: PathBuf,
    from: String,
    login_on: bool,
}

fn job() -> Job {
    if let Some(why) = refusal(&|k| std::env::var(k).ok()) {
        panic!("refusing: {why}");
    }
    let home = dirs::home_dir().expect("a home");
    let n = app_support_dir_in(&home);
    let previous = n.parent().expect("a data root").join(PREVIOUS);
    let state = PathBuf::from(std::env::var("LUMEN_UPGRADE_STATE").expect("LUMEN_UPGRADE_STATE"));
    std::fs::create_dir_all(&state).unwrap();
    let from = std::env::var("LUMEN_UPGRADE_FROM").expect("LUMEN_UPGRADE_FROM");
    let login_on = std::env::var("LUMEN_UPGRADE_LOGIN").as_deref() == Ok("on");
    Job {
        home,
        n,
        previous,
        state,
        from,
        login_on,
    }
}

/// A rig whose hooks run against the real home: the project, temp dir and tool links are still
/// its own.
fn real_rig(home: &Path) -> Rig {
    let root = TempDir::new().expect("tempdir");
    let mk = |p: PathBuf| {
        std::fs::create_dir_all(&p).unwrap();
        p
    };
    let bin = mk(root.path().join("bin"));
    for tool in SHIM_TOOLS {
        link_tool(&bin, tool);
    }
    let data = app_support_dir_in(home);
    Rig {
        tmp: mk(root.path().join("tmp")),
        proj: mk(root.path().join("proj")),
        out: mk(root.path().join("out")),
        spool: data.join("faults.jsonl"),
        db: data.join("lumen.db"),
        bin,
        home: home.to_path_buf(),
        root,
    }
}

/// Where this build is installed: the app, its `lumen-mcp` and its `lumen-cli`.
fn installed(name: &str) -> PathBuf {
    if cfg!(target_os = "macos") {
        PathBuf::from("/Applications/Lumen.app/Contents/MacOS").join(name)
    } else if cfg!(windows) {
        PathBuf::from(std::env::var("LOCALAPPDATA").expect("LOCALAPPDATA"))
            .join("Lumen")
            .join(format!("{name}.exe"))
    } else {
        PathBuf::from("/usr/bin").join(name)
    }
}

fn old_ledger(db: &Path, from: &str, rows: i64, tag: &str) {
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    let c = Connection::open(db).unwrap();
    c.execute_batch(match from {
        "1.8.0" => DDL_180,
        "1.9.0" => DDL_190,
        other => panic!("no schema for {other}"),
    })
    .unwrap();
    for i in 0..rows {
        c.execute(
            "INSERT INTO read_events(ts,tool,path,tokens_returned,full_tokens,saved_tokens,routed_via) \
             VALUES ('2026-10-01T00:00:00Z','smart_read',?1,10,100,90,'smart_read')",
            [format!("/{tag}/{i}")],
        )
        .unwrap();
    }
    c.execute(
        "INSERT INTO faults(ts,kind) VALUES ('2026-10-01T00:00:00Z','seeded')",
        [],
    )
    .unwrap();
}

/// Per-table row counts and the read events themselves, sorted, as JSON: a few dozen rows, so
/// the rows are compared outright rather than through a hash.
fn digest(db: &Path) -> Value {
    let c = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .unwrap_or_else(|e| panic!("open {}: {e}", db.display()));
    let count = |t: &str| -> i64 {
        c.query_row(&format!("SELECT count(*) FROM {t}"), [], |r| r.get(0))
            .unwrap_or_else(|e| panic!("count {t} in {}: {e}", db.display()))
    };
    let mut rows: Vec<String> = c
        .prepare("SELECT ts||'|'||tool||'|'||path||'|'||saved_tokens FROM read_events")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    rows.sort();
    json!({
        "read_events": count("read_events"),
        "faults": count("faults"),
        "rows": rows,
    })
}

fn save(job: &Job, name: &str, v: &Value) {
    write_json(&job.state.join(name), v);
}

fn load(job: &Job, name: &str) -> Value {
    serde_json::from_slice(&std::fs::read(job.state.join(name)).unwrap()).unwrap()
}

fn run(program: &str, args: &[&str]) -> (bool, String) {
    match Command::new(program).args(args).output() {
        Ok(o) => (
            o.status.success(),
            format!(
                "{}{}",
                String::from_utf8_lossy(&o.stdout),
                String::from_utf8_lossy(&o.stderr)
            ),
        ),
        Err(e) => (false, e.to_string()),
    }
}

/// 1–2. What the previous release left, and its hooks firing before this build ever ran.
#[test]
#[ignore = "upgrade workflow only: writes the real home"]
fn seed() {
    let job = job();
    let rig = real_rig(&job.home);
    let mcp = sidecar();
    let old_db = job.previous.join("lumen.db");
    old_ledger(&old_db, &job.from, SEEDED, "old");
    std::fs::write(job.previous.join("faults.jsonl"), "{\"k\":1}\n{\"k\":2}\n").unwrap();
    std::fs::write(
        job.previous.join("lumen_settings.json"),
        "{\"billing_mode\":\"api\"}",
    )
    .unwrap();
    std::fs::write(
        job.previous.join("update_check.json"),
        "{\"last_checked\":1}",
    )
    .unwrap();

    // The shims as that release installed them, baking the previous folder.
    let dir = lumen_dir_in(&job.home);
    std::fs::create_dir_all(&dir).unwrap();
    for name in ["lumen_meter.sh", "lumen_read_intercept.sh"] {
        let at = dir.join(name);
        std::fs::write(&at, upgrade_shim(&job.from, name, &job.previous, &mcp)).unwrap();
        set_mode(&at, 0o755).unwrap();
    }
    let settings = global_settings_path_in(&job.home);
    std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
    let audit = json!({"type": "command", "command": "/usr/local/bin/audit-read"});
    write_json(
        &settings,
        &json!({"model": "opus", "hooks": {"PreToolUse": [{"matcher": "Read", "hooks": [audit]}]}}),
    );
    let step = step_install_hooks_in(&job.home);
    assert_eq!(step.status, StepStatus::Ok, "{step:?}");
    write_json(
        &claude_json_path_in(&job.home),
        &json!({"numStartups": 7, "mcpServers": {
            "lumen": {"type": "stdio", "command": mcp.to_string_lossy(), "args": [],
                      "env": {"LUMEN_DB": old_db.to_string_lossy()}},
            "github": {"type": "stdio", "command": "/usr/local/bin/github-mcp", "args": [], "env": {}}
        }}),
    );
    std::fs::write(marker_path_in(&job.home), "").unwrap();
    std::fs::write(
        autostart_marker_in(&job.home),
        installed("Lumen").to_string_lossy().as_bytes(),
    )
    .unwrap();
    std::fs::write(
        job.home.join(".lumen_db_path"),
        old_db.to_string_lossy().as_bytes(),
    )
    .unwrap();

    seed_platform(&job);

    // The previous release's hooks work before this build has run.
    let env = rig.claude_env(READ_SESSION);
    let file = rig.file("hello.rs", HELLO);
    rig.meter(&rig.captured("read_post.json", Some(&file)), &env);
    let before = digest(&old_db);
    assert_eq!(
        before["read_events"],
        SEEDED + 1,
        "the old hook wrote: {before}"
    );
    save(&job, "before.json", &before);
    println!(
        "seeded {} from {}: {before}",
        job.previous.display(),
        job.from
    );
}

/// The previous install's traces outside its data folder, and the login entry when it was on.
fn seed_platform(job: &Job) {
    #[cfg(target_os = "macos")]
    {
        let lib = job.home.join("Library");
        for d in ["Logs", "Caches"] {
            let at = lib.join(d).join(PREVIOUS);
            std::fs::create_dir_all(&at).unwrap();
            std::fs::write(at.join("seeded.txt"), d).unwrap();
        }
        let (ok, out) = run(
            "defaults",
            &[
                "write",
                PREVIOUS,
                "NSStatusItem Preferred Position Item-0",
                "-float",
                POSITION,
            ],
        );
        assert!(ok, "defaults write: {out}");
        // The previous app, so the install replaces something: its bundle id is the old one.
        let app = PathBuf::from("/Applications/Lumen.app/Contents");
        std::fs::create_dir_all(app.join("MacOS")).unwrap();
        std::fs::write(
            app.join("Info.plist"),
            format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\"><dict>\
                 <key>CFBundleIdentifier</key><string>{PREVIOUS}</string>\
                 <key>CFBundleExecutable</key><string>Lumen</string></dict></plist>\n"
            ),
        )
        .unwrap();
        std::fs::write(app.join("MacOS/Lumen"), "#!/bin/sh\nexit 0\n").unwrap();
        if job.login_on {
            let agents = lib.join("LaunchAgents");
            std::fs::create_dir_all(&agents).unwrap();
            std::fs::write(
                agents.join("Lumen.plist"),
                format!(
                    "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\"><dict>\
                     <key>Label</key><string>Lumen</string><key>ProgramArguments</key>\
                     <array><string>{}</string></array><key>RunAtLoad</key><true/></dict></plist>\n",
                    installed("Lumen").display()
                ),
            )
            .unwrap();
        }
    }
    #[cfg(windows)]
    {
        let local = PathBuf::from(std::env::var("LOCALAPPDATA").unwrap());
        let at = local.join(PREVIOUS);
        std::fs::create_dir_all(&at).unwrap();
        std::fs::write(at.join("seeded.txt"), "webview").unwrap();
        let instdir = local.join("Lumen").to_string_lossy().into_owned();
        let uninstall =
            format!(r"HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\{PREVIOUS}");
        for (name, value) in [
            ("DisplayName", "Lumen".to_string()),
            ("Publisher", OLD_VENDOR.to_string()),
            ("InstallLocation", format!("\"{instdir}\"")),
        ] {
            let (ok, out) = run(
                "reg",
                &[
                    "add", &uninstall, "/v", name, "/t", "REG_SZ", "/d", &value, "/f",
                ],
            );
            assert!(ok, "reg add {name}: {out}");
        }
        let vendor = format!(r"HKCU\Software\{OLD_VENDOR}\Lumen");
        let (ok, out) = run(
            "reg",
            &["add", &vendor, "/ve", "/t", "REG_SZ", "/d", &instdir, "/f"],
        );
        assert!(ok, "reg add vendor: {out}");
        if job.login_on {
            let exe = installed("Lumen").to_string_lossy().into_owned();
            let (ok, out) = run(
                "reg",
                &[
                    "add",
                    r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run",
                    "/v",
                    "Lumen",
                    "/t",
                    "REG_SZ",
                    "/d",
                    &exe,
                    "/f",
                ],
            );
            assert!(ok, "reg add Run: {out}");
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        for d in [".cache", ".config"] {
            let at = job.home.join(d).join(PREVIOUS);
            std::fs::create_dir_all(&at).unwrap();
            std::fs::write(at.join("seeded.txt"), d).unwrap();
        }
        if job.login_on {
            let auto = job.home.join(".config/autostart");
            std::fs::create_dir_all(&auto).unwrap();
            std::fs::write(
                auto.join("Lumen.desktop"),
                format!(
                    "[Desktop Entry]\nType=Application\nName=Lumen\nExec={}\n",
                    installed("Lumen").display()
                ),
            )
            .unwrap();
        }
    }
}

/// Whether the first start also ran Setup.
///
/// The Setup page runs Setup as it loads, and Home sends the window there while Setup is
/// needed. On Windows and Linux the hidden main window loads its page at the start, so the
/// first start runs Setup, which then freezes the backup. On macOS a hidden window loads
/// nothing until it is first opened, and Setup waits for the user.
const FIRST_START_RUNS_SETUP: bool = !cfg!(target_os = "macos");

/// Step 6: the launched app moves the previous install. Wait for its committed marker and,
/// where the first start runs Setup, for Setup and the freeze too, so the quit cannot cut them
/// short.
#[test]
#[ignore = "upgrade workflow only: reads the real home"]
fn wait_for_the_move() {
    let job = job();
    let deadline = std::time::Instant::now() + Duration::from_secs(180);
    loop {
        match marker::read(&job.n) {
            Seen::Migrated(m)
                if backup_dir(&job.n, &m).is_some_and(|b| {
                    b.join("data").is_dir()
                        && (!FIRST_START_RUNS_SETUP || is_frozen(&job.n.join("lumen.db"), &b))
                }) =>
            {
                println!("moved: {m:?}");
                return;
            }
            other if std::time::Instant::now() > deadline => {
                let listing: Vec<String> = match std::fs::read_dir(job.n.parent().unwrap()) {
                    Ok(rd) => rd
                        .flatten()
                        .map(|e| e.file_name().to_string_lossy().into_owned())
                        .collect(),
                    Err(e) => vec![format!("(unreadable: {e})")],
                };
                panic!(
                    "no move{} within 180 s: marker {other:?}; data root holds {listing:?}",
                    if FIRST_START_RUNS_SETUP {
                        " and freeze"
                    } else {
                        ""
                    }
                );
            }
            _ => std::thread::sleep(Duration::from_secs(2)),
        }
    }
}

/// 7. Everything the first start was to do, and one installed app.
#[test]
#[ignore = "upgrade workflow only: reads the real home"]
fn verify_after_start() {
    let job = job();
    let before = load(&job, "before.json");
    let new_db = job.n.join("lumen.db");
    let Seen::Migrated(m) = marker::read(&job.n) else {
        panic!("no committed marker in {}", job.n.display());
    };
    let backup = backup_dir(&job.n, &m).expect("the marker names a backup");

    // The ledger: every row came, the backup holds the previous ledger whole.
    assert_eq!(digest(&new_db)["rows"], before["rows"]);
    assert_eq!(digest(&new_db)["read_events"], before["read_events"]);
    assert_eq!(
        digest(&backup.join("data").join("lumen.db"))["rows"],
        before["rows"]
    );
    assert!(
        !job.previous.exists(),
        "the previous folder moved into the backup"
    );

    // Claude's files: Lumen's entry re-pointed, the foreign one untouched; the pointer and
    // the shims name the new folder.
    let cfg: Value =
        serde_json::from_slice(&std::fs::read(claude_json_path_in(&job.home)).unwrap()).unwrap();
    assert_eq!(
        cfg["mcpServers"]["lumen"]["env"]["LUMEN_DB"],
        new_db.to_string_lossy().as_ref()
    );
    assert_eq!(
        cfg["mcpServers"]["github"]["command"],
        "/usr/local/bin/github-mcp"
    );
    assert_eq!(cfg["numStartups"], 7);
    let pointer = std::fs::read_to_string(job.home.join(".lumen_db_path")).unwrap();
    assert_eq!(Path::new(pointer.trim()), new_db.as_path());
    for name in ["lumen_meter.sh", "lumen_read_intercept.sh"] {
        let text = std::fs::read_to_string(lumen_dir_in(&job.home).join(name)).unwrap();
        assert!(
            !text.contains(PREVIOUS),
            "{name} still bakes the previous folder"
        );
    }

    // The hooks fire into the new ledger, and nothing is frozen before Setup.
    let rig = real_rig(&job.home);
    let file = rig.file("hello.rs", HELLO);
    rig.meter(
        &rig.captured("read_post.json", Some(&file)),
        &rig.claude_env(READ_SESSION),
    );
    assert_eq!(
        digest(&new_db)["read_events"],
        before["read_events"].as_i64().unwrap() + 1
    );
    // Setup and the freeze: still to come on macOS, done by the first start elsewhere.
    assert_eq!(
        !setup_needed_with(&job.home, &job.n, &Seen::Migrated(m.clone())),
        FIRST_START_RUNS_SETUP,
        "Setup ran at the first start"
    );
    assert_eq!(
        is_frozen(&new_db, &backup),
        FIRST_START_RUNS_SETUP,
        "the backup is frozen"
    );

    verify_platform(&job, &backup);
    println!("verified: {}", digest(&new_db));
}

fn verify_platform(job: &Job, backup: &Path) {
    #[cfg(target_os = "macos")]
    {
        let _ = backup;
        let lib = job.home.join("Library");
        for d in ["Logs", "Caches"] {
            assert!(
                !lib.join(d).join(PREVIOUS).exists(),
                "{d} still holds the previous name"
            );
        }
        assert!(
            !lib.join("Preferences")
                .join(format!("{PREVIOUS}.plist"))
                .exists(),
            "the previous Preferences file came back (cfprefsd)"
        );
        let (ok, out) = run(
            "defaults",
            &[
                "read",
                lumen_core::meter::APP_ID,
                "NSStatusItem Preferred Position Item-0",
            ],
        );
        assert!(
            ok && out.trim() == POSITION,
            "the menu-bar position was not carried: {out}"
        );
        let (_, id) = run(
            "defaults",
            &[
                "read",
                "/Applications/Lumen.app/Contents/Info.plist",
                "CFBundleIdentifier",
            ],
        );
        assert_eq!(id.trim(), lumen_core::meter::APP_ID, "the installed app");
        let apps: Vec<String> = [
            PathBuf::from("/Applications"),
            job.home.join("Applications"),
        ]
        .iter()
        .flat_map(|d| std::fs::read_dir(d).into_iter().flatten().flatten())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("Lumen") && n.ends_with(".app"))
        .collect();
        assert_eq!(apps, ["Lumen.app"], "one installed app");
        let agent = lib.join("LaunchAgents/Lumen.plist");
        assert_eq!(agent.is_file(), job.login_on, "the login item is as it was");
    }
    #[cfg(windows)]
    {
        let local = PathBuf::from(std::env::var("LOCALAPPDATA").unwrap());
        assert!(
            !local.join(PREVIOUS).exists(),
            "%LOCALAPPDATA% still holds the previous name"
        );
        let gone = |key: &str| !run("reg", &["query", key]).0;
        assert!(
            gone(&format!(
                r"HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\{PREVIOUS}"
            )),
            "the second Apps & features entry is still there"
        );
        assert!(
            gone(&format!(r"HKCU\Software\{OLD_VENDOR}\Lumen")),
            "the previous manufacturer key is still there"
        );
        let (ok, out) = run(
            "reg",
            &[
                "query",
                r"HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\Lumen",
                "/v",
                "Publisher",
            ],
        );
        assert!(ok && out.contains("HackPoint"), "Lumen's own entry: {out}");
        let (on, _) = run(
            "reg",
            &[
                "query",
                r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run",
                "/v",
                "Lumen",
            ],
        );
        assert_eq!(on, job.login_on, "the login item is as it was");
        let _ = backup;
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = backup;
        for d in [".cache", ".config"] {
            assert!(
                !job.home.join(d).join(PREVIOUS).exists(),
                "{d} still holds the previous name"
            );
        }
        let (ok, out) = run("dpkg-query", &["-W", "-f", "${Status}\\n", "lumen"]);
        assert!(
            ok && out.matches("install ok installed").count() == 1,
            "one lumen package: {out}"
        );
        let desktops: Vec<PathBuf> = [
            PathBuf::from("/usr/share/applications"),
            job.home.join(".local/share/applications"),
        ]
        .iter()
        .flat_map(|d| std::fs::read_dir(d).into_iter().flatten().flatten())
        .map(|e| e.path())
        .filter(|p| {
            std::fs::read_to_string(p).is_ok_and(|t| {
                t.lines()
                    .any(|l| l.starts_with("Exec=") && l.contains("Lumen"))
            })
        })
        .collect();
        assert_eq!(
            desktops.len(),
            1,
            "one desktop entry runs Lumen: {desktops:?}"
        );
        let auto = job.home.join(".config/autostart/Lumen.desktop");
        assert_eq!(auto.is_file(), job.login_on, "the login item is as it was");
    }
}

/// 9. Setup, then the freeze, and the hooks after both.
#[test]
#[ignore = "upgrade workflow only: writes the real home"]
fn setup_then_freeze() {
    let job = job();
    assert!(
        std::env::var_os("LUMEN_DB").is_none(),
        "Setup bakes an exported LUMEN_DB into the config this checks"
    );
    let new_db = job.n.join("lumen.db");
    let Seen::Migrated(m) = marker::read(&job.n) else {
        panic!("no committed marker");
    };
    let backup = backup_dir(&job.n, &m).unwrap();
    let item = LoginItem(Cell::new(job.login_on));
    let steps = run_setup_in(&job.home, &item);
    assert!(
        steps
            .iter()
            .all(|s| matches!(s.status, StepStatus::Ok | StepStatus::Warn)),
        "{steps:?}"
    );
    assert!(!setup_needed_with(&job.home, &job.n, &marker::read(&job.n)));
    let report = freeze_after_setup(&job.home, &job.n);
    assert!(report.problems.is_empty(), "{report:?}");
    assert!(is_frozen(&new_db, &backup));
    assert!(
        std::fs::metadata(backup.join("data").join("lumen.db"))
            .unwrap()
            .permissions()
            .readonly(),
        "the backup is read-only"
    );
    let rows = digest(&new_db)["read_events"].as_i64().unwrap();
    let rig = real_rig(&job.home);
    let file = rig.file("hello.rs", HELLO);
    rig.meter(
        &rig.captured("read_post.json", Some(&file)),
        &rig.claude_env(READ_SESSION),
    );
    assert_eq!(
        digest(&new_db)["read_events"],
        rows + 1,
        "the hooks after Setup"
    );
    assert_eq!(
        item.0.get(),
        job.login_on,
        "Setup keeps the login item as it was"
    );
    save(&job, "after-setup.json", &digest(&new_db));
}

/// 10a. An old app started again after the move writes the previous folder anew.
#[test]
#[ignore = "upgrade workflow only: writes the real home"]
fn recreate() {
    let job = job();
    assert!(!job.previous.exists());
    old_ledger(
        &job.previous.join("lumen.db"),
        &job.from,
        RECREATED,
        "recreated",
    );
    save(&job, "before-import.json", &digest(&job.n.join("lumen.db")));
}

/// 10b. The relaunch reported the re-created folder and merged nothing; `lumen import-ledger`
/// brings its rows in once.
#[test]
#[ignore = "upgrade workflow only: writes the real home"]
fn report_and_import() {
    let job = job();
    let new_db = job.n.join("lumen.db");
    let before = load(&job, "before-import.json");
    assert_eq!(
        digest(&new_db)["read_events"],
        before["read_events"],
        "nothing was merged"
    );
    let mut log_dirs = vec![
        job.home
            .join("Library/Logs")
            .join(lumen_core::meter::APP_ID),
        job.n.join("logs"),
    ];
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        log_dirs.push(
            PathBuf::from(local)
                .join(lumen_core::meter::APP_ID)
                .join("logs"),
        );
    }
    let logs: Vec<PathBuf> = log_dirs
        .iter()
        .flat_map(|d| std::fs::read_dir(d).into_iter().flatten().flatten())
        .map(|e| e.path())
        .collect();
    let said = logs
        .iter()
        .filter_map(|p| std::fs::read_to_string(p).ok())
        .any(|t| t.contains(PREVIOUS) && t.contains("import-ledger"));
    assert!(
        said,
        "the start named the folder and the command; logs: {logs:?}"
    );

    let cli = installed("lumen-cli");
    let dir = job.previous.to_string_lossy().into_owned();
    let (ok, out) = run(&cli.to_string_lossy(), &["import-ledger", &dir]);
    assert!(ok, "import-ledger: {out}");
    assert_eq!(
        digest(&new_db)["read_events"],
        before["read_events"].as_i64().unwrap() + RECREATED,
        "{out}"
    );
    assert!(
        !job.previous.exists(),
        "the imported folder moved into legacy-imports"
    );
    let (_, again) = run(&cli.to_string_lossy(), &["import-ledger", &dir]);
    println!("first import: {out}\nrerun: {again}");
}
