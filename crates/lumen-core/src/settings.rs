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
//! { "repeat_reads": false, "billing_mode": "subscription" }
//! ```
//!
//! `billing_mode` is how the user pays for Claude Code, asked once in the app (1.9.0). It is
//! read and validated like `repeat_reads`, from the same capped read, but it decides nothing
//! here: a bad value in it is reported where it is read and leaves repeat reads alone.
//!
//! It is read on every `tools/call`, not once at startup: a setting change takes effect on the
//! very next read, with no restart of Claude Code. The read is one stat and one small read,
//! never more than 64 KiB.
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
/// The key for how the user pays for Claude Code.
pub const BILLING_MODE_KEY: &str = "billing_mode";
/// The most the settings file may hold. It is read on every call; a larger one is a mistake.
pub const MAX_FILE_BYTES: u64 = 64 * 1024;
/// The most characters of a bad value that a problem repeats back.
const ECHO_MAX: usize = 64;

/// Held by every writer from its read of the file to its rename, so two writers in this process
/// take turns.
///
/// Each writer reads the file, changes its own key and writes the whole object back. The app
/// runs the Repeat reads switch and the billing choice on separate blocking tasks, and without
/// this two of them at once could each write back the other's old value: a lost
/// `repeat_reads: false` turns repeat reads back on with nothing said. Only the app writes the
/// file; across processes each write is still whole (it is renamed into place) and the last
/// one wins.
static WRITE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Numbers each temporary file this process writes, so no two writes share one.
static WRITE_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The writers' turn. A writer that panicked holding it cannot have left half a file, since the
/// file is only ever replaced by a rename, so a poisoned lock is taken as it stands.
fn write_turn() -> std::sync::MutexGuard<'static, ()> {
    WRITE_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

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
                 Fix or delete the file (deleting it gives the default, on), or choose a \
                 setting with the Repeat reads switch on the Settings screen of the main \
                 window in the Lumen app, which rewrites it. Setting {REPEAT_READS_ENV} to on \
                 or off takes precedence over the file.",
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

/// The text of the regular file at `path`, read to [`MAX_FILE_BYTES`], or why it cannot be used.
pub fn read_file(path: &Path) -> FileState {
    let opened = std::fs::metadata(path).and_then(|m| {
        if m.is_file() {
            std::fs::File::open(path)
        } else {
            Err(std::io::Error::other("not a regular file"))
        }
    });
    match opened.and_then(read_capped) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => FileState::Absent,
        Err(e) => FileState::Unreadable(e.to_string()),
        Ok(bytes) if bytes.len() as u64 > MAX_FILE_BYTES => {
            FileState::Unreadable(format!("larger than {MAX_FILE_BYTES} bytes"))
        }
        Ok(bytes) => match String::from_utf8(bytes) {
            Ok(t) => FileState::Text(t),
            Err(_) => FileState::Unreadable("stream did not contain valid UTF-8".to_string()),
        },
    }
}

/// At most one byte past the cap: enough to tell a file over it from one at it.
fn read_capped(r: impl std::io::Read) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let mut bytes = Vec::new();
    r.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// `s` as it is, or its first [`ECHO_MAX`] characters and an ellipsis.
fn bounded(s: &str) -> String {
    match s.char_indices().nth(ECHO_MAX) {
        Some((cut, _)) => format!("{}…", &s[..cut]),
        None => s.to_string(),
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
        _ => Err(bounded(t)),
    })
}

/// The file's top-level object: `Ok(None)` when there is no file, `Err` when it cannot be used.
/// Every key is read through this, so each says the same about a file that is not a setting.
fn parse_object(
    state: &FileState,
) -> Result<Option<serde_json::Map<String, serde_json::Value>>, String> {
    let text = match state {
        FileState::Absent => return Ok(None),
        FileState::Unreadable(why) => return Err(format!("unreadable: {why}")),
        FileState::Text(t) => t,
    };
    let v: serde_json::Value = serde_json::from_str(text).map_err(|e| format!("not JSON: {e}"))?;
    match v {
        serde_json::Value::Object(obj) => Ok(Some(obj)),
        _ => Err("not a JSON object".to_string()),
    }
}

