# Tokenizer calibration

How far Lumen's token counts are from Claude's. Lumen counts tokens with `cl100k_base`
(`lumen_core::tokenizer`, the `lumen-tok` binary), OpenAI's tokenizer, because Claude's is not
available offline. This directory measures Claude's count of the same text, per content type and
Claude model, so that a figure built on the proxy can say how close it is.

**As of 2026-10-14**, through Claude Code 2.1.296, for twelve models: `claude-opus-5-5`,
`claude-sonnet-5-5` and `claude-haiku-5-5` (measured 2026-10-13), and `claude-fable-5`,
`claude-opus-5`, `claude-sonnet-5`, `claude-opus-4-8`, `claude-opus-4-7`, `claude-opus-4-6`,
`claude-sonnet-4-6`, `claude-opus-4-5-20251101` and `claude-haiku-4-5-20251001` (2026-10-14). A
newer model can bring a new tokenizer, so re-run it for every release (below) and whenever a new
model ships.

## Results

Claude's tokens per `cl100k_base` token, over the samples of each content type: the median, and
the 10th and 90th percentiles (linear interpolation between the closest ranks), with the number
of samples. The twelve models fall into two groups, and within a group every model counted every
sample alike (`results/samples.tsv`: their columns are equal on every row), so each group's rows
are equal and are shown once; `results/table.tsv` has every model's.

| content type | Opus 4.7 and later (8 models) | earlier (4 models) |
|---|---|---|
| Rust | 1.587 (1.487–1.628), n 8 | 1.254 (1.222–1.309), n 8 |
| Python | 1.658 (1.564–1.694), n 7 | 1.267 (1.245–1.307), n 7 |
| TypeScript / JavaScript | 1.633 (1.583–1.748), n 8 | 1.304 (1.249–1.328), n 8 |
| Go | 1.572 (1.513–1.677), n 7 | 1.252 (1.182–1.283), n 7 |
| shell | 1.700 (1.610–1.785), n 6 | 1.228 (1.203–1.250), n 6 |
| JSON | 1.473 (1.439–1.626), n 8 | 1.216 (1.198–1.235), n 8 |
| logs | 1.441 (1.341–1.655), n 8 | 1.160 (1.091–1.315), n 8 |
| prose (Markdown, reST) | 1.559 (1.480–1.625), n 8 | 1.165 (1.127–1.231), n 8 |
| **pooled** | **1.592 (1.444–1.699), n 60** | **1.238 (1.143–1.314), n 60** |

Opus 4.7 and later: `claude-opus-4-7`, `claude-opus-4-8`, `claude-opus-5`, `claude-sonnet-5`,
`claude-fable-5`, `claude-opus-5-5`, `claude-sonnet-5-5`, `claude-haiku-5-5`. Earlier:
`claude-opus-4-6`, `claude-sonnet-4-6`, `claude-opus-4-5`, `claude-haiku-4-5`.
`claude-fable-5` has one sample fewer: it stopped on `log-05` (see Method), so its logs row is
1.444 (1.358–1.659) over 7 samples and its pooled row 1.593 (1.447–1.699) over 59; on the other
59 it counted as the rest of its group. Pooled over all twelve models the median is 1.503
(1.213–1.686), over 60 texts sent to 12 models each.

- **Claude counts more, for every sample, on every model.** On the later group the lowest ratio
  of the 60 is 1.335 (Go's runtime panic output, `log-05`) and the highest 1.794 (nvm's
  `install.sh`, `shell-05`), so a `cl100k_base` count is short of Claude's by a quarter to over
  two fifths; 1,000 tokens counted by Lumen are about 1,590 of Claude's (1,440 to 1,700). On the
  earlier group the ratios run from 1.056 to 1.344, 1.24 at the median.
- **The tokenizer changed at Opus 4.7.** The later group counts about 29% more than the earlier
  one at the median, and a two-word sample shows it: `hello world` costs 4 tokens on the later
  group and 3 on the earlier.
- **It depends on the kind of file.** On the later group logs and JSON sit lowest (medians 1.44
  and 1.47) and shell, Python and TypeScript/JavaScript highest (1.70, 1.66, 1.63); on the
  earlier one logs and prose sit lowest (1.16, 1.17) and TypeScript/JavaScript highest (1.30).
- **The run's own checks** (`results/run.json`): for every model the empty prompt cost the same
  on all nine sends, and every sample's first two sends agreed, so none needed a third. Apart from
  `claude-fable-5` on `log-05`, no sample was dropped.

