//! `lumen-tok` is the boundary between "measured" and "estimated" in the ledger.
//!
//! Its contract is an exit code, because the caller is a shell script that cannot
//! inspect anything richer:
//!
//!   0  a count is on stdout
//!   3  the input is not text, so no count exists
//!   1  stdin was unreadable
//!
//! Before 1.2.1 there was no code 3. `read_to_string().expect()` panicked on the
//! first invalid UTF-8 byte, the meter read the nonzero status as "the tokenizer is
//! broken", and substituted bytes/4 — which overstates a PNG by roughly 40x. Those
//! fabricated counts then fed the missed-optimization metric.

use std::io::Write;
use std::process::{Command, Stdio};

/// Run lumen-tok with `input` on stdin. Returns (exit code, stdout).
fn run(input: &[u8]) -> (i32, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_lumen-tok"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn lumen-tok");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(input)
        .expect("write stdin");
    let out = child.wait_with_output().expect("wait");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
    )
}

#[test]
fn text_gets_a_count_and_a_zero_exit() {
    let (code, stdout) = run(b"fn main() {}\n");
    assert_eq!(code, 0, "text input must succeed");
    let n: u64 = stdout.parse().expect("stdout must be a bare integer");
    assert!(
        n > 0,
        "a non-empty file has a positive token count, got {n}"
    );
}

#[test]
fn empty_input_is_text_and_counts_zero() {
    let (code, stdout) = run(b"");
    assert_eq!(code, 0, "empty input is valid UTF-8, not a failure");
    assert_eq!(stdout, "0");
}

/// The PNG magic number followed by bytes that cannot be UTF-8.
#[test]
fn binary_input_reports_unsupported_rather_than_a_number() {
    let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    png.extend_from_slice(&[0xFF, 0xFE, 0xFD, 0xC0, 0x80, 0x00, 0x91]);

    let (code, stdout) = run(&png);
    assert_eq!(
        code, 3,
        "binary input must exit 3 (not text), not panic and not 0"
    );
    assert!(
        stdout.is_empty(),
        "nothing may go to stdout, or the caller would read it as a count: {stdout:?}"
    );
}

/// A lone continuation byte — the minimal invalid UTF-8 sequence.
#[test]
fn a_single_invalid_byte_is_enough_to_report_unsupported() {
    let (code, stdout) = run(&[0x80]);
    assert_eq!(code, 3);
    assert!(stdout.is_empty());
}

/// Multi-byte UTF-8 must not be mistaken for binary. This is the negative control
/// for the check above: a validity test that rejected all non-ASCII would pass
/// every test in this file except this one.
#[test]
fn valid_multibyte_utf8_is_still_text() {
    let (code, stdout) = run("héllo wörld — ﬁne\nпривет\n日本語\n".as_bytes());
    assert_eq!(code, 0, "valid UTF-8 must count, whatever its byte width");
    let n: u64 = stdout.parse().expect("a count");
    assert!(n > 0);
}

/// The count must be the tokenizer's, not a byte-derived stand-in. A 4-byte-per-token
/// estimate of this input would land near 25; real BPE on repeated words is far lower.
#[test]
fn the_count_is_bpe_and_not_bytes_over_four() {
    let text = "token token token token token token token token token token\n";
    let (code, stdout) = run(text.as_bytes());
    assert_eq!(code, 0);
    let n: u64 = stdout.parse().unwrap();
    let bytes_over_four = (text.len() / 4) as u64;
    assert_ne!(
        n, bytes_over_four,
        "a BPE count of repeated words must not coincide with bytes/4"
    );
}
