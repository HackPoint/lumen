//! `lumen report` — render a fault report, and file it as a GitHub issue.
//!
//! Three sources feed one renderer: rows drained from the JSONL fault spool into
//! `faults`, ranked declines already metered in `read_events`, and a live schema check.
//! Every one of them degrades rather than aborting — a stale or damaged database is
//! exactly what this report exists to describe, so a reporter that dies on one fails
//! precisely when it is needed.
//!
//! `--faults <file>` renders a fixture instead, and is deliberately permanent: it is the
//! only way to snapshot-test the renderer without standing up a database.
//!
//! Redaction is the load-bearing property here, not the formatting. The repository is
//! public and Lumen's whole job is reading the user's source files, so the renderer
//! emits metadata only — extension, line count, content hash — and never a file's
//! contents, an absolute path, a filename from outside the workspace, or the value of a
//! path-valued environment override. `--include-source` opts into embedding
//! in-workspace file bodies, and prints a manifest of what it will embed first.
//!
//! Filing is deduplicated on a fingerprint of `(kind, variant, version)`, carried in the
//! body as an HTML comment: a second run comments on the existing issue instead of
//! opening a duplicate.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

use serde::Deserialize;
use sha2::{Digest, Sha256};

/// Longest free-form detail rendered before truncation, in lines. A decline detail
/// is a couple of lines; a schema diff can be dozens, and would bury the table.
const DETAIL_LINE_CAP: usize = 12;

/// Stand-in for a kind that has no sub-kind (`schema_drift` has no route or guard).
/// Part of the fingerprint, so the sentinel is a stable string, not a formatting choice.
const NO_VARIANT: &str = "-";

/// One recorded fault, already aggregated over its occurrences.
#[derive(Debug, Clone, Deserialize)]
pub struct Fault {
    pub kind: String,
    /// `routed_via` for a ranked decline — `ranked_too_slow`, `ranked_no_defs`, …
    #[serde(default)]
    pub route: Option<String>,
    /// Which fail-open guard in the Read intercept released the call.
    #[serde(default)]
    pub guard: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub lines: Option<i64>,
    #[serde(default)]
    pub detail: Option<String>,
    #[serde(default = "default_count")]
    pub count: u64,
    #[serde(default)]
    pub first_seen: Option<String>,
    #[serde(default)]
    pub last_seen: Option<String>,
}

fn default_count() -> u64 {
    1
}

/// Hand-written rather than derived so `count` agrees with the serde default. A derived
/// `Default` would give 0, and a `..Default::default()` construction would then silently
/// contribute nothing to any total.
impl Default for Fault {
    fn default() -> Self {
        Self {
            kind: String::new(),
            route: None,
            guard: None,
            path: None,
            lines: None,
            detail: None,
            count: default_count(),
            first_seen: None,
            last_seen: None,
        }
    }
}

impl Fault {
    /// The sub-kind that distinguishes two faults sharing a `kind`. Part of the
    /// fingerprint, so it must not vary per user or per run.
    fn variant(&self) -> &str {
        self.route
            .as_deref()
            .or(self.guard.as_deref())
            .unwrap_or(NO_VARIANT)
    }

    /// Set an already-resolved sub-kind, as stored in `faults.variant`.
    fn with_variant(mut self, variant: String) -> Self {
        // Into `guard`: `variant()` reads route first, and a stored variant must not
        // masquerade as a decline route when the kind is not a decline.
        self.guard = Some(variant);
        self
    }
}

/// Host facts a maintainer needs, and that the reporter can actually know.
///
/// Built by [`Environment::collect`] in real use and constructed literally in tests,
/// so a snapshot does not move when the host does.
#[derive(Debug, Clone, Default)]
pub struct Environment {
    pub lumen_version: String,
    pub git_sha: Option<String>,
    pub os: String,
    pub arch: String,
    pub channel: String,
    pub mcp_scope: String,
    pub mcp_json_servers: Option<usize>,
    pub hooks_digest: Option<String>,
    pub read_events_cols: Option<usize>,
    pub env_overrides: Vec<(String, String)>,
    /// The menu-bar/tray state, when the caller knows it. `collect()` cannot: the tray belongs
    /// to the GUI process, so the GUI fills this in after collecting. A report that says the
    /// icon was never visible answers the first question a maintainer would otherwise ask.
    pub tray: Option<String>,
    /// Startup steps that failed without aborting. Same source as the in-app degraded banner.
    pub startup_degradations: Vec<String>,
    /// Workspace root, used to relativise paths. Never rendered.
    pub workspace_root: Option<PathBuf>,
    /// Home directory, scrubbed from all rendered text. Never rendered.
    pub home: Option<PathBuf>,
}

impl Environment {
    /// Collect for the CLI. See [`Environment::collect_for`] for why the channel is a parameter.
    pub fn collect() -> Self {
        Self::collect_for("cli")
    }

    /// Collect, naming the channel the report is being filed from.
    ///
    /// This used to hardcode `"cli"`, so every report filed from the GUI's **File issue** button
    /// claimed to come from the CLI. MESSAGING_CONTRACT.md makes channel honesty an explicit
    /// rule — CLI is Full mode, VS Code is Soft mode — and the report screen is the one place a
    /// maintainer takes it at face value.
    pub fn collect_for(channel: &str) -> Self {
        let workspace_root = workspace_root();
        let home = dirs::home_dir();

        let mut env_overrides: Vec<(String, String)> = std::env::vars()
            .filter(|(k, _)| k.starts_with("LUMEN_"))
            .collect();
        env_overrides.sort();

        Self {
            lumen_version: env!("CARGO_PKG_VERSION").to_string(),
            git_sha: git_sha(workspace_root.as_deref()),
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
            channel: channel.to_string(),
            mcp_scope: mcp_scope(workspace_root.as_deref()),
            mcp_json_servers: mcp_json_servers(workspace_root.as_deref()),
            hooks_digest: hooks_digest(workspace_root.as_deref()),
            read_events_cols: read_events_cols(),
            env_overrides,
            tray: None,
            startup_degradations: Vec::new(),
            workspace_root,
            home,
        }
    }
}

/// Walk up from the executable, then from the cwd, looking for the workspace marker.
fn workspace_root() -> Option<PathBuf> {
    let starts = [
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(Path::to_path_buf)),
        std::env::current_dir().ok(),
    ];
    for start in starts.into_iter().flatten() {
        let mut cur: Option<&Path> = Some(&start);
        while let Some(dir) = cur {
            if dir.join("Cargo.toml").is_file() && dir.join("crates").is_dir() {
                return Some(dir.to_path_buf());
            }
            cur = dir.parent();
        }
    }
    None
}

fn git_sha(root: Option<&Path>) -> Option<String> {
    let root = root?;
    let out = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .current_dir(root)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let sha = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!sha.is_empty()).then_some(sha)
}

/// How many servers the tracked `.mcp.json` declares. Zero is the interesting
/// answer: it means the repo ships the routing demand without the routing target.
fn mcp_json_servers(root: Option<&Path>) -> Option<usize> {
    let text = std::fs::read_to_string(root?.join(".mcp.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    Some(v.get("mcpServers")?.as_object()?.len())
}

fn mcp_scope(root: Option<&Path>) -> String {
    match mcp_json_servers(root) {
        Some(n) if n > 0 => "project (.mcp.json)".to_string(),
        _ if dirs::home_dir().is_some_and(|h| h.join(".claude.json").is_file()) => {
            "user (~/.claude.json)".to_string()
        }
        _ => "unknown".to_string(),
    }
}

/// Digest of the hook scripts, so a report says whether the intercept in play is the
/// shipped one. Sorted by name for determinism.
fn hooks_digest(root: Option<&Path>) -> Option<String> {
    let dir = root?.join(".claude/hooks");
    let mut names: Vec<PathBuf> = std::fs::read_dir(&dir)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "sh"))
        .collect();
    if names.is_empty() {
        return None;
    }
    names.sort();

    let mut hasher = Sha256::new();
    for p in names {
        hasher.update(p.file_name()?.as_encoded_bytes());
        hasher.update(std::fs::read(&p).ok()?);
    }
    Some(hex(&hasher.finalize()))
}

/// Column count of `read_events`, to catch a database that missed a migration.
fn read_events_cols() -> Option<usize> {
    let db = crate::meter::db_path()?;
    let conn = rusqlite::Connection::open_with_flags(
        db,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )
    .ok()?;
    let mut stmt = conn.prepare("PRAGMA table_info(read_events)").ok()?;
    let n = stmt.query_map([], |_| Ok(())).ok()?.count();
    (n > 0).then_some(n)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut s, b| {
        use std::fmt::Write;
        let _ = write!(s, "{b:02x}");
        s
    })
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

/// Faults of one `(kind, variant)`, merged across files.
struct Group {
    kind: String,
    variant: String,
    files: BTreeSet<String>,
    count: u64,
    first_seen: Option<String>,
    last_seen: Option<String>,
    details: Vec<String>,
}

/// Every fault kind this build can emit.
///
/// The renderer has to know all of them: an unknown kind renders as "Unclassified
/// fault" and sorts last, which is what the first real report filed from the app did
/// because the daemon's kinds were added without teaching the renderer about them.
/// `every_emitted_kind_is_classified` fails if that happens again.
pub const FAULT_KINDS: &[&str] = &[
    "hook_fail_open",
    "schema_drift",
    "ingest_failed",
    "reporter_degraded",
    "ws_restart",
    "ranked_decline",
];

/// The impact line used for a kind the renderer does not recognise.
const UNCLASSIFIED: &str = "Unclassified fault. See the table below.";

