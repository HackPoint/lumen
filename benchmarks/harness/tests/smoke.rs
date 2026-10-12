//! The harness end to end on a tiny fixture: no network, no corpus download, a few seconds.
//!
//! It drives the real `lumen-mcp` binary (built here if it is not already), so the token figures
//! below are what that binary returns for these exact files. If an intentional change to the
//! outline format, the recall rule, the stub or the delta moves them, update EXPECTED_ROWS in the
//! same change: that is the point of pinning them.

use lumen_bench::builds::Build;
use lumen_bench::corpus;
use lumen_bench::driver::Identity;
use lumen_bench::latency::LatencyConfig;
use lumen_bench::run::{self, Output, Plan};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

/// A Rust file over the 300-line interception threshold: 40 functions of 8 lines, plus a header.
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

const SMALL_RS: &str = "\
pub fn add(a: i64, b: i64) -> i64 {
    a + b
}

pub fn double(a: i64) -> i64 {
    add(a, a)
}
";

const TOOLS_PY: &str = "\
class Counter:
    def __init__(self):
        self.n = 0

    def bump(self):
        self.n += 1
        return self.n


def total(xs):
    return sum(xs)
";

const APP_TS: &str = "\
export function greet(name: string): string {
  return `hello ${name}`;
}

export function shout(name: string): string {
  return greet(name).toUpperCase();
}
";

fn write_fixture(root: &Path) {
    let files: [(&str, String); 4] = [
        ("src/big.rs", big_rs()),
        ("src/small.rs", SMALL_RS.to_string()),
        ("lib/tools.py", TOOLS_PY.to_string()),
        ("web/app.ts", APP_TS.to_string()),
    ];
    for (rel, text) in files {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().expect("a fixture file has a parent"))
            .expect("create a fixture directory");
        std::fs::write(&path, text).expect("write a fixture file");
    }
}

/// `target/<profile>/lumen-mcp`, built with the same cargo and profile as this test.
fn mcp_bin() -> PathBuf {
    let exe = std::env::current_exe().expect("the test binary's own path");
    let profile_dir = exe
        .parent()
        .and_then(Path::parent)
        .expect("target/<profile>/deps/<test>")
        .to_path_buf();
    let target_dir = profile_dir.parent().expect("target/<profile>");
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
    let status = build.status().expect("run cargo build -p lumen-mcp");
    assert!(status.success(), "cargo build -p lumen-mcp failed");
    let name = if cfg!(windows) {
        "lumen-mcp.exe"
    } else {
        "lumen-mcp"
    };
    let bin = profile_dir.join(name);
    assert!(bin.is_file(), "{} was not built", bin.display());
    bin
}

fn output() -> &'static Output {
    static OUT: OnceLock<Output> = OnceLock::new();
    OUT.get_or_init(|| {
        let dir = tempfile::tempdir().expect("a scratch directory");
        let root = dir.path().join("fixture");
        write_fixture(&root);
        let corpus = corpus::from_dir("fixture", &root).expect("load the fixture");
        let build = Build {
            label: "current".to_string(),
            role: "candidate",
            sha: "0".repeat(40),
            dirty: false,
            bin: mcp_bin(),
            profile: "debug",
            profile_note: "the smoke test's own profile".to_string(),
            source: "the workspace this test runs in".to_string(),
            // Replaced by what `run::execute` finds by running the binary.
            identity: Identity::Env,
            inproc: true,
        };
        let plan = Plan {
            label: "smoke".to_string(),
            corpora: vec![corpus],
            builds: vec![build],
            tokens: true,
            latency: Some(LatencyConfig { runs: 3, warmup: 1 }),
            work: dir.path().join("work"),
        };
        run::execute(&plan).expect("the harness runs on the fixture")
    })
}

