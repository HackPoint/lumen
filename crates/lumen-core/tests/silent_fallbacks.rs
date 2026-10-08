//! No failure may be recorded as a plausible value.
//!
//! The same bug has shipped four times. Each time a failure was answered with a value
//! that looked like a measurement, so nothing downstream could tell it from one:
//!
//!   - The DMG tokenizer (0.1.0 to 1.1.4). Setup baked a tokenizer path inside the
//!     mounted disk image; once it was ejected the meter hook fell back to `bytes ÷ 4`
//!     without saying so, and the figures were described as measured.
//!   - Ingest in 1.1.3. A column the daemon binds was missing on upgraded databases, so
//!     every insert failed and the gauge froze at its last value, with no error shown.
//!   - `|| echo 0`. The developer meter hook ran `$("$LUMEN_TOK" < "$f" || echo 0)`, so a
//!     PNG was recorded as `full_tokens=0, token_source='measured'`.
//!   - The metering INSERT, until 1.6.0: `' 2>/dev/null || true`. A failed insert left no
//!     trace, in the table every figure is computed from.
//!
//! A guard was asked for in 1.2.1 and never written, which is why there was a fourth.
//! This is it, in the shape of `every_step_run_setup_emits_is_accounted_for`: it scans
//! the shell scripts and the production Rust for the shapes such a fallback takes, and
//! fails on every one not listed in [`ALLOWED`] with the reason it is not a measurement.
//! The hook templates are string literals in `setup.rs`, so they are scanned there as
//! well as in their generated copies. A new fallback has two ways out: report the failure
//! (a fault, a stderr line, a returned error), or explain itself here.
//!
//! A text scan, not a parser. It finds the shapes this codebase has used, not every way
//! there is to write one.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use Shape::{DroppedWrite, EchoNumber, NumberForError, OrDefault, OrTrue};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Shape {
    /// `|| echo 0`, `|| printf 0`: a failed command's output replaced by a number.
    EchoNumber,
    /// `|| true`, `|| :`: a failed command's status thrown away. A scan cannot tell which
    /// commands write data, so every one is listed.
    OrTrue,
    /// `.unwrap_or(0)`, `.map_or(0, …)`, `.unwrap_or_else(|_| 0)`: an error or an absence
    /// read as a number.
    NumberForError,
    /// `.unwrap_or_default()`: the same whenever the default is a number, which a scan
    /// cannot see, so every one is listed.
    OrDefault,
    /// `let _ = <write>`, `<write>.ok();`: a failed write thrown away.
    DroppedWrite,
}

impl Shape {
    fn label(self) -> &'static str {
        match self {
            EchoNumber => "|| echo <number>",
            OrTrue => "|| true",
            NumberForError => "unwrap_or(<number>)",
            OrDefault => "unwrap_or_default()",
            DroppedWrite => "discarded write",
        }
    }
}

/// A fallback that is not a measurement, and why.
///
/// It covers the hits in `file` of `shape` whose statement contains `needle`, and must
/// cover exactly `n` of them: an entry that matches nothing is stale, and one that matches
/// more than it says has started excusing code nobody looked at.
struct Allowed {
    file: &'static str,
    shape: Shape,
    needle: &'static str,
    n: usize,
    why: &'static str,
}

const fn allow(
    file: &'static str,
    shape: Shape,
    needle: &'static str,
    why: &'static str,
) -> Allowed {
    Allowed {
        file,
        shape,
        needle,
        n: 1,
        why,
    }
}

impl Allowed {
    const fn times(self, n: usize) -> Allowed {
        Allowed { n, ..self }
    }

    fn covers(&self, hit: &Hit) -> bool {
        self.file == hit.file && self.shape == hit.shape && hit.statement.contains(self.needle)
    }
}

