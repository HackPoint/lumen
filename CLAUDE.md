# Lumen workspace — Claude Code guidance

## Context optimizer — routing rules (FOLLOW THESE)

Lumen ships MCP tools that read source files and logs at a fraction of the token
cost of the built-in Read. Use them as described below.

### Source files (Rust, Python, TypeScript, TSX)

For any source file **≥ 300 lines**, always prefer the lumen tools over built-in Read:

1. `lumen:smart_read(path="<file>")` — returns a structural outline (functions,
   classes, structs, interfaces, imports with exact line ranges) instead of the
   file's bodies. Start here whenever you need to understand what a file contains.

2. `lumen:recall_file(path="<file>", names=["<name>"])` — fetches a specific
   named item (function, class, struct) plus 3 lines of context, without reading
   the rest. Use after smart_read once you know which item you need.

3. `lumen:recall_file(path="<file>", start_line=N, end_line=M)` — fetches an
   explicit line range. Use when you already know the exact location.

Use the built-in Read only for files **< 300 lines** or when you genuinely need
every line of a large file (rare; start with smart_read first to confirm).

### Logs, stack traces, build output

For any log file, crash dump, or verbose build/test output:

`lumen:compress_logs(path="<file>")` or `lumen:compress_logs(text="<pasted text>")`

This collapses repeated identical lines, stack frame runs, and blank-line runs
deterministically. It is not lossless: the middle of a long stack trace is replaced
by a count of the frames dropped. Always compress before analyzing; the full file is
available via smart_read(mode="full") if needed.

### Why

Each call reports what it saved, measured against a full read of the same file,
in `_meta.saved_tokens`. No typical figure is given here: what an outline saves
varies by file, and the ledger is where it is measured.

## Authorship rule (ENFORCE — no exceptions)

**HackPoint is the sole author and contributor of this project.**

- Do NOT add Claude, Anthropic, or any AI tool as an author, contributor, or
  co-author anywhere: commits (`Co-Authored-By:`), code comments, docs, metadata
  (`authors` in Cargo.toml / package.json), footers, or generated files.
- Do NOT add `🤖 Generated with Claude Code` footers or similar attribution lines.
- Do NOT add `Signed-off-by:` lines for any non-human entity.
- **Allowed:** product references to "Claude Code" (the tool Lumen monitors),
  model name strings (`claude-sonnet-4-6` etc.), and `~/.claude/` path references
  — these describe the third-party tool, not authorship of Lumen.
- Git identity for all commits: `HackPoint <6758579+HackPoint@users.noreply.github.com>`.
