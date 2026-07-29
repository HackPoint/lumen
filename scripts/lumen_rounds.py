#!/usr/bin/env python3
"""Measure the OBSERVED cost of the round following each tool call.

Read-only. Touches no database and writes nothing.

Why a replay rather than a SQL join: `read_events` records events at second
precision and, for rows predating 1.1.5, without a session id — so with several
sessions running concurrently there is no way to say which turn followed which
read. The transcript can say it exactly, because it carries an explicit chain:

    assistant record  emits a tool_use   -> has `uuid`
    user record       carries the result -> has `sourceToolAssistantUUID` = that uuid
    assistant record  the next round     -> has `parentUuid` = the user record's uuid
                                            and the `usage` we want

`attributionMcpTool` is deliberately not used. It is sticky rather than per-turn:
in one session 508 records carry `attributionMcpTool = recall_file` with no
tool_use at all, and 384 carry it while the actual tool was Bash.

One further correction the data forces: Claude Code writes one JSONL line per
content block and repeats the whole `usage` object on each, so a session with 1,431
assistant lines holds only 722 distinct `message.id`. Counting lines would inflate
every figure ~2x. Rounds are therefore deduplicated by `message.id`, which is what
makes one row equal one billable request.
"""

from __future__ import annotations

import collections
import glob
import json
import os
import sys

# Prices per token, mirroring crates/lumen-core/src/rates.rs.
INPUT, OUTPUT, CACHE_READ, CACHE_WRITE = 5e-6, 25e-6, 0.5e-6, 6.25e-6

LUMEN_TOOLS = {
    "mcp__lumen__smart_read",
    "mcp__lumen__recall_file",
    "mcp__lumen__compress_logs",
}


def cost(u: dict) -> float:
    return (
        u.get("cache_read_input_tokens", 0) * CACHE_READ
        + u.get("cache_creation_input_tokens", 0) * CACHE_WRITE
        + u.get("input_tokens", 0) * INPUT
        + u.get("output_tokens", 0) * OUTPUT
    )


def load(paths: list[str]) -> list[dict]:
    out = []
    for p in paths:
        with open(p, errors="replace") as fh:
            for line in fh:
                line = line.strip()
                if not line:
                    continue
                try:
                    out.append(json.loads(line))
                except Exception:
                    continue  # a partially written final line is normal
    return out


def main() -> None:
    root = os.path.expanduser("~/.claude/projects")
    # Subagent transcripts are excluded: they reuse the parent's sessionId but carry
    # their own fresh context, so their rounds are not the parent's rounds.
    paths = [p for p in glob.glob(f"{root}/*/*.jsonl") if "/subagents/" not in p]
    if not paths:
        print(f"no transcripts under {root}", file=sys.stderr)
        sys.exit(1)

    recs = load(paths)
    by_uuid = {r["uuid"]: r for r in recs if isinstance(r.get("uuid"), str)}

    # uuid of an assistant record -> the tool it invoked.
    tool_of_assistant: dict[str, str] = {}
    for r in recs:
        if r.get("type") != "assistant":
            continue
        for block in (r.get("message") or {}).get("content") or []:
            if isinstance(block, dict) and block.get("type") == "tool_use":
                tool_of_assistant[r["uuid"]] = block.get("name") or "?"
                break

    # parentUuid -> assistant records that followed it.
    children: dict[str, list[dict]] = collections.defaultdict(list)
    for r in recs:
        if r.get("type") == "assistant" and isinstance(r.get("parentUuid"), str):
            children[r["parentUuid"]].append(r)

    # Walk each tool result to the round it caused.
    seen_msgs: set[str] = set()
    stats: dict[str, list[dict]] = collections.defaultdict(list)
    for r in recs:
        src = r.get("sourceToolAssistantUUID")
        if r.get("type") != "user" or not isinstance(src, str):
            continue
        tool = tool_of_assistant.get(src)
        if tool is None:
            continue
        for nxt in children.get(r["uuid"], []):
            msg = nxt.get("message") or {}
            usage = msg.get("usage")
            mid = msg.get("id")
            if not usage or not isinstance(mid, str):
                continue
            if mid in seen_msgs:  # usage repeats per content block
                continue
            seen_msgs.add(mid)
            stats[tool].append(usage)

    if not stats:
        print("the uuid chain yielded no pairs — cannot answer, not estimating")
        sys.exit(2)

    print(f"transcripts: {len(paths)}   records: {len(recs):,}   "
          f"attributed rounds: {sum(len(v) for v in stats.values()):,}\n")
    print(f"{'tool':<28}{'rounds':>8}{'cache_read':>12}{'output':>9}{'$/round':>10}")
    print("-" * 67)

    rows = sorted(stats.items(), key=lambda kv: -len(kv[1]))
    lumen_usages: list[dict] = []
    other_usages: list[dict] = []
    for tool, usages in rows:
        n = len(usages)
        cr = sum(u.get("cache_read_input_tokens", 0) for u in usages) / n
        out = sum(u.get("output_tokens", 0) for u in usages) / n
        avg = sum(cost(u) for u in usages) / n
        mark = " *" if tool in LUMEN_TOOLS else ""
        print(f"{tool + mark:<28}{n:>8,}{cr:>12,.0f}{out:>9,.0f}{avg:>10.4f}")
        (lumen_usages if tool in LUMEN_TOOLS else other_usages).extend(usages)

    print("\n* = a Lumen tool. The round after it is the overhead interception buys.")

    def summarise(label: str, usages: list[dict]) -> tuple[int, float, float]:
        if not usages:
            return 0, 0.0, 0.0
        n = len(usages)
        cr = sum(u.get("cache_read_input_tokens", 0) for u in usages) / n
        avg = sum(cost(u) for u in usages) / n
        print(f"{label:<28}{n:>8,}{cr:>12,.0f}{'':>9}{avg:>10.4f}")
        return n, cr, avg

    print()
    print(f"{'':<28}{'rounds':>8}{'cache_read':>12}{'':>9}{'$/round':>10}")
    print("-" * 67)
    ln, lcr, lavg = summarise("after a Lumen tool", lumen_usages)
    on, ocr, oavg = summarise("after any other tool", other_usages)

    if ln and on:
        print(f"\nobserved overhead of one round after a Lumen tool: ${lavg:.4f}")
        print(f"same for every other tool:                          ${oavg:.4f}")
        # The modelled figure uses the measured context of the round itself, so the
        # honest comparison is against the model evaluated at THAT context, not at a
        # nominal 100k.
        modelled = lcr * CACHE_READ + 1085 * OUTPUT
        print(f"modelled at the observed context ({lcr:,.0f} cache-read tokens): ${modelled:.4f}")
        delta = (lavg - modelled) / modelled * 100 if modelled else 0.0
        print(f"observed vs modelled: {delta:+.1f}%")
        if abs(delta) > 15:
            print("  -> they disagree by more than 15%; the observed number stands and")
            print("     the model needs explaining, per the measurement protocol.")
        else:
            print("  -> within 15%; the model is a fair stand-in at this context size.")


if __name__ == "__main__":
    main()
