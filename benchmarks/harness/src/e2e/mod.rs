//! `lumen-bench e2e ...`: the end-to-end runs (E1-E3).
//!
//! E1 (this module): the shared runner. An [`arms::Arm`] installs into a fresh scratch HOME, a
//! [`RunSpec`] says what one run is, an [`Executor`] runs it (a stub in tests, Docker for real),
//! [`cap`] reads the stream live and kills a run at its dollar cap, [`transcript`] counts what
//! the run used, and [`schedule`] orders the runs. Scoring is E2's, the paid pilot E3's.
//!
//! **A real run cannot start by accident.** `e2e run` refuses unless `ANTHROPIC_API_KEY` is set
//! and `--i-have-a-budget <usd>` is given, and that budget bounds the run's cap. Nothing here
//! reads the owner's Claude Code login or `~/.claude`: every run's environment is the
//! [`Sandbox`]'s, with the key as its one credential.
//!
//! `main.rs` dispatches here and nowhere else.

pub mod arms;
pub mod cap;
pub mod realhome;
pub mod schedule;
pub mod transcript;

use crate::manifest::{Tasks, Tools};
use crate::sandbox::Sandbox;
use crate::util::{Ctx, Res};
use arms::{Arm, ArmPlan, Fetcher, InstallCtx};
use cap::Outcome;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

pub const HELP: &str = "\
usage: benchmarks/run.sh e2e <command> [options]

  schedule  print the seeded order of the runs (no cost, no network)
              [--seed S] [--suite competitors|versions|both] [--reps N] [--pilot]
  arms      describe every arm (what it installs, its telemetry switches and deviations)
  run-schedule  the runs of a seeded schedule, one after another, within one budget. Same guard as
            `run`; stops when what is left of the budget cannot cover a run, and keeps the spend
            in <out>/budget-ledger.json so a restart cannot spend it twice.
              --i-have-a-budget USD --task-dir DIR (DIR/<task>/prompt.txt and DIR/<task>/image)
              --cache DIR [--seed S] [--suite ..] [--reps N] [--pilot] [--arms A,B] [--tasks T,U]
              [--cap USD] [--min-run-usd USD] [--out DIR] [--hook-scripts DIR]
  run       one real run in a container. Refuses to start unless ANTHROPIC_API_KEY is set and
            --i-have-a-budget <usd> is given.
              --arm ID --task ID --image IMAGE --prompt-file FILE --i-have-a-budget USD
              [--cap USD] [--out DIR] [--cache DIR] [--hook-scripts DIR]

The paid pilot (E3) is the first real use of `run`.
";

/// The model every run uses.
pub const MODEL: &str = "claude-sonnet-5-5";

/// One line after the problem statement, the same for every arm and task.
pub const INSTRUCTION: &str = "Fix the issue described above by editing the files of the current repository. \
Do not modify existing tests. When the fix is in place, stop.";

pub fn build_prompt(problem_statement: &str) -> String {
    format!("{}\n\n{INSTRUCTION}", problem_statement.trim_end())
}

// --- the guard against an accidental real run --------------------------------------------------------

/// Permission to spend: the key is set and the owner named a budget.
#[derive(Clone, Debug, PartialEq)]
pub struct Authorized {
    pub budget_usd: f64,
}

/// Refuse unless both the key and the budget flag are present and sane. Pure: the caller reads
/// the environment, so a test needs no real key.
pub fn authorize(api_key: Option<&str>, budget_flag: Option<&str>) -> Res<Authorized> {
    let mut missing = Vec::new();
    if api_key.is_none_or(|k| k.trim().is_empty()) {
        missing.push("ANTHROPIC_API_KEY is not set");
    }
    if budget_flag.is_none() {
        missing.push("--i-have-a-budget <usd> is not given");
    }
    if !missing.is_empty() {
        return Err(format!(
            "refusing to start a real run: {}. A real run spends money; it needs both.",
            missing.join(" and ")
        ));
    }
    let raw = budget_flag.unwrap_or_default();
    let budget_usd: f64 = raw
        .trim()
        .parse()
        .map_err(|_| format!("--i-have-a-budget {raw:?} is not a number of dollars"))?;
    if !budget_usd.is_finite() || budget_usd <= 0.0 {
        return Err(format!(
            "--i-have-a-budget {raw:?} must be a positive number of dollars"
        ));
    }
    Ok(Authorized { budget_usd })
}

/// What the run may spend: the per-run cap, bounded by what is left of the budget.
#[derive(Clone, Debug)]
pub struct Budget {
    pub total_usd: f64,
    pub spent_usd: f64,
}

impl Budget {
    pub fn new(a: &Authorized) -> Budget {
        Budget {
            total_usd: a.budget_usd,
            spent_usd: 0.0,
        }
    }
    pub fn remaining(&self) -> f64 {
        (self.total_usd - self.spent_usd).max(0.0)
    }
    /// The cap for the next run, or an error when nothing is left.
    pub fn cap_for_run(&self, per_run_usd: f64) -> Res<f64> {
        if self.remaining() <= 0.0 {
            return Err(format!(
                "the budget of ${:.2} is spent: no run may start",
                self.total_usd
            ));
        }
        Ok(per_run_usd.min(self.remaining()))
    }
    pub fn record(&mut self, usd: f64) {
        self.spent_usd += usd;
    }
}

