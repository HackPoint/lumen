use super::*;
use crate::legacy::MARKER_NAME;
use crate::legacy::marker::{self, MarkerV2, Pending};
use std::fs;
use tempfile::TempDir;

const NEW: &str = "fake.new.app";
const OLD: &str = "fake.old.app";

/// Redirect with a cache of its own, so no answer leaks from one call to the next.
fn go(path: &Path, roots: &[&Path]) -> PathBuf {
    redirect_in(path, NEW, roots, &MarkerCache::new())
}

fn migrated_marker(old_dir: &Path) -> MarkerV2 {
    MarkerV2 {
        format: marker::FORMAT,
        state: marker::STATE_MIGRATED.into(),
        source: old_dir.file_name().unwrap().to_string_lossy().into_owned(),
        source_dir: old_dir.to_string_lossy().into_owned(),
        backup: "legacy-backup-2026-10-19".into(),
        ..Default::default()
    }
}

/// N holds a committed marker that names `old_dir`.
fn mark(new_dir: &Path, old_dir: &Path) {
    fs::create_dir_all(new_dir).unwrap();
    marker::write_marker(new_dir, &migrated_marker(old_dir)).unwrap();
}

fn pend(new_dir: &Path, old_dir: &Path) {
    fs::create_dir_all(new_dir).unwrap();
    marker::write_pending(
        new_dir,
        &Pending {
            source: OLD.into(),
            dir: old_dir.to_path_buf(),
        },
    )
    .unwrap();
}

#[test]
fn a_stale_path_is_redirected_only_with_the_marker() {
    let root = TempDir::new().unwrap();
    let old = root.path().join(OLD);
    let new = root.path().join(NEW);
    fs::create_dir_all(&old).unwrap();
    let stale = old.join("lumen.db");

    assert_eq!(go(&stale, &[root.path()]), stale, "no marker, no move");
    mark(&new, &old);
    assert_eq!(go(&stale, &[root.path()]), new.join("lumen.db"));
    // Deeper files (shims under bin/) follow too.
    assert_eq!(
        go(&old.join("bin").join("x"), &[root.path()]),
        new.join("bin").join("x")
    );
}

#[test]
fn a_stale_path_is_redirected_by_the_folder_the_marker_records() {
    // The previous folder has a name nothing in this code knows. The marker records where the
    // data came from, and that is all the redirect needs. Red on release/1.10.0, where a
    // marker naming a neutral folder redirects nothing.
    let root = TempDir::new().unwrap();
    let prev = root.path().join("example.previous.app");
    let new = root.path().join(NEW);
    fs::create_dir_all(&prev).unwrap();
    mark(&new, &prev);

    let roots = [root.path()];
    assert_eq!(
        go(&prev.join("lumen.db"), &roots),
        new.join("lumen.db"),
        "the database"
    );
    assert_eq!(
        go(&prev.join("faults.jsonl"), &roots),
        new.join("faults.jsonl")
    );
    assert_eq!(
        go(&prev.join("bin").join("lumen-mcp"), &roots),
        new.join("bin").join("lumen-mcp")
    );
    // A folder that is not the one the marker records stays where it is, even beside it, and
    // even when its name only starts with the recorded one.
    for other in [
        root.path().join("example.other.app"),
        root.path().join("example.previous.app.bak"),
        root.path().join("example.previous"),
    ] {
        assert_eq!(go(&other.join("lumen.db"), &roots), other.join("lumen.db"));
    }
}

#[test]
fn a_marker_in_another_root_redirects_a_stale_path_in_this_one() {
    // The previous folder sits in one data root and N in the other (Linux with XDG_DATA_HOME).
    let r1 = TempDir::new().unwrap();
    let r2 = TempDir::new().unwrap();
    let prev = r1.path().join("example.previous.app");
    fs::create_dir_all(&prev).unwrap();
    mark(&r2.path().join(NEW), &prev);
    assert_eq!(
        go(&prev.join("lumen.db"), &[r1.path(), r2.path()]),
        r2.path().join(NEW).join("lumen.db")
    );
    // The same folder name in the other root is not the recorded one.
    let twin = r2.path().join("example.previous.app");
    assert_eq!(
        go(&twin.join("lumen.db"), &[r1.path(), r2.path()]),
        twin.join("lumen.db")
    );
}

