#!/usr/bin/env python3
"""Calibrate Lumen's token counts against Claude's, per content type and Claude model.

Lumen counts tokens with `cl100k_base` (OpenAI's tokenizer, through `lumen-tok`). Claude
uses its own. This measures the ratio between the two on a pinned public corpus, so a
figure built on the proxy can carry a stated error instead of being called exact.

How Claude's count is measured, with no API key. Claude Code's print mode reports the
provider's `usage` for the request it sent. The same prompt is sent twice, once with a
sample between the markers and once with nothing between them; the difference of the two
prompt totals (input + cache creation + cache read) is what Claude charged for the sample.
The fixed part of the request (Claude Code's identity line, a one-line system prompt, an
account reminder and an environment block, 740 to 1,000 tokens by model) cancels out, and the run
checks that it does: the empty prompt is sent three times per model before any sample and
again every ten samples, and every one of those totals must be equal. Each sample is sent
twice and the two totals must agree; a third send settles a disagreement or the sample is
dropped and reported, as is a sample the model stopped on safety grounds and Claude Code
answered with another model: at most one such sample per model and one per content type, or the
run aborts (FALLBACKS_PER_MODEL, FALLBACKS_PER_TYPE). cl100k is counted the same way, as the difference of the two
wrapped prompts, so the boundary tokens where the sample meets the markers cancel on both
sides of the ratio.

Isolation. Every call runs with `--no-session-persistence` and is checked afterwards: no
transcript may appear under ~/.claude/projects, because a running Lumen daemon ingests
every transcript there into its ledger and a calibration call would be counted as the
owner's own use. No tools, no MCP servers, no hooks, no settings files, no slash commands,
no session-title call, and the attribution line is off (its version suffix differed
between two requests of one run, so it could differ between the two prompts). Two more
things moved the fixed part between consecutive calls during development, both found by
logging request bodies through a local proxy (not part of this script): Claude Code writes
a per-session path into the environment block, so the session id is fixed, and it sometimes
adds a `<total_tokens>` line (23 or 24 tokens), so its internal `totalTokensReminder`
setting is off.

Read-only except its output: the corpus cache (outside the repository) and the files in
results/ next to this script. Commands, in order:

    cargo build --release --locked -p lumen-mcp --bin lumen-tok
    python3 benchmarks/calibration/calibrate.py fetch
    python3 benchmarks/calibration/calibrate.py validate
    python3 benchmarks/calibration/calibrate.py run
    python3 benchmarks/calibration/calibrate.py summarize

`run --models claude-haiku-5-5` re-runs one model. `summarize` joins the models' call logs into
results/samples.tsv and results/table.tsv, and refuses if Claude Code has updated itself
since `validate` recorded its version.
"""

from __future__ import annotations

import argparse
import datetime
import hashlib
import json
import os
import re
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
import urllib.request
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
CORPUS = HERE / "corpus.tsv"
RESULTS = HERE / "results"
DEFAULT_CACHE = Path(tempfile.gettempdir()) / "lumen-calibration-cache"
DEFAULT_TOK = ROOT / "target" / "release" / "lumen-tok"

# Every model measured, by the id Claude Code accepts. The 5.5 generation is the current one;
# the rest are older generations that still turn up in a ledger.
MODELS = (
    "claude-opus-5-5", "claude-sonnet-5-5", "claude-haiku-5-5",
    "claude-fable-5", "claude-opus-5", "claude-sonnet-5",
    "claude-opus-4-8", "claude-opus-4-7", "claude-opus-4-6", "claude-sonnet-4-6",
    "claude-opus-4-5-20251101", "claude-haiku-4-5-20251001",
)
PREFIX = "Reply with the single word OK.\n<sample>\n"
SUFFIX = "\n</sample>\n"
SYSTEM_PROMPT = "You answer with one word."
# Fixed, so the environment block Claude Code adds is the same on every call (see above).
SESSION_ID = "00000000-0000-4000-8000-00000000ca1b"
# How often the empty prompt is re-sent during a run to catch drift in the fixed part.
BASELINE_EVERY = 10
# A sample may cost at most this many cl100k tokens, so Claude's count stays under ~30k.
MAX_CL100K = 22_000
# How many samples may fall back (the model stopped on safety grounds and Claude Code answered
# with another) before a run is not trusted: one per model, and one per content type over every
# model. A fallen-back sample is dropped for its model, so the rows it would be in rest on fewer
# texts than the corpus has; one is an accident, more is a pattern, and the run aborts rather
# than publish rows that quietly lost their samples. A Rust test holds results/run.json to the
# same caps (calibration::tests::the_run_drops_no_more_safety_fallbacks_than_the_runner_allows).
FALLBACKS_PER_MODEL = 1
FALLBACKS_PER_TYPE = 1