// --- one run -------------------------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct RunSpec {
    /// The nonce: a uuid-shaped string, also the run's directory name.
    pub run_id: String,
    pub arm: String,
    pub task: String,
    /// The container image the run starts from.
    pub image: String,
    pub model: String,
    pub claude_version: String,
    /// Problem statement and the fixed instruction.
    pub prompt: String,
    pub timeout: Duration,
    pub cap_usd: f64,
    /// How often the subagents' transcripts are read while the run goes.
    pub poll_every: Duration,
}

impl RunSpec {
    pub fn append_system_prompt(&self) -> String {
        format!("run {}", self.run_id)
    }

    /// The arguments of `claude`, as METHOD.md gives them.
    pub fn claude_args(&self) -> Vec<String> {
        [
            "-p",
            &self.prompt,
            "--model",
            &self.model,
            "--output-format",
            "stream-json",
            "--verbose",
            "--dangerously-skip-permissions",
            "--append-system-prompt",
            &self.append_system_prompt(),
        ]
        .iter()
        .map(|s| s.to_string())
        .collect()
    }
}

/// What an executor is given.
pub struct RunCtx<'a> {
    pub spec: &'a RunSpec,
    pub plan: &'a ArmPlan,
    pub sandbox: &'a Sandbox,
    pub api_key: Option<&'a str>,
}

/// Runs `claude` somewhere. A stub runs a test program on this machine; Docker runs a container.
pub trait Executor {
    /// HOME as Claude Code sees it, for the paths in its configuration.
    fn claude_home(&self, sandbox: &Sandbox) -> String;
    /// Before the run: the container is created and the staged HOME copied in. Default: nothing.
    fn prepare(&self, _ctx: &RunCtx<'_>) -> Res<()> {
        Ok(())
    }
    /// The process to spawn; its stdout is the stream-json.
    fn command(&self, ctx: &RunCtx<'_>) -> Res<Command>;
    /// End a run. A container must be stopped, not only its client.
    fn kill(&self, _ctx: &RunCtx<'_>, child: &mut Child) {
        let _ = child.kill();
    }
    /// Every subagent transcript line written so far (`subagents/agent-*.jsonl`), read while the
    /// run goes so their spend is in the live total. Repeats are fine: a message id counts once.
    fn subagent_lines(&self, _ctx: &RunCtx<'_>) -> Vec<String> {
        Vec::new()
    }
    /// After the run, before cleanup: bring the transcripts back into the sandbox's HOME.
    fn collect(&self, _ctx: &RunCtx<'_>) -> Res<()> {
        Ok(())
    }
    /// Always, last: remove what the run left (the container).
    fn cleanup(&self, _ctx: &RunCtx<'_>) {}
}

/// Runs a program (a stub `claude` in tests) with the arguments `claude` would get, in the
/// sandbox, on this machine. `lead_args` come first (a stub's scenario).
pub struct ProgramExecutor {
    pub program: PathBuf,
    pub lead_args: Vec<String>,
}

impl Executor for ProgramExecutor {
    fn claude_home(&self, sandbox: &Sandbox) -> String {
        sandbox.home.to_string_lossy().replace('\\', "/")
    }
    fn command(&self, ctx: &RunCtx<'_>) -> Res<Command> {
        let mut cmd = Command::new(&self.program);
        ctx.sandbox.apply(&mut cmd);
        for (k, v) in &ctx.plan.env {
            cmd.env(k, v);
        }
        cmd.args(&self.lead_args).args(ctx.spec.claude_args());
        Ok(cmd)
    }
    fn subagent_lines(&self, ctx: &RunCtx<'_>) -> Vec<String> {
        let Ok((_, subs)) = transcript::find_transcripts(&ctx.sandbox.home) else {
            return Vec::new();
        };
        subs.iter()
            .filter_map(|p| std::fs::read_to_string(p).ok())
            .flat_map(|t| t.lines().map(str::to_string).collect::<Vec<_>>())
            .collect()
    }
}

/// Docker: `create`, copy the staged HOME in (no host mount), `start -a`, copy the transcripts
/// out, `rm`. Only its command lines are tested here; E3 is its first real use.
pub struct DockerExecutor {
    pub docker: String,
    /// Where the repository of the instance is in the image.
    pub workdir: String,
}

pub const CONTAINER_HOME: &str = "/home/bench";

/// The instance images are x86_64; on any other host Docker would otherwise pick another
/// architecture or emulate silently.
pub const DOCKER_PLATFORM: &str = "linux/amd64";

/// An image must be named by digest (`repo@sha256:<64 hex>`): a tag can move between two runs,
/// and then two arms would not have started from the same image.
pub fn validate_image(image: &str) -> Res<()> {
    match image.rsplit_once("@sha256:") {
        Some((repo, hex))
            if !repo.is_empty()
                && hex.len() == 64
                && hex
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) =>
        {
            Ok(())
        }
        _ => Err(format!(
            "image {image:?} is not pinned: name it as <repo>@sha256:<64 hex digits>"
        )),
    }
}

fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

impl DockerExecutor {
    pub fn container_name(spec: &RunSpec) -> String {
        format!("lumen-e2e-{}", spec.run_id)
    }

