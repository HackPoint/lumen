//! Satellites: everything else on the machine that is named after the previous folder.
//!
//! Besides the data folder (L), an install leaves per-user items named after its identifier:
//! logs, caches, the WebKit store, preferences, saved state and cookies on macOS, the WebView2
//! store on Windows, the XDG cache and config folders on Linux. After a migration none of them
//! may keep the previous name, so each is renamed into the backup under a neutral label
//! (`logs`, `caches`, ...). Nothing is copied, nothing is deleted.
//!
//! - **Exact names only.** The previous name is `X` (learned at run time, never written here). An
//!   item counts when it is `X`, or `X` plus one of `.plist`, `.savedState`, `.binarycookies`. A
//!   neighbour whose name merely starts with `X` is never looked at: the paths are built from `X`,
//!   not found by a prefix scan.
//! - **In use** (a Windows sharing violation, `EBUSY`, anything the OS refuses): the item stays where
//!   it is, the report says so, and the next start tries again.
//! - **On another volume** (`EXDEV`, Windows folder redirection, a custom XDG mount): the item is
//!   renamed instead to `<its parent>/<new id>/<backup name>/<label>`. That is the same parent, so
//!   the same volume; the previous name is gone and nothing is deleted.
//! - **Windows `%LOCALAPPDATA%\X`** goes to `%LOCALAPPDATA%\<new id>\<backup name>\local` from the
//!   start (owner decision of 2026-10-19), so a roaming profile does not carry it.
//! - **A target that exists** gets `-2`, `-3`, ... so a re-created item never overwrites an earlier
//!   one.
//!
//! The roots are injectable ([`Roots`]) and so is the platform ([`Platform`]), so every table is
//! tested on every host.

use super::retire::{Ops, unused_name};
use std::io;
use std::path::{Path, PathBuf};

/// Which table applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    MacOs,
    Windows,
    Linux,
}

impl Platform {
    pub const fn current() -> Platform {
        if cfg!(target_os = "macos") {
            Platform::MacOs
        } else if cfg!(windows) {
            Platform::Windows
        } else {
            Platform::Linux
        }
    }
}

/// The folders the satellites sit in. A field that does not apply to the platform, or that the
/// system could not name, is `None`, and its items are skipped.
#[derive(Debug, Clone, Default)]
pub struct Roots {
    /// macOS: `~/Library`.
    pub library: Option<PathBuf>,
    /// macOS: `confstr(_CS_DARWIN_USER_CACHE_DIR)`.
    pub darwin_cache: Option<PathBuf>,
    /// macOS: `confstr(_CS_DARWIN_USER_TEMP_DIR)`.
    pub darwin_temp: Option<PathBuf>,
    /// Windows: `%LOCALAPPDATA%`.
    pub local_data: Option<PathBuf>,
    /// Linux: `$XDG_CACHE_HOME`, else `~/.cache`.
    pub xdg_cache: Option<PathBuf>,
    /// Linux: `$XDG_CONFIG_HOME`, else `~/.config`.
    pub xdg_config: Option<PathBuf>,
}

impl Roots {
    /// The real machine's roots, for [`Platform::current`].
    ///
    /// The two macOS per-user system folders are taken from the temp dir (`.../T`) and its sibling
    /// (`.../C`), which is what `confstr` returns for them; if `TMPDIR` has been pointed somewhere
    /// else they are left out rather than guessed.
    pub fn current() -> Roots {
        let mut r = Roots::default();
        match Platform::current() {
            Platform::MacOs => {
                r.library = dirs::home_dir().map(|h| h.join("Library"));
                let tmp = std::env::temp_dir();
                if tmp.file_name().is_some_and(|n| n == "T")
                    && let Some(base) = tmp.parent()
                {
                    r.darwin_cache = Some(base.join("C"));
                    r.darwin_temp = Some(tmp.clone());
                }
            }
            Platform::Windows => r.local_data = dirs::data_local_dir(),
            Platform::Linux => {
                r.xdg_cache = dirs::cache_dir();
                r.xdg_config = dirs::config_dir();
            }
        }
        r
    }
}

/// One item to move: where it is, and where it goes inside the backup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub from: PathBuf,
    /// Neutral label, relative to the backup folder.
    pub label: &'static str,
    /// Goes to `<parent>/<new id>/<backup name>/<label>` straight away instead of into the backup.
    pub beside: bool,
}

