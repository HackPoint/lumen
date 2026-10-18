# Hook scripts from earlier releases

| directory | what it is |
| --- | --- |
| `v1.5.1/` | copied from a real 1.5.1 install (python3 scripts); see its README |
| `v1.8.0/`, `v1.9.0/` | the shims those tags' Setup generated (`desired_meter_script` and `desired_intercept_script`), byte for byte |

The 1.8.0 and 1.9.0 shims are generated, not copied from a machine, with two placeholders
in the baked defaults:

- `__APP_DIR__`: the data directory the install baked. The upgrade tests in
  `setup::hook_e2e` put the legacy directory there, taken from `legacy::LEGACY_APP_ID` at
  test time, so no fixture carries the old identifier.
- `__MCP__`: the `lumen-mcp` the install baked.

The two releases' shims differ only in the `lumen-generator` stamp.