    /// The script the container runs: Claude Code at the pinned version, the arm's setup, then
    /// `claude`, all with the container's own HOME.
    pub fn script(&self, ctx: &RunCtx<'_>, claude_package: &str) -> String {
        let mut s = String::from("set -e\n");
        if !ctx.plan.path_prepend.is_empty() {
            s.push_str(&format!(
                "export PATH={}:\"$PATH\"\n",
                ctx.plan.path_prepend.join(":")
            ));
        }
        s.push_str(&format!("npm install -g {}\n", sh_quote(claude_package)));
        for argv in &ctx.plan.setup {
            s.push_str(
                &argv
                    .iter()
                    .map(|a| sh_quote(a))
                    .collect::<Vec<_>>()
                    .join(" "),
            );
            s.push('\n');
        }
        s.push_str(&format!("cd {}\nexec ", sh_quote(&self.workdir)));
        for p in &ctx.plan.claude_prefix {
            s.push_str(&sh_quote(p));
            s.push(' ');
        }
        s.push_str("claude");
        for a in ctx.spec.claude_args() {
            s.push(' ');
            s.push_str(&sh_quote(&a));
        }
        s.push('\n');
        s
    }

    /// `docker create ...`. The key is passed by name, so its value is never on a command line.
    pub fn create_argv(&self, ctx: &RunCtx<'_>, claude_package: &str) -> Vec<String> {
        let mut v: Vec<String> = vec![
            self.docker.clone(),
            "create".into(),
            "--platform".into(),
            DOCKER_PLATFORM.into(),
            "--init".into(),
            "--name".into(),
            Self::container_name(ctx.spec),
            "-e".into(),
            format!("HOME={CONTAINER_HOME}"),
        ];
        if ctx.api_key.is_some() {
            v.extend(["-e".into(), "ANTHROPIC_API_KEY".into()]);
        }
        for (k, val) in &ctx.plan.env {
            v.extend(["-e".into(), format!("{k}={val}")]);
        }
        v.extend([
            ctx.spec.image.clone(),
            "bash".into(),
            "-lc".into(),
            self.script(ctx, claude_package),
        ]);
        v
    }

