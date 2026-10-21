//! The re-point and the Setup prompt after a v2 migration, on a real committed migration
//! with neutral folder names.

use super::*;
use crate::setup::{
    hook_command, marker_path_in, setup_done_text, setup_needed_with, validate_mcp_with,
};
use lumen_core::legacy::commit::{self, CommitOptions, Outcome};
use lumen_core::legacy::marker::Seen;
use lumen_core::legacy::retire::Native;
use serde_json::json;
use tempfile::TempDir;

const OLD: &str = "example.previous.app";
const NEW: &str = "example.new.app";

struct Rig {
    _t: TempDir,
    home: PathBuf,
    /// The previous folder, which the migration moved away.
    l: PathBuf,
    n: PathBuf,
    marker: MarkerV2,
    /// This build's lumen-mcp (a file that exists).
    mcp: PathBuf,
}

impl Rig {
    fn new() -> Rig {
        Rig::with_root("data")
    }

    /// `root` is the data root's folder name, so a test can put a space in it.
    fn with_root(root: &str) -> Rig {
        let t = TempDir::new().unwrap();
        let home = t.path().join("home");
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        let root = t.path().join(root);
        let l = root.join(OLD);
        std::fs::create_dir_all(l.join("bin")).unwrap();
        std::fs::write(l.join("bin/lumen-mcp"), "old binary").unwrap();
        lumen_core::meter::insert_read_event_at(
            &l.join("lumen.db"),
            "/old/0",
            Some(1),
            10,
            20,
            10,
            "builtin_read",
            "cli",
            "Read",
            Some("s1"),
            Some(1),
            None,
            &Default::default(),
        )
        .unwrap();
        let n = root.join(NEW);
        let marker =
            match commit::migrate(&l, &n, &CommitOptions::new("2026-10-19T08:00:00Z"), &Native) {
                Outcome::Migrated { marker, .. } => marker,
                other => panic!("the migration did not commit: {other:?}"),
            };
        assert!(!l.exists(), "the previous folder moved into the backup");
        let mcp = t.path().join("lumen-mcp");
        std::fs::write(&mcp, "").unwrap();
        Rig {
            _t: t,
            home,
            l,
            n,
            marker,
            mcp,
        }
    }

    fn new_db(&self) -> PathBuf {
        self.n.join("lumen.db")
    }

    /// Where the previous folder's files are now.
    fn backup_data(&self) -> PathBuf {
        backup_dir(&self.n, &self.marker).unwrap().join(DATA_DIR)
    }

    fn claude_json(&self) -> PathBuf {
        claude_json_path_in(&self.home)
    }

    fn settings(&self) -> PathBuf {
        global_settings_path_in(&self.home)
    }

    fn write(&self, path: &Path, v: &Value) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, serde_json::to_vec_pretty(v).unwrap()).unwrap();
    }

    fn read(&self, path: &Path) -> Value {
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
    }

    fn run(&self) -> Report {
        repoint_lumen_entries_in(
            &self.home,
            &self.marker,
            &self.mcp.to_string_lossy(),
            &self.new_db(),
        )
    }

    fn seen(&self) -> Seen {
        Seen::Migrated(std::sync::Arc::new(self.marker.clone()))
    }

    fn lumen_entry(&self) -> Value {
        json!({"type": "stdio",
               "command": self.l.join("bin/lumen-mcp").to_string_lossy(),
               "args": [],
               "env": {"LUMEN_DB": self.l.join("lumen.db").to_string_lossy()}})
    }
}

fn lumen_hook(command: &str) -> Value {
    json!({"matcher": "Read", "hooks": [{"type": "command", "command": command}]})
}