/// Sort order for the table and for picking the headline. A fired fail-open guard
/// outranks everything: it means the routing contract broke in the field.
fn kind_priority(kind: &str) -> u8 {
    match kind {
        "hook_fail_open" => 0,
        "schema_drift" => 1,
        // Silent data loss: the daemon logged and continued, so the gauge is wrong and
        // nothing said so. Ranks above the two that merely retry.
        "ingest_failed" => 2,
        // The report itself is incomplete. Not the fault being reported, but it changes
        // how much the rest of the report can be trusted.
        "reporter_degraded" => 3,
        "ws_restart" => 4,
        "ranked_decline" => 5,
        _ => 6,
    }
}

fn impact(kind: &str) -> &'static str {
    match kind {
        "hook_fail_open" => {
            "The Read intercept redirected to lumen, lumen did not serve the call, and a \
             fail-open guard released the Read. Routing is degraded, not broken — context \
             was spent that lumen was supposed to save."
        }
        "schema_drift" => {
            "The database's `read_events` columns do not match the set this build expects. \
             Metering rows may be dropped or written to the wrong column."
        }
        "ingest_failed" => {
            "The daemon could not ingest a transcript and carried on. Those turns are \
             missing from the ledger, so the gauge and the cost figures are low by \
             whatever they contained — and nothing else reports it."
        }
        "ws_restart" => {
            "The daemon's WebSocket server exited and was restarted. The live stream \
             drops for a couple of seconds each time; a repeating count usually means \
             the port is held by an orphaned daemon from a previous build."
        }
        "reporter_degraded" => {
            "One of this report's own sources could not be read, so the report is \
             incomplete. What is listed is still real; what is missing is unknown."
        }
        "ranked_decline" => {
            "The ranked outline refused and fell back to the legacy outline. Not a failure \
             on its own, but a high rate on one language or file shape points at a gap in \
             the ranking path."
        }
        _ => UNCLASSIFIED,
    }
}

/// `retry_escape_valve` → `retry escape valve`
fn humanize(s: &str) -> String {
    s.replace('_', " ")
}

/// Renders the no-sub-kind sentinel as an em dash, so a table cell reads as "nothing
/// here" rather than as a literal value named `-`.
fn em_dash_if_absent(variant: &str) -> String {
    if variant == NO_VARIANT {
        "—".to_string()
    } else {
        variant.to_string()
    }
}

/// `2026-07-29T14:02:11Z` → `07-29 14:02`. Returns the input unchanged if it is not
/// the shape expected, rather than inventing a timestamp.
fn short_ts(ts: &str) -> String {
    let bytes = ts.as_bytes();
    if bytes.len() >= 16 && bytes[4] == b'-' && bytes[10] == b'T' {
        format!("{} {}", &ts[5..10], &ts[11..16])
    } else {
        ts.to_string()
    }
}

/// Whether a path string is absolute on **any** platform.
///
/// `Path::is_relative` answers only for the host. On Windows `/Users/me/clients/acme`
/// carries no drive letter and is therefore "relative", so a report generated on Windows
/// treated every Unix-style absolute path as workspace-relative and published it
/// verbatim — including the client directory names redaction exists to strip. CI caught
/// it on windows-latest; macOS could not have.
///
/// Redaction must not depend on which machine renders the report, so absoluteness is
/// decided from the string: a leading separator (Unix root, Windows rooted path, or a
/// UNC share) or a drive qualifier.
fn is_absolute_anywhere(raw: &str) -> bool {
    let b = raw.as_bytes();
    if b.first().is_some_and(|c| *c == b'/' || *c == b'\\') {
        return true;
    }
    b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/')
}

/// Reduce a path to something safe to publish.
///
/// Inside the workspace a relative path is fine — it is public code. Outside it, the
/// basename itself can carry a client or project name, so only the extension survives;
/// the content hash is what lets a maintainer confirm they hold the same file.
fn redact_path(raw: &str, env: &Environment) -> String {
    let p = Path::new(raw);
    if !is_absolute_anywhere(raw) {
        return raw.to_string();
    }
    if let Some(root) = &env.workspace_root
        && let Ok(rel) = p.strip_prefix(root)
    {
        return rel.to_string_lossy().into_owned();
    }
    match p.extension().and_then(|e| e.to_str()) {
        Some(ext) => format!("<redacted:external>.{ext}"),
        None => "<redacted:external>".to_string(),
    }
}

/// Reduce an env override's value to something publishable.
///
/// The knobs worth reporting are flags and numbers (`LUMEN_LINE_THRESHOLD=300`,
/// `LUMEN_CAPTURE=0`). The ones that carry a path — `LUMEN_DB`, `LUMEN_FAULT_SPOOL`,
/// `LUMEN_PROJECTS_DIR` — matter only in that they are *set*, and their values name
/// directories that can identify a client. `$HOME` collapsing is not enough on its own:
/// `/Users/me/clients/acme/lumen.db` would still ship "clients/acme".
fn redact_env_value(value: &str) -> String {
    if value.contains('/') || value.contains('\\') {
        "<path>".to_string()
    } else {
        value.to_string()
    }
}

/// Last-resort scrub over fully rendered text. `redact_path` handles the path fields;
/// this catches an absolute path or a username that rode in on a free-form `detail`.
fn scrub(text: &str, env: &Environment) -> String {
    let mut out = text.to_string();
    if let Some(home) = &env.home {
        let home = home.to_string_lossy();
        out = out.replace(home.as_ref(), "~");
        if let Some(user) = Path::new(home.as_ref())
            .file_name()
            .and_then(|u| u.to_str())
            && user.len() >= 3
        {
            out = out.replace(user, "<user>");
        }
    }
    out
}

fn group(faults: &[Fault], env: &Environment) -> Vec<Group> {
    let mut by_key: BTreeMap<(String, String), Group> = BTreeMap::new();

    for f in faults {
        let key = (f.kind.clone(), f.variant().to_string());
        let g = by_key.entry(key).or_insert_with(|| Group {
            kind: f.kind.clone(),
            variant: f.variant().to_string(),
            files: BTreeSet::new(),
            count: 0,
            first_seen: None,
            last_seen: None,
            details: Vec::new(),
        });

        g.count += f.count;
        if let Some(p) = &f.path {
            g.files.insert(redact_path(p, env));
        }
        if let Some(d) = &f.detail
            && !g.details.contains(d)
        {
            g.details.push(d.clone());
        }
        // Strings are ISO-8601 UTC, so lexicographic ordering is chronological.
        if let Some(t) = &f.first_seen
            && g.first_seen.as_ref().is_none_or(|cur| t < cur)
        {
            g.first_seen = Some(t.clone());
        }
        if let Some(t) = &f.last_seen
            && g.last_seen.as_ref().is_none_or(|cur| t > cur)
        {
            g.last_seen = Some(t.clone());
        }
    }

    let mut groups: Vec<Group> = by_key.into_values().collect();
    groups.sort_by(|a, b| {
        kind_priority(&a.kind)
            .cmp(&kind_priority(&b.kind))
            .then(b.count.cmp(&a.count))
            .then(a.variant.cmp(&b.variant))
    });
    groups
}

/// Dedupe key. Covers only `(kind, variant, version)` — deliberately not counts,
/// timestamps or paths, so the same defect fingerprints identically across runs and
/// across users, and `lumen report` can find the issue it already filed.
pub fn fingerprint(faults: &[Fault], env: &Environment) -> String {
    let mut keys: Vec<String> = faults
        .iter()
        .map(|f| format!("{}|{}", f.kind, f.variant()))
        .collect();
    keys.sort();
    keys.dedup();
    keys.push(env.lumen_version.clone());
    sha256_hex(keys.join("\n").as_bytes())[..8].to_string()
}

fn headline(top: &Group, version: &str) -> String {
    let files = top.files.len();
    let body = match top.kind.as_str() {
        "hook_fail_open" => format!(
            "{} fired {}× on {} file{}",
            humanize(&top.variant),
            top.count,
            files,
            if files == 1 { "" } else { "s" }
        ),
        "ranked_decline" => format!(
            "ranked outline declined ({}) {}× on {} file{}",
            top.variant,
            top.count,
            files,
            if files == 1 { "" } else { "s" }
        ),
        "schema_drift" => "read_events schema drift".to_string(),
        "ingest_failed" => format!(
            "ingest failed {}× ({}) — turns missing from the ledger",
            top.count, top.variant
        ),
        "ws_restart" => format!("the daemon's WebSocket server restarted {}×", top.count),
        "reporter_degraded" => "this report is incomplete".to_string(),
        other => format!("{} ×{}", humanize(other), top.count),
    };
    format!("lumen {version} — {body}")
}

/// Rendering choices that change what leaves the machine.
#[derive(Debug, Clone, Default)]
pub struct RenderOpts {
    /// Embed the contents of affected files. Off by default, and the CLI prints a
    /// manifest of exactly what it will embed before emitting the body.
    pub include_source: bool,
}

/// Bytes of any one file embedded under `include_source`. A 1900-line Rust file would
/// otherwise produce an issue nobody can read and a paste nobody vetted.
const INCLUDE_SOURCE_BYTE_CAP: usize = 8 * 1024;

