//! The v2 marker and the pending file.
//!
//! `N/.legacy-migration.json` is written exactly once. It either records a committed
//! migration (`"state":"migrated"`) or settles a fresh install (`"state":"none"`). Nothing
//! rewrites it, so every later state is read from the disk, and a parsed marker can be
//! cached for the life of the process.
//!
//! `N/.legacy-pending.json` says where the data still is while a migration has not
//! committed, so a path inside N resolves there instead of starting a second ledger.
//!
//! The previous identifier is never written here: it is `source` in a marker or a pending
//! file, a value learned at runtime.

use super::MARKER_NAME;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// `N/.legacy-pending.json`.
pub const PENDING_NAME: &str = ".legacy-pending.json";

/// The marker's `format` value this module writes.
pub const FORMAT: u32 = 2;
/// `state` of a committed migration.
pub const STATE_MIGRATED: &str = "migrated";
/// `state` of a settled fresh install.
pub const STATE_NONE: &str = "none";

fn is_zero(n: &u32) -> bool {
    *n == 0
}

/// `N/.legacy-migration.json`, format 2.
///
/// A settled install is exactly `{"format":2,"state":"none"}`: every other field is left
/// out when empty. `state` stays a string so a newer writer's states still parse.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarkerV2 {
    #[serde(default, skip_serializing_if = "is_zero")]
    pub format: u32,
    #[serde(default)]
    pub state: String,
    /// Name of the folder the data came from (`X`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub source: String,
    /// Where that folder was, absolute (`<R>/<X>`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub source_dir: String,
    /// The backup folder's name, relative to the folder that holds it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub backup: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub committed_at: String,
    /// Highest rowid per table in the snapshot.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub hwm: BTreeMap<String, i64>,
    /// SHA-256 of the row at `hwm`, for the positional tables.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub hwm_sig: BTreeMap<String, String>,
    /// Other Lumen folders discovery found and did not take.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub others: Vec<String>,
}

impl MarkerV2 {
    /// The marker of a fresh install that found nothing to migrate.
    pub fn settled() -> Self {
        MarkerV2 {
            format: FORMAT,
            state: STATE_NONE.into(),
            ..Default::default()
        }
    }
}

/// What the file at [`MARKER_NAME`] says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Seen {
    /// No marker. A directory at that name counts as none.
    Absent,
    /// `{"format":2,"state":"none"}`: a fresh install, settled.
    Settled,
    /// A committed migration.
    Migrated(Arc<MarkerV2>),
    /// A pre-release v1 marker (no `format`): it still redirects through `source`.
    V1 { source: String },
    /// There, but it does not parse. It pins paths in N to N and cannot redirect a stale
    /// path, because it cannot say what the old folder was.
    Bad,
}

impl Seen {
    /// Whether a file was there at all, whatever it said.
    pub fn is_present(&self) -> bool {
        !matches!(self, Seen::Absent)
    }
}

/// Classify the bytes of a marker file.
pub fn parse(bytes: &[u8]) -> Seen {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return Seen::Bad;
    };
    let Some(obj) = value.as_object() else {
        return Seen::Bad;
    };
    match obj.get("format") {
        None => match obj.get("source").and_then(|s| s.as_str()) {
            Some(source) if !source.is_empty() => Seen::V1 {
                source: source.to_string(),
            },
            _ => Seen::Bad,
        },
        Some(f) if f.as_u64().is_some() => {
            let Ok(m) = serde_json::from_value::<MarkerV2>(value) else {
                return Seen::Bad;
            };
            if m.state == STATE_NONE {
                Seen::Settled
            } else if !m.source.is_empty() || !m.source_dir.is_empty() {
                // "migrated", or a later writer's state that still names its source.
                Seen::Migrated(Arc::new(m))
            } else {
                Seen::Bad
            }
        }
        Some(_) => Seen::Bad,
    }
}