/// build, corpus, path, language, lines, bucket, full, outline, outline route, recall item,
/// recall, recall route, stub, stub route, delta line, delta, delta route, first read matches.
///
/// The three small files are `would_inflate`: for a file this short the outline is longer than the
/// file, so the server returns the file instead, and the table says so rather than hiding it.
const EXPECTED_ROWS: &[&str] = &[
    "current\tfixture\tlib/tools.py\tpython\t11\t<300\t43\t68\twould_inflate\tCounter\t68\twould_inflate\t30\tsmart_read_unchanged/unchanged\t3\t75\twould_inflate/no_base\ttrue",
    "current\tfixture\tsrc/big.rs\trust\t322\t300-999\t2245\t644\tsmart_read\tstep_19\t134\trecall_file\t30\tsmart_read_unchanged/unchanged\t158\t44\tsmart_read_delta/delta\ttrue",
    "current\tfixture\tsrc/small.rs\trust\t7\t<300\t42\t67\twould_inflate\tadd\t67\twould_inflate\t30\tsmart_read_unchanged/unchanged\t2\t74\twould_inflate/no_base\ttrue",
    "current\tfixture\tweb/app.ts\ttypescript\t7\t<300\t34\t59\twould_inflate\tgreet\t59\twould_inflate\t30\tsmart_read_unchanged/unchanged\t2\t68\twould_inflate/no_base\ttrue",
];

#[test]
fn token_figures_for_the_fixture_are_exactly_these() {
    let tsv = &output().tsv;
    let tsv = tsv.as_ref().expect("the run measured tokens");
    let mut lines = tsv.lines();
    assert_eq!(
        lines.next(),
        Some(
            "build\tcorpus\tpath\tlanguage\tlines\tbucket\tfull\toutline\toutline_route\t\
             recall_item\trecall\trecall_route\tstub\tstub_route\tdelta_line\tdelta\t\
             delta_route\tfirst_matches_off"
        )
    );
    let rows: Vec<&str> = lines.collect();
    assert_eq!(rows, EXPECTED_ROWS);
}

#[test]
fn the_json_has_every_section_and_a_stable_key_order() {
    let out = output();
    let text = serde_json::to_string_pretty(&out.results).expect("serialise");
    let v: serde_json::Value = serde_json::from_str(&text).expect("well-formed JSON");

    let sections = [
        "schema",
        "label",
        "pre_release",
        "tokenizer",
        "corpus",
        "builds",
        "latency",
        "model",
        "environment",
    ];
    let mut last = 0;
    for key in sections {
        assert!(v.get(key).is_some(), "the JSON has no {key:?} section");
        let at = text
            .find(&format!("\n  \"{key}\""))
            .unwrap_or_else(|| panic!("{key} is not a top-level key"));
        assert!(at >= last, "{key} is out of the declared order");
        last = at;
    }
    assert_eq!(v["label"], "smoke");
    assert_eq!(v["pre_release"], true);
    assert_eq!(v["tokenizer"]["is_proxy_for_claude"], true);
    assert_eq!(v["model"]["status"], "modelled");
    assert!(
        v["environment"]["os"]
            .as_str()
            .is_some_and(|s| !s.is_empty())
    );

    let digest = v["builds"][0]["tokens"]["digest"]
        .as_str()
        .expect("a token digest");
    assert_eq!(digest.len(), 64);
    assert!(digest.bytes().all(|b| b.is_ascii_hexdigit()));
}

