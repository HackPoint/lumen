# Hook scripts as Lumen 1.5.1 installed them

These are the two scripts a 1.5.1 Setup wrote to `~/.claude/lumen/`. They were copied
from a real 1.5.1 install on macOS, not regenerated from the 1.5.1 source, so they are
what an upgrading user actually has on disk.

The upgrade test (`setup::hook_e2e`) puts them in a temp home next to a 1.5.1-shaped
`settings.json` and `~/.claude.json`, then checks what 1.6.0 does to them: the scripts
are rewritten, the configs are left byte-identical until Setup is re-run, and a re-run
keeps the user's settings and other MCP servers.

There is one edit: the home directory in each script's default paths was replaced with
`__HOME__`. The test substitutes the temp home back in. Every other byte is as installed.

| file | what it shows |
| --- | --- |
| `lumen_meter.sh` | the python3 meter, with a baked `LUMEN_DB` and the `LUMEN_TOK` tokenizer path |
| `lumen_read_intercept.sh` | the python3 intercept, with a baked `LUMEN_MCP_BIN` and fault spool |