/// Read the marker in `n_dir`. Nothing at the name, or a directory there, is no marker. A
/// file that is there but cannot be read (permissions, a sharing violation, an I/O error) is
/// [`Seen::Bad`]: it is present, so it pins N and nothing migrates again.
pub fn read(n_dir: &Path) -> Seen {
    let path = n_dir.join(MARKER_NAME);
    match fs::symlink_metadata(&path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Seen::Absent,
        Ok(m) if m.is_dir() => return Seen::Absent,
        _ => {}
    }
    match fs::read(&path) {
        Ok(bytes) => parse(&bytes),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Seen::Absent,
        Err(_) => Seen::Bad,
    }
}

/// Write the marker: temp file, fsync, rename, then a directory fsync. It is the commit
/// point, and it is written once: a marker that is already there is an error, never
/// replaced.
pub fn write_marker(n_dir: &Path, m: &MarkerV2) -> io::Result<()> {
    let target = n_dir.join(MARKER_NAME);
    if fs::symlink_metadata(&target).is_ok() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "the legacy marker is written once and never replaced",
        ));
    }
    write_atomic(
        n_dir,
        &target,
        &serde_json::to_vec_pretty(m).map_err(io::Error::other)?,
        true,
    )
}

/// Settle a fresh install: write `{"format":2,"state":"none"}`.
pub fn write_settled(n_dir: &Path) -> io::Result<()> {
    write_marker(n_dir, &MarkerV2::settled())
}

/// `N/.legacy-pending.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pending {
    /// Name of the old folder (`X`).
    pub source: String,
    /// Its absolute path (`L`).
    pub dir: PathBuf,
}

/// The pending file in `n_dir`, when there is one that parses.
pub fn read_pending(n_dir: &Path) -> Option<Pending> {
    serde_json::from_slice(&fs::read(n_dir.join(PENDING_NAME)).ok()?).ok()
}

/// Write the pending file. Harmless if the run dies right after: resolution only trusts it
/// while `dir` is a real directory and no marker exists.
pub fn write_pending(n_dir: &Path, p: &Pending) -> io::Result<()> {
    write_atomic(
        n_dir,
        &n_dir.join(PENDING_NAME),
        &serde_json::to_vec(p).map_err(io::Error::other)?,
        false,
    )
}

