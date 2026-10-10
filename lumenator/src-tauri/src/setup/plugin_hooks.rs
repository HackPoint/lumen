//! The Claude Code plugin's copies of Setup's hooks.
//!
//! The plugin (`.claude-plugin/plugin.json`) registers `hooks/hooks.json`, which runs
//! the two scripts in `.claude/hooks/`. Those are generated, not written: each is
//! [`plugin_script`] applied to the template Setup installs from, and the first test
//! below fails when a checked-in copy differs from it by a byte. They used to be
//! python3 scripts of their own, and they drifted from Setup's until the meter wrote
//! to a ledger with no schema and recorded nothing.
//!
//! Regenerate them with `LUMEN_BLESS_HOOKS=1 cargo test -p Lumen --lib plugin_hooks`.
//! `setup::hook_e2e` runs the checked-in copies the way Claude Code runs a plugin's.

use super::*;

/// Where the plugin's copy says it came from.
const PLUGIN_ORIGIN: &str = "the Claude Code plugin's copy of the hook
# Lumen Setup installs, generated from the same template in setup.rs. Do not
# hand-edit; regenerate with `LUMEN_BLESS_HOOKS=1 cargo test -p Lumen`.";

/// How the plugin's copy finds the binary. The ledger and the spool are left to
/// lumen-mcp, which resolves both the way every other writer does.
const PLUGIN_LOCATE: &str = r#"# Nothing is baked in: a plugin cannot know where Lumen is installed. The binary is
# the one built in this checkout, else lumen-mcp on PATH, and lumen-mcp finds the
# ledger itself. LUMEN_MCP_BIN overrides the first.
here="${BASH_SOURCE[0]%[/\\]*}"
[ "$here" = "${BASH_SOURCE[0]}" ] && here=.
LUMEN_MCP_BIN="${LUMEN_MCP_BIN:-$here/../../target/release/lumen-mcp}""#;

