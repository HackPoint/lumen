//! Persisted settings, and the one place the repeat-reads switch is decided.
//!
//! Repeat reads are on by default (1.8.0). There are two ways to turn them off, and one
//! precedence: the environment variable `LUMEN_REPEAT_READS` beats the persisted setting, and
//! the persisted setting beats the default.
//!
//! The persisted setting is a small JSON file, `lumen_settings.json`, in the directory the
//! ledger lives in (so `LUMEN_DB` moves it with the ledger, and a test that points `LUMEN_DB` at
//! a temp dir never sees a developer's real setting):
//!
//! ```json
//! { "repeat_reads": false }
//! ```
//!
//! It is read on every `tools/call`, not once at startup: a setting change takes effect on the
//! very next read, with no restart of Claude Code. The read is one small `open + read`.
//!
//! Anything that cannot be read as an answer is the safe side, which is OFF (every read in
//! full, nothing stubbed) and is reported, not swallowed: [`Resolved::problem`] says what was
//! wrong and [`surface`] turns it into a fault row and a stderr line. An absent file is not a
//! problem; it is a fresh install.

use crate::ranked::Mode;
use std::path::{Path, PathBuf};

/// The environment variable that turns repeat reads off (or `ab`, for the experiment).
pub const REPEAT_READS_ENV: &str = "LUMEN_REPEAT_READS";
/// The settings file, beside the ledger.
pub const FILE_NAME: &str = "lumen_settings.json";
/// The key in the file.
pub const REPEAT_READS_KEY: &str = "repeat_reads";

/// Where the setting file is for a ledger at `db`.
pub fn settings_path(db: &Path) -> PathBuf {
    crate::identity::record_dir(db).join(FILE_NAME)
}

/// Where the answer came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Default,
    File,
    Env,
}

/// Why the answer is the safe side instead of what was asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problem {
    /// `LUMEN_REPEAT_READS` holds a value that means nothing. Carries the value.
    Env(String),
    /// The settings file exists but cannot be used. Carries why.
    File(String),
}

impl Problem {
    /// The fault variant this is recorded under.
    pub fn variant(&self) -> &'static str {
        match self {
            Problem::Env(_) => "env",
            Problem::File(_) => "file",
        }
    }

    /// A sentence a person can act on.
    pub fn describe(&self, file: &Path) -> String {
        match self {
            Problem::Env(v) => format!(
                "{REPEAT_READS_ENV}={v:?} is not on, off or ab: repeat reads are off, every \
                 read is in full. Set it to on or off, or unset it."
            ),
            Problem::File(why) => format!(
                "{} cannot be used ({why}): repeat reads are off, every read is in full. \
                 Fix or delete the file (deleting it gives the default, on), or set \
                 {REPEAT_READS_ENV} to on or off, which takes precedence over the file.",
                file.display()
            ),
        }
    }
}

/// The decided state of repeat reads, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub mode: Mode,
    pub source: Source,
    /// Set when `mode` is Off because something was unreadable, not because it was asked for.
    pub problem: Option<Problem>,
}

/// What reading the settings file gave.
#[derive(Debug)]
pub enum FileState {
    /// No file: a fresh install, or one that never changed the setting.
    Absent,
    Text(String),
    /// The file is there and could not be read.
    Unreadable(String),
}

/// Read the settings file at `path`.
pub fn read_file(path: &Path) -> FileState {
    match std::fs::read_to_string(path) {
        Ok(t) => FileState::Text(t),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => FileState::Absent,
        Err(e) => FileState::Unreadable(e.to_string()),
    }
}

/// The environment variable's meaning: `None` when it says nothing (unset or empty),
/// `Some(Err(value))` when it says something that is not a mode.
fn parse_env(v: Option<&str>) -> Option<Result<Mode, String>> {
    let v = v?;
    let t = v.trim();
    if t.is_empty() {
        return None;
    }
    Some(match t.to_ascii_lowercase().as_str() {
        "on" | "1" | "true" => Ok(Mode::On),
        "off" | "0" | "false" => Ok(Mode::Off),
        "ab" => Ok(Mode::Ab),
        _ => Err(t.to_string()),
    })
}

