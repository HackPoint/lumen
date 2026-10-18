#!/usr/bin/env python3
"""Reproduce the SWE-bench Verified draw recorded in tasks.toml.

The draw is a hash ranking, not a PRNG sample, so it does not depend on the Python version:
every instance id is ranked by sha256("<seed>\n<instance_id>") and the first N are kept, in
rank order. Nothing is filtered: no repo, size, difficulty or language is excluded.

Dependencies: the Python standard library only, with one exception. Reading the dataset's
parquet file needs `pyarrow`:

    uv run --with pyarrow python benchmarks/competitors/draw_tasks.py

Without pyarrow, pass the 500 instance ids as a text file (one per line) with --ids-file;
the draw itself is the same. The parquet file is downloaded from the pinned revision and its
sha256 is checked before it is read; a mismatch stops the run.

    draw_tasks.py                  print the draw as TOML blocks
    draw_tasks.py --check          fail unless tasks.toml holds exactly this draw
    draw_tasks.py --parquet FILE   use a parquet file already on disk (still sha256-checked)
"""
import argparse
import hashlib
import re
import sys
import urllib.request
from pathlib import Path

DATASET = "princeton-nlp/SWE-bench_Verified"
REVISION = "c104f840cc67f8b6eec6f759ebc8b2693d585d4a"
PARQUET_PATH = "data/test-00000-of-00001.parquet"
PARQUET_SHA256 = "a45b1fe4e2f0c8390b2b2938ac83e92ed5979000856808f3679c07812e9e6dcd"
SEED = "lumen-p2-2026-10-18"
COUNT = 10
EXPECTED_ROWS = 500


def rank(ids, seed, count):
    """The first `count` ids by sha256(seed + newline + id), ties impossible for distinct ids."""
    uniq = sorted(set(ids))
    key = lambda i: hashlib.sha256(f"{seed}\n{i}".encode()).hexdigest()
    return sorted(uniq, key=key)[:count]


def fetch_parquet(dest):
    url = f"https://huggingface.co/datasets/{DATASET}/resolve/{REVISION}/{PARQUET_PATH}"
    with urllib.request.urlopen(url, timeout=120) as r:
        dest.write_bytes(r.read())


def read_rows(path):
    digest = hashlib.sha256(path.read_bytes()).hexdigest()
    if digest != PARQUET_SHA256:
        sys.exit(f"parquet sha256 {digest} != pinned {PARQUET_SHA256}")
    try:
        import pyarrow.parquet as pq
    except ImportError:
        sys.exit("reading parquet needs pyarrow (uv run --with pyarrow ...), or use --ids-file")
    t = pq.read_table(path, columns=["instance_id", "repo", "base_commit"])
    return {r["instance_id"]: r for r in t.to_pylist()}


def toml_blocks(rows, picked):
    out = []
    for i in picked:
        r = rows[i]
        out.append(
            f'[[swe]]\nid = "{i}"\nrepo = "{r["repo"]}"\nbase_commit = "{r["base_commit"]}"\n'
        )
    return "\n".join(out)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawTextHelpFormatter)
    ap.add_argument("--parquet", type=Path)
    ap.add_argument("--ids-file", type=Path)
    ap.add_argument("--check", action="store_true")
    a = ap.parse_args()

    if a.ids_file:
        ids = [l.strip() for l in a.ids_file.read_text().splitlines() if l.strip()]
        rows = None
    else:
        p = a.parquet
        if p is None:
            p = Path("/tmp/swe-bench-verified-" + REVISION[:12] + ".parquet")
            if not p.exists():
                fetch_parquet(p)
        rows = read_rows(p)
        ids = list(rows)
    if len(set(ids)) != EXPECTED_ROWS:
        sys.exit(f"expected {EXPECTED_ROWS} distinct instance ids, got {len(set(ids))}")
    picked = rank(ids, SEED, COUNT)

    if a.check:
        here = Path(__file__).with_name("tasks.toml").read_text()
        have = re.findall(r'^\[\[swe\]\]\s*\nid = "([^"]+)"', here, re.M)
        if have != picked:
            sys.exit(f"tasks.toml has {have}, the draw is {picked}")
        print("tasks.toml matches the draw")
        return
    print(f"# dataset {DATASET} @ {REVISION}\n# seed {SEED!r}, first {COUNT} by sha256 rank")
    if rows is None:
        print("\n".join(picked))
    else:
        print(toml_blocks(rows, picked))


if __name__ == "__main__":
    main()