# ── corpus ──────────────────────────────────────────────────────────────────────────


def corpus() -> list[dict]:
    rows = []
    with open(CORPUS, encoding="utf-8") as fh:
        header = fh.readline().rstrip("\n").split("\t")
        for line in fh:
            if line.strip():
                rows.append(dict(zip(header, line.rstrip("\n").split("\t"))))
    return rows


def raw_url(row: dict) -> str:
    repo = row["repository"].removeprefix("https://github.com/")
    path = urllib.request.quote(row["path"])
    return f"https://raw.githubusercontent.com/{repo}/{row['commit']}/{path}"


def git_blob_id(data: bytes) -> str:
    """The id git gives this content, so a download is checked against the pinned tree."""
    return hashlib.sha1(b"blob %d\0" % len(data) + data).hexdigest()


def cut(text: str, lines: str) -> str:
    if lines == "all":
        return text
    first, last = (int(n) for n in lines.split("-"))
    return "".join(text.splitlines(keepends=True)[first - 1 : last])


def sample_path(cache: Path, sid: str) -> Path:
    return cache / "samples" / f"{sid}.txt"


def count(tok: Path, text: str) -> int:
    out = subprocess.run(
        [str(tok)], input=text.encode("utf-8"), capture_output=True, check=True
    )
    return int(out.stdout.decode().strip())


def wrap(content: str) -> str:
    return PREFIX + content + SUFFIX


def cmd_fetch(args: argparse.Namespace) -> None:
    """Download each pinned file, check it is the pinned blob, cut it, count it."""
    tok = Path(args.tok)
    if not tok.is_file():
        sys.exit(f"{tok} not found: cargo build --release --locked -p lumen-mcp --bin lumen-tok")
    cache = Path(args.cache)
    (cache / "blobs").mkdir(parents=True, exist_ok=True)
    (cache / "samples").mkdir(parents=True, exist_ok=True)
    empty = count(tok, wrap(""))
    out = [
        "id\tcontent_type\trepository\tcommit\tpath\tlines\tbytes\tcl100k\tcl100k_wrapped"
    ]
    for row in corpus():
        blob = cache / "blobs" / row["blob"]
        if not blob.is_file():
            with urllib.request.urlopen(raw_url(row), timeout=120) as resp:
                data = resp.read()
            got = git_blob_id(data)
            if got != row["blob"]:
                sys.exit(f"{row['id']}: {raw_url(row)} is blob {got}, pinned {row['blob']}")
            blob.write_bytes(data)
        content = cut(blob.read_bytes().decode("utf-8"), row["lines"])
        sample_path(cache, row["id"]).write_text(content, encoding="utf-8")
        n = count(tok, content)
        wrapped = count(tok, wrap(content)) - empty
        if n > MAX_CL100K:
            sys.exit(f"{row['id']}: {n} cl100k tokens, over the {MAX_CL100K} cap")
        out.append(
            "\t".join(
                [row["id"], row["content_type"], row["repository"], row["commit"], row["path"],
                 row["lines"], str(len(content.encode("utf-8"))), str(n), str(wrapped)]
            )
        )
        print(f"{row['id']:<10} {n:>6} cl100k  {row['path']}")
    RESULTS.mkdir(exist_ok=True)
    (RESULTS / "counts.tsv").write_text("\n".join(out) + "\n", encoding="utf-8")


# ── calls ───────────────────────────────────────────────────────────────────────────


class Abort(Exception):
    pass


class Fallback(Exception):
    """The requested model stopped on safety grounds and Claude Code answered with another.

    The usage is then the other model's, so the sample has no count for the requested one. It
    is logged and dropped for that model, and the run goes on: one sample a model will not
    answer says nothing about the method.
    """


