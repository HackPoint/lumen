//! The typed manifest reads every committed method file, and the stubs fail loudly.

use lumen_bench::manifest::{AssetSpec, PLATFORMS, Tasks, Tools, VersionsManifest};
use std::process::Command;

const VERSIONS: [&str; 10] = [
    "1.0.0", "1.1.5", "1.2.4", "1.3.1", "1.4.0", "1.5.1", "1.6.0", "1.7.0", "1.8.0", "1.9.0",
];

#[test]
fn every_version_answers_for_every_os() {
    let m = VersionsManifest::load_committed().unwrap();
    assert_eq!(m.meta.versions, VERSIONS);
    for v in VERSIONS {
        let e = m.get(v).unwrap();
        assert_eq!(e.tag, format!("v{v}"));
        for p in PLATFORMS {
            match e.asset.get(p).unwrap_or_else(|| panic!("{v} {p}")) {
                AssetSpec::Pinned(a) => {
                    assert_eq!(a.sha256.len(), 64, "{v} {p}");
                    assert!(a.name.contains(v), "{v} {p}: {}", a.name);
                }
                AssetSpec::Na(n) => assert!(!n.na.trim().is_empty()),
            }
        }
    }
    // 1.0.0 has no Linux app asset: an explicit n/a with a reason, not a gap.
    let na = m.asset("1.0.0", "linux-x86_64").unwrap().unwrap_err();
    assert!(na.contains("no Linux app asset"), "{na}");
    let a = m.asset("1.9.0", "macos-arm64").unwrap().unwrap();
    assert_eq!(a.name, "Lumen_1.9.0_aarch64.dmg");
    // Every other cell is pinned.
    let na_cells = VERSIONS
        .iter()
        .flat_map(|v| PLATFORMS.iter().map(move |p| (*v, *p)))
        .filter(|(v, p)| m.asset(v, p).unwrap().is_err())
        .count();
    assert_eq!(na_cells, 1);
}

#[test]
fn the_capability_map_is_typed() {
    let m = VersionsManifest::load_committed().unwrap();
    let v100 = m.get("1.0.0").unwrap();
    assert_eq!(v100.saved_tokens, "clamped");
    assert!(!v100.hook_cli && v100.daemon_fixed_port && v100.daemon_reads_home);
    let v190 = m.get("1.9.0").unwrap();
    assert!(v190.hook_cli && v190.bash_meter && v190.e2e);
    assert_eq!(v190.repeat_reads, "default");
    assert_eq!(m.meta.e2e, ["1.1.5", "1.6.0", "1.8.0", "1.9.0"]);
    assert_eq!(m.meta.e2e_stand_in["1.0.0"], "1.1.5");
    assert!(m.get("9.9.9").is_err());
}

#[test]
fn a_manifest_that_skips_a_platform_or_a_version_is_refused() {
    let text = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../versions/manifest.toml"),
    )
    .unwrap()
    .replace("\r\n", "\n");
    // Drop one asset table: the platform is neither pinned nor n/a.
    let cut = text.replacen(
        "[version.\"1.9.0\".asset.windows-x64]",
        "[version.\"1.9.0\".asset.nowhere]",
        1,
    );
    let err = VersionsManifest::parse(&cut).unwrap_err();
    assert!(
        err.contains("windows-x64") || err.contains("nowhere"),
        "{err}"
    );
    // A listed version without a table.
    let cut = text.replacen("\"1.9.0\"]", "\"1.9.0\", \"2.0.0\"]", 1);
    assert!(VersionsManifest::parse(&cut).is_err());
    // A typo in the capability map is not read as "unset".
    let cut = text.replacen("hook_cli = true", "hook_clii = true", 1);
    assert!(VersionsManifest::parse(&cut).is_err());
}

#[test]
fn the_competitor_files_parse() {
    let t = Tools::load_committed().unwrap();
    assert_eq!(t.meta.model, "claude-sonnet-5-5");
    for id in [
        "none",
        "lumen-1.9.0",
        "squeez",
        "rtk",
        "headroom",
        "caveman",
    ] {
        t.arm(id).unwrap();
    }
    let l = t.arm("lumen-1.9.0").unwrap();
    assert_eq!(l.asset[0].name, "lumen_1.9.0_amd64.deb");
    assert_eq!(l.asset[0].sha256.len(), 64);
    assert_eq!(t.arm("none").unwrap().kind, "none");

    let k = Tasks::load_committed().unwrap();
    assert_eq!(k.swe.len(), 10);
    assert_eq!(k.swe_bench.count, 10);
    assert_eq!(k.scenario.len(), 3);
    assert!(k.scenario.iter().all(|s| s.base_commit.len() == 40));
}

fn bench(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_lumen-bench"))
        .args(args)
        .output()
        .unwrap()
}

/// `versions` is implemented (V2): a bare invocation must not start a run, so it refuses and says
/// what to name; `--help` still works.
#[test]
fn a_bare_versions_command_starts_nothing() {
    for args in [vec!["versions"], vec!["versions", "--tokens-only"]] {
        let o = bench(&args);
        assert!(!o.status.success(), "{args:?}");
        let err = String::from_utf8_lossy(&o.stderr);
        assert!(
            err.contains("--versions") && err.contains("--all"),
            "{args:?}: {err}"
        );
    }
    let help = bench(&["versions", "--help"]);
    assert!(help.status.success());
    assert!(String::from_utf8_lossy(&help.stdout).contains("versions"));
}

#[test]
fn the_new_subcommands_are_stubs_that_exit_non_zero() {
    for sub in ["competitors", "e2e"] {
        for args in [vec![sub], vec![sub, "run", "--out", "x"]] {
            let o = bench(&args);
            assert!(!o.status.success(), "{args:?}");
            let err = String::from_utf8_lossy(&o.stderr);
            assert!(err.contains("not yet implemented"), "{args:?}: {err}");
        }
        let help = bench(&[sub, "--help"]);
        assert!(help.status.success());
        assert!(String::from_utf8_lossy(&help.stdout).contains(sub));
    }
}
