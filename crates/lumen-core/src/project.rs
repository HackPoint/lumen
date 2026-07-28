//! Identifying which project and which agent a transcript belongs to.
//!
//! Claude Code lays transcripts out as:
//!
//! ```text
//! ~/.claude/projects/<encoded-project-dir>/<session-uuid>.jsonl
//! ~/.claude/projects/<encoded-project-dir>/<session-uuid>/subagents/agent-<id>.jsonl
//! ```
//!
//! Two facts drive this module:
//!
//! 1. **Subagent transcripts reuse the parent's `sessionId`.** Their turns
//!    therefore fold into the parent session, and because a subagent starts with
//!    a fresh, small context, treating its `cache_read` as the session's context
//!    fill makes the gauge dip while the subagent runs. Subagent tokens are real
//!    spend and must stay in cost totals — but they are not your context.
//!
//! 2. **The project directory is the only project identity available.** The
//!    transcript records no `cwd`, so the encoded directory name is all we have.

use std::path::{Path, PathBuf};

/// Does this transcript path belong to a subagent rather than the main agent?
///
/// Subagent transcripts live under a `subagents/` directory inside the parent
/// session's own directory.
pub fn is_subagent_path(path: &str) -> bool {
    // Match the separator on both sides so a project literally named
    // "subagents" cannot be mistaken for one.
    path.contains("/subagents/") || path.contains("\\subagents\\")
}

/// The encoded project directory from a transcript path, e.g.
/// `-Users-me-dev-lumen` from `~/.claude/projects/-Users-me-dev-lumen/x.jsonl`.
///
/// Returns None if the path is not under a `projects/` directory.
pub fn encoded_project_dir(path: &str) -> Option<&str> {
    let p = path.replace('\\', "/");
    let idx = p.find("/projects/")?;
    let rest = &path[idx + "/projects/".len()..];
    let end = rest.find(['/', '\\'])?;
    Some(&rest[..end])
}

/// Decode an encoded project directory back into a filesystem path.
///
/// Claude Code encodes the project's absolute path by replacing every `/` with
/// `-`, which is **lossy**: a directory whose own name contains a dash is
/// indistinguishable from a separator (`ai-workspace` vs `ai/workspace`). We
/// disambiguate by asking the filesystem, accumulating segments until a
/// candidate directory actually exists.
///
/// `exists` is injected so this is testable without touching the real disk.
pub fn decode_project_dir(encoded: &str, exists: &dyn Fn(&Path) -> bool) -> PathBuf {
    let mut resolved = PathBuf::from("/");
    let mut pending = String::new();

    for segment in encoded.trim_start_matches('-').split('-') {
        let candidate = if pending.is_empty() {
            segment.to_string()
        } else {
            format!("{pending}-{segment}")
        };
        if exists(&resolved.join(&candidate)) {
            resolved.push(candidate);
            pending.clear();
        } else {
            // Not a directory yet — the dash was part of a name, keep going.
            pending = candidate;
        }
    }

    // Anything left never resolved (project moved or deleted); keep it verbatim
    // so the label is still recognisable rather than silently truncated.
    if !pending.is_empty() {
        resolved.push(pending);
    }
    resolved
}

/// A short, human label for a project — the last path component.
///
/// Falls back to the encoded directory with its leading dashes stripped, so a
/// project that no longer exists on disk still gets a usable name.
pub fn project_label(encoded: &str, exists: &dyn Fn(&Path) -> bool) -> String {
    let decoded = decode_project_dir(encoded, exists);
    decoded
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| encoded.trim_start_matches('-').to_string())
}

/// [`project_label`] against the real filesystem.
pub fn project_label_on_disk(encoded: &str) -> String {
    project_label(encoded, &|p: &Path| p.is_dir())
}