class Isolation:
    """Proves no call left a transcript where the Lumen daemon would ingest it."""

    def __init__(self, cwd: Path):
        self.projects = Path.home() / ".claude" / "projects"
        self.cwd = cwd
        self.started = time.time()
        self.before = set(self._transcripts())

    def _transcripts(self) -> list[Path]:
        return list(self.projects.rglob("*.jsonl")) if self.projects.is_dir() else []

    def check_quick(self) -> None:
        # Claude Code names a transcript after its session id and its folder after the cwd.
        if any(self.projects.rglob(f"{SESSION_ID}*")):
            raise Abort(f"a transcript for session {SESSION_ID} appeared under {self.projects}")
        tag = re.sub(r"[^A-Za-z0-9]", "-", self.cwd.name)
        if self.projects.is_dir() and any(tag in p.name for p in self.projects.iterdir()):
            raise Abort(f"a project folder for {self.cwd} appeared under {self.projects}")

    def check_full(self) -> dict:
        """Every transcript new or written since the run began, read for a calibration record."""
        self.check_quick()
        new = [p for p in self._transcripts() if p not in self.before]
        touched = [p for p in self._transcripts() if p.stat().st_mtime >= self.started]
        for p in set(new) | set(touched):
            with open(p, encoding="utf-8", errors="replace") as fh:
                for line in fh:
                    try:
                        rec = json.loads(line)
                    except ValueError:
                        continue
                    if not isinstance(rec, dict):
                        continue
                    if rec.get("sessionId") == SESSION_ID or rec.get("cwd") == str(self.cwd):
                        raise Abort(f"{p} holds a record of a calibration call")
        return {"transcripts_new": len(new), "transcripts_written": len(touched),
                "calibration_records_found": 0}


def claude(model: str, prompt: str, cwd: Path, iso: Isolation) -> dict:
    cmd = [
        "claude", "-p", "--output-format", "json",
        "--no-session-persistence", "--session-id", SESSION_ID, "--name", "lumen-calibration",
        "--safe-mode", "--setting-sources", "",
        "--settings", '{"disableAllHooks":true,"totalTokensReminder":"off"}',
        "--tools", "", "--strict-mcp-config", "--mcp-config", '{"mcpServers":{}}',
        "--disable-slash-commands", "--prompt-suggestions", "false",
        "--system-prompt", SYSTEM_PROMPT, "--model", model, "--effort", "low",
    ]
    env = dict(os.environ, CLAUDE_CODE_ATTRIBUTION_HEADER="0")
    proc = subprocess.run(
        cmd, input=prompt.encode("utf-8"), capture_output=True, cwd=cwd, env=env, timeout=600
    )
    iso.check_quick()
    try:
        d = json.loads(proc.stdout)
    except ValueError:
        raise Abort(f"claude exited {proc.returncode}: {proc.stderr.decode()[-400:]}")
    if d.get("is_error") or d.get("subtype") != "success":
        raise Abort(f"claude reported an error: {d.get('result')!r} {d.get('api_error_status')}")
    u = d["usage"]
    if d.get("safety_stops") and len(d["modelUsage"]) > 1 and model in d["modelUsage"]:
        raise Fallback(f"{model} stopped {d['safety_stops']} time(s); answered by "
                       f"{[m for m in d['modelUsage'] if m != model]}")
    # One request, one turn: a second request would put another prompt into the total.
    if d.get("num_turns") != 1 or len(u.get("iterations", [])) != 1 or len(d["modelUsage"]) != 1:
        raise Abort(f"expected one request, got turns={d.get('num_turns')} "
                    f"iterations={len(u.get('iterations', []))} models={list(d['modelUsage'])}")
    return {
        "model_id": next(iter(d["modelUsage"])),
        "input": u["input_tokens"],
        "cache_creation": u["cache_creation_input_tokens"],
        "cache_read": u["cache_read_input_tokens"],
        "total": (u["input_tokens"] + u["cache_creation_input_tokens"]
                  + u["cache_read_input_tokens"]),
        "reply": (d.get("result") or "").strip()[:20],
    }