/// The setting file's meaning: `Ok(None)` when it says nothing about repeat reads.
fn parse_file(state: &FileState) -> Result<Option<bool>, String> {
    let text = match state {
        FileState::Absent => return Ok(None),
        FileState::Unreadable(why) => return Err(format!("unreadable: {why}")),
        FileState::Text(t) => t,
    };
    let v: serde_json::Value = serde_json::from_str(text).map_err(|e| format!("not JSON: {e}"))?;
    let obj = v
        .as_object()
        .ok_or_else(|| "not a JSON object".to_string())?;
    match obj.get(REPEAT_READS_KEY) {
        None => Ok(None),
        Some(serde_json::Value::Bool(b)) => Ok(Some(*b)),
        Some(other) => Err(format!(
            "{REPEAT_READS_KEY} must be true or false, not {other}"
        )),
    }
}

/// Decide from what was read. Pure, so the precedence is testable without touching the
/// process environment.
///
/// env (when it says anything) > file > default ON. A bad env value does not fall through to
/// the file: it is the safe side, off, and it is reported.
pub fn resolve(env: Option<&str>, file: &FileState) -> Resolved {
    match parse_env(env) {
        Some(Ok(mode)) => {
            return Resolved {
                mode,
                source: Source::Env,
                problem: None,
            };
        }
        Some(Err(value)) => {
            return Resolved {
                mode: Mode::Off,
                source: Source::Env,
                problem: Some(Problem::Env(value)),
            };
        }
        None => {}
    }
    match parse_file(file) {
        Ok(Some(on)) => Resolved {
            mode: if on { Mode::On } else { Mode::Off },
            source: Source::File,
            problem: None,
        },
        Ok(None) => Resolved {
            mode: Mode::On,
            source: Source::Default,
            problem: None,
        },
        Err(why) => Resolved {
            mode: Mode::Off,
            source: Source::File,
            problem: Some(Problem::File(why)),
        },
    }
}

/// The state of repeat reads for a ledger at `db`, reading the environment and the file now.
pub fn repeat_reads_for(db: &Path) -> Resolved {
    with_env(&read_file(&settings_path(db)))
}

/// [`repeat_reads_for`] against the ledger this process uses. When there is no data directory
/// to look in there is no file to find: the environment and the default decide, and every
/// repeat is answered in full anyway because there is no ledger to check the base against.
///
/// Cheap enough to call on every `tools/call`, which is why the server does: measured in a
/// release build, a median of 1.4 us with no settings file and 29 us (p99 78 us) with one,
/// against the 1-2 ms the hook and the claim already add to a call. So a change of the
/// setting applies to the very next call, with no restart of the server.
pub fn repeat_reads() -> Resolved {
    match crate::meter::db_path() {
        Some(db) => repeat_reads_for(&db),
        None => with_env(&FileState::Absent),
    }
}

fn with_env(file: &FileState) -> Resolved {
    match std::env::var(REPEAT_READS_ENV) {
        Ok(v) => resolve(Some(&v), file),
        Err(std::env::VarError::NotPresent) => resolve(None, file),
        // Not unicode: it cannot be a mode, and it is reported as a value that is not one.
        Err(std::env::VarError::NotUnicode(raw)) => Resolved {
            mode: Mode::Off,
            source: Source::Env,
            problem: Some(Problem::Env(raw.to_string_lossy().into_owned())),
        },
    }
}

/// Say what is wrong: one fault row (at most one per `min_interval` per variant, so a
/// per-call check does not write a row per read) and, the first time, one line on stderr.
pub fn surface(problem: &Problem, file: &Path, min_interval: std::time::Duration) {
    let rec = crate::faults::FaultRecord::now("repeat_reads_config", problem.variant())
        .with_detail(problem.describe(file));
    if crate::faults::record_throttled(&rec, min_interval) {
        eprintln!("lumen: {}", problem.describe(file));
    }
}