/// Where an item went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Moved {
    pub from: PathBuf,
    pub to: PathBuf,
    /// It could not cross volumes into the backup, so it went beside itself.
    pub fallback: bool,
}

/// An item that stayed where it was.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Left {
    pub from: PathBuf,
    pub why: String,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub moved: Vec<Moved>,
    /// Left in place; the next start tries again.
    pub left: Vec<Left>,
}

/// Whether `x` can be the name of a previous folder: one plain path component, and not the new
/// identifier's own folder (case-folded on every system, to be safe).
fn usable(x: &str, new_id: &str) -> bool {
    !x.is_empty()
        && x != "."
        && x != ".."
        && !x.contains(['/', '\\', '\0'])
        && !x.eq_ignore_ascii_case(new_id)
}

/// The items of the table for `platform`, for the previous name `x`. Empty when `x` is not a
/// plain name.
pub fn items(platform: Platform, roots: &Roots, x: &str, new_id: &str) -> Vec<Item> {
    if !usable(x, new_id) {
        return Vec::new();
    }
    let mut v = Vec::new();
    let mut add = |root: &Option<PathBuf>, rel: &[&str], file: String, label, beside| {
        if let Some(r) = root {
            let mut from = r.clone();
            from.extend(rel);
            from.push(file);
            v.push(Item {
                from,
                label,
                beside,
            });
        }
    };
    match platform {
        Platform::MacOs => {
            let l = &roots.library;
            add(l, &["Logs"], x.into(), "logs", false);
            add(l, &["Caches"], x.into(), "caches", false);
            add(l, &["WebKit"], x.into(), "webkit", false);
            add(l, &["HTTPStorages"], x.into(), "httpstorages", false);
            add(
                l,
                &["HTTPStorages"],
                format!("{x}.binarycookies"),
                "httpstorages.binarycookies",
                false,
            );
            add(
                l,
                &["Cookies"],
                format!("{x}.binarycookies"),
                "cookies.binarycookies",
                false,
            );
            add(
                l,
                &["Preferences"],
                format!("{x}.plist"),
                "preferences.plist",
                false,
            );
            add(
                l,
                &["Saved Application State"],
                format!("{x}.savedState"),
                "saved-state.savedState",
                false,
            );
            add(&roots.darwin_cache, &[], x.into(), "darwin-cache", false);
            add(&roots.darwin_temp, &[], x.into(), "darwin-temp", false);
        }
        Platform::Windows => add(&roots.local_data, &[], x.into(), "local", true),
        Platform::Linux => {
            add(&roots.xdg_cache, &[], x.into(), "cache", false);
            add(&roots.xdg_config, &[], x.into(), "config", false);
        }
    }
    v
}

fn exists(p: &Path) -> io::Result<bool> {
    match std::fs::symlink_metadata(p) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

/// `<parent of from>/<new id>/<backup name>`: beside `from`, on its volume.
fn beside_dir(from: &Path, backup: &Path, new_id: &str) -> Option<PathBuf> {
    Some(from.parent()?.join(new_id).join(backup.file_name()?))
}

fn rename_into(from: &Path, container: &Path, rel: &Path, ops: &dyn Ops) -> io::Result<PathBuf> {
    let name = rel
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "an empty label"))?;
    let dir = match rel.parent() {
        Some(p) if !p.as_os_str().is_empty() => container.join(p),
        _ => container.to_path_buf(),
    };
    std::fs::create_dir_all(&dir)?;
    let to = unused_name(&dir, &name.to_string_lossy());
    ops.rename(from, &to)?;
    Ok(to)
}

/// Rename `from` to `rel` inside the backup folder, or, when the OS says the two are on
/// different volumes (or `beside_first` is set), to `rel` inside `<parent of from>/<new id>/<backup
/// name>`. `Ok(None)` when `from` does not exist. `Err` leaves `from` where it is.
///
/// This is the one move every satellite and every login entry goes through.
pub fn move_into_backup(
    from: &Path,
    rel: &Path,
    backup: &Path,
    new_id: &str,
    beside_first: bool,
    ops: &dyn Ops,
) -> Result<Option<Moved>, String> {
    match exists(from) {
        Ok(true) => {}
        Ok(false) => return Ok(None),
        Err(e) => return Err(format!("{}: {e}", from.display())),
    }
    let beside = beside_dir(from, backup, new_id);
    let first: &Path = if beside_first {
        beside
            .as_deref()
            .ok_or_else(|| format!("{}: no parent folder", from.display()))?
    } else {
        backup
    };
    match rename_into(from, first, rel, ops) {
        Ok(to) => Ok(Some(Moved {
            from: from.to_path_buf(),
            to,
            fallback: false,
        })),
        Err(e) if !beside_first && e.kind() == io::ErrorKind::CrossesDevices => {
            let Some(b) = beside else {
                return Err(format!("{}: {e}", from.display()));
            };
            match rename_into(from, &b, rel, ops) {
                Ok(to) => Ok(Some(Moved {
                    from: from.to_path_buf(),
                    to,
                    fallback: true,
                })),
                Err(e2) => Err(format!("{}: {e2}", from.display())),
            }
        }
        Err(e) => Err(format!("{}: {e}", from.display())),
    }
}