class Log:
    """Every call, appended as it returns, so an aborted run still shows what it saw."""

    COLUMNS = ("time", "kind", "id", "attempt", "model_id", "input", "cache_creation",
               "cache_read", "total", "reply")

    def __init__(self, path: Path):
        self.path = path
        self.fh = open(path, "w", encoding="utf-8")
        self.fh.write("\t".join(self.COLUMNS) + "\n")

    def add(self, kind: str, sid: str, attempt: int, r: dict) -> None:
        now = datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
        vals = {"time": now, "kind": kind, "id": sid, "attempt": attempt, **r}
        self.fh.write("\t".join(str(vals[c]) for c in self.COLUMNS) + "\n")
        self.fh.flush()


def baseline(model: str, cwd: Path, iso: Isolation, log: Log, times: int, delay: float) -> int:
    totals = []
    for i in range(times):
        r = claude(model, wrap(""), cwd, iso)
        log.add("baseline", "-", i + 1, r)
        totals.append(r["total"])
        time.sleep(delay)
    if len(set(totals)) != 1:
        raise Abort(f"{model}: the empty prompt cost {totals}; the fixed part is not fixed")
    return totals[0]


def measure(model: str, content: str, sid: str, cwd: Path, iso: Isolation, log: Log,
            delay: float, kind: str = "sample") -> list[int]:
    """Send one prompt until two totals agree (at most three sends). Returns the totals."""
    totals: list[int] = []
    for attempt in (1, 2, 3):
        try:
            r = claude(model, wrap(content), cwd, iso)
        except Fallback as e:
            if kind != "sample":
                raise Abort(f"during validation: {e}")
            log.add("fallback", sid, attempt, {"model_id": model, "input": "", "cache_creation": "",
                                               "cache_read": "", "total": "", "reply": str(e)})
            time.sleep(delay)
            return []
        log.add(kind, sid, attempt, r)
        totals.append(r["total"])
        time.sleep(delay)
        if attempt >= 2 and any(totals.count(t) >= 2 for t in totals):
            break
    return totals


def fallback_excess(fell_back: dict[str, list[str]], type_of: dict[str, str]) -> str | None:
    """Why `fell_back` (model -> the samples it fell back on) is past the caps, or None."""
    for model, ids in sorted(fell_back.items()):
        if len(ids) > FALLBACKS_PER_MODEL:
            return (f"{model} fell back on {len(ids)} samples {sorted(ids)}; at most "
                    f"{FALLBACKS_PER_MODEL} may")
    by_type: dict[str, list[str]] = {}
    for model, ids in fell_back.items():
        for sid in ids:
            by_type.setdefault(type_of[sid], []).append(f"{sid}@{model}")
    for t, hits in sorted(by_type.items()):
        if len(hits) > FALLBACKS_PER_TYPE:
            return (f"{t} fell back {len(hits)} times {sorted(hits)}; at most "
                    f"{FALLBACKS_PER_TYPE} may")
    return None


def agreed(totals: list[int]) -> int | None:
    for t in totals:
        if totals.count(t) >= 2:
            return t
    return None


def workdir() -> Path:
    # A fresh, empty directory: nothing in it for Claude Code to attach or discover.
    return Path(tempfile.mkdtemp(prefix="lumen-calibration-"))


def load_samples(cache: Path) -> dict[str, str]:
    out = {}
    for row in corpus():
        p = sample_path(cache, row["id"])
        if not p.is_file():
            sys.exit(f"{p} missing: run `calibrate.py fetch` first")
        out[row["id"]] = p.read_text(encoding="utf-8")
    return out