const ALLOWED: &[Allowed] = &[
    // ── lumen-cli ─────────────────────────────────────────────────────────────────
    allow(
        "crates/lumen-cli/src/doctor.rs",
        OrDefault,
        "f.launch_agent",
        "A path quoted in the stale-login-item finding, which is only built once the agent's \
         target is known to be missing.",
    ),
    allow(
        "crates/lumen-cli/src/doctor.rs",
        OrDefault,
        "dirs::home_dir()",
        "macOS diagnostics: with no home the paths under it are looked up relative to the \
         working directory. A finding in a report, never a figure.",
    ),
    allow(
        "crates/lumen-cli/src/main.rs",
        NumberForError,
        "checked_div(d.window)",
        "Unreachable: resolve_window never returns less than 200,000. The raw fill and window \
         are printed beside the percentage.",
    ),
    // ── lumen-core ────────────────────────────────────────────────────────────────
    allow(
        "crates/lumen-core/src/compress.rs",
        NumberForError,
        r#"t.find(": ")"#,
        "A string offset: a line with no \": \" is matched whole.",
    ),
    allow(
        "crates/lumen-core/src/faults.rs",
        NumberForError,
        "UNIX_EPOCH",
        "A clock before 1970 stamps the fault at the epoch: a timestamp, not a count.",
    ),
    allow(
        "crates/lumen-core/src/faults.rs",
        DroppedWrite,
        "append_line(path, rec)",
        "Best-effort by contract: the spool is the last channel there is, and every \
         production caller also logs the failure itself.",
    ),
    allow(
        "crates/lumen-core/src/faults.rs",
        NumberForError,
        "spool_len_at(&p)",
        "With no spool path nothing can have been spooled.",
    ),
    allow(
        "crates/lumen-core/src/faults.rs",
        DroppedWrite,
        "merge_into(path, &taken)",
        "A batch the ledger refuses stays aside, where it is still counted and listed.",
    ),
    allow(
        "crates/lumen-core/src/faults.rs",
        DroppedWrite,
        "remove_file(&taken)",
        "The batch is already in the ledger; one left behind may be counted twice, never lost.",
    ),
    allow(
        "crates/lumen-core/src/meter.rs",
        OrDefault,
        "CLAUDE_CODE_ENTRYPOINT",
        "Unset selects the default channel label.",
    ),
    allow(
        "crates/lumen-core/src/meter.rs",
        DroppedWrite,
        "execute_batch(migration)",
        "A duplicate column is the expected outcome. Any other failure leaves a column \
         missing, and the insert that binds it files meter_write_failed.",
    ),
    allow(
        "crates/lumen-core/src/meter.rs",
        DroppedWrite,
        "set_permissions(&side",
        "Another owner's sidecar keeps its mode, and the write it then refuses is filed as \
         meter_write_failed.",
    ),
    allow(
        "crates/lumen-core/src/meter.rs",
        NumberForError,
        "UNIX_EPOCH",
        "A clock before 1970 stamps the row at the epoch: a timestamp, not a count.",
    ),
    allow(
        "crates/lumen-core/src/ranked.rs",
        NumberForError,
        r#"== "name""#,
        "Unreachable: the loop moves on unless the match has a @name capture.",
    ),
    allow(
        "crates/lumen-core/src/ranked.rs",
        NumberForError,
        "top_level_hits",
        "A file missing from the map has no top-level hits.",
    ),
    allow(
        "crates/lumen-core/src/ranked.rs",
        OrDefault,
        "LUMEN_RANKED_OUTLINE",
        "Unset selects the default outline mode.",
    ),
    allow(
        "crates/lumen-core/src/report.rs",
        NumberForError,
        "INCLUDE_SOURCE_BYTE_CAP",
        "A truncation offset in an attached source.",
    ),
    allow(
        "crates/lumen-core/src/report.rs",
        NumberForError,
        "recorded_faults(&c)",
        "An unreadable ledger counts as one fault, so the badge lights instead of reading \
         zero.",
    ),
    allow(
        "crates/lumen-core/src/report.rs",
        NumberForError,
        "spool.map_or(0",
        "No spool, nothing waiting in one.",
    ),
    allow(
        "crates/lumen-core/src/report.rs",
        DroppedWrite,
        "remove_file(path)",
        "Deleting an owner-only scratch file this process created; nothing reads it again.",
    )
    .times(2),
    allow(
        "crates/lumen-core/src/report.rs",
        NumberForError,
        "parse().unwrap_or(0)",
        "0 is not an HTTP status: the callers accept 200 or 2xx and quote any other code \
         in their error.",
    ),
    allow(
        "crates/lumen-core/src/report.rs",
        DroppedWrite,
        "remove_file(&path)",
        "Deleting the issue-body scratch file once gh has read it.",
    ),
    allow(
        "crates/lumen-core/src/report.rs",
        OrDefault,
        r#"get("url")"#,
        "A link shown beside an existing issue's number; commenting uses the number.",
    ),
    allow(
        "crates/lumen-core/src/report.rs",
        OrDefault,
        "let html =",
        "The link shown after filing, once the POST has already succeeded.",
    ),
    allow(
        "crates/lumen-core/src/schema.rs",
        DroppedWrite,
        "AssertSqlSafe(*migration)",
        "A duplicate column is the expected outcome. Any other failure leaves a column \
         missing, and the insert that binds it fails and is filed (ingest_failed, \
         meter_write_failed): how 1.1.3's missing column would show now.",
    ),
    allow(
        "crates/lumen-core/src/update.rs",
        OrDefault,
        "serde_json::from_str(&t)",
        "Update-check bookkeeping: state that cannot be read means a check is due.",
    ),
    allow(
        "crates/lumen-core/src/update.rs",
        DroppedWrite,
        "fs::write(path, json)",
        "Update-check bookkeeping: a lost save means the next launch checks again and may \
         repeat a notice.",
    ),
    // ── lumen-daemon ──────────────────────────────────────────────────────────────
    allow(
        "crates/lumen-daemon/src/main.rs",
        OrDefault,
        "LUMEN_LOG",
        "Unset means not verbose.",
    ),
    allow(
        "crates/lumen-daemon/src/main.rs",
        DroppedWrite,
        "create_dir_all(parent)",
        "The connect just below opens the ledger in that directory and returns its error.",
    ),
    allow(
        "crates/lumen-daemon/src/main.rs",
        NumberForError,
        "unwrap_or(-1)",
        "A startup log line, where -1 cannot pass for a count of turns.",
    ),
    allow(
        "crates/lumen-daemon/src/main.rs",
        OrDefault,
        "rec.message.model",
        "The model label in the live broadcast; the ledger row binds the Option itself.",
    ),
    allow(
        "crates/lumen-daemon/src/main.rs",
        NumberForError,
        "fill.unwrap_or(0)",
        "SQL NULL, not an error: a session of subagent turns only has no main-agent fill.",
    ),
    // ── lumen-mcp ─────────────────────────────────────────────────────────────────
    allow(
        "crates/lumen-mcp/src/hook.rs",
        DroppedWrite,
        "lumen_hook_dump.json",
        "LUMEN_DEBUG=1 fixture capture; a debugging aid must not cost a row.",
    ),
    allow(
        "crates/lumen-mcp/src/lib.rs",
        OrDefault,
        "start.map",
        "An absent bound, formatted into a cache key.",
    ),
    allow(
        "crates/lumen-mcp/src/lib.rs",
        OrDefault,
        "end.map",
        "An absent bound, formatted into a cache key.",
    ),
    // ── lumen-stats ───────────────────────────────────────────────────────────────
    allow(
        "crates/lumen-stats/src/lib.rs",
        NumberForError,
        "factor.0",
        "The identity, for a ledger with no calibration rows; nothing in the app or CLI \
         displays the factor.",
    ),
    allow(
        "crates/lumen-stats/src/lib.rs",
        NumberForError,
        "unchanged.get(&path)",
        "A file missing from the GROUP BY had no unchanged re-reads; the query's own error \
         is returned.",
    ),
    // ── lumenator ─────────────────────────────────────────────────────────────────
    allow(
        "lumenator/src-tauri/src/lib.rs",
        OrDefault,
        "available_monitors()",
        "An empty monitor list is classified Unknown, never off-screen.",
    ),
    allow(
        "lumenator/src-tauri/src/lib.rs",
        NumberForError,
        "UNIX_EPOCH",
        "A clock before 1970 reads as the epoch, when no update check is due: a timestamp, \
         not a count.",
    ),
    allow(
        "lumenator/src-tauri/src/setup.rs",
        DroppedWrite,
        "0o755",
        "fs::copy has already carried the source's mode. A copy that still cannot run is \
         reported by the hook as lumen_mcp_unrunnable.",
    ),
    allow(
        "lumenator/src-tauri/src/setup.rs",
        OrDefault,
        r#"stable_binary("lumen-mcp")"#,
        "An empty path means the sidecar was not found: the shim then looks on PATH and \
         reports lumen_mcp_missing.",
    ),
    allow(
        "lumenator/src-tauri/src/setup.rs",
        NumberForError,
        "0o600",
        "The mode for a file that is absent or cannot be stat'd: private, and a mode, not a \
         figure.",
    ),
    allow(
        "lumenator/src-tauri/src/setup.rs",
        DroppedWrite,
        "d.sync_all()",
        "Durability of a rename that has already happened.",
    ),
    allow(
        "lumenator/src-tauri/src/setup.rs",
        DroppedWrite,
        "remove_file(&tmp)",
        "Cleanup on a path that returns the error it is cleaning up after.",
    ),
    allow(
        "lumenator/src-tauri/src/setup.rs",
        OrDefault,
        r#"find_binary("lumen-cli")"#,
        "Only in a build without lumen-cli: the link then reads as stale, and Install CLI \
         says the binary is missing.",
    ),
    allow(
        "lumenator/src-tauri/src/setup.rs",
        OrDefault,
        r#""faults.jsonl""#,
        "Only an empty or root ledger path has no parent. An empty spool makes the hook say \
         the fault could not be written.",
    ),
    allow(
        "lumenator/src-tauri/src/setup.rs",
        DroppedWrite,
        "lumen_dir_in(home)",
        "The first-run marker. Unwritten, Setup shows again at the next launch, and Setup \
         is safe to run twice.",
    ),
    allow(
        "lumenator/src-tauri/src/setup.rs",
        DroppedWrite,
        "marker_path_in(home)",
        "The first-run marker. Unwritten, Setup shows again at the next launch, and Setup \
         is safe to run twice.",
    ),
    allow(
        "lumenator/src-tauri/src/setup.rs",
        DroppedWrite,
        "remove_file(&target)",
        "A target that cannot be removed makes the symlink below fail, and the step reports \
         it; on Windows the copy overwrites it.",
    ),
    // ── Shell ─────────────────────────────────────────────────────────────────────
    allow(
        "scripts/release.sh",
        OrTrue,
        "FEATS=",
        "grep exits 1 for no matching commits, where an empty section is right. git log \
         runs once above, unguarded, under set -e.",
    ),
    allow(
        "scripts/release.sh",
        OrTrue,
        "FIXES=",
        "grep exits 1 for no matching commits, where an empty section is right. git log \
         runs once above, unguarded, under set -e.",
    ),
    allow(
        "scripts/release.sh",
        OrTrue,
        "CHORES=",
        "grep exits 1 for no matching commits, where an empty section is right. git log \
         runs once above, unguarded, under set -e.",
    ),
    allow(
        "scripts/release.sh",
        OrTrue,
        "OTHERS=",
        "grep exits 1 for no matching commits, where an empty section is right. git log \
         runs once above, unguarded, under set -e.",
    ),
    allow(
        "scripts/verify-install.sh",
        OrTrue,
        r#"CLI="$(find_bin"#,
        "Not found is find_bin's answer, and the Binaries checks below report it.",
    ),
    allow(
        "scripts/verify-install.sh",
        OrTrue,
        r#"MCP="$(find_bin"#,
        "Not found is find_bin's answer, and the Binaries checks below report it.",
    ),
    allow(
        "scripts/verify-install.sh",
        OrTrue,
        r#"TOK="$(find_bin"#,
        "Not found is find_bin's answer, and the Binaries checks below report it.",
    ),
];