    fn run(&self, args: &[String], ctx: &RunCtx<'_>) -> Res<()> {
        let mut cmd = Command::new(&args[0]);
        ctx.sandbox.apply(&mut cmd);
        cmd.args(&args[1..]);
        if let Some(k) = ctx.api_key {
            cmd.env("ANTHROPIC_API_KEY", k);
        }
        let out = cmd.output().ctx(&format!("run {}", args[1..].join(" ")))?;
        if out.status.success() {
            Ok(())
        } else {
            Err(format!(
                "{} {} failed: {}",
                args[0],
                args[1],
                String::from_utf8_lossy(&out.stderr).trim()
            ))
        }
    }
}

impl Executor for DockerExecutor {
    fn claude_home(&self, _: &Sandbox) -> String {
        CONTAINER_HOME.to_string()
    }
    fn prepare(&self, ctx: &RunCtx<'_>) -> Res<()> {
        validate_image(&ctx.spec.image)?;
        let tools = Tools::load_committed()?;
        let cc = &tools.claude_code;
        let package = format!("{}@{}", cc.package, cc.version);
        self.run(&self.create_argv(ctx, &package), ctx)?;
        let name = Self::container_name(ctx.spec);
        self.run(
            &[
                self.docker.clone(),
                "cp".into(),
                format!("{}/.", ctx.sandbox.home.display()),
                format!("{name}:{CONTAINER_HOME}"),
            ],
            ctx,
        )
    }
    fn command(&self, ctx: &RunCtx<'_>) -> Res<Command> {
        let mut cmd = Command::new(&self.docker);
        ctx.sandbox.apply(&mut cmd);
        cmd.args(["start", "-a", &Self::container_name(ctx.spec)]);
        Ok(cmd)
    }
    fn subagent_lines(&self, ctx: &RunCtx<'_>) -> Vec<String> {
        let mut cmd = Command::new(&self.docker);
        ctx.sandbox.apply(&mut cmd);
        cmd.args(["exec", &Self::container_name(ctx.spec), "sh", "-c"])
            .arg("cat \"$HOME\"/.claude/projects/*/*/subagents/agent-*.jsonl 2>/dev/null; true");
        match cmd.output() {
            Ok(o) => String::from_utf8_lossy(&o.stdout)
                .lines()
                .map(str::to_string)
                .collect(),
            Err(_) => Vec::new(),
        }
    }
    fn kill(&self, ctx: &RunCtx<'_>, child: &mut Child) {
        let _ = self.run(
            &[
                self.docker.clone(),
                "kill".into(),
                Self::container_name(ctx.spec),
            ],
            ctx,
        );
        let _ = child.kill();
    }
    fn collect(&self, ctx: &RunCtx<'_>) -> Res<()> {
        self.run(
            &[
                self.docker.clone(),
                "cp".into(),
                format!(
                    "{}:{CONTAINER_HOME}/.claude/projects",
                    Self::container_name(ctx.spec)
                ),
                format!("{}/.claude/", ctx.sandbox.home.display()),
            ],
            ctx,
        )
    }
    fn cleanup(&self, ctx: &RunCtx<'_>) {
        let _ = self.run(
            &[
                self.docker.clone(),
                "rm".into(),
                "-f".into(),
                Self::container_name(ctx.spec),
            ],
            ctx,
        );
    }
}

/// The record of one run: everything the reports need, and nothing that is a secret.
#[derive(Clone, Debug, Serialize)]
pub struct RunRecord {
    pub run_id: String,
    pub arm: String,
    pub task: String,
    pub model: String,
    pub claude_version: String,
    pub image: String,
    pub cap_usd: f64,
    pub timeout_secs: u64,
    pub outcome: Outcome,
    /// A cap kill, a timeout, a non-zero exit or an error result. Task success is E2's.
    pub failure: bool,
    pub failure_reason: Option<String>,
    /// Dollars the live stream showed when the run ended: a floor (no subagents).
    pub live_usd: f64,
    pub live_rounds: u32,
    /// From the transcripts, subagents included. `None` when none could be read.
    pub transcript: Option<transcript::Totals>,
    pub transcript_error: Option<String>,
    pub stream_result: Option<transcript::StreamResult>,
    pub cross_check: transcript::CrossCheck,
    /// What the run cost: the larger of the live and the transcript's figure.
    pub usd: f64,
    /// Spawn to exit.
    pub wall_secs: f64,
    pub provenance: Vec<String>,
    pub flags: Vec<String>,
    pub stderr_tail: Vec<String>,
}

/// Remove every occurrence of `secret` from `text`.
pub fn redact(text: &str, secret: Option<&str>) -> String {
    match secret.filter(|s| !s.is_empty()) {
        Some(s) => text.replace(s, "[redacted]"),
        None => text.to_string(),
    }
}

pub struct RunInputs<'a> {
    pub spec: &'a RunSpec,
    pub arm: &'a dyn Arm,
    pub fetcher: &'a dyn Fetcher,
    pub executor: &'a dyn Executor,
    /// Where the run's scratch directory is made.
    pub root: &'a Path,
    pub api_key: Option<&'a str>,
    pub hook_scripts: Option<&'a Path>,
}

/// Install the arm into a fresh HOME, run, watch the cost, read the transcripts.
pub fn run_one(inp: &RunInputs<'_>) -> Res<RunRecord> {
    let spec = inp.spec;
    if spec.arm != inp.arm.id() {
        return Err(format!(
            "the run is for arm {} but was given arm {}",
            spec.arm,
            inp.arm.id()
        ));
    }
    let mut sandbox = Sandbox::create(&inp.root.join(&spec.run_id))?;
    if let Some(k) = inp.api_key {
        sandbox = sandbox.set("ANTHROPIC_API_KEY", k);
    }
    let claude_home = inp.executor.claude_home(&sandbox);
    let plan = inp.arm.install(&InstallCtx {
        home: &sandbox.home,
        claude_home: &claude_home,
        fetcher: inp.fetcher,
        hook_scripts: inp.hook_scripts,
    })?;
    let ctx = RunCtx {
        spec,
        plan: &plan,
        sandbox: &sandbox,
        api_key: inp.api_key,
    };
    let result = run_in(inp, &ctx);
    inp.executor.cleanup(&ctx);
    result
}

