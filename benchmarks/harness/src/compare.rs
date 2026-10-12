//! `lumen-bench compare <committed.json>`: how a fresh latency run sits against a committed one.
//!
//! Latency does not travel between machines, or between two hours on one machine. This module says
//! by how much, instead of leaving the reader to guess:
//!
//! * per series, the ratio of the median now to the committed median;
//! * the same ratio after dividing each median by its own run's floor (the process floor for the
//!   hooks and `server.start`, the JSON-RPC round trip for the other server series), which takes out
//!   most of what a faster or busier machine does to everything alike;
//! * how many series are within 20% of the committed value, both ways (ratio 0.80 to 1.20);
//! * whether the machine looks like the one the committed run was made on: `hook.floor` and
//!   `server.ping` each within 15% of the committed run's, in every build.
//!
//! Series are matched by build role (baseline, candidate) and by id, so a candidate built from a
//! different commit is still compared with the committed candidate, and the output says both
//! commits.

use crate::builds::Build;
use crate::latency::{BuildLatency, Series};
use crate::util::{Ctx, Res};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fmt::Write;

/// A series is "within" when its ratio to the committed value is in `1 - WITHIN ..= 1 + WITHIN`.
pub const WITHIN: f64 = 0.20;
/// The machine matches when both floors' ratios are in `1 - MACHINE ..= 1 + MACHINE`.
pub const MACHINE: f64 = 0.15;

const HOOK_FLOOR: &str = "hook.floor";
const PING: &str = "server.ping";

/// The series a given series is divided by, when there is one of its own kind. The floors
/// themselves have none, and neither have the in-process hooks (no process start, so a process
/// floor says nothing about them).
pub fn floor_for(id: &str) -> Option<&'static str> {
    if id == HOOK_FLOOR || id == PING || id.ends_with(".inproc") {
        None
    } else if id.starts_with("hook.") || id == "server.start" {
        Some(HOOK_FLOOR)
    } else if id.starts_with("server.") {
        Some(PING)
    } else {
        None
    }
}

/// One build of a committed run.
#[derive(Debug, Clone)]
pub struct CommittedBuild {
    pub role: String,
    pub label: String,
    pub sha: String,
    /// `(series id, median ns)` for every series that has a figure, in file order.
    pub series: Vec<(String, i64)>,
    pub load_before: String,
    pub load_after: String,
}

#[derive(Debug, Clone)]
pub struct Committed {
    pub label: String,
    pub os: String,
    pub runs: usize,
    pub warmup: usize,
    pub subjects: Vec<String>,
    pub corpus_sha: Option<String>,
    pub builds: Vec<CommittedBuild>,
}

fn text(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Read what a comparison needs from a committed results file. The file is read as plain JSON, so
/// a file from an earlier version of the harness (fewer fields) still compares.
pub fn parse_committed(json: &str) -> Res<Committed> {
    let v: Value = serde_json::from_str(json).ctx("parse the committed results")?;
    let lat = v
        .get("latency")
        .filter(|l| !l.is_null())
        .ok_or("the committed results have no latency section to compare with")?;
    let info = v.get("builds").and_then(Value::as_array).ok_or("no builds")?;
    let role_of = |label: &str| -> Option<(String, String)> {
        info.iter()
            .find(|b| b.get("label").and_then(Value::as_str) == Some(label))
            .map(|b| (text(b, "role"), text(b, "sha")))
    };
    let mut builds = Vec::new();
    for b in lat
        .get("builds")
        .and_then(Value::as_array)
        .ok_or("no latency builds")?
    {
        let label = text(b, "label");
        let (role, sha) = role_of(&label)
            .ok_or_else(|| format!("latency build {label} is not in the builds list"))?;
        let mut series = Vec::new();
        for s in b.get("series").and_then(Value::as_array).unwrap_or(&vec![]) {
            if let Some(m) = s
                .get("lat")
                .and_then(|l| l.get("median_ns"))
                .and_then(Value::as_i64)
            {
                series.push((text(s, "id"), m));
            }
        }
        builds.push(CommittedBuild {
            role,
            label,
            sha,
            series,
            load_before: text(b, "load_before"),
            load_after: text(b, "load_after"),
        });
    }
    Ok(Committed {
        label: text(&v, "label"),
        os: v
            .get("environment")
            .map(|e| text(e, "os"))
            .unwrap_or_default(),
        runs: lat.get("runs").and_then(Value::as_u64).unwrap_or(0) as usize,
        warmup: lat.get("warmup_runs").and_then(Value::as_u64).unwrap_or(0) as usize,
        subjects: lat
            .get("subjects")
            .and_then(Value::as_array)
            .map(|a| a.iter().map(|s| text(s, "rel")).collect())
            .unwrap_or_default(),
        corpus_sha: v
            .get("corpus")
            .and_then(Value::as_array)
            .and_then(|a| a.first())
            .map(|c| text(c, "sha")),
        builds,
    })
}

/// One series, now against committed.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub id: String,
    pub committed_ns: i64,
    pub now_ns: i64,
    /// `now / committed`.
    pub ratio: f64,
    /// `(now / now floor) / (committed / committed floor)`, for a series with a floor.
    pub normalized: Option<f64>,
}

