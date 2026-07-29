#!/usr/bin/env python3
"""Per-call economics: (R, round cost) as a JOINT pair, plus follow-up rate.

Read-only. Touches no database and writes nothing.

Why joint. `R` (rounds remaining) and `round cost` are both functions of the same
position in the same session. Deriving them separately produces an artefact: the cost
side sat at a global mean context of 362,965 tokens while `R` varied per call, so a
late-session call was charged an early-session context. Taking both from the same
call removes that, and it retires the mean-versus-median argument entirely — no
average has to be chosen when every call carries its own numbers.

Why the transcript and not `read_events`. `req_key` is not yet flowing from the MCP
server, so the ledger cannot pair a `smart_read` with the `recall_file` that followed
it. The transcript can, and always could: every tool call is recorded with its
arguments, so "was this file fetched again in this session" is answerable directly.
That makes the follow-up rate measurable now rather than after a week.

Chain used, per the same structure `lumen_rounds.py` relies on:

    assistant record emitting tool_use   -> uuid
    user record carrying the result      -> sourceToolAssistantUUID = that uuid
    next assistant record                -> parentUuid = the user record's uuid,
                                            and the usage for the extra round

`usage` repeats once per content block, so rounds are deduplicated by `message.id`:
one row per billable request. Counting lines would roughly double every figure.
"""

from __future__ import annotations

import collections
import glob
import json
import os
import statistics
import sys

# Per-token prices, mirroring crates/lumen-core/src/rates.rs.
INPUT, OUTPUT, CACHE_READ, CACHE_WRITE = 5e-6, 25e-6, 0.5e-6, 6.25e-6

LUMEN_TOOLS = {
    "mcp__lumen__smart_read": "smart_read",
    "mcp__lumen__recall_file": "recall_file",
    "mcp__lumen__compress_logs": "compress_logs",
}


def round_cost(u: dict) -> float:
    return (
        u.get("cache_read_input_tokens", 0) * CACHE_READ
        + u.get("cache_creation_input_tokens", 0) * CACHE_WRITE
        + u.get("input_tokens", 0) * INPUT
        + u.get("output_tokens", 0) * OUTPUT
    )