fn run_in(inp: &RunInputs<'_>, ctx: &RunCtx<'_>) -> Res<RunRecord> {
    let spec = ctx.spec;
    inp.executor.prepare(ctx)?;
    let stderr_path = ctx.sandbox.root.join("stderr.log");
    let stderr = std::fs::File::create(&stderr_path).ctx("create the stderr log")?;
    let mut cmd = inp.executor.command(ctx)?;
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::from(stderr));
    let started = Instant::now();
    let mut child = cmd.spawn().ctx(&format!("spawn {:?}", cmd.get_program()))?;
    let w = cap::watch(
        &mut child,
        started,
        spec.cap_usd,
        spec.timeout,
        &|c| inp.executor.kill(ctx, c),
        &|| inp.executor.subagent_lines(ctx),
        spec.poll_every,
    )?;

    let mut flags = Vec::new();
    let collected = inp.executor.collect(ctx);
    if let Err(e) = &collected {
        flags.push(format!("could not collect the transcripts: {e}"));
    }
    let (totals, transcript_error) = match transcript::parse_home(&ctx.sandbox.home) {
        Ok(t) if t.files > 0 => (Some(t), None),
        Ok(_) => (None, Some("no transcript was written".to_string())),
        Err(e) => (None, Some(e)),
    };
    let result = w.meter.result().cloned();
    let cross = match &totals {
        Some(t) => transcript::cross_check(&t.tokens, result.as_ref()),
        None => transcript::cross_check(&Default::default(), None),
    };
    if cross.flagged() {
        flags.push(format!(
            "transcript and stream result differ by more than {:.0}%: {}",
            transcript::CROSS_CHECK_TOLERANCE * 100.0,
            cross.mismatches.join("; ")
        ));
    }
    if let Some(t) = &totals {
        if t.unpriced_tokens > 0 {
            flags.push(format!(
                "{} tokens of a model with no price are not in the dollars",
                t.unpriced_tokens
            ));
        }
        if t.disagreements > 0 {
            flags.push(format!(
                "{} message id(s) were repeated with different usage",
                t.disagreements
            ));
        }
        if t.no_id_entries > 0 {
            flags.push(format!(
                "{} usage entries had no message id",
                t.no_id_entries
            ));
        }
    }

    let live_usd = w.meter.usd();
    if matches!(w.outcome, Outcome::Exited { .. }) && live_usd >= spec.cap_usd {
        flags.push(format!(
            "the run ended on its own with ${live_usd:.4} spent, over its ${:.2} cap: the \
             subagents' last spend landed after the final look",
            spec.cap_usd
        ));
    }
    let usd = totals.as_ref().map_or(live_usd, |t| t.usd.max(live_usd));
    let (mut failure, mut reason) = match &w.outcome {
        Outcome::CapExceeded { cap_usd } => (
            true,
            Some(format!(
                "killed at the ${cap_usd:.2} cap with ${live_usd:.4} spent"
            )),
        ),
        Outcome::TimedOut { after_secs } => (true, Some(format!("timed out after {after_secs} s"))),
        Outcome::Unpriced { model_tokens } => (
            true,
            Some(format!(
                "killed: {model_tokens} tokens of a model with no price, so the cap cannot be enforced"
            )),
        ),
        Outcome::Exited { code } if *code != Some(0) => {
            (true, Some(format!("exited with {code:?}")))
        }
        Outcome::Exited { .. } => (false, None),
    };
    if let Some(r) = &result
        && r.is_error
        && !failure
    {
        failure = true;
        reason = Some(format!("the stream result is an error ({})", r.subtype));
    }
    let tail: Vec<String> = std::fs::read_to_string(&stderr_path)
        .unwrap_or_default()
        .lines()
        .rev()
        .take(20)
        .map(|l| redact(l, ctx.api_key))
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    Ok(RunRecord {
        run_id: spec.run_id.clone(),
        arm: spec.arm.clone(),
        task: spec.task.clone(),
        model: spec.model.clone(),
        claude_version: spec.claude_version.clone(),
        image: spec.image.clone(),
        cap_usd: spec.cap_usd,
        timeout_secs: spec.timeout.as_secs(),
        outcome: w.outcome,
        failure,
        failure_reason: reason,
        live_usd,
        live_rounds: w.meter.rounds(),
        transcript: totals,
        transcript_error,
        stream_result: result,
        cross_check: cross,
        usd,
        wall_secs: w.wall.as_secs_f64(),
        provenance: ctx.plan.provenance.clone(),
        flags,
        stderr_tail: tail,
    })
}

/// Write the record as `<dir>/<run_id>.json`, refusing if the key is in it.
pub fn write_record(dir: &Path, rec: &RunRecord, api_key: Option<&str>) -> Res<PathBuf> {
    let text = serde_json::to_string_pretty(rec).ctx("serialize the record")?;
    if let Some(k) = api_key.filter(|k| !k.is_empty())
        && text.contains(k)
    {
        return Err("the record contains the API key: not written".into());
    }
    std::fs::create_dir_all(dir).ctx(&format!("create {}", dir.display()))?;
    let path = dir.join(format!("{}.json", rec.run_id));
    std::fs::write(&path, text).ctx(&format!("write {}", path.display()))?;
    Ok(path)
}

// --- the command line --------------------------------------------------------------------------------------

fn flag(args: &[String], name: &str) -> Res<Option<String>> {
    match args.iter().position(|a| a == name) {
        None => Ok(None),
        Some(i) => args
            .get(i + 1)
            .filter(|v| !v.starts_with("--"))
            .cloned()
            .map(Some)
            .ok_or_else(|| format!("{name} needs a value")),
    }
}

fn need(args: &[String], name: &str) -> Res<String> {
    flag(args, name)?.ok_or_else(|| format!("{name} is required"))
}

