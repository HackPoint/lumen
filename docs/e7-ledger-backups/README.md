# Pre-unification ledger backups (E7)

Snapshots of both metering databases as they stood immediately before the E7 ledger
unification, kept so that unification can be reversed or independently re-derived.

| file | rows | what it was |
|---|---|---|
| `app_before_e7.db` | 4,140 read_events | `~/Library/Application Support/io.speedata.lumen/lumen.db` — the ledger the GUI reads |
| `repo_before_e7.db` | 195 read_events | `<repo>/lumen.db` — a second ledger written whenever the hook ran with `LUMEN_DB` unset |

`SHA256SUMS.txt` records, in order: the two pre-state checksums, the post-unification
checksum of the app database, and a second post-state line after `writer_hook` was
corrected on four MCP-written rows.

**The `.db` files are deliberately not in git.** They are 18 MB of binary snapshots and
the repository's `.gitignore` excludes `*.db`. The checksums are committed instead, so
the backups remain verifiable without putting binaries in history. Do not delete the
directory — the pre-state is the only record of the 32 values the unification changed.

## Why two databases existed

Two hook installations were live at once. The repo copy resolved
`LUMEN_DB="${LUMEN_DB:-$WORKSPACE_ROOT/lumen.db}"`, so with the variable unset it wrote
beside the checkout; the installed copy had the Application Support path baked in at
setup time. 146 events were recorded in both.

## Why 32 rows changed

Where a pair diverged, the repo copy held the better number. Its `LUMEN_TOK` resolved to
a working tokenizer, while the installed copy's pointed inside an ejected disk image and
silently fell back to `bytes ÷ 4`. The unification adopted the measured value and
recorded provenance on the survivor. Divergence appears only in `full_tokens` and
`tokens_returned`; `tool`, `lines`, `saved_tokens` and `channel` never differ, which is
what identifies these as one event measured twice rather than two distinct events.
