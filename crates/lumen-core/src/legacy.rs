//! The old application identifier and everything that resolves around it.
//!
//! This is the only module where the old identifier may appear. The migration
//! itself (probe, copy, verify, commit, retire) is the app's job and lands in
//! later changes; this file holds what every reader needs: the constant, the
//! source list, the marker's shape, and [`effective`], which every
//! `resolve_db_path` caller goes through so that a stale path keeps working.

pub mod commit;
pub mod discover;
pub mod freeze;
pub mod marker;
pub mod migrate;
pub mod redirect;
pub mod retire;

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

pub mod merge;

/// A placeholder for the v1 migration code until F53 v2 T7 removes it. The previous install is
/// found by what it holds (`discover`), never by its folder's name, and no file in the
/// repository names it.
pub const LEGACY_APP_ID: &str = "io.previous.lumen";

/// Written into the new directory as the migration's commit point.
pub const MARKER_NAME: &str = ".legacy-migration.json";

/// Which systems a [`Source`] applies to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    All,
    MacOs,
}

/// What a [`Source`] contributes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Take {
    /// The whole directory.
    Dir,
    /// `lumen.db` only.
    LedgerOnly,
}

/// What happens to the old location afterwards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Retire {
    /// Rename to a dated backup and link the old path to it.
    RenameAndLink,
    /// Leave it alone (the identifier belongs to other software too).
    LeaveInPlace,
}

/// An older data directory to migrate from.
#[derive(Debug, Clone, Copy)]
pub struct Source {
    pub id: &'static str,
    pub os: Os,
    pub take: Take,
    pub retire: Retire,
}

/// Checked newest first.
pub const SOURCES: [Source; 2] = [
    Source {
        id: LEGACY_APP_ID,
        os: Os::All,
        take: Take::Dir,
        retire: Retire::RenameAndLink,
    },
    Source {
        id: "com.tauri.dev",
        os: Os::MacOs,
        take: Take::LedgerOnly,
        retire: Retire::LeaveInPlace,
    },
];

/// `N/.legacy-migration.json`. Read and parsed here; the writer comes with the
/// migration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Marker {
    /// Identifier of the directory the data came from.
    pub source: String,
    /// Where the old directory went.
    pub backup: String,
    /// `"copied"` today; kept a string so a newer writer's states still parse.
    pub state: String,
    /// Highest rowid per table already carried over.
    #[serde(default)]
    pub hwm: BTreeMap<String, i64>,
    /// Bytes of the call spool already carried over.
    #[serde(default)]
    pub spool_offset: u64,
}

/// Read the marker in `new_dir`. `None` when absent or unreadable; `Some(Err)`
/// when it is there but does not parse.
pub fn read_marker(new_dir: &Path) -> Option<Result<Marker, serde_json::Error>> {
    let bytes = std::fs::read(new_dir.join(MARKER_NAME)).ok()?;
    Some(serde_json::from_slice(&bytes))
}

fn name_eq(a: &std::ffi::OsStr, b: &str) -> bool {
    if cfg!(any(windows, target_os = "macos")) {
        // These file systems fold case by default.
        a.to_string_lossy().eq_ignore_ascii_case(b)
    } else {
        a == b
    }
}

/// Whether two directory paths name the same place, folding case where the
/// file system does. Component-wise, so a trailing separator does not matter.
fn same_dir(a: &Path, b: &Path) -> bool {
    let mut x = a.components();
    let mut y = b.components();
    loop {
        match (x.next(), y.next()) {
            (None, None) => return true,
            (Some(Component::Normal(p)), Some(Component::Normal(q))) => {
                if !name_eq(p, &q.to_string_lossy()) {
                    return false;
                }
            }
            (Some(p), Some(q)) => {
                if p != q {
                    return false;
                }
            }
            _ => return false,
        }
    }
}

/// Where the platform keeps per-user application data directories, for the anchoring in
/// [`redirect::redirect_in`] and the scan in [`discover::discover`]: the one under `home` and
/// the one the platform reports (which honours XDG on Linux).
pub fn data_roots(home: Option<&Path>) -> Vec<PathBuf> {
    data_roots_cached(home).to_vec()
}

/// [`data_roots`], computed once per `home` for the life of the process: the roots do not
/// change, and a hook asks on every call.
pub(crate) fn data_roots_cached(home: Option<&Path>) -> Arc<[PathBuf]> {
    type Cache = Mutex<HashMap<Option<PathBuf>, Arc<[PathBuf]>>>;
    static CACHE: OnceLock<Cache> = OnceLock::new();
    let mut map = CACHE
        .get_or_init(Cache::default)
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    map.entry(home.map(Path::to_path_buf))
        .or_insert_with(|| compute_data_roots(home).into())
        .clone()
}

