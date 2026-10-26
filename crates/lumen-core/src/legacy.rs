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
    if let Some(d) = platform_data_dir()
        && !v.contains(&d)
    {
        v.push(d);
    }
    v
}

/// The platform's data directory, as `dirs::data_dir` names it, without what that costs on
/// Windows. There `dirs` asks the shell (`SHGetKnownFolderPath`), and every hook process computes
/// the data roots: the 1.10.0 benchmark caught a hook that does no work going from 5.7 to 10.4 ms
/// on a Windows runner. Not calling it was not enough: while `dirs::data_dir` is referenced at
/// all, the binary imports shell32, and Windows loads it into every hook process at start (a hook
/// that does no work still took 6.6 ms against 1.9.0's 3.9). So on Windows `dirs` is not named
/// here: `%APPDATA%` is the same folder, a redirected Roaming AppData included, and the profile's
/// `AppData\Roaming` stands in when it is unset. Elsewhere `dirs` reads only the environment.
#[cfg(windows)]
fn platform_data_dir() -> Option<PathBuf> {
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| {
            std::env::var_os("USERPROFILE")
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .map(|h| h.join("AppData").join("Roaming"))
        })
}

#[cfg(not(windows))]
fn platform_data_dir() -> Option<PathBuf> {
    dirs::data_dir()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// On Windows the platform's data root comes from `%APPDATA%`, never from the shell, which
    /// costs every hook process milliseconds (see [`platform_data_dir`]).
    #[cfg(windows)]
    #[test]
    fn the_windows_data_root_is_read_from_appdata() {
        let appdata = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .expect("APPDATA");
        assert_eq!(platform_data_dir(), Some(appdata));
    }

    #[test]
    fn the_home_based_data_root_comes_first() {
        let home = Path::new("/home/someone");
        let roots = compute_data_roots(Some(home));
        let first = crate::meter::app_data_dir_in(home)
            .parent()
            .map(Path::to_path_buf);
        assert_eq!(roots.first(), first.as_ref());
    }
}