/// The plugin's copy has no baked spool, so before it reports it resolves the one
/// lumen-mcp would have used: `lumen_core::meter::resolve_db_path` and
/// `faults::spool_path`, in shell. Only this path runs it; with the binary present,
/// the binary resolves.
const PLUGIN_SPOOL: &str = r#"# Where lumen-mcp would have put the fault: LUMEN_FAULT_SPOOL, else faults.jsonl
# beside the ledger, which is LUMEN_DB, else ~/.lumen_db_path, else the per-OS one.
if [ -z "${LUMEN_FAULT_SPOOL:-}" ]; then
    db="${LUMEN_DB:-}"
    home="${HOME:-${USERPROFILE:-}}"
    if [ -z "$db" ] && [ -n "$home" ]; then
        db="$(cat "$home/.lumen_db_path" 2>/dev/null)"
        db="${db#"${db%%[![:space:]]*}"}"
        db="${db%"${db##*[![:space:]]}"}"
    fi
    if [ -z "$db" ] && [ -n "$home" ]; then
        case "${OSTYPE:-}" in
            darwin*) db="$home/Library/Application Support/io.speedata.lumen/lumen.db" ;;
            msys* | cygwin*) db="$home/AppData/Roaming/io.speedata.lumen/lumen.db" ;;
            *) db="$home/.local/share/io.speedata.lumen/lumen.db" ;;
        esac
    fi
    case "$db" in
        */* | *\\*) LUMEN_FAULT_SPOOL="${db%[/\\]*}/faults.jsonl" ;;
        ?*) LUMEN_FAULT_SPOOL=faults.jsonl ;;
    esac
fi
"#;

/// A template filled in for the plugin. The writer keeps the name the plugin's meter
/// has always put in `writer_hook`, so its rows stay told apart from Setup's.
fn plugin_script(template: &str) -> String {
    with_stamp(
        &template
            .replace("__LUMEN_ORIGIN__", PLUGIN_ORIGIN)
            .replace("__LUMEN_LOCATE__", PLUGIN_LOCATE)
            .replace("__LUMEN_WRITER__", "repo:.claude/hooks/lumen_meter.sh")
            .replace("__LUMEN_REPORT__", &format!("{PLUGIN_SPOOL}{REPORT_TAIL}")),
    )
}

/// The plugin's scripts, by name under `.claude/hooks/`, and their templates.
const SCRIPTS: [(&str, &str); 2] = [
    ("lumen_meter.sh", METER_TEMPLATE),
    ("lumen_read_intercept.sh", INTERCEPT_TEMPLATE),
];

/// The repository, which is the plugin's root.
fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The `command` hooks.json registers for a script: its path under the plugin root
/// as one double-quoted word. Claude Code puts the root in the hook's environment as
/// `CLAUDE_PLUGIN_ROOT` and leaves the expansion to the shell, so the quotes keep a
/// root with a space in it one word, and a `$` or backtick in it stays literal.
fn plugin_command(script: &str) -> String {
    format!("\"${{CLAUDE_PLUGIN_ROOT}}/.claude/hooks/{script}\"")
}

/// Every byte, the stamp included: the version on it is the one a fault from the
/// script reports, and `scripts/release.sh` moves it with the others.
#[test]
fn the_plugin_hooks_are_what_setups_templates_generate() {
    let bless = std::env::var_os("LUMEN_BLESS_HOOKS").is_some_and(|v| v == "1");
    for (name, template) in SCRIPTS {
        let path = repo().join(".claude/hooks").join(name);
        let want = plugin_script(template);
        if bless && std::fs::read_to_string(&path).ok().as_deref() != Some(want.as_str()) {
            std::fs::write(&path, &want).unwrap();
            set_mode(&path, 0o755).unwrap();
            eprintln!("blessed {}", path.display());
        }
        let have =
            std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        if have != want {
            let line = have.lines().zip(want.lines()).position(|(h, w)| h != w);
            panic!(
                "{name} is not what its template generates (first difference: line {}) — \
                 regenerate it with LUMEN_BLESS_HOOKS=1 cargo test -p Lumen --lib plugin_hooks",
                line.map_or_else(
                    || "past the shorter end".to_string(),
                    |i| (i + 1).to_string()
                )
            );
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(
                mode & 0o111,
                0o111,
                "{name}: Claude Code runs it by path, so it must be executable ({mode:o})"
            );
        }
    }
}

/// hooks.json registers what Setup registers, nothing more: the intercept before a
/// Read, the meter after a Read or a Bash, and so none of the matchers Setup retired.
#[test]
fn the_plugin_registers_the_hooks_setup_registers() {
    let path = repo().join("hooks/hooks.json");
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let hooks = v["hooks"].as_object().expect("a hooks object");
    let mut phases: Vec<&str> = hooks.keys().map(String::as_str).collect();
    phases.sort_unstable();
    assert_eq!(phases, ["PostToolUse", "PreToolUse"]);

    let registered = |phase: &str| -> Vec<(String, Vec<serde_json::Value>)> {
        hooks[phase]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| {
                (
                    e["matcher"].as_str().unwrap().to_string(),
                    e["hooks"].as_array().unwrap().clone(),
                )
            })
            .collect()
    };
    let runs = |script: &str| {
        vec![serde_json::json!({"type": "command", "command": plugin_command(script)})]
    };
    assert_eq!(
        registered("PreToolUse"),
        [(
            INTERCEPT_MATCHER.to_string(),
            runs("lumen_read_intercept.sh")
        )]
    );
    assert_eq!(
        registered("PostToolUse"),
        METER_MATCHERS.map(|m| (m.to_string(), runs("lumen_meter.sh")))
    );

    // On Windows Claude Code runs a command through `bash` only when its first word,
    // read its way, names a `.sh`. Quoted whole, it does.
    for (name, _) in SCRIPTS {
        assert_eq!(
            hook_command_path(&plugin_command(name)),
            format!("${{CLAUDE_PLUGIN_ROOT}}/.claude/hooks/{name}")
        );
    }
}