/// Entry point for `lumen-bench e2e <args>`. `args` are what follows the subcommand.
pub fn entry(args: &[String]) -> Res<()> {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print!("{HELP}");
        return Ok(());
    }
    if args.is_empty() {
        return Err(format!("e2e: a command is required\n\n{HELP}"));
    }
    let rest = &args[1..];
    match args[0].as_str() {
        "schedule" => cmd_schedule(rest),
        "arms" => cmd_arms(),
        "run" => cmd_run(rest),
        "run-schedule" => cmd_run_schedule(rest),
        other => Err(format!("e2e: unknown command {other:?}\n\n{HELP}")),
    }
}

fn cmd_schedule(args: &[String]) -> Res<()> {
    let tools = Tools::load_committed()?;
    let tasks = Tasks::load_committed()?;
    let seed = flag(args, "--seed")?.unwrap_or_else(|| "lumen-p2-e2e".to_string());
    if args.iter().any(|a| a == "--pilot") {
        print!("{}", schedule::pilot(&seed, &tools, &tasks)?.render());
        return Ok(());
    }
    let suites = match flag(args, "--suite")?.as_deref().unwrap_or("both") {
        "competitors" => vec![schedule::Suite::Competitors],
        "versions" => vec![schedule::Suite::Versions],
        "both" => vec![schedule::Suite::Competitors, schedule::Suite::Versions],
        other => return Err(format!("--suite {other:?}: competitors, versions or both")),
    };
    let reps: u32 = flag(args, "--reps")?
        .map(|r| {
            r.parse()
                .map_err(|_| format!("--reps {r:?} is not a number"))
        })
        .transpose()?
        .unwrap_or(schedule::MIN_REPS);
    print!(
        "{}",
        schedule::full(&seed, &tools, &tasks, &suites, reps)?.render()
    );
    Ok(())
}

fn cmd_arms() -> Res<()> {
    let tools = Tools::load_committed()?;
    let d: Vec<_> = arms::all_arms(&tools)?
        .iter()
        .map(|a| a.describe())
        .collect();
    println!("{}", serde_json::to_string_pretty(&d).ctx("serialize")?);
    Ok(())
}

/// Every value of a repeatable flag.
fn flag_all(args: &[String], name: &str) -> Vec<String> {
    args.windows(2)
        .filter(|w| w[0] == name)
        .map(|w| w[1].clone())
        .collect()
}

fn number(args: &[String], name: &str, default: f64) -> Res<f64> {
    match flag(args, name)? {
        None => Ok(default),
        Some(v) => v
            .parse::<f64>()
            .ok()
            .filter(|n| n.is_finite() && *n >= 0.0)
            .ok_or_else(|| format!("{name} {v:?} is not a number of dollars")),
    }
}

// --- the spend ledger -----------------------------------------------------------------------------------

pub const LEDGER_FILE: &str = "budget-ledger.json";

#[derive(Clone, Debug, Default, Serialize, serde::Deserialize)]
pub struct LedgerRun {
    pub run_id: String,
    pub usd: f64,
}

/// What has been spent from the results directory, kept on disk so a restart cannot spend the
/// same budget twice. A run is written as `pending` at its cap before it starts: if the harness
/// dies mid-run, the next start counts that cap as spent (the worst the run could have cost).
#[derive(Clone, Debug, Default, Serialize, serde::Deserialize)]
pub struct Ledger {
    pub spent_usd: f64,
    pub runs: Vec<LedgerRun>,
    pub pending: Option<LedgerRun>,
}

impl Ledger {
    pub fn load(dir: &Path) -> Res<Ledger> {
        let path = dir.join(LEDGER_FILE);
        if !path.exists() {
            return Ok(Ledger::default());
        }
        let text = std::fs::read_to_string(&path).ctx(&format!("read {}", path.display()))?;
        let mut l: Ledger =
            serde_json::from_str(&text).ctx(&format!("{} is not a ledger", path.display()))?;
        if let Some(p) = l.pending.take() {
            l.spent_usd += p.usd;
            l.runs.push(p);
            l.save(dir)?;
        }
        Ok(l)
    }

    pub fn save(&self, dir: &Path) -> Res<()> {
        std::fs::create_dir_all(dir).ctx(&format!("create {}", dir.display()))?;
        let tmp = dir.join(format!("{LEDGER_FILE}.tmp"));
        let text = serde_json::to_string_pretty(self).ctx("serialize the ledger")?;
        std::fs::write(&tmp, text).ctx("write the ledger")?;
        std::fs::rename(&tmp, dir.join(LEDGER_FILE)).ctx("replace the ledger")
    }
}

/// What came of one slot.
pub enum SlotResult {
    /// Its record is already in the results directory.
    Done,
    /// What is left of the budget cannot cover a run.
    OutOfBudget {
        remaining_usd: f64,
    },
    Ran(Box<RunRecord>),
}

/// Everything the commands share.
struct Cli {
    tools: Tools,
    key: Option<String>,
    exec: Box<dyn Executor>,
    docker: bool,
    fetcher: arms::NetworkFetcher,
    out: PathBuf,
    scratch: tempfile::TempDir,
    hook_scripts: Option<PathBuf>,
    poll_every: Duration,
    per_run_usd: f64,
    min_run_usd: f64,
}