/// Render the issue body. `None` when there is nothing to report — the caller must
/// not file an empty issue.
pub fn render(faults: &[Fault], env: &Environment, opts: &RenderOpts) -> Option<String> {
    let groups = group(faults, env);
    let top = groups.first()?;

    let mut s = String::new();
    s.push_str(&format!("### {}\n\n", headline(top, &env.lumen_version)));
    s.push_str(&format!("**Impact:** {}\n\n", impact(&top.kind)));

    // "variant", not "detail": the free-form detail gets its own section below, and
    // labelling both the same made the table look like it had been truncated.
    s.push_str("| kind | variant | files | count | first seen | last seen |\n");
    s.push_str("|---|---|---|---|---|---|\n");
    for g in &groups {
        s.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} |\n",
            g.kind,
            em_dash_if_absent(&g.variant),
            if g.files.is_empty() {
                "—".to_string()
            } else {
                g.files.len().to_string()
            },
            g.count,
            g.first_seen
                .as_deref()
                .map(short_ts)
                .unwrap_or_else(|| "—".into()),
            g.last_seen
                .as_deref()
                .map(short_ts)
                .unwrap_or_else(|| "—".into()),
        ));
    }

    let files = affected_files(faults, env);
    if !files.is_empty() {
        s.push_str("\n**Affected files** (metadata only — no contents attached)\n");
        for line in files {
            s.push_str(&format!("- {line}\n"));
        }
    }

    let detailed: Vec<&Group> = groups.iter().filter(|g| !g.details.is_empty()).collect();
    if !detailed.is_empty() {
        s.push_str("\n**Details**\n");
        for g in detailed {
            let heading = if g.variant == NO_VARIANT {
                format!("`{}`", g.kind)
            } else {
                format!("`{}` / `{}`", g.kind, g.variant)
            };
            s.push_str(&format!("\n{heading}\n```\n"));
            for d in &g.details {
                s.push_str(&clamp_detail(d));
                s.push('\n');
            }
            s.push_str("```\n");
        }
    }

    if opts.include_source {
        for (label, body) in embedded_sources(faults, env) {
            s.push_str(&format!(
                "\n<details><summary>source: {label}</summary>\n\n```\n{body}\n```\n</details>\n"
            ));
        }
    }

    s.push_str("\n**Environment**\n");
    for line in environment_lines(env) {
        s.push_str(&format!("- {line}\n"));
    }

    s.push_str(&format!(
        "\n<!-- lumen-fault: {} -->\n",
        fingerprint(faults, env)
    ));

    Some(scrub(&s, env))
}

/// A long detail buries the table, so keep the head and say what was dropped —
/// silently truncating would read as a complete report.
fn clamp_detail(detail: &str) -> String {
    let lines: Vec<&str> = detail.lines().collect();
    if lines.len() <= DETAIL_LINE_CAP {
        return detail.to_string();
    }
    let kept = lines[..DETAIL_LINE_CAP].join("\n");
    let dropped = lines.len() - DETAIL_LINE_CAP;
    format!("{kept}\n… ({dropped} more lines omitted)")
}

/// One line per distinct file: redacted label, line count, extension, content hash.
/// The hash is the reproducer handle — it identifies the file without shipping it.
fn affected_files(faults: &[Fault], env: &Environment) -> Vec<String> {
    let mut seen: BTreeMap<String, String> = BTreeMap::new();

    for f in faults {
        let Some(raw) = &f.path else { continue };
        let label = redact_path(raw, env);
        if seen.contains_key(&label) {
            continue;
        }

        let ext = Path::new(raw)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("—");
        let lines = f
            .lines
            .map(|n| format!("{n} lines"))
            .unwrap_or_else(|| "? lines".into());

        // Resolve relative fixture paths against the workspace before hashing.
        let abs = match (is_absolute_anywhere(raw), &env.workspace_root) {
            (false, Some(root)) => root.join(raw),
            _ => PathBuf::from(raw),
        };
        let digest = match std::fs::read(&abs) {
            Ok(bytes) => format!("sha256:{}", &sha256_hex(&bytes)[..12]),
            Err(_) => "sha256:unavailable".to_string(),
        };

        seen.insert(
            label.clone(),
            format!("`{label}` · {lines} · {ext} · {digest}"),
        );
    }

    seen.into_values().collect()
}

fn environment_lines(env: &Environment) -> Vec<String> {
    let sha = env.git_sha.as_deref().unwrap_or("unavailable");
    let mut lines = vec![
        format!("lumen {} · git `{}`", env.lumen_version, sha),
        format!(
            "{} {} · channel `{}` · MCP scope: {}",
            env.os, env.arch, env.channel, env.mcp_scope
        ),
    ];

    let servers = env
        .mcp_json_servers
        .map(|n| format!("{n} server(s)"))
        .unwrap_or_else(|| "not readable".into());
    let hooks = env
        .hooks_digest
        .as_ref()
        .map(|d| format!("sha256:{}", &d[..8.min(d.len())]))
        .unwrap_or_else(|| "absent".into());
    lines.push(format!(
        "`.mcp.json` declares {servers} · hooks digest `{hooks}`"
    ));

    lines.push(match env.read_events_cols {
        Some(n) => format!("`read_events` {n} columns"),
        None => "`read_events` not readable (no database at the resolved path)".to_string(),
    });

    lines.push(if env.env_overrides.is_empty() {
        "env overrides in effect: none".to_string()
    } else {
        let joined = env
            .env_overrides
            .iter()
            .map(|(k, v)| format!("`{k}={}`", redact_env_value(v)))
            .collect::<Vec<_>>()
            .join(", ");
        format!("env overrides in effect: {joined}")
    });

    // Only rendered when the caller knew. The CLI cannot — the tray belongs to the GUI process
    // — and printing "tray: unknown" on every CLI report would be noise that reads like a
    // finding.
    if let Some(tray) = &env.tray {
        lines.push(format!("menu-bar icon: {tray}"));
    }
    if !env.startup_degradations.is_empty() {
        lines.push(format!(
            "startup degraded: {}",
            env.startup_degradations.join("; ")
        ));
    }

    lines
}

/// Resolve each affected file and read a capped prefix, for `--include-source`.
///
/// Only files inside the workspace are eligible. An out-of-workspace path was redacted
/// precisely because its name could identify a client; embedding its body would leak far
/// more than the name did, so `--include-source` must not reach it.
pub fn embedded_sources(faults: &[Fault], env: &Environment) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();

    for f in faults {
        let Some(raw) = &f.path else { continue };
        let label = redact_path(raw, env);
        if label.starts_with("<redacted") || !seen.insert(label.clone()) {
            continue;
        }
        let Some(root) = &env.workspace_root else {
            continue;
        };
        let abs = if is_absolute_anywhere(raw) {
            PathBuf::from(raw)
        } else {
            root.join(raw)
        };
        let Ok(text) = std::fs::read_to_string(&abs) else {
            continue;
        };

        let body = if text.len() > INCLUDE_SOURCE_BYTE_CAP {
            let cut = text
                .char_indices()
                .map(|(i, _)| i)
                .take_while(|i| *i <= INCLUDE_SOURCE_BYTE_CAP)
                .last()
                .unwrap_or(0);
            format!(
                "{}\n… truncated at {} of {} bytes",
                &text[..cut],
                cut,
                text.len()
            )
        } else {
            text
        };
        out.push((label, body));
    }
    out
}

/// What `--include-source` would upload, for printing before it does. Returned rather
/// than printed so the caller decides the stream and the wording.
pub fn source_manifest(faults: &[Fault], env: &Environment) -> Vec<(String, usize)> {
    embedded_sources(faults, env)
        .into_iter()
        .map(|(label, body)| (label, body.len()))
        .collect()
}

/// The five `routed_via` values that mean the ranked outline refused.
///
/// Built from the enum rather than written out, so a new `Decline` variant cannot be
/// silently missed by the reporter.
fn decline_routes() -> Vec<&'static str> {
    use crate::ranked::Decline::*;
    [NoQuery, NoDefs, NotWorthIt, WouldInflate, TooSlow]
        .iter()
        .map(|d| d.route())
        .collect()
}

/// Collect faults from the database: drained spool rows, ranked declines already in
/// `read_events`, and a live schema check.
///
/// The drain runs first so a fault recorded seconds ago by a hook is in this report.
pub fn load_faults_from_db(conn: &rusqlite::Connection) -> Result<Vec<Fault>, String> {
    let mut out = Vec::new();
    let mut degraded: Vec<String> = Vec::new();

    // Every read below degrades instead of aborting. A stale or damaged database is
    // precisely what this report exists to describe, so a reporter that dies on one
    // cannot do its job — it would fail exactly when it is most needed.
    if let Err(e) = crate::faults::drain_spool(conn) {
        degraded.push(format!("spool drain: {e}"));
    }
    match spooled_faults(conn) {
        Ok(mut f) => out.append(&mut f),
        Err(e) => degraded.push(format!("faults table: {e}")),
    }
    match declines(conn) {
        Ok(mut f) => out.append(&mut f),
        Err(e) => degraded.push(format!("ranked declines: {e}")),
    }

    if let Some(drift) = schema_drift_fault(conn) {
        out.push(drift);
    }
    // Surfaced as a fault rather than swallowed: a report that quietly omits a source
    // reads as "nothing there", which is the opposite of what happened.
    for detail in degraded {
        out.push(Fault {
            kind: "reporter_degraded".to_string(),
            detail: Some(detail),
            ..Default::default()
        });
    }
    Ok(out)
}

