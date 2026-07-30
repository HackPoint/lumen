---
name: Fault report
about: A Lumen routing, metering or ingest fault — the shape `lumen report` generates
title: 'lumen <version> — <what fired, how often>'
labels: fault
---

<!--
`lumen report --dry-run` generates this whole body for you, with the environment
block filled in and paths already redacted. Prefer that over filling this in by
hand; this template exists so a hand-written report is diffable against a
generated one.

Redaction: Lumen reads your source files, and this tracker is public. The
generator never attaches file contents and reduces any path outside the Lumen
workspace to its extension. If you are filling this in manually, do the same —
do not paste source, absolute paths, or private project names.
-->

**Impact:** <what is degraded, and what still works>

| kind | variant | files | count | first seen | last seen |
|---|---|---|---|---|---|
| | | | | | |

**Affected files** (metadata only — no contents attached)
- `<repo-relative path>` · <n> lines · <ext> · sha256:<12 hex>

**Details**

```
<the error text, or the decision inputs for a decline>
```

**Environment**
- lumen <version> · git `<sha>`
- <os> <arch> · channel `<cli|vscode>` · MCP scope: <user|project>
- `.mcp.json` declares <n> server(s) · hooks digest `sha256:<8 hex>`
- `read_events` <n> columns
- env overrides in effect: <`LUMEN_*=...`, or none>

<!--
A generated report ends with a fingerprint marker. `lumen report` looks for it to
comment on an existing issue instead of opening a duplicate, so leave it in place
if you copied a generated body:

<!-- lumen-fault: xxxxxxxx -->
-->