/// Move every satellite named after `x` into the backup `backup` (a folder the marker names).
/// Safe to run at every start: what has moved is gone, what is in use stays and is reported.
pub fn move_satellites(
    platform: Platform,
    roots: &Roots,
    x: &str,
    new_id: &str,
    backup: &Path,
    ops: &dyn Ops,
) -> Report {
    let mut r = Report::default();
    for it in items(platform, roots, x, new_id) {
        match move_into_backup(
            &it.from,
            Path::new(it.label),
            backup,
            new_id,
            it.beside,
            ops,
        ) {
            Ok(Some(m)) => r.moved.push(m),
            Ok(None) => {}
            Err(why) => r.left.push(Left { from: it.from, why }),
        }
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::legacy::retire::Native;
    use std::cell::RefCell;
    use tempfile::TempDir;

    const X: &str = "example.previous.app";
    const NEW: &str = "io.hackpoint.lumen";

    fn roots(t: &Path) -> Roots {
        Roots {
            library: Some(t.join("Library")),
            darwin_cache: Some(t.join("var/C")),
            darwin_temp: Some(t.join("var/T")),
            local_data: Some(t.join("Local")),
            xdg_cache: Some(t.join("xdg-cache")),
            xdg_config: Some(t.join("xdg-config")),
        }
    }

    /// Every item of the table for `p`, written out by hand: where it sits under the temp root,
    /// whether it is a folder, and where it ends up under the backup (or beside itself).
    fn table(p: Platform) -> Vec<(String, bool, &'static str)> {
        let x = X;
        let v = |s: String, dir: bool, l: &'static str| (s, dir, l);
        match p {
            Platform::MacOs => vec![
                v(format!("Library/Logs/{x}"), true, "logs"),
                v(format!("Library/Caches/{x}"), true, "caches"),
                v(format!("Library/WebKit/{x}"), true, "webkit"),
                v(format!("Library/HTTPStorages/{x}"), true, "httpstorages"),
                v(
                    format!("Library/HTTPStorages/{x}.binarycookies"),
                    false,
                    "httpstorages.binarycookies",
                ),
                v(
                    format!("Library/Cookies/{x}.binarycookies"),
                    false,
                    "cookies.binarycookies",
                ),
                v(
                    format!("Library/Preferences/{x}.plist"),
                    false,
                    "preferences.plist",
                ),
                v(
                    format!("Library/Saved Application State/{x}.savedState"),
                    true,
                    "saved-state.savedState",
                ),
                v(format!("var/C/{x}"), true, "darwin-cache"),
                v(format!("var/T/{x}"), true, "darwin-temp"),
            ],
            Platform::Windows => vec![v(format!("Local/{x}"), true, "local")],
            Platform::Linux => vec![
                v(format!("xdg-cache/{x}"), true, "cache"),
                v(format!("xdg-config/{x}"), true, "config"),
            ],
        }
    }

    fn make(t: &Path, rel: &str, dir: bool) {
        let p = t.join(rel);
        if dir {
            std::fs::create_dir_all(p.join("inner")).unwrap();
            std::fs::write(p.join("inner/file.txt"), rel).unwrap();
        } else {
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, rel).unwrap();
        }
    }

    fn all_names(root: &Path) -> Vec<String> {
        let mut out = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).unwrap().flatten() {
                out.push(e.file_name().to_string_lossy().into_owned());
                if e.file_type().unwrap().is_dir() {
                    stack.push(e.path());
                }
            }
        }
        out
    }

    #[test]
    fn every_item_named_after_the_previous_folder_moves_into_the_backup() {
        for p in [Platform::MacOs, Platform::Windows, Platform::Linux] {
            let t = TempDir::new().unwrap();
            let backup = t.path().join("N").join("legacy-backup-2026-10-19");
            for (rel, dir, _) in table(p) {
                make(t.path(), &rel, dir);
            }
            let rep = move_satellites(p, &roots(t.path()), X, NEW, &backup, &Native);
            assert!(rep.left.is_empty(), "{p:?}: {:?}", rep.left);
            assert_eq!(rep.moved.len(), table(p).len(), "{p:?}");
            for (rel, dir, label) in table(p) {
                assert!(!t.path().join(&rel).exists(), "{p:?}: {rel} is still there");
                // The Windows item goes beside itself in Local, owner decision 2; the rest into B.
                let to = if p == Platform::Windows {
                    t.path()
                        .join("Local")
                        .join(NEW)
                        .join("legacy-backup-2026-10-19")
                        .join(label)
                } else {
                    backup.join(label)
                };
                assert!(to.exists(), "{p:?}: {rel} is not at {}", to.display());
                // The content came along.
                let file = if dir { to.join("inner/file.txt") } else { to };
                assert_eq!(std::fs::read_to_string(file).unwrap(), rel, "{p:?}");
            }
            // The previous name is nowhere in what was created.
            let names = all_names(t.path());
            let hits: Vec<_> = names.iter().filter(|n| n.contains(X)).collect();
            assert!(hits.is_empty(), "{p:?}: the old name survives as {hits:?}");
        }
    }

    #[test]
    fn a_name_that_only_starts_with_it_is_left_alone() {
        for p in [Platform::MacOs, Platform::Windows, Platform::Linux] {
            let t = TempDir::new().unwrap();
            let backup = t.path().join("N/legacy-backup-2026-10-19");
            let mut decoys = Vec::new();
            for (rel, dir, _) in table(p) {
                make(t.path(), &rel, dir);
                for suffix in ["-old", "2", ".bak", ".plist.bak", ".savedState.old"] {
                    // A neighbour, and the same item with its extension kept at the end.
                    let d = format!("{rel}{suffix}");
                    make(t.path(), &d, dir);
                    decoys.push(d);
                }
            }
            let rep = move_satellites(p, &roots(t.path()), X, NEW, &backup, &Native);
            assert!(rep.left.is_empty(), "{p:?}: {:?}", rep.left);
            for d in decoys {
                assert!(t.path().join(&d).exists(), "{p:?}: {d} was taken");
            }
            for (rel, _, _) in table(p) {
                assert!(!t.path().join(&rel).exists(), "{p:?}: {rel} was not taken");
            }
        }
    }

    #[test]
    fn a_name_that_is_not_a_plain_folder_name_moves_nothing() {
        let t = TempDir::new().unwrap();
        let backup = t.path().join("N/b");
        make(t.path(), "xdg-cache/real", true);
        for bad in ["", ".", "..", "a/b", "..\\x", NEW, "IO.HackPoint.Lumen"] {
            let rep = move_satellites(
                Platform::Linux,
                &roots(t.path()),
                bad,
                NEW,
                &backup,
                &Native,
            );
            assert_eq!(rep, Report::default(), "{bad:?}");
        }
        assert!(t.path().join("xdg-cache/real").exists());
        assert!(!backup.exists());
    }

    /// Refuses (or crosses a volume) for chosen destinations, otherwise renames for real.
    struct Fake {
        /// Fails with this kind when the target is under this folder.
        fail_under: RefCell<Option<(PathBuf, io::ErrorKind)>>,
    }

    impl Ops for Fake {
        fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
            if let Some((under, kind)) = &*self.fail_under.borrow()
                && to.starts_with(under)
            {
                return Err(io::Error::new(*kind, "refused"));
            }
            std::fs::rename(from, to)
        }
    }

    #[test]
    fn an_item_in_use_is_retried_next_start() {
        let t = TempDir::new().unwrap();
        let backup = t.path().join("N/legacy-backup-2026-10-19");
        make(t.path(), &format!("xdg-cache/{X}"), true);
        make(t.path(), &format!("xdg-config/{X}"), true);
        let busy = Fake {
            fail_under: RefCell::new(Some((backup.join("cache"), io::ErrorKind::ResourceBusy))),
        };
        let r = roots(t.path());
        let rep = move_satellites(Platform::Linux, &r, X, NEW, &backup, &busy);
        assert_eq!(rep.moved.len(), 1, "the free one moves: {rep:?}");
        assert_eq!(rep.left.len(), 1, "{rep:?}");
        assert!(rep.left[0].why.contains("refused"), "{:?}", rep.left[0]);
        let cache = t.path().join(format!("xdg-cache/{X}/inner/file.txt"));
        assert!(cache.exists(), "the busy item stays whole where it was");
        assert!(!backup.join("cache").exists());

        // The next start, nothing holds it any more.
        let rep = move_satellites(Platform::Linux, &r, X, NEW, &backup, &Native);
        assert!(rep.left.is_empty(), "{rep:?}");
        assert_eq!(rep.moved.len(), 1, "{rep:?}");
        assert!(backup.join("cache/inner/file.txt").exists());
        assert!(!t.path().join(format!("xdg-cache/{X}")).exists());
    }

    #[test]
    fn an_item_on_another_volume_moves_beside_itself_under_the_new_folder() {
        let t = TempDir::new().unwrap();
        let backup = t.path().join("N/legacy-backup-2026-10-19");
        make(t.path(), &format!("xdg-cache/{X}"), true);
        make(t.path(), &format!("xdg-config/{X}"), true);
        // The backup is on "another volume" for the cache folder only.
        let fake = Fake {
            fail_under: RefCell::new(Some((backup.join("cache"), io::ErrorKind::CrossesDevices))),
        };
        let rep = move_satellites(Platform::Linux, &roots(t.path()), X, NEW, &backup, &fake);
        assert!(rep.left.is_empty(), "{rep:?}");
        let cache = rep
            .moved
            .iter()
            .find(|m| m.fallback)
            .expect("one fell back");
        let want = t
            .path()
            .join("xdg-cache")
            .join(NEW)
            .join("legacy-backup-2026-10-19")
            .join("cache");
        assert_eq!(cache.to, want);
        assert!(want.join("inner/file.txt").exists());
        assert!(!t.path().join(format!("xdg-cache/{X}")).exists());
        // The other one went into the backup as usual.
        assert!(backup.join("config/inner/file.txt").exists());
        assert_eq!(rep.moved.iter().filter(|m| m.fallback).count(), 1);
    }

    #[test]
    fn running_twice_moves_nothing_more() {
        for p in [Platform::MacOs, Platform::Windows, Platform::Linux] {
            let t = TempDir::new().unwrap();
            let backup = t.path().join("N/legacy-backup-2026-10-19");
            for (rel, dir, _) in table(p) {
                make(t.path(), &rel, dir);
            }
            let r = roots(t.path());
            let first = move_satellites(p, &r, X, NEW, &backup, &Native);
            assert_eq!(first.moved.len(), table(p).len());
            let before = all_names(t.path());
            let second = move_satellites(p, &r, X, NEW, &backup, &Native);
            assert_eq!(second, Report::default(), "{p:?}");
            let mut a = before;
            let mut b = all_names(t.path());
            a.sort();
            b.sort();
            assert_eq!(a, b, "{p:?}: the second run changed the tree");
        }
    }

    #[test]
    fn a_recreated_item_gets_a_suffix() {
        let t = TempDir::new().unwrap();
        let backup = t.path().join("N/legacy-backup-2026-10-19");
        let r = roots(t.path());
        make(t.path(), &format!("xdg-cache/{X}"), true);
        move_satellites(Platform::Linux, &r, X, NEW, &backup, &Native);
        // The old app runs again and creates its folder anew.
        std::fs::create_dir_all(t.path().join(format!("xdg-cache/{X}"))).unwrap();
        std::fs::write(t.path().join(format!("xdg-cache/{X}/again.txt")), "2").unwrap();
        let rep = move_satellites(Platform::Linux, &r, X, NEW, &backup, &Native);
        assert_eq!(rep.moved.len(), 1, "{rep:?}");
        assert_eq!(rep.moved[0].to, backup.join("cache-2"));
        assert!(backup.join("cache-2/again.txt").exists());
        assert!(
            backup.join("cache/inner/file.txt").exists(),
            "the first set is untouched"
        );
    }

    #[test]
    fn a_missing_root_skips_its_items() {
        let t = TempDir::new().unwrap();
        let r = Roots {
            xdg_config: Some(t.path().join("c")),
            ..Roots::default()
        };
        let it = items(Platform::Linux, &r, X, NEW);
        assert_eq!(it.len(), 1);
        assert_eq!(it[0].label, "config");
    }
}