#[test]
fn startup_repoints_only_lumens_entries_by_command() {
    let r = Rig::new();
    let foreign_mcp = json!({"type": "stdio",
        "command": r.l.join("bin/other-tool").to_string_lossy(),
        "args": [r.l.join("x").to_string_lossy()],
        "env": {"LUMEN_DB": r.l.join("lumen.db").to_string_lossy()}});
    let foreign_hook = lumen_hook(&format!("'{}'", r.l.join("their-hook.sh").display()));
    let shim = r.home.join(".claude/lumen/lumen_meter.sh");
    let ours_hook = lumen_hook(&hook_command(&shim));
    r.write(
        &r.claude_json(),
        &json!({"numStartups": 3, "mcpServers": {"lumen": r.lumen_entry(), "other": foreign_mcp}}),
    );
    r.write(
        &r.settings(),
        &json!({"model": "opus", "hooks": {"PostToolUse": [ours_hook, foreign_hook]},
                "env": {"THEIR_VAR": r.l.join("y").to_string_lossy()}}),
    );
    let settings_before = r.read(&r.settings());

    // What v1 left: the command points into the moved folder, so it is dangling.
    let v = validate_mcp_with(&r.home, &r.n, &r.seen());
    assert!(!v.healthy && v.detail.contains("dangling"), "{v:?}");

    let report = r.run();
    assert_eq!(report.problems, Vec::<String>::new());
    assert_eq!(
        report.rewritten,
        vec!["claude.json"],
        "only the file that had something"
    );

    let cfg = r.read(&r.claude_json());
    let lumen = &cfg["mcpServers"]["lumen"];
    assert_eq!(lumen["command"], r.mcp.to_string_lossy().as_ref());
    assert_eq!(
        lumen["env"]["LUMEN_DB"],
        r.new_db().to_string_lossy().as_ref()
    );
    assert_eq!(
        cfg["mcpServers"]["other"], foreign_mcp,
        "a foreign entry is untouched"
    );
    assert_eq!(cfg["numStartups"], 3);
    assert_eq!(r.read(&r.settings()), settings_before);
    let v = validate_mcp_with(&r.home, &r.n, &r.seen());
    assert!(v.healthy, "{v:?}");
}

#[test]
fn an_entry_under_another_key_or_in_a_project_scope_is_found() {
    let r = Rig::new();
    let mut by_command = r.lumen_entry();
    by_command["env"] = json!({"LUMEN_DB": r.backup_data().join("lumen.db").to_string_lossy()});
    let key_only = json!({"type": "stdio", "command": "/somewhere/else/wrapper",
        "env": {"LUMEN_DB": r.l.join("lumen.db").to_string_lossy()}});
    r.write(
        &r.claude_json(),
        &json!({
            "mcpServers": {"my-meter": by_command, "lumen": key_only},
            "projects": {"/work/app": {"mcpServers": {"renamed": r.lumen_entry()}},
                         "/work/none": {"allowedTools": []}},
        }),
    );
    let report = r.run();
    assert_eq!(report.problems, Vec::<String>::new());
    let cfg = r.read(&r.claude_json());
    let db = r.new_db().to_string_lossy().to_string();
    assert_eq!(
        cfg["mcpServers"]["my-meter"]["env"]["LUMEN_DB"],
        db.as_str()
    );
    assert_eq!(
        cfg["mcpServers"]["my-meter"]["command"],
        r.mcp.to_string_lossy().as_ref()
    );
    assert_eq!(cfg["mcpServers"]["lumen"]["env"]["LUMEN_DB"], db.as_str());
    assert_eq!(
        cfg["mcpServers"]["lumen"]["command"], "/somewhere/else/wrapper",
        "a command outside the moved folders stays"
    );
    assert_eq!(
        cfg["projects"]["/work/app"]["mcpServers"]["renamed"]["env"]["LUMEN_DB"],
        db.as_str()
    );
    assert_eq!(cfg["projects"]["/work/none"], json!({"allowedTools": []}));
}