/// Calls whose failure loses something written: a row, a file, a mode.
const WRITES: &[&str] = &[
    "execute(",
    "execute_batch(",
    "sqlx::query(",
    "sqlx::raw_sql(",
    "fs::write(",
    "fs::copy(",
    "fs::rename(",
    "fs::remove_file(",
    "fs::remove_dir_all(",
    "fs::create_dir_all(",
    "fs::create_dir(",
    "fs::hard_link(",
    "set_permissions(",
    "append_line(",
    "merge_into(",
    "write_all(",
    "sync_all(",
];

/// Directories never walked for shell scripts: build output, dependencies, other
/// checkouts.
const SKIP_DIRS: &[&str] = &[".git", "target", "node_modules", ".angular", "dist"];

/// One fallback found.
#[derive(Debug)]
struct Hit {
    /// Relative to the workspace root, `/`-separated on every OS.
    file: String,
    line: usize,
    shape: Shape,
    /// The statement the hit is part of, joined onto one line. Entries match against it.
    statement: String,
    /// The enclosing item, so the message says where to look.
    item: String,
}

impl std::fmt::Display for Hit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}:{} [{}] in {}: {}",
            self.file,
            self.line,
            self.shape.label(),
            self.item,
            self.statement
        )
    }
}

#[derive(Default)]
struct Scan {
    /// The production lines of each Rust file scanned.
    rust: BTreeMap<String, Vec<String>>,
    shell: BTreeSet<String>,
    /// Files skipped whole as `#[cfg(test)] mod name;` modules.
    test_files: BTreeSet<String>,
    hits: Vec<Hit>,
}

/// Walk up from this crate to the workspace root.
fn workspace_root() -> PathBuf {
    let mut dir: Option<&Path> = Some(Path::new(env!("CARGO_MANIFEST_DIR")));
    while let Some(d) = dir {
        if d.join("Cargo.toml").is_file() && d.join("crates").is_dir() {
            return d.to_path_buf();
        }
        dir = d.parent();
    }
    // Not an early return: a guard that passes because it found nothing to scan is the
    // bug it exists to catch.
    panic!("no workspace root above {}", env!("CARGO_MANIFEST_DIR"));
}

