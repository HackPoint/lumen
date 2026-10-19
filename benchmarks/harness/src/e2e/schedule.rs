//! The order of the runs: seeded, shuffled, balanced, and each shared arm once.
//!
//! The competitor comparison (F22) and the version comparison (V1) share two arms, no Lumen and
//! Lumen 1.9.0. A schedule built for both lists each (arm, task, run) once, and both reports
//! cite those runs.
//!
//! No `rand` crate: a small deterministic generator seeded from the sha256 of the seed string, so
//! the same seed gives the same order on every platform and toolchain.

use crate::manifest::{Tasks, Tools};
use crate::util::{Res, sha256_hex};
use serde::Serialize;
use std::collections::BTreeSet;

/// Runs per (arm, task) cell in a full run.
pub const MIN_REPS: u32 = 3;

/// Which comparison an arm belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Suite {
    /// F22: no Lumen, Lumen 1.9.0 and the four tools.
    Competitors,
    /// V1: no Lumen, Lumen 1.9.0 and the older versions that run end to end.
    Versions,
}

/// The arm ids of `suite`, in tools.toml order.
pub fn arms_for(tools: &Tools, suite: Suite) -> Vec<String> {
    tools
        .arm
        .iter()
        .filter(|a| match suite {
            Suite::Competitors => a.shared || a.kind == "competitor",
            Suite::Versions => a.shared || a.kind == "lumen",
        })
        .map(|a| a.id.clone())
        .collect()
}

/// Every task id: the SWE-bench instances, then the scenarios.
pub fn task_ids(tasks: &Tasks) -> Vec<String> {
    tasks
        .swe
        .iter()
        .map(|t| t.id.clone())
        .chain(tasks.scenario.iter().map(|s| s.id.clone()))
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Slot {
    pub index: usize,
    pub arm: String,
    pub task: String,
    /// 0-based; the n-th run of this cell.
    pub rep: u32,
}

#[derive(Clone, Debug, Serialize)]
pub struct Schedule {
    pub seed: String,
    pub reps: u32,
    pub slots: Vec<Slot>,
}

struct Rng(u64);

impl Rng {
    fn new(seed: &str, salt: &str) -> Rng {
        let h = sha256_hex(format!("{seed}\n{salt}").as_bytes());
        Rng(u64::from_str_radix(&h[..16], 16).unwrap_or(0x9E37_79B9_7F4A_7C15))
    }
    /// splitmix64.
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    /// Uniform enough for a shuffle of a few dozen items.
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn shuffle<T>(&mut self, v: &mut [T]) {
        for i in (1..v.len()).rev() {
            let j = self.below(i + 1);
            v.swap(i, j);
        }
    }
}

fn dedup_keep_order(ids: &[String]) -> Vec<String> {
    let mut seen = BTreeSet::new();
    ids.iter()
        .filter(|i| seen.insert((*i).clone()))
        .cloned()
        .collect()
}

/// Every arm x task x `reps` once. For each rep the tasks come in a seeded order and, within a
/// task, the arms do too, so no arm always goes first or runs in the same hour. Arms and tasks
/// listed twice are listed once. Pure in `(seed, arms, tasks, reps)`.
pub fn build(seed: &str, arms: &[String], tasks: &[String], reps: u32) -> Res<Schedule> {
    if reps == 0 {
        return Err("a schedule needs at least one run per cell".into());
    }
    let arms = dedup_keep_order(arms);
    let tasks = dedup_keep_order(tasks);
    if arms.is_empty() || tasks.is_empty() {
        return Err("a schedule needs at least one arm and one task".into());
    }
    let mut slots = Vec::new();
    for rep in 0..reps {
        let mut order = tasks.clone();
        Rng::new(seed, &format!("tasks/{rep}")).shuffle(&mut order);
        for task in order {
            let mut a = arms.clone();
            Rng::new(seed, &format!("arms/{rep}/{task}")).shuffle(&mut a);
            for arm in a {
                slots.push(Slot {
                    index: slots.len(),
                    arm,
                    task: task.clone(),
                    rep,
                });
            }
        }
    }
    Ok(Schedule {
        seed: seed.to_string(),
        reps,
        slots,
    })
}

/// The full runs of one or both comparisons: every task, `reps` (at least [`MIN_REPS`]) runs per
/// cell, a shared arm once.
pub fn full(
    seed: &str,
    tools: &Tools,
    tasks: &Tasks,
    suites: &[Suite],
    reps: u32,
) -> Res<Schedule> {
    if reps < MIN_REPS {
        return Err(format!(
            "a full run needs at least {MIN_REPS} runs per cell, not {reps}"
        ));
    }
    let arms: Vec<String> = suites.iter().flat_map(|s| arms_for(tools, *s)).collect();
    build(seed, &arms, &task_ids(tasks), reps)
}

/// The pilot: one run of every arm on the first task of the draw, in a seeded order. Published
/// like any run and not counted in the results.
pub fn pilot(seed: &str, tools: &Tools, tasks: &Tasks) -> Res<Schedule> {
    let arms: Vec<String> = tools.arm.iter().map(|a| a.id.clone()).collect();
    let first = task_ids(tasks)
        .into_iter()
        .next()
        .ok_or("tasks.toml has no task")?;
    build(seed, &arms, &[first], 1)
}

impl Schedule {
    /// Runs of each (arm, task) cell.
    pub fn cell_counts(&self) -> std::collections::BTreeMap<(String, String), u32> {
        let mut m = std::collections::BTreeMap::new();
        for s in &self.slots {
            *m.entry((s.arm.clone(), s.task.clone())).or_insert(0) += 1;
        }
        m
    }

    /// One line per run, for the schedule command and the record.
    pub fn render(&self) -> String {
        let mut out = format!(
            "seed {:?}, {} run(s) per cell, {} runs\n",
            self.seed,
            self.reps,
            self.slots.len()
        );
        for s in &self.slots {
            out.push_str(&format!(
                "{:>4}  {:<14} {}  #{}\n",
                s.index + 1,
                s.arm,
                s.task,
                s.rep + 1
            ));
        }
        out
    }
}

/// A run's nonce: a deterministic uuid-shaped string of the seed, the run and the attempt, so a
/// rerun after a failure does not share its cache prefix with the attempt before it.
pub fn nonce(seed: &str, slot: &Slot, attempt: u32) -> String {
    let h = sha256_hex(
        format!(
            "{seed}\n{}\n{}\n{}\n{attempt}",
            slot.arm, slot.task, slot.rep
        )
        .as_bytes(),
    );
    format!(
        "{}-{}-4{}-a{}-{}",
        &h[0..8],
        &h[8..12],
        &h[13..16],
        &h[17..20],
        &h[20..32]
    )
}