fn compute_data_roots(home: Option<&Path>) -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(h) = home
        && let Some(p) = crate::meter::app_data_dir_in(h).parent()
    {
        v.push(p.to_path_buf());
    }
    if let Some(d) = dirs::data_dir()
        && !v.contains(&d)
    {
        v.push(d);
    }
    v
}

/// The path to use for `path`, given the identifiers of the new and old
/// directories. Separate from [`effective`] so tests can use fake identifiers.
///
/// - Under the old directory, and the new one carries the marker: the same
///   file under the new directory.
/// - Under the new directory, no marker, the old one a real directory
///   (not a symlink): the same file under the old directory (migration pending).
/// - Anything else, including a path outside both: unchanged.
///
/// A marker that exists but does not parse still counts as present. The marker
/// is written by rename, so a file at that name means the copy committed; going
/// back to the old directory would split the ledger, going forward to the new
/// one cannot lose a row the migration already carried. Whoever reads the
/// contents must treat a parse failure as "copied, nothing known about the
/// high-water marks".
///
/// Only a directory that sits directly in one of `roots` (the platform data
/// directory) counts. A user's own `LUMEN_DB=/elsewhere/<id>/x.db` is never
/// redirected, even when a directory named like the other identifier is next
/// to it.
pub fn effective_with(path: &Path, new_id: &str, legacy_id: &str, roots: &[&Path]) -> PathBuf {
    if new_id == legacy_id {
        return path.to_path_buf();
    }
    // Find the closest anchored ancestor named like one of the two; the tail is the file.
    for dir in path.ancestors().skip(1) {
        let Some(name) = dir.file_name() else {
            continue;
        };
        if !dir
            .parent()
            .is_some_and(|p| roots.iter().any(|r| same_dir(p, r)))
        {
            continue;
        }
        let (here_is_old, other) = if name_eq(name, legacy_id) {
            (true, new_id)
        } else if name_eq(name, new_id) {
            (false, legacy_id)
        } else {
            continue;
        };
        let Ok(tail) = path.strip_prefix(dir) else {
            return path.to_path_buf();
        };
        // `..` could walk out of the directory; leave such a path alone.
        if tail.components().any(|c| matches!(c, Component::ParentDir)) {
            return path.to_path_buf();
        }
        let sibling = dir.with_file_name(other);
        let (old_dir, new_dir) = if here_is_old {
            (dir, sibling.as_path())
        } else {
            (sibling.as_path(), dir)
        };
        let marked = new_dir.join(MARKER_NAME).is_file();
        if here_is_old {
            return if marked {
                new_dir.join(tail)
            } else {
                path.to_path_buf()
            };
        }
        if marked {
            return path.to_path_buf();
        }
        let old_is_real_dir = std::fs::symlink_metadata(old_dir)
            .map(|m| m.file_type().is_dir())
            .unwrap_or(false);
        return if old_is_real_dir {
            old_dir.join(tail)
        } else {
            path.to_path_buf()
        };
    }
    path.to_path_buf()
}