/// The setting file's meaning: `Ok(None)` when it says nothing about repeat reads.
fn parse_file(state: &FileState) -> Result<Option<bool>, String> {
    let Some(obj) = parse_object(state)? else {
        return Ok(None);
    };
    match obj.get(REPEAT_READS_KEY) {
        None => Ok(None),
        Some(serde_json::Value::Bool(b)) => Ok(Some(*b)),
        Some(other) => Err(format!(
            "{REPEAT_READS_KEY} must be true or false, not {}",
            bounded(&other.to_string())
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
            problem: Some(Problem::Env(bounded(&raw.to_string_lossy()))),
        },
    }
}

/// How the user pays for Claude Code, which decides what a saving is worth to them: on the API a
/// token not sent is money not spent, on a subscription it is room under the plan's limits.
///
/// There is no default. The file says nothing until the user has answered the app's question,
/// and "not asked yet" is a state of its own (`Ok(None)` from [`billing_mode_for`]), never a
/// guess at one of these.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BillingMode {
    /// A Claude Pro or Max plan.
    Subscription,
    /// The API, paid per token.
    Api,
}

impl BillingMode {
    /// The spelling in the file and between the app's window and its backend.
    pub fn as_str(self) -> &'static str {
        match self {
            BillingMode::Subscription => "subscription",
            BillingMode::Api => "api",
        }
    }

    /// The mode `s` spells, exactly; anything else is not one. Exact because the file is
    /// written by the app, and a near miss is more likely a mistake than a choice.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "subscription" => Some(BillingMode::Subscription),
            "api" => Some(BillingMode::Api),
            _ => None,
        }
    }
}

/// What the file says about billing mode: `Ok(None)` when nothing (not asked yet), `Err(why)`
/// when the file cannot be used or the value is not a mode. An unknown value is rejected and
/// repeated back short, as a bad `repeat_reads` is.
pub fn parse_billing_mode(state: &FileState) -> Result<Option<BillingMode>, String> {
    let Some(obj) = parse_object(state)? else {
        return Ok(None);
    };
    let Some(value) = obj.get(BILLING_MODE_KEY) else {
        return Ok(None);
    };
    match value.as_str().and_then(BillingMode::parse) {
        Some(mode) => Ok(Some(mode)),
        None => Err(format!(
            "{BILLING_MODE_KEY} must be \"subscription\" or \"api\", not {}",
            bounded(&value.to_string())
        )),
    }
}

