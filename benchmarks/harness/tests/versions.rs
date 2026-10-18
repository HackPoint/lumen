//! The version runner end to end on a tiny fixture, with a spy in front of the real `lumen-mcp`.
//!
//! The spy is a shell script that appends its arguments to a spawn log and then execs the real
//! binary. A build the capability map says has no `hook` subcommand must never appear in that
//! log with `hook` as its first argument: on a real 1.0.0 that would start an MCP server.
#![cfg(unix)]

use lumen_bench::builds::Build;
use lumen_bench::corpus;
use lumen_bench::driver::Identity;
use lumen_bench::latency::LatencyConfig;
use lumen_bench::manifest::VersionsManifest;
use lumen_bench::run::{self, Plan};
use lumen_bench::versions::caps::Caps;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

fn big_rs() -> String {
    let mut s = String::from("//! A generated fixture.\n\n");
    for i in 0..40 {
        s.push_str(&format!(
            "pub fn step_{i:02}(x: i64) -> i64 {{\n    let a = x + {i};\n    let b = a * 2;\n    \
             let c = b - {i};\n    let d = c / 3;\n    d + a\n}}\n\n"
        ));
    }
    s
}

fn write_fixture(root: &Path) {
    let files: [(&str, String); 2] = [
        ("src/big.rs", big_rs()),
        (
            "src/small.rs",
            "pub fn add(a: i64, b: i64) -> i64 {\n    a + b\n}\n".to_string(),
        ),
    ];
    for (rel, text) in files {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, text).unwrap();
    }
}

/// `target/<profile>/lumen-mcp`, built with the same cargo and profile as this test.
fn mcp_bin() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let profile_dir = exe.parent().and_then(Path::parent).unwrap().to_path_buf();
    let target_dir = profile_dir.parent().unwrap();
    let mut build = Command::new(env!("CARGO"));
    build
        .args(["build", "--quiet", "-p", "lumen-mcp", "--target-dir"])
        .arg(target_dir);
    match profile_dir.file_name().and_then(|n| n.to_str()) {
        Some("debug") => {}
        Some("release") => {
            build.arg("--release");
        }
        other => panic!("an unknown cargo profile directory {other:?}"),
    }
    assert!(build.status().unwrap().success(), "cargo build failed");
    let bin = profile_dir.join("lumen-mcp");
    assert!(bin.is_file());
    bin
}