/// The project label for a transcript path, or None if it has no project dir.
pub fn label_for_transcript(path: &str) -> Option<String> {
    encoded_project_dir(path).map(project_label_on_disk)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// A fake filesystem: only the listed directories exist.
    fn fs(dirs: &[&str]) -> impl Fn(&Path) -> bool + use<> {
        let set: HashSet<PathBuf> = dirs.iter().map(PathBuf::from).collect();
        move |p: &Path| set.contains(p)
    }

    // ── is_subagent_path ─────────────────────────────────────────────────────

    #[test]
    fn a_subagents_directory_marks_a_subagent_transcript() {
        assert!(is_subagent_path(
            "/h/.claude/projects/-p/abc-123/subagents/agent-deadbeef.jsonl"
        ));
    }

    #[test]
    fn a_main_session_transcript_is_not_a_subagent() {
        assert!(!is_subagent_path("/h/.claude/projects/-p/abc-123.jsonl"));
    }

    #[test]
    fn a_project_named_subagents_is_not_mistaken_for_one() {
        // The separators are part of the match, so a project directory whose
        // name merely contains "subagents" does not trip it.
        assert!(!is_subagent_path(
            "/h/.claude/projects/-Users-me-subagents/abc.jsonl"
        ));
        assert!(!is_subagent_path("/h/subagents-tooling/abc.jsonl"));
    }

    #[test]
    fn windows_separators_are_handled() {
        assert!(is_subagent_path(
            r"C:\Users\me\.claude\projects\-p\abc\subagents\agent-1.jsonl"
        ));
    }

    // ── encoded_project_dir ──────────────────────────────────────────────────

    #[test]
    fn the_project_dir_comes_from_after_projects() {
        assert_eq!(
            encoded_project_dir("/h/.claude/projects/-Users-me-dev-lumen/abc.jsonl"),
            Some("-Users-me-dev-lumen")
        );
    }

    #[test]
    fn a_subagent_path_still_yields_its_project_dir() {
        assert_eq!(
            encoded_project_dir("/h/.claude/projects/-Users-me-dev-lumen/abc/subagents/a.jsonl"),
            Some("-Users-me-dev-lumen")
        );
    }

    #[test]
    fn a_path_outside_projects_has_no_project_dir() {
        assert_eq!(encoded_project_dir("/tmp/session.jsonl"), None);
        assert_eq!(encoded_project_dir(""), None);
    }

    // ── decode_project_dir ───────────────────────────────────────────────────

    #[test]
    fn a_path_with_no_dashes_in_any_name_decodes_directly() {
        let exists = fs(&[
            "/Users",
            "/Users/me",
            "/Users/me/dev",
            "/Users/me/dev/lumen",
        ]);
        assert_eq!(
            decode_project_dir("-Users-me-dev-lumen", &exists),
            PathBuf::from("/Users/me/dev/lumen")
        );
    }

    #[test]
    fn a_directory_whose_name_contains_a_dash_is_resolved_by_the_filesystem() {
        // The ambiguity this whole function exists for: "ai-workspace" could be
        // "ai/workspace". Only the filesystem can say.
        let exists = fs(&[
            "/Users",
            "/Users/me",
            "/Users/me/dev",
            "/Users/me/dev/ai-workspace",
            "/Users/me/dev/ai-workspace/lumen",
        ]);
        assert_eq!(
            decode_project_dir("-Users-me-dev-ai-workspace-lumen", &exists),
            PathBuf::from("/Users/me/dev/ai-workspace/lumen")
        );
    }

    #[test]
    fn several_dashed_components_all_resolve() {
        // Real case from the shipped DB: -Users-x-dev-speedata-ws-datapulse-gitlab
        let exists = fs(&[
            "/Users",
            "/Users/x",
            "/Users/x/dev",
            "/Users/x/dev/speedata-ws",
            "/Users/x/dev/speedata-ws/datapulse-gitlab",
        ]);
        assert_eq!(
            decode_project_dir("-Users-x-dev-speedata-ws-datapulse-gitlab", &exists),
            PathBuf::from("/Users/x/dev/speedata-ws/datapulse-gitlab")
        );
    }

    #[test]
    fn an_unresolvable_tail_is_kept_verbatim() {
        // Project deleted or moved: keep what we could not resolve rather than
        // dropping it, so the label is still recognisable.
        let exists = fs(&["/Users", "/Users/me"]);
        assert_eq!(
            decode_project_dir("-Users-me-gone-project", &exists),
            PathBuf::from("/Users/me/gone-project")
        );
    }

    #[test]
    fn nothing_resolvable_still_produces_a_path() {
        let exists = fs(&[]);
        assert_eq!(
            decode_project_dir("-a-b-c", &exists),
            PathBuf::from("/a-b-c")
        );
    }

    #[test]
    fn an_empty_encoding_does_not_panic() {
        let exists = fs(&[]);
        assert_eq!(decode_project_dir("", &exists), PathBuf::from("/"));
    }

    // ── project_label ────────────────────────────────────────────────────────

    #[test]
    fn the_label_is_the_last_path_component() {
        let exists = fs(&[
            "/Users",
            "/Users/me",
            "/Users/me/dev",
            "/Users/me/dev/ai-workspace",
            "/Users/me/dev/ai-workspace/lumen",
        ]);
        assert_eq!(
            project_label("-Users-me-dev-ai-workspace-lumen", &exists),
            "lumen"
        );
    }

    #[test]
    fn a_dashed_project_name_survives_in_the_label() {
        let exists = fs(&[
            "/Users",
            "/Users/x",
            "/Users/x/dev",
            "/Users/x/dev/datapulse-gitlab",
        ]);
        assert_eq!(
            project_label("-Users-x-dev-datapulse-gitlab", &exists),
            "datapulse-gitlab",
            "the label must not be truncated to just 'gitlab'"
        );
    }

    #[test]
    fn an_unresolvable_project_still_gets_a_label() {
        let exists = fs(&[]);
        assert_eq!(
            project_label("-Users-me-dev-lumen", &exists),
            "Users-me-dev-lumen"
        );
    }

    #[test]
    fn an_empty_encoding_labels_as_empty_rather_than_panicking() {
        let exists = fs(&[]);
        assert_eq!(project_label("", &exists), "");
    }

    // ── label_for_transcript ─────────────────────────────────────────────────

    #[test]
    fn a_transcript_outside_projects_has_no_label() {
        assert_eq!(label_for_transcript("/tmp/x.jsonl"), None);
    }

    #[test]
    fn a_transcript_under_projects_gets_a_label() {
        // Resolves against the real filesystem, so assert only that it is
        // non-empty and dash-free at the end — the machine's layout is unknown.
        let label = label_for_transcript("/h/.claude/projects/-nonexistent-project-xyz/a.jsonl");
        assert_eq!(label.as_deref(), Some("nonexistent-project-xyz"));
    }
}