/// The billing mode for a ledger at `db`, read now, through the same capped read as the
/// repeat-reads switch.
pub fn billing_mode_for(db: &Path) -> Result<Option<BillingMode>, String> {
    parse_billing_mode(&read_file(&settings_path(db)))
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

/// Write the persisted setting for a ledger at `db`.
///
/// The Repeat reads switch in the Lumen app (`set_repeat_reads_setting`) calls this.
/// It sits beside the reader so the two agree on the format, and the tests round-trip it.
///
/// Atomic (written beside, renamed into place) so a reader never sees half a file, and
/// private to this user. Other keys in a valid existing file are kept. A file that cannot be
/// used is replaced: choosing the setting is the repair.
pub fn write_repeat_reads(db: &Path, on: bool) -> std::io::Result<()> {
    let path = settings_path(db);
    let _turn = write_turn();
    let mut obj = match read_file(&path) {
        FileState::Text(t) => match serde_json::from_str::<serde_json::Value>(&t) {
            Ok(serde_json::Value::Object(o)) => o,
            _ => serde_json::Map::new(),
        },
        FileState::Absent | FileState::Unreadable(_) => serde_json::Map::new(),
    };
    obj.insert(REPEAT_READS_KEY.to_string(), serde_json::Value::Bool(on));
    write_object(&path, obj)
}

/// Write the billing mode for a ledger at `db`. The Billing mode setting in the Lumen app
/// (`set_billing_mode_setting`) calls this.
///
/// Other keys are kept, as [`write_repeat_reads`] keeps them. Unlike that writer, this one
/// leaves a file that cannot be used alone and returns why: replacing it would drop whatever it
/// said about repeat reads, and a file that cannot be used means repeat reads are off, so a
/// file holding only this key would turn them back on, which nobody chose here. The Repeat
/// reads switch is the repair for such a file.
pub fn write_billing_mode(db: &Path, mode: BillingMode) -> std::io::Result<()> {
    let path = settings_path(db);
    let _turn = write_turn();
    // Not naming the path: every caller says which file it was writing.
    let existing = parse_object(&read_file(&path)).map_err(|why| {
        std::io::Error::other(format!(
            "the settings file cannot be used ({why}), so it is left as it is"
        ))
    })?;
    // No file yet: this is the first key in a new one.
    let mut obj = match existing {
        Some(obj) => obj,
        None => serde_json::Map::new(),
    };
    obj.insert(
        BILLING_MODE_KEY.to_string(),
        serde_json::Value::String(mode.as_str().to_string()),
    );
    write_object(&path, obj)
}

/// Write `obj` as the settings file at `path`: atomic (written beside, renamed into place) so
/// a reader never sees half a file, and private to this user. The caller holds [`write_turn`].
///
/// The temporary name carries the process id and a number of its own: with the process id
/// alone, two writes at once wrote the same file, and one renamed it away before the other
/// could, which then failed.
fn write_object(
    path: &Path,
    obj: serde_json::Map<String, serde_json::Value>,
) -> std::io::Result<()> {
    let body = serde_json::to_string_pretty(&serde_json::Value::Object(obj))
        .map_err(std::io::Error::other)?;
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let seq = WRITE_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = path.with_extension(format!("json.tmp.{}.{seq}", std::process::id()));
    write_private(&tmp, body.as_bytes())?;
    if let Err(e) = std::fs::rename(&tmp, path) {
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
        // The main window's switch rewrites the file, so it repairs a file that cannot be used.
        assert!(
            said.contains("Repeat reads switch on the Settings screen of the main window"),
            "{said}"
        );
        // It writes the file, and the variable outranks the file: a bad value there is not
        // something it can fix, so that message must not send anyone to it.
        assert!(
            !Problem::Env("x".into())
                .describe(file)
                .contains("Settings screen"),
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

    #[cfg(unix)]
    #[test]
    fn a_reader_holding_the_file_open_still_sees_the_whole_old_one_after_a_write() {
        use std::io::Read;
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("ledger.db");
        write_repeat_reads(&db, false).unwrap();
        let before = std::fs::read_to_string(settings_path(&db)).unwrap();
        let mut held = std::fs::File::open(settings_path(&db)).unwrap();
        write_repeat_reads(&db, true).unwrap();
        let mut seen = String::new();
        held.read_to_string(&mut seen).unwrap();
        assert_eq!(seen, before);
        assert_eq!(read_file_mode(&db), Mode::On);
    }

    #[test]
    fn the_writer_creates_a_missing_directory() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("not").join("yet").join("ledger.db");
        write_repeat_reads(&db, false).unwrap();
        assert_eq!(read_file_mode(&db), Mode::Off);
    }

    fn padded_to(len: usize) -> String {
        let bare = r#"{"repeat_reads":false,"pad":""}"#.len();
        format!(
            r#"{{"repeat_reads":false,"pad":"{}"}}"#,
            "x".repeat(len - bare)
        )
    }

    #[test]
    fn a_file_at_the_cap_is_read_and_one_byte_over_is_not() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE_NAME);
        let cap = MAX_FILE_BYTES as usize;
        std::fs::write(&path, padded_to(cap)).unwrap();
        assert_eq!(resolve(None, &read_file(&path)).mode, Mode::Off);
        assert!(
            resolve(None, &read_file(&path)).problem.is_none(),
            "a file at the cap is a setting"
        );
        std::fs::write(&path, padded_to(cap + 1)).unwrap();
        let r = resolve(None, &read_file(&path));
        assert_eq!(r.mode, Mode::Off);
        assert!(
            matches!(&r.problem, Some(Problem::File(why)) if why.contains("larger than")),
            "{r:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_pipe_where_the_file_should_be_is_a_problem_and_never_blocks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE_NAME);
        let made = std::process::Command::new("mkfifo")
            .arg(&path)
            .status()
            .unwrap();
        assert!(made.success());
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || tx.send(read_file(&path)).unwrap());
        match rx.recv_timeout(std::time::Duration::from_secs(10)) {
            Ok(FileState::Unreadable(why)) => {
                assert!(why.contains("not a regular file"), "{why}")
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_reader_that_never_ends_is_read_only_up_to_the_cap() {
        struct Endless(u64);
        impl std::io::Read for Endless {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                if self.0 > MAX_FILE_BYTES + 1 {
                    return Err(std::io::Error::other("read past the cap"));
                }
                buf.fill(b'x');
                self.0 += buf.len() as u64;
                Ok(buf.len())
            }
        }
        let bytes = read_capped(Endless(0)).unwrap();
        assert_eq!(bytes.len() as u64, MAX_FILE_BYTES + 1);
    }

    #[test]
    fn a_wrong_typed_value_is_echoed_short_and_a_short_one_whole() {
        let prefix = "repeat_reads must be true or false, not ";
        let long = "y".repeat(10_000);
        let why = parse_file(&text(&format!(r#"{{"repeat_reads":"{long}"}}"#))).unwrap_err();
        assert_eq!(why, format!("{prefix}\"{}…", "y".repeat(63)));
        let why = parse_file(&text(&format!(r#"{{"repeat_reads":[{long:?}]}}"#))).unwrap_err();
        assert_eq!(
            why.chars().count(),
            prefix.chars().count() + 64 + 1,
            "{why}"
        );
        let short = parse_file(&text(r#"{"repeat_reads":"yes"}"#)).unwrap_err();
        assert_eq!(short, format!("{prefix}\"yes\""));
    }

    #[test]
    fn an_echo_keeps_64_characters_and_cuts_the_65th() {
        assert_eq!(bounded(&"é".repeat(64)), "é".repeat(64));
        assert_eq!(bounded(&"é".repeat(65)), format!("{}…", "é".repeat(64)));
    }

    #[test]
    fn an_env_value_that_means_nothing_is_echoed_short_and_a_short_one_whole() {
        for long in ["z".repeat(10_000), "é".repeat(10_000)] {
            let first_64: String = long.chars().take(64).collect();
            assert_eq!(
                resolve(Some(&long), &FileState::Absent).problem,
                Some(Problem::Env(format!("{first_64}…")))
            );
        }
        assert_eq!(
            resolve(Some("bogus"), &FileState::Absent).problem,
            Some(Problem::Env("bogus".into()))
        );
    }

    // ── billing mode ──────────────────────────────────────────────────────────

    #[test]
    fn each_billing_mode_is_read_as_itself() {
        for (body, mode) in [
            (
                r#"{"billing_mode":"subscription"}"#,
                BillingMode::Subscription,
            ),
            (r#"{"billing_mode":"api"}"#, BillingMode::Api),
            (
                r#"{"repeat_reads":false,"billing_mode":"api"}"#,
                BillingMode::Api,
            ),
        ] {
            assert_eq!(parse_billing_mode(&text(body)), Ok(Some(mode)), "{body}");
        }
    }

    #[test]
    fn no_file_or_no_key_is_not_asked_yet() {
        assert_eq!(parse_billing_mode(&FileState::Absent), Ok(None));
        for body in [
            "{}",
            r#"{"repeat_reads":true}"#,
            r#"{"Billing_Mode":"api"}"#,
        ] {
            assert_eq!(parse_billing_mode(&text(body)), Ok(None), "{body}");
        }
    }

    #[test]
    fn an_unknown_or_wrong_typed_billing_mode_is_rejected_and_repeated_back() {
        for (body, echoed) in [
            (r#"{"billing_mode":"pro"}"#, r#""pro""#),
            (r#"{"billing_mode":"API"}"#, r#""API""#),
            (r#"{"billing_mode":" api"}"#, r#"" api""#),
            (r#"{"billing_mode":""}"#, r#""""#),
            (r#"{"billing_mode":true}"#, "true"),
            (r#"{"billing_mode":1}"#, "1"),
            (r#"{"billing_mode":null}"#, "null"),
            (r#"{"billing_mode":["api"]}"#, r#"["api"]"#),
        ] {
            let why = parse_billing_mode(&text(body)).unwrap_err();
            assert_eq!(
                why,
                format!(r#"billing_mode must be "subscription" or "api", not {echoed}"#),
                "{body}"
            );
        }
        let long = "z".repeat(10_000);
        let why =
            parse_billing_mode(&text(&format!(r#"{{"billing_mode":"{long}"}}"#))).unwrap_err();
        assert!(why.ends_with(&format!("\"{}…", "z".repeat(63))), "{why}");
    }

    #[test]
    fn a_file_that_cannot_be_used_is_an_error_for_billing_mode_too() {
        for body in ["{not json", "", "[]", "\"api\""] {
            assert!(parse_billing_mode(&text(body)).is_err(), "{body:?}");
        }
        assert!(
            parse_billing_mode(&FileState::Unreadable("permission denied".into()))
                .unwrap_err()
                .starts_with("unreadable:")
        );
    }

    #[test]
    fn a_settings_file_over_the_cap_gives_no_billing_mode() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("ledger.db");
        let path = settings_path(&db);
        let bare = r#"{"billing_mode":"api","pad":""}"#.len();
        let padded = |len: usize| {
            format!(
                r#"{{"billing_mode":"api","pad":"{}"}}"#,
                "x".repeat(len - bare)
            )
        };
        std::fs::write(&path, padded(MAX_FILE_BYTES as usize)).unwrap();
        assert_eq!(billing_mode_for(&db), Ok(Some(BillingMode::Api)));
        std::fs::write(&path, padded(MAX_FILE_BYTES as usize + 1)).unwrap();
        let why = billing_mode_for(&db).unwrap_err();
        assert!(why.contains("larger than"), "{why}");
    }

    #[test]
    fn a_bad_billing_mode_leaves_repeat_reads_alone_and_the_other_way_round() {
        let r = resolve(
            None,
            &text(r#"{"repeat_reads":false,"billing_mode":"pro"}"#),
        );
        assert_eq!(
            (r.mode, r.source, r.problem),
            (Mode::Off, Source::File, None)
        );
        assert_eq!(
            parse_billing_mode(&text(r#"{"repeat_reads":"yes","billing_mode":"api"}"#)),
            Ok(Some(BillingMode::Api))
        );
    }

    #[test]
    fn what_the_billing_writer_writes_the_reader_reads_and_other_keys_stay() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("not").join("yet").join("ledger.db");
        assert_eq!(billing_mode_for(&db), Ok(None));
        write_billing_mode(&db, BillingMode::Subscription).unwrap();
        assert_eq!(billing_mode_for(&db), Ok(Some(BillingMode::Subscription)));
        write_repeat_reads(&db, false).unwrap();
        write_billing_mode(&db, BillingMode::Api).unwrap();
        assert_eq!(billing_mode_for(&db), Ok(Some(BillingMode::Api)));
        assert_eq!(read_file_mode(&db), Mode::Off, "repeat reads kept");
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(settings_path(&db)).unwrap()).unwrap();
        assert_eq!(
            v,
            serde_json::json!({ "repeat_reads": false, "billing_mode": "api" })
        );
        let left: Vec<_> = std::fs::read_dir(settings_path(&db).parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(left, vec![FILE_NAME.to_string()]);
    }

    #[test]
    fn the_billing_writer_repairs_a_bad_value_but_leaves_an_unusable_file_alone() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("ledger.db");
        let path = settings_path(&db);
        std::fs::write(&path, r#"{"billing_mode":"pro","theme":"dark"}"#).unwrap();
        write_billing_mode(&db, BillingMode::Api).unwrap();
        assert_eq!(billing_mode_for(&db), Ok(Some(BillingMode::Api)));
        // A file that cannot be used turns repeat reads off; replacing it with this key alone
        // would turn them back on. It is left byte for byte, and the error says why.
        for body in ["{broken", "[]"] {
            std::fs::write(&path, body).unwrap();
            let err = write_billing_mode(&db, BillingMode::Subscription).unwrap_err();
            assert!(err.to_string().contains("left as it is"), "{err}");
            assert!(err.to_string().contains("cannot be used"), "{err}");
            assert_eq!(std::fs::read_to_string(&path).unwrap(), body);
            assert_eq!(read_file_mode(&db), Mode::Off);
        }
    }

    #[cfg(unix)]
    #[test]
    fn the_billing_writer_writes_a_private_file() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("ledger.db");
        write_billing_mode(&db, BillingMode::Api).unwrap();
        let mode = std::fs::metadata(settings_path(&db))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0, "{mode:o}");
    }

    #[test]
    fn each_billing_mode_spells_back_as_it_parses() {
        for m in [BillingMode::Subscription, BillingMode::Api] {
            assert_eq!(BillingMode::parse(m.as_str()), Some(m));
        }
    }

    /// The app writes both keys from blocking tasks that can run at once (the Repeat reads
    /// switch and the billing choice). Each writer reads the file, changes its key and writes
    /// it back, so two at once could each keep the other's old value, and both used the same
    /// temporary name, so one could rename the other's file away and fail. Measured before the
    /// fix: of 2,000 rounds, 395 lost `repeat_reads: false` (repeat reads silently back on)
    /// and 1,931 had a writer fail.
    #[test]
    fn two_writers_at_once_lose_neither_key_and_neither_fails() {
        use std::sync::{Arc, Barrier};
        const ROUNDS: usize = 300;
        let (mut lost_repeat, mut lost_billing, mut failed) = (0, 0, Vec::new());
        for _ in 0..ROUNDS {
            let dir = tempfile::tempdir().unwrap();
            let db = dir.path().join("ledger.db");
            let start = Arc::new(Barrier::new(2));
            let (db_a, start_a) = (db.clone(), Arc::clone(&start));
            let a = std::thread::spawn(move || {
                start_a.wait();
                write_repeat_reads(&db_a, false)
            });
            let (db_b, start_b) = (db.clone(), Arc::clone(&start));
            let b = std::thread::spawn(move || {
                start_b.wait();
                write_billing_mode(&db_b, BillingMode::Api)
            });
            for result in [a.join().unwrap(), b.join().unwrap()] {
                if let Err(e) = result {
                    failed.push(e.to_string());
                }
            }
            if read_file_mode(&db) != Mode::Off {
                lost_repeat += 1;
            }
            if billing_mode_for(&db) != Ok(Some(BillingMode::Api)) {
                lost_billing += 1;
            }
            let left: Vec<_> = std::fs::read_dir(dir.path())
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            assert_eq!(
                left,
                vec![FILE_NAME.to_string()],
                "a temporary file was left"
            );
        }
        assert_eq!(
            (lost_repeat, lost_billing, failed.len()),
            (0, 0, 0),
            "of {ROUNDS} rounds: repeat_reads lost, billing_mode lost, writers failed \
             (first failure: {:?})",
            failed.first()
        );
    }
}
