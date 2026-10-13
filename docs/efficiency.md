# Does the optimizer actually save anything?

Every number on this page comes from `cargo test --release -p lumen-mcp --test efficiency --
--nocapture`. It is a test, not a spreadsheet: the thresholds are asserted, so a regression
that makes these claims false fails CI instead of quietly ageing into marketing copy.

Reproduce it yourself — the corpus is whatever repository you run it in.

Measured on macOS arm64, `cl100k_base` via `lumen-tok`, in two sittings:

- **Sections 1 and 4, and the corpus figures in the last subsection of §3, on 2026-10-12**, at
  v1.7.0 plus the unreleased changes in `CHANGELOG.md`. The corpus is this repository's own
  files, so these move with every commit that adds or edits a source file of 300 lines or more.
- **The ledger figures in sections 2 and 3, on 2026-07-31**, at v1.5.1 plus the unreleased fixes
  below. The test reads the ledger of the machine it runs on, here the author's, and these were
  not re-measured.

---

## 1. An outline costs 7.4% of the file it describes

61 files in this repository are at or above the 300-line interception threshold.

| | tokens |
| --- | --- |
| reading all 61 in full | 547,191 |
| `smart_read` outlines only | **40,323** — 7.4% of full |
| outline + one `recall_file` for the item you wanted | 51,977 — 9.5% of full |

Per file the median outline is 8.6% and the worst is 20.9%. The worst case still beats
reading the file, which is asserted; an outline that costs more than the file would mean
interception made that read worse.

The largest files, where it matters most:

| file | lines | full | outline | ratio |
| --- | --- | --- | --- | --- |
| `lumenator/src-tauri/src/setup.rs` | 4,729 | 45,251 | 1,831 | 4.0% |
| `lumenator/src-tauri/src/setup/hook_e2e.rs` | 2,713 | 27,025 | 1,949 | 7.2% |
| `crates/lumen-core/src/report.rs` | 2,601 | 23,846 | 1,325 | 5.6% |
| `crates/lumen-mcp/src/lib.rs` | 2,573 | 23,787 | 1,027 | 4.3% |
| `crates/lumen-mcp/tests/efficiency.rs` | 2,365 | 22,713 | 1,522 | 6.7% |

Among the ten largest files the outline costs between 3.7% (`crates/lumen-daemon/src/main.rs`)
and 11.7% (`crates/lumen-stats/tests/aggregates.rs`) of the file; the test prints all ten. The
outline grows with the number of declarations and the file with the number of lines inside them,
so the ratio is not a function of size alone.

## 2. What it actually saved on this machine

Recorded reads between 2026-06-07 and 2026-07-31. The ledger is live — it grows while you
work, including from the reads that produced this page — so re-running the test gives slightly
larger numbers, not the same ones.

**1,573 intercepted reads, 11.03M tokens saved.**

The dollar value this section gave is withdrawn (1.6.0). It was priced through the `Econ`
model, whose rounds-remaining input `R` was one assumed constant rather than derived per call,
so the figure was that assumption multiplied out. From 1.9.0 `lumen report` derives `R` and the
round's cost for each call from the ledger's own turns (`lumen_core::savings`); this page quotes
no dollar figure, because that one is this machine's.

The missed-optimization baseline — what the un-intercepted reads cost — is **3,500,277 tokens**.
The raw figure is 6,931,572, and **half of that is binary files** whose token count came from a
bytes/4 guess; see §3.

## 3. The three numbers that make this honest

Each of these is asserted by a test, because each is a way the headline above could lie.

### Almost nothing leaked — and zero was the wrong thing to assert

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

**A correction to the first version of this page.** It said the assertion was an equality — zero
eligible reads bypassing the optimizer, no acceptable non-zero value. That was wrong, and it broke
within the hour of being written.

The intercept has two deliberate fail-open guards: `retry_escape_valve` lets a second read of the
same path through in one session, so a model that genuinely cannot reach the MCP tools is never
deadlocked, and `lumen_mcp_missing` does the same when the server is absent. (Since #36 the
first guard is narrower: once `smart_read` or `recall_file` has answered for the path in the
session, the retry is sent to `recall_file` once more before it is released.) Both produce exactly
the shape counted above — a built-in read of a file the hook covers — and both are working as
designed. The count is now a bounded share with the offending paths printed, so a real routing
regression (the hook not firing at all, which would put every eligible read in this bucket) is still
caught, while a guard doing its job is not reported as a defect.

The other rows are a statement of *scope*, and the last one is a bill:

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

### 170 calls returned more than the file, and what stops it now

92,347 tokens spent above what a plain read would have cost, between 2026-06-08 and 2026-07-28.
That figure is a **floor**: the JSON-RPC envelope and `_meta` block are never counted, so every
recorded `tokens_returned` understates real cost by another 40–60 tokens per call.

96% of it came from one mechanism. `recall_file` prefixed every emitted line with `{lineno:>5}: `
— about three tokens of gutter per line — and applied that across essentially whole files,
because name matching was `contains()` while the tool's own description promised exact matching.
`names: ["e"]` selected nearly every item in a file. Worst single call: a 1,436-line file,
9,928 tokens as a plain read, 14,845 returned.

Fixed at five levels, and then guarded at a sixth:

- exact matching first; substring only as a labelled fallback, capped at 5 items
- a compact `123|` gutter instead of `  123: ` (removable entirely with `LUMEN_LINE_NUMBERS=0`)
- overlapping and adjacent items merged into one block, so shared lines are emitted once
- the no-selector branch returns the outline, not the file plus a header
- `smart_read(mode="full")` moved to its own route, so delivering a whole file can no longer pool
  with outline savings