#[test]
fn a_marker_without_a_recorded_folder_falls_back_to_its_source_name() {
    let root = TempDir::new().unwrap();
    let new = root.path().join(NEW);
    fs::create_dir_all(&new).unwrap();
    marker::write_marker(
        &new,
        &MarkerV2 {
            format: marker::FORMAT,
            state: marker::STATE_MIGRATED.into(),
            source: OLD.into(),
            ..Default::default()
        },
    )
    .unwrap();
    let p = root.path().join(OLD).join("lumen.db");
    assert_eq!(go(&p, &[root.path()]), new.join("lumen.db"));
}

#[test]
fn a_v1_marker_still_redirects() {
    // Written by a pre-release build: no `format`, the folder's name in `source`.
    let root = TempDir::new().unwrap();
    let new = root.path().join(NEW);
    fs::create_dir_all(&new).unwrap();
    fs::write(
        new.join(MARKER_NAME),
        format!(r#"{{"source":"{OLD}","backup":"b","state":"copied","hwm":{{"read_events":7}},"spool_offset":42}}"#),
    )
    .unwrap();
    let old = root.path().join(OLD);
    assert_eq!(
        go(&old.join("lumen.db"), &[root.path()]),
        new.join("lumen.db")
    );
    // N's own paths stay in N, and a different folder is not redirected.
    assert_eq!(
        go(&new.join("lumen.db"), &[root.path()]),
        new.join("lumen.db")
    );
    let other = root.path().join("fake.other.app").join("lumen.db");
    assert_eq!(go(&other, &[root.path()]), other);
}

#[test]
fn a_pending_migration_is_known_from_the_pending_file() {
    let root = TempDir::new().unwrap();
    let old = root.path().join(OLD);
    fs::create_dir_all(&old).unwrap();
    let new = root.path().join(NEW);
    let canonical = new.join("lumen.db");
    let roots = [root.path()];

    // An old folder with no pending file is nothing: the new one is a fresh install.
    assert_eq!(go(&canonical, &roots), canonical);
    // Same when N exists but is empty (WebKit makes it on Linux).
    fs::create_dir_all(&new).unwrap();
    assert_eq!(go(&canonical, &roots), canonical);

    pend(&new, &old);
    assert_eq!(go(&canonical, &roots), old.join("lumen.db"));
    assert_eq!(
        go(&new.join("faults.jsonl"), &roots),
        old.join("faults.jsonl")
    );
    // The old folder's own paths are untouched while pending.
    assert_eq!(go(&old.join("lumen.db"), &roots), old.join("lumen.db"));

    // A pending file whose folder has vanished is nothing.
    fs::remove_dir_all(&old).unwrap();
    assert_eq!(go(&canonical, &roots), canonical);
}

#[test]
fn a_pending_file_names_a_real_directory_or_nothing() {
    let root = TempDir::new().unwrap();
    let new = root.path().join(NEW);
    let canonical = new.join("lumen.db");
    let roots = [root.path()];

    // A file where the folder should be.
    let file = root.path().join("a-file");
    fs::write(&file, b"x").unwrap();
    pend(&new, &file);
    assert_eq!(go(&canonical, &roots), canonical);

    // A link to a folder (a retired layout) is not pending.
    #[cfg(unix)]
    {
        let real = root.path().join("real");
        fs::create_dir_all(&real).unwrap();
        let link = root.path().join(OLD);
        std::os::unix::fs::symlink(&real, &link).unwrap();
        pend(&new, &link);
        assert_eq!(go(&canonical, &roots), canonical);
    }

    // Once a marker exists the pending file is stale, whatever it says.
    let old = root.path().join(OLD);
    let _ = fs::remove_file(&old);
    fs::create_dir_all(&old).unwrap();
    pend(&new, &old);
    assert_eq!(go(&canonical, &roots), old.join("lumen.db"));
    marker::write_settled(&new).unwrap();
    assert_eq!(go(&canonical, &roots), canonical);
}

#[test]
fn a_fresh_install_resolves_to_the_new_directory() {
    let root = TempDir::new().unwrap();
    let canonical = root.path().join(NEW).join("lumen.db");
    assert_eq!(go(&canonical, &[root.path()]), canonical);
}

#[test]
fn after_migration_the_new_path_stays_put() {
    let root = TempDir::new().unwrap();
    let old = root.path().join(OLD);
    fs::create_dir_all(&old).unwrap();
    mark(&root.path().join(NEW), &old);
    let canonical = root.path().join(NEW).join("lumen.db");
    assert_eq!(go(&canonical, &[root.path()]), canonical);
}

#[test]
fn a_custom_path_outside_the_app_dir_is_untouched() {
    let root = TempDir::new().unwrap();
    let old = root.path().join(OLD);
    fs::create_dir_all(&old).unwrap();
    mark(&root.path().join(NEW), &old);
    for p in [
        root.path().join("elsewhere").join("lumen.db"),
        root.path().join("lumen.db"),
        root.path().join(format!("{OLD}.bak")).join("lumen.db"),
        PathBuf::from("lumen.db"),
        PathBuf::from("/"),
        PathBuf::new(),
    ] {
        assert_eq!(go(&p, &[root.path()]), p);
    }
}

#[test]
fn only_a_directory_in_the_data_root_is_redirected() {
    // A custom ledger in a directory that happens to be named like the new identifier, with
    // the recorded old folder beside it, is the user's own file.
    let root = TempDir::new().unwrap();
    let data = root.path().join("data");
    fs::create_dir_all(data.join(OLD)).unwrap();
    mark(&data.join(NEW), &data.join(OLD));
    let elsewhere = root.path().join("elsewhere");
    fs::create_dir_all(elsewhere.join(OLD)).unwrap();
    mark(&elsewhere.join(NEW), &elsewhere.join(OLD));
    let nested = data.join("deeper");
    fs::create_dir_all(nested.join(OLD)).unwrap();
    mark(&nested.join(NEW), &nested.join(OLD));

    let anchors = [data.as_path()];
    for custom in [
        elsewhere.join(NEW).join("x.db"),
        elsewhere.join(OLD).join("x.db"),
        nested.join(OLD).join("x.db"),
        nested.join(NEW).join("x.db"),
    ] {
        assert_eq!(go(&custom, &anchors), custom);
    }
    // The anchored one still moves, with or without a trailing separator.
    assert_eq!(
        go(&data.join(OLD).join("x.db"), &anchors),
        data.join(NEW).join("x.db")
    );
    let slash = PathBuf::from(format!("{}/", data.display()));
    assert_eq!(
        go(&data.join(OLD).join("x.db"), &[slash.as_path()]),
        data.join(NEW).join("x.db")
    );
}

#[test]
fn a_parent_dir_component_cannot_walk_out() {
    let root = TempDir::new().unwrap();
    let old = root.path().join(OLD);
    fs::create_dir_all(&old).unwrap();
    mark(&root.path().join(NEW), &old);
    let p = old.join("..").join("x").join("lumen.db");
    assert_eq!(go(&p, &[root.path()]), p);
    // And out of N while pending.
    let root = TempDir::new().unwrap();
    let old = root.path().join(OLD);
    fs::create_dir_all(&old).unwrap();
    pend(&root.path().join(NEW), &old);
    let p = root.path().join(NEW).join("..").join("x").join("lumen.db");
    assert_eq!(go(&p, &[root.path()]), p);
}

#[test]
fn a_marker_that_does_not_parse_pins_n_but_cannot_redirect() {
    // It cannot say what the old folder was, so a stale path stays; N's own paths stay in N,
    // and a pending file does not pull them back out.
    let root = TempDir::new().unwrap();
    let old = root.path().join(OLD);
    let new = root.path().join(NEW);
    fs::create_dir_all(&old).unwrap();
    fs::create_dir_all(&new).unwrap();
    fs::write(new.join(MARKER_NAME), b"{ not json").unwrap();
    pend(&new, &old);

    assert_eq!(
        go(&old.join("lumen.db"), &[root.path()]),
        old.join("lumen.db")
    );
    assert_eq!(
        go(&new.join("lumen.db"), &[root.path()]),
        new.join("lumen.db")
    );
    assert!(marker::read(&new) == marker::Seen::Bad);
}

#[test]
fn a_marker_must_be_a_file() {
    let root = TempDir::new().unwrap();
    let old = root.path().join(OLD);
    fs::create_dir_all(&old).unwrap();
    fs::create_dir_all(root.path().join(NEW).join(MARKER_NAME)).unwrap();
    pend(&root.path().join(NEW), &old);
    let canonical = root.path().join(NEW).join("lumen.db");
    assert_eq!(go(&canonical, &[root.path()]), old.join("lumen.db"));
}

#[test]
fn names_fold_case_exactly_where_the_file_system_does() {
    // Kills "compare case-sensitively on macOS": folding is asserted on macOS and Windows, and
    // its absence on Linux.
    let root = TempDir::new().unwrap();
    let old = root.path().join(OLD);
    fs::create_dir_all(&old).unwrap();
    mark(&root.path().join(NEW), &old);
    let shouty = root.path().join(OLD.to_ascii_uppercase()).join("lumen.db");
    let expect = if cfg!(any(windows, target_os = "macos")) {
        root.path().join(NEW).join("lumen.db")
    } else {
        shouty.clone()
    };
    assert_eq!(go(&shouty, &[root.path()]), expect);
}

#[cfg(windows)]
#[test]
fn windows_paths_use_components_and_fold_case() {
    let root = TempDir::new().unwrap();
    let old = root.path().join(OLD);
    fs::create_dir_all(&old).unwrap();
    mark(&root.path().join(NEW), &old);
    let raw = format!(
        "{}\\{}\\bin\\x.exe",
        root.path().display(),
        OLD.to_ascii_uppercase()
    );
    let got = go(Path::new(&raw), &[root.path()]);
    assert_eq!(got, root.path().join(NEW).join("bin").join("x.exe"));
}

#[cfg(unix)]
#[test]
fn a_backslash_is_not_a_separator_on_unix() {
    let p = PathBuf::from(format!("C:\\Users\\u\\{OLD}\\lumen.db"));
    assert_eq!(go(&p, &[Path::new("C:\\Users\\u")]), p);
}

#[test]
fn a_settled_install_resolves_with_one_stat_then_none() {
    let root = TempDir::new().unwrap();
    let new = root.path().join(NEW);
    fs::create_dir_all(&new).unwrap();
    marker::write_settled(&new).unwrap();
    let canonical = new.join("lumen.db");
    let roots = [root.path()];
    let cache = MarkerCache::new();

    assert_eq!(redirect_in(&canonical, NEW, &roots, &cache), canonical);
    assert_eq!(cache.reads(), 1, "the first call looks the marker up once");
    for _ in 0..50 {
        assert_eq!(redirect_in(&canonical, NEW, &roots, &cache), canonical);
        assert_eq!(
            redirect_in(&new.join("faults.jsonl"), NEW, &roots, &cache),
            new.join("faults.jsonl")
        );
    }
    assert_eq!(cache.reads(), 1, "then the cache answers: no lookup at all");

    // A marker never changes once written, so the cache is right even if the file goes.
    fs::remove_file(new.join(MARKER_NAME)).unwrap();
    assert_eq!(redirect_in(&canonical, NEW, &roots, &cache), canonical);
    assert_eq!(cache.reads(), 1);
}

#[test]
fn a_committed_marker_is_cached_for_stale_paths_too() {
    let root = TempDir::new().unwrap();
    let old = root.path().join("example.previous.app");
    fs::create_dir_all(&old).unwrap();
    let new = root.path().join(NEW);
    mark(&new, &old);
    let roots = [root.path()];
    let cache = MarkerCache::new();
    for _ in 0..5 {
        assert_eq!(
            redirect_in(&old.join("lumen.db"), NEW, &roots, &cache),
            new.join("lumen.db")
        );
    }
    assert_eq!(cache.reads(), 1);
}

#[test]
fn a_marker_that_is_not_there_yet_is_not_cached() {
    let root = TempDir::new().unwrap();
    let old = root.path().join(OLD);
    fs::create_dir_all(&old).unwrap();
    let new = root.path().join(NEW);
    let roots = [root.path()];
    let cache = MarkerCache::new();
    let stale = old.join("lumen.db");
    assert_eq!(redirect_in(&stale, NEW, &roots, &cache), stale);
    assert_eq!(redirect_in(&stale, NEW, &roots, &cache), stale);
    assert_eq!(cache.reads(), 2, "absent: looked up every time");
    mark(&new, &old);
    assert_eq!(
        redirect_in(&stale, NEW, &roots, &cache),
        new.join("lumen.db")
    );
}

#[test]
fn a_stale_fault_spool_is_redirected_too() {
    // Old shims export LUMEN_FAULT_SPOOL into the previous folder, and `lumen-mcp hook` and
    // `faults::spool_path` both resolve the spool themselves. Both apply `spool_in` over their
    // own environment. Red when the variable's value is used as it stands.
    let root = TempDir::new().unwrap();
    let prev = root.path().join("example.previous.app");
    let new = root.path().join(NEW);
    fs::create_dir_all(&prev).unwrap();
    mark(&new, &prev);
    let roots = [root.path()];
    let cache = MarkerCache::new();

    // The variable names a file in the previous folder: it lands in N.
    let stale = prev.join("faults.jsonl");
    assert_eq!(
        spool_in(Some(&stale), None, NEW, &roots, &cache),
        Some(new.join("faults.jsonl"))
    );
    // A spool the user placed elsewhere is theirs.
    let own = root.path().join("elsewhere").join("spool.jsonl");
    assert_eq!(
        spool_in(Some(&own), None, NEW, &roots, &cache),
        Some(own.clone())
    );
    // No variable: beside the database as already resolved, which is N's.
    let db = new.join("lumen.db");
    assert_eq!(
        spool_in(None, Some(&db), NEW, &roots, &cache),
        Some(new.join("faults.jsonl"))
    );
    // Nothing to go on.
    assert_eq!(spool_in(None, None, NEW, &roots, &cache), None);

    // Pending: the variable and the database both still mean the previous folder.
    let root2 = TempDir::new().unwrap();
    let prev2 = root2.path().join("example.previous.app");
    fs::create_dir_all(&prev2).unwrap();
    pend(&root2.path().join(NEW), &prev2);
    let canonical = root2.path().join(NEW).join("faults.jsonl");
    assert_eq!(
        spool_in(
            Some(&canonical),
            None,
            NEW,
            &[root2.path()],
            &MarkerCache::new()
        ),
        Some(prev2.join("faults.jsonl"))
    );
}

#[test]
fn the_process_wide_redirect_leaves_unrelated_paths_alone() {
    let p = Path::new("/no-such-root/anywhere/lumen.db");
    assert_eq!(redirect(p), p);
}
