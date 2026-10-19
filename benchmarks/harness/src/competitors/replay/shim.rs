//! PATH shims that replay a recorded command's output.
//!
//! RTK and squeez wrap a command: they run the real program and filter what it printed. To test
//! the filter on the recorded bytes, the program they run must print exactly those bytes and exit
//! with the recorded status, so a shim named after the program stands first on `PATH`. It writes
//! the recorded file to stdout unchanged (a `cat`, so CRLF, a missing final newline and invalid
//! UTF-8 all survive) and exits with the recorded code.
//!
//! A wrapper also runs the program for its own reasons (RTK asks `git rev-parse --show-toplevel`
//! before every command). A shim answers only the invocation it was made for, the one whose first
//! argument is the recorded subcommand; any other call runs the real program, found on the PATH
//! without the shim directory. Every call, answered or passed on, is appended to a log, so a run
//! can tell a wrapper that really called the shim from one that never did.
//!
//! The shims are POSIX `sh` scripts. Windows has none: a replay there is `n/a`.

use crate::util::{Ctx, Res};
use std::path::{Path, PathBuf};

/// One replayed command.
#[derive(Debug, Clone)]
pub struct Shim {
    /// The program name, as the command line says it (`git`, `cargo`, `curl`).
    pub program: String,
    /// The first argument that selects the recorded invocation (`ls-tree`, `show`, `test`). `None`
    /// answers every call.
    pub first_arg: Option<String>,
    /// The recorded stdout, replayed byte for byte.
    pub stdout: Vec<u8>,
    /// The recorded exit status.
    pub exit_code: i32,
}

/// Where the shims and their log live.
#[derive(Debug, Clone)]
pub struct ShimDir {
    pub dir: PathBuf,
    pub log: PathBuf,
}

/// What the shims saw: one entry per call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShimCall {
    pub program: String,
    pub answered: bool,
    pub args: String,
}

/// Single-quote `s` for `sh`.
pub fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// A program name that is safe to use as a file name and a word of shell.
fn plain_program(p: &str) -> bool {
    !p.is_empty()
        && p.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        && p != "."
        && p != ".."
}

