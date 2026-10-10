# Hook payloads captured from Claude Code

These are the stdin payloads Claude Code handed to a hook, not JSON written to match
what the hooks expect. Hand-written fixtures agree with the assumptions of whoever wrote
the hook, and that is the failure mode they exist to catch.

Captured 2026-10-08 from Claude Code 2.1.270 on macOS: a `claude -p` session with
`--setting-sources project,local --strict-mcp-config`. `--settings` registered a single
hook on `Read|Bash` for both `PreToolUse` and `PostToolUse`, and that hook did
`cat > file`. The model was claude-haiku-4-5. The working directory was a scratch
project holding one file, `hello.rs`.

| file | event | what it shows |
| --- | --- | --- |
| `read_pre.json` | PreToolUse Read | the shape the intercept parses |
| `read_post.json` | PostToolUse Read | `tool_response.file.content` / `numLines` / `totalLines` |
| `read_post_partial.json` | PostToolUse Read, `offset: 2, limit: 1` | the response holds only the slice; `totalLines` is still the whole file |
| `bash_pre.json` | PreToolUse Bash | |
| `bash_post.json` | PostToolUse Bash | stderr arrives merged into `stdout`; `"stderr"` is empty |

There is one edit: the account name in paths and in the `ls -la` output was replaced
with `dev`. Every other byte is as captured. Tests replace `cwd`, `tool_input.file_path`
and `tool_response.file.filePath` with a temp file on the platform running them. All
other fields pass through unchanged.

Observed while capturing, and not visible in the files: a Bash command that exits
non-zero fires `PreToolUse` but no `PostToolUse`. `ls -la . ; ls missing-file`
produced a pre payload and nothing after it, so the meter never sees the output of a
failed command.
