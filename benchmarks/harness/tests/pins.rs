//! The committed method files of the competitor and version benchmarks (P2).
//!
//! `benchmarks/competitors/{tools,tasks}.toml` and `benchmarks/versions/manifest.toml` are
//! committed before any run, so what they say cannot be chosen after seeing a result. This test
//! parses them and checks the things that would make a result unreproducible: an abbreviated
//! pin, a missing task, a version without an asset or an explicit `n/a`, a capability map that
//! skips a version. The checks are functions over a parsed value, and each has a fixture that
//! must fail, so a check that stopped checking would be caught.

use std::path::PathBuf;
use toml::Value;

fn bench_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")
}

/// A committed file as text with LF line ends, so the checks do not depend on the checkout's
/// line-ending setting (a Windows clone may have CRLF).
fn read_norm(rel: &str) -> String {
    let path = bench_dir().join(rel);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
        .replace("\r\n", "\n")
}

fn load(rel: &str) -> Value {
    let path = bench_dir().join(rel);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    parse(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn parse(text: &str) -> Result<Value, toml::de::Error> {
    toml::from_str::<Value>(text)
}

fn is_hex(s: &str, len: usize) -> bool {
    s.len() == len
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Keys that hold a git commit (40 hex) and keys that hold a sha256 (64 hex), at any depth.
const COMMIT_KEYS: &[&str] = &["commit", "base_commit", "gold_commit", "revision"];
const SHA256_KEYS: &[&str] = &["sha256", "parquet_sha256", "ids_sha256"];

/// Every pin in the value, at any depth, that is not a full lowercase hex value. Returns the
/// path of each offender; empty means every pin is full.
fn bad_pins(v: &Value) -> Vec<String> {
    fn walk(v: &Value, path: &str, out: &mut Vec<String>) {
        match v {
            Value::Table(t) => {
                for (k, x) in t {
                    let p = if path.is_empty() {
                        k.clone()
                    } else {
                        format!("{path}.{k}")
                    };
                    if let Some(s) = x.as_str() {
                        if COMMIT_KEYS.contains(&k.as_str()) && !is_hex(s, 40) {
                            out.push(format!("{p} = {s:?}: not a 40-character commit"));
                        }
                        if SHA256_KEYS.contains(&k.as_str()) && !is_hex(s, 64) {
                            out.push(format!("{p} = {s:?}: not a 64-hex sha256"));
                        }
                    }
                    walk(x, &p, out);
                }
            }
            Value::Array(a) => {
                for (i, x) in a.iter().enumerate() {
                    walk(x, &format!("{path}[{i}]"), out);
                }
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    walk(v, "", &mut out);
    out
}

fn str_field<'a>(v: &'a Value, k: &str) -> Option<&'a str> {
    v.get(k)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
}

/// tasks.toml: 10 distinct SWE-bench instances, 3 scenarios, each with what a run needs.
fn task_problems(v: &Value) -> Vec<String> {
    let mut out = Vec::new();
    let swe = v
        .get("swe")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if swe.len() != 10 {
        out.push(format!("{} SWE-bench instances, expected 10", swe.len()));
    }
    let mut ids: Vec<&str> = swe.iter().filter_map(|s| str_field(s, "id")).collect();
    ids.sort_unstable();
    ids.dedup();
    if ids.len() != swe.len() {
        out.push("an instance has no id or an id is repeated".into());
    }
    for s in &swe {
        for k in ["id", "repo", "base_commit"] {
            if str_field(s, k).is_none() {
                out.push(format!("a swe entry lacks {k}"));
            }
        }
    }
    let sb = v.get("swe_bench");
    for k in ["dataset", "revision", "parquet_sha256", "seed"] {
        if sb.and_then(|t| str_field(t, k)).is_none() {
            out.push(format!("swe_bench.{k} is missing"));
        }
    }
    if sb.and_then(|t| t.get("count")).and_then(Value::as_integer) != Some(swe.len() as i64) {
        out.push("swe_bench.count does not match the number of instances".into());
    }
    let sc = v
        .get("scenario")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if sc.len() != 3 {
        out.push(format!("{} scenarios, expected 3", sc.len()));
    }
    let mut want = vec![
        "large-file-exploration",
        "log-heavy-debugging",
        "multi-file-refactor",
    ];
    for s in &sc {
        for k in [
            "id",
            "repo",
            "license",
            "base_commit",
            "gold_commit",
            "prompt",
            "success",
        ] {
            if str_field(s, k).is_none() {
                out.push(format!("a scenario lacks {k}"));
            }
        }
        if let Some(id) = str_field(s, "id") {
            want.retain(|w| *w != id);
        }
    }
    if !want.is_empty() {
        out.push(format!("scenarios missing: {want:?}"));
    }
    out
}

const OSES: [&str; 3] = ["macos-arm64", "linux-x86_64", "windows-x64"];
const VERSIONS: [&str; 10] = [
    "1.0.0", "1.1.5", "1.2.4", "1.3.1", "1.4.0", "1.5.1", "1.6.0", "1.7.0", "1.8.0", "1.9.0",
];

/// manifest.toml: every version listed, an asset or an explained `n/a` per OS, a full
/// capability map for each.
fn manifest_problems(v: &Value) -> Vec<String> {
    let mut out = Vec::new();
    let listed: Vec<&str> = v
        .get("meta")
        .and_then(|m| m.get("versions"))
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    if listed != VERSIONS {
        out.push(format!(
            "meta.versions is {listed:?}, expected {VERSIONS:?}"
        ));
    }
    let table = v.get("version").and_then(Value::as_table);
    for ver in VERSIONS {
        let Some(e) = table.and_then(|t| t.get(ver)) else {
            out.push(format!("{ver}: no [version] entry"));
            continue;
        };
        for k in ["tag", "commit"] {
            if str_field(e, k).is_none() {
                out.push(format!("{ver}: {k} missing"));
            }
        }
        for os in OSES {
            match e.get("asset").and_then(|a| a.get(os)) {
                None => out.push(format!("{ver}: no asset entry for {os}")),
                Some(a) => {
                    let has_asset =
                        str_field(a, "name").is_some() && str_field(a, "sha256").is_some();
                    let na = str_field(a, "na");
                    if has_asset == na.is_some() {
                        out.push(format!("{ver}/{os}: needs a name and sha256, or an na reason, not both or neither"));
                    }
                    if let Some(reason) = na
                        && reason.len() < 20
                    {
                        out.push(format!(
                            "{ver}/{os}: the na reason is too short to be a reason"
                        ));
                    }
                }
            }
        }
        let enum_ok = |k: &str, allowed: &[&str]| {
            e.get(k)
                .and_then(Value::as_str)
                .is_some_and(|s| allowed.contains(&s))
        };
        if !enum_ok("saved_tokens", &["clamped", "signed"]) {
            out.push(format!("{ver}: saved_tokens must be clamped or signed"));
        }
        if !enum_ok(
            "hook_kind",
            &["scripts", "scripts+cli", "scripts+cli+identity"],
        ) {
            out.push(format!("{ver}: hook_kind missing or unknown"));
        }
        if !enum_ok("repeat_reads", &["none", "opt-in", "default"]) {
            out.push(format!("{ver}: repeat_reads missing or unknown"));
        }
        if !enum_ok("identity", &["none", "env", "hook"]) {
            out.push(format!("{ver}: identity missing or unknown"));
        }
        for k in [
            "hook_cli",
            "bash_meter",
            "daemon_fixed_port",
            "daemon_reads_home",
            "e2e",
        ] {
            if e.get(k).and_then(Value::as_bool).is_none() {
                out.push(format!("{ver}: {k} missing"));
            }
        }
        let env_ok = e
            .get("daemon_env")
            .and_then(Value::as_array)
            .is_some_and(|a| a.iter().any(|x| x.as_str() == Some("LUMEN_DB")));
        if !env_ok {
            out.push(format!("{ver}: daemon_env must list LUMEN_DB"));
        }
    }
    out
}

/// tools.toml: every arm of the comparison, with what it takes to install it.
fn tool_problems(v: &Value) -> Vec<String> {
    let mut out = Vec::new();
    let arms = v
        .get("arm")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let find = |id: &str| arms.iter().find(|a| str_field(a, "id") == Some(id));
    for id in [
        "none",
        "lumen-1.9.0",
        "squeez",
        "rtk",
        "headroom",
        "caveman",
    ] {
        if find(id).is_none() {
            out.push(format!("arm {id} is missing"));
        }
    }
    for a in &arms {
        let id = str_field(a, "id").unwrap_or("?");
        if str_field(a, "kind") == Some("none") {
            continue;
        }
        for k in ["repo", "license", "version", "tag", "commit"] {
            if str_field(a, k).is_none() {
                out.push(format!("{id}: {k} missing"));
            }
        }
        if a.get("install")
            .and_then(Value::as_array)
            .is_none_or(|i| i.is_empty())
        {
            out.push(format!("{id}: no install steps"));
        }
        let has_sha = a
            .get("asset")
            .and_then(Value::as_array)
            .is_some_and(|x| x.iter().any(|y| str_field(y, "sha256").is_some()))
            || a.get("model").is_some()
            || a.get("npm").is_some();
        if !has_sha {
            out.push(format!(
                "{id}: nothing downloaded is pinned by sha256 or integrity"
            ));
        }
        if str_field(a, "kind") == Some("competitor")
            && a.get("telemetry_env")
                .and_then(Value::as_table)
                .is_none_or(|t| t.is_empty())
        {
            out.push(format!("{id}: a competitor needs its telemetry-off env"));
        }
    }
    out
}

/// The version report's E2E arms are the same builds the manifest lists for Linux.
fn cross_problems(tools: &Value, manifest: &Value) -> Vec<String> {
    let mut out = Vec::new();
    let arms = tools
        .get("arm")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let e2e: Vec<String> = manifest
        .get("meta")
        .and_then(|m| m.get("e2e"))
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    if e2e != ["1.1.5", "1.6.0", "1.8.0", "1.9.0"] {
        out.push(format!("meta.e2e is {e2e:?}"));
    }
    for ver in &e2e {
        let id = format!("lumen-{ver}");
        let arm = arms
            .iter()
            .find(|a| str_field(a, "id") == Some(id.as_str()));
        let m = manifest.get("version").and_then(|t| t.get(ver.as_str()));
        let (Some(arm), Some(m)) = (arm, m) else {
            out.push(format!(
                "{ver}: in meta.e2e but no tools.toml arm or manifest entry"
            ));
            continue;
        };
        let linux = m.get("asset").and_then(|a| a.get("linux-x86_64"));
        let from_arm = arm
            .get("asset")
            .and_then(Value::as_array)
            .and_then(|a| a.first())
            .and_then(|a| str_field(a, "sha256"));
        if from_arm.is_none() || from_arm != linux.and_then(|l| str_field(l, "sha256")) {
            out.push(format!(
                "{ver}: tools.toml and manifest.toml disagree on the Linux asset"
            ));
        }
        if str_field(arm, "commit") != str_field(m, "commit") {
            out.push(format!(
                "{ver}: tools.toml and manifest.toml disagree on the commit"
            ));
        }
    }
    out
}

/// draw_tasks.py repeats the pins that define the draw; they must be tasks.toml's.
fn draw_script_problems(tasks: &Value, script: &str) -> Vec<String> {
    let mut out = Vec::new();
    let sb = tasks.get("swe_bench");
    for (name, key) in [
        ("REVISION", "revision"),
        ("PARQUET_SHA256", "parquet_sha256"),
        ("SEED", "seed"),
        ("DATASET", "dataset"),
    ] {
        let want = sb.and_then(|t| str_field(t, key)).unwrap_or("");
        let have = script
            .lines()
            .find_map(|l| l.strip_prefix(&format!("{name} = ")))
            .map(|r| r.trim().trim_matches('"').to_string());
        if have.as_deref() != Some(want) {
            out.push(format!(
                "draw_tasks.py {name} is {have:?}, tasks.toml {key} is {want:?}"
            ));
        }
    }
    out
}

#[track_caller]
fn assert_clean(what: &str, problems: Vec<String>) {
    assert!(problems.is_empty(), "{what}:\n  {}", problems.join("\n  "));
}

#[test]
fn every_pin_in_every_file_is_a_full_hash() {
    for rel in [
        "competitors/tools.toml",
        "competitors/tasks.toml",
        "versions/manifest.toml",
    ] {
        assert_clean(rel, bad_pins(&load(rel)));
    }
}

#[test]
fn tasks_are_ten_instances_and_three_scenarios() {
    assert_clean("tasks.toml", task_problems(&load("competitors/tasks.toml")));
}

#[test]
fn manifest_has_every_version_per_os_and_a_capability_map() {
    assert_clean(
        "manifest.toml",
        manifest_problems(&load("versions/manifest.toml")),
    );
}

#[test]
fn only_1_0_0_has_a_missing_asset_and_it_is_linux() {
    let m = load("versions/manifest.toml");
    let mut na = Vec::new();
    for ver in VERSIONS {
        for os in OSES {
            let a = &m["version"][ver]["asset"][os];
            if a.get("na").is_some() {
                na.push(format!("{ver}/{os}"));
            }
        }
    }
    assert_eq!(na, ["1.0.0/linux-x86_64"]);
}

#[test]
fn tools_cover_every_arm() {
    assert_clean("tools.toml", tool_problems(&load("competitors/tools.toml")));
}

#[test]
fn tools_and_manifest_agree_on_the_e2e_builds() {
    assert_clean(
        "tools.toml vs manifest.toml",
        cross_problems(
            &load("competitors/tools.toml"),
            &load("versions/manifest.toml"),
        ),
    );
}

#[test]
fn draw_script_carries_the_pins_of_tasks_toml() {
    let script = read_norm("competitors/draw_tasks.py");
    assert_clean(
        "draw_tasks.py",
        draw_script_problems(
            &load("competitors/tasks.toml"),
            &script.replace("\r\n", "\n"),
        ),
    );
}

#[test]
fn method_files_exist_and_name_their_rules() {
    let read = |rel: &str| {
        std::fs::read_to_string(bench_dir().join(rel))
            .unwrap()
            .replace("\r\n", "\n")
    };
    let c = read("competitors/METHOD.md");
    for needle in [
        "cheapest median dollars",
        "overlap",
        "opens an issue",
        "$3",
        "1.1.5 stands in for 1.0.0",
        "message.id",
        "claude-sonnet-5-5",
        "2.1.292",
        "Deviation from defaults: telemetry",
    ] {
        assert!(c.contains(needle), "competitors/METHOD.md lacks {needle:?}");
    }
    let v = read("versions/METHOD.md");
    for needle in [
        "never a rebuild",
        "today's engine",
        "published as worse and opens",
        "no Lumen",
        "n/a rules",
        "1.1.5",
    ] {
        assert!(v.contains(needle), "versions/METHOD.md lacks {needle:?}");
    }
}

// --- The checks must fail on bad input: a check that cannot fail checks nothing. ----------------

#[test]
fn a_short_commit_is_refused() {
    let v = parse("[[arm]]\ncommit = \"fa10c0dde7a8\"\n").unwrap();
    let bad = bad_pins(&v);
    assert_eq!(bad.len(), 1, "{bad:?}");
    assert!(bad[0].contains("40-character"));
}

#[test]
fn a_short_or_uppercase_sha256_is_refused() {
    let short = parse("[[arm.asset]]\nsha256 = \"76133cff59b7\"\n").unwrap();
    assert_eq!(bad_pins(&short).len(), 1);
    let upper = format!("[a]\nsha256 = \"{}\"\n", "A".repeat(64));
    assert_eq!(bad_pins(&parse(&upper).unwrap()).len(), 1);
    let good = format!(
        "[a]\nsha256 = \"{}\"\ncommit = \"{}\"\n",
        "a".repeat(64),
        "0".repeat(40)
    );
    assert!(bad_pins(&parse(&good).unwrap()).is_empty());
}

#[test]
fn a_short_pin_in_the_real_files_is_found() {
    // The committed tools.toml with one pin cut short: the same check the real file passes.
    let text = read_norm("competitors/tools.toml");
    let full = "fa10c0dde7a81bdc5bea41dbaf5fa9c975624872";
    assert!(text.contains(full), "fixture pin not found in tools.toml");
    let mutant = text.replacen(full, &full[..12], 1);
    assert_eq!(bad_pins(&parse(&mutant).unwrap()).len(), 1);
}

#[test]
fn nine_instances_or_two_scenarios_is_refused() {
    let text = read_norm("competitors/tasks.toml");
    let swe_at = text.find("[[swe]]").unwrap();
    let second = text[swe_at + 1..].find("[[swe]]").unwrap() + swe_at + 1;
    let nine = format!("{}{}", &text[..swe_at], &text[second..]);
    assert!(
        task_problems(&parse(&nine).unwrap())
            .iter()
            .any(|p| p.contains("9 SWE-bench"))
    );
    let sc_at = text.find("[[scenario]]").unwrap();
    let two = &text[..text[sc_at + 1..].rfind("[[scenario]]").unwrap() + sc_at + 1];
    assert!(
        task_problems(&parse(two).unwrap())
            .iter()
            .any(|p| p.contains("2 scenarios"))
    );
}

#[test]
fn a_version_without_an_asset_or_an_na_reason_is_refused() {
    let text = read_norm("versions/manifest.toml");
    // 1.0.0's Linux entry is an na with a reason; strip the reason and it must fail.
    let reason_at = text.find("na = \"1.0.0 shipped no Linux").unwrap();
    let end = text[reason_at..].find('\n').unwrap() + reason_at;
    let no_reason = format!("{}na = \"\"{}", &text[..reason_at], &text[end..]);
    let p = manifest_problems(&parse(&no_reason).unwrap());
    assert!(p.iter().any(|x| x.contains("1.0.0/linux-x86_64")), "{p:?}");
    // And a whole version missing from the capability map.
    let at = text.find("[version.\"1.7.0\"]\n").unwrap();
    let next = text[at + 1..].find("[version.\"1.8.0\"]\n").unwrap() + at + 1;
    let dropped = format!("{}{}", &text[..at], &text[next..]);
    let p = manifest_problems(&parse(&dropped).unwrap());
    assert!(
        p.iter().any(|x| x.contains("1.7.0: no [version] entry")),
        "{p:?}"
    );
}

#[test]
fn a_capability_map_with_an_unknown_value_is_refused() {
    let text = read_norm("versions/manifest.toml");
    let mutant = text.replacen("saved_tokens = \"clamped\"", "saved_tokens = \"maybe\"", 1);
    let p = manifest_problems(&parse(&mutant).unwrap());
    assert!(p.iter().any(|x| x.contains("1.0.0: saved_tokens")), "{p:?}");
}

#[test]
fn a_competitor_without_telemetry_off_is_refused() {
    let text = read_norm("competitors/tools.toml");
    let mutant = text.replacen("telemetry_env = { HEADROOM_BEACON = \"off\", DO_NOT_TRACK = \"1\", HF_HUB_OFFLINE = \"1\" }", "", 1);
    let p = tool_problems(&parse(&mutant).unwrap());
    assert!(
        p.iter()
            .any(|x| x.contains("headroom") && x.contains("telemetry")),
        "{p:?}"
    );
}

#[test]
fn disagreeing_e2e_pins_are_refused() {
    let tools = read_norm("competitors/tools.toml");
    let manifest = read_norm("versions/manifest.toml");
    let sha = "d400a0c7832207d36a8f3984004ddce967be6cc03fe5dca27210d9dd55dc3bfe";
    assert!(tools.contains(sha) && manifest.contains(sha));
    let mutant = tools.replacen(sha, &"0".repeat(64), 1);
    let p = cross_problems(&parse(&mutant).unwrap(), &parse(&manifest).unwrap());
    assert!(
        p.iter()
            .any(|x| x.contains("1.1.5") && x.contains("disagree")),
        "{p:?}"
    );
}

#[test]
fn a_draw_script_with_another_seed_is_refused() {
    let tasks = load("competitors/tasks.toml");
    let script = read_norm("competitors/draw_tasks.py");
    let mutant = script.replace("SEED = \"lumen-p2-2026-10-18\"", "SEED = \"another\"");
    assert_ne!(script, mutant);
    assert!(!draw_script_problems(&tasks, &mutant).is_empty());
}

/// The ten drawn ids, in rank order, hashed: tasks.toml commits the digest, so swapping one id
/// (or reordering) is caught even though every id is still a well-formed string.
fn ids_digest_problems(v: &Value) -> Vec<String> {
    use sha2::{Digest, Sha256};
    let ids: Vec<&str> = v
        .get("swe")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|s| str_field(s, "id")).collect())
        .unwrap_or_default();
    let have: String = Sha256::digest(ids.join("\n").as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let want = v
        .get("swe_bench")
        .and_then(|t| str_field(t, "ids_sha256"))
        .unwrap_or("");
    if have == want {
        vec![]
    } else {
        vec![format!("ids hash to {have}, tasks.toml says {want:?}")]
    }
}

#[test]
fn the_ten_ids_match_their_committed_digest() {
    assert_clean(
        "tasks.toml ids",
        ids_digest_problems(&load("competitors/tasks.toml")),
    );
}

#[test]
fn a_swapped_or_reordered_instance_id_is_refused() {
    let text = read_norm("competitors/tasks.toml");
    let swapped = text.replacen("django__django-14915", "django__django-14916", 1);
    assert_ne!(text, swapped);
    assert_eq!(ids_digest_problems(&parse(&swapped).unwrap()).len(), 1);
    let a = text.replacen("django__django-14915", "TMP", 1).replacen(
        "django__django-16801",
        "django__django-14915",
        1,
    );
    let reordered = a.replacen("TMP", "django__django-16801", 1);
    assert_eq!(ids_digest_problems(&parse(&reordered).unwrap()).len(), 1);
}

/// The honesty rules each METHOD.md must carry, as (what, phrase) pairs.
fn honesty_problems(file: &str, text: &str, phrases: &[(&str, &str)]) -> Vec<String> {
    let mut out = Vec::new();
    if !text.contains("\n## Honesty rule\n") {
        out.push(format!("{file}: no \"Honesty rule\" section"));
    }
    let section = text
        .split("\n## Honesty rule\n")
        .nth(1)
        .and_then(|r| r.split("\n## ").next());
    for (what, phrase) in phrases {
        if !section.is_some_and(|s| {
            s.split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .contains(phrase)
        }) {
            out.push(format!(
                "{file}: the honesty section lacks {what} ({phrase:?})"
            ));
        }
    }
    out
}

const COMPETITOR_RULES: &[(&str, &str)] = &[
    (
        "the task list fixed before any run",
        "committed first. They are not changed after a result exists",
    ),
    ("every result published", "Every result is published"),
    (
        "an issue per Lumen loss",
        "Every row Lumen loses opens an issue",
    ),
    ("no win when success drops", "does not win the row"),
];
const VERSION_RULES: &[(&str, &str)] = &[
    ("the metrics fixed before any run", "fixed before a run"),
    ("every result published", "Every result is published"),
    (
        "an issue per worse version",
        "published as worse and opens an issue",
    ),
    ("no win when success drops", "does not win the row"),
];

#[test]
fn both_method_files_carry_the_honesty_rules() {
    let c = read_norm("competitors/METHOD.md");
    assert_clean(
        "competitors/METHOD.md",
        honesty_problems("competitors", &c, COMPETITOR_RULES),
    );
    let v = read_norm("versions/METHOD.md");
    assert_clean(
        "versions/METHOD.md",
        honesty_problems("versions", &v, VERSION_RULES),
    );
}

#[test]
fn a_method_file_without_its_honesty_rules_is_refused() {
    let c = read_norm("competitors/METHOD.md");
    let no_heading = c.replacen("## Honesty rule", "## Notes", 1);
    assert!(!honesty_problems("c", &no_heading, COMPETITOR_RULES).is_empty());
    let no_issue = c.replacen("opens an issue", "mentions a ticket", 1);
    assert!(!honesty_problems("c", &no_issue, COMPETITOR_RULES).is_empty());
    let v = read_norm("versions/METHOD.md");
    let no_all = v.replacen("Every result is published", "Results are kept", 1);
    assert!(!honesty_problems("v", &no_all, VERSION_RULES).is_empty());
}

#[test]
fn no_file_names_the_legacy_app_identifier_as_a_literal() {
    // The legacy identifier lives only in lumen-core's legacy module; the method files say
    // "the legacy data directory". The forbidden word is assembled so this file does not hold it.
    let word = ["speed", "ata"].concat();
    for rel in [
        "competitors/METHOD.md",
        "competitors/tools.toml",
        "competitors/tasks.toml",
        "competitors/draw_tasks.py",
        "versions/METHOD.md",
        "versions/manifest.toml",
        "harness/tests/pins.rs",
    ] {
        assert!(
            !read_norm(rel).to_lowercase().contains(&word),
            "{rel} names the legacy identifier"
        );
    }
}

#[test]
fn daemon_env_lists_supervised_from_1_2_4() {
    let m = load("versions/manifest.toml");
    for ver in VERSIONS {
        let has = m["version"][ver]["daemon_env"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x.as_str() == Some("LUMEN_SUPERVISED"));
        let from = !["1.0.0", "1.1.5"].contains(&ver);
        assert_eq!(has, from, "{ver}");
    }
}
