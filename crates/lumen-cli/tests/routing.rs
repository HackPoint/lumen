//! Which command does what, driven through the built `lumen` binary.
//!
//! 1.9.0 moved three things at once: bare `lumen` prints one headline line, the terminal
//! dashboard is `lumen dash`, and the fault report is `lumen faults`, with `lumen report`
//! kept for the savings detail. A fault report filed with the old spelling has to keep
//! working, because it is in shell history, in docs written before 1.9.0 and in the
//! `/lumen-report` command of an older plugin, so these run the real binary rather than the
//! parser alone.
//!
//! Every run gets a home and a ledger path of its own: nothing here may read the developer's
//! real ledger, pointer file or settings.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// A fault fixture the renderer is snapshot-tested with, so its body is known to render.
fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../lumen-core/tests/fixtures/faults_mixed.json")
}

/// A phrase from the head of the fixture's rendered body.
const FIXTURE_HEAD: &str = "retry escape valve fired 7";

/// Run `lumen` with `args` in a sandbox: its own HOME and its own ledger path, both empty.
fn lumen(args: &[&str]) -> Output {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_lumen"));
    cmd.args(args)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("LUMEN_DB", dir.path().join("lumen.db"));
    let out = cmd.output().expect("the lumen binary runs");
    drop(dir);
    out
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

#[test]
fn bare_lumen_prints_exactly_one_line_and_points_at_the_detail() {
    let o = lumen(&[]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    let out = stdout(&o);
    assert_eq!(out.lines().count(), 1, "{out:?}");
    assert!(out.ends_with('\n'), "{out:?}");
    assert!(out.starts_with("Lumen: "), "{out:?}");
    assert!(out.contains("lumen report"), "{out:?}");
    // Until 1.9.0 bare `lumen` was the dashboard, so the line says where that went.
    assert!(out.contains("lumen dash"), "{out:?}");
    assert!(stderr(&o).is_empty(), "{}", stderr(&o));
}

#[test]
fn lumen_faults_renders_the_fault_report() {
    let f = fixture();
    let o = lumen(&["faults", "--faults", f.to_str().unwrap(), "--dry-run"]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert!(stdout(&o).contains(FIXTURE_HEAD), "{}", stdout(&o));
    // The new name says nothing about an old one.
    assert!(!stderr(&o).contains("now `lumen faults`"), "{}", stderr(&o));
}

#[test]
fn lumen_faults_still_refuses_to_file_without_yes() {
    let f = fixture();
    let o = lumen(&["faults", "--faults", f.to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(2), "{}", stderr(&o));
    assert!(stdout(&o).contains(FIXTURE_HEAD), "{}", stdout(&o));
    assert!(
        stderr(&o).contains("lumen faults: not filed"),
        "{}",
        stderr(&o)
    );
}

#[test]
fn an_old_lumen_report_with_fault_flags_still_renders_and_says_where_it_moved() {
    let f = fixture();
    let old = lumen(&["report", "--faults", f.to_str().unwrap(), "--dry-run"]);
    let new = lumen(&["faults", "--faults", f.to_str().unwrap(), "--dry-run"]);
    assert_eq!(old.status.code(), Some(0), "{}", stderr(&old));
    assert_eq!(stdout(&old), stdout(&new), "the same report either way");
    let said = stderr(&old);
    let note: Vec<&str> = said
        .lines()
        .filter(|l| l.contains("lumen faults"))
        .collect();
    assert_eq!(note.len(), 1, "one line says so: {said}");
    assert!(note[0].contains("lumen report"), "{}", note[0]);
}

#[test]
fn every_old_fault_flag_alone_routes_to_the_fault_report() {
    let f = fixture();
    let fixture_arg = f.to_str().unwrap();
    // Each with the fixture, so nothing reads a ledger; --dry-run is in every set so
    // nothing can be filed by a test.
    for extra in [
        vec!["--dry-run"],
        vec!["--dry-run", "--include-source"],
        vec!["--dry-run", "--repo", "someone/else"],
    ] {
        let mut args = vec!["report", "--faults", fixture_arg];
        args.extend(extra.iter().copied());
        let o = lumen(&args);
        assert_eq!(o.status.code(), Some(0), "{args:?}: {}", stderr(&o));
        assert!(stdout(&o).contains(FIXTURE_HEAD), "{args:?}");
        assert!(stderr(&o).contains("lumen faults"), "{args:?}");
    }
}

#[test]
fn lumen_report_alone_is_the_savings_detail_and_files_nothing() {
    let o = lumen(&["report"]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    let out = stdout(&o);
    // An empty sandbox has recorded nothing, and the detail says so, and how it would measure.
    assert!(out.contains("no Lumen activity recorded"), "{out}");
    assert!(out.contains("How it is measured"), "{out}");
    assert!(
        !out.contains('$'),
        "no dollar figure where nothing was priced: {out}"
    );
    // Not a fault report: nothing was rendered or read for one.
    assert!(!out.contains("no faults recorded"), "{out}");
    assert!(!stderr(&o).contains("no faults recorded"), "{}", stderr(&o));
}

#[test]
fn lumen_report_json_is_the_summary_the_app_receives() {
    let o = lumen(&["report", "--json", "--as-of", "2026-10-14T12:00:00Z"]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).expect("JSON on stdout");
    assert_eq!(v["asOf"], "2026-10-14T12:00:00Z");
    assert_eq!(v["allTime"]["dataQuality"]["state"], "empty");
    assert_eq!(v["status"]["state"], "noData");
}

#[test]
fn lumen_dash_exists_and_keeps_no_anim() {
    let o = lumen(&["dash", "--help"]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert!(stdout(&o).contains("--no-anim"), "{}", stdout(&o));

    // Piped, the dashboard prints its one-shot line instead of taking over the terminal.
    let o = lumen(&["dash", "--no-anim"]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert_eq!(stdout(&o), "no data\n");
}

#[test]
fn an_old_lumen_no_anim_still_opens_the_dashboard_and_says_where_it_moved() {
    let o = lumen(&["--no-anim"]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert_eq!(stdout(&o), "no data\n");
    assert!(stderr(&o).contains("lumen dash"), "{}", stderr(&o));
    assert_eq!(stderr(&o).lines().count(), 1, "{}", stderr(&o));
}

#[test]
fn no_anim_given_to_another_command_is_said_to_do_nothing() {
    let o = lumen(&["--no-anim", "report"]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert!(stdout(&o).contains("How it is measured"), "{}", stdout(&o));
    let said = stderr(&o);
    assert_eq!(said.lines().count(), 1, "{said}");
    assert!(
        said.contains("--no-anim") && said.contains("lumen dash"),
        "{said}"
    );
}

#[test]
fn the_help_names_every_command() {
    let o = lumen(&["--help"]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    let out = stdout(&o);
    for cmd in ["dash", "report", "faults", "show", "doctor"] {
        assert!(
            out.lines().any(|l| l.trim_start().starts_with(cmd)),
            "{cmd} missing from:\n{out}"
        );
    }
}