fn rel(root: &Path, p: &Path) -> String {
    p.strip_prefix(root)
        .unwrap_or(p)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Every regular file under `dir`, symlinks not followed, skipping directories `skip`
/// rejects. An unreadable directory panics rather than being passed over in silence.
fn walk(dir: &Path, skip: &dyn Fn(&Path) -> bool, out: &mut Vec<PathBuf>) {
    let entries =
        std::fs::read_dir(dir).unwrap_or_else(|e| panic!("cannot list {}: {e}", dir.display()));
    for entry in entries {
        let entry = entry.unwrap_or_else(|e| panic!("cannot list {}: {e}", dir.display()));
        let kind = entry
            .file_type()
            .unwrap_or_else(|e| panic!("cannot stat {}: {e}", entry.path().display()));
        let path = entry.path();
        if kind.is_dir() {
            if !skip(&path) {
                walk(&path, skip, out);
            }
        } else if kind.is_file() {
            out.push(path);
        }
    }
}

fn scan(root: &Path) -> Scan {
    let mut scan = Scan::default();

    // ── Rust: every crate's src, and the GUI's ────────────────────────────────────
    let mut rust_roots = vec![root.join("lumenator/src-tauri/src")];
    for entry in std::fs::read_dir(root.join("crates")).expect("crates/ is listable") {
        rust_roots.push(entry.expect("crates/ entry").path().join("src"));
    }
    let mut files = Vec::new();
    for r in rust_roots.iter().filter(|r| r.is_dir()) {
        walk(r, &|_: &Path| false, &mut files);
    }
    files.retain(|p| p.extension().is_some_and(|e| e == "rs"));
    files.sort();

    let sources: Vec<(PathBuf, String)> = files
        .into_iter()
        .map(|p| {
            let text = std::fs::read_to_string(&p)
                .unwrap_or_else(|e| panic!("cannot read {}: {e}", p.display()));
            (p, text)
        })
        .collect();

    // Modules declared `#[cfg(test)] mod name;` live in their own files, which are skipped
    // whole. Collected first: the declaring file may sort after the declared one.
    let mut test_files: BTreeSet<PathBuf> = BTreeSet::new();
    for (path, text) in &sources {
        let lines: Vec<&str> = text.lines().collect();
        for item in test_items(&rel(root, path), &lines) {
            if let TestItem::File(name) = item {
                let dir = match path.file_name().and_then(|n| n.to_str()) {
                    Some("lib.rs" | "main.rs" | "mod.rs") => path.parent().unwrap().to_path_buf(),
                    _ => path.with_extension(""),
                };
                test_files.insert(dir.join(format!("{name}.rs")));
                test_files.insert(dir.join(name));
            }
        }
    }

    for (path, text) in &sources {
        let file = rel(root, path);
        if test_files.iter().any(|t| path == t || path.starts_with(t)) {
            scan.test_files.insert(file);
            continue;
        }
        let lines: Vec<&str> = text.lines().collect();
        if lines.iter().any(|l| l.trim() == "#![cfg(test)]") {
            scan.test_files.insert(file);
            continue;
        }
        let prod = production_lines(&file, &lines);
        for (i, &(line, _)) in prod.iter().enumerate() {
            for shape in rust_shapes(&prod, i) {
                let statement = match shape {
                    EchoNumber | OrTrue => shell_statement(&prod, i),
                    _ => rust_statement(&prod, i, shape == DroppedWrite),
                };
                scan.hits.push(Hit {
                    file: file.clone(),
                    line,
                    shape,
                    statement,
                    item: rust_item(&prod, i),
                });
            }
        }
        scan.rust
            .insert(file, prod.iter().map(|(_, l)| l.to_string()).collect());
    }

    // ── Shell: every script in the repository ─────────────────────────────────────
    let skip = |p: &Path| {
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        SKIP_DIRS.contains(&name)
            || p.ends_with(".claude/worktrees")
            // Frozen copies of what earlier releases installed. The upgrade tests run them,
            // so they have to keep the bugs those releases shipped.
            || (name == "fixtures" && p.parent().is_some_and(|d| d.ends_with("tests")))
    };
    let mut scripts = Vec::new();
    walk(root, &skip, &mut scripts);
    scripts.retain(|p| p.extension().is_some_and(|e| e == "sh"));
    scripts.sort();
    for path in scripts {
        let file = rel(root, &path);
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
        let lines: Vec<(usize, &str)> = text.lines().enumerate().map(|(i, l)| (i + 1, l)).collect();
        for (i, &(line, text)) in lines.iter().enumerate() {
            if text.trim_start().starts_with('#') {
                continue;
            }
            for shape in shell_shapes(text) {
                scan.hits.push(Hit {
                    file: file.clone(),
                    line,
                    shape,
                    statement: shell_statement(&lines, i),
                    item: shell_item(&lines, i),
                });
            }
        }
        scan.shell.insert(file);
    }
    scan
}

enum TestItem<'a> {
    /// `#[cfg(test)] mod name;`
    File(&'a str),
    /// `#[cfg(test)] mod name { … }`, as line indices, inclusive.
    Block(usize, usize),
}

/// Every `#[cfg(test)]` item in a file. Panics on any shape but a module: the scan would
/// not know where it ends, and guessing either hides production code or scans tests.
fn test_items<'a>(file: &str, lines: &[&'a str]) -> Vec<TestItem<'a>> {
    let mut items = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if lines[i].trim() != "#[cfg(test)]" {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        while j < lines.len() && (lines[j].trim().is_empty() || lines[j].trim().starts_with("#[")) {
            j += 1;
        }
        let decl = lines.get(j).map_or("", |l| l.trim());
        let decl = decl
            .strip_prefix("pub(crate) ")
            .or_else(|| decl.strip_prefix("pub "))
            .unwrap_or(decl);
        let Some(module) = decl.strip_prefix("mod ") else {
            panic!(
                "{file}:{}: #[cfg(test)] on something other than a module: {decl:?}",
                j + 1
            );
        };
        if let Some(name) = module.strip_suffix(';') {
            items.push(TestItem::File(name.trim()));
            i = j + 1;
        } else if module.ends_with('{') {
            let Some(end) = closing_brace(lines, j) else {
                panic!("{file}:{}: this test module's braces never balance", j + 1);
            };
            // A check on the lexer: rustfmt puts the closing brace at the opening line's
            // indentation, so anything else means the count went wrong.
            let indent = &lines[j][..lines[j].len() - lines[j].trim_start().len()];
            assert_eq!(
                lines[end].trim_end(),
                format!("{indent}}}"),
                "{file}:{}: the brace closing the test module opened on line {} is not at its indentation",
                end + 1,
                j + 1
            );
            items.push(TestItem::Block(i, end));
            i = end + 1;
        } else {
            panic!(
                "{file}:{}: unrecognised test module declaration: {decl:?}",
                j + 1
            );
        }
    }
    items
}