def cmd_validate(args: argparse.Namespace) -> None:
    """The method's three premises, per model, before any number is trusted."""
    samples = load_samples(Path(args.cache))
    a, b = samples[args.pair[0]], samples[args.pair[1]]
    cwd = workdir()
    iso = Isolation(cwd)
    RESULTS.mkdir(exist_ok=True)
    # One file for every validation, added to model by model, so validating one more model
    # keeps what the others showed. Two Claude Code versions in one file would be two runs.
    path = RESULTS / "validation.json"
    report: dict = json.loads(path.read_text()) if path.is_file() else {"models": {}}
    version = claude_version()
    if report.setdefault("claude_code", version) != version:
        sys.exit(f"validation.json is from Claude Code {report['claude_code']}, this is {version}")
    if isinstance(report.get("isolation"), dict):
        report["isolation"] = [report["isolation"]]
    try:
        for model in args.models:
            log = Log(RESULTS / f"validate-{model}.tsv")
            # (a) The same prompt gives the same total: five sends of the empty prompt.
            base = baseline(model, cwd, iso, log, 5, args.delay)
            # (b) A sample's delta is stable: each sent twice, both totals must agree.
            ta = measure(model, a, args.pair[0], cwd, iso, log, args.delay, "validate")
            tb = measure(model, b, args.pair[1], cwd, iso, log, args.delay, "validate")
            # (c) The overhead is constant: tiny contents cost a few tokens, and two samples
            # sent together cost what they cost apart, plus the newline between them.
            tiny = {t: measure(model, t, f"tiny:{t}", cwd, iso, log, args.delay, "validate")
                    for t in ("x", "hello world")}
            tab = measure(model, a + "\n" + b, "+".join(args.pair), cwd, iso, log, args.delay,
                          "validate")
            da, db, dab = (agreed(t) - base for t in (ta, tb, tab))
            report["models"][model] = {
                "baseline_totals": [base] * 5,
                "pair": {args.pair[0]: ta, args.pair[1]: tb},
                "tiny_deltas": {t: [x - base for x in v] for t, v in tiny.items()},
                "additivity": {"a": da, "b": db, "a_newline_b": dab,
                               "a_plus_b_minus_together": da + db - dab},
            }
            print(json.dumps({model: report["models"][model]}))
        report.setdefault("isolation", []).append(
            {"models": args.models, **iso.check_full()})
    finally:
        shutil.rmtree(cwd, ignore_errors=True)
    path.write_text(json.dumps(report, indent=2) + "\n")


def cmd_run(args: argparse.Namespace) -> None:
    samples = load_samples(Path(args.cache))
    rows = corpus()
    cwd = workdir()
    iso = Isolation(cwd)
    RESULTS.mkdir(exist_ok=True)
    type_of = {r["id"]: r["content_type"] for r in rows}
    fell_back: dict[str, list[str]] = {}
    try:
        for model in args.models:
            log = Log(RESULTS / f"calls-{model}.tsv")
            base = baseline(model, cwd, iso, log, 3, args.delay)
            print(f"{model}: empty prompt {base} tokens")
            for i, row in enumerate(rows, 1):
                totals = measure(model, samples[row["id"]], row["id"], cwd, iso, log, args.delay)
                t = agreed(totals)
                print(f"{model} {row['id']:<10} {totals} -> "
                      f"{'fell back, dropped' if not totals else 'disagree' if t is None else t - base}")
                if not totals:
                    fell_back.setdefault(model, []).append(row["id"])
                    excess = fallback_excess(fell_back, type_of)
                    if excess:
                        raise Abort(f"too many safety fallbacks: {excess}")
                if i % BASELINE_EVERY == 0 or i == len(rows):
                    again = baseline(model, cwd, iso, log, 1, args.delay)
                    if again != base:
                        raise Abort(f"{model}: the empty prompt drifted from {base} to {again}")
        check = {"models": args.models, **iso.check_full()}
        print(json.dumps(check))
        # Kept beside the calls, so the transcript check is part of the evidence.
        path = RESULTS / "isolation.json"
        done = json.loads(path.read_text()) if path.is_file() else []
        path.write_text(json.dumps(done + [check], indent=2) + "\n")
    finally:
        shutil.rmtree(cwd, ignore_errors=True)


def claude_version() -> str:
    out = subprocess.run(["claude", "--version"], capture_output=True, check=True)
    return out.stdout.decode().split()[0]


# ── summary ─────────────────────────────────────────────────────────────────────────


def read_tsv(path: Path) -> list[dict]:
    with open(path, encoding="utf-8") as fh:
        header = fh.readline().rstrip("\n").split("\t")
        return [dict(zip(header, line.rstrip("\n").split("\t"))) for line in fh if line.strip()]


def quantile(xs: list[float], q: float) -> float:
    """Linear interpolation between closest ranks (numpy's default), so p50 is the median."""
    s = sorted(xs)
    pos = (len(s) - 1) * q
    lo = int(pos)
    hi = min(lo + 1, len(s) - 1)
    return s[lo] + (s[hi] - s[lo]) * (pos - lo)


