//! The per-run cost cap.
//!
//! The run's stream-json output is read line by line while it runs. Each `assistant` event's
//! usage is priced through lumen-core's table (`rates.rs`), once per `message.id`. When the
//! dollars reach the cap the run is killed and recorded as a failure with what it spent.
//!
//! The live stream carries the main thread's messages; a subagent's usage appears only in its
//! transcript, which is read after the run. So the live figure is a floor of the run's cost and
//! the cap can be overshot by subagent work: the record carries both the live figure and the
//! transcript's.

use super::transcript::{self, Dedup, StreamResult};
use crate::util::{Ctx, Res};
use serde::Serialize;
use std::io::{BufRead, BufReader};
use std::process::{Child, ExitStatus};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Dollars per run at which a run is killed.
pub const DEFAULT_CAP_USD: f64 = 3.0;
/// Wall-clock limit of one run.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(45 * 60);

/// Dollars so far, from the stream.
#[derive(Debug, Default)]
pub struct CostMeter {
    dedup: Dedup,
    result: Option<StreamResult>,
    /// Lines that were not JSON: counted, never fatal (a tool may print a banner).
    pub non_json_lines: u32,
}

impl CostMeter {
    pub fn feed_line(&mut self, line: &str) {
        let line = line.trim();
        if line.is_empty() {
            return;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            self.non_json_lines += 1;
            return;
        };
        if let Some(r) = transcript::stream_result(&v) {
            self.result = Some(r);
        }
        match transcript::usage_of(&v) {
            transcript::Usage::Entry(e) => self.dedup.add(e),
            transcript::Usage::None => {}
            transcript::Usage::NoId => self.dedup.no_id_entries += 1,
        }
    }

    pub fn usd(&self) -> f64 {
        self.dedup.spend().priced().cost.total()
    }

    /// Tokens of a model with no price. The cap cannot be enforced on them.
    pub fn unpriced_tokens(&self) -> i64 {
        self.dedup.spend().priced().unpriced.total()
    }

    pub fn result(&self) -> Option<&StreamResult> {
        self.result.as_ref()
    }

    pub fn rounds(&self) -> u32 {
        self.dedup.requests().count() as u32
    }
}

/// How the run ended.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", content = "detail")]
pub enum Outcome {
    /// The process exited. Whether the task was solved is E2's question.
    Exited { code: Option<i32> },
    /// Killed: the run's dollars reached the cap. A failure; the spend still counts.
    CapExceeded { cap_usd: f64 },
    /// Killed at the time limit. A failure.
    TimedOut { after_secs: u64 },
    /// Killed because the cap cannot be enforced (a model with no price).
    Unpriced { model_tokens: i64 },
}

impl Outcome {
    /// Cap kills, timeouts and unenforceable runs are failures; a clean exit is not one by itself.
    pub fn is_failure(&self) -> bool {
        match self {
            Outcome::Exited { code } => *code != Some(0),
            _ => true,
        }
    }
}

pub struct Watched {
    pub outcome: Outcome,
    pub lines: Vec<String>,
    /// Spawn to exit, as measured here.
    pub wall: Duration,
    pub status: Option<ExitStatus>,
    pub meter: CostMeter,
}

/// Read `child`'s stdout (it must be piped) until it ends, enforcing `cap_usd` and `timeout`.
/// `kill` ends the run; for a container it must stop the container, not only the client.
///
/// `poll` returns the subagent transcript lines written so far (all of them, every time: the
/// meter counts a message id once). It is called every `poll_every`, so a subagent's spend is in
/// the live total and the cap sees main thread plus subagents.
pub fn watch(
    child: &mut Child,
    started: Instant,
    cap_usd: f64,
    timeout: Duration,
    kill: &dyn Fn(&mut Child),
    poll: &dyn Fn() -> Vec<String>,
    poll_every: Duration,
) -> Res<Watched> {
    let stdout = child
        .stdout
        .take()
        .ok_or("the child's stdout is not piped")?;
    let (tx, rx) = mpsc::channel::<String>();
    // Detached: a grandchild that keeps the pipe open must not hold the harness.
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if tx.send(line).is_err() {
                break;
            }
        }
    });

    let verdict = |meter: &CostMeter| -> Option<Outcome> {
        if meter.unpriced_tokens() > 0 {
            Some(Outcome::Unpriced {
                model_tokens: meter.unpriced_tokens(),
            })
        } else if meter.usd() >= cap_usd {
            Some(Outcome::CapExceeded { cap_usd })
        } else if started.elapsed() >= timeout {
            Some(Outcome::TimedOut {
                after_secs: started.elapsed().as_secs(),
            })
        } else {
            None
        }
    };

    let mut meter = CostMeter::default();
    let mut lines = Vec::new();
    let mut last_poll = Instant::now();
    let killed: Option<Outcome> = loop {
        let got = rx.recv_timeout(Duration::from_millis(20));
        let disconnected = matches!(got, Err(mpsc::RecvTimeoutError::Disconnected));
        if let Ok(line) = got {
            meter.feed_line(&line);
            lines.push(line);
        }
        if last_poll.elapsed() >= poll_every {
            for l in poll() {
                meter.feed_line(&l);
            }
            last_poll = Instant::now();
        }
        if let Some(o) = verdict(&meter) {
            kill(child);
            break Some(o);
        }
        if disconnected {
            break None;
        }
    };
    // The last look at the subagents: what they spent before the run ended counts.
    for l in poll() {
        meter.feed_line(&l);
    }
    // What was already written when the kill landed still counts.
    let grace = Instant::now();
    while grace.elapsed() < Duration::from_millis(300) {
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(line) => {
                meter.feed_line(&line);
                lines.push(line);
            }
            Err(_) => break,
        }
    }
    let status = child.wait().ctx("wait for the run")?;
    let wall = started.elapsed();
    let outcome = killed.unwrap_or(Outcome::Exited {
        code: status.code(),
    });
    Ok(Watched {
        outcome,
        lines,
        wall,
        status: Some(status),
        meter,
    })
}