enum Lex {
    Code,
    Str,
    /// A raw string, with its number of `#`s.
    Raw(usize),
    /// A block comment, with its nesting depth.
    Comment(usize),
}

/// The line holding the brace that closes the one opened on line `open`, counting only
/// braces outside comments, strings and character literals. The first `}` at the right
/// indentation is not enough: test modules hold source fixtures with their own.
fn closing_brace(lines: &[&str], open: usize) -> Option<usize> {
    let mut lex = Lex::Code;
    let mut depth = 0usize;
    for (n, line) in lines.iter().enumerate().skip(open) {
        let c: Vec<char> = line.chars().collect();
        let at = |i: usize| c.get(i).copied();
        let mut i = 0;
        while i < c.len() {
            match lex {
                Lex::Code => match c[i] {
                    '/' if at(i + 1) == Some('/') => break,
                    '/' if at(i + 1) == Some('*') => {
                        lex = Lex::Comment(1);
                        i += 1;
                    }
                    '"' => lex = Lex::Str,
                    'r' if i == 0 || !(c[i - 1].is_alphanumeric() || c[i - 1] == '_') || {
                        // `br"…"`, `cr"…"`
                        matches!(c[i - 1], 'b' | 'c')
                            && (i < 2 || !(c[i - 2].is_alphanumeric() || c[i - 2] == '_'))
                    } =>
                    {
                        let hashes = c[i + 1..].iter().take_while(|&&h| h == '#').count();
                        if at(i + 1 + hashes) == Some('"') {
                            lex = Lex::Raw(hashes);
                            i += hashes + 1;
                        }
                    }
                    '\'' if at(i + 1) == Some('\\') => {
                        // '\n', '\'', '\u{7f}': past the escaped character to the close.
                        i += 3;
                        while i < c.len() && c[i] != '\'' {
                            i += 1;
                        }
                    }
                    // 'x', but not the lifetime in &'a str.
                    '\'' if at(i + 2) == Some('\'') => i += 2,
                    '{' => depth += 1,
                    '}' => {
                        depth = depth.checked_sub(1)?;
                        if depth == 0 {
                            return Some(n);
                        }
                    }
                    _ => {}
                },
                Lex::Str => match c[i] {
                    '\\' => i += 1,
                    '"' => lex = Lex::Code,
                    _ => {}
                },
                Lex::Raw(hashes) => {
                    if c[i] == '"' && (1..=hashes).all(|k| at(i + k) == Some('#')) {
                        lex = Lex::Code;
                        i += hashes;
                    }
                }
                Lex::Comment(d) => {
                    if c[i] == '*' && at(i + 1) == Some('/') {
                        lex = if d == 1 {
                            Lex::Code
                        } else {
                            Lex::Comment(d - 1)
                        };
                        i += 1;
                    } else if c[i] == '/' && at(i + 1) == Some('*') {
                        lex = Lex::Comment(d + 1);
                        i += 1;
                    }
                }
            }
            i += 1;
        }
    }
    None
}

/// The lines compiled outside tests, numbered from 1.
fn production_lines<'a>(file: &str, lines: &[&'a str]) -> Vec<(usize, &'a str)> {
    let mut skip = vec![false; lines.len()];
    for item in test_items(file, lines) {
        if let TestItem::Block(from, to) = item {
            skip[from..=to].iter_mut().for_each(|s| *s = true);
        }
    }
    lines
        .iter()
        .enumerate()
        .filter(|(i, _)| !skip[*i])
        .map(|(i, l)| (i + 1, *l))
        .collect()
}

/// The shapes on production Rust line `i`. Shell shapes too: the hook templates are
/// string literals here.
fn rust_shapes(lines: &[(usize, &str)], i: usize) -> Vec<Shape> {
    let t = lines[i].1.trim();
    if t.starts_with("//") {
        return Vec::new();
    }
    let mut found = if t.starts_with('#') {
        Vec::new()
    } else {
        shell_shapes(t)
    };
    if number_for_error(t) {
        found.push(NumberForError);
    }
    if t.contains(".unwrap_or_default()") {
        found.push(OrDefault);
    }
    let dropped = if t.contains("let _ =") {
        Some(rust_statement(lines, i, true))
    } else if t.ends_with(".ok();") {
        let statement = rust_statement(lines, i, false);
        // `let x = f().ok();` keeps the value; only a bare statement discards it.
        let head = statement.as_str();
        let discards =
            !head.starts_with("let ") && !head.starts_with("return ") && !head.contains(" = ");
        discards.then_some(statement)
    } else {
        None
    };
    if dropped.is_some_and(|s| WRITES.iter().any(|w| s.contains(w))) {
        found.push(DroppedWrite);
    }
    found
}

/// The shell shapes on one line.
fn shell_shapes(line: &str) -> Vec<Shape> {
    let mut found = Vec::new();
    let mut rest = line;
    while let Some(at) = rest.find("||") {
        rest = &rest[at + 2..];
        let after = rest.trim_start();
        if echoes_a_number(after) {
            found.push(EchoNumber);
        }
        let status = after
            .strip_prefix("true")
            .or_else(|| after.strip_prefix(':'));
        if status.is_some_and(|r| {
            r.chars()
                .next()
                .is_none_or(|c| c.is_whitespace() || ";)}&|`\"'".contains(c))
        }) {
            found.push(OrTrue);
        }
    }
    found.sort();
    found.dedup();
    found
}

/// `echo …` or `printf …` with a number among its words.
fn echoes_a_number(command: &str) -> bool {
    let Some(args) = ["echo", "printf"]
        .iter()
        .find_map(|c| command.strip_prefix(c))
    else {
        return false;
    };
    args.starts_with(char::is_whitespace) && shell_words(args).iter().any(|w| is_number(w))
}

/// The words of one command, quotes honoured and removed, up to whatever ends it.
fn shell_words(s: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quote: Option<char> = None;
    for c in s.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => word.push(c),
            None if c == '"' || c == '\'' => quote = Some(c),
            None if c.is_whitespace() => {
                if !word.is_empty() {
                    words.push(std::mem::take(&mut word));
                }
            }
            None if ";|&)`<>#".contains(c) => break,
            None => word.push(c),
        }
    }
    if !word.is_empty() {
        words.push(word);
    }
    words
}