/// Remove the pending file. A file that is not there is fine.
pub fn remove_pending(n_dir: &Path) -> io::Result<()> {
    match fs::remove_file(n_dir.join(PENDING_NAME)) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

fn write_atomic(dir: &Path, target: &Path, bytes: &[u8], durable: bool) -> io::Result<()> {
    let tmp = target.with_extension("tmp");
    let mut f = File::create(&tmp)?;
    f.write_all(bytes)?;
    if durable {
        f.sync_all()?;
    }
    drop(f);
    fs::rename(&tmp, target)?;
    if durable {
        sync_dir(dir);
    }
    Ok(())
}

fn sync_dir(_dir: &Path) {
    // A directory cannot be opened for fsync on Windows; NTFS journals the rename.
    #[cfg(unix)]
    if let Ok(d) = File::open(_dir) {
        let _ = d.sync_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn a_settled_marker_is_exactly_format_and_state() {
        let d = TempDir::new().unwrap();
        write_settled(d.path()).unwrap();
        let text = fs::read_to_string(d.path().join(MARKER_NAME)).unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v, serde_json::json!({"format": 2, "state": "none"}));
        assert_eq!(read(d.path()), Seen::Settled);
    }

    #[test]
    fn a_migrated_marker_round_trips() {
        let d = TempDir::new().unwrap();
        let m = MarkerV2 {
            format: FORMAT,
            state: STATE_MIGRATED.into(),
            source: "example.previous.app".into(),
            source_dir: "/r/example.previous.app".into(),
            backup: "legacy-backup-2026-10-19".into(),
            committed_at: "2026-10-19T08:00:00Z".into(),
            hwm: BTreeMap::from([("read_events".into(), 4140), ("faults".into(), 12)]),
            hwm_sig: BTreeMap::from([("read_events".into(), "ab".into())]),
            others: vec!["/r/another".into()],
        };
        write_marker(d.path(), &m).unwrap();
        assert_eq!(read(d.path()), Seen::Migrated(Arc::new(m)));
    }

    #[test]
    fn the_marker_is_written_once_and_never_replaced() {
        let d = TempDir::new().unwrap();
        write_settled(d.path()).unwrap();
        let before = fs::read(d.path().join(MARKER_NAME)).unwrap();
        let again = MarkerV2 {
            format: FORMAT,
            state: STATE_MIGRATED.into(),
            source: "x".into(),
            ..Default::default()
        };
        let err = write_marker(d.path(), &again).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(d.path().join(MARKER_NAME)).unwrap(), before);
        assert!(!d.path().join(".legacy-migration.tmp").exists());
    }

    #[test]
    fn every_kind_of_marker_file_is_told_apart() {
        assert_eq!(parse(br#"{"format":2,"state":"none"}"#), Seen::Settled);
        assert!(matches!(
            parse(br#"{"format":2,"state":"migrated","source":"s","source_dir":"/r/s"}"#),
            Seen::Migrated(_)
        ));
        // A later writer's state that still names its source redirects.
        assert!(matches!(
            parse(br#"{"format":3,"state":"later","source":"s"}"#),
            Seen::Migrated(_)
        ));
        assert_eq!(
            parse(br#"{"source":"s","backup":"b","state":"copied","hwm":{"read_events":7}}"#),
            Seen::V1 { source: "s".into() }
        );
        for bad in [
            &b"{ not json"[..],
            b"[]",
            b"null",
            b"{}",
            br#"{"format":"2"}"#,
            br#"{"format":2,"state":"migrated"}"#,
            br#"{"source":""}"#,
        ] {
            assert_eq!(parse(bad), Seen::Bad, "{}", String::from_utf8_lossy(bad));
        }
    }

    #[test]
    fn a_directory_at_the_marker_name_is_no_marker() {
        let d = TempDir::new().unwrap();
        fs::create_dir(d.path().join(MARKER_NAME)).unwrap();
        assert_eq!(read(d.path()), Seen::Absent);
        assert_eq!(read(&d.path().join("missing")), Seen::Absent);
    }

    /// A marker that is there but cannot be read is present, never "no marker".
    #[cfg(unix)]
    #[test]
    fn an_unreadable_marker_is_bad_not_absent() {
        use std::os::unix::fs::PermissionsExt;
        let d = TempDir::new().unwrap();
        write_settled(d.path()).unwrap();
        let m = d.path().join(MARKER_NAME);
        fs::set_permissions(&m, fs::Permissions::from_mode(0o000)).unwrap();
        if fs::read(&m).is_ok() {
            return; // running as root: the file stays readable
        }
        let seen = read(d.path());
        let again = write_settled(d.path()).unwrap_err().kind();
        fs::set_permissions(&m, fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(seen, Seen::Bad);
        assert_eq!(again, io::ErrorKind::AlreadyExists);
    }

    #[test]
    fn the_pending_file_round_trips_and_goes_away() {
        let d = TempDir::new().unwrap();
        assert_eq!(read_pending(d.path()), None);
        let p = Pending {
            source: "example.previous.app".into(),
            dir: d.path().join("example.previous.app"),
        };
        write_pending(d.path(), &p).unwrap();
        assert_eq!(read_pending(d.path()), Some(p));
        remove_pending(d.path()).unwrap();
        remove_pending(d.path()).unwrap();
        assert_eq!(read_pending(d.path()), None);
        fs::write(d.path().join(PENDING_NAME), b"{ torn").unwrap();
        assert_eq!(read_pending(d.path()), None);
    }
}