pub fn is_within(ratio: f64) -> bool {
    ((1.0 - WITHIN)..=(1.0 + WITHIN)).contains(&ratio)
}

fn is_machine(ratio: f64) -> bool {
    ((1.0 - MACHINE)..=(1.0 + MACHINE)).contains(&ratio)
}

fn get<'a>(list: &'a [(String, i64)], id: &str) -> Option<&'a i64> {
    list.iter().find(|(i, _)| i == id).map(|(_, m)| m)
}

/// Compare one build's series. A series that has a figure on one side only is returned in the
/// second list instead: there is nothing to divide.
pub fn rows(committed: &[(String, i64)], now: &[(String, i64)]) -> (Vec<Row>, Vec<String>) {
    let mut out = Vec::new();
    let mut missing = Vec::new();
    for (id, c) in committed {
        let Some(&n) = get(now, id) else {
            missing.push(format!("{id} (no figure now)"));
            continue;
        };
        let normalized = floor_for(id).and_then(|f| {
            let (cf, nf) = (*get(committed, f)?, *get(now, f)?);
            (cf > 0 && nf > 0 && *c > 0)
                .then(|| (n as f64 / nf as f64) / (*c as f64 / cf as f64))
        });
        out.push(Row {
            id: id.clone(),
            committed_ns: *c,
            now_ns: n,
            ratio: if *c > 0 { n as f64 / *c as f64 } else { f64::NAN },
            normalized,
        });
    }
    for (id, _) in now {
        if get(committed, id).is_none() {
            missing.push(format!("{id} (no figure in the committed run)"));
        }
    }
    (out, missing)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tally {
    pub raw_within: usize,
    pub raw_total: usize,
    pub normalized_within: usize,
    pub normalized_total: usize,
}

pub fn tally<'a>(rows: impl IntoIterator<Item = &'a Row>) -> Tally {
    let mut t = Tally {
        raw_within: 0,
        raw_total: 0,
        normalized_within: 0,
        normalized_total: 0,
    };
    for r in rows {
        t.raw_total += 1;
        t.raw_within += usize::from(is_within(r.ratio));
        if let Some(n) = r.normalized {
            t.normalized_total += 1;
            t.normalized_within += usize::from(is_within(n));
        }
    }
    t
}