/// Write the persisted setting for a ledger at `db`: the Settings toggle's write.
///
/// Atomic (written beside, renamed into place) so a reader never sees half a file, and
/// private to this user. Other keys in a valid existing file are kept. A file that cannot be
/// used is replaced: choosing the setting is the repair.
pub fn write_repeat_reads(db: &Path, on: bool) -> std::io::Result<()> {
    let path = settings_path(db);
    let mut obj = match read_file(&path) {
        FileState::Text(t) => match serde_json::from_str::<serde_json::Value>(&t) {
            Ok(serde_json::Value::Object(o)) => o,
            _ => serde_json::Map::new(),
        },
        FileState::Absent | FileState::Unreadable(_) => serde_json::Map::new(),
    };
    obj.insert(REPEAT_READS_KEY.to_string(), serde_json::Value::Bool(on));
    let body = serde_json::to_string_pretty(&serde_json::Value::Object(obj))
        .map_err(std::io::Error::other)?;
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension(format!("json.tmp.{}", std::process::id()));
    write_private(&tmp, body.as_bytes())?;
    if let Err(e) = std::fs::rename(&tmp, &path) {
        if let Err(rm) = std::fs::remove_file(&tmp) {
            eprintln!("lumen: cannot remove {}: {rm}", tmp.display());
        }
        return Err(e);
    }
    Ok(())
}

fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    opts.open(path)?.write_all(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(s: &str) -> FileState {
        FileState::Text(s.to_string())
    }

    #[test]
    fn no_env_and_no_file_is_on() {
        let r = resolve(None, &FileState::Absent);
        assert_eq!(r.mode, Mode::On);
        assert_eq!(r.source, Source::Default);
        assert_eq!(r.problem, None);
    }

    #[test]
    fn an_empty_env_says_nothing() {
        assert_eq!(resolve(Some(""), &FileState::Absent).mode, Mode::On);
        assert_eq!(
            resolve(Some("  "), &text(r#"{"repeat_reads":false}"#)).mode,
            Mode::Off
        );
    }

    #[test]
    fn env_off_beats_a_file_that_says_on() {
        let r = resolve(Some("off"), &text(r#"{"repeat_reads":true}"#));
        assert_eq!(
            (r.mode, r.source, r.problem),
            (Mode::Off, Source::Env, None)
        );
    }

    #[test]
    fn env_on_beats_a_file_that_says_off() {
        let r = resolve(Some("ON"), &text(r#"{"repeat_reads":false}"#));
        assert_eq!((r.mode, r.source, r.problem), (Mode::On, Source::Env, None));
    }

    #[test]
    fn the_file_decides_when_env_is_silent() {
        assert_eq!(
            resolve(None, &text(r#"{"repeat_reads":false}"#)).mode,
            Mode::Off
        );
        assert_eq!(
            resolve(None, &text(r#"{"repeat_reads":true}"#)).mode,
            Mode::On
        );
    }

    #[test]
    fn every_spelling_of_off_is_off_and_not_a_problem() {
        for v in ["off", "OFF", " off ", "0", "false"] {
            let r = resolve(Some(v), &FileState::Absent);
            assert_eq!((r.mode, r.problem), (Mode::Off, None), "{v:?}");
        }
    }

    #[test]
    fn ab_is_still_available_through_the_env() {
        assert_eq!(resolve(Some("ab"), &FileState::Absent).mode, Mode::Ab);
    }

    #[test]
    fn an_unknown_env_value_is_off_and_reported_and_does_not_fall_to_the_file() {
        let r = resolve(Some("enabled"), &text(r#"{"repeat_reads":true}"#));
        assert_eq!(r.mode, Mode::Off);
        assert_eq!(r.problem, Some(Problem::Env("enabled".to_string())));
    }

    #[test]
    fn a_corrupt_or_wrong_typed_file_is_off_and_reported() {
        for body in [
            "{not json",
            "",
            "[]",
            "true",
            r#"{"repeat_reads":"yes"}"#,
            r#"{"repeat_reads":null}"#,
            r#"{"repeat_reads":1}"#,
        ] {
            let r = resolve(None, &text(body));
            assert_eq!(r.mode, Mode::Off, "{body:?}");
            assert!(matches!(r.problem, Some(Problem::File(_))), "{body:?}");
        }
    }

    #[test]
    fn an_unreadable_file_is_off_and_reported() {
        let r = resolve(None, &FileState::Unreadable("permission denied".into()));
        assert_eq!(r.mode, Mode::Off);
        assert!(matches!(r.problem, Some(Problem::File(_))));
    }

    #[test]
    fn a_file_with_other_keys_only_says_nothing_about_repeat_reads() {
        // A mistyped key is an unknown key: ignored, and the default, on, stands. The README
        // says so and tells the reader to check what `lumen doctor` resolved.
        for body in [
            r#"{"something_else":1}"#,
            r#"{"repeat_read":false}"#,
            r#"{"Repeat_Reads":false}"#,
            "{}",
        ] {
            let r = resolve(None, &text(body));
            assert_eq!(
                (r.mode, r.source, r.problem),
                (Mode::On, Source::Default, None),
                "{body}"
            );
        }
    }

    #[test]
    fn a_byte_order_mark_utf16_or_an_empty_file_is_corrupt_not_a_setting() {
        // What the README promises: UTF-8 without a BOM. The ways an editor gets that wrong
        // are rejected as corrupt (off, with a fault), never guessed at and never read as on.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE_NAME);
        let json = "{\"repeat_reads\":true}";
        let utf16: Vec<u8> = format!("\u{feff}{json}")
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        for (what, bytes) in [
            ("utf-8 with a BOM", format!("\u{feff}{json}").into_bytes()),
            ("utf-16", utf16),
            ("empty", Vec::new()),
        ] {
            std::fs::write(&path, bytes).unwrap();
            let r = resolve(None, &read_file(&path));
            assert_eq!(r.mode, Mode::Off, "{what}");
            assert!(matches!(r.problem, Some(Problem::File(_))), "{what}: {r:?}");
        }
        std::fs::write(&path, json).unwrap();
        assert_eq!(
            resolve(None, &read_file(&path)).mode,
            Mode::On,
            "the same JSON as plain UTF-8 is read"
        );
    }

    #[test]
    fn a_file_problem_offers_only_remedies_that_exist_today() {
        let file = Path::new("/data/lumen_settings.json");
        let said = Problem::File("not JSON: x".into()).describe(file);
        assert!(said.contains("/data/lumen_settings.json"), "{said}");
        assert!(said.contains("delete the file"), "{said}");
        assert!(said.contains(REPEAT_READS_ENV), "{said}");
        // There is no switch in the app to go back to: do not send anyone looking for one.
        assert!(!said.contains("in the app"), "{said}");
        assert!(
            !Problem::Env("x".into())
                .describe(file)
                .contains("in the app"),
            "the same for the environment variable's message"
        );
    }

    #[test]
    fn a_directory_where_the_file_should_be_is_a_problem_not_absence() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(FILE_NAME)).unwrap();
        let r = repeat_reads_for(&dir.path().join("ledger.db"));
        assert_eq!(r.mode, Mode::Off);
        assert!(matches!(r.problem, Some(Problem::File(_))), "{r:?}");
    }

    #[test]
    fn the_file_sits_beside_the_ledger() {
        assert_eq!(
            settings_path(Path::new("/x/y/lumen.db")),
            Path::new("/x/y").join(FILE_NAME)
        );
        assert_eq!(
            settings_path(Path::new("lumen.db")),
            Path::new(".").join(FILE_NAME)
        );
    }

    #[test]
    fn what_the_writer_writes_the_reader_reads_and_nothing_is_left_behind() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("ledger.db");
        write_repeat_reads(&db, false).unwrap();
        assert_eq!(read_file_mode(&db), Mode::Off);
        write_repeat_reads(&db, true).unwrap();
        assert_eq!(read_file_mode(&db), Mode::On);
        let left: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(left, vec![FILE_NAME.to_string()]);
    }

    fn read_file_mode(db: &Path) -> Mode {
        resolve(None, &read_file(&settings_path(db))).mode
    }

    #[test]
    fn the_writer_keeps_other_keys_and_repairs_a_corrupt_file() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("ledger.db");
        let path = settings_path(&db);
        std::fs::write(&path, r#"{"theme":"dark"}"#).unwrap();
        write_repeat_reads(&db, false).unwrap();
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(v["theme"], "dark");
        assert_eq!(v["repeat_reads"], false);
        std::fs::write(&path, "{broken").unwrap();
        write_repeat_reads(&db, true).unwrap();
        assert_eq!(read_file_mode(&db), Mode::On);
    }

    #[cfg(unix)]
    #[test]
    fn the_written_file_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("ledger.db");
        write_repeat_reads(&db, true).unwrap();
        let mode = std::fs::metadata(settings_path(&db))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0, "{mode:o}");
    }

    #[test]
    fn the_writer_creates_a_missing_directory() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("not").join("yet").join("ledger.db");
        write_repeat_reads(&db, false).unwrap();
        assert_eq!(read_file_mode(&db), Mode::Off);
    }
}