fn is_number(w: &str) -> bool {
    let w = w.strip_prefix('-').unwrap_or(w);
    let (int, frac) = w.split_once('.').unwrap_or((w, ""));
    !int.is_empty()
        && int.bytes().all(|b| b.is_ascii_digit())
        && frac.bytes().all(|b| b.is_ascii_digit())
}

/// `.unwrap_or(<n>)`, `.map_or(<n>, …)`, `.unwrap_or_else(|…| <n>)`,
/// `.map_or_else(|…| <n>, …)`.
fn number_for_error(line: &str) -> bool {
    for (call, closure, end) in [
        (".unwrap_or(", false, ')'),
        (".map_or(", false, ','),
        (".unwrap_or_else(", true, ')'),
        (".map_or_else(", true, ','),
    ] {
        let mut rest = line;
        while let Some(at) = rest.find(call) {
            rest = &rest[at + call.len()..];
            let mut arg = rest.trim_start();
            if closure {
                let Some(params) = arg.strip_prefix('|') else {
                    continue;
                };
                let Some(close) = params.find('|') else {
                    continue;
                };
                arg = params[close + 1..].trim_start();
            }
            let n = rust_number_len(arg);
            if n > 0 && arg[n..].trim_start().starts_with(end) {
                return true;
            }
        }
    }
    false
}

/// The length of the numeric literal `s` starts with (`0`, `-1`, `0.0`, `1_000u64`), or 0.
fn rust_number_len(s: &str) -> usize {
    let b = s.as_bytes();
    let digit = |i: usize| b.get(i).is_some_and(u8::is_ascii_digit);
    let mut i = usize::from(b.first() == Some(&b'-'));
    if !digit(i) {
        return 0;
    }
    while digit(i) || b.get(i) == Some(&b'_') {
        i += 1;
    }
    if b.get(i) == Some(&b'.') && digit(i + 1) {
        i += 1;
        while digit(i) || b.get(i) == Some(&b'_') {
            i += 1;
        }
    }
    while b
        .get(i)
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
    {
        i += 1;
    }
    i
}

/// The statement around line `i`, on one line: back to its head, over a method chain
/// (lines starting `.` or `?`) and out of any bracket those lines close without opening,
/// and, with `forward`, on to the `;` that ends it.
fn rust_statement(lines: &[(usize, &str)], i: usize, forward: bool) -> String {
    let unopened = |from: usize| {
        lines[from..=i]
            .iter()
            .map(|(_, l)| bracket_balance(l))
            .sum::<i64>()
            < 0
    };
    let mut from = i;
    while from > 0
        && i - from < 20
        && (lines[from].1.trim_start().starts_with(['.', '?']) || unopened(from))
    {
        from -= 1;
    }
    let mut to = i;
    while forward && to + 1 < lines.len() && to - i < 20 && !lines[to].1.trim_end().ends_with(';') {
        to += 1;
    }
    join(&lines[from..=to])
}

/// Brackets opened minus brackets closed on one line of Rust, outside strings, character
/// literals and a `//` comment.
fn bracket_balance(line: &str) -> i64 {
    let c: Vec<char> = line.chars().collect();
    let mut balance = 0;
    let mut i = 0;
    while i < c.len() {
        match c[i] {
            '/' if c.get(i + 1) == Some(&'/') => break,
            '"' => {
                i += 1;
                while i < c.len() && c[i] != '"' {
                    i += 1 + usize::from(c[i] == '\\');
                }
            }
            // '\n', '\'', '\u{7f}': past the escaped character to the close.
            '\'' if c.get(i + 1) == Some(&'\\') => {
                i += 3;
                while i < c.len() && c[i] != '\'' {
                    i += 1;
                }
            }
            // '(', but not the lifetime in &'a str.
            '\'' if c.get(i + 2) == Some(&'\'') => i += 2,
            '(' | '[' | '{' => balance += 1,
            ')' | ']' | '}' => balance -= 1,
            _ => {}
        }
        i += 1;
    }
    balance
}

/// The command around line `i`, on one line, back over `\` continuations.
fn shell_statement(lines: &[(usize, &str)], i: usize) -> String {
    let mut from = i;
    while from > 0 && lines[from - 1].1.trim_end().ends_with('\\') {
        from -= 1;
    }
    join(&lines[from..=i])
}

fn join(lines: &[(usize, &str)]) -> String {
    lines
        .iter()
        .map(|(_, l)| l.trim())
        .collect::<Vec<_>>()
        .join(" ")
}

/// The nearest item above line `i`: `fn name`, `const NAME`, `impl Type`…
fn rust_item(lines: &[(usize, &str)], i: usize) -> String {
    for (_, l) in lines[..=i].iter().rev() {
        let mut t = l.trim_start();
        for prefix in ["pub(crate) ", "pub(super) ", "pub ", "async ", "unsafe "] {
            t = t.strip_prefix(prefix).unwrap_or(t);
        }
        let kind = [
            "fn ",
            "const ",
            "static ",
            "impl ",
            "impl<",
            "struct ",
            "enum ",
            "trait ",
            "mod ",
            "macro_rules! ",
        ]
        .into_iter()
        .find(|k| t.starts_with(k));
        let Some(kind) = kind else { continue };
        let ends: &[char] = if kind == "const " || kind == "static " {
            &[':', '=']
        } else {
            &['(', '{', ';', '=']
        };
        let head = t.split(ends).next().unwrap_or(t).trim();
        // `const fn x(`, but `const X: T` — a `:` inside `const fn` generics is not the end.
        let head = if t.starts_with("const fn ") {
            t.split('(').next().unwrap_or(t).trim()
        } else {
            head
        };
        return head.to_string();
    }
    "(top level)".to_string()
}

/// The shell function line `i` is in, or `(top level)`.
///
/// Functions in these scripts close with a `}` in column 0, so meeting one on the way up
/// means the line is outside every function.
fn shell_item(lines: &[(usize, &str)], i: usize) -> String {
    for (k, (_, l)) in lines[..=i].iter().enumerate().rev() {
        if k < i && l.trim_end() == "}" {
            break;
        }
        let t = l.trim();
        let t = t.strip_prefix("function ").unwrap_or(t);
        if let Some((name, _)) = t.split_once("()") {
            let name = name.trim();
            if !name.is_empty()
                && name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
            {
                return format!("{name}()");
            }
        }
    }
    "(top level)".to_string()
}