fn cli_from(args: &[String], key: Option<String>) -> Res<Cli> {
    let tools = Tools::load_committed()?;
    let docker = flag(args, "--claude-program")?.is_none();
    // `--claude-program` runs a program on this machine in place of a container: for the tests'
    // stub `claude`. It is not a way around the guard, which has already passed.
    let exec: Box<dyn Executor> = match flag(args, "--claude-program")? {
        Some(p) => Box::new(ProgramExecutor {
            program: PathBuf::from(p),
            lead_args: flag_all(args, "--claude-lead-arg"),
        }),
        None => Box::new(DockerExecutor {
            docker: "docker".into(),
            workdir: "/testbed".into(),
        }),
    };
    Ok(Cli {
        tools,
        key,
        exec,
        docker,
        fetcher: arms::NetworkFetcher {
            cache: PathBuf::from(need(args, "--cache")?),
            repo_root: crate::manifest::bench_dir().join(".."),
        },
        out: PathBuf::from(flag(args, "--out")?.unwrap_or_else(|| "e2e-results".into())),
        scratch: tempfile::tempdir().ctx("a scratch directory")?,
        hook_scripts: flag(args, "--hook-scripts")?.map(PathBuf::from),
        poll_every: Duration::from_millis(number(args, "--poll-ms", 2000.0)? as u64),
        per_run_usd: number(args, "--cap", cap::DEFAULT_CAP_USD)?,
        min_run_usd: number(args, "--min-run-usd", 0.5)?,
    })
}

/// One slot, inside the budget: skip it if already done, stop if the budget cannot cover a run,
/// otherwise run it with a cap of `min(per-run cap, what is left)` and charge the ledger what the
/// run cost.
fn run_slot(
    cli: &Cli,
    auth: &Authorized,
    ledger: &mut Ledger,
    run_id: &str,
    slot: &schedule::Slot,
    image: &str,
    statement: &str,
) -> Res<SlotResult> {
    let run_id = run_id.to_string();
    if cli.out.join(format!("{run_id}.json")).exists() {
        return Ok(SlotResult::Done);
    }
    let mut budget = Budget::new(auth);
    budget.record(ledger.spent_usd);
    if budget.remaining() < cli.min_run_usd.max(f64::MIN_POSITIVE) {
        return Ok(SlotResult::OutOfBudget {
            remaining_usd: budget.remaining(),
        });
    }
    let cap_usd = budget.cap_for_run(cli.per_run_usd)?;
    if cli.docker {
        validate_image(image)?;
    }
    let arm = arms::arm_for(&cli.tools, &slot.arm)?;
    let spec = RunSpec {
        run_id: run_id.clone(),
        arm: slot.arm.clone(),
        task: slot.task.clone(),
        image: image.to_string(),
        model: MODEL.to_string(),
        claude_version: cli.tools.claude_code.version.clone(),
        prompt: build_prompt(statement),
        timeout: cap::DEFAULT_TIMEOUT,
        cap_usd,
        poll_every: cli.poll_every,
    };
    // Reserve the cap before the run starts; see [`Ledger`].
    ledger.pending = Some(LedgerRun {
        run_id: run_id.clone(),
        usd: cap_usd,
    });
    ledger.save(&cli.out)?;
    let rec = run_one(&RunInputs {
        spec: &spec,
        arm: arm.as_ref(),
        fetcher: &cli.fetcher,
        executor: cli.exec.as_ref(),
        root: cli.scratch.path(),
        api_key: cli.key.as_deref(),
        hook_scripts: cli.hook_scripts.as_deref(),
    })?;
    ledger.pending = None;
    ledger.spent_usd += rec.usd;
    ledger.runs.push(LedgerRun {
        run_id,
        usd: rec.usd,
    });
    write_record(&cli.out, &rec, cli.key.as_deref())?;
    ledger.save(&cli.out)?;
    Ok(SlotResult::Ran(Box::new(rec)))
}

fn say(rec: &RunRecord, ledger: &Ledger, auth: &Authorized) {
    println!(
        "{} {}: {} in {:.0} s, ${:.4}{}; spent ${:.4} of ${:.2}",
        rec.arm,
        rec.task,
        if rec.failure { "FAILED" } else { "ran" },
        rec.wall_secs,
        rec.usd,
        rec.failure_reason
            .as_deref()
            .map(|r| format!(" ({r})"))
            .unwrap_or_default(),
        ledger.spent_usd,
        auth.budget_usd
    );
}

fn authorize_cli(args: &[String]) -> Res<(Option<String>, Authorized)> {
    // The guard comes first: nothing below is reached without the key and the budget.
    let key = std::env::var("ANTHROPIC_API_KEY").ok();
    let auth = authorize(key.as_deref(), flag(args, "--i-have-a-budget")?.as_deref())?;
    Ok((key, auth))
}

fn cmd_run(args: &[String]) -> Res<()> {
    let (key, auth) = authorize_cli(args)?;
    run_cmd(args, key, &auth)
}