/// Whether the machine looks like the committed run's, from the two floors of every build.
#[derive(Debug, Clone, PartialEq)]
pub struct Machine {
    /// `(build role, series id, ratio now / committed)` for each floor of each build.
    pub floors: Vec<(String, &'static str, Option<f64>)>,
    /// `Some(true)` when every floor is within 15%, `Some(false)` when one is not, `None` when a
    /// floor is missing on one side and the question cannot be answered.
    pub matches: Option<bool>,
}

pub fn machine(builds: &[(String, Vec<Row>)]) -> Machine {
    let mut floors = Vec::new();
    for (role, rows) in builds {
        for id in [HOOK_FLOOR, PING] {
            let r = rows.iter().find(|r| r.id == id).map(|r| r.ratio);
            floors.push((role.clone(), id, r));
        }
    }
    let matches = if floors.is_empty() || floors.iter().any(|(_, _, r)| r.is_none()) {
        None
    } else {
        Some(floors.iter().all(|(_, _, r)| r.is_some_and(is_machine)))
    };
    Machine { floors, matches }
}

fn ms(ns: i64) -> String {
    format!("{:.3} ms", ns as f64 / 1e6)
}

/// What one side of a comparison says about itself.
pub struct Side {
    pub role: String,
    pub committed_label: String,
    pub committed_sha: String,
    pub now_label: String,
    pub now_sha: String,
    pub committed_load: String,
    pub now_load: String,
    pub rows: Vec<Row>,
    pub missing: Vec<String>,
}

pub fn render(
    committed: &Committed,
    now_corpus_sha: &str,
    now_subjects: &[String],
    sides: &[Side],
) -> String {
    let mut s = String::new();
    let _ = writeln!(
        s,
        "compare: the committed run `{}` ({}, {} runs per series, {} warm-up per pass) against this run\n",
        committed.label, committed.os, committed.runs, committed.warmup
    );
    if let Some(then) = &committed.corpus_sha
        && then != now_corpus_sha
    {
        let _ = writeln!(
            s,
            "WARNING: the corpus differs (committed {then}, now {now_corpus_sha}); the series are \
             not like for like.\n"
        );
    }
    if !committed.subjects.is_empty() && committed.subjects != now_subjects {
        let _ = writeln!(
            s,
            "WARNING: the latency subjects differ (committed {:?}, now {:?}); the series are not \
             like for like.\n",
            committed.subjects, now_subjects
        );
    }
    let mut by_build: Vec<(String, Vec<Row>)> = Vec::new();
    for side in sides {
        let _ = writeln!(
            s,
            "== {}: committed {} @ {}, now {} @ {}\n   load average, committed run: {}; this run: {}",
            side.role,
            side.committed_label,
            side.committed_sha.chars().take(12).collect::<String>(),
            side.now_label,
            side.now_sha.chars().take(12).collect::<String>(),
            side.committed_load,
            side.now_load
        );
        let _ = writeln!(
            s,
            "{:<42} {:>12} {:>12} {:>7} {:>11}",
            "series", "committed", "now", "ratio", "vs floor"
        );
        for r in &side.rows {
            let norm = r.normalized.map_or("-".to_string(), |n| format!("{n:.2}"));
            let flag = |x: f64| if is_within(x) { ' ' } else { '*' };
            let _ = writeln!(
                s,
                "{:<42} {:>12} {:>12} {:>6.2}{} {:>10}{}",
                r.id,
                ms(r.committed_ns),
                ms(r.now_ns),
                r.ratio,
                flag(r.ratio),
                norm,
                r.normalized.map_or(' ', flag)
            );
        }
        for m in &side.missing {
            let _ = writeln!(s, "not compared: {m}");
        }
        let t = tally(&side.rows);
        let _ = writeln!(
            s,
            "   within 20% of the committed value: {} of {} series as measured, {} of {} after \
             dividing by the floor\n",
            t.raw_within, t.raw_total, t.normalized_within, t.normalized_total
        );
        by_build.push((side.role.clone(), side.rows.clone()));
    }
    let all: Vec<&Row> = by_build.iter().flat_map(|(_, r)| r).collect();
    let t = tally(all);
    let _ = writeln!(
        s,
        "all builds: within 20% (ratio {:.2} to {:.2}, `*` marks a series outside): {} of {} series \
         as measured, {} of {} after dividing by the floor (`hook.floor` for the hooks and \
         `server.start`, `server.ping` for the other server series; the floors and the in-process \
         hooks have no floor of their own kind and are counted as measured only)",
        1.0 - WITHIN,
        1.0 + WITHIN,
        t.raw_within,
        t.raw_total,
        t.normalized_within,
        t.normalized_total
    );
    let m = machine(&by_build);
    for (role, id, r) in &m.floors {
        let _ = writeln!(
            s,
            "machine check, {role} {id}: {}",
            r.map_or("no figure".to_string(), |r| format!("ratio {r:.2}"))
        );
    }
    let verdict = match m.matches {
        Some(true) => format!(
            "THE MACHINE MATCHES: `{HOOK_FLOOR}` and `{PING}` are within {:.0}% of the committed \
             run in every build, so the raw ratios above are meaningful.",
            MACHINE * 100.0
        ),
        Some(false) => format!(
            "THE MACHINE DOES NOT MATCH: `{HOOK_FLOOR}` or `{PING}` is more than {:.0}% from the \
             committed run in some build. Read the ratios after dividing by the floor, not the raw \
             ones, and do not take a difference in a raw median as a difference in the code.",
            MACHINE * 100.0
        ),
        None => "THE MACHINE CANNOT BE JUDGED: a floor series has no figure on one side.".to_string(),
    };
    let _ = writeln!(s, "{verdict}");
    s
}

/// `(series id, median ns)` of a fresh run, from its `Series` list.
pub fn medians(series: &[Series]) -> Vec<(String, i64)> {
    series
        .iter()
        .filter_map(|s| s.lat.as_ref().map(|l| (s.id.clone(), l.median_ns)))
        .collect()
}

/// The role-to-committed-build map, for the caller's lookups.
pub fn by_role(c: &Committed) -> BTreeMap<&str, &CommittedBuild> {
    c.builds.iter().map(|b| (b.role.as_str(), b)).collect()
}

/// Pair each build just measured with the committed build of the same role.
pub fn sides(committed: &Committed, builds: &[Build], now: &[BuildLatency]) -> Res<Vec<Side>> {
    let roles = by_role(committed);
    let mut out = Vec::new();
    for b in builds {
        let then = roles.get(b.role).ok_or_else(|| {
            format!(
                "the committed run `{}` has no {} build to compare {} with",
                committed.label, b.role, b.label
            )
        })?;
        let lat = now
            .iter()
            .find(|l| l.label == b.label)
            .ok_or_else(|| format!("no latency figures for {}", b.label))?;
        let (rows, missing) = rows(&then.series, &medians(&lat.series));
        out.push(Side {
            role: b.role.to_string(),
            committed_label: then.label.clone(),
            committed_sha: then.sha.clone(),
            now_label: b.label.clone(),
            now_sha: b.sha.clone(),
            committed_load: format!("{} before, {} after", then.load_before, then.load_after),
            now_load: format!("{} before, {} after", lat.load_before, lat.load_after),
            rows,
            missing,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(list: &[(&str, i64)]) -> Vec<(String, i64)> {
        list.iter().map(|(i, m)| (i.to_string(), *m)).collect()
    }

    #[test]
    fn a_series_is_divided_by_the_floor_of_its_own_kind() {
        assert_eq!(floor_for("hook.meter_read.spawn"), Some("hook.floor"));
        assert_eq!(floor_for("hook.per_call_identity"), Some("hook.floor"));
        assert_eq!(floor_for("server.start"), Some("hook.floor"));
        assert_eq!(floor_for("server.smart_read.small"), Some("server.ping"));
        assert_eq!(floor_for("server.identity.placed"), Some("server.ping"));
        // The floors, and what has no process in it, have none.
        assert_eq!(floor_for("hook.floor"), None);
        assert_eq!(floor_for("server.ping"), None);
        assert_eq!(floor_for("hook.meter_read.inproc"), None);
    }

    #[test]
    fn the_ratio_and_the_floor_normalized_ratio_come_out_right() {
        // The machine is 1.5x slower across the board: the raw ratio is 1.5, the normalized 1.0.
        let committed = v(&[("hook.floor", 4_000), ("hook.meter_read.spawn", 6_000)]);
        let now = v(&[("hook.floor", 6_000), ("hook.meter_read.spawn", 9_000)]);
        let (rows, missing) = rows(&committed, &now);
        assert!(missing.is_empty());
        let r = rows.iter().find(|r| r.id == "hook.meter_read.spawn").unwrap();
        assert!((r.ratio - 1.5).abs() < 1e-9);
        assert!((r.normalized.unwrap() - 1.0).abs() < 1e-9);
        let f = rows.iter().find(|r| r.id == "hook.floor").unwrap();
        assert!((f.ratio - 1.5).abs() < 1e-9);
        assert_eq!(f.normalized, None);
        // The code got 20% slower on that machine too: normalized shows it, raw is 1.8.
        let now = v(&[("hook.floor", 6_000), ("hook.meter_read.spawn", 10_800)]);
        let (rows, _) = self::rows(&committed, &now);
        let r = rows.iter().find(|r| r.id == "hook.meter_read.spawn").unwrap();
        assert!((r.ratio - 1.8).abs() < 1e-9);
        assert!((r.normalized.unwrap() - 1.2).abs() < 1e-9);
    }

    #[test]
    fn a_series_with_a_figure_on_one_side_only_is_listed_not_counted() {
        let committed = v(&[("a", 10), ("b", 10)]);
        let now = v(&[("a", 11), ("c", 10)]);
        let (rows, missing) = rows(&committed, &now);
        assert_eq!(rows.len(), 1);
        assert_eq!(missing.len(), 2);
        assert!(missing.iter().any(|m| m.starts_with("b ")));
        assert!(missing.iter().any(|m| m.starts_with("c ")));
    }

    #[test]
    fn within_twenty_percent_means_both_ways() {
        assert!(is_within(1.0));
        assert!(is_within(1.2));
        assert!(is_within(0.8));
        assert!(!is_within(1.21));
        assert!(!is_within(0.79));
        assert!(!is_within(f64::NAN));
    }

    #[test]
    fn the_tally_counts_raw_and_normalized_separately() {
        let row = |ratio, normalized| Row {
            id: String::new(),
            committed_ns: 1,
            now_ns: 1,
            ratio,
            normalized,
        };
        let t = tally(&[
            row(1.0, Some(1.0)),
            row(1.5, Some(1.1)),
            row(0.7, None),
            row(1.1, Some(1.5)),
        ]);
        assert_eq!(
            t,
            Tally {
                raw_within: 2,
                raw_total: 4,
                normalized_within: 2,
                normalized_total: 3
            }
        );
    }

    #[test]
    fn the_machine_matches_only_when_both_floors_of_every_build_are_within_15_percent() {
        let floors = |hook: f64, ping: f64| {
            let r = |id: &str, ratio| Row {
                id: id.to_string(),
                committed_ns: 1,
                now_ns: 1,
                ratio,
                normalized: None,
            };
            vec![r("hook.floor", hook), r("server.ping", ping)]
        };
        let m = machine(&[("candidate".to_string(), floors(1.10, 0.90))]);
        assert_eq!(m.matches, Some(true));
        let m = machine(&[("candidate".to_string(), floors(1.10, 1.16))]);
        assert_eq!(m.matches, Some(false));
        // One build off is enough.
        let m = machine(&[
            ("baseline".to_string(), floors(1.0, 1.0)),
            ("candidate".to_string(), floors(0.80, 1.0)),
        ]);
        assert_eq!(m.matches, Some(false));
        // A floor with no figure is no answer, not a match.
        let m = machine(&[("candidate".to_string(), vec![])]);
        assert_eq!(m.matches, None);
    }

    #[test]
    fn a_committed_file_is_read_by_role_and_id() {
        let json = r#"{
            "label": "pre-release-abc", "environment": {"os": "macos"},
            "corpus": [{"sha": "c0ffee"}],
            "builds": [
              {"label": "v1.7.0", "role": "baseline", "sha": "aaaa"},
              {"label": "pre-abc", "role": "candidate", "sha": "bbbb"}],
            "latency": {"runs": 200, "warmup_runs": 20,
              "subjects": [{"rel": "x.rs"}],
              "builds": [
                {"label": "v1.7.0", "load_before": "1", "load_after": "2",
                 "series": [{"id": "hook.floor", "lat": {"median_ns": 3000}},
                            {"id": "hook.per_call_identity", "lat": null}]},
                {"label": "pre-abc", "load_before": "1", "load_after": "2",
                 "series": [{"id": "hook.floor", "lat": {"median_ns": 3100}}]}]}}"#;
        let c = parse_committed(json).unwrap();
        assert_eq!(c.runs, 200);
        assert_eq!(c.warmup, 20);
        assert_eq!(c.subjects, vec!["x.rs"]);
        let roles = by_role(&c);
        assert_eq!(roles["baseline"].sha, "aaaa");
        // A series with no figure (n/a) is not in the list.
        assert_eq!(roles["baseline"].series, v(&[("hook.floor", 3000)]));
        assert_eq!(roles["candidate"].series, v(&[("hook.floor", 3100)]));
        assert!(parse_committed("{\"builds\": []}").is_err());
    }

    #[test]
    fn the_rendering_states_the_verdict_in_words() {
        let committed = parse_committed(
            r#"{"label":"c","environment":{"os":"macos"},"corpus":[],
                "builds":[{"label":"b","role":"candidate","sha":"s"}],
                "latency":{"runs":100,"warmup_runs":1,"subjects":[],
                  "builds":[{"label":"b","series":[]}]}}"#,
        )
        .unwrap();
        let side = |hook: i64| {
            let committed = v(&[("hook.floor", 1000), ("server.ping", 100)]);
            let now = v(&[("hook.floor", hook), ("server.ping", 100)]);
            let (rows, missing) = rows(&committed, &now);
            Side {
                role: "candidate".to_string(),
                committed_label: "b".to_string(),
                committed_sha: "s".to_string(),
                now_label: "b".to_string(),
                now_sha: "s".to_string(),
                committed_load: String::new(),
                now_load: String::new(),
                rows,
                missing,
            }
        };
        let near = render(&committed, "", &[], &[side(1100)]);
        assert!(near.contains("THE MACHINE MATCHES"), "{near}");
        let far = render(&committed, "", &[], &[side(2000)]);
        assert!(far.contains("THE MACHINE DOES NOT MATCH"), "{far}");
        assert!(far.contains("2 of 2") || far.contains("1 of 2"), "{far}");
    }
}
