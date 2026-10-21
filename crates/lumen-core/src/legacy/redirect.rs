//! Keep a stale path working: [`redirect`] maps a path in the previous folder to the same
//! file in N, and a path in N to the previous folder while a migration is pending.
//!
//! It replaces v1's `effective_with`. The difference is that nothing here knows the
//! previous identifier: the marker records the folder the data came from, and the pending
//! file records where it still is.
//!
//! Only a directory that sits directly in a data root counts. A user's own
//! `LUMEN_DB=/elsewhere/x/lumen.db` is never redirected.
//!
//! Cost: a settled or migrated install pays one marker read per process, then nothing; a
//! parsed marker is cached because it never changes once written. An install with no marker
//! pays two failed opens per call (the marker, then the pending file), and only until the
//! first start has written one.

use super::marker::{self, Seen};
use super::{name_eq, same_dir};
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock, PoisonError};

/// Parsed markers, by the folder that holds them. Only a marker that was there and parsed is
/// kept: an absent one can still appear, and an unparseable one can still be repaired.
#[derive(Default)]
pub struct MarkerCache {
    seen: Mutex<HashMap<PathBuf, Seen>>,
    reads: AtomicUsize,
}

impl MarkerCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// The marker in `n_dir`, from the cache once it has been read and parsed.
    pub fn seen(&self, n_dir: &Path) -> Seen {
        let mut map = self.seen.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(s) = map.get(n_dir) {
            return s.clone();
        }
        self.reads.fetch_add(1, Ordering::Relaxed);
        let s = marker::read(n_dir);
        if matches!(s, Seen::Settled | Seen::Migrated(_) | Seen::V1 { .. }) {
            map.insert(n_dir.to_path_buf(), s.clone());
        }
        s
    }

    /// How many times a marker file was looked up on disk. A test reads it to prove the cache.
    pub fn reads(&self) -> usize {
        self.reads.load(Ordering::Relaxed)
    }
}

/// [`redirect`] with the identifier, the data roots and the marker cache passed in, so tests
/// can use fake identifiers and count lookups.
///
/// The closest ancestor `D` of `path` that sits directly in a root decides:
///
/// - `D` is N (named `new_id`):
///   - a marker is there (of any kind, even one that does not parse): `path`;
///   - else the pending file names a directory that is real: the same file under it;
///   - else `path`.
/// - `D` is anything else: for each root whose `<new_id>` folder carries a marker that
///   records `D` as its source (`source_dir`, or for a v1 marker the name `source`), the same
///   file under that folder. Otherwise `path`.
///
/// A tail with `..` in it is left alone: it could walk out of the directory.
pub fn redirect_in(path: &Path, new_id: &str, roots: &[&Path], cache: &MarkerCache) -> PathBuf {
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
        let Ok(tail) = path.strip_prefix(dir) else {
            return path.to_path_buf();
        };
        if tail.components().any(|c| matches!(c, Component::ParentDir)) {
            return path.to_path_buf();
        }
        if name_eq(name, new_id) {
            if cache.seen(dir).is_present() {
                return path.to_path_buf();
            }
            if let Some(p) = marker::read_pending(dir)
                && is_real_dir(&p.dir)
            {
                return p.dir.join(tail);
            }
            return path.to_path_buf();
        }
        for root in roots {
            let n_dir = root.join(new_id);
            let hit = match cache.seen(&n_dir) {
                Seen::Migrated(m) if !m.source_dir.is_empty() => {
                    same_dir(Path::new(&m.source_dir), dir)
                }
                Seen::Migrated(m) => name_eq(name, &m.source),
                Seen::V1 { source } => name_eq(name, &source),
                _ => false,
            };
            if hit {
                return n_dir.join(tail);
            }
        }
        return path.to_path_buf();
    }
    path.to_path_buf()
}

fn is_real_dir(p: &Path) -> bool {
    std::fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_dir())
}

/// The fault spool's path under the same rule, over an injected environment: the value of
/// `LUMEN_FAULT_SPOOL` redirected like any path, else `faults.jsonl` beside the database.
///
/// An empty `LUMEN_FAULT_SPOOL` counts as unset, as an empty `LUMEN_DB` does and as the shims'
/// `${VAR:-default}` did: an exported but blank variable must not name a spool called "".
///
/// `resolved_db` is the database as `meter::resolve_db_path` returns it, already redirected,
/// so a spool beside it follows. Both `faults::spool_path` and the hook's `Env::spool_path`
/// apply this one rule. Old shims export `LUMEN_FAULT_SPOOL` into the previous folder, and
/// without the redirect the hook would append to the folder the migration moved away.
pub fn spool_in(
    env_spool: Option<&Path>,
    resolved_db: Option<&Path>,
    new_id: &str,
    roots: &[&Path],
    cache: &MarkerCache,
) -> Option<PathBuf> {
    if let Some(p) = env_spool.filter(|p| !p.as_os_str().is_empty()) {
        return Some(redirect_in(p, new_id, roots, cache));
    }
    Some(resolved_db?.parent()?.join("faults.jsonl"))
}

fn process_cache() -> &'static MarkerCache {
    static CACHE: OnceLock<MarkerCache> = OnceLock::new();
    CACHE.get_or_init(MarkerCache::new)
}

/// [`redirect_in`] for this build's identifier, the platform's data roots under `home` (the
/// home the caller resolved its path against) and the process-wide marker cache.
pub fn redirect_for_home(path: &Path, home: Option<&Path>) -> PathBuf {
    let roots = super::data_roots_cached(home);
    let refs: Vec<&Path> = roots.iter().map(PathBuf::as_path).collect();
    redirect_in(path, crate::meter::APP_ID, &refs, process_cache())
}

/// [`redirect_for_home`] for the real home.
pub fn redirect(path: &Path) -> PathBuf {
    redirect_for_home(path, dirs::home_dir().as_deref())
}

/// [`spool_in`] for this build's identifier, the data roots under `home` and the process-wide
/// marker cache. `faults::spool_path` and the hook's `Env::spool_path` both call this.
pub fn spool_for_home(
    env_spool: Option<&Path>,
    resolved_db: Option<&Path>,
    home: Option<&Path>,
) -> Option<PathBuf> {
    let roots = super::data_roots_cached(home);
    let refs: Vec<&Path> = roots.iter().map(PathBuf::as_path).collect();
    spool_in(
        env_spool,
        resolved_db,
        crate::meter::APP_ID,
        &refs,
        process_cache(),
    )
}

#[cfg(test)]
mod tests;
