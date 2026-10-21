//! Finding the previous install and keeping its paths working.
//!
//! The previous folder is found by what it holds ([`discover`]), never by its name; the marker
//! ([`marker`]) records what was found, and [`redirect::redirect`], which every
//! `resolve_db_path` caller goes through, keeps a stale path working. This file holds what the
//! submodules share: the generic-bundle source list, the data roots and the name comparison.

pub mod commit;
pub mod discover;
mod faultset;
pub mod freeze;
pub mod import;
pub mod marker;
pub mod migrate;
pub mod redirect;
pub mod retire;
pub mod satellites;

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

pub mod merge;

/// Written into the new directory as the migration's commit point.
pub const MARKER_NAME: &str = ".legacy-migration.json";

/// Which systems a [`Source`] applies to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    MacOs,
}

/// What a [`Source`] contributes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Take {
    /// `lumen.db` only.
    LedgerOnly,
}

/// What happens to the old location afterwards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Retire {
    /// Leave it alone (the identifier belongs to other software too).
    LeaveInPlace,
}

/// A folder of another application whose ledger is imported, and which stays where it is.
#[derive(Debug, Clone, Copy)]
pub struct Source {
    pub id: &'static str,
    pub os: Os,
    pub take: Take,
    pub retire: Retire,
}

/// Tauri's generic development identifier: its ledger is read, the folder is never touched.
pub const SOURCES: [Source; 1] = [Source {
    id: "com.tauri.dev",
    os: Os::MacOs,
    take: Take::LedgerOnly,
    retire: Retire::LeaveInPlace,
}];

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