#[test]
fn foreign_entries_and_keys_are_value_identical() {
    let r = Rig::new();
    let inside = r.l.join("anything").to_string_lossy().to_string();
    let original = json!({
        "numStartups": 9,
        "history": [{"display": "x", "n": 1.5}],
        "mcpServers": {
            "lumen": r.lumen_entry(),
            "github": {"type": "stdio", "command": "/usr/local/bin/github-mcp", "args": ["--x", inside],
                       "env": {"TOKEN": inside}},
            "lumenish": {"type": "stdio", "command": "/usr/bin/lumen-mcp-not", "args": [inside]},
            "remote": {"type": "http", "url": "https://example.test/mcp"},
        },
        "projects": {"/p": {"mcpServers": {"x": {"command": "/bin/true", "args": [inside]}},
                            "lastCost": 0.25}},
    });
    r.write(&r.claude_json(), &original);
    r.run();
    let mut after = r.read(&r.claude_json());
    let mut expected = original.clone();
    // Everything but Lumen's own entry is equal, value for value.
    after["mcpServers"].as_object_mut().unwrap().remove("lumen");
    expected["mcpServers"]
        .as_object_mut()
        .unwrap()
        .remove("lumen");
    assert_eq!(after, expected);
}

#[test]
fn a_command_inside_the_old_folder_becomes_this_builds_lumen_mcp_never_the_backups_bin() {
    let r = Rig::new();
    // The migration moved the old binary to B/data/bin; a config written after that, or a
    // path mapped into it, must still never end up as the command.
    let in_backup = r.backup_data().join("bin/lumen-mcp");
    assert!(in_backup.exists());
    let mut e1 = r.lumen_entry();
    e1["command"] = json!(in_backup.to_string_lossy());
    r.write(
        &r.claude_json(),
        &json!({"mcpServers": {"lumen": r.lumen_entry(), "b": e1}}),
    );
    r.run();
    let cfg = r.read(&r.claude_json());
    for k in ["lumen", "b"] {
        assert_eq!(
            cfg["mcpServers"][k]["command"],
            r.mcp.to_string_lossy().as_ref(),
            "{k}"
        );
    }
    assert!(!cfg
        .to_string()
        .contains(&*r.backup_data().to_string_lossy().replace('\\', "\\\\")));

    // This build has no lumen-mcp to point at: the command is left and the skip said so.
    r.write(
        &r.claude_json(),
        &json!({"mcpServers": {"lumen": r.lumen_entry()}}),
    );
    let report = repoint_lumen_entries_in(&r.home, &r.marker, "", &r.new_db());
    assert!(
        report.rewritten.is_empty() || !report.problems.is_empty(),
        "{report:?}"
    );
    assert!(
        report.problems.iter().any(|p| p.contains("lumen-mcp")),
        "{report:?}"
    );
}