def load(path: str) -> list[dict]:
    out = []
    with open(path, errors="replace") as fh:
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
    # Subagent transcripts reuse the parent's sessionId but carry their own fresh
    # context, so their rounds are not the rounds an interception adds to.
    paths = [p for p in glob.glob(f"{root}/*/*.jsonl") if "/subagents/" not in p]
    if not paths:
        print(f"no transcripts under {root}", file=sys.stderr)
        sys.exit(1)

    calls: list[dict] = []
    followups = collections.Counter()
    pair_gaps: list[int] = []

    for path in paths:
        recs = load(path)
        by_uuid = {r["uuid"]: r for r in recs if isinstance(r.get("uuid"), str)}

        # Ordered list of this session's distinct billable rounds, so R can be counted
        # as "how many of these come after me".
        order: list[str] = []
        seen: set[str] = set()
        for r in recs:
            if r.get("type") != "assistant":
                continue
            mid = ((r.get("message") or {}).get("id")) or ""
            if mid and mid not in seen:
                seen.add(mid)
                order.append(mid)
        position = {mid: i for i, mid in enumerate(order)}
        total_rounds = len(order)

        # Round indices at which the context was compacted.
        #
        # This bounds R, and it is not a detail: a saved token stops paying the moment the
        # context is rebuilt, because the file's content is gone from it either way.
        # Counting to the end of the session instead put the call-weighted median R at 658
        # and inflated the gross figure roughly tenfold.
        #
        # The marker is a `user` record carrying `isCompactSummary`, with no assistant
        # message id of its own — so it cannot be located by id lookup, which silently
        # found zero compactions on the first attempt. It is located by file order
        # instead: walk the records once and note how many distinct rounds have been
        # seen when the marker appears.
        compactions: list[int] = []
        seen_rounds: set[str] = set()
        for r in recs:
            if r.get("type") == "assistant":
                mid = ((r.get("message") or {}).get("id")) or ""
                if mid:
                    seen_rounds.add(mid)
            elif r.get("isCompactSummary") or r.get("compactMetadata"):
                compactions.append(len(seen_rounds))
        compactions.sort()

        # Every tool call, in order, with the file it touched.
        tool_calls: list[tuple[int, str, str]] = []  # (round index, tool, path arg)
        for r in recs:
            if r.get("type") != "assistant":
                continue
            mid = ((r.get("message") or {}).get("id")) or ""
            idx = position.get(mid, -1)
            for b in (r.get("message") or {}).get("content") or []:
                if not (isinstance(b, dict) and b.get("type") == "tool_use"):
                    continue
                name = b.get("name") or ""
                fp = (b.get("input") or {}).get("path") or (b.get("input") or {}).get(
                    "file_path"
                ) or ""
                tool_calls.append((idx, name, fp))

        # uuid of an assistant record -> (tool, path)
        tool_of: dict[str, tuple[str, str]] = {}
        for r in recs:
            if r.get("type") != "assistant":
                continue
            for b in (r.get("message") or {}).get("content") or []:
                if isinstance(b, dict) and b.get("type") == "tool_use":
                    fp = (b.get("input") or {}).get("path") or (
                        b.get("input") or {}
                    ).get("file_path") or ""
                    tool_of[r["uuid"]] = (b.get("name") or "?", fp)
                    break

        children: dict[str, list[dict]] = collections.defaultdict(list)
        for r in recs:
            if r.get("type") == "assistant" and isinstance(r.get("parentUuid"), str):
                children[r["parentUuid"]].append(r)

        counted: set[str] = set()
        for r in recs:
            src = r.get("sourceToolAssistantUUID")
            if r.get("type") != "user" or not isinstance(src, str):
                continue
            info = tool_of.get(src)
            if info is None:
                continue
            tool, fp = info
            short = LUMEN_TOOLS.get(tool)
            if short is None:
                continue

            for nxt in children.get(r["uuid"], []):
                msg = nxt.get("message") or {}
                usage, mid = msg.get("usage"), msg.get("id")
                if not usage or not isinstance(mid, str) or mid in counted:
                    continue
                counted.add(mid)

                idx = position.get(mid, 0)
                # R runs to the next compaction, not to the end of the session.
                horizon = next((c for c in compactions if c > idx), total_rounds)
                # R and cost from the SAME call. This is the joint pair.
                calls.append(
                    {
                        "tool": short,
                        "path": fp,
                        "cost": round_cost(usage),
                        "R": max(0, horizon - idx - 1),
                        "compaction_bounded": horizon < total_rounds,
                        "context": usage.get("cache_read_input_tokens", 0),
                        "output": usage.get("output_tokens", 0),
                    }
                )

                # Follow-up: was the same path fetched again later in this session?
                if short == "smart_read" and fp:
                    later = [
                        (i, t)
                        for (i, t, p) in tool_calls
                        if p == fp and i > idx and t == "mcp__lumen__recall_file"
                    ]
                    followups[bool(later)] += 1
                    if later:
                        pair_gaps.append(min(i for i, _ in later) - idx)

    if not calls:
        print("the uuid chain yielded no Lumen tool calls — cannot answer")
        sys.exit(2)

    print(f"transcripts: {len(paths)}   attributed Lumen calls: {len(calls):,}\n")

    # ── the joint pair ───────────────────────────────────────────────────────
    print("=== per-call (R, round cost) — joint, from the same call ===")
    print(f"{'tool':<14}{'n':>6}{'med R':>8}{'med $':>9}{'mean $':>9}{'med ctx':>10}")
    print("-" * 56)
    by_tool: dict[str, list[dict]] = collections.defaultdict(list)
    for c in calls:
        by_tool[c["tool"]].append(c)
    for tool in sorted(by_tool, key=lambda t: -len(by_tool[t])):
        cs = by_tool[tool]
        print(
            f"{tool:<14}{len(cs):>6}"
            f"{statistics.median(c['R'] for c in cs):>8.0f}"
            f"{statistics.median(c['cost'] for c in cs):>9.4f}"
            f"{statistics.mean(c['cost'] for c in cs):>9.4f}"
            f"{statistics.median(c['context'] for c in cs):>10,.0f}"
        )

    # ── the pair multiplier ──────────────────────────────────────────────────
    total_fu = followups[True] + followups[False]
    print("\n=== follow-up rate: a smart_read whose file was later recall_file'd ===")
    if total_fu:
        rate = followups[True] / total_fu
        print(f"  smart_read calls with a resolvable path : {total_fu}")
        print(f"  followed by recall_file on the same path: {followups[True]} ({100*rate:.1f}%)")
        print(f"  pair multiplier (rounds per intercept)  : {1 + rate:.3f}")
        if pair_gaps:
            print(f"  median rounds until the follow-up       : {statistics.median(pair_gaps):.0f}")
    else:
        print("  no smart_read call had a resolvable path — cannot answer")
        rate = 0.0

    # ── the dollar figure ────────────────────────────────────────────────────
    # Value of saving S tokens on a call = S x (cache_write + cache_read x R) / 1e6,
    # with that call's own R. Cost = its own round cost, multiplied by the measured
    # rounds-per-intercept.
    print("\n=== net value per route, per-call R and per-call cost ===")
    print(f"{'route':<14}{'n':>6}{'gross $':>10}{'round $':>10}{'net $':>11}{'paid?':>8}")
    print("-" * 59)

    import sqlite3

    db = os.path.expanduser("~/Library/Application Support/io.speedata.lumen/lumen.db")
    saved_by_tool: dict[str, list[int]] = collections.defaultdict(list)
    try:
        con = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
        for route, saved in con.execute(
            "SELECT routed_via, saved_tokens FROM read_events "
            "WHERE routed_via IN ('smart_read','recall_file')"
        ):
            saved_by_tool[route].append(saved)
    except Exception as e:  # pragma: no cover
        print(f"  (ledger unreadable: {e})")

    grand_net = 0.0
    for tool in ("smart_read", "recall_file"):
        cs = by_tool.get(tool) or []
        saved = saved_by_tool.get(tool) or []
        if not cs or not saved:
            continue
        # Pair the two populations by rank: the ledger and the transcript are the same
        # calls seen from two sides, but they cannot be joined without req_key, so each
        # call's saving is matched to a call's economics at the same quantile. Stated
        # rather than hidden: this is the one approximation in the figure.
        cs_sorted = sorted(cs, key=lambda c: c["R"])
        saved_sorted = sorted(saved)
        m = min(len(cs_sorted), len(saved_sorted))
        gross = cost = 0.0
        paid = 0
        for i in range(m):
            c = cs_sorted[int(i * len(cs_sorted) / m)]
            s = saved_sorted[int(i * len(saved_sorted) / m)]
            v = s * (CACHE_WRITE + CACHE_READ * c["R"])
            k = c["cost"] * (1 + rate)
            gross += v
            cost += k
            if v > k:
                paid += 1
        net = gross - cost
        grand_net += net
        print(
            f"{tool:<14}{m:>6}{gross:>10.2f}{cost:>10.2f}{net:>+11.2f}"
            f"{100*paid/m:>7.1f}%"
        )

    print("-" * 59)
    print(f"{'TOTAL':<14}{'':>6}{'':>10}{'':>10}{grand_net:>+11.2f}")
    print()


if __name__ == "__main__":
    main()