/// A script that logs its arguments and runs the real binary.
fn spy(dir: &Path, name: &str, real: &Path, log: &Path) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(
        &path,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nexec '{}' \"$@\"\n",
            log.display(),
            real.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn caps_of(version: &str) -> Caps {
    let m = VersionsManifest::load_committed().unwrap();
    Caps::from_entry(version, m.get(version).unwrap()).unwrap()
}

fn build(label: &str, bin: PathBuf, caps: Caps) -> Build {
    Build {
        label: label.to_string(),
        role: "version",
        sha: "0".repeat(40),
        dirty: false,
        bin,
        profile: "debug",
        profile_note: "the test's own profile".to_string(),
        source: "a spy in front of the workspace's lumen-mcp".to_string(),
        identity: Identity::Env,
        inproc: false,
        caps: Some(caps),
    }
}

fn plan(dir: &Path, builds: Vec<Build>, tokens: bool, latency: bool) -> Plan {
    let root = dir.join("fixture");
    write_fixture(&root);
    Plan {
        label: "versions-test".to_string(),
        corpora: vec![corpus::from_dir("fixture", &root).unwrap()],
        builds,
        tokens,
        latency: latency.then_some(LatencyConfig { runs: 3, warmup: 1 }),
        work: dir.join("work"),
    }
}

fn spawned(log: &Path) -> Vec<String> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

/// A hook-less build (the map's 1.1.5: signed, no `hook`), plus no Lumen, through the tokens and
/// latency runners.
#[test]
fn a_build_without_hook_gets_n_a_rows_and_is_never_spawned_with_hook() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("spawns.log");
    let bin = spy(dir.path(), "lumen-mcp", &mcp_bin(), &log);
    let builds = vec![build("1.1.5", bin, caps_of("1.1.5")), Build::no_lumen()];
    let p = plan(dir.path(), builds, true, true);
    let out = run::execute(&p).expect("the run");

    let spawns = spawned(&log);
    assert!(
        spawns.len() > 5,
        "the server was spawned by the token and latency passes: {spawns:?}"
    );
    let hooked: Vec<&String> = spawns.iter().filter(|a| a.starts_with("hook")).collect();
    assert!(
        hooked.is_empty(),
        "a version the map says has no `hook` subcommand was spawned with it: {hooked:?}"
    );

    let l = out.results.latency.as_ref().unwrap();
    let old = &l.builds[0];
    let st = |id: &str| {
        old.series
            .iter()
            .find(|s| s.id == id)
            .unwrap()
            .status
            .clone()
    };
    for id in [
        "hook.floor",
        "hook.intercept_pass.spawn",
        "hook.intercept_block.spawn",
        "hook.meter_read.spawn",
        "hook.meter_bash.spawn",
        "hook.per_call_identity",
        "server.identity.placed",
        "server.identity.unplaced",
        "server.repeat_unchanged.300-999",
        "server.repeat_changed.300-999",
    ] {
        assert!(st(id).starts_with("n/a — "), "{id}: {}", st(id));
    }
    assert!(st("hook.meter_bash.spawn").contains("no Bash meter"));
    // What the version can do is measured: the server series are figures.
    assert_eq!(st("server.smart_read.300-999"), "measured");
    assert!(
        old.series
            .iter()
            .find(|s| s.id == "server.smart_read.300-999")
            .unwrap()
            .lat
            .is_some()
    );
    // No Lumen has no figure anywhere in the latency section.
    let none = &l.builds[1];
    assert!(
        none.series
            .iter()
            .all(|s| s.lat.is_none() && s.status.starts_with("n/a"))
    );

    // Tokens: the stub and delta cells of the old build and of no Lumen are n/a, never 0.
    let tsv = out.tsv.as_ref().unwrap();
    for row in tsv.lines().skip(1) {
        let c: Vec<&str> = row.split('\t').collect();
        assert_eq!(c[12], "n/a", "{row}");
        assert_eq!(c[15], "n/a", "{row}");
        assert!(
            c[13].starts_with("n/a — ") && c[16].starts_with("n/a — "),
            "{row}"
        );
    }
    assert!(tsv.lines().skip(1).any(|r| r.starts_with("1.1.5\t")));
    assert!(tsv.lines().skip(1).any(|r| r.starts_with("no-lumen\t")));
    let b = &out.results.builds;
    assert_eq!(b[1].reported_version, "n/a — no Lumen");
    assert!(b[1].labels.iter().any(|l| l.contains("no Lumen")));
    // No Lumen reads the whole file: its outline is the file.
    let big = tsv
        .lines()
        .find(|r| r.starts_with("no-lumen\t") && r.contains("src/big.rs"))
        .unwrap();
    let c: Vec<&str> = big.split('\t').collect();
    assert_eq!(c[6], c[7], "no Lumen: tokens are the full file");
}

/// The control: the same spy under a map that says `hook` exists (1.8.0) does get `hook`
/// spawns, so the log would show a violation.
#[test]
fn the_spawn_log_does_record_hook_for_a_version_that_has_it() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("spawns.log");
    let bin = spy(dir.path(), "lumen-mcp", &mcp_bin(), &log);
    let p = plan(
        dir.path(),
        vec![build("1.9.0", bin, caps_of("1.9.0"))],
        true,
        false,
    );
    run::execute(&p).expect("the run");
    assert!(
        spawned(&log).iter().any(|a| a.starts_with("hook")),
        "the identity probe spawns `hook intercept`"
    );
}

/// The workspace's binary records identity by hook. A map that says 1.6.0 (identity by
/// environment) is wrong about it, and the probe stops the run.
#[test]
fn a_probe_that_disagrees_with_the_map_stops_the_run() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("spawns.log");
    let bin = spy(dir.path(), "lumen-mcp", &mcp_bin(), &log);
    let p = plan(
        dir.path(),
        vec![build("1.6.0", bin, caps_of("1.6.0"))],
        true,
        false,
    );
    let e = match run::execute(&p) {
        Ok(_) => panic!("the run went on with a map the probe contradicts"),
        Err(e) => e,
    };
    assert!(
        e.contains("STOP") && e.contains("1.6.0") && e.contains("env") && e.contains("hook"),
        "{e}"
    );
}

/// A signed binary under a map that says it clamps is a disagreement too: the fixture's small
/// file is longer as an outline than as a file, so the server reports a negative saving.
#[test]
fn a_clamp_claim_the_server_contradicts_stops_the_run() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("spawns.log");
    let bin = spy(dir.path(), "lumen-mcp", &mcp_bin(), &log);
    let mut caps = caps_of("1.1.5");
    caps.saved_clamped = true;
    let p = plan(dir.path(), vec![build("1.1.5", bin, caps)], true, false);
    let e = run::execute(&p).err().expect("the run stops");
    assert!(e.contains("zero floor"), "{e}");
}
