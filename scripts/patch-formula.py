#!/usr/bin/env python3
"""Stamp a version and per-platform sha256 into the Homebrew formula.

The formula has one `sha256` line per platform block. A `sed`-based substitution
matched them all, so the macOS tarball's hash was written onto the Linux entry
too and every Linux `brew install` failed its checksum. This walks the file and
attributes each hash to its enclosing `on_macos` / `on_linux` block.

Usage:
    patch-formula.py FORMULA --version 1.1.0 --macos-sha aabb.. [--linux-sha ccdd..]

A hash that is empty or omitted leaves that block's existing value alone: on a
release where a platform's tarball was never published, keeping the old hash is
better than writing one we know is wrong.
"""

from __future__ import annotations

import argparse
import re
import sys

# `on_macos do` / `on_linux do`, at any indentation.
BLOCK_RE = re.compile(r"^\s*on_(macos|linux)\s+do\s*$")
# The top-level `version "..."` stanza, which is not inside a platform block.
VERSION_RE = re.compile(r'^(\s*version\s+")[^"]*(".*)$')
SHA_RE = re.compile(r'^(\s*sha256\s+")[^"]*(".*)$')


def patch(lines: list[str], version: str, shas: dict[str, str]) -> tuple[list[str], list[str]]:
    """Return the patched lines plus a log of what changed."""
    out: list[str] = []
    log: list[str] = []
    block: str | None = None
    seen: set[str] = set()

    for line in lines:
        if m := BLOCK_RE.match(line):
            block = m.group(1)

        if m := VERSION_RE.match(line):
            line = f"{m.group(1)}{version}{m.group(2)}\n"
            log.append(f"version -> {version}")
        elif m := SHA_RE.match(line):
            if block is None:
                # A sha256 outside any platform block is not something this
                # script knows how to attribute; refuse rather than guess.
                raise SystemExit("error: sha256 line found outside any on_<platform> block")
            seen.add(block)
            new = shas.get(block, "")
            if new:
                line = f"{m.group(1)}{new}{m.group(2)}\n"
                log.append(f"{block} sha256 -> {new}")
            else:
                log.append(f"{block} sha256 left untouched (no hash supplied)")
        out.append(line)

    # A supplied hash that matched no block means the formula and the release
    # pipeline have drifted apart — loud failure, not a silent no-op.
    for platform, sha in shas.items():
        if sha and platform not in seen:
            raise SystemExit(f"error: --{platform}-sha given but the formula has no on_{platform} block")

    return out, log


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("formula")
    ap.add_argument("--version", required=True)
    ap.add_argument("--macos-sha", default="")
    ap.add_argument("--linux-sha", default="")
    args = ap.parse_args()

    with open(args.formula) as fh:
        lines = fh.readlines()

    patched, log = patch(
        lines, args.version, {"macos": args.macos_sha, "linux": args.linux_sha}
    )

    with open(args.formula, "w") as fh:
        fh.writelines(patched)

    for entry in log:
        print(entry, file=sys.stderr)


if __name__ == "__main__":
    main()
