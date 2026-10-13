# Hook payloads captured from Claude Code

These are the stdin payloads Claude Code handed to a hook, not JSON written to match
what the hooks expect. Hand-written fixtures agree with the assumptions of whoever wrote
the hook, and that is the failure mode they exist to catch.

Captured 2026-10-08 from Claude Code 2.1.270 on macOS: a `claude -p` session with
`--setting-sources project,local --strict-mcp-config`. `--settings` registered a single
hook on `Read|Bash` for both `PreToolUse` and `PostToolUse`, and that hook did
`cat > file`. The model was claude-haiku-4-5. The working directory was a scratch
project holding one file, `hello.rs`.

Captured 2026-10-13 from Claude Code 2.1.296 on macOS: two `claude -p` runs on haiku with
a scratch config directory and ledger and `--setting-sources project --strict-mcp-config`.
In the first, `--settings` registered two `PreToolUse` hooks on each of
`mcp__lumen__smart_read`, `mcp__lumen__recall_file` and `mcp__lumen__compress_logs`: one
that did `cat > file`, and `lumen-mcp hook intercept`. The prompt had a general-purpose
subagent call `smart_read` on a 331-line file, and then called it itself. The second run
put the `cat > file` hook on `Read` and had a subagent read the same file.

| file | event | what it shows |
| --- | --- | --- |
| `read_pre.json` | PreToolUse Read | the shape the intercept parses |
| `read_post.json` | PostToolUse Read | `tool_response.file.content` / `numLines` / `totalLines` |
| `read_post_partial.json` | PostToolUse Read, `offset: 2, limit: 1` | the response holds only the slice; `totalLines` is still the whole file |
| `bash_pre.json` | PreToolUse Bash | |
| `bash_post.json` | PostToolUse Bash | stderr arrives merged into `stdout`; `"stderr"` is empty |
| `smart_read_pre.json` | PreToolUse `mcp__lumen__smart_read`, main agent | the payload the identity record is made from: `session_id`, `tool_use_id`, no `agent_id` |
| `smart_read_pre_subagent.json` | PreToolUse `mcp__lumen__smart_read`, from a subagent | the parent's `session_id` and an `agent_id` and `agent_type` of its own |
| `read_pre_subagent.json` | PreToolUse Read, from a subagent | the same two keys on a built-in tool, from the second run |

The first five files have one edit: the account name in paths and in the `ls -la` output
was replaced with `dev`. Every other byte is as captured. Tests replace `cwd`,
`tool_input.file_path` and `tool_response.file.filePath` with a temp file on the platform
running them. All other fields pass through unchanged.

The last three are redacted, and only in values: `session_id`, `prompt_id`, `agent_id` and
`tool_use_id` are fixed placeholders of the same shape, `transcript_path` is built from
the placeholder session, `cwd` and the file are the placeholder project of the other
files, and the account name is `dev`. Keys, nesting, order and every other value are as
captured. The first two come from one run and share a session; `read_pre_subagent.json`
has its own. `live_identity.rs` puts the session, agent, `cwd`, path and `tool_use_id` of
each call into them, and in one test hands over the bytes as they are.

Observed while capturing, and not visible in the files: a Bash command that exits
non-zero fires `PreToolUse` but no `PostToolUse`. `ls -la . ; ls missing-file`
produced a pre payload and nothing after it, so the meter never sees the output of a
failed command.

Also not visible in the files, from the 2026-10-13 runs: a subagent's calls carry the
parent's `session_id`, and the parent's own calls never carry `agent_id` or `agent_type`.
`prompt_id` is not the conversation: the parent's first call after a subagent returned
had a new one in both runs, with the same `session_id`. `effort` and `mcp_server` (MCP
tools only) are not in the 2.1.270 files. For an MCP call, `_meta["claudecode/toolUseId"]`
in the `tools/call` request is the payload's `tool_use_id`, checked call by call on the
stdio between Claude Code and `lumen-mcp`.
