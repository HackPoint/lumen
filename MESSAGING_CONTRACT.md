# Lumen — Honest Messaging Contract

The rule for every surface (Savings screen, README, docs, mascot copy): Lumen's
credibility IS the product. Never inflate. The three layers below are a hierarchy,
shown together, never blurred.

## Layer 1 — Observability is the primary value
Lead with this. Lumen shows the truth about tokens: live context fill, real cost,
caching savings, compaction warnings, usage over time. Useful from second one,
before any optimization. This is the "why install it" message.

## Layer 2 — Effectiveness % is the optimizer's hero metric
When Lumen intercepts a read, it returns ~87% fewer tokens. Lead the optimizer story
with the RATIO (effectiveness %), which is honestly impressive and independent of how
much data exists yet. "Every intercepted read is ~87% cheaper."

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
- Never estimate optimizer savings — only measured full_tokens − returned_tokens.
- builtin_read = "not optimized (read in full)" — context, never counted as savings.
- Channel honesty: CLI = "Full mode" (interception on); VS Code = "Soft mode"
  (tools available, not enforced; bypassed reads invisible). State this plainly.
- The one-line pitch: "Lumen shows you the whole truth about your tokens, and gives
  you verifiable optimization — measured to the token, never inflated."

## What NOT to say
- ✗ "Lumen saved you $X" where X includes caching.
- ✗ "Save up to 90%!" as a headline dollar promise.
- ✗ Any number we can't derive from the DB on demand.
