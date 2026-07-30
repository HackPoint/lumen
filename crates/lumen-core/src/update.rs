//! Notice when a newer Lumen is released, and say so once — for minor and major bumps
//! only.
//!
//! **This is the only part of Lumen that makes an unprompted network request.** Everything
//! else is local by construction, and the README says so, so this is opt-out-able with
//! `LUMEN_UPDATE_CHECK=0` and disclosed there. The request is an unauthenticated GET of
//! the repository's latest release; it sends no identifier beyond what any HTTPS request
//! reveals, and nothing about the machine, the projects on it, or the ledger.
//!
//! Patch releases are deliberately silent. A notification that fires for every `x.y.Z`
//! trains people to dismiss it, and then the one that mattered gets dismissed too. Minor
//! and major bumps are the ones that change behaviour or add surfaces worth knowing about.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// How long to wait between checks. A day is frequent enough that a release is noticed
/// promptly and rare enough that it is not a heartbeat.
pub const CHECK_INTERVAL_SECS: u64 = 24 * 60 * 60;

/// A three-part version. Prerelease and build metadata are parsed off and ignored: the
/// comparison this module makes is about which release line you are on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl Version {
    /// Parse `1.5.0`, `v1.5.0`, `1.5.0-rc.1` or `1.5.0+build.7`.
    ///
    /// Returns `None` rather than defaulting to zeros: a tag that cannot be parsed must
    /// not silently compare as older than everything, which would notify on every check.
    pub fn parse(raw: &str) -> Option<Self> {
        let s = raw.trim();
        let s = s
            .strip_prefix('v')
            .or_else(|| s.strip_prefix('V'))
            .unwrap_or(s);
        // Strip prerelease / build metadata.
        let s = s.split(['-', '+']).next()?;

        let mut parts = s.split('.');
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next()?.parse().ok()?;
        let patch = parts.next()?.parse().ok()?;
        // A fourth component is not semver; refuse rather than guess.
        if parts.next().is_some() {
            return None;
        }
        Some(Self {
            major,
            minor,
            patch,
        })
    }

    /// This build's version.
    pub fn current() -> Self {
        Self::parse(env!("CARGO_PKG_VERSION")).unwrap_or(Self {
            major: 0,
            minor: 0,
            patch: 0,
        })
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// The size of the step from one version to another.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bump {
    /// Same or older.
    None,
    Patch,
    Minor,
    Major,
}

/// Classify the step from `from` to `to`.
pub fn bump(from: Version, to: Version) -> Bump {
    if to <= from {
        return Bump::None;
    }
    if to.major > from.major {
        Bump::Major
    } else if to.minor > from.minor {
        Bump::Minor
    } else {
        Bump::Patch
    }
}

/// Whether a bump is worth interrupting someone for.
///
/// Minor and above only. See the module note: notifying on patches is how a notification
/// becomes noise.
pub fn is_notifiable(b: Bump) -> bool {
    matches!(b, Bump::Minor | Bump::Major)
}

/// Persisted so a version is announced once, not on every launch.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CheckState {
    /// Unix seconds of the last completed check, successful or not.
    #[serde(default)]
    pub last_checked: u64,
    /// The version most recently announced, so it is not announced again.
    #[serde(default)]
    pub last_notified: Option<String>,
}

/// Where the check state lives: beside the database, like the fault spool.
pub fn state_path() -> Option<PathBuf> {
    let db = crate::meter::db_path()?;
    Some(db.parent()?.join("update_check.json"))
}

pub fn load_state(path: &Path) -> CheckState {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

pub fn save_state(path: &Path, state: &CheckState) {
    if let Ok(json) = serde_json::to_string_pretty(state) {
        let _ = std::fs::write(path, json);
    }
}

/// Whether checking is permitted at all. `LUMEN_UPDATE_CHECK=0` disables it.
pub fn enabled() -> bool {
    std::env::var("LUMEN_UPDATE_CHECK").as_deref() != Ok("0")
}

/// Whether enough time has passed to check again.
pub fn due(state: &CheckState, now: u64, interval: u64) -> bool {
    now.saturating_sub(state.last_checked) >= interval
}

/// An update worth telling the user about.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateAvailable {
    pub current: String,
    pub latest: String,
    /// `minor` or `major`.
    pub bump: String,
    pub url: String,
}