- **and a guard**: a reply that would cost more than the file returns the file instead, with one
  line saying so, routed `would_inflate`

The guard is the load-bearing change, and it is not one point every reply passes through. It sits
where each reply is built: in `metered_guarded` (the `smart_read` outline and every `recall_file`
branch), on the ranked arm's decline path, in the fallback for a file with no structure, and in a
check of its own inside `compress_logs`. `smart_read(mode="full")` has none: the whole file is
what it was asked for.

Two corpus tests hold it to the text each reply sent, counted again, not to what the reply says
about itself (`no_reply_costs_more_than_the_file_and_its_label_on_any_real_file` and
`compress_logs_costs_no_more_than_the_log_unless_the_log_is_blank`). They ask every UTF-8 file of
300 lines or more in this repository that fits in 50,000 tokens, outside dot-directories, `target`,
`node_modules` and `dist` (70 files of any extension, 61 with structure; two lockfiles and a
benchmark table are over the limit), 8 small files and 56 synthetic logs, in each of these shapes,
1,134 calls on the 70 files:

- `smart_read`, as an outline and as `mode="full"`;
- `recall_file` by an exact name, a substring, a name that matches nothing, a name too broad and
  every item; with no selector; and by line range: the whole file, nearly all of it, half, a few
  lines, one line, and a range that runs past the end;
- `compress_logs`, by path and inline.

A reply must cost no more than the file and what labels it:

| reply | allowed above the file | most seen |
| --- | --- | --- |
| the file behind the one-line inflation note (`would_inflate`) | the note, up to 80 tokens | 27 |
| a file with no structure, behind its note (`ranked_no_defs`) | the note, which carries the path | 61 |
| `smart_read(mode="full")` | its header, which carries the path | 47 |
| `compress_logs` | nothing: the log itself when the reply cannot cost less | 0 |
| a file or log with nothing visible in it | no bound: the reply stays visible, as a client may reject an empty text block | 59 |

"Most seen" is in tokens, on this repository's files, except the last row, which is an empty file
read by `compress_logs` by path among the test's small-file fixtures (no file here is blank). The
figures that carry a path, that row's among them, are for the paths of the checkout that measured
them.

**Not checked.** Error replies, which carry no file content. The answers to a repeat read, sent
only when cheaper than the full answer and tested in `repeat_reads.rs`. The JSON-RPC envelope and
`_meta` block, which no one counts. A file that is not UTF-8, and one over 50,000 tokens. A
`recall_file` request whose start line is after its end line, or far beyond the end of the file:
the test does not generate those.

**A correction to the previous version of this page.** It put the backstop "at the one point every
metered reply passes through", said this "can no longer happen" and gave the worst overage as 27
tokens, 0.33% of the file. There is no such point, and `compress_logs` was outside the guard: it
counted the body it compressed and not the header it added, so a log that compressed badly came
back costing more than the log, under a header that said it saved tokens. On 56 synthetic logs,
by path and inline (112 replies), 31 cost more than the log and 11 of those said they saved
tokens. It now counts the reply it sends and returns the log itself when that reply cannot cost
less, routed `would_inflate`. The header's "saved N" still counts the body only; `_meta` and the
ledger count the whole reply, so they read lower by the header.

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

That test drove the shell meters, and went with them. From 1.6.0 the installed meter hook and
the repository's copy are both shims over one meter, `lumen-mcp hook meter`, which counts with
`lumen-tok`'s tokenizer in-process: there are no copies to compare and no exit code to drop.
`a_file_that_is_not_utf8_is_recorded_as_unsupported_with_no_count`, in
`crates/lumen-mcp/tests/hook_cli.rs`, holds it to the same rule.

## 4. Where interception does not pay

At the assumed `R`, break-even is **5,383 avoided tokens per read** — below that, the extra
round the block forces costs more than the outline saves. **31 of 61 files fall short**
(`the_files_where_interception_does_not_pay_are_named` prints the count and the eight least
avoided; these are the first five):

| avoided | lines | file |
| --- | --- | --- |
| 1,994 | 303 | `crates/lumen-daemon/tests/upgrade_matrix/releases.rs` |
| 2,101 | 306 | `crates/lumen-core/src/project.rs` |
| 2,351 | 310 | `benchmarks/harness/src/main.rs` |
| 2,413 | 326 | `benchmarks/harness/src/env.rs` |
| 2,567 | 304 | `crates/lumen-core/src/rates.rs` |

The eight it prints are 303 to 419 lines long, which is what you would expect: a file barely over
the threshold cannot avoid much. The test asserts only that not every file falls short; the count
is printed, not pinned. Net across the corpus came out positive at that `R`, so the threshold
pays in aggregate under that assumption — but "the optimizer saves 94%" is false for these files,
and the test names them rather than averaging them away.

Raising the threshold is the obvious response and it is not clearly right: the loss on a
310-line file is small and bounded, while the gain on `setup.rs` is 43,420 tokens. Left at
300 deliberately, with the cost written down.

---

## Reading these numbers

The percentage is the honest way to describe **one intercepted read**: an outline of
`setup.rs` costs 4.0% of the file.

Dollars are the honest way to describe **the feature**, once `R` is derived per call. A
percentage cannot express the round-trip an intercept forces, which is why 1.4.0 replaced
the ratio headline: the denominator is the read Lumen prevented, and the numerator has to
include the read it caused instead. Until then no dollar figure is published here.

Both are in the tests. Neither is asserted to be positive — `the_net_value_prices_in_the_
round_interception_costs` deliberately prints a negative net if that is what the corpus
gives, because a test that requires a favourable answer measures nothing.