impl ShimDir {
    /// Write the shims of `shims` under `dir` (created). `real_path` is the `PATH` an unanswered
    /// call runs the real program with; it must not contain `dir`.
    pub fn create(dir: &Path, shims: &[Shim], real_path: &str) -> Res<ShimDir> {
        if cfg!(windows) {
            return Err("replay shims are POSIX sh scripts: n/a on Windows".to_string());
        }
        std::fs::create_dir_all(dir).ctx(&format!("create {}", dir.display()))?;
        let log = dir.join("calls.log");
        std::fs::write(&log, b"").ctx("create the shim log")?;
        for (i, s) in shims.iter().enumerate() {
            if !plain_program(&s.program) {
                return Err(format!(
                    "{:?} is not a program name a shim can take",
                    s.program
                ));
            }
            let data = dir.join(format!("recorded-{i}.out"));
            std::fs::write(&data, &s.stdout).ctx("write the recorded output")?;
            let cond = match &s.first_arg {
                Some(a) => format!("[ \"$1\" = {} ]", quote(a)),
                None => "true".to_string(),
            };
            let script = format!(
                "#!/bin/sh\n\
                 # Replays recorded output; see competitors/replay/shim.rs.\n\
                 if {cond}; then\n\
                 \tprintf 'answered\\t%s\\t%s\\n' {prog} \"$*\" >> {log}\n\
                 \tcat {data}\n\
                 \texit {code}\n\
                 fi\n\
                 printf 'passed\\t%s\\t%s\\n' {prog} \"$*\" >> {log}\n\
                 PATH={real} exec {prog} \"$@\"\n",
                prog = quote(&s.program),
                log = quote(&log.to_string_lossy()),
                data = quote(&data.to_string_lossy()),
                code = s.exit_code,
                real = quote(real_path),
            );
            let path = dir.join(&s.program);
            std::fs::write(&path, script).ctx(&format!("write the {} shim", s.program))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                    .ctx("make the shim executable")?;
            }
        }
        Ok(ShimDir {
            dir: dir.to_path_buf(),
            log,
        })
    }

    /// `PATH` with the shim directory first.
    pub fn path_with(&self, rest: &str) -> String {
        let sep = if cfg!(windows) { ";" } else { ":" };
        format!("{}{sep}{rest}", self.dir.display())
    }

    /// The calls the shims have seen so far.
    pub fn calls(&self) -> Res<Vec<ShimCall>> {
        let text = std::fs::read_to_string(&self.log).ctx("read the shim log")?;
        Ok(text
            .lines()
            .filter_map(|l| {
                let mut f = l.splitn(3, '\t');
                let kind = f.next()?;
                Some(ShimCall {
                    answered: kind == "answered",
                    program: f.next()?.to_string(),
                    args: f.next().unwrap_or("").to_string(),
                })
            })
            .collect())
    }

    /// How many calls a shim answered with the recorded output.
    pub fn answered(&self) -> Res<usize> {
        Ok(self.calls()?.iter().filter(|c| c.answered).count())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::process::Command;

    fn run(dir: &ShimDir, args: &str) -> (Vec<u8>, i32) {
        let path = dir.path_with("/usr/bin:/bin");
        let out = Command::new("sh")
            .arg("-c")
            .arg(args)
            .env_clear()
            .env("PATH", path)
            .output()
            .unwrap();
        (out.stdout, out.status.code().unwrap())
    }

    fn shim(program: &str, first: Option<&str>, stdout: &[u8], code: i32) -> Shim {
        Shim {
            program: program.into(),
            first_arg: first.map(Into::into),
            stdout: stdout.to_vec(),
            exit_code: code,
        }
    }

    #[test]
    fn a_shim_replays_the_exact_bytes_and_the_exit_code() {
        // CRLF, no final newline, a NUL, a quote and invalid UTF-8: none may change.
        let tricky: Vec<u8> = b"a\r\nb 'q' \"d\" $x `y`\n\0\xff\xfe tail".to_vec();
        let t = tempfile::tempdir().unwrap();
        let d = ShimDir::create(
            &t.path().join("shims"),
            &[shim("rec-prog", None, &tricky, 3)],
            "/usr/bin:/bin",
        )
        .unwrap();
        let (out, code) = run(&d, "rec-prog --anything 'with args'");
        assert_eq!(out, tricky);
        assert_eq!(code, 3, "a non-zero status is the recorded one");
        // Zero, and an empty recording, are exact too.
        let d0 = ShimDir::create(
            &t.path().join("z"),
            &[shim("rec-zero", None, b"", 0)],
            "/bin",
        )
        .unwrap();
        assert_eq!(run(&d0, "rec-zero"), (Vec::new(), 0));
        assert_eq!(d.answered().unwrap(), 1);
    }

    #[test]
    fn a_shim_answers_only_its_subcommand_and_passes_the_rest_to_the_real_program() {
        let t = tempfile::tempdir().unwrap();
        let d = ShimDir::create(
            &t.path().join("s"),
            // `basename` stands in for `git`: a real program (not a shell builtin) that prints
            // something checkable.
            &[shim("basename", Some("show"), b"RECORDED\n", 7)],
            "/bin:/usr/bin",
        )
        .unwrap();
        let (out, code) = run(&d, "basename show --stat");
        assert_eq!((out.as_slice(), code), (b"RECORDED\n".as_slice(), 7));
        let (out, code) = run(&d, "basename /a/b/toplevel");
        assert_eq!((out.as_slice(), code), (b"toplevel\n".as_slice(), 0));
        let calls = d.calls().unwrap();
        assert_eq!(
            calls,
            vec![
                ShimCall {
                    program: "basename".into(),
                    answered: true,
                    args: "show --stat".into()
                },
                ShimCall {
                    program: "basename".into(),
                    answered: false,
                    args: "/a/b/toplevel".into()
                },
            ]
        );
    }

    #[test]
    fn a_program_name_cannot_escape_the_shim_directory() {
        let t = tempfile::tempdir().unwrap();
        for bad in ["../x", "a/b", "", "..", "a b", "a;b"] {
            let e = ShimDir::create(&t.path().join("s"), &[shim(bad, None, b"", 0)], "/bin");
            assert!(e.is_err(), "{bad:?} must be refused");
        }
    }

    #[test]
    fn quoting_survives_a_single_quote() {
        assert_eq!(quote("it's"), "'it'\\''s'");
    }
}
