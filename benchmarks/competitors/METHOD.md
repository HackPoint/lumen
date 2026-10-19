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
- **Replay** (C2, `benchmarks/harness/src/competitors/replay.rs`, run with
  `benchmarks/run.sh competitors replay`): each item is given the tool call a model would have made
  (a file read and a JSON file of a repository: `Read`; `git grep` output: `Grep`; `git ls-tree`,
  `git show --stat` and a downloaded JSON document: `Bash` with that command; test and build logs:
  `Bash` with `cargo test`, `pytest`, `cargo build` or `tsc --noEmit`, exiting 101, 1, 101 or 2 when the log reports a failure), the
  same for every arm. Each arm is then driven through the mechanism it really uses, installed from
  its own pin into a scratch HOME inside a `Sandbox` (an allowlisted environment, so nothing of the
  user's HOME, ledger or `~/.claude` is visible), with telemetry off:
  - **none**: the raw bytes.
  - **Lumen 1.9.0** (the release asset, via `assets.rs`): a `Read` goes through the real
    `lumen-mcp hook intercept`; a source file of 300 lines or more is blocked (exit 2; the block
    message is part of what the model receives), then `smart_read` (the redirect, one round).
    That is Lumen's first answer. Then the follow-ups: one `recall_file` for each needle the
    first answer lacks, at the lines the needle is on, one round and its tokens each (see
    "Follow-ups" below). A log goes to `compress_logs` (CLAUDE.md's own routing; no hook sits on
    Bash output). Output Lumen has no tool for (shell, grep, JSON documents) passes through
    unchanged.
  - **squeez** and **RTK**: the hooks the tool's own installer wrote into the scratch
    `settings.json` (`squeez setup`; `rtk init -g --auto-patch`) are run, for the events and tool
    names their `matcher` selects, with the JSON Claude Code sends, in settings order. PreToolUse
    `updatedInput` replaces the call's input (squeez wraps a command in `squeez wrap`, and adds
    `limit` to a `Read` and `head_limit` to a `Grep`); the replaced command runs under `sh -c` with
    PATH shims first, which print the recorded stdout byte for byte and exit with the recorded
    status, so the wrapper sees the "real" output of the real command; a shim answers only the
    invocation it was made for and passes any other call (RTK asks `git rev-parse` first) to the
    real program, and every call is logged. A wrapper that never calls the recorded program is
    `n/a`, never a measurement. PostToolUse `updatedToolOutput` replaces the result. RTK registers
    only a PreToolUse hook on Bash, so a `Read` or `Grep` reaches it unchanged: that is its real
    behaviour, and the row says "left the input alone". Payload shapes, from squeez's own
    `hooks/posttooluse.sh` and `src/commands/compress_output.rs` at the pinned commit (which parse
    and emit them) and RTK's `hooks/claude/rtk-rewrite.sh`: PreToolUse
    `{session_id, transcript_path, cwd, permission_mode, hook_event_name, tool_name, tool_input,
    tool_use_id}` with `tool_input` `{file_path}` / `{command}` / `{pattern, output_mode, -n}`;
    PostToolUse adds `tool_response`: for `Read`
    `{"type":"text","file":{filePath,content,numLines,startLine,totalLines}}`, for `Grep`
    `{mode,numFiles,filenames,content,numLines}`, for `Bash` `{stdout,stderr,interrupted,isImage}`;
    a rewrite comes back as `hookSpecificOutput.updatedInput` / `.updatedToolOutput`. The shims are
    POSIX `sh`: on Windows these arms are `n/a`.
  - **Headroom**: `headroom proxy --port N --workers 1`, which is what `headroom wrap claude`
    starts, with `ANTHROPIC_TARGET_API_URL` pointed at a local echo upstream (`replay/echo.rs`,
    bound to 127.0.0.1) that keeps every request body. Each item is sent as the request Claude Code
    would make after the tool ran (prompt, the model's `tool_use`, the result as the newest
    `tool_result`); the `tool_result` in the body the proxy forwards is the output. The prompt
    carries a nonce, because the proxy's response cache is on by default and would answer a
    repeated request itself. The proxy runs with `HEADROOM_OFFLINE=1` (its own air-gap switch: it
    dials nothing but its upstream), `HF_HUB_OFFLINE=1` and `LITELLM_LOCAL_MODEL_COST_MAP=True`, so
    no connection leaves the machine, and the replay waits until its Kompress model has loaded
    (about a minute), since a request before that is forwarded uncompressed. Defaults otherwise:
    mode `cache`, which freezes earlier messages and never touches `Read`/`Glob` results
    (`excluded_tool`); `--mode token` is not measured. The model weights are the revisions
    Headroom 0.41.0 itself pins (`headroom.onnx_runtime._PINNED_REVISIONS`, read from the installed
    package; Kompress `b1563631b35b`), which is what a run at defaults asks the Hub for. `tools.toml`
    records exactly that revision (its first draft recorded the newest upload, `e00cc5e60ff7`, which
    this Headroom never loads), and the prefetch stops if the two differ.
  - **Caveman**: `n/a — changes the model's replies, not tool output (measured in Layer 2)`. That is
    its output-style skill. Its 4.0.x input-side proxy (`caveman-proxy`, installed by
    `cave setup --install`, binaries checksummed by the installer from release `bin-v4.0.1`)
    does compress tool output, but `cave setup --json` states that without the agent-side
    recovery MCP server (`caveman-mcp`) streaming requests pass through uncompressed, and Claude
    Code always streams; a faithful replay needs a Claude Code session that registers that server,
    which is Layer 2. Layer 1 does not guess at it.
  Installs are pre-fetched with `--prefetch` (the only step that uses the network): every asset is
  checked against its sha256 in `tools.toml` (and, for squeez and RTK, against the release's own
  pinned checksum file) before use, Headroom is installed with `uv` into the cache from its pinned
  wheel with `--exclude-newer` at the resolving date, and nothing is installed globally. `tools.toml`
  carries the macOS arm64 asset of each (added with C2, same releases, GitHub's digests).
- **Measure:** two columns for every arm.
  - *First answer only*: the tokens of what the arm hands the model for the call, and which needles
    it still states.
  - *With follow-ups*: the same after the follow-ups an ideal asker makes (below), with their
    tokens added, the needles stated then, and the extra rounds (round trips beyond the original
    call; Lumen's redirect to `smart_read` is one).
  Tokens are re-counted with Lumen's cl100k counter and converted with the calibration table
  (`lumen_core::calibration`, for `claude-sonnet-5-5`; a file's own text is converted as a file,
  anything an arm made from it as an answer, with the wider band; not an exact count of any
  model's tokenizer). The wall time is the replay itself: hooks, the command, the tool calls, the
  proxy round trip and the follow-ups; not installation, not Headroom's start-up. Claude Code's own
  wrapping of a result (the `cat -n` line numbers of a `Read`, the `Exit code N` line of a failed
  command) is added for no arm, so what differs between arms is what the arm did. A wrapper that
  asks for terser output by changing a command's flags (RTK's `pytest` and `cargo` wrappers) is
  shown the recorded, unchanged output, since the recording was made once.
- **Follow-ups.** A tool that drops something usually says how to get it back. For every arm whose
  recovery path works at the tool's documented defaults, the replay makes the same ideal
  follow-up: while a needle's fact is missing, one call through that path, one round and its
  tokens counted. An arm with no such path gets none, and the row says so:
  - Lumen: `recall_file` at the lines of each missing needle (the block message names it).
  - Headroom: the marker `Retrieve more: hash=<hex>` names `headroom_retrieve`, the MCP server
    `headroom wrap claude` registers unless `--no-mcp` (`headroom/cli/wrap.py`,
    `_setup_headroom_mcp`: "without this registration those markers point at nothing"). The
    replay asks the proxy's `/v1/retrieve`, which is what that tool calls, and counts the JSON the
    tool hands the model (retrieval is by hash and returns the whole original, so one call
    recovers every needle).
  - RTK: the marker `[full output: rtk recall <id>]` is a command the model runs through Bash;
    the recall store is written from the first install (`src/core/retriever.rs`, `tee.rs` at
    the pin). The replay runs the installed `rtk recall <id>` in the case's HOME.
  - squeez: none. Its marker names `squeez_retrieve`, an MCP tool; `squeez setup` registers no MCP
    server (README: `claude mcp add squeez -- squeez mcp` is a manual step; T0 records it as off by
    default), and the replay stops if the install did register one. The `squeez protocol` text
    that mentions the session file is itself delivered by that MCP server or a manual command.
  - Caveman: not replayed (above).
  The follow-up is ideal: the asker always knows exactly what it is missing and asks at once. That
  flatters every arm that has a path, and the first-answer column shows the figure without it.
- **Needles.** A needle states a fact; it counts as present when the fact is, not when the bytes
  are. The matcher (`fact_present`): whitespace is not part of a fact (both sides compared with
  all whitespace removed); a needle of several lines is a set, each line present in any order; for
  a `grep` item a hit grouped under its file's name (`path` then `LINE:text`) states the same fact
  as `path:LINE:text`. A changed number or word, a line that is gone, a hit under another file, or
  words in another order is a lost fact (the tests check both directions). A tool that rewrites
  text into words of its own loses the fact even if it means the same.
  The needles asked, by category and kind (`needle_kind`; 172 in all):

  | category | signature | error line | test name | value | other |
  |---|---:|---:|---:|---:|---:|
  | file reads (11 items) | 24 | | | | 32 |
  | grep (7) | 6 | | | 3 | 12 |
  | shell (6) | | | | 18 | |
  | JSON (9) | | | | 32 | |
  | test logs (11) | | 7 | 21 | | |
  | build logs (6) | | 17 | | | |
  | all | 30 | 24 | 21 | 53 | 44 |

  What an outline keeps (a definition line, and an import) is 17% of the needles overall, and in
  the only category an outline applies to, file reads, 43% are definitions and 57% are imports,
  comments, body lines and assertions (an outline keeps imports too, so the "other" share is not
  all out of its reach). A test checks that definitions stay under a quarter of the needles. The
  mix is therefore not tilted toward what an outline keeps, and the corpus is unchanged.
- **Row decision (C3):** the winner of a row is the tool with the fewest tokens among the tools
  that keep every needle, on the with-follow-ups column (its tokens include the follow-ups, and the
  extra rounds are shown beside it; the first-answer column is shown too). A tool that drops a
  needle does not win the row however few tokens it sends. Counts are deterministic, so equal counts are a tie; there is no noise band.

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