/// What the scan must have covered for its silence to mean anything.
fn premises(root: &Path, scan: &Scan) {
    let setup = scan
        .rust
        .get("lumenator/src-tauri/src/setup.rs")
        .expect("setup.rs, which holds the hook templates, was not scanned");
    assert!(
        setup
            .iter()
            .any(|l| l.contains("hook meter --writer __LUMEN_WRITER__")),
        "the meter template is not among setup.rs's production lines"
    );
    // A test module ended too early leaves its tests behind as "production" lines.
    for (file, lines) in &scan.rust {
        assert!(
            !lines.iter().any(|l| l.trim_start().starts_with("#[test]")
                || l.trim_start().starts_with("#[tokio::test")),
            "{file}: a test was scanned as production code — a test module was not bounded"
        );
    }
    for f in [
        "crates/lumen-core/src/meter.rs",
        "crates/lumen-core/src/faults.rs",
        "crates/lumen-core/src/report.rs",
        "crates/lumen-daemon/src/main.rs",
        "crates/lumen-mcp/src/hook.rs",
        "crates/lumen-stats/src/lib.rs",
        "crates/lumen-cli/src/main.rs",
        "lumenator/src-tauri/src/lib.rs",
    ] {
        assert!(scan.rust.contains_key(f), "{f} was not scanned");
    }
    for f in [
        ".claude/hooks/lumen_meter.sh",
        ".claude/hooks/lumen_read_intercept.sh",
        ".claude/lumen_report.sh",
        "scripts/release.sh",
        "scripts/verify-install.sh",
    ] {
        assert!(scan.shell.contains(f), "{f} was not scanned");
    }
    // Skipped because they are test modules, and present — so the skip is doing something.
    for f in [
        "lumenator/src-tauri/src/setup/hook_e2e.rs",
        "lumenator/src-tauri/src/setup/plugin_hooks.rs",
    ] {
        assert!(root.join(f).is_file(), "{f} is gone; update this premise");
        assert!(
            scan.test_files.contains(f),
            "{f} is test-only but was scanned"
        );
    }
}

#[test]
fn every_silent_fallback_is_accounted_for() {
    let root = workspace_root();
    let scan = scan(&root);
    premises(&root, &scan);

    let mut used = vec![0usize; ALLOWED.len()];
    let mut unaccounted = Vec::new();
    let mut ambiguous = Vec::new();
    for hit in &scan.hits {
        let by: Vec<usize> = (0..ALLOWED.len())
            .filter(|&a| ALLOWED[a].covers(hit))
            .collect();
        match by.as_slice() {
            [] => unaccounted.push(hit),
            [a] => used[*a] += 1,
            _ => ambiguous.push((hit, by)),
        }
    }

    let mut per_shape: BTreeMap<&str, usize> = BTreeMap::new();
    for hit in &scan.hits {
        *per_shape.entry(hit.shape.label()).or_default() += 1;
    }
    eprintln!(
        "scanned {} Rust files ({} production lines, {} test-only files skipped) and {} shell scripts; \
         {} fallbacks found {per_shape:?}; {} allowed entries",
        scan.rust.len(),
        scan.rust.values().map(Vec::len).sum::<usize>(),
        scan.test_files.len(),
        scan.shell.len(),
        scan.hits.len(),
        ALLOWED.len(),
    );

    let mut problems = String::new();
    if !unaccounted.is_empty() {
        problems.push_str(
            "\nNot accounted for — each replaces a failure with a value that looks real:\n",
        );
        for hit in &unaccounted {
            problems.push_str(&format!("  {hit}\n"));
        }
    }
    for (hit, by) in &ambiguous {
        problems.push_str(&format!(
            "\nCovered by {} entries ({by:?}), so by none clearly:\n  {hit}\n",
            by.len()
        ));
    }
    for (a, n) in ALLOWED.iter().zip(&used).filter(|(a, n)| a.n != **n) {
        problems.push_str(&format!(
            "\nALLOWED entry for {} [{}] {:?} covers {n} hit(s), not {}: {}\n",
            a.file,
            a.shape.label(),
            a.needle,
            a.n,
            if *n == 0 {
                "it is stale — remove it"
            } else {
                "it now excuses code nobody looked at"
            },
        ));
    }
    assert!(
        problems.is_empty(),
        "{problems}\nReport the failure instead — a fault (lumen_core::faults), a stderr line, a returned \
         error — or, if the fallback is right, add it to ALLOWED in {} saying why it is not a measurement.",
        file!()
    );
}

#[test]
fn every_allowed_entry_states_a_reason() {
    for a in ALLOWED {
        assert!(
            !a.needle.trim().is_empty(),
            "{} [{}]: an empty needle covers the whole file",
            a.file,
            a.shape.label()
        );
        assert!(
            !a.why.trim().is_empty(),
            "{} [{}] {:?}: no reason given",
            a.file,
            a.shape.label(),
            a.needle
        );
        assert!(
            a.n > 0,
            "{} [{}] {:?}: covers nothing by design",
            a.file,
            a.shape.label(),
            a.needle
        );
    }
}