/// Decide what to announce, given a fetched latest version and the stored state.
///
/// Pure, so the policy — minor-and-above, once per version — is testable without a clock,
/// a network or a filesystem. The caller does the I/O.
pub fn decide(
    current: Version,
    latest: Version,
    state: &CheckState,
    repo: &str,
) -> Option<UpdateAvailable> {
    let b = bump(current, latest);
    if !is_notifiable(b) {
        return None;
    }
    // Already announced this exact version; saying it again is nagging.
    if state.last_notified.as_deref() == Some(&latest.to_string()) {
        return None;
    }
    Some(UpdateAvailable {
        current: current.to_string(),
        latest: latest.to_string(),
        bump: match b {
            Bump::Major => "major".to_string(),
            _ => "minor".to_string(),
        },
        url: format!("https://github.com/{repo}/releases/tag/v{latest}"),
    })
}

/// Read the latest released version from a GitHub releases payload.
///
/// Takes the JSON rather than fetching it so the parsing is testable, and so the caller
/// owns the request — this module should not be the only thing that knows how to make one.
pub fn latest_from_json(json: &str) -> Option<Version> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    // `/releases/latest` returns an object; `/releases` an array. Accept either, so a
    // caller that used the wrong endpoint still works.
    let tag = match &v {
        serde_json::Value::Array(items) => items
            .iter()
            .find_map(|i| i.get("tag_name").and_then(|t| t.as_str()))?,
        _ => v.get("tag_name").and_then(|t| t.as_str())?,
    };
    Version::parse(tag)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_tag_shapes_a_release_actually_uses() {
        assert_eq!(
            Version::parse("v1.5.0"),
            Some(Version {
                major: 1,
                minor: 5,
                patch: 0
            })
        );
        assert_eq!(Version::parse("1.5.0"), Version::parse("v1.5.0"));
        assert_eq!(Version::parse("V1.5.0"), Version::parse("v1.5.0"));
        // Prereleases compare as their release line; the workflow accepts `v1.5.0-rc.1`.
        assert_eq!(Version::parse("v1.5.0-rc.1"), Version::parse("1.5.0"));
        assert_eq!(Version::parse("1.5.0+build.7"), Version::parse("1.5.0"));
        assert_eq!(Version::parse("  v1.5.0  "), Version::parse("1.5.0"));
    }

    /// An unparseable tag must not read as "older than everything", which would announce
    /// an update on every single check.
    #[test]
    fn refuses_what_it_cannot_understand_instead_of_guessing() {
        for bad in [
            "", "v", "1", "1.5", "1.5.0.1", "latest", "v1.x.0", "1.5.0abc",
        ] {
            assert_eq!(Version::parse(bad), None, "{bad:?} should not parse");
        }
    }

    fn v(major: u32, minor: u32, patch: u32) -> Version {
        Version {
            major,
            minor,
            patch,
        }
    }

    #[test]
    fn classifies_the_step_between_versions() {
        assert_eq!(bump(v(1, 4, 0), v(1, 4, 1)), Bump::Patch);
        assert_eq!(bump(v(1, 4, 0), v(1, 5, 0)), Bump::Minor);
        assert_eq!(bump(v(1, 4, 0), v(2, 0, 0)), Bump::Major);
        // Equal or older is not a bump.
        assert_eq!(bump(v(1, 5, 0), v(1, 5, 0)), Bump::None);
        assert_eq!(bump(v1_5(), v(1, 4, 9)), Bump::None);
        // A minor bump that also raises the patch is still minor.
        assert_eq!(bump(v(1, 4, 7), v(1, 5, 2)), Bump::Minor);
        // A major bump with a lower minor is still major.
        assert_eq!(bump(v(1, 9, 0), v(2, 0, 0)), Bump::Major);
    }

    fn v1_5() -> Version {
        v(1, 5, 0)
    }

    #[test]
    fn only_minor_and_major_are_worth_interrupting_for() {
        assert!(!is_notifiable(Bump::None));
        assert!(!is_notifiable(Bump::Patch));
        assert!(is_notifiable(Bump::Minor));
        assert!(is_notifiable(Bump::Major));
    }

    #[test]
    fn a_patch_release_is_never_announced() {
        let state = CheckState::default();
        assert_eq!(decide(v(1, 5, 0), v(1, 5, 1), &state, "o/r"), None);
        assert_eq!(decide(v(1, 5, 0), v(1, 5, 9), &state, "o/r"), None);
    }

    #[test]
    fn a_minor_release_is_announced_with_its_tag_url() {
        let got = decide(
            v(1, 4, 0),
            v(1, 5, 0),
            &CheckState::default(),
            "HackPoint/lumen",
        )
        .expect("a minor bump is notifiable");
        assert_eq!(got.current, "1.4.0");
        assert_eq!(got.latest, "1.5.0");
        assert_eq!(got.bump, "minor");
        assert_eq!(
            got.url,
            "https://github.com/HackPoint/lumen/releases/tag/v1.5.0"
        );
    }

    #[test]
    fn a_major_release_is_labelled_as_such() {
        let got =
            decide(v(1, 9, 3), v(2, 0, 0), &CheckState::default(), "o/r").expect("notifiable");
        assert_eq!(got.bump, "major");
    }

    #[test]
    fn the_same_version_is_announced_once() {
        let state = CheckState {
            last_checked: 0,
            last_notified: Some("1.5.0".to_string()),
        };
        assert_eq!(decide(v(1, 4, 0), v(1, 5, 0), &state, "o/r"), None);
        // A newer one still gets through.
        assert!(decide(v(1, 4, 0), v(1, 6, 0), &state, "o/r").is_some());
    }

    #[test]
    fn nothing_is_announced_when_already_current_or_ahead() {
        let s = CheckState::default();
        assert_eq!(decide(v(1, 5, 0), v(1, 5, 0), &s, "o/r"), None);
        // A local build ahead of the published release — a maintainer's normal state.
        assert_eq!(decide(v(1, 6, 0), v(1, 5, 0), &s, "o/r"), None);
    }

    #[test]
    fn checks_are_throttled_to_the_interval() {
        let state = CheckState {
            last_checked: 1_000,
            last_notified: None,
        };
        assert!(!due(&state, 1_000, CHECK_INTERVAL_SECS));
        assert!(!due(
            &state,
            1_000 + CHECK_INTERVAL_SECS - 1,
            CHECK_INTERVAL_SECS
        ));
        assert!(due(
            &state,
            1_000 + CHECK_INTERVAL_SECS,
            CHECK_INTERVAL_SECS
        ));
        // A never-checked state is due at any realistic clock value. (At now == 0 it is
        // not, which is correct and irrelevant: a real clock is never at the epoch.)
        assert!(due(
            &CheckState::default(),
            1_800_000_000,
            CHECK_INTERVAL_SECS
        ));
        // A clock that went backwards must not make it wait forever.
        assert!(!due(&state, 500, CHECK_INTERVAL_SECS));
    }

    #[test]
    fn reads_the_tag_from_either_releases_shape() {
        let obj = r#"{"tag_name":"v1.5.0","name":"1.5.0"}"#;
        assert_eq!(latest_from_json(obj), Version::parse("1.5.0"));

        let arr = r#"[{"tag_name":"v1.6.0"},{"tag_name":"v1.5.0"}]"#;
        assert_eq!(latest_from_json(arr), Version::parse("1.6.0"));

        for bad in ["", "{}", "[]", "not json", r#"{"tag_name":"nightly"}"#] {
            assert_eq!(latest_from_json(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn state_round_trips_and_tolerates_a_missing_or_partial_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let p = dir.path().join("update_check.json");

        // Absent file: defaults, and therefore due.
        let s = load_state(&p);
        assert_eq!(s.last_checked, 0);
        assert!(s.last_notified.is_none());

        save_state(
            &p,
            &CheckState {
                last_checked: 42,
                last_notified: Some("1.5.0".into()),
            },
        );
        let s = load_state(&p);
        assert_eq!(s.last_checked, 42);
        assert_eq!(s.last_notified.as_deref(), Some("1.5.0"));

        // A partial file must not lose the whole state.
        std::fs::write(&p, r#"{"last_checked": 7}"#).unwrap();
        let s = load_state(&p);
        assert_eq!(s.last_checked, 7);
        assert!(s.last_notified.is_none());

        // Garbage falls back to defaults rather than panicking.
        std::fs::write(&p, "{{{").unwrap();
        assert_eq!(load_state(&p).last_checked, 0);
    }

    #[test]
    fn the_check_can_be_turned_off() {
        // SAFETY: single-threaded within this test, and no other test reads this var.
        unsafe { std::env::set_var("LUMEN_UPDATE_CHECK", "0") };
        assert!(
            !enabled(),
            "LUMEN_UPDATE_CHECK=0 must disable the only network call"
        );
        unsafe { std::env::set_var("LUMEN_UPDATE_CHECK", "1") };
        assert!(enabled());
        unsafe { std::env::remove_var("LUMEN_UPDATE_CHECK") };
        assert!(enabled(), "checking is the default");
    }
}
