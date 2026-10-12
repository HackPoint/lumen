//! Error handling and small process helpers shared by every module.
//!
//! One rule runs through the harness: a step that fails stops the run with a message that says
//! which step. Nothing here substitutes a default for a failure, because a benchmark that
//! records a plausible number for something that did not happen is worse than no benchmark.

use std::fmt::Display;
use std::process::Command;

pub type Res<T> = Result<T, String>;

/// Adds the name of the step to an error.
pub trait Ctx<T> {
    fn ctx(self, what: &str) -> Res<T>;
}

impl<T, E: Display> Ctx<T> for Result<T, E> {
    fn ctx(self, what: &str) -> Res<T> {
        self.map_err(|e| format!("{what}: {e}"))
    }
}

/// Run `cmd`, require success and return its stdout as text.
pub fn run_text(cmd: &mut Command) -> Res<String> {
    let shown = format!("{cmd:?}");
    let out = cmd.output().ctx(&format!("cannot run {shown}"))?;
    if !out.status.success() {
        return Err(format!(
            "{shown} exited with {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    String::from_utf8(out.stdout).ctx(&format!("{shown} printed non-UTF-8"))
}

/// Lowercase hex of a SHA-256 digest of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    let mut s = String::with_capacity(64);
    for b in digest.iter() {
        s.push_str(&format!("{b:02x}"));
    }
    s
}