#[test]
fn hook_commands_are_found_by_their_script_and_requoted() {
    // A space in the folder makes the command need its quotes.
    let r = Rig::with_root("data root");
    let shim = |name: &str| r.home.join(".claude/lumen").join(name);
    let in_l = r.l.join("bin/lumen_meter.sh");
    let mcp_hook = format!("'{}' hook", r.l.join("bin/lumen-mcp").display());
    r.write(
        &r.settings(),
        &json!({
            "hooks": {
                "PreToolUse": [lumen_hook(&hook_command(&shim("lumen_read_intercept.sh")))],
                "PostToolUse": [
                    lumen_hook(&format!("'{}'", in_l.display())),
                    lumen_hook(&format!("\"{}\" hook --flag", r.l.join("bin/lumen-mcp.exe").display())),
                    lumen_hook(&mcp_hook),
                    lumen_hook("/usr/local/bin/audit-read"),
                ],
                "Stop": [{"hooks": [{"type": "command", "command": hook_command(&in_l)}]}],
            },
            "env": {"LUMEN_METER_BASH": "0", "LUMEN_DB": r.l.join("lumen.db").to_string_lossy(),
                    "OTHER": r.l.join("z").to_string_lossy()},
        }),
    );
    let report = r.run();
    assert_eq!(report.problems, Vec::<String>::new());
    assert_eq!(report.rewritten, vec!["settings.json"]);
    let s = r.read(&r.settings());
    let cmd = |ev: &str, i: usize| {
        s["hooks"][ev][i]["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .to_string()
    };

    // The shim under ~/.claude/lumen was never in the moved folder: untouched.
    assert_eq!(
        cmd("PreToolUse", 0),
        hook_command(&shim("lumen_read_intercept.sh"))
    );
    // A shim inside the moved folder is the same file in N, as one quoted word.
    let moved_shim = r.n.join("bin/lumen_meter.sh");
    assert_eq!(cmd("PostToolUse", 0), hook_command(&moved_shim));
    assert!(
        cmd("PostToolUse", 0).starts_with('\''),
        "quoted so a space does not split it"
    );
    // `lumen-mcp hook` runs this build's binary, and what follows the program is kept.
    assert_eq!(
        cmd("PostToolUse", 1),
        format!("{} hook --flag", hook_command(&r.mcp))
    );
    assert_eq!(
        cmd("PostToolUse", 2),
        format!("{} hook", hook_command(&r.mcp))
    );
    assert_eq!(cmd("PostToolUse", 3), "/usr/local/bin/audit-read");
    assert_eq!(
        s["hooks"]["Stop"][0]["hooks"][0]["command"],
        hook_command(&moved_shim).as_str()
    );
    // On Windows the command carries forward slashes, which Git Bash and Claude Code's scan
    // both read as the path.
    if cfg!(windows) {
        assert!(
            !cmd("PostToolUse", 0).contains('\\'),
            "{}",
            cmd("PostToolUse", 0)
        );
    }
    // The top-level LUMEN_* values follow; another variable does not.
    assert_eq!(s["env"]["LUMEN_METER_BASH"], "0");
    assert_eq!(s["env"]["LUMEN_DB"], r.new_db().to_string_lossy().as_ref());
    assert_eq!(s["env"]["OTHER"], r.l.join("z").to_string_lossy().as_ref());
}

#[test]
fn a_file_changed_during_the_rewrite_is_reread_not_clobbered() {
    let r = Rig::new();
    r.write(
        &r.claude_json(),
        &json!({"mcpServers": {"lumen": r.lumen_entry()}}),
    );
    // Claude Code writes the file once while the rewrite is between building and renaming.
    let mut writes = 0;
    let path = r.claude_json();
    let report = repoint_with(
        &r.home,
        &r.marker,
        &r.mcp.to_string_lossy(),
        &r.new_db(),
        &mut |p| {
            if p == path && writes == 0 {
                writes += 1;
                let mut v: Value = serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap();
                v["claudeWrote"] = json!("this");
                std::fs::write(p, serde_json::to_vec_pretty(&v).unwrap()).unwrap();
            }
        },
    );
    assert_eq!(report.problems, Vec::<String>::new());
    let cfg = r.read(&path);
    assert_eq!(
        cfg["claudeWrote"], "this",
        "the other writer's change survived"
    );
    assert_eq!(
        cfg["mcpServers"]["lumen"]["command"],
        r.mcp.to_string_lossy().as_ref()
    );

    // A writer that never stops: skipped and said so, the file left as it was.
    r.write(&path, &json!({"mcpServers": {"lumen": r.lumen_entry()}}));
    let mut n = 0;
    let report = repoint_with(
        &r.home,
        &r.marker,
        &r.mcp.to_string_lossy(),
        &r.new_db(),
        &mut |p| {
            if p == path {
                n += 1;
                let mut v: Value = serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap();
                v["tick"] = json!(n);
                std::fs::write(p, serde_json::to_vec_pretty(&v).unwrap()).unwrap();
            }
        },
    );
    assert_eq!(n, ATTEMPTS, "it tried {ATTEMPTS} times, no more");
    assert!(
        report.problems.iter().any(|p| p.contains("kept changing")),
        "{report:?}"
    );
    assert!(report.rewritten.is_empty());
    assert_eq!(
        r.read(&path)["mcpServers"]["lumen"]["command"],
        r.l.join("bin/lumen-mcp").to_string_lossy().as_ref()
    );
}

#[test]
fn an_unparseable_file_is_left_and_reported() {
    let r = Rig::new();
    std::fs::write(r.claude_json(), "{ \"mcpServers\": { \"lumen\": ").unwrap();
    let before = std::fs::read(r.claude_json()).unwrap();
    r.write(
        &r.settings(),
        &json!({"env": {"LUMEN_DB": r.l.join("lumen.db").to_string_lossy()}}),
    );
    let report = r.run();
    assert_eq!(std::fs::read(r.claude_json()).unwrap(), before);
    assert!(
        report.problems.iter().any(|p| p.contains("not valid JSON")),
        "{report:?}"
    );
    // The other file is still done, and a file that is not an object is left too.
    assert_eq!(report.rewritten, vec!["settings.json"]);
    std::fs::write(r.claude_json(), "[1, 2]").unwrap();
    let report = r.run();
    assert!(
        report
            .problems
            .iter()
            .any(|p| p.contains("not a JSON object")),
        "{report:?}"
    );
    assert_eq!(std::fs::read(r.claude_json()).unwrap(), b"[1, 2]");
    // No file at all is nothing to do.
    std::fs::remove_file(r.claude_json()).unwrap();
    assert_eq!(r.run().problems, Vec::<String>::new());
}

#[test]
fn the_rewrite_is_idempotent_and_the_original_is_copied_once() {
    let r = Rig::new();
    r.write(
        &r.claude_json(),
        &json!({"mcpServers": {"lumen": r.lumen_entry()}}),
    );
    let original = std::fs::read(r.claude_json()).unwrap();

    let first = r.run();
    assert_eq!(first.rewritten, vec!["claude.json"]);
    let kept = backup_dir(&r.n, &r.marker)
        .unwrap()
        .join("claude/claude.json");
    assert_eq!(
        std::fs::read(&kept).unwrap(),
        original,
        "the original, byte for byte"
    );
    let after = std::fs::read(r.claude_json()).unwrap();
    let bak = r.claude_json().with_extension("json.lumen_bak");
    assert_eq!(
        std::fs::read(&bak).unwrap(),
        original,
        "back_up made its copy too"
    );

    // Nothing left to change: no write at all (the file's bytes and mtime stay).
    let mtime = std::fs::metadata(r.claude_json())
        .unwrap()
        .modified()
        .unwrap();
    let second = r.run();
    assert_eq!(second, Report::default());
    assert_eq!(std::fs::read(r.claude_json()).unwrap(), after);
    assert_eq!(
        std::fs::metadata(r.claude_json())
            .unwrap()
            .modified()
            .unwrap(),
        mtime
    );

    // Something new to change later: the kept original stays the first one.
    let mut v = r.read(&r.claude_json());
    v["mcpServers"]["lumen"]["env"]["LUMEN_FAULT_SPOOL"] =
        json!(r.backup_data().join("faults.jsonl").to_string_lossy());
    r.write(&r.claude_json(), &v);
    let third = r.run();
    assert_eq!(third.rewritten, vec!["claude.json"]);
    assert_eq!(std::fs::read(&kept).unwrap(), original);
    assert_eq!(
        r.read(&r.claude_json())["mcpServers"]["lumen"]["env"]["LUMEN_FAULT_SPOOL"],
        r.n.join("faults.jsonl").to_string_lossy().as_ref()
    );
}

#[test]
fn a_copy_that_cannot_be_kept_leaves_the_file_alone() {
    let r = Rig::new();
    r.write(
        &r.claude_json(),
        &json!({"mcpServers": {"lumen": r.lumen_entry()}}),
    );
    let before = std::fs::read(r.claude_json()).unwrap();
    // A file where the backup's claude/ folder should be.
    std::fs::write(backup_dir(&r.n, &r.marker).unwrap().join("claude"), "x").unwrap();
    let report = r.run();
    assert_eq!(std::fs::read(r.claude_json()).unwrap(), before);
    assert!(
        report
            .problems
            .iter()
            .any(|p| p.contains("cannot keep a copy")),
        "{report:?}"
    );
    assert!(report.rewritten.is_empty());
}

#[test]
fn a_tail_that_walks_out_of_the_old_folder_is_not_inside() {
    let r = Rig::new();
    let sneaky = format!("{}/../elsewhere/x", r.l.display());
    let m = Moved::of(&r.marker, &r.n).unwrap();
    assert!(!m.inside(&sneaky));
    assert!(m.inside(&r.l.join("a/b").to_string_lossy()));
    assert!(m.inside(&r.l.to_string_lossy()));
    // A sibling that only starts with the same name is not inside.
    assert!(!m.inside(&format!("{}-copy/x", r.l.display())));
    assert_eq!(
        m.moved_path(&r.l.join("a/b").to_string_lossy()),
        Some(r.n.join("a").join("b"))
    );
}

#[test]
fn the_first_word_scan_agrees_with_the_one_hooks_use() {
    for c in [
        "/usr/bin/x",
        "'/a b/c.sh' arg",
        "\"/a b/c\" --flag \"y z\"",
        "C:/Users/me/x.sh",
        "/a\\ b/c rest",
        "'unterminated",
        "  /padded  ",
    ] {
        assert_eq!(first_word(c).0, crate::setup::hook_command_path(c), "{c}");
    }
    assert_eq!(first_word("'/a b/c' x y").1, " x y");
}

// ── the Setup prompt ─────────────────────────────────────────────────────────

fn done(r: &Rig, text: &str) {
    let p = marker_path_in(&r.home);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

#[test]
fn setup_is_needed_after_a_migration_until_setup_records_the_new_folder() {
    let r = Rig::new();
    let seen = r.seen();
    // A clean config: nothing in Claude's files points into the old folder.
    r.write(
        &r.claude_json(),
        &json!({"mcpServers": {"lumen": {"command": r.mcp.to_string_lossy(),
                                          "env": {"LUMEN_DB": r.new_db().to_string_lossy()}}}}),
    );
    // Setup never ran: needed.
    assert!(setup_needed_with(&r.home, &r.n, &seen));
    // An old install's empty `.setup_done`: needed once, after the upgrade.
    done(&r, "");
    assert!(setup_needed_with(&r.home, &r.n, &seen), "red before: false");
    // Setup ran while the old folder was still in use, so it recorded that one.
    done(
        &r,
        &setup_done_text(&r.l.join("lumen.db").to_string_lossy()),
    );
    assert!(setup_needed_with(&r.home, &r.n, &seen));
    // Setup recorded the new folder: done.
    done(&r, &setup_done_text(&r.new_db().to_string_lossy()));
    assert!(!setup_needed_with(&r.home, &r.n, &seen));
    if cfg!(any(windows, target_os = "macos")) {
        let loud = format!("data_dir={}\n", r.n.to_string_lossy().to_uppercase());
        done(&r, &loud);
        assert!(
            !setup_needed_with(&r.home, &r.n, &seen),
            "folded like the file system"
        );
    }
    // A value of Lumen's still inside the old folder or its backup brings it back, in either
    // of Claude's files.
    done(&r, &setup_done_text(&r.new_db().to_string_lossy()));
    r.write(
        &r.claude_json(),
        &json!({"mcpServers": {"lumen": r.lumen_entry()}}),
    );
    assert!(setup_needed_with(&r.home, &r.n, &seen));
    r.run();
    assert!(!setup_needed_with(&r.home, &r.n, &seen));
    r.write(
        &r.settings(),
        &json!({"env": {"LUMEN_DB": r.backup_data().join("lumen.db").to_string_lossy()}}),
    );
    assert!(setup_needed_with(&r.home, &r.n, &seen));
}

#[test]
fn a_settled_fresh_install_needs_setup_only_without_setup_done() {
    let r = Rig::new();
    let n = r.home.join("fresh-n");
    assert!(setup_needed_with(&r.home, &n, &Seen::Settled));
    done(&r, "");
    assert!(
        !setup_needed_with(&r.home, &n, &Seen::Settled),
        "an empty file is enough"
    );
    done(&r, "data_dir=/somewhere/else\n");
    assert!(!setup_needed_with(&r.home, &n, &Seen::Settled));
    assert!(!setup_needed_with(&r.home, &n, &Seen::Absent));
    // A marker that does not parse cannot name a stale folder.
    assert!(!setup_needed_with(&r.home, &n, &Seen::Bad));
}

#[test]
fn setup_done_names_the_folder_of_the_ledger_it_baked() {
    assert_eq!(setup_done_text("/data/x/lumen.db"), "data_dir=/data/x\n");
    assert_eq!(setup_done_text("lumen.db"), "data_dir=\n");
}

#[cfg(unix)]
#[test]
fn the_cli_link_into_the_old_folder_follows_this_build() {
    let r = Rig::new();
    let bin = r.home.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let cli = r.home.join("lumen-cli");
    std::fs::write(&cli, "").unwrap();
    let link = bin.join("lumen");
    std::os::unix::fs::symlink(r.backup_data().join("bin/lumen-mcp"), &link).unwrap();

    assert_eq!(
        repoint_cli_link(&link, &r.marker, &r.n, Some(&cli)),
        Ok(true)
    );
    assert_eq!(std::fs::read_link(&link).unwrap(), cli);
    // Already this build's: left alone. A Homebrew link: left alone.
    assert_eq!(
        repoint_cli_link(&link, &r.marker, &r.n, Some(&cli)),
        Ok(false)
    );
    let brew = bin.join("brewed");
    std::os::unix::fs::symlink("/opt/homebrew/bin/lumen", &brew).unwrap();
    assert_eq!(
        repoint_cli_link(&brew, &r.marker, &r.n, Some(&cli)),
        Ok(false)
    );
    // Not a link at all, or nothing there.
    assert_eq!(
        repoint_cli_link(&cli, &r.marker, &r.n, Some(&cli)),
        Ok(false)
    );
    assert_eq!(
        repoint_cli_link(&bin.join("none"), &r.marker, &r.n, Some(&cli)),
        Ok(false)
    );
    // No lumen-cli of this build: reported, and the link is as it was.
    let old = r.l.join("bin/lumen");
    let stale = bin.join("stale");
    std::os::unix::fs::symlink(&old, &stale).unwrap();
    assert!(repoint_cli_link(&stale, &r.marker, &r.n, None).is_err());
    assert_eq!(std::fs::read_link(&stale).unwrap(), old);
}

#[test]
fn an_mcp_command_is_one_path_with_spaces_and_native_separators() {
    assert!(is_mcp_command(
        "/Users/me/Library/Application Support/X/bin/lumen-mcp"
    ));
    assert!(is_mcp_command(
        r"C:\Users\me\AppData\Local\Lumen\lumen-mcp.exe"
    ));
    assert!(is_mcp_command(r#""C:\Program Files\Lumen\lumen-mcp.exe""#));
    assert!(!is_mcp_command("/Application Support/X/bin/other-tool"));
    assert!(!is_mcp_command("lumen-mcp --flag"));
}

#[test]
fn an_entry_with_a_space_in_its_path_under_another_key_is_found() {
    let r = Rig::with_root("Application Support");
    let e = r.lumen_entry();
    r.write(
        &r.claude_json(),
        &json!({"mcpServers": {"my-meter": e.clone()},
                "projects": {"/w": {"mcpServers": {"meter": e}}}}),
    );
    let rep = r.run();
    assert_eq!(rep.problems, Vec::<String>::new());
    let cfg = r.read(&r.claude_json());
    let db = r.new_db().to_string_lossy().to_string();
    let mcp = r.mcp.to_string_lossy().to_string();
    for entry in [
        &cfg["mcpServers"]["my-meter"],
        &cfg["projects"]["/w"]["mcpServers"]["meter"],
    ] {
        assert_eq!(entry["command"], mcp.as_str());
        assert_eq!(entry["env"]["LUMEN_DB"], db.as_str());
    }
}
