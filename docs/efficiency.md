# Does the optimizer actually save anything?

Every number on this page comes from `cargo test --release -p lumen-mcp --test efficiency --
--nocapture`. It is a test, not a spreadsheet: the thresholds are asserted, so a regression
that makes these claims false fails CI instead of quietly ageing into marketing copy.

Reproduce it yourself — the corpus is whatever repository you run it in.

Measured 2026-07-31 on Lumen at v1.5.1 plus the unreleased fixes below, macOS arm64,
`cl100k_base` via `lumen-tok`.

---

## 1. An outline costs 6.2% of the file it describes

31 files in this repository are at or above the 300-line interception threshold.

| | tokens |
| --- | --- |
| reading all 31 in full | 258,247 |
| `smart_read` outlines only | **16,022** — 6.2% of full |
| outline + one `recall_file` for the item you wanted | 23,281 — 9.0% of full |

Per file the median outline is 7.0% and the worst is 16.1%. The worst case still beats
reading the file, which is asserted; an outline that costs more than the file would mean
interception made that read worse.

The largest files, where it matters most:

| file | lines | full | outline | ratio |
| --- | --- | --- | --- | --- |
| `lumenator/src-tauri/src/setup.rs` | 4,568 | 43,125 | 1,476 | 3.4% |
| `crates/lumen-core/src/report.rs` | 2,269 | 20,425 | 1,221 | 6.0% |
| `crates/lumen-core/src/ranked.rs` | 1,922 | 17,614 | 964 | 5.5% |
| `crates/lumen-mcp/src/lib.rs` | 2,077 | 19,478 | 803 | 4.1% |
| `crates/lumen-daemon/src/main.rs` | 1,150 | 11,225 | 534 | 4.8% |

The ratio improves with size, which is the useful direction: the outline grows with the
number of declarations, the file grows with the number of lines inside them.

## 2. What it actually saved on this machine

Recorded reads between 2026-06-07 and 2026-07-31. The ledger is live — it grows while you
work, including from the reads that produced this page — so re-running the test gives slightly
larger numbers, not the same ones.

**1,573 intercepted reads, 11.03M tokens saved.** Priced through the same `Econ` model the UI
headline uses:

| | |
| --- | --- |
| gross value of the saving | $427.35 |
| cost of the extra round each intercept forces | −$328.14 |
| **net** | **$99.21** |

The missed-optimization baseline — what the un-intercepted reads cost — is **3,500,277 tokens**.
The raw figure is 6,931,572, and **half of that is binary files** whose token count came from a
bytes/4 guess; see §3.

## 3. The three numbers that make this honest

Each of these is asserted by a test, because each is a way the headline above could lie.

### Nothing leaked. **0 of 1,573 eligible reads bypassed the optimizer.**

**This section previously published "64.9% of reads never reached a Lumen tool", and that
figure was wrong** — not unflattering, wrong. It divided every recorded read of every file type
by the total, so every read was in the denominator whether or not interception was ever going to
fire. It measured the file mix of one machine, mostly how many screenshots its author had looked
at, and it read as a routing failure.

On the honest denominator — an extension the hook intercepts, at or above the threshold — the
leak rate is zero, and a test now asserts exactly that rather than a percentage:

| bucket | calls | tokens |
| --- | --- | --- |
| optimized | 1,573 | — |
| **leaked** — eligible, not intercepted | **0** | — |
| out of scope: under 300 lines | 2,553 | — |
| out of scope: unmeasurable (binary) | 124 | 3,431,295 *excluded from every baseline* |
| out of scope: kinds not handled | 95 | 640,335 — **the coverage backlog** |

A leak would mean the hook did not fire on a file it claims to cover, so there is no acceptable
non-zero value and the assertion is an equality. The other rows are a statement of *scope*, and
the last one is a bill:

| kind | calls | tokens read whole |
| --- | --- | --- |
| `.md` | 37 | 250,403 |
| `.scss` | 18 | 180,448 |
| `.js` | 20 | 97,678 |
| `.css` | 13 | 63,175 |
| `.yml` | 5 | 31,010 |

That is the honest replacement for the ratio: what is *not* covered, and what it would be worth.
None of it is scheduled — see the reasoning at the end of §4.

### Half the "missed optimization" baseline was screenshots

3,431,295 of 6,931,572 tokens — **50%** — came from 124 reads of binary files, almost all PNGs.
Their `full_tokens` is a bytes/4 estimate, which this codebase's own tokenizer notes overstates a
screenshot by roughly 40×. There was never a saving available to miss on a PNG.