def base_id(model_id: str) -> str:
    """The id a table row is keyed by: a dated or `[1m]` variant is its base model."""
    return re.sub(r"-\d{8}$", "", model_id.lower().removesuffix("[1m]"))


def model_deltas(path: Path) -> tuple[str, dict[str, int], dict]:
    """Claude's delta per sample from one model's call log, its base id, and its checks."""
    calls = read_tsv(path)
    fell_back = sorted({c["id"] for c in calls if c["kind"] == "fallback"})
    ids = {c["model_id"] for c in calls}
    if len(ids) != 1:
        sys.exit(f"{path.name}: more than one model answered: {sorted(ids)}")
    bases = [int(c["total"]) for c in calls if c["kind"] == "baseline"]
    if len(set(bases)) != 1:
        sys.exit(f"{path.name}: baselines differ {sorted(set(bases))}")
    base = bases[0]
    by_id: dict[str, list[int]] = {}
    for c in calls:
        if c["kind"] == "sample":
            by_id.setdefault(c["id"], []).append(int(c["total"]))
    deltas = {}
    dropped = []
    third = []
    for sid, totals in by_id.items():
        t = agreed(totals)
        if t is None:
            dropped.append(sid)
        else:
            deltas[sid] = t - base
        if len(totals) > 2:
            third.append({"id": sid, "totals": totals})
    model_id = ids.pop()
    checks = {"model_id": model_id, "as_of": max(c["time"] for c in calls)[:10],
              "baseline": base, "baseline_sends": len(bases), "samples": len(deltas),
              "needed_a_third_send": third, "dropped": dropped,
              "fell_back_to_another_model": fell_back,
              "first_two_sends_agreed": sum(1 for v in by_id.values() if v[0] == v[1])}
    return base_id(model_id), deltas, checks


def stats(xs: list[float]) -> dict:
    return {"median": round(quantile(xs, 0.5), 3), "p10": round(quantile(xs, 0.1), 3),
            "p90": round(quantile(xs, 0.9), 3), "min": round(min(xs), 3),
            "max": round(max(xs), 3)}


