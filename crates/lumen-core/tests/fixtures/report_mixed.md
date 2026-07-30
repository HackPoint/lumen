### lumen 1.4.0 — retry escape valve fired 7× on 2 files

**Impact:** The Read intercept redirected to lumen, lumen did not serve the call, and a fail-open guard released the Read. Routing is degraded, not broken — context was spent that lumen was supposed to save.

| kind | variant | files | count | first seen | last seen |
|---|---|---|---|---|---|
| hook_fail_open | retry_escape_valve | 2 | 7 | 07-29 14:02 | 07-30 09:41 |
| hook_fail_open | lumen_mcp_missing | 1 | 1 | 07-30 09:00 | 07-30 09:00 |
| schema_drift | — | — | 1 | 07-30 06:00 | 07-30 06:00 |
| ingest_failed | — | 1 | 12 | 07-30 04:10 | 07-30 04:55 |
| ws_restart | — | — | 3 | 07-29 22:14 | 07-30 05:31 |
| ranked_decline | ranked_no_query | 1 | 403 | 07-29 08:20 | 07-30 07:02 |
| ranked_decline | ranked_too_slow | 1 | 214 | 07-29 08:15 | 07-30 09:38 |

**Affected files** (metadata only — no contents attached)
- `<redacted:external>.jsonl` · ? lines · jsonl · sha256:unavailable
- `<redacted:external>.ts` · 683 lines · ts · sha256:unavailable
- `<redacted:external>.tsx` · 412 lines · tsx · sha256:unavailable
- `crates/lumen-core/src/ranked.rs` · 1909 lines · rs · sha256:unavailable
- `crates/lumen-mcp/src/lib.rs` · 1559 lines · rs · sha256:unavailable

**Details**

`schema_drift`
```
expected 24 columns, found 22
missing: econ_source, k_selected
```

`ingest_failed`
```
no such column: is_subagent
```

`ws_restart`
```
Address already in use (os error 48)
```

`ranked_decline` / `ranked_no_query`
```
lang=tsx tag_query=absent
```

`ranked_decline` / `ranked_too_slow`
```
budget=12000 s_min=9400 k=0/41
wall_clock_ms=812 ceiling_ms=750
```

**Environment**
- lumen 1.4.0 · git `703c1f2`
- macos aarch64 · channel `cli` · MCP scope: user (~/.claude.json)
- `.mcp.json` declares 0 server(s) · hooks digest `sha256:c41afe09`
- `read_events` 24 columns
- env overrides in effect: `LUMEN_LINE_THRESHOLD=300`

<!-- lumen-fault: 71606613 -->