`crates/lumen-core/src/calibration.rs` embeds this table: `calibrate(cl100k_tokens, content_type,
model)` returns Claude's count from the median with the p10 to p90 band. A model the table does
not have keeps the content type and gets its row over every model, with a band over both groups;
a content type it does not have gets the model's pooled row, with a band over that model's rows.

## Re-running it (every release)

```sh
cargo build --release --locked -p lumen-mcp --bin lumen-tok
python3 benchmarks/calibration/calibrate.py fetch       # download, verify and count the corpus
python3 benchmarks/calibration/calibrate.py validate    # the method's premises, per model
python3 benchmarks/calibration/calibrate.py run         # 60 samples x 12 models, sequential
python3 benchmarks/calibration/calibrate.py summarize   # tables, and the rows for calibration.rs
```

It needs network access to `raw.githubusercontent.com`, a logged-in `claude` (the
owner's subscription; no API key is used or needed) and Python 3.9 or later with the standard
library only. `fetch` takes a minute; `validate` about a minute and a half per model (15 calls)
and `run` about twelve per model (120 sample calls and 9 baseline calls), one call at a time with
a pause between them, so all twelve models take about three hours. `--models` takes model ids
(`MODELS` in the script lists the ones measured); `run --models claude-haiku-5-5` re-runs one.
Add a new model to `MODELS` here and in `calibration.rs`. `summarize` prints the table and the
`TABLE` rows for `crates/lumen-core/src/calibration.rs`; paste those, update `AS_OF`,
`CLAUDE_CODE_VERSION` and `MODELS` there, and `cargo test -p lumen-core calibration` checks the
module against `results/table.tsv` and `results/run.json`. The figures quoted in the README, the
Optimizer tab, `docs/efficiency.md` and `benchmarks/README.md` are checked against the same
rows, so they fail the same test until they are updated too.

The script writes nothing but the corpus cache (outside the repository,
`<system temp>/lumen-calibration-cache` by default) and the files in `results/`.

## Method

Claude Code's print mode (`claude -p --output-format json`) reports the provider's `usage` for
the request it sent. Each sample is embedded in one fixed prompt,

```text
Reply with the single word OK.
<sample>
…the sample…
</sample>
```

and the same prompt with nothing between the markers is the baseline. Claude's count for the
sample is the difference of the two prompt totals, where a total is
`input_tokens + cache_creation_input_tokens + cache_read_input_tokens`; the split between the
three depends on the cache, the sum does not. Everything else in the request (Claude Code's
identity line, a one-line system prompt, an account reminder and an environment block, 740 to
1,000 tokens depending on the model) is in both and cancels. The ratio for a sample is that difference over the same
difference counted with `cl100k_base`, so the tokens where the sample meets the markers are
treated alike on both sides. On this corpus `cl100k_base`'s difference equals its count of the
sample alone for 58 samples of 60 and is one token off for the other two.

The reply is one word, so the output is a few tokens and is not part of any figure.

Every call must be one request to the model asked for. One exception is handled rather than
stopped on: when the model stops on safety grounds and Claude Code answers with another model,
the usage is the other model's, so the run logs the call as `fallback`, drops that sample for
that model and goes on, since one sample a model will not answer says nothing about the method.
It happened once in 720 samples: `claude-fable-5` stopped twice on `log-05` (Go's runtime panic
output) and `claude-opus-4-8` answered. Anything else that is not one request (a second turn, a second model) stops the run.

### Isolation

Every call runs with `--no-session-persistence`, from a new empty directory, and the run then
checks `~/.claude/projects`: no transcript of the calibration session may appear, no project
folder for its directory, and no transcript written during the run may hold a record with its
session id or directory. A running Lumen daemon ingests every transcript there into its ledger,
so a calibration call that left one would be counted as the owner's own use. No run left one.
`validate` records its check in `results/validation.json` (`isolation`), and `run` checks again
when it ends: the run of the 5.5 models printed it (no calibration record in the two transcripts
created and the six written while it ran, all of them other sessions' own), and later runs keep
it in `results/isolation.json`.

Nothing else is loaded: `--tools ""`, `--strict-mcp-config` with an empty `--mcp-config`,
`--safe-mode` (no CLAUDE.md, skills, installed plugins, hooks, custom commands or agents),
`--setting-sources ""`, `disableAllHooks`, `--disable-slash-commands` and `--prompt-suggestions
false`. `--name` stops the separate session-title request, which also carried the sample.
Claude Code's built-in plugins load regardless of safe mode; what they add to the request is
part of the fixed part, and the logged request body held nothing else.

Three things in the request could differ between two calls, and each is switched off. The
first was seen in Claude Code's debug log, the other two by logging request bodies through a
local proxy during development (`ANTHROPIC_BASE_URL` pointed at it; it is not part of this
directory):

| what could differ | by | fix |
|---|---|---|
| the attribution line, whose version suffix differed between the two requests of one run | not measured: switched off on sight | `CLAUDE_CODE_ATTRIBUTION_HEADER=0` |
| a per-session path in the environment block | a few tokens, call to call | a fixed `--session-id` (nothing is persisted under it) |
| a `<total_tokens>` line, sent on some calls and not others | 23 or 24 tokens | the setting `totalTokensReminder: "off"` |

### Validation

`validate` checks the method's premises for each model before a sample is trusted
(`results/validation.json`, every call in `results/validate-<model>.tsv`).

| model | (a) empty prompt ×5 | (b) `rust-02` ×2 | (b) `prose-03` ×2 | (c) `x` ×2 | (c) `hello world` ×2 | (c) together − apart |
|---|---:|---|---|---|---|---:|
| `claude-opus-5-5` | 955 ×5 | 2,233 = 2,233 | 1,959 = 1,959 | 2, 2 | 4, 4 | 0 |
| `claude-sonnet-5-5` | 959 ×5 | 2,233 = 2,233 | 1,959 = 1,959 | 2, 2 | 4, 4 | 0 |
| `claude-haiku-5-5` | 957 ×5 | 2,233 = 2,233 | 1,959 = 1,959 | 2, 2 | 4, 4 | 0 |
| `claude-fable-5` | 952 ×5 | 2,233 = 2,233 | 1,959 = 1,959 | 2, 2 | 4, 4 | 0 |
| `claude-opus-5` | 952 ×5 | 2,233 = 2,233 | 1,959 = 1,959 | 2, 2 | 4, 4 | 0 |
| `claude-sonnet-5` | 1004 ×5 | 2,233 = 2,233 | 1,959 = 1,959 | 2, 2 | 4, 4 | 0 |
| `claude-opus-4-8` | 956 ×5 | 2,233 = 2,233 | 1,959 = 1,959 | 2, 2 | 4, 4 | 0 |
| `claude-opus-4-7` | 1002 ×5 | 2,233 = 2,233 | 1,959 = 1,959 | 2, 2 | 4, 4 | 0 |
| `claude-opus-4-6` | 736 ×5 | 1,839 = 1,839 | 1,473 = 1,473 | 2, 2 | 3, 3 | 0 |
| `claude-sonnet-4-6` | 738 ×5 | 1,839 = 1,839 | 1,473 = 1,473 | 2, 2 | 3, 3 | 0 |
| `claude-opus-4-5-20251101` | 871 ×5 | 1,839 = 1,839 | 1,473 = 1,473 | 2, 2 | 3, 3 | 0 |
| `claude-haiku-4-5-20251001` | 771 ×5 | 1,839 = 1,839 | 1,473 = 1,473 | 2, 2 | 3, 3 | 0 |

(a) is the "same prompt, same total" premise. (b) holds for every sample, not only these two:
`run` sends each sample twice and keeps it only if both totals agree, and re-sends the empty
prompt every ten samples and stops if its total moves. (c) is the constant-overhead premise: the
empty sample is the baseline itself, a one-character sample costs two tokens (the character, and
one for the line break that no longer merges with the next), and two samples cost together what
they cost apart, so the overhead does not depend on what is in the prompt.

## Corpus

Sixty samples from ten public repositories, each pinned by commit, path and the file's git blob
id, which `fetch` checks against the download: `corpus.tsv` lists them, with a line range where
only part of a file is used. Sizes run from 69 to 18,479 `cl100k_base` tokens, under about 30,000
of Claude's. Nothing comes from the owner's machine or any private project.

| repository | license | commit | used for |
|---|---|---|---|
| [HackPoint/lumen](https://github.com/HackPoint/lumen) | MIT | `3b951e93dbe55e46dc0e15951666b303110861e7` (the token benchmark's pin) | Rust, TypeScript, shell, JSON, Markdown |
| [react-hook-form/react-hook-form](https://github.com/react-hook-form/react-hook-form) | MIT | `8ac7cc0bcc1f50f887230d50b1929d977b4d3fb4` (v7.90.0, the token benchmark's pin) | TypeScript, TSX, JavaScript, JSON, Markdown |
| [pytest-dev/pytest](https://github.com/pytest-dev/pytest) | MIT | `cf470ec0bf7eb89cd97dd56df4859eae5db46447` (9.1.1, the token benchmark's pin) | Python, reStructuredText |
| [golang/go](https://github.com/golang/go) | BSD-3-Clause | `6885bad7dd86880be6929c02085e5c7a67ff2887` (go1.23.0) | Go, `go test` output and its JSON, a runtime panic log |
| [spf13/cobra](https://github.com/spf13/cobra) | Apache-2.0 | `e94f6d0dd9a5e5738dca6bce03c4b1207ffbc0ec` (v1.8.1) | Go, Markdown |
| [nvm-sh/nvm](https://github.com/nvm-sh/nvm) | MIT | `179d45050be0a71fd57591b0ed8aedf9b177ba10` (v0.40.1) | shell |
| [SchemaStore/schemastore](https://github.com/SchemaStore/schemastore) | Apache-2.0 | `10223f2b993a8297f06b4e7c4f9f55aaf4e3cdd1` | JSON |
| [rust-lang/rust](https://github.com/rust-lang/rust) | MIT or Apache-2.0 | `051478957371ee0084a7c0913941d2a8c4757bb9` (1.80.0) | Rust, `rustc` diagnostics, a lockfile, release notes |
| [microsoft/TypeScript](https://github.com/microsoft/TypeScript) | Apache-2.0 | `c8a7d589e647e19c94150d9892909f3aa93e48eb` (v5.5.4) | `tsc` diagnostics |
| [elastic/examples](https://github.com/elastic/examples) | Apache-2.0 | `6d86454ebb7a850bcd7e80abe86fe683370018a6` | Kafka server and GC logs, an Apache access log |

Content types: `rust`, `python`, `ts_js` (TypeScript and JavaScript, TSX included), `go`, `shell`,
`json`, `log` (compiler diagnostics, `go test` output, a runtime panic, server, GC and access
logs) and `prose` (Markdown and reStructuredText), eight samples each, except seven for `python`
and `go` and six for `shell`.

## Files

| file | content |
|---|---|
| `calibrate.py` | the runner; its docstring repeats the method |
| `corpus.tsv` | the pinned samples |
| `results/counts.tsv` | per sample: source, content type, bytes, `cl100k_base` count alone and in the wrapper |
| `results/calls-<model>.tsv` | every call of the run: kind, sample, attempt, model id, the usage fields, the total |
| `results/samples.tsv` | per sample: source, content type, bytes, `cl100k_base` count, Claude's count per model |
| `results/table.tsv` | per content type and model, and pooled: n, models, median, p10, p90, min, max, as `calibration.rs` embeds it |
| `results/validation.json`, `results/validate-<model>.tsv` | the validation above, and its transcript check |
| `results/isolation.json` | the transcript check at the end of each `run` (from the second run on; the first printed it) |
| `results/run.json` | as-of date, Claude Code version, and per model its id, date and the run's own checks |

## Limits

- **Raw text, not a tool result.** A sample is measured as text in a user message. What Claude is
  charged for a `Read` also includes the line-number prefix on every line and the tool-result
  wrapper, and a Lumen reply has its own header lines; neither is in these ratios.
- **One run per release.** Within a run the totals are repeatable to the token (every sample's two
  sends agreed), but nothing here says how a later Claude Code or model changes them.
- **Sixty samples.** Six to eight per content type: p10 and p90 interpolate between the lowest or
  highest two samples of a cell, so a cell's band is a rough one. The corpus is English, and has no
  CSS, HTML, YAML, SQL, minified code or non-Latin text; a type it does not cover gets the model's
  pooled row with a band over all its types (`calibrate` in `calibration.rs`).
- **Twelve models.** A model not in the table, an alias such as `opus` included, gets the content
  type's row over every model, with a band over every model's row for that type: two tokenizers
  are in the table, so that band spans both.
- **A row over several models counts each text once per model.** `n` in `table.tsv` is the
  number of distinct samples and `models` how many models they were sent to.
