---
description: Render Lumen's fault report, show it, and file it only after you approve
argument-hint: "[--include-source] [--repo owner/name]"
allowed-tools: Bash(lumen report:*)
---

Render the Lumen fault report and show it to the user before anything is filed.

1. Run `lumen report --dry-run $ARGUMENTS` and show the full body verbatim.
2. Summarise in one or two lines: which fault leads, how many occurrences, and
   whether routing is degraded or merely noisy.
3. If the body is empty (`no faults recorded`), say so and stop. Do not file an
   empty issue.
4. Ask whether to file it. Only if the user says yes, run the same command with
   `--yes` and without `--dry-run`, then report the URL.

Notes:

- The tracker is public and Lumen reads the user's source files. The generated
  body is metadata-only by default: paths outside the Lumen workspace are reduced
  to their extension, and no file contents are attached.
- `--include-source` embeds in-workspace file bodies. It prints a manifest of
  exactly what it will embed to stderr first — show that manifest to the user and
  get explicit approval before filing with it.
- Filing is deduplicated on a fingerprint of `(kind, variant, version)`. A second
  run comments on the existing issue instead of opening a duplicate, so re-running
  is safe.
- Never pass `--yes` on the user's behalf without being asked to file.