#[test]
fn the_latency_section_has_every_series_and_only_measured_or_stated_gaps() {
    let out = output();
    let v: serde_json::Value = serde_json::to_value(&out.results).expect("serialise");
    assert_eq!(v["latency"]["runs"], 3);
    assert_eq!(v["latency"]["minimum_runs"], 100);
    let series = v["latency"]["builds"][0]["series"]
        .as_array()
        .expect("a series list");
    let find = |id: &str| {
        series
            .iter()
            .find(|s| s["id"] == id)
            .unwrap_or_else(|| panic!("no series {id}"))
    };
    for id in [
        "hook.floor",
        "hook.intercept_pass.spawn",
        "hook.intercept_block.spawn",
        "hook.meter_read.spawn",
        "hook.meter_bash.spawn",
        "hook.intercept_pass.inproc",
        "hook.intercept_block.inproc",
        "hook.meter_read.inproc",
        "hook.meter_bash.inproc",
        "server.start",
        "server.ping",
        "server.smart_read.300-999",
        "server.recall_file.300-999",
        "server.repeat_unchanged.300-999",
        "server.repeat_changed.300-999",
    ] {
        let s = find(id);
        let status = s["status"].as_str().expect("a status");
        assert!(
            status.starts_with("measured"),
            "{id} was not measured: {status}"
        );
        assert_eq!(s["lat"]["n"], 3, "{id} has the wrong number of runs");
        for key in ["median_ns", "p90_ns", "p99_ns"] {
            assert!(s["lat"][key].as_i64().is_some_and(|n| n > 0), "{id} {key}");
        }
    }
    // The per-call identity slot: this workspace's build records no per-call identity (a
    // PreToolUse payload for a lumen tool, sent to `hook intercept`, leaves no record), so the
    // slot holds no figure and says that, in words, with the commit it was asked of.
    let slot = find("hook.per_call_identity");
    assert!(slot["lat"].is_null());
    assert!(
        slot["status"]
            .as_str()
            .is_some_and(|s| s.starts_with("n/a — this build records no per-call identity")),
        "the identity slot: {}",
        slot["status"]
    );
    // An Env build has no claim path to time.
    assert!(
        !series.iter().any(|s| s["id"] == "server.identity.placed"),
        "an env-identity build has no server.identity series"
    );
    // Every series that is not measured says why.
    for s in series {
        let status = s["status"].as_str().expect("a status");
        assert!(
            status.starts_with("measured") || status.starts_with("n/a"),
            "{}: {status}",
            s["id"]
        );
    }
}

#[test]
fn the_summary_labels_dollars_modelled_and_has_no_headline_percentage() {
    let s = &output().summary;
    assert!(s.contains("MODELLED"));
    assert!(s.contains("proxy for Claude's tokenizer"));
    assert!(s.contains("usage mix"));
    assert!(!s.contains("% saved") && !s.contains("% savings"));
}

#[test]
fn the_documents_name_no_local_path_and_keep_each_pass_median() {
    let out = output();
    let json = serde_json::to_string_pretty(&out.results).expect("serialise");
    let home = std::env::var("HOME").unwrap_or_default();
    let cwd = std::env::current_dir().unwrap_or_default();
    for (name, text) in [("json", &json), ("summary", &out.summary)] {
        if !home.is_empty() {
            assert!(!text.contains(&home), "the {name} names the home directory");
        }
        assert!(
            !text.contains(&*cwd.to_string_lossy()),
            "the {name} names the working directory"
        );
    }
    let v: serde_json::Value = serde_json::from_str(&json).expect("well-formed JSON");
    assert!(
        v["latency"]["percentile_method"]
            .as_str()
            .is_some_and(|s| s.contains("nearest-rank")),
        "the percentile method is disclosed"
    );
    let floor = v["latency"]["builds"][0]["series"]
        .as_array()
        .and_then(|s| s.iter().find(|s| s["id"] == "hook.floor"))
        .expect("the hook floor");
    let passes = v["latency"]["passes"].as_u64().expect("a pass count");
    assert_eq!(
        floor["pass_medians_ns"].as_array().map(Vec::len),
        Some(passes as usize),
        "one median per pass"
    );
}

#[test]
fn the_documents_are_written_under_the_label_and_the_os() {
    let dir = tempfile::tempdir().expect("a scratch directory");
    let written = run::write_outputs(dir.path(), output()).expect("write the outputs");
    let os = &output().results.environment.os;
    let names: Vec<String> = written
        .iter()
        .map(|p| {
            p.file_name()
                .expect("a file name")
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    assert_eq!(
        names,
        [
            format!("smoke-{os}.json"),
            "smoke-summary.md".to_string(),
            "smoke-tokens.tsv".to_string()
        ]
    );
    for p in written {
        assert!(std::fs::metadata(&p).expect("stat").len() > 0);
    }
}
