# Competitor comparison: method

Lumen against squeez, RTK, Headroom and Caveman, and against Claude Code with none of them.
This file, `tools.toml` and `tasks.toml` are committed before any run. They fix what is
compared, on which tasks, with which versions, and how a row is decided.

## Honesty rule

- The task list, the arms, the pins and the decision rules below are committed first. They are
  not changed after a result exists. If a prompt turns out to be unjudgeable while the scoring
  is built (E2), it is amended in a commit before the pilot, with the reason in the message.
  After the pilot nothing in these three files changes; a harness bug found later is fixed in
  the harness, disclosed, and the affected runs are redone and both sets published.
- Every result is published, wins, ties, losses, n/a rows, failed and killed runs, and the
  pilot. Nothing is dropped, re-rolled or averaged away.
- Every row Lumen loses opens an issue in HackPoint/lumen naming the winner and why (evidence
  from the transcripts, not a guess).
- A tool that saves tokens but lowers task success does not win the row (Layer 2 row decision).
- No claim is published without a row that measures it. Counts of tokens are not claimed exact
  (they are cl100k re-counts, see below); savings are signed, a loss stays negative.

## Arms

`tools.toml` is the list: no Lumen, Lumen 1.9.0, squeez 1.48.18, RTK 1.0.1, Headroom 0.41.0,
Caveman 4.0.1, and for the version report Lumen 1.1.5, 1.6.0 and 1.8.0. Each is installed as its
own documentation says, at defaults, at the pinned version, from a pinned commit or a
checksummed release asset. No arm is paid: Headroom's Enterprise tier and Caveman Cloud ("Auto",
login) are not used. RTK's default binary is the Apache-2.0 CLI with the BUSL-1.1 engine; the
license permits free production use and none of the four forbids publishing a benchmark.

