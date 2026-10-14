# Lumen — Honest Messaging Contract

The rule for every surface (Savings screen, README, docs, mascot copy): Lumen's
credibility IS the product. Never inflate. The three layers below are a hierarchy,
shown together, never blurred.

## Layer 1 — Observability is the primary value
Lead with this. Lumen shows the truth about tokens: live context fill, real cost,
caching savings, compaction warnings, usage over time. Useful from second one,
before any optimization. This is the "why install it" message.

## Layer 2 — Effectiveness is shown, never quoted
The optimizer's effectiveness is computed from the ledger where it is displayed, never
written into copy as a constant. This layer used to say "Every intercepted read is ~87%
cheaper", and the model was told "~5-10% token cost". A ratio measured on one machine's
files is not a property of the product, and the model routes on what it is told, so an
inflated constant biases its choices as well as the reader's. The dollar value is derived per
call from the user's own ledger (`lumen_core::savings`), at the price of the model that consumed
the tokens, and shown as a band. The figure shown is the one "on record", a floor, said as "at
least": priced from the turns the provider's usage data records, on token counts converted to
Claude's by the calibration, at the low end of their band: the file at the smallest ratio any
sample of its row came out at, less Lumen's answer (a kind of text no row measured) at the
largest any sample the model was sent came out at, never at a percentile. What may be worth
more (an `R` that may be too long) is shown beside it, labelled modelled, and never added to
it; a call with no turn of its own to price it by has tokens and no dollars, never a price
borrowed from other calls. No dollar figure is published outside the app and the CLI, which
compute it from the reader's own ledger.

## Layer 3 — Absolute numbers, side by side, never merged
Show two clearly distinct figures:
- "Lumen optimized (caused)"  = SUM(read_events.saved_tokens) over lumen tools. Small
  but 100% verifiable. Grows with use.
- "Saved by caching (reported by Claude Code)" = from turns.cache_read. NOT caused by
  Lumen. Orders of magnitude larger — and that's fine, because we never claim it.
Two labels, two numbers, one shared price source. Never add them together. Never
imply the caching number is Lumen's doing.

## Hard rules
- Never present "Saved by caching" as a Lumen achievement.
- Never guess optimizer savings — only counted full_tokens − returned_tokens: no bytes/4,
  no extrapolation.
- Say what a token figure is. Lumen counts with `cl100k_base`, a local stand-in for
  Claude's tokenizer, so its counts are deterministic but are not Claude's: quote them
  with the calibration (`lumen_core::calibration`, `benchmarks/calibration`) and its
  error, never as exact. Savings figures are Claude's tokens converted that way ("≈", with
  the band on hover, in `lumen report` and on How it's measured).
- "Measured" is for provider usage data, which Claude Code's transcripts carry as the API
  reported it, and for the calibration, which is measured against it. The dollar floor rests
  on calibrated estimates as well as on that data, so every surface calls it "on record"
  ("at least $X saved, net", "no dollars on record", "Net on record"), never "measured". The
  summary's field names predate the word and keep it: `Period.measured` and a call line's
  `dollars: "measured"` mean "on record". `crates/lumen-core/tests/proxy_count_claims.rs`
  fails on user-facing text that calls the floor measured.
- builtin_read = "not optimized (read in full)" — context, never counted as savings.
- Channel honesty: CLI = "Full mode" (interception on); VS Code = "Soft mode"
  (tools available, not enforced; bypassed reads invisible). State this plainly.
- The one-line pitch: "Lumen shows you the whole truth about your tokens, and gives
  you verifiable optimization — counted on every call, never inflated."

## What NOT to say
- ✗ "Lumen saved you $X" where X includes caching.
- ✗ "Save up to 90%!" as a headline dollar promise.
- ✗ Any number we can't derive from the DB on demand.