/// Rows drained from the spool into `faults`.
///
/// Grouped by detail as well as path: distinct details are distinct information, and the
/// renderer merges them back under one heading while summing the counts.
fn spooled_faults(conn: &rusqlite::Connection) -> rusqlite::Result<Vec<Fault>> {
    let lines = if has_column(conn, "faults", "lines") {
        "lines"
    } else {
        "NULL"
    };
    let mut stmt = conn.prepare(&format!(
        "SELECT kind, variant, path, {lines}, detail, count(*), min(ts), max(ts) \
         FROM faults GROUP BY kind, variant, path, detail"
    ))?;
    let rows = stmt.query_map([], |r| {
        Ok(Fault {
            kind: r.get(0)?,
            path: r.get(2)?,
            lines: r.get(3)?,
            detail: r.get(4)?,
            count: r.get::<_, i64>(5)? as u64,
            first_seen: r.get(6)?,
            last_seen: r.get(7)?,
            ..Default::default()
        }
        // Carried in `guard`: `variant()` reads route first, and a stored variant must
        // not masquerade as a decline route when the kind is not a decline.
        .with_variant(r.get::<_, String>(1)?))
    })?;
    rows.collect()
}

/// Ranked declines were already metered by the MCP server; they need a reader, not a
/// writer. `lines` is selected only when present, so a database predating it still
/// yields its declines instead of erroring the whole report.
fn declines(conn: &rusqlite::Connection) -> rusqlite::Result<Vec<Fault>> {
    let routes = decline_routes();
    let placeholders = vec!["?"; routes.len()].join(",");
    let lines = if has_column(conn, "read_events", "lines") {
        "lines"
    } else {
        "NULL"
    };
    let mut stmt = conn.prepare(&format!(
        "SELECT routed_via, path, {lines}, count(*), min(ts), max(ts) FROM read_events \
         WHERE routed_via IN ({placeholders}) GROUP BY routed_via, path"
    ))?;
    let rows = stmt.query_map(rusqlite::params_from_iter(routes.iter()), |r| {
        Ok(Fault {
            kind: "ranked_decline".to_string(),
            route: Some(r.get(0)?),
            path: r.get(1)?,
            lines: r.get(2)?,
            count: r.get::<_, i64>(3)? as u64,
            first_seen: r.get(4)?,
            last_seen: r.get(5)?,
            ..Default::default()
        })
    })?;
    rows.collect()
}

fn has_column(conn: &rusqlite::Connection, table: &str, column: &str) -> bool {
    // `table` is a literal at every call site, never user input.
    let Ok(mut stmt) = conn.prepare(&format!("PRAGMA table_info({table})")) else {
        return false;
    };
    let Ok(rows) = stmt.query_map([], |r| r.get::<_, String>(1)) else {
        return false;
    };
    rows.flatten().any(|name| name == column)
}

/// Faults worth prompting someone about: recorded occurrences plus anything still
/// spooled.
///
/// Ranked declines are deliberately excluded even though the report body includes them.
/// A decline is a normal fallback, there are hundreds of them on any active install, and
/// counting them would keep a badge permanently lit for something nobody needs to act on
/// — which trains people to ignore the badge.
///
/// Read-only: it does not drain, so refreshing a badge on navigation writes nothing.
pub fn actionable_fault_count(conn: &rusqlite::Connection) -> u64 {
    let recorded: i64 = conn
        .query_row("SELECT count(*) FROM faults", [], |r| r.get(0))
        .unwrap_or(0);
    recorded.max(0) as u64 + lumen_core_spool_len()
}

/// Indirection so the count can be unit-tested without a spool on disk.
fn lumen_core_spool_len() -> u64 {
    crate::faults::spool_len() as u64
}

/// A live comparison of `read_events` against the column set this build expects.
///
/// Synthesised at read time rather than captured: drift is a standing condition, not an
/// event, so there is no moment at which to record it.
fn schema_drift_fault(conn: &rusqlite::Connection) -> Option<Fault> {
    let mut stmt = conn.prepare("PRAGMA table_info(read_events)").ok()?;
    let live = stmt.query_map([], |_| Ok(())).ok()?.count();
    if live == 0 || live == crate::schema::READ_EVENTS_COLUMNS {
        return None;
    }
    Some(Fault {
        kind: "schema_drift".to_string(),
        detail: Some(format!(
            "read_events has {live} columns; this build expects {}",
            crate::schema::READ_EVENTS_COLUMNS
        )),
        count: 1,
        ..Default::default()
    })
}

/// Where filing talks to, and how it opens a browser.
///
/// Injected rather than hardcoded so the REST and browser routes can be exercised for
/// real — against a local listener and a stub opener — instead of only at the point they
/// first meet GitHub. Both shipped in 1.5.0 having never created anything, which is not a
/// state a filing path should reach users in.
///
/// `from_env` reads `LUMEN_GITHUB_API`, `LUMEN_GITHUB_WEB` and `LUMEN_OPEN_CMD`; tests
/// construct the struct directly so they never mutate process-global environment, which
/// cannot be done safely under a parallel test runner.
#[derive(Debug, Clone)]
pub struct Endpoints {
    /// REST base, no trailing slash. `https://api.github.com` in production.
    pub api_base: String,
    /// Web base for the prefilled form. `https://github.com` in production.
    pub web_base: String,
    /// argv for opening a URL, which is appended as the final argument.
    /// `None` picks the platform default.
    pub open_cmd: Option<Vec<String>>,
    /// argv for the GitHub CLI. `None` means plain `gh` on `PATH`.
    ///
    /// Injected for the same reason as `open_cmd`: without it, a test that needs the gh
    /// route to fail has to rely on `gh` rejecting a nonexistent repository, which is a
    /// live network call — slow, and flaky enough that the suite passed or failed
    /// depending on GitHub's mood.
    pub gh_cmd: Option<Vec<String>>,
    /// Bearer token for the REST route. `None` means that route declines.
    ///
    /// Carried here rather than read from the environment at the point of use: two tests
    /// that each needed a different answer had to set and unset the same variable, and
    /// under a parallel runner they raced — one test's `remove_var` made another's route
    /// decline, which looked exactly like a logic bug in the chain.
    pub token: Option<String>,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            api_base: "https://api.github.com".to_string(),
            web_base: "https://github.com".to_string(),
            open_cmd: None,
            gh_cmd: None,
            token: None,
        }
    }
}

impl Endpoints {
    pub fn from_env() -> Self {
        let d = Self::default();
        Self {
            api_base: std::env::var("LUMEN_GITHUB_API").unwrap_or(d.api_base),
            web_base: std::env::var("LUMEN_GITHUB_WEB").unwrap_or(d.web_base),
            open_cmd: std::env::var("LUMEN_OPEN_CMD").ok().map(|s| {
                s.split_whitespace()
                    .map(str::to_string)
                    .collect::<Vec<String>>()
            }),
            gh_cmd: std::env::var("LUMEN_GH_CMD").ok().map(|s| {
                s.split_whitespace()
                    .map(str::to_string)
                    .collect::<Vec<String>>()
            }),
            token: api_token(),
        }
    }
}

/// Default issue tracker. Overridable so a fork, or a rehearsal against a scratch repo,
/// does not have to be a code change.
pub const DEFAULT_REPO: &str = "HackPoint/lumen";

/// Open issues scanned when looking for a prior report of this fingerprint.
const DEDUPE_SCAN_LIMIT: usize = 100;

/// The marker `render` embeds, which is also the dedupe key.
pub fn marker(fp: &str) -> String {
    format!("<!-- lumen-fault: {fp} -->")
}

/// What filing did.
#[derive(Debug, PartialEq, Eq)]
pub enum Filed {
    Created(String),
    /// Commented on an existing issue rather than opening a duplicate.
    Commented(String),
    /// A prefilled form was opened in the browser. **Nothing is published yet** — the
    /// human still has to press Submit. Callers must not report this as filed.
    Handoff(String),
}

/// The outcome plus which route produced it.
#[derive(Debug)]
pub struct Filing {
    pub outcome: Filed,
    /// The route that worked: `gh`, `api` or `browser`.
    pub route: &'static str,
    /// Why each earlier route was passed over, in order. Empty when the first worked.
    ///
    /// Kept and surfaced rather than swallowed: "filing failed" with no history is
    /// unfixable, and a silent fallback hides that the preferred route is broken.
    pub fell_back: Vec<String>,
}