`lumen-stats` has excluded these from its missed-optimization total since 1.2.1, using a
constant that already existed. **This page did not**, because the test that produces it
aggregated raw. Both now read the same list from one place. The corrected baseline is
**3,500,277**.

Also collapsed: **28 duplicate rows.** Two meter hooks are registered on a development machine —
one from `~/.claude/settings.json`, one from the repo — and both fire, so some reads were counted
twice.

### 170 calls returned more than the file — and that can no longer happen

92,347 tokens spent above what a plain read would have cost, between 2026-06-08 and 2026-07-28.
That figure is a **floor**: the JSON-RPC envelope and `_meta` block are never counted, so every
recorded `tokens_returned` understates real cost by another 40–60 tokens per call.

96% of it came from one mechanism. `recall_file` prefixed every emitted line with `{lineno:>5}: `
— about three tokens of gutter per line — and applied that across essentially whole files,
because name matching was `contains()` while the tool's own description promised exact matching.
`names: ["e"]` selected nearly every item in a file. Worst single call: a 1,436-line file,
9,928 tokens as a plain read, 14,845 returned.

Fixed at five levels, and then made impossible at a sixth:

- exact matching first; substring only as a labelled fallback, capped at 5 items
- a compact `123|` gutter instead of `  123: ` (removable entirely with `LUMEN_LINE_NUMBERS=0`)
- overlapping and adjacent items merged into one block, so shared lines are emitted once
- the no-selector branch returns the outline, not the file plus a header
- `smart_read(mode="full")` moved to its own route, so delivering a whole file can no longer pool
  with outline savings
- **and a backstop at the one point every metered reply passes through**: a reply that would cost
  more than the file returns the file instead, with one line saying so, routed `would_inflate`

The last one is the load-bearing change. A corpus test now asserts it against every real file in
this repository at or above the threshold, for every shape a caller can ask for. Worst overage
across the whole corpus: **27 tokens, 0.33% of the file** — the explanatory line itself.

### 96.6% of the saving rests on a real tokenizer count

`full_tokens` is the denominator of every claim here, and it comes either from `lumen-tok` or
from a bytes/4 guess. `token_source` records which, and a test fails below 80%. The remainder is
old rows written before that column existed; the ledger is append-only, so they stay as a
disclosed unknown rather than a rewritten guess.

One live bug was found here. The repository's own copy of the meter hook did

```sh
FULL_TOKENS=$("$LUMEN_TOK" < "$FILE_PATH" 2>/dev/null || echo 0)
TOKEN_SOURCE="measured"
```

discarding the exit code — and `lumen-tok` exits 3 for a file that is not valid UTF-8. So a PNG
was recorded as `full_tokens=0, token_source='measured'`: an unsupported file laundered as a
measurement, in the one column that exists to tell those two apart. The installed copy got this
right. The drift test compared column *names*, so it passed.

The test now runs **both** copies against a stub tokenizer and compares what they actually
record, across exit 0, exit 3, exit 1 and a missing binary. Reintroducing the bug fails it with
`generated ("unsupported") vs repo ("measured")`.

## 4. Where interception does not pay

Break-even is **5,383 avoided tokens per read** — below that, the extra round the block
forces costs more than the outline saves. **17 of 31 files fall short**:

| avoided | lines | file |
| --- | --- | --- |
| 2,137 | 306 | `crates/lumen-core/src/project.rs` |
| 2,603 | 304 | `crates/lumen-core/src/rates.rs` |
| 2,659 | 395 | `crates/lumen-core/tests/filing_routes.rs` |
| 2,831 | 421 | `crates/lumen-core/src/structure.rs` |
| 3,315 | 409 | `crates/lumen-core/src/update.rs` |

They cluster just above 300 lines, which is what you would expect: a file barely over the
threshold cannot avoid much. Net across the corpus is still positive (+$2.64), so the
threshold pays in aggregate — but "the optimizer saves 94%" is false for these files, and
the test names them rather than averaging them away.

Raising the threshold is the obvious response and it is not clearly right: the loss on a
310-line file is small and bounded, while the gain on `setup.rs` is 41,649 tokens. Left at
300 deliberately, with the cost written down.

---

## Reading these numbers

The percentage is the honest way to describe **one intercepted read**: an outline of
`setup.rs` costs 3.4% of the file.

Dollars are the honest way to describe **the feature**. A percentage cannot express the
round-trip an intercept forces, which is why 1.4.0 replaced the ratio headline: the
denominator is the read Lumen prevented, and the numerator has to include the read it
caused instead.

Both are in the tests. Neither is asserted to be positive — `the_net_value_prices_in_the_
round_interception_costs` deliberately prints a negative net if that is what the corpus
gives, because a test that requires a favourable answer measures nothing.