/// The matchers, against the shapes they name and against lookalikes that are not
/// fallbacks. Without the second half a matcher that fires on everything passes.
#[test]
fn the_shapes_catch_what_they_name_and_nothing_that_merely_looks_alike() {
    for (line, shape) in [
        (r#"lines=$(wc -l < "$1" 2>/dev/null || echo 0)"#, EchoNumber),
        ("n=$(cat f || printf '%s' 0)", EchoNumber),
        (r#"x=$(cmd || echo "-1")"#, EchoNumber),
        ("rm -f \"$tmp\" || true", OrTrue),
        ("v=$(grep -c x f || true)", OrTrue),
        ("mkdir -p d || :", OrTrue),
    ] {
        assert_eq!(shell_shapes(line), [shape], "{line}");
    }
    for line in [
        r#"printf '%s\n' "$line" >>"$LUMEN_FAULT_SPOOL" || echo "lumen: nor could the fault be written" >&2"#,
        r#"[ -x "$bin" ] || bin="$(command -v lumen-mcp 2>/dev/null)""#,
        r#"cmd || echo "attempt 2 failed" >&2"#,
        "cmd || echo none",
        "cmd || truer",
        "a || b",
    ] {
        assert_eq!(shell_shapes(line), [], "{line}");
    }

    let rust = |src: &str| -> Vec<Shape> {
        let lines: Vec<(usize, &str)> = src.lines().enumerate().map(|(i, l)| (i + 1, l)).collect();
        let mut found: Vec<Shape> = (0..lines.len())
            .flat_map(|i| rust_shapes(&lines, i))
            .collect();
        found.sort();
        found.dedup();
        found
    };
    for (src, shape) in [
        ("let n = x.unwrap_or(0);", NumberForError),
        ("let f = x.unwrap_or(0.0);", NumberForError),
        ("let n = x.unwrap_or(-1);", NumberForError),
        ("let n = x.unwrap_or(0u64);", NumberForError),
        ("let n = x.unwrap_or(0_i64);", NumberForError),
        ("let n = x.unwrap_or_else(|_| 0);", NumberForError),
        ("let n = x.unwrap_or_else(|| 0);", NumberForError),
        ("let n = x.map_or(0, |v| v.len());", NumberForError),
        ("let n = x.map_or_else(|_| 1, f);", NumberForError),
        (
            "let n = q\n    .first()\n    .unwrap_or(0);",
            NumberForError,
        ),
        ("let n: u64 = x.unwrap_or_default();", OrDefault),
        ("let _ = std::fs::write(&p, s);", DroppedWrite),
        (
            "let _ = sqlx::query(\n    \"INSERT …\",\n)\n.execute(pool)\n.await;",
            DroppedWrite,
        ),
        ("let _ = conn\n    .execute_batch(sql);", DroppedWrite),
        ("std::fs::create_dir_all(&d).ok();", DroppedWrite),
        ("std::fs::remove_file(&p)\n    .ok();", DroppedWrite),
        ("let s = r#\"\nx=$(cmd || echo 0)\n\"#;", EchoNumber),
    ] {
        assert_eq!(rust(src), [shape], "{src}");
    }
    for src in [
        "let n = x.unwrap_or(limit);",
        "let n = x.unwrap_or(f(0));",
        "let n = x.unwrap_or_else(|e| e.code());",
        "let b = x.unwrap_or(false);",
        "// let n = x.unwrap_or(0);",
        "/// `2>/dev/null || true` was the bug",
        "let _ = window.hide();",
        "let _ = tx.send(msg);",
        "let x = std::fs::read(&p).ok();",
        "let ok = std::fs::write(&p, s).is_ok();",
        "return std::fs::remove_file(&p).ok();",
        "std::io::stdout().flush().ok();",
        "# x=$(cmd || echo 0)",
    ] {
        assert_eq!(rust(src), [], "{src}");
    }
}

/// Test modules are skipped to their closing brace and no further, and anything else
/// under `#[cfg(test)]` stops the scan rather than being guessed at.
#[test]
fn only_test_modules_are_skipped() {
    let src = "fn a() { x.unwrap_or(0); }\n#[cfg(test)]\nmod tests {\n    fn t() { x.unwrap_or(0); }\n}\nfn b() { y.unwrap_or(0); }\n";
    let lines: Vec<&str> = src.lines().collect();
    let numbers: Vec<usize> = production_lines("x.rs", &lines)
        .iter()
        .map(|(n, _)| *n)
        .collect();
    assert_eq!(numbers, [1, 6]);

    // The shape that fooled the first version of this scan: a fixture inside a test with
    // its own `}` at column 0, beside braces in strings, characters and comments.
    let src = "#[cfg(test)]\nmod tests {\n    const SRC: &str = r#\"\nfn f() {\n}\n\"#;\n    fn t<'a>(s: &'a str) -> char { let _ = \"}\"; '}' } // }\n    /* } */\n}\nfn b() { y.unwrap_or(0); }\n";
    let lines: Vec<&str> = src.lines().collect();
    let numbers: Vec<usize> = production_lines("x.rs", &lines)
        .iter()
        .map(|(n, _)| *n)
        .collect();
    assert_eq!(numbers, [10]);

    let src = "#[cfg(test)]\n#[allow(dead_code)]\nmod hook_e2e;\nfn a() {}\n";
    let lines: Vec<&str> = src.lines().collect();
    assert!(matches!(
        test_items("x.rs", &lines).as_slice(),
        [TestItem::File("hook_e2e")]
    ));

    for src in [
        "#[cfg(test)]\nfn helper() {}\n",
        "#[cfg(test)]\nmod tests {\n  fn t() {}\n",
    ] {
        let lines: Vec<&str> = src.lines().collect();
        let result = std::panic::catch_unwind(|| test_items("x.rs", &lines).len());
        assert!(result.is_err(), "should have refused: {src:?}");
    }
}

#[test]
fn a_shell_hit_names_the_function_it_is_in() {
    let src = "f() {\n    a || true\n}\nb || true\ng() { c || true; }\n";
    let lines: Vec<(usize, &str)> = src.lines().enumerate().map(|(i, l)| (i + 1, l)).collect();
    assert_eq!(shell_item(&lines, 1), "f()");
    assert_eq!(shell_item(&lines, 3), "(top level)");
    assert_eq!(shell_item(&lines, 4), "g()");
}

#[test]
fn a_rust_hit_is_read_as_the_whole_statement() {
    let src = r#"let html = from_str(&resp)
    .ok()
    .and_then(|v| {
        v.get("html_url")
            .map(str::to_string)
    })
    .unwrap_or_default();
std::fs::write(
    path,
    ")",
)
.ok();
let r = std::fs::write(
    path,
    '(',
)
.ok();
"#;
    let lines: Vec<(usize, &str)> = src.lines().enumerate().map(|(i, l)| (i + 1, l)).collect();
    assert_eq!(
        rust_statement(&lines, 6, false),
        r#"let html = from_str(&resp) .ok() .and_then(|v| { v.get("html_url") .map(str::to_string) }) .unwrap_or_default();"#
    );
    // A write split over lines is still a write, and keeping its result still keeps it.
    assert_eq!(rust_shapes(&lines, 11), vec![DroppedWrite]);
    assert_eq!(rust_shapes(&lines, 16), vec![]);
}
