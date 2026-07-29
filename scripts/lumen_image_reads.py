#!/usr/bin/env python3
"""Attribute every image Read to the human prompt that led to it, and classify it.

Read-only. This is the gate for the 1.2.0 non-text interception feature: blocking
image reads is only safe if the agent opens them on its own initiative. If a
substantial share were the user explicitly asking about a screenshot, blanket
blocking breaks real work.

Attribution walks the transcript chain backwards rather than guessing from
timestamps:

    assistant record emitting Read(*.png)   -> parentUuid
      -> ... follow parentUuid upwards, skipping tool-result records ...
        -> the nearest `user` record that is a REAL human prompt

A real human prompt is a `user` record with neither `sourceToolAssistantUUID` nor
`toolUseResult` — i.e. typed by a person, not injected by the harness as a tool
result. That distinction is what makes the classification meaningful.

Classification is deliberately conservative: anything whose prompt mentions an
image, a screenshot, or the file's own name counts as DELIBERATE. Ambiguity is
resolved towards "deliberate", because a false "agent-initiated" would understate
the risk of the feature under test.
"""

from __future__ import annotations

import collections
import glob
import json
import os
import re
import sys

IMAGE_RE = re.compile(r"\.(png|jpe?g|gif|webp|bmp|tiff?|ico|pdf)$", re.I)

# Words a person uses when they mean "look at this picture".
DELIBERATE_WORDS = re.compile(
    r"\b(screenshot|screen shot|image|picture|photo|diagram|mockup|mock-up|figure|"
    r"png|jpe?g|svg|icon|logo|chart|graph|visual|look at|see the|attached|"
    r"скриншот|картинк|изображени|схем|макет|посмотри)\b",
    re.I,
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
                continue
    return out


def human_text(rec: dict) -> str:
    """The typed text of a user record, flattened."""
    msg = rec.get("message") or {}
    content = msg.get("content")
    if isinstance(content, str):
        return content
    parts = []
    for b in content or []:
        if isinstance(b, dict) and b.get("type") == "text":
            parts.append(b.get("text") or "")
        elif isinstance(b, str):
            parts.append(b)
    return " ".join(parts)


def is_human_prompt(rec: dict) -> bool:
    # A tool result is also type=user; these two keys are what separate a person
    # typing from the harness injecting output.
    return (
        rec.get("type") == "user"
        and not rec.get("sourceToolAssistantUUID")
        and rec.get("toolUseResult") is None
    )


def main() -> None:
    root = os.path.expanduser("~/.claude/projects")
    paths = [p for p in glob.glob(f"{root}/*/*.jsonl") if "/subagents/" not in p]

    total = 0
    unattributed = 0
    verdicts: collections.Counter = collections.Counter()
    samples: list[tuple[str, str, str]] = []

    for path in paths:
        recs = load(path)
        by_uuid = {r["uuid"]: r for r in recs if isinstance(r.get("uuid"), str)}

        for r in recs:
            if r.get("type") != "assistant":
                continue
            for block in (r.get("message") or {}).get("content") or []:
                if not (isinstance(block, dict) and block.get("type") == "tool_use"):
                    continue
                if block.get("name") != "Read":
                    continue
                fp = (block.get("input") or {}).get("file_path") or ""
                if not IMAGE_RE.search(fp):
                    continue

                total += 1
                # Walk up to the nearest real human prompt.
                cur, prompt, hops = r, None, 0
                while cur is not None and hops < 400:
                    parent = by_uuid.get(cur.get("parentUuid") or "")
                    if parent is None:
                        break
                    if is_human_prompt(parent):
                        prompt = human_text(parent)
                        break
                    cur, hops = parent, hops + 1

                if prompt is None:
                    unattributed += 1
                    verdicts["UNATTRIBUTED"] += 1
                    continue

                name = os.path.basename(fp)
                stem = os.path.splitext(name)[0]
                if name in prompt or (len(stem) > 3 and stem in prompt):
                    v = "DELIBERATE (prompt names the file)"
                elif DELIBERATE_WORDS.search(prompt):
                    v = "DELIBERATE (prompt mentions an image)"
                else:
                    v = "AGENT-INITIATED"
                verdicts[v] += 1
                if len(samples) < 12:
                    samples.append((v, name, " ".join(prompt.split())[:88]))

    if total == 0:
        print("no image Read calls found in any transcript — cannot answer")
        sys.exit(2)

    print(f"transcripts scanned : {len(paths)}")
    print(f"image Read calls    : {total}")
    print(f"unattributed        : {unattributed} "
          f"({100 * unattributed / total:.1f}%)\n")

    print(f"{'verdict':<38}{'n':>5}{'share':>8}")
    print("-" * 51)
    for v, n in verdicts.most_common():
        print(f"{v:<38}{n:>5}{100 * n / total:>7.1f}%")

    deliberate = sum(n for v, n in verdicts.items() if v.startswith("DELIBERATE"))
    agent = verdicts.get("AGENT-INITIATED", 0)
    attributed = deliberate + agent
    if attributed:
        print(f"\nof the {attributed} attributed reads: "
              f"{100 * deliberate / attributed:.1f}% deliberate, "
              f"{100 * agent / attributed:.1f}% agent-initiated")

    print("\nsample classifications:")
    for v, name, prompt in samples:
        print(f"  [{v.split(' ')[0]:<15}] {name:<28} <- {prompt!r}")


if __name__ == "__main__":
    main()