fn run_cmd(args: &[String], key: Option<String>, auth: &Authorized) -> Res<()> {
    let cli = cli_from(args, key)?;
    let arm_id = need(args, "--arm")?;
    cli.tools.arm(&arm_id)?;
    let prompt_file = need(args, "--prompt-file")?;
    let statement = std::fs::read_to_string(&prompt_file).ctx(&format!("read {prompt_file}"))?;
    let image = flag(args, "--image")?.unwrap_or_else(|| "stub".into());
    let slot = schedule::Slot {
        index: 0,
        arm: arm_id,
        task: need(args, "--task")?,
        rep: 0,
    };
    let mut ledger = Ledger::load(&cli.out)?;
    let seed = flag(args, "--seed")?.unwrap_or_else(|| "single-run".into());
    match run_slot(
        &cli,
        auth,
        &mut ledger,
        &schedule::nonce(&seed, &slot, now_attempt()),
        &slot,
        &image,
        &statement,
    )? {
        SlotResult::Ran(rec) => say(&rec, &ledger, auth),
        SlotResult::Done => println!("already run"),
        SlotResult::OutOfBudget { remaining_usd } => {
            return Err(format!(
                "the budget cannot cover a run: ${remaining_usd:.2} left, a run needs at least ${:.2}",
                cli.min_run_usd
            ));
        }
    }
    Ok(())
}

fn cmd_run_schedule(args: &[String]) -> Res<()> {
    let (key, auth) = authorize_cli(args)?;
    schedule_cmd(args, key, &auth)
}

fn schedule_cmd(args: &[String], key: Option<String>, auth: &Authorized) -> Res<()> {
    let cli = cli_from(args, key)?;
    let tasks = Tasks::load_committed()?;
    let seed = flag(args, "--seed")?.unwrap_or_else(|| "lumen-p2-e2e".into());
    let sch = if args.iter().any(|a| a == "--pilot") {
        schedule::pilot(&seed, &cli.tools, &tasks)?
    } else if flag(args, "--arms")?.is_some() || flag(args, "--tasks")?.is_some() {
        let list = |name: &str| {
            flag(args, name)
                .map(|v| v.map(|v| v.split(',').map(str::to_string).collect::<Vec<_>>()))
        };
        let arms =
            list("--arms")?.unwrap_or_else(|| cli.tools.arm.iter().map(|a| a.id.clone()).collect());
        let task_ids = list("--tasks")?.unwrap_or_else(|| schedule::task_ids(&tasks));
        let reps = number(args, "--reps", schedule::MIN_REPS as f64)? as u32;
        schedule::build(&seed, &arms, &task_ids, reps)?
    } else {
        let suites = suites_of(args)?;
        let reps = number(args, "--reps", schedule::MIN_REPS as f64)? as u32;
        schedule::full(&seed, &cli.tools, &tasks, &suites, reps)?
    };
    let task_dir = PathBuf::from(need(args, "--task-dir")?);
    let mut ledger = Ledger::load(&cli.out)?;
    let (mut ran, mut done) = (0, 0);
    for slot in &sch.slots {
        let dir = task_dir.join(&slot.task);
        let statement = std::fs::read_to_string(dir.join("prompt.txt"))
            .ctx(&format!("read the prompt of task {}", slot.task))?;
        let image = match std::fs::read_to_string(dir.join("image")) {
            Ok(i) => i.trim().to_string(),
            Err(_) if !cli.docker => "stub".to_string(),
            Err(e) => return Err(format!("read the image of task {}: {e}", slot.task)),
        };
        match run_slot(
            &cli,
            auth,
            &mut ledger,
            &schedule::nonce(&seed, slot, 0),
            slot,
            &image,
            &statement,
        )? {
            SlotResult::Ran(rec) => {
                ran += 1;
                say(&rec, &ledger, auth);
            }
            SlotResult::Done => done += 1,
            SlotResult::OutOfBudget { remaining_usd } => {
                println!(
                    "stopping: ${remaining_usd:.2} of ${:.2} is left, less than the ${:.2} a run needs; \
                     {ran} run(s) this time, {done} already done, {} not started",
                    auth.budget_usd,
                    cli.min_run_usd,
                    sch.slots.len() - ran - done
                );
                return Ok(());
            }
        }
    }
    println!("schedule complete: {ran} run(s) this time, {done} already done");
    Ok(())
}

fn suites_of(args: &[String]) -> Res<Vec<schedule::Suite>> {
    Ok(match flag(args, "--suite")?.as_deref().unwrap_or("both") {
        "competitors" => vec![schedule::Suite::Competitors],
        "versions" => vec![schedule::Suite::Versions],
        "both" => vec![schedule::Suite::Competitors, schedule::Suite::Versions],
        other => return Err(format!("--suite {other:?}: competitors, versions or both")),
    })
}

/// A different attempt number each time a single run is started by hand, so two runs of one
/// arm and task never share a nonce (and so a warm cache prefix).
fn now_attempt() -> u32 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| (d.as_secs() % u32::MAX as u64) as u32)
        .unwrap_or(0)
}