fn gh(ep: &Endpoints, args: &[&str]) -> Result<String, String> {
    let (program, prefix) = match ep.gh_cmd.as_deref() {
        Some([first, rest @ ..]) => (first.as_str(), rest),
        _ => ("gh", &[][..]),
    };
    let out = std::process::Command::new(program)
        .args(prefix)
        .args(args)
        .output()
        .map_err(|e| format!("cannot run {program} (is the GitHub CLI installed?): {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "{program} {} failed: {}",
            args.first().unwrap_or(&""),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// A token for the REST route. Never read from a file or a keychain here — only the
/// environment, so this cannot quietly acquire credentials the user did not offer.
fn api_token() -> Option<String> {
    for k in ["GITHUB_TOKEN", "GH_TOKEN"] {
        if let Ok(v) = std::env::var(k) {
            let v = v.trim().to_string();
            if !v.is_empty() {
                return Some(v);
            }
        }
    }
    None
}

/// Percent-encode for a query string. Unreserved set per RFC 3986; everything else is
/// escaped, including the spaces and newlines a rendered report is full of.
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// A temp-file name unique to this call, not just this process.
///
/// These used to be keyed on the pid alone. Two filing operations in one process then
/// shared a path: each wrote its own curl config, and whichever finished first deleted the
/// other's out from under it — so a request could be made with a config belonging to a
/// different call, or none at all. The Tauri commands run on a blocking pool and the CLI
/// does a dedupe read before a write, so concurrency here is ordinary rather than exotic.
fn scratch_path(stem: &str, ext: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("{stem}-{}-{n}.{ext}", std::process::id()))
}

/// One HTTPS request via `curl`, returning `(status, body)`.
///
/// curl rather than an HTTP crate: the workspace has no HTTP client, and adding one
/// pulls in a TLS stack for three requests. This shells out the way the `git` and `gh`
/// calls already do. curl ships with macOS, every mainstream Linux, and Windows 10+.
///
/// The token goes in a `--config` file, never in argv: arguments are readable by any
/// process on the machine via `ps`, and a leaked token is worse than a failed filing.
fn curl(
    method: &str,
    url: &str,
    token: Option<&str>,
    body: Option<&str>,
) -> Result<(u16, String), String> {
    let cfg_path = scratch_path("lumen-curl", "cfg");
    let body_path = scratch_path("lumen-curl", "json");

    let mut cfg = String::new();
    cfg.push_str("silent\nshow-error\n");
    cfg.push_str(&format!("request = {method}\n"));
    cfg.push_str("header = \"Accept: application/vnd.github+json\"\n");
    cfg.push_str("header = \"X-GitHub-Api-Version: 2022-11-28\"\n");
    cfg.push_str("header = \"User-Agent: lumen\"\n");
    if let Some(t) = token {
        cfg.push_str(&format!("header = \"Authorization: Bearer {t}\"\n"));
    }
    if let Some(b) = body {
        std::fs::write(&body_path, b).map_err(|e| format!("cannot stage request body: {e}"))?;
        cfg.push_str(&format!("data-binary = @{}\n", body_path.display()));
    }
    cfg.push_str(&format!("url = {url}\n"));
    cfg.push_str("write-out = \"\\n%{http_code}\"\n");

    write_private(&cfg_path, &cfg)?;
    let out = std::process::Command::new("curl")
        .arg("--config")
        .arg(&cfg_path)
        .output();
    let _ = std::fs::remove_file(&cfg_path);
    let _ = std::fs::remove_file(&body_path);

    let out = out.map_err(|e| format!("cannot run curl: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    if !out.status.success() && text.is_empty() {
        return Err(format!(
            "curl failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    // write-out appended the status after a newline.
    let (body, code) = text.rsplit_once('\n').unwrap_or(("", text.as_str()));
    let code: u16 = code.trim().parse().unwrap_or(0);
    Ok((code, body.to_string()))
}

/// Write a file only the owner can read. The curl config carries a bearer token.
fn write_private(path: &Path, contents: &str) -> Result<(), String> {
    std::fs::write(path, contents).map_err(|e| format!("cannot stage curl config: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

/// Find an open issue already carrying this fingerprint, and its number + URL.
///
/// Bodies are fetched and matched locally rather than handed to a search query:
/// GitHub's search index does not reliably match text inside an HTML comment, and a
/// dedupe that silently misses is worse than no dedupe — it opens a duplicate every run.
///
/// Tries `gh`, then the REST API. The read works unauthenticated on a public repo, so
/// dedupe survives on a machine with neither `gh` nor a token.
pub fn find_existing(repo: &str, fp: &str) -> Result<Option<(u64, String)>, String> {
    find_existing_with(&Endpoints::from_env(), repo, fp)
}

/// [`find_existing`] against explicit endpoints.
pub fn find_existing_with(
    ep: &Endpoints,
    repo: &str,
    fp: &str,
) -> Result<Option<(u64, String)>, String> {
    let limit = DEDUPE_SCAN_LIMIT.to_string();
    let json = gh(
        ep,
        &[
            "issue",
            "list",
            "--repo",
            repo,
            "--state",
            "open",
            "--limit",
            &limit,
            "--json",
            "number,body,url",
        ],
    )
    .or_else(|_| {
        let url = format!(
            "{}/repos/{repo}/issues?state=open&per_page={limit}",
            ep.api_base
        );
        curl("GET", &url, ep.token.as_deref(), None).and_then(|(code, body)| {
            if code == 200 {
                Ok(body)
            } else {
                Err(format!("GET issues returned {code}"))
            }
        })
    })?;

    let issues: Vec<serde_json::Value> =
        serde_json::from_str(&json).map_err(|e| format!("cannot parse issue list: {e}"))?;
    if issues.len() == DEDUPE_SCAN_LIMIT {
        eprintln!(
            "lumen report: scanned only the {DEDUPE_SCAN_LIMIT} most recent open issues; \
             an older duplicate would not be found"
        );
    }

    let needle = marker(fp);
    Ok(issues
        .iter()
        .find(|i| {
            i.get("body")
                .and_then(|b| b.as_str())
                .is_some_and(|b| b.contains(&needle))
        })
        .and_then(|i| {
            let n = i.get("number")?.as_u64()?;
            let url = i
                .get("url")
                .or_else(|| i.get("html_url"))
                .and_then(|u| u.as_str())
                .unwrap_or_default()
                .to_string();
            Some((n, url))
        }))
}

/// File the report, trying each route in turn until one works.
///
/// `gh` first because it is the only route that can *comment* on an existing issue
/// without a human, and a maintainer on a terminal already has it. Then the REST API,
/// which needs a token. Then the browser, which needs nothing but cannot finish the job
/// on its own — it hands a prefilled form to the user.
///
/// Every failure is collected. A caller that reports only the last one would say
/// "cannot open a browser" on a machine whose real problem is an expired token.
pub fn file_issue(repo: &str, title: &str, body: &str, fp: &str) -> Result<Filing, String> {
    file_issue_with(&Endpoints::from_env(), repo, title, body, fp)
}

/// [`file_issue`] against explicit endpoints.
pub fn file_issue_with(
    ep: &Endpoints,
    repo: &str,
    title: &str,
    body: &str,
    fp: &str,
) -> Result<Filing, String> {
    let mut fell_back: Vec<String> = Vec::new();

    // The dedupe read has its own fallbacks; if every one fails, file anyway rather than
    // lose the report, and say so.
    let existing = match find_existing_with(ep, repo, fp) {
        Ok(e) => e,
        Err(e) => {
            fell_back.push(format!(
                "dedupe check unavailable ({e}); may open a duplicate"
            ));
            None
        }
    };

    match via_gh(ep, repo, title, body, existing.as_ref()) {
        Ok(outcome) => {
            return Ok(Filing {
                outcome,
                route: "gh",
                fell_back,
            });
        }
        Err(e) => fell_back.push(format!("gh: {e}")),
    }

    match via_api(ep, repo, title, body, existing.as_ref()) {
        Ok(outcome) => {
            return Ok(Filing {
                outcome,
                route: "api",
                fell_back,
            });
        }
        Err(e) => fell_back.push(format!("api: {e}")),
    }

    match via_browser(ep, repo, title, body, existing.as_ref()) {
        Ok(outcome) => {
            return Ok(Filing {
                outcome,
                route: "browser",
                fell_back,
            });
        }
        Err(e) => fell_back.push(format!("browser: {e}")),
    }

    Err(format!(
        "every filing route failed:\n  - {}",
        fell_back.join("\n  - ")
    ))
}

fn via_gh(
    ep: &Endpoints,
    repo: &str,
    title: &str,
    body: &str,
    existing: Option<&(u64, String)>,
) -> Result<Filed, String> {
    if let Some((number, _)) = existing {
        let n = number.to_string();
        let url = write_via_tempfile(ep, &["issue", "comment", &n, "--repo", repo], body)?;
        return Ok(Filed::Commented(url.trim().to_string()));
    }
    let url = write_via_tempfile(
        ep,
        &["issue", "create", "--repo", repo, "--title", title],
        body,
    )?;
    Ok(Filed::Created(url.trim().to_string()))
}

fn via_api(
    ep: &Endpoints,
    repo: &str,
    title: &str,
    body: &str,
    existing: Option<&(u64, String)>,
) -> Result<Filed, String> {
    let token = ep
        .token
        .as_deref()
        .ok_or("no GITHUB_TOKEN or GH_TOKEN in the environment")?;

    let (url, payload, commenting) = match existing {
        Some((number, _)) => (
            format!("{}/repos/{repo}/issues/{number}/comments", ep.api_base),
            serde_json::json!({ "body": body }),
            true,
        ),
        None => (
            format!("{}/repos/{repo}/issues", ep.api_base),
            serde_json::json!({ "title": title, "body": body }),
            false,
        ),
    };

    let (code, resp) = curl("POST", &url, Some(token), Some(&payload.to_string()))?;
    if !(200..300).contains(&code) {
        let msg = serde_json::from_str::<serde_json::Value>(&resp)
            .ok()
            .and_then(|v| {
                v.get("message")
                    .and_then(|m| m.as_str())
                    .map(str::to_string)
            })
            .unwrap_or_else(|| resp.chars().take(160).collect());
        return Err(format!("POST returned {code}: {msg}"));
    }

    let html = serde_json::from_str::<serde_json::Value>(&resp)
        .ok()
        .and_then(|v| {
            v.get("html_url")
                .and_then(|u| u.as_str())
                .map(str::to_string)
        })
        .unwrap_or_default();
    Ok(if commenting {
        Filed::Commented(html)
    } else {
        Filed::Created(html)
    })
}

/// Longest prefilled URL attempted. Browsers and intermediaries start truncating well
/// before this; past it, open the blank form so the body is not silently cut in half.
const PREFILL_URL_CAP: usize = 6000;

fn via_browser(
    ep: &Endpoints,
    repo: &str,
    title: &str,
    body: &str,
    existing: Option<&(u64, String)>,
) -> Result<Filed, String> {
    // An existing issue cannot be commented on from a URL, so go to the issue itself
    // rather than opening a form that would create a duplicate.
    if let Some((number, url)) = existing {
        let target = if url.is_empty() {
            format!("{}/{repo}/issues/{number}", ep.web_base)
        } else {
            url.clone()
        };
        open_in_browser(ep, &target)?;
        return Ok(Filed::Handoff(target));
    }

    let full = format!(
        "{}/{repo}/issues/new?title={}&body={}",
        ep.web_base,
        percent_encode(title),
        percent_encode(body)
    );
    let target = if full.len() <= PREFILL_URL_CAP {
        full
    } else {
        // Truncating the body would file a half report that looks complete.
        let path = std::env::temp_dir().join("lumen-issue-body.md");
        std::fs::write(&path, body).map_err(|e| format!("cannot stage the body: {e}"))?;
        eprintln!(
            "lumen report: the report is too long to prefill; its text is at {} — \
             paste it into the form that just opened",
            path.display()
        );
        format!("{}/{repo}/issues/new", ep.web_base)
    };
    open_in_browser(ep, &target)?;
    Ok(Filed::Handoff(target))
}

fn open_in_browser(ep: &Endpoints, url: &str) -> Result<(), String> {
    // An injected opener is what makes this route testable: a stub records the URL it was
    // handed, so the prefilled body can be asserted without a browser window.
    if let Some(argv) = &ep.open_cmd {
        let (cmd, rest) = argv.split_first().ok_or("LUMEN_OPEN_CMD is empty")?;
        let status = std::process::Command::new(cmd)
            .args(rest)
            .arg(url)
            .status()
            .map_err(|e| format!("cannot run {cmd}: {e}"))?;
        return if status.success() {
            Ok(())
        } else {
            Err(format!("{cmd} exited with {status}"))
        };
    }
    // Windows needs a hand-built command line. `start` is a cmd.exe builtin, and cmd
    // treats `&` as a command separator — every prefilled URL contains `&body=`, so
    // passing it as an ordinary argument made cmd try to run the second half as a command
    // and exit 1. Rust's normal argument quoting follows the MSVC convention, which
    // cmd.exe does not honour, so `raw_arg` is the only way to get the quotes cmd needs.
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        // A URL cannot legally contain a double quote, so quoting it is unambiguous.
        // The empty "" is `start`'s window-title argument, which it requires before a URL.
        let status = std::process::Command::new("cmd")
            .raw_arg(format!("/C start \"\" \"{url}\""))
            .status()
            .map_err(|e| format!("cannot run cmd: {e}"))?;
        return if status.success() {
            Ok(())
        } else {
            Err(format!("cmd exited with {status}"))
        };
    }

    #[cfg(not(target_os = "windows"))]
    {
        let cmd = if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        };
        let status = std::process::Command::new(cmd)
            .arg(url)
            .status()
            .map_err(|e| format!("cannot run {cmd}: {e}"))?;
        if !status.success() {
            return Err(format!("{cmd} exited with {status}"));
        }
        Ok(())
    }
}

/// `gh` reads `--body-file -` from stdin, which `Command::output` cannot supply without
/// a writer thread. A temp file is simpler and leaves the body inspectable if gh fails.
fn write_via_tempfile(ep: &Endpoints, args: &[&str], body: &str) -> Result<String, String> {
    let path = scratch_path("lumen-issue", "md");
    std::fs::write(&path, body).map_err(|e| format!("cannot stage issue body: {e}"))?;

    let mut full: Vec<&str> = args.to_vec();
    let p = path.to_string_lossy().into_owned();
    full.push("--body-file");
    full.push(&p);

    let result = gh(ep, &full);
    let _ = std::fs::remove_file(&path);
    result
}

/// First line of the body, without the `### ` marker — the issue title.
pub fn title_from(body: &str) -> String {
    body.lines()
        .next()
        .unwrap_or("lumen fault report")
        .trim_start_matches('#')
        .trim()
        .to_string()
}

/// Parse a fault fixture. Errors carry the path, since a typo in the JSON is the
/// most likely failure while iterating on the body.
pub fn load_faults(path: &Path) -> Result<Vec<Fault>, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("cannot parse {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
    }

    /// A fixed environment, so the snapshot does not move when the host does.
    fn test_env() -> Environment {
        Environment {
            lumen_version: "1.4.0".into(),
            git_sha: Some("703c1f2".into()),
            os: "macos".into(),
            arch: "aarch64".into(),
            channel: "cli".into(),
            mcp_scope: "user (~/.claude.json)".into(),
            mcp_json_servers: Some(0),
            hooks_digest: Some("c41afe0912345678".into()),
            read_events_cols: Some(24),
            env_overrides: vec![("LUMEN_LINE_THRESHOLD".into(), "300".into())],
            // The CLI's shape: it cannot know the tray state, so the existing snapshots must
            // stay byte-identical. The tests below set these explicitly.
            tray: None,
            startup_degradations: Vec::new(),
            // Deliberately not the real root: paths must relativise identically on
            // any machine, and nothing outside it may survive rendering.
            workspace_root: Some(PathBuf::from("/w/lumen")),
            home: Some(PathBuf::from("/Users/testuser")),
        }
    }

    #[test]
    fn a_cli_report_does_not_claim_to_know_the_tray_state() {
        // It cannot: the tray belongs to the GUI process. Printing "tray: unknown" on every CLI
        // report would be noise that reads like a finding.
        let lines = environment_lines(&test_env());
        assert!(
            !lines.iter().any(|l| l.contains("menu-bar icon")),
            "{lines:#?}"
        );
        assert!(!lines.iter().any(|l| l.contains("startup degraded")));
    }

    #[test]
    fn a_gui_report_renders_the_tray_state_and_any_degradations() {
        // The issue #5 shape: this is the line that answers the first question a maintainer
        // would otherwise have to ask for.
        let mut env = test_env();
        env.channel = "gui".into();
        env.tray = Some("built but not visible: hidden by preference".into());
        env.startup_degradations = vec!["daemon: could not spawn: ENOENT".into()];
        let lines = environment_lines(&env);
        let joined = lines.join("\n");
        assert!(
            joined.contains("menu-bar icon: built but not visible"),
            "{joined}"
        );
        assert!(
            joined.contains("startup degraded: daemon: could not spawn"),
            "{joined}"
        );
        assert!(joined.contains("channel `gui`"), "{joined}");
    }

    #[test]
    fn the_channel_is_whatever_the_caller_says_it_is() {
        // Was hardcoded "cli", so every report filed from the app's own button claimed to come
        // from the CLI — and MESSAGING_CONTRACT.md makes channel honesty an explicit rule.
        assert_eq!(Environment::collect_for("gui").channel, "gui");
        assert_eq!(Environment::collect_for("vscode").channel, "vscode");
        assert_eq!(Environment::collect().channel, "cli");
    }

    fn load(name: &str) -> Vec<Fault> {
        load_faults(&fixture_dir().join(name)).expect("fixture parses")
    }

    #[test]
    fn renders_mixed_fixture_to_snapshot() {
        let body = render(
            &load("faults_mixed.json"),
            &test_env(),
            &RenderOpts::default(),
        )
        .expect("non-empty");
        let snap_path = fixture_dir().join("report_mixed.md");

        if std::env::var("UPDATE_SNAPSHOTS").is_ok() {
            std::fs::write(&snap_path, &body).expect("write snapshot");
            return;
        }

        // Normalised: git checks the snapshot out with CRLF on Windows, and a line-ending
        // difference is not a rendering regression.
        let expected = std::fs::read_to_string(&snap_path)
            .expect("snapshot exists")
            .replace("\r\n", "\n");
        assert_eq!(
            body, expected,
            "rendered body drifted from the snapshot; re-run with UPDATE_SNAPSHOTS=1 to accept"
        );
    }

    /// The repository is public. This is the test that keeps it publishable.
    #[test]
    fn leaks_no_absolute_path_home_or_username() {
        let body = render(
            &load("faults_mixed.json"),
            &test_env(),
            &RenderOpts::default(),
        )
        .expect("non-empty");

        assert!(!body.contains("/Users/testuser"), "home directory leaked");
        assert!(!body.contains("testuser"), "username leaked");
        assert!(!body.contains("/w/lumen"), "workspace root leaked");
        assert!(
            !body.contains("acme"),
            "a private project name outside the workspace leaked"
        );
        assert!(
            !body.lines().any(|l| l.starts_with("- `/")),
            "an absolute path was rendered as a file label"
        );
        assert!(
            body.contains("<redacted:external>"),
            "an out-of-workspace path should be redacted, not dropped silently"
        );
    }

    /// A path-valued env override must never publish its value. Found in a real run:
    /// `LUMEN_DB=/var/folders/.../tmp.X/lumen.db` was printed verbatim, and a value
    /// under `$HOME` would still have leaked the directory names below it.
    #[test]
    fn path_valued_env_overrides_are_reduced_to_a_marker() {
        let mut env = test_env();
        env.env_overrides = vec![
            ("LUMEN_DB".into(), "/Users/me/clients/acme/lumen.db".into()),
            ("LUMEN_LINE_THRESHOLD".into(), "300".into()),
            ("LUMEN_CAPTURE".into(), "0".into()),
        ];

        let body = render(&load("faults_mixed.json"), &env, &RenderOpts::default()).unwrap();
        assert!(!body.contains("acme"), "a client directory leaked via env");
        assert!(!body.contains("clients"), "a path segment leaked via env");
        assert!(
            body.contains("`LUMEN_DB=<path>`"),
            "the knob should still be reported"
        );
        assert!(
            body.contains("`LUMEN_LINE_THRESHOLD=300`") && body.contains("`LUMEN_CAPTURE=0`"),
            "non-path values stay visible — they are the useful ones"
        );
    }

    /// Source contents must never ride along, even when the file is readable.
    #[test]
    fn attaches_no_source_content() {
        let faults = vec![Fault {
            kind: "ranked_decline".into(),
            route: Some("ranked_no_defs".into()),
            guard: None,
            path: Some("crates/lumen-cli/src/report.rs".into()),
            lines: Some(400),
            detail: None,
            count: 1,
            first_seen: None,
            last_seen: None,
        }];
        let mut env = test_env();
        env.workspace_root = workspace_root();

        let body = render(&faults, &env, &RenderOpts::default()).expect("non-empty");
        assert!(
            !body.contains("DETAIL_LINE_CAP"),
            "a token from the referenced source file appeared in the body"
        );
        assert!(
            body.contains("sha256:"),
            "the content hash handle is missing"
        );
    }

    #[test]
    fn fingerprint_is_stable_across_runs_and_ignores_volatile_fields() {
        let faults = load("faults_mixed.json");
        let env = test_env();
        assert_eq!(fingerprint(&faults, &env), fingerprint(&faults, &env));

        // Counts and timestamps move constantly; the dedupe key must not.
        let mut noisier = faults.clone();
        for f in &mut noisier {
            f.count += 991;
            f.last_seen = Some("2027-01-01T00:00:00Z".into());
        }
        assert_eq!(
            fingerprint(&faults, &env),
            fingerprint(&noisier, &env),
            "fingerprint moved on counts/timestamps, so every report would open a new issue"
        );
    }

    #[test]
    fn fingerprint_distinguishes_kinds_and_versions() {
        let env = test_env();
        let base = load("faults_mixed.json");

        let one = vec![base[0].clone()];
        assert_ne!(
            fingerprint(&base, &env),
            fingerprint(&one, &env),
            "a different fault set must not collide"
        );

        let mut bumped = test_env();
        bumped.lumen_version = "1.5.0".into();
        assert_ne!(
            fingerprint(&base, &env),
            fingerprint(&base, &bumped),
            "a regression in a new version must file separately"
        );
    }

    #[test]
    fn empty_fixture_renders_nothing() {
        assert!(
            render(
                &load("faults_empty.json"),
                &test_env(),
                &RenderOpts::default()
            )
            .is_none(),
            "an empty fault list must not produce a body to file"
        );
    }

    /// The 40-line-detail stress case: kept readable, and honest about the cut.
    #[test]
    fn long_detail_is_clamped_and_says_so() {
        let long = (1..=40)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let faults = vec![Fault {
            kind: "schema_drift".into(),
            route: None,
            guard: None,
            path: None,
            lines: None,
            detail: Some(long),
            count: 1,
            first_seen: None,
            last_seen: None,
        }];

        let body = render(&faults, &test_env(), &RenderOpts::default()).expect("non-empty");
        assert!(body.contains("line 12"));
        assert!(!body.contains("line 13"));
        assert!(body.contains("(28 more lines omitted)"));
    }

    #[test]
    fn fail_open_outranks_a_far_noisier_decline() {
        let body = render(
            &load("faults_mixed.json"),
            &test_env(),
            &RenderOpts::default(),
        )
        .expect("non-empty");
        let first = body.lines().next().unwrap_or_default();
        assert!(
            first.contains("escape valve"),
            "headline should lead with the fail-open guard, got: {first}"
        );
    }

    #[test]
    fn single_occurrence_and_single_file_read_naturally() {
        let faults = vec![Fault {
            kind: "hook_fail_open".into(),
            route: None,
            guard: Some("lumen_mcp_missing".into()),
            path: Some("crates/lumen-core/src/ranked.rs".into()),
            lines: Some(1909),
            detail: None,
            count: 1,
            first_seen: Some("2026-07-30T09:00:00Z".into()),
            last_seen: Some("2026-07-30T09:00:00Z".into()),
        }];
        let body = render(&faults, &test_env(), &RenderOpts::default()).expect("non-empty");
        assert!(
            body.contains("fired 1× on 1 file\n") || body.contains("fired 1× on 1 file "),
            "singular should not read '1 files': {}",
            body.lines().next().unwrap_or_default()
        );
    }

    fn db_with_schema() -> rusqlite::Connection {
        let c = rusqlite::Connection::open_in_memory().unwrap();
        c.execute_batch(crate::schema::DDL).unwrap();
        for m in crate::schema::MIGRATIONS {
            let _ = c.execute_batch(m);
        }
        c
    }

    /// The decline routes the reader queries must be exactly the enum's, or a new
    /// variant would be silently invisible to every report.
    #[test]
    fn decline_routes_match_the_enum() {
        let routes = decline_routes();
        assert_eq!(routes.len(), 5, "a Decline variant was added or removed");
        // All but one are ranked-only. `would_inflate` is shared by every metered path, which is
        // why the report must pick these up from the enum rather than from a prefix match.
        assert!(routes.contains(&"would_inflate"));
        assert_eq!(
            routes.iter().filter(|r| r.starts_with("ranked_")).count(),
            4
        );
        assert!(
            !routes.contains(&crate::ranked::ROUTE_RANKED),
            "the success route must never be counted as a decline"
        );
    }

    #[test]
    fn db_reader_aggregates_faults_and_declines() {
        let conn = db_with_schema();

        // Two occurrences of one fault, one of another.
        for (kind, variant, path, ts) in [
            (
                "hook_fail_open",
                "retry_escape_valve",
                "a.rs",
                "2026-07-01T00:00:00Z",
            ),
            (
                "hook_fail_open",
                "retry_escape_valve",
                "a.rs",
                "2026-07-03T00:00:00Z",
            ),
            ("ingest_failed", "poll", "b.jsonl", "2026-07-02T00:00:00Z"),
        ] {
            conn.execute(
                "INSERT INTO faults(ts,kind,variant,path,channel) VALUES(?1,?2,?3,?4,'cli')",
                rusqlite::params![ts, kind, variant, path],
            )
            .unwrap();
        }

        // A decline and a success on the ranked path; only the decline is a fault.
        for route in ["ranked_too_slow", crate::ranked::ROUTE_RANKED] {
            conn.execute(
                "INSERT INTO read_events(ts,tool,path,tokens_returned,full_tokens,\
                 saved_tokens,routed_via,channel) \
                 VALUES('2026-07-04T00:00:00Z','smart_read','c.rs',1,2,1,?1,'cli')",
                [route],
            )
            .unwrap();
        }

        let faults = load_faults_from_db(&conn).expect("reader runs");

        let valve = faults
            .iter()
            .find(|f| f.variant() == "retry_escape_valve")
            .expect("aggregated fault present");
        assert_eq!(valve.count, 2, "occurrences must be summed");
        assert_eq!(valve.first_seen.as_deref(), Some("2026-07-01T00:00:00Z"));
        assert_eq!(valve.last_seen.as_deref(), Some("2026-07-03T00:00:00Z"));

        let declines: Vec<&Fault> = faults
            .iter()
            .filter(|f| f.kind == "ranked_decline")
            .collect();
        assert_eq!(declines.len(), 1, "only the declining route is a fault");
        assert_eq!(declines[0].variant(), "ranked_too_slow");

        assert!(
            faults.iter().all(|f| f.kind != "schema_drift"),
            "a current schema must not report drift"
        );
    }

    #[test]
    fn db_reader_reports_drift_on_a_stale_read_events() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        // A read_events from before the ranked columns landed.
        conn.execute_batch(
            "CREATE TABLE read_events (ts TEXT NOT NULL, tool TEXT NOT NULL, \
             path TEXT NOT NULL, tokens_returned INTEGER NOT NULL, \
             full_tokens INTEGER NOT NULL, saved_tokens INTEGER NOT NULL, \
             routed_via TEXT NOT NULL, channel TEXT NOT NULL DEFAULT 'unknown');\
             CREATE TABLE faults (ts TEXT NOT NULL, kind TEXT NOT NULL, \
             variant TEXT NOT NULL DEFAULT '-', path TEXT, lines INTEGER, detail TEXT, \
             session_id TEXT, version TEXT, channel TEXT NOT NULL DEFAULT 'unknown');",
        )
        .unwrap();

        let faults = load_faults_from_db(&conn).expect("reader runs");
        let drift = faults
            .iter()
            .find(|f| f.kind == "schema_drift")
            .expect("drift detected");
        assert!(
            drift
                .detail
                .as_deref()
                .unwrap_or_default()
                .contains("8 columns"),
            "detail should name the live count: {:?}",
            drift.detail
        );
    }

    #[test]
    fn include_source_embeds_workspace_files_and_never_redacted_ones() {
        let faults = vec![
            Fault {
                kind: "hook_fail_open".into(),
                guard: Some("retry_escape_valve".into()),
                path: Some("crates/lumen-core/tests/fixtures/faults_empty.json".into()),
                ..Default::default()
            },
            Fault {
                kind: "hook_fail_open".into(),
                guard: Some("retry_escape_valve".into()),
                path: Some("/Users/testuser/dev/acme-billing/src/invoice.ts".into()),
                ..Default::default()
            },
        ];
        let mut env = test_env();
        env.workspace_root = workspace_root();

        let embedded = embedded_sources(&faults, &env);
        assert_eq!(embedded.len(), 1, "only the in-workspace file is eligible");
        assert!(embedded[0].0.ends_with("faults_empty.json"));

        let body = render(
            &faults,
            &env,
            &RenderOpts {
                include_source: true,
            },
        )
        .unwrap();
        assert!(body.contains("<details><summary>source:"));
        assert!(
            !body.contains("acme"),
            "--include-source must not reach an out-of-workspace file"
        );

        // Default stays metadata-only.
        let plain = render(&faults, &env, &RenderOpts::default()).unwrap();
        assert!(!plain.contains("<details>"));
    }

    #[test]
    fn title_is_the_headline_without_the_heading_marker() {
        let body = render(
            &load("faults_mixed.json"),
            &test_env(),
            &RenderOpts::default(),
        )
        .unwrap();
        let title = title_from(&body);
        assert!(title.starts_with("lumen 1.4.0 —"), "got: {title}");
        assert!(!title.contains('#'));
    }

    #[test]
    fn the_marker_in_the_body_is_the_dedupe_key() {
        let faults = load("faults_mixed.json");
        let env = test_env();
        let body = render(&faults, &env, &RenderOpts::default()).unwrap();
        assert!(
            body.contains(&marker(&fingerprint(&faults, &env))),
            "find_existing looks for exactly this string"
        );
    }

    #[test]
    fn malformed_fixture_names_the_file() {
        let err = load_faults(&fixture_dir().join("does_not_exist.json")).unwrap_err();
        assert!(
            err.contains("does_not_exist.json"),
            "error lost the path: {err}"
        );
    }
    // ── Filing fallback chain ───────────────────────────────────────────────────
    //
    // gh first, then the REST API, then a prefilled browser form. The chain exists
    // because `gh` is an undocumented dependency that virtually no end user has, so a
    // filing path that only tries it works for the maintainer and nobody else.
    //
    // These test the parts that do not need a network: the token source, the encoder the
    // browser route depends on, and that Handoff is a distinct outcome from Created.
    // The routes themselves are exercised by hand against a scratch repo.

    #[test]
    fn a_handoff_is_not_a_filing() {
        // The browser route opens a form; nothing is published until a human submits.
        // Callers match on this, so it must never collapse into Created.
        assert_ne!(
            Filed::Handoff("https://x/issues/new".into()),
            Filed::Created("https://x/issues/new".into())
        );
    }

    #[test]
    fn percent_encoding_escapes_everything_a_report_contains() {
        // A rendered body is full of newlines, spaces, pipes, hashes and backticks; any
        // one of them unescaped truncates the prefilled body at that character.
        assert_eq!(percent_encode("a b"), "a%20b");
        assert_eq!(percent_encode("x\ny"), "x%0Ay");
        assert_eq!(percent_encode("### t"), "%23%23%23%20t");
        assert_eq!(percent_encode("a|b"), "a%7Cb");
        assert_eq!(percent_encode("`c`"), "%60c%60");
        assert_eq!(percent_encode("&q=1"), "%26q%3D1");
        // Unreserved characters must survive, or every URL doubles in length.
        assert_eq!(percent_encode("Aa0-_.~"), "Aa0-_.~");
        // Multi-byte input is encoded per UTF-8 byte.
        assert_eq!(percent_encode("≥"), "%E2%89%A5");
    }

    #[test]
    fn a_marker_survives_a_round_trip_through_the_url_encoder() {
        // The dedupe key rides in the body. If the encoder mangled it, a browser-filed
        // issue would never be found again and every report would open a duplicate.
        let m = marker("ffd15312");
        let encoded = percent_encode(&m);
        assert!(
            !encoded.contains('<'),
            "raw angle bracket would break the query"
        );
        let decoded: String = {
            let b = encoded.as_bytes();
            let mut out = Vec::new();
            let mut i = 0;
            while i < b.len() {
                if b[i] == b'%' {
                    out.push(
                        u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap(), 16)
                            .unwrap(),
                    );
                    i += 3;
                } else {
                    out.push(b[i]);
                    i += 1;
                }
            }
            String::from_utf8(out).unwrap()
        };
        assert_eq!(decoded, m);
    }

    #[test]
    fn the_token_comes_only_from_the_environment() {
        // SAFETY: single-threaded within this test, and no other test reads these.
        unsafe {
            std::env::remove_var("GITHUB_TOKEN");
            std::env::remove_var("GH_TOKEN");
        }
        assert!(api_token().is_none(), "no token must mean no api route");

        unsafe { std::env::set_var("GH_TOKEN", "   ") };
        assert!(api_token().is_none(), "whitespace is not a token");

        unsafe { std::env::set_var("GH_TOKEN", " abc ") };
        assert_eq!(api_token().as_deref(), Some("abc"), "trimmed");

        // GITHUB_TOKEN is checked first.
        unsafe { std::env::set_var("GITHUB_TOKEN", "primary") };
        assert_eq!(api_token().as_deref(), Some("primary"));

        unsafe {
            std::env::remove_var("GITHUB_TOKEN");
            std::env::remove_var("GH_TOKEN");
        }
    }

    /// A prefilled URL for the current report shape must stay under the cap, or the
    /// browser route silently degrades to a blank form on every single call.
    #[test]
    fn a_typical_report_fits_in_a_prefilled_url() {
        let body = render(
            &load("faults_mixed.json"),
            &test_env(),
            &RenderOpts::default(),
        )
        .unwrap();
        let url = format!(
            "https://github.com/{DEFAULT_REPO}/issues/new?title={}&body={}",
            percent_encode(&title_from(&body)),
            percent_encode(&body)
        );
        assert!(
            url.len() <= PREFILL_URL_CAP,
            "prefill URL is {} bytes, over the {PREFILL_URL_CAP} cap",
            url.len()
        );
    }

    /// The failure message has to name every route. "Filing failed" with no history is
    /// unfixable, and the last error is usually the least informative one.
    #[test]
    fn total_failure_reports_every_route_it_tried() {
        // Endpoints, not the environment. The first version cleared PATH so nothing would
        // resolve — which works on Unix and not on Windows, where CreateProcess finds
        // `cmd` in System32 whatever PATH says. The browser route then succeeded, the
        // `unwrap_err` panicked, and the failure looked like a bug in the chain rather
        // than in how the test forced it.
        let ep = Endpoints {
            api_base: "http://127.0.0.1:1".into(),
            web_base: "http://127.0.0.1:1".into(),
            gh_cmd: Some(vec!["lumen-no-such-gh-binary".into()]),
            open_cmd: Some(vec!["lumen-no-such-opener".into()]),
            token: None,
        };

        let err = file_issue_with(&ep, "owner/repo", "t", "b", "deadbeef").unwrap_err();

        assert!(err.contains("every filing route failed"), "{err}");
        for route in ["gh:", "api:", "browser:"] {
            assert!(err.contains(route), "missing {route} in: {err}");
        }
        assert!(
            err.contains("GITHUB_TOKEN") || err.contains("GH_TOKEN"),
            "the api failure should say what was missing: {err}"
        );
    }
    /// The test that would have caught it: the first fault report ever filed from the app
    /// read "Unclassified fault" because the daemon's kinds were added without teaching
    /// the renderer about them. A kind that reaches a user unclassified is a bug.
    #[test]
    fn every_emitted_kind_is_classified() {
        for kind in FAULT_KINDS {
            assert_ne!(
                impact(kind),
                UNCLASSIFIED,
                "{kind} has no impact line — it would render as an unclassified fault"
            );
            assert!(
                kind_priority(kind) < 6,
                "{kind} falls into the catch-all priority and would sort below everything"
            );
            let f = Fault {
                kind: (*kind).to_string(),
                ..Default::default()
            };
            let body = render(&[f], &test_env(), &RenderOpts::default())
                .unwrap_or_else(|| panic!("{kind} rendered nothing"));
            assert!(
                !body.contains(UNCLASSIFIED),
                "{kind} still renders the unclassified impact line"
            );
        }
    }

    /// Priorities must be distinct, or the table order is decided by count alone and the
    /// headline picks an arbitrary kind.
    #[test]
    fn every_kind_has_its_own_priority() {
        let mut seen = std::collections::BTreeMap::new();
        for kind in FAULT_KINDS {
            if let Some(other) = seen.insert(kind_priority(kind), *kind) {
                panic!("{kind} and {other} share priority {}", kind_priority(kind));
            }
        }
    }

    /// Redaction must not depend on the machine rendering the report.
    ///
    /// This is the bug CI found on windows-latest: `Path::is_relative` called a
    /// Unix-style absolute path "relative" for want of a drive letter, so every
    /// out-of-workspace path was published verbatim. Asserted through the string
    /// classifier so it holds on any host, not just the one that happened to be wrong.
    #[test]
    fn absoluteness_is_decided_the_same_way_on_every_platform() {
        for abs in [
            "/Users/me/clients/acme/invoice.ts",
            "/etc/passwd",
            "\\\\server\\share\\file.rs",
            "C:\\Users\\me\\clients\\acme\\invoice.ts",
            "c:/Users/me/x.rs",
        ] {
            assert!(is_absolute_anywhere(abs), "{abs} must count as absolute");
        }
        for rel in [
            "crates/lumen-core/src/report.rs",
            "a.rs",
            "./a.rs",
            "sub\\dir\\a.rs",
        ] {
            assert!(!is_absolute_anywhere(rel), "{rel} must count as relative");
        }
    }

    #[test]
    fn an_out_of_workspace_path_is_redacted_in_either_path_style() {
        let env = test_env();
        for raw in [
            "/Users/testuser/dev/acme-billing/src/invoice.ts",
            "C:\\Users\\testuser\\dev\\acme-billing\\src\\invoice.ts",
        ] {
            let label = redact_path(raw, &env);
            assert!(
                !label.contains("acme"),
                "{raw} leaked a private directory name as {label}"
            );
            assert!(label.starts_with("<redacted:external>"), "got {label}");
        }
    }
}