Not in scope: ojuschugh1/sqz (a different compressor) and reachingforthejack/rtk (a different tool
with RTK's name).

**The no-Lumen and Lumen 1.9.0 arms are shared.** They run once and are cited by both this report
and the version report (`benchmarks/versions/`), so the two never disagree about them.

**Lumen 1.1.5 stands in for 1.0.0 in E2E.** 1.0.0 shipped no Linux app asset, so it cannot run in
the Linux containers. 1.1.5 is the first version with a `.deb`. It is labelled as the stand-in
in every table; the other metrics of the version report still measure 1.0.0 on macOS and Windows.

**How the Lumen arms are configured.** There is no headless Setup, so the arm writes what Setup
writes: the `mcpServers.lumen` entry and the hooks, with Setup's matchers for that version (checked
against a golden produced by `setup.rs`'s own writers). The two hook scripts are the happy path of
Setup's scripts: the same `exec lumen-mcp hook ...` lines, without the fault report Setup's script
writes when `lumen-mcp` is missing. That branch cannot run here, because the sidecar sits next to
the script; the exit behaviour is the same.

**Containers.** Images are named by `@sha256:` digest (a tag can move between two runs) and run
with `--platform linux/amd64`. The cost cap counts the main thread and the subagents live, and a
run is also bounded by what is left of the owner's budget (`--i-have-a-budget`), which is kept in
`budget-ledger.json` beside the results.

## Deviation from defaults: telemetry

Every arm runs with its telemetry switched off (`telemetry_env` in `tools.toml`: `DO_NOT_TRACK=1`,
`HEADROOM_BEACON=off`, and for Lumen `LUMEN_UPDATE_CHECK=0`, which stops its update check;
Lumen sends no telemetry). Headroom and Caveman default to sending anonymous usage statistics;
RTK and squeez default to none. Switching it off does not change what any tool compresses, and a
benchmark must not send data from a run. This is the main deviation from "at defaults"; all of them are listed in this section so none
is a surprise.

The deviations from defaults, all of them: (1) telemetry off, above; (2) scripted `claude -p` starts Caveman off whatever the install did, so the
Caveman arm sets `CAVEMAN_DEFAULT_MODE=caveman` as Caveman's own documentation says. Without it the
arm would measure stock Claude Code. Headroom's Hugging Face weights are fetched once at a pinned
revision and the run is offline (`HF_HUB_OFFLINE=1`), so the arm does not depend on a moving model.

## Layer 1: replay (deterministic, no model, no API cost)

Question: for the same tool output, how many tokens reach the model, and does the answer survive?

- **Corpus** (C1, `benchmarks/competitors/corpus/`, recorded by `competitors record`): real
  outputs from public sources, in six categories: file reads by size bucket (under 300, 300-999,
  1000-2999 and 3000+ lines; from the Lumen, react-hook-form and pytest commits pinned in
  `benchmarks/README.md`, byte for byte from git's object store), shell output (`git ls-tree`,
  `git show --stat`), grep output (`git grep`), test logs (real pytest runs quoted in pytest's
  own documentation at the pinned commit, and real `cargo test` failures from the Rust book's
  listings at a pinned commit), build logs (rustc and tsc diagnostics from the
  compilers' own test baselines at pinned commits) and JSON (package registry, PyPI and GitHub
  API documents, plus JSON files from the pinned repositories). The logs are public and pinned
  rather than produced by running a build here, because a build at record time would depend on
  the network, the toolchain and the clock. Each item has needles: the facts a task needs (a
  definition line, a failing test and its message, an error code, a version and a checksum).
  The corpus is committed (at most 5 MB), every file pinned by sha256 in `manifest.toml`, with
  no home paths, user names or employer names in it (a documentation example's `/home/sweet/` is
  rewritten to `/work/` and the item says so). A download is checked against its pinned sha256
  before use, so a moved source stops the record instead of changing the corpus.
- **Replay** (C2): each tool is driven through the mechanism it really uses, on the recorded
  bytes. squeez and RTK: the PreToolUse Bash rewrite through PATH shims that replay the recorded
  stdout and exit code, and the PostToolUse `updatedToolOutput` path. Headroom: its proxy in
  front of a local echo upstream that captures the request body. Lumen: the Read block, then
  `smart_read` / `recall_file` / `compress_logs`. Caveman changes the model's output style and
  has no mechanism on tool output: its Layer 1 cells are `n/a` with that reason, never 0.
- **Measure:** tokens in what reaches the model, re-counted with Lumen's cl100k counter (a
  calibrated estimate, not an exact count of any model's tokenizer), and whether every needle is
  still present byte for byte.
- **Row decision (C3):** the winner of a row is the tool with the fewest tokens among the tools
  that keep every needle. A tool that drops a needle does not win the row however few tokens it
  sends. Counts are deterministic, so equal counts are a tie; there is no noise band.

## Layer 2: end to end

**What a run is.** A fresh container from the official SWE-bench Verified instance image (x86_64)
or the scenario's image, a fresh HOME. Claude Code at the version in `tools.toml`
(`DISABLE_AUTOUPDATER=1`), then the arm's install, then

```sh
claude -p "<problem statement + one fixed instruction line>" --model claude-sonnet-5-5 \
  --output-format stream-json --verbose --dangerously-skip-permissions \
  --append-system-prompt "run <uuid>"
```

inside the disposable container only (see Safety). The per-run `<uuid>` keeps the prompt cache
cold past the static prefix, so one arm's cache cannot warm another's; the first turn's
`cache_read` is reported. Timeout 45 minutes. Model pinned: `claude-sonnet-5-5`. Claude Code
pinned: 2.1.292.

**Tasks.** 13: 10 SWE-bench Verified instances drawn by seed from a pinned dataset revision
(`draw_tasks.py` reproduces the draw), and 3 Lumen scenarios (large-file exploration, log-heavy
debugging, multi-file refactor) from real upstream fixes. The list is in `tasks.toml` and does
not change. SWE-bench instances are public and may be in a model's training data; that
affects every arm equally and is stated in the report.

**Runs.** 3 runs per arm and task. The order of arms is a seeded rotation per task, so no arm
always runs first or in the same hour. The seed is recorded with the results.

**Pilot first.** 9 runs, one per arm, on the first instance of the draw: it checks every
install, records what each tool actually enabled (for example `caveman status`), and measures
cost per run. The pilot is published like any run and is not counted in the results. The full
runs start only after the owner has seen the re-estimate from the pilot.

**Cost cap.** The harness kills a run when its dollars reach $3 and records it as a failure; the
dollars it spent still count. A timeout is also a failure. Neither is dropped.

**Success.** The run's `git diff` becomes a predictions line and goes through
`swebench.harness.run_evaluation` (swebench and the dataset revision pinned; Docker). The 3
scenarios use the same shape: a pinned repo and commit, a Dockerfile, hidden FAIL_TO_PASS tests
the agent never sees (built and validated in E2: they fail at the base commit and pass with the
gold patch; an empty patch fails). Success is binary per run.

**Tokens and dollars.** Read from the Claude Code transcript, not from the tools' own claims:

- every `message.usage` in the session transcript and in `subagents/agent-*.jsonl`, so subagent
  work is counted;
- deduplicated by `message.id`: a streamed message is written several times, and counts once. If
  repeated entries of one id disagree, the one with the greatest `output_tokens` is used and the
  disagreement is counted and reported;
- input, cache creation (5 minute and 1 hour, separately), cache read and output;
- cross-checked against the stream-json result's totals; a difference of more than 2% is flagged
  in the report;
- dollars at the consuming model's price, from `crates/lumen-core/src/rates.rs` (`PRICES`), never
  from a tool's own estimate. Rounds are API requests. Wall time is spawn to exit.

**Row decision (win, tie, loss, n/a).** For one task row, with 3 runs per arm:

1. An arm *qualifies* if its success rate (resolved runs of 3) is at least the no-Lumen arm's.
2. Among qualifying arms, the one with the **cheapest median dollars** is the best.
3. Arms whose **min-max dollar ranges overlap** (closed intervals) with the best arm's range are
   tied with it. Only the best arm and arms it overlaps are winners; a tie is reported as a tie,
   never as a win.
4. A tool that saves tokens but lowers task success does not win the row. It is shown with its
   dollars and marked "cheaper, less successful".

Lumen's result on a row: **win** if it is the best arm and no other arm's range overlaps it;
**tie** if it is the best arm or overlaps the best arm; **loss** if it does not qualify or some
qualifying arm is cheaper without an overlap; **n/a** only where the cell cannot exist (an
install that cannot run on the platform, with the reason), never for a failure, a timeout or a
cap kill. The report also gives the table-wide counts and the median ratio against no Lumen.

**Cost estimate.** About $0.90 a run at the centre and $1.45 at the high end, on Sonnet 5.5:
9 arms x 13 tasks x 3 runs = 351 runs, about $316 expected and $509 high, with the $3 cap giving a
hard ceiling near $1,050. The pilot replaces these with a measured figure.

## Safety and sandbox

- **Containers only.** `--dangerously-skip-permissions` and every competitor tool run inside a
  disposable container with a fresh HOME, no host mounts, deleted after the run. No competitor
  tool is installed on the host. No arm ever touches the owner's `~/.claude`, `~/.claude.json`,
  `~/.claude/settings.json` or any real Lumen ledger.
- **Credential.** A dedicated API workspace key with a spend limit, passed as an environment
  variable by the owner locally. It is never a CI secret, never written to an image layer, a
  transcript, a result file or a log; results are scanned for it before commit.
- **Lumen binaries** (the daemon, `lumen-mcp`, a release asset) never run on the host without
  `HOME` and `LUMEN_DB` pointing at scratch directories; a bare daemon starts on the real ledger.
  In the E2E arms the daemon is not started at all.
- **Downloads are untrusted.** Each is verified against the pinned sha256 before use, unpacked
  into a fresh directory, and an install script is run only if its own sha256 in `tools.toml`
  matches; scripts that fetch `latest` are not run.
- **Network** in a run is limited to the Anthropic API and the package mirrors the pinned installs
  need; no telemetry endpoint is a run's business (see the deviation above).
- **Public artifacts** carry no employer names or identifiers, no usernames or home paths, and no
  AI-tool authorship lines.