/// [`effective_with`] for this build's identifiers.
pub fn effective(path: &Path) -> PathBuf {
    let home = dirs::home_dir();
    let roots = data_roots(home.as_deref());
    let refs: Vec<&Path> = roots.iter().map(PathBuf::as_path).collect();
    effective_with(path, crate::meter::APP_ID, LEGACY_APP_ID, &refs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    const NEW: &str = "fake.new.app";
    const OLD: &str = "fake.old.app";

    fn mark(dir: &Path) {
        std::fs::create_dir_all(dir).unwrap();
        let m = Marker {
            source: OLD.into(),
            backup: "b".into(),
            state: "copied".into(),
            hwm: BTreeMap::new(),
            spool_offset: 0,
        };
        std::fs::write(dir.join(MARKER_NAME), serde_json::to_vec(&m).unwrap()).unwrap();
    }

    #[test]
    fn a_stale_path_is_redirected_only_with_the_marker() {
        let root = TempDir::new().unwrap();
        let old = root.path().join(OLD);
        let new = root.path().join(NEW);
        std::fs::create_dir_all(&old).unwrap();
        let stale = old.join("lumen.db");

        assert_eq!(
            effective_with(&stale, NEW, OLD, &[root.path()]),
            stale,
            "no marker, no move"
        );
        mark(&new);
        assert_eq!(
            effective_with(&stale, NEW, OLD, &[root.path()]),
            new.join("lumen.db")
        );
        // Deeper files (shims under bin/) follow too.
        assert_eq!(
            effective_with(&old.join("bin").join("x"), NEW, OLD, &[root.path()]),
            new.join("bin").join("x")
        );
    }

    #[test]
    fn pending_migration_keeps_using_the_old_directory() {
        let root = TempDir::new().unwrap();
        let old = root.path().join(OLD);
        std::fs::create_dir_all(&old).unwrap();
        let canonical = root.path().join(NEW).join("lumen.db");
        assert_eq!(
            effective_with(&canonical, NEW, OLD, &[root.path()]),
            old.join("lumen.db")
        );
        // Same answer when the new directory exists but is unmarked (WebKit makes it on Linux).
        std::fs::create_dir_all(root.path().join(NEW)).unwrap();
        assert_eq!(
            effective_with(&canonical, NEW, OLD, &[root.path()]),
            old.join("lumen.db")
        );
    }

    #[test]
    fn a_fresh_install_resolves_to_the_new_directory() {
        let root = TempDir::new().unwrap();
        let canonical = root.path().join(NEW).join("lumen.db");
        assert_eq!(
            effective_with(&canonical, NEW, OLD, &[root.path()]),
            canonical
        );
    }

    #[test]
    fn after_migration_the_new_path_stays_put() {
        let root = TempDir::new().unwrap();
        std::fs::create_dir_all(root.path().join(OLD)).unwrap();
        mark(&root.path().join(NEW));
        let canonical = root.path().join(NEW).join("lumen.db");
        assert_eq!(
            effective_with(&canonical, NEW, OLD, &[root.path()]),
            canonical
        );
    }

    #[test]
    fn a_custom_path_outside_the_app_dir_is_untouched() {
        let root = TempDir::new().unwrap();
        std::fs::create_dir_all(root.path().join(OLD)).unwrap();
        mark(&root.path().join(NEW));
        for p in [
            root.path().join("elsewhere").join("lumen.db"),
            root.path().join("lumen.db"),
            root.path().join(format!("{OLD}.bak")).join("lumen.db"),
            PathBuf::from("lumen.db"),
            PathBuf::from("/"),
            PathBuf::new(),
        ] {
            assert_eq!(effective_with(&p, NEW, OLD, &[root.path()]), p);
        }
    }

    #[test]
    fn only_a_directory_in_the_data_root_is_redirected() {
        // A custom ledger in a directory that happens to be named like the new
        // identifier, with a directory named like the old one beside it, is the
        // user's own file. Red without the anchor: both were redirected.
        let root = TempDir::new().unwrap();
        let data = root.path().join("data");
        std::fs::create_dir_all(data.join(OLD)).unwrap();
        mark(&data.join(NEW));
        let elsewhere = root.path().join("elsewhere");
        std::fs::create_dir_all(elsewhere.join(OLD)).unwrap();
        mark(&elsewhere.join(NEW));
        let nested = data.join("deeper");
        std::fs::create_dir_all(nested.join(OLD)).unwrap();
        mark(&nested.join(NEW));

        let anchors = [data.as_path()];
        for custom in [
            elsewhere.join(NEW).join("x.db"),
            elsewhere.join(OLD).join("x.db"),
            nested.join(OLD).join("x.db"),
            nested.join(NEW).join("x.db"),
        ] {
            assert_eq!(effective_with(&custom, NEW, OLD, &anchors), custom);
        }
        // The anchored one still moves, with or without a trailing separator.
        assert_eq!(
            effective_with(&data.join(OLD).join("x.db"), NEW, OLD, &anchors),
            data.join(NEW).join("x.db")
        );
        let slash = PathBuf::from(format!("{}/", data.display()));
        assert_eq!(
            effective_with(&data.join(OLD).join("x.db"), NEW, OLD, &[slash.as_path()]),
            data.join(NEW).join("x.db")
        );
    }

    #[test]
    fn a_parent_dir_component_cannot_walk_out() {
        let root = TempDir::new().unwrap();
        std::fs::create_dir_all(root.path().join(OLD)).unwrap();
        mark(&root.path().join(NEW));
        let p = root.path().join(OLD).join("..").join("x").join("lumen.db");
        assert_eq!(effective_with(&p, NEW, OLD, &[root.path()]), p);
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_old_dir_is_not_pending() {
        // After retirement L is a link to the backup; no marker must not send
        // anyone back through it.
        let root = TempDir::new().unwrap();
        let backup = root.path().join("backup");
        std::fs::create_dir_all(&backup).unwrap();
        std::os::unix::fs::symlink(&backup, root.path().join(OLD)).unwrap();
        let canonical = root.path().join(NEW).join("lumen.db");
        assert_eq!(
            effective_with(&canonical, NEW, OLD, &[root.path()]),
            canonical
        );
    }

    #[test]
    fn an_old_path_that_is_a_file_is_not_pending() {
        let root = TempDir::new().unwrap();
        std::fs::write(root.path().join(OLD), b"x").unwrap();
        let canonical = root.path().join(NEW).join("lumen.db");
        assert_eq!(
            effective_with(&canonical, NEW, OLD, &[root.path()]),
            canonical
        );
    }

    #[test]
    fn a_marker_that_does_not_parse_still_redirects() {
        let root = TempDir::new().unwrap();
        let old = root.path().join(OLD);
        let new = root.path().join(NEW);
        std::fs::create_dir_all(&old).unwrap();
        std::fs::create_dir_all(&new).unwrap();
        std::fs::write(new.join(MARKER_NAME), b"{ not json").unwrap();

        assert_eq!(
            effective_with(&old.join("lumen.db"), NEW, OLD, &[root.path()]),
            new.join("lumen.db")
        );
        assert_eq!(
            effective_with(&new.join("lumen.db"), NEW, OLD, &[root.path()]),
            new.join("lumen.db")
        );
        assert!(read_marker(&new).unwrap().is_err());
    }

    #[test]
    fn a_marker_must_be_a_file() {
        let root = TempDir::new().unwrap();
        std::fs::create_dir_all(root.path().join(OLD)).unwrap();
        std::fs::create_dir_all(root.path().join(NEW).join(MARKER_NAME)).unwrap();
        let canonical = root.path().join(NEW).join("lumen.db");
        assert_eq!(
            effective_with(&canonical, NEW, OLD, &[root.path()]),
            root.path().join(OLD).join("lumen.db")
        );
    }

    #[test]
    fn marker_round_trips_and_tolerates_missing_optional_fields() {
        let root = TempDir::new().unwrap();
        assert!(read_marker(root.path()).is_none());
        std::fs::write(
            root.path().join(MARKER_NAME),
            br#"{"source":"s","backup":"b","state":"copied","hwm":{"read_events":7},"spool_offset":42}"#,
        )
        .unwrap();
        let m = read_marker(root.path()).unwrap().unwrap();
        assert_eq!(m.hwm["read_events"], 7);
        assert_eq!(m.spool_offset, 42);
        std::fs::write(
            root.path().join(MARKER_NAME),
            br#"{"source":"s","backup":"b","state":"later-state"}"#,
        )
        .unwrap();
        let m = read_marker(root.path()).unwrap().unwrap();
        assert!(m.hwm.is_empty() && m.spool_offset == 0);
    }

    #[test]
    fn names_fold_case_exactly_where_the_file_system_does() {
        // Kills "compare case-sensitively on macOS": folding is asserted on
        // macOS and Windows, and its absence on Linux.
        let root = TempDir::new().unwrap();
        mark(&root.path().join(NEW));
        let shouty = root.path().join(OLD.to_ascii_uppercase()).join("lumen.db");
        let expect = if cfg!(any(windows, target_os = "macos")) {
            root.path().join(NEW).join("lumen.db")
        } else {
            shouty.clone()
        };
        assert_eq!(effective_with(&shouty, NEW, OLD, &[root.path()]), expect);
    }

    #[test]
    fn with_identical_ids_nothing_moves() {
        let root = TempDir::new().unwrap();
        let p = root.path().join(OLD).join("lumen.db");
        std::fs::create_dir_all(root.path().join(OLD)).unwrap();
        assert_eq!(effective_with(&p, OLD, OLD, &[root.path()]), p);
    }

    #[cfg(windows)]
    #[test]
    fn windows_paths_use_components_and_fold_case() {
        let root = TempDir::new().unwrap();
        let old = root.path().join(OLD);
        std::fs::create_dir_all(&old).unwrap();
        mark(&root.path().join(NEW));
        let raw = format!(
            "{}\\{}\\bin\\x.exe",
            root.path().display(),
            OLD.to_ascii_uppercase()
        );
        let got = effective_with(Path::new(&raw), NEW, OLD, &[root.path()]);
        assert_eq!(got, root.path().join(NEW).join("bin").join("x.exe"));
    }

    #[cfg(unix)]
    #[test]
    fn a_backslash_is_not_a_separator_on_unix() {
        let p = PathBuf::from(format!("C:\\Users\\u\\{OLD}\\lumen.db"));
        assert_eq!(
            effective_with(&p, NEW, OLD, &[Path::new("C:\\Users\\u")]),
            p
        );
    }

    #[test]
    fn the_real_ids_do_not_move_anything_until_they_differ() {
        // The constant pair is the identity today; this guards the flip, which
        // must be a deliberate change to APP_ID alone.
        let p = Path::new("/h").join(LEGACY_APP_ID).join("lumen.db");
        if crate::meter::APP_ID == LEGACY_APP_ID {
            assert_eq!(effective(&p), p);
        }
    }
}
