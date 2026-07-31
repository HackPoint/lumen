# Does the optimizer actually save anything?

Every number on this page comes from `cargo test --release -p lumen-mcp --test efficiency --
--nocapture`. It is a test, not a spreadsheet: the thresholds are asserted, so a regression
that makes these claims false fails CI instead of quietly ageing into marketing copy.

Reproduce it yourself — the corpus is whatever repository you run it in.

Measured 2026-07-31 on Lumen at v1.5.1, macOS arm64, `cl100k_base` via `lumen-tok`.

---

## 1. An outline costs 6.4% of the file it describes

29 files in this repository are at or above the 300-line interception threshold.

| | tokens |
| --- | --- |
| reading all 29 in full | 234,126 |
| `smart_read` outlines only | **15,063** — 6.4% of full |
| outline + one `recall_file` for the item you wanted | 20,685 — 8.8% of full |

Per file the median outline is 7.3% and the worst is 16.1%. The worst case still beats
reading the file, which is asserted; an outline that costs more than the file would mean
interception made that read worse.

The largest files, where it matters most:

| file | lines | full | outline | ratio |
| --- | --- | --- | --- | --- |
| `lumenator/src-tauri/src/setup.rs` | 4,568 | 43,125 | 1,476 | 3.4% |
| `crates/lumen-core/src/report.rs` | 2,191 | 19,536 | 1,217 | 6.2% |
| `crates/lumen-core/src/ranked.rs` | 1,909 | 17,438 | 964 | 5.5% |
| `crates/lumen-mcp/src/lib.rs` | 1,563 | 13,586 | 665 | 4.9% |
| `crates/lumen-daemon/src/main.rs` | 1,150 | 11,225 | 534 | 4.8% |

The ratio improves with size, which is the useful direction: the outline grows with the
number of declarations, the file grows with the number of lines inside them.

## 2. What it actually saved on this machine

4,261 recorded reads between 2026-06-07 and 2026-07-31. The ledger is live — it grows
while you work, including from the reads that produced this page — so re-running the test
gives slightly larger numbers, not the same ones.

| route | calls | full tokens | returned | saved | saved % |
| --- | --- | --- | --- | --- | --- |
| `recall_file` | 1,074 | 9,731,044 | 1,606,349 | 8,213,421 | **84.4%** |
| `smart_read` | 423 | 2,049,881 | 411,646 | 1,641,856 | **80.1%** |
| `builtin_read` | 2,764 | 6,873,340 | 6,873,340 | 0 | 0.0% |

**9.86M tokens saved** across 1,497 intercepted reads. Priced through the same `Econ` model
the UI headline uses:

| | |
| --- | --- |
| gross value of the saving | $381.89 |
| cost of the extra round each intercept forces | −$312.29 |
| **net** | **$69.61** |

## 3. The three numbers that make this honest

Each of these is asserted by a test, because each is a way the headline above could lie.

**64.9% of reads never reached a Lumen tool.** 2,764 of 4,261. A saving on a third of reads
is not a saving on all of them, and the percentages in §2 are per-route, not per-session.
Most bypasses are files under the 300-line threshold, where interception should not fire —
but the denominator belongs on the page.

**170 calls returned *more* than the file was worth** — 92,347 tokens spent above what a
plain read would have cost. `saved_tokens` clamps at zero, so the ledger structurally cannot
show these; the headline can only ever be flattered by them. They are 0.94% of the reported
saving, and a test fails if they exceed 5%.

**96.2% of the saving rests on a real tokenizer count.** `full_tokens` is the denominator of
every claim here, and it comes either from `lumen-tok` or from a bytes/4 guess.
`token_source` records which, and a test fails below 80%. The remaining 3.8% is old rows
written before that column existed.

## 4. Where interception does not pay

Break-even is **5,383 avoided tokens per read** — below that, the extra round the block
forces costs more than the outline saves. **16 of 29 files fall short**:

| avoided | lines | file |
| --- | --- | --- |
| 2,137 | 306 | `crates/lumen-core/src/project.rs` |
| 2,603 | 304 | `crates/lumen-core/src/rates.rs` |
| 2,659 | 395 | `crates/lumen-core/tests/filing_routes.rs` |
| 2,831 | 421 | `crates/lumen-core/src/structure.rs` |
| 3,315 | 409 | `crates/lumen-core/src/update.rs` |

They cluster just above 300 lines, which is what you would expect: a file barely over the
threshold cannot avoid much. Net across the corpus is still positive (+$2.22), so the
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