def cmd_summarize(args: argparse.Namespace) -> None:
    counts = read_tsv(RESULTS / "counts.tsv")
    measured = {}
    for path in sorted(RESULTS.glob("calls-*.tsv")):
        mid, deltas, checks = model_deltas(path)
        if mid in measured:
            sys.exit(f"{path.name}: {mid} is measured twice")
        measured[mid] = (deltas, checks)
    # The caps over every model's run, which `run` can only check within one invocation.
    excess = fallback_excess({m: c["fell_back_to_another_model"] for m, (_, c) in measured.items()},
                             {r["id"]: r["content_type"] for r in counts})
    if excess:
        sys.exit(f"too many safety fallbacks: {excess}")
    known = [base_id(m) for m in MODELS]
    models = sorted(measured, key=lambda m: (known.index(m) if m in known else len(known), m))
    expected = {r["id"] for r in counts}
    for m in models:
        if set(measured[m][0]) != expected:
            missing = sorted(expected - set(measured[m][0]))
            print(f"warning: {m} has no agreed count for {missing}", file=sys.stderr)

    # The joined raw table: one row per sample, Claude's count per model.
    head = ["id", "content_type", "repository", "commit", "path", "lines", "bytes",
            "cl100k", "cl100k_wrapped"] + [f"claude:{m}" for m in models]
    lines = ["\t".join(head)]
    ratios: dict[tuple[str, str], dict[str, float]] = {}
    for r in counts:
        vals = [r[h] for h in head[:9]]
        for m in models:
            d = measured[m][0].get(r["id"])
            vals.append("" if d is None else str(d))
            if d is not None:
                ratio = d / int(r["cl100k_wrapped"])
                for key in ((r["content_type"], m), ("pooled", m), (r["content_type"], "all"),
                            ("pooled", "all")):
                    ratios.setdefault(key, {})[f"{r['id']}@{m}"] = ratio
        lines.append("\t".join(vals))
    (RESULTS / "samples.tsv").write_text("\n".join(lines) + "\n", encoding="utf-8")

    # One row per content type and model, the pooled row per model, and the same pooled over
    # every model. `n` counts distinct samples and `models` the models they were sent to, so
    # a row over all models does not count one text once per model.
    types = sorted({r["content_type"] for r in counts}) + ["pooled"]
    table = []
    for t in types:
        for m in models + ["all"]:
            got = ratios.get((t, m), {})
            if got:
                texts = {k.split("@")[0] for k in got}
                k = len({key.split("@")[1] for key in got})
                table.append({"content_type": t, "model": m, "n": len(texts), "models": k,
                              **stats(list(got.values()))})
    cols = ("content_type", "model", "n", "models", "median", "p10", "p90", "min", "max")
    tsv = ["\t".join(cols)] + ["\t".join(str(row[c]) for c in cols) for row in table]
    (RESULTS / "table.tsv").write_text("\n".join(tsv) + "\n", encoding="utf-8")

    # The version the calls went through: validate records it, and a Claude Code that has
    # updated itself since may tokenize or wrap differently, so a mismatch is a re-run.
    validated = json.loads((RESULTS / "validation.json").read_text())["claude_code"]
    if claude_version() != validated:
        sys.exit(f"Claude Code is now {claude_version()}, the run went through {validated}")
    meta = {
        "as_of": max(measured[m][1]["as_of"] for m in models),
        "claude_code": validated,
        "models": {m: measured[m][1] for m in models},
    }
    (RESULTS / "run.json").write_text(json.dumps(meta, indent=2) + "\n")

    print("| content type | model | n | models | median | p10 | p90 |")
    print("|---|---|---:|---:|---:|---:|---:|")
    for row in table:
        print(f"| {row['content_type']} | {row['model']} | {row['n']} | {row['models']} "
              f"| {row['median']:.3f} | {row['p10']:.3f} | {row['p90']:.3f} |")
    print()
    print("// For crates/lumen-core/src/calibration.rs (ratios x 1000): the model consts, then TABLE.")
    names = {"ts_js": "TsJs"}
    const = {m: m.removeprefix("claude-").replace("-", "_").upper() for m in models}
    for m in models:
        print(f'const {const[m]}: Model = Model("{m}");')
    for row in table:
        t, m = row["content_type"], row["model"]
        t = names.get(t, t.capitalize())
        ratios = "[" + ", ".join(str(round(row[k] * 1000))
                                 for k in ("median", "p10", "p90", "min", "max")) + "]"
        if t == "Pooled" and m == "all":
            print(f"    everything({row['n']}, {row['models']}, {ratios}),")
        elif t == "Pooled":
            print(f"    pooled_for({const[m]}, {row['n']}, {ratios}),")
        elif m == "all":
            print(f"    all_models({t}, {row['n']}, {row['models']}, {ratios}),")
        else:
            print(f"    cell({t}, {const[m]}, {row['n']}, {ratios}),")


def main() -> None:
    p = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    p.add_argument("--cache", default=str(DEFAULT_CACHE), help="corpus cache, outside the repo")
    sub = p.add_subparsers(dest="cmd", required=True)
    f = sub.add_parser("fetch", help="download, verify and count the pinned samples")
    f.add_argument("--tok", default=str(DEFAULT_TOK), help="the lumen-tok binary")
    for name, fn in (("validate", cmd_validate), ("run", cmd_run)):
        s = sub.add_parser(name)
        s.add_argument("--models", type=lambda v: v.split(","), default=list(MODELS))
        s.add_argument("--delay", type=float, default=1.5, help="seconds between calls")
        s.set_defaults(fn=fn)
    sub.choices["validate"].add_argument("--pair", nargs=2, default=["rust-02", "prose-03"])
    s = sub.add_parser("summarize", help="join the models' runs into the tables")
    f.set_defaults(fn=cmd_fetch)
    s.set_defaults(fn=cmd_summarize)
    args = p.parse_args()
    if Path(args.cache).resolve().is_relative_to(ROOT):
        sys.exit("the cache must be outside the repository")
    try:
        args.fn(args)
    except (Abort, Fallback) as e:
        sys.exit(f"ABORTED: {e}")


if __name__ == "__main__":
    main()
