//! A read-only view of the real Claude Code config, for tests that must prove nothing touched it.
//!
//! `~/.claude.json` is rewritten by any Claude Code that is running (counters, project lists), so a
//! hash of the file would fail for reasons that are not ours. The keys that say how Claude Code
//! behaves (`mcpServers`, `hooks`, `env`, `permissions`, `model`) are what an arm could change, and
//! those are compared. Shared by the E2E tests and the replay tests.

use serde_json::{Map, Value};
use std::path::{Path, PathBuf};

/// The keys of `.claude.json` an arm could change.
pub const WATCHED_KEYS: [&str; 5] = ["mcpServers", "hooks", "env", "permissions", "model"];

/// The user's home as the environment names it.
pub fn real_home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// The watched keys of `<home>/.claude.json`, or `None` when it is absent or not JSON.
pub fn claude_json_view(home: &Path) -> Option<Value> {
    let text = std::fs::read_to_string(home.join(".claude.json")).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    let mut out = Map::new();
    for k in WATCHED_KEYS {
        if let Some(x) = v.get(k) {
            out.insert(k.to_string(), x.clone());
        }
    }
    Some(Value::Object(out))
}
