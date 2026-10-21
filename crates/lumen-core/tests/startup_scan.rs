//! What the tray and sidecar startup of the Lumen app is claimed to do, held to its source.
//!
//! None of it can be launched from a test: the app needs a display, a tray and a bundle. What can
//! run on every platform CI covers is a scan of the four production files of
//! `lumenator/src-tauri/src` with comments, strings and `#[cfg(test)]` modules blanked out. It
//! enforces exactly this, and nothing wider:
//!
//! 1. The `.setup` closure, and every function of the crate it can reach by naming it, contain no
//!    `?` (or `some!`/`tried!`, the macros that spell it out), except the functions in `EXEMPT`: each returns an error its one caller handles, and the
//!    scan checks that the caller still does.
//! 2. The only `expect` in production code is `Builder::build`'s, in `run`, outside the closure.
//! 3. The log plugin is registered at the top level of the closure with no profile, `cfg` or branch
//!    in front of it, with a file target; its level comes from `log_level_from`, which defaults to
//!    `Warn` in release; a `warn!` line follows; nothing else in the crate installs a logger, and
//!    no manifest caps the `log` level.
//!
//! Not checked, and not claimed: calls through trait objects or function pointers, code outside
//! the crate, a `?` in a function startup does not name, `unwrap` (listed below, not pinned), and
//! that a release binary writes the file. No test launches one, and a `release_max_level_*`
//! feature switched on by a dependency cannot be read from a manifest.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};

const APP_SRC: &str = "lumenator/src-tauri/src";
const APP_MANIFEST: &str = "lumenator/src-tauri/Cargo.toml";
/// The production files of the app. Anything else under `APP_SRC` must be a test-only module.
const APP_FILES: [&str; 7] = [
    "lib.rs",
    "health.rs",
    "setup.rs",
    "main.rs",
    "upgrade.rs",
    "upgrade/autostart.rs",
    "setup/repoint.rs",
];
const SKIP_DIRS: [&str; 6] = [
    ".git",
    "target",
    "node_modules",
    ".angular",
    "dist",
    ".claude",
];
/// The name the report gives the `.setup` closure.
const CLOSURE: &str = "the .setup closure";

fn workspace_root() -> PathBuf {
    let mut dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    loop {
        if dir.join("Cargo.toml").is_file()
            && dir.join("crates").is_dir()
            && dir.join(APP_SRC).is_dir()
        {
            return dir;
        }
        assert!(
            dir.pop(),
            "no workspace root above {}",
            env!("CARGO_MANIFEST_DIR")
        );
    }
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

// ── Lexing: what is code, and what only looks like it ─────────────────────────────────────────

fn is_ident(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

#[derive(PartialEq)]
enum Lit {
    Comment,
    Str,
    Char,
}

/// The end of the string literal starting at `i`, prefixes (`b`, `c`, `r`, `br`, `cr`) included.
fn string_end(b: &[u8], i: usize) -> Option<usize> {
    let n = b.len();
    let mut j = i;
    if matches!(b[j], b'b' | b'c') && matches!(b.get(j + 1), Some(b'"' | b'r')) {
        j += 1;
    }
    if b[j] == b'r' {
        let mut k = j + 1;
        while k < n && b[k] == b'#' {
            k += 1;
        }
        let hashes = k - (j + 1);
        if b.get(k) != Some(&b'"') {
            return None;
        }
        let mut m = k + 1;
        while m < n {
            if b[m] == b'"' && b[m + 1..].iter().take_while(|&&x| x == b'#').count() >= hashes {
                return Some(m + 1 + hashes);
            }
            m += 1;
        }
        return Some(n);
    }
    if b[j] != b'"' {
        return None;
    }
    let mut m = j + 1;
    while m < n {
        match b[m] {
            b'\\' => m += 2,
            b'"' => return Some(m + 1),
            _ => m += 1,
        }
    }
    Some(n)
}

/// The end of the character literal starting at `i`, or `None` for a lifetime or a label.
fn char_end(b: &[u8], i: usize) -> Option<usize> {
    let n = b.len();
    let first = *b.get(i + 1)?;
    if first == b'\\' {
        let mut j = i + 3;
        while j < n && b[j] != b'\'' && b[j] != b'\n' {
            j += 1;
        }
        return (b.get(j) == Some(&b'\'')).then_some(j + 1);
    }
    if first == b'\'' || first == b'\n' {
        return None;
    }
    let width = match first {
        0..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        _ => 4,
    };
    (b.get(i + 1 + width) == Some(&b'\'')).then_some(i + 2 + width)
}

fn literal_at(b: &[u8], i: usize) -> Option<(usize, Lit)> {
    let n = b.len();
    let next = b.get(i + 1).copied();
    match b[i] {
        b'/' if next == Some(b'/') => {
            let end = b[i..].iter().position(|&x| x == b'\n').map_or(n, |p| i + p);
            Some((end, Lit::Comment))
        }
        b'/' if next == Some(b'*') => {
            let (mut depth, mut j) = (1, i + 2);
            while j < n && depth > 0 {
                match (b[j], b.get(j + 1).copied()) {
                    (b'/', Some(b'*')) => {
                        depth += 1;
                        j += 2;
                    }
                    (b'*', Some(b'/')) => {
                        depth -= 1;
                        j += 2;
                    }
                    _ => j += 1,
                }
            }
            Some((j, Lit::Comment))
        }
        b'\'' => char_end(b, i).map(|e| (e, Lit::Char)),
        b'"' => string_end(b, i).map(|e| (e, Lit::Str)),
        b'r' | b'b' | b'c' if i == 0 || !is_ident(b[i - 1]) => {
            string_end(b, i).map(|e| (e, Lit::Str))
        }
        _ => None,
    }
}

fn blank_range(out: &mut [u8], from: usize, to: usize) {
    for x in &mut out[from..to] {
        if *x != b'\n' && *x != b'\r' {
            *x = b' ';
        }
    }
}

/// `raw` with comments and character literals replaced by spaces, and string literals too when
/// `strings` is set. Newlines stay and so does every byte offset, so a position found in one view
/// is the same position in the others.
fn blank(raw: &str, strings: bool) -> Vec<u8> {
    let b = raw.as_bytes();
    let mut out = b.to_vec();
    let mut i = 0;
    while i < b.len() {
        match literal_at(b, i) {
            Some((end, kind)) => {
                if kind != Lit::Str || strings {
                    blank_range(&mut out, i, end);
                }
                i = end.max(i + 1);
            }
            None => i += 1,
        }
    }
    out
}

// ── Small helpers over a blanked view ─────────────────────────────────────────────────────────

fn find(b: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if from > b.len() || needle.is_empty() {
        return None;
    }
    b[from..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| p + from)
}

/// `word` as a whole token: not the middle of a longer identifier.
fn find_word(b: &[u8], word: &str, from: usize) -> Option<usize> {
    let w = word.as_bytes();
    let mut at = from;
    while let Some(p) = find(b, w, at) {
        let before = p == 0 || !is_ident(b[p - 1]);
        let after = b.get(p + w.len()).is_none_or(|&x| !is_ident(x));
        if before && after {
            return Some(p);
        }
        at = p + 1;
    }
    None
}

fn has_word(b: &[u8], word: &str) -> bool {
    find_word(b, word, 0).is_some()
}

/// `word` as a whole token starting exactly at `at`.
fn word_at(b: &[u8], at: usize, word: &str) -> bool {
    let w = word.as_bytes();
    b.get(at..at + w.len()) == Some(w)
        && (at == 0 || !is_ident(b[at - 1]))
        && b.get(at + w.len()).is_none_or(|&x| !is_ident(x))
}

fn skip_ws(b: &[u8], mut i: usize) -> usize {
    while i < b.len() && b[i].is_ascii_whitespace() {
        i += 1;
    }
    i
}

fn ident_end(b: &[u8], mut i: usize) -> usize {
    while i < b.len() && is_ident(b[i]) {
        i += 1;
    }
    i
}

fn prev_non_space(b: &[u8], at: usize) -> Option<u8> {
    b[..at]
        .iter()
        .rev()
        .find(|x| !x.is_ascii_whitespace())
        .copied()
}

fn next_non_space(b: &[u8], at: usize) -> Option<u8> {
    b.get(skip_ws(b, at)).copied()
}

fn line_of(b: &[u8], at: usize) -> usize {
    1 + b[..at.min(b.len())].iter().filter(|&&x| x == b'\n').count()
}

fn text_of(b: &[u8], from: usize, to: usize) -> String {
    String::from_utf8_lossy(&b[from..to]).into_owned()
}

/// The bracket matching the opening one at `open`.
fn matching(b: &[u8], open: usize) -> Option<usize> {
    let (up, down) = match *b.get(open)? {
        b'{' => (b'{', b'}'),
        b'(' => (b'(', b')'),
        b'[' => (b'[', b']'),
        _ => return None,
    };
    let mut depth = 0usize;
    for (k, &c) in b.iter().enumerate().skip(open) {
        if c == up {
            depth += 1;
        } else if c == down {
            depth -= 1;
            if depth == 0 {
                return Some(k);
            }
        }
    }
    None
}

/// Identifier tokens in `from..to`, as byte spans. A run that starts with a digit is a number.
fn idents(b: &[u8], from: usize, to: usize) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut i = from;
    while i < to {
        if is_ident(b[i]) {
            let e = ident_end(b, i).min(to);
            if !b[i].is_ascii_digit() {
                out.push((i, e));
            }
            i = e;
        } else {
            i += 1;
        }
    }
    out
}

/// Braces opened and not yet closed between `open` (a `{`) and `at`: 1 is the top level of the
/// block `open` starts.
fn depth_at(b: &[u8], open: usize, at: usize) -> usize {
    let opened = b[open..at].iter().filter(|&&x| x == b'{').count();
    let closed = b[open..at].iter().filter(|&&x| x == b'}').count();
    opened.saturating_sub(closed)
}

/// Where the statement containing `at` begins: after the previous `;`, `{` or `}` that is not
/// inside parentheses or brackets between the two. `floor` bounds the walk.
fn statement_start(b: &[u8], at: usize, floor: usize) -> usize {
    let mut depth = 0usize;
    let mut k = at;
    while k > floor {
        match b[k - 1] {
            b')' | b']' => depth += 1,
            b'(' | b'[' => depth = depth.saturating_sub(1),
            b';' | b'{' | b'}' if depth == 0 => return k,
            _ => {}
        }
        k -= 1;
    }
    floor
}

/// Where the statement containing `at` ends: the next `;` outside parentheses, brackets and
/// braces, or the end of the enclosing block.
fn statement_end(b: &[u8], at: usize, ceil: usize) -> usize {
    let mut depth = 0usize;
    for (k, &c) in b.iter().enumerate().take(ceil).skip(at) {
        match c {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => {
                if depth == 0 {
                    return k;
                }
                depth -= 1;
            }
            b';' if depth == 0 => return k,
            _ => {}
        }
    }
    ceil
}

// ── A source file, as the scan sees it ────────────────────────────────────────────────────────

struct Source {
    name: String,
    raw: String,
    /// Comments, strings, character literals and test modules blanked.
    code: Vec<u8>,
    /// The same with string literals kept: what the logging checks read.
    text: Vec<u8>,
    /// Comments blanked, strings and test modules kept: where the app's own tests are read.
    with_tests: Vec<u8>,
    /// The files `#[cfg(test)] mod x;` stands for.
    test_mods: Vec<String>,
}

impl Source {
    fn new(name: &str, raw: &str) -> Source {
        let with_tests = blank(raw, false);
        let mut code = blank(raw, true);
        let mut text = with_tests.clone();
        let test_mods = blank_test_items(name, &mut code, &mut text);
        Source {
            name: name.to_string(),
            raw: raw.to_string(),
            code,
            text,
            with_tests,
            test_mods,
        }
    }

    fn at(&self, offset: usize) -> String {
        format!("{}:{}", self.name, line_of(&self.code, offset))
    }
}

fn load(root: &Path) -> Vec<Source> {
    APP_FILES
        .iter()
        .map(|f| Source::new(f, &read(&root.join(APP_SRC).join(f))))
        .collect()
}

/// Blanks every `#[cfg(test)]` module, inline or `mod x;`, and returns the files the latter name.
///
/// A `cfg(test)` on anything else, or any other cfg that mentions `test`, is a shape this scan
/// has not been taught: it stops rather than guess which code is production.
fn blank_test_items(name: &str, code: &mut [u8], text: &mut [u8]) -> Vec<String> {
    let mut files = Vec::new();
    let mut at = 0;
    while let Some(p) = find(code, b"#", at) {
        at = p + 1;
        let open = skip_ws(
            code,
            if code.get(p + 1) == Some(&b'!') {
                p + 2
            } else {
                p + 1
            },
        );
        if code.get(open) != Some(&b'[') {
            continue;
        }
        let Some(close) = matching(code, open) else {
            continue;
        };
        let attr: String = text_of(code, open + 1, close).split_whitespace().collect();
        if !has_word(attr.as_bytes(), "test") {
            continue;
        }
        if attr == "cfg(not(test))" || attr.starts_with("cfg_attr(not(test),") {
            continue;
        }
        assert!(
            attr == "cfg(test)" && code[p + 1] != b'!',
            "{name}: `#[{attr}]` is a test attribute this scan has not been taught. Teach it, \
             rather than let code it cannot place go unscanned."
        );
        // Past any further attributes and a visibility, the item must be a module.
        let mut k = skip_ws(code, close + 1);
        while code.get(k) == Some(&b'#') {
            let inner = skip_ws(code, k + 1);
            k = skip_ws(code, matching(code, inner).map_or(code.len(), |m| m + 1));
        }
        if word_at(code, k, "pub") {
            k = skip_ws(code, k + 3);
            if code.get(k) == Some(&b'(') {
                k = skip_ws(code, matching(code, k).map_or(code.len(), |m| m + 1));
            }
        }
        assert!(
            word_at(code, k, "mod"),
            "{name}: `#[cfg(test)]` on something other than a module, at line {}: teach \
             startup_scan.rs the shape, or move the item into a test module",
            line_of(code, p)
        );
        let id_start = skip_ws(code, k + 3);
        let id_end = ident_end(code, id_start);
        let after = skip_ws(code, id_end);
        let end = match code.get(after) {
            Some(b'{') => matching(code, after).map_or(code.len(), |m| m + 1),
            Some(b';') => {
                files.push(text_of(code, id_start, id_end));
                after + 1
            }
            _ => panic!("{name}: `#[cfg(test)] mod` with neither a body nor a `;`"),
        };
        blank_range(code, p, end);
        blank_range(text, p, end);
        at = end;
    }
    files
}

struct Func {
    name: String,
    open: usize,
    close: usize,
}

/// Every `fn` with a body: free functions, methods and nested functions alike.
fn functions(code: &[u8]) -> Vec<Func> {
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(p) = find_word(code, "fn", at) {
        at = p + 2;
        let s = skip_ws(code, p + 2);
        let e = ident_end(code, s);
        if e == s || s == p + 2 || code[s].is_ascii_digit() {
            continue;
        }
        let mut depth = 0i32;
        let mut k = e;
        while k < code.len() {
            match code[k] {
                b'(' | b'[' => depth += 1,
                b')' | b']' => depth -= 1,
                b'{' | b';' if depth == 0 => break,
                _ => {}
            }
            k += 1;
        }
        if code.get(k) == Some(&b'{')
            && let Some(close) = matching(code, k)
        {
            out.push(Func {
                name: text_of(code, s, e),
                open: k,
                close,
            });
        }
    }
    out
}

/// The body of every `.setup(<closure>)` in the file, as the span of its braces.
fn setup_closures(name: &str, code: &[u8]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(p) = find(code, b".setup", at) {
        at = p + 6;
        let paren = skip_ws(code, p + 6);
        if code.get(paren) != Some(&b'(') {
            continue;
        }
        let mut k = skip_ws(code, paren + 1);
        if word_at(code, k, "move") {
            k = skip_ws(code, k + 4);
        }
        assert!(
            code.get(k) == Some(&b'|'),
            "{name}: `.setup` is given something other than a closure literal, at line {}: \
             teach startup_scan.rs to follow it",
            line_of(code, p)
        );
        let Some(bar) = find(code, b"|", k + 1) else {
            panic!("{name}: a `.setup` closure with no parameter list");
        };
        k = skip_ws(code, bar + 1);
        if code[k..].starts_with(b"->") {
            k = find(code, b"{", k).unwrap_or(k);
        }
        assert!(
            code.get(k) == Some(&b'{'),
            "{name}: a `.setup` closure whose body is not a block, at line {}",
            line_of(code, p)
        );
        let close = matching(code, k)
            .unwrap_or_else(|| panic!("{name}: the `.setup` closure's braces do not balance"));
        out.push((k, close));
    }
    out
}

// ── What the claims are about ─────────────────────────────────────────────────────────────────

/// The macros the app spells `?` with, so that startup code can return early without the
/// operator: each is `match $e { Ok(v) => v, Err(e) => return Err(e) }` or its `Option` twin.
/// The scan reads a call of one as the `?` it replaces.
const TRY_MACROS: [&str; 2] = ["some", "tried"];

/// Where a `?` operator, or a call of a `?` macro, sits in `from..to`. `?Sized` is a bound, not
/// an operator.
fn tries(code: &[u8], from: usize, to: usize) -> Vec<usize> {
    let mut at: Vec<usize> = (from..to)
        .filter(|&i| code[i] == b'?' && !matches!(prev_non_space(code, i), Some(b':' | b'+')))
        .collect();
    at.extend(
        idents(code, from, to)
            .into_iter()
            .filter(|&(s, e)| {
                TRY_MACROS.iter().any(|m| m.as_bytes() == &code[s..e])
                    && next_non_space(code, e) == Some(b'!')
            })
            .map(|(s, _)| s),
    );
    at.sort_unstable();
    at
}

/// Where `expect` and `expect_err` are called in `from..to`, as a method or as `Result::expect`.
fn expects(code: &[u8], from: usize, to: usize) -> Vec<usize> {
    idents(code, from, to)
        .into_iter()
        .filter(|&(s, e)| {
            matches!(&code[s..e], b"expect" | b"expect_err")
                && next_non_space(code, e) == Some(b'(')
                && matches!(prev_non_space(code, s), Some(b'.' | b':'))
        })
        .map(|(s, _)| s)
        .collect()
}

/// Where `.unwrap()` is called in the whole file.
fn unwraps(code: &[u8]) -> Vec<usize> {
    idents(code, 0, code.len())
        .into_iter()
        .filter(|&(s, e)| {
            &code[s..e] == b"unwrap"
                && prev_non_space(code, s) == Some(b'.')
                && next_non_space(code, e) == Some(b'(')
        })
        .map(|(s, _)| s)
        .collect()
}

/// The name of the call whose result `.expect` is called on: `build` in `.build(x).expect(..)`.
fn receiver_call(code: &[u8], expect_at: usize) -> Option<String> {
    let mut k = expect_at;
    while k > 0 && code[k - 1].is_ascii_whitespace() {
        k -= 1;
    }
    k = k.checked_sub(1).filter(|&d| code[d] == b'.')?;
    while k > 0 && code[k - 1].is_ascii_whitespace() {
        k -= 1;
    }
    if k == 0 || code[k - 1] != b')' {
        return None;
    }
    let mut depth = 0usize;
    let mut j = k - 1;
    loop {
        match code[j] {
            b')' => depth += 1,
            b'(' => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            _ => {}
        }
        j = j.checked_sub(1)?;
    }
    let mut e = j;
    while e > 0 && code[e - 1].is_ascii_whitespace() {
        e -= 1;
    }
    let mut s = e;
    while s > 0 && is_ident(code[s - 1]) {
        s -= 1;
    }
    (s < e).then(|| text_of(code, s, e))
}

// ── Claim 1 and 2: `?` and `expect` in startup ────────────────────────────────────────────────

/// How the one caller of an exempt function deals with the error it returns.
#[derive(Clone, Copy)]
enum Handled {
    /// It is the scrutinee of a `match` whose arms neither return, panic nor `?`.
    ByMatch,
    /// The statement it is in turns a failure into a value through this adapter.
    By(&'static str),
    /// The call is wrapped in `some!`/`tried!`, so the error goes up one more level, and the
    /// calling function is itself in `EXEMPT`: its own entry says who deals with it.
    Forwarded,
    /// The function returns an `Option`: its `?` or `some!` is an early `None`, which no caller
    /// can turn into an error that aborts startup (the closure's `?` would not compile on it).
    /// Any caller may use it. The scan checks the return type.
    Option,
}

#[derive(Clone, Copy)]
struct Exempt {
    name: &'static str,
    file: &'static str,
    /// The only body that may call it; `None` is the `.setup` closure itself.
    caller: Option<&'static str>,
    handled: Handled,
    why: &'static str,
}

/// Functions startup reaches that use `?`, or `some!`/`tried!`, which the scan reads as `?`. In
/// each, it hands an error back to a caller that deals with it, so none can abort startup. The
/// scan fails if one stops using `?` (the entry is stale) or if its caller stops dealing with the
/// error. A function with several callers has one entry per caller.
const EXEMPT: [Exempt; 15] = [
    Exempt {
        name: "build_tray_menu_items",
        file: "health.rs",
        caller: None,
        handled: Handled::ByMatch,
        why: "the closure matches its result and degrades to no tray menu on `Err`",
    },
    Exempt {
        name: "savings_summary_with",
        file: "lib.rs",
        caller: Some("refresh_tray_savings"),
        handled: Handled::By("unwrap_or_else"),
        why: "the tray refresh setup spawns logs an `Err` and shows no savings line",
    },
    Exempt {
        name: "stable_binary",
        file: "setup.rs",
        caller: Some("baked_paths"),
        handled: Handled::By("unwrap_or_default"),
        why: "it returns an `Option`, so its `?` is an early `None`, and its caller defaults it",
    },
    Exempt {
        name: "write_atomic",
        file: "setup.rs",
        caller: Some("ensure_scripts_fresh_in"),
        handled: Handled::ByMatch,
        why: "its caller logs a warning on `Err` and leaves the script as it was",
    },
    Exempt {
        name: "write_atomic",
        file: "setup.rs",
        caller: Some("repoint_file"),
        handled: Handled::Forwarded,
        why: "repoint_file hands the error on, and its own caller reports it as a problem",
    },
    Exempt {
        name: "repoint_file",
        file: "setup/repoint.rs",
        caller: Some("repoint_with"),
        handled: Handled::ByMatch,
        why: "repoint_with records an `Err` as a problem and goes on to the next file",
    },
    Exempt {
        name: "settle",
        file: "upgrade/autostart.rs",
        caller: Some("adopt_other_entries"),
        handled: Handled::ByMatch,
        why: "adopt_other_entries records an `Err` as a problem and goes on to the next entry",
    },
    Exempt {
        name: "write_run_json",
        file: "upgrade/autostart.rs",
        caller: Some("settle"),
        handled: Handled::Forwarded,
        why: "settle hands the error on, and its own caller records it as a problem",
    },
    Exempt {
        name: "scan_registry",
        file: "upgrade/autostart.rs",
        caller: Some("scan"),
        handled: Handled::ByMatch,
        why: "scan passes the error on, and adopt_other_entries reports it as a problem",
    },
    Exempt {
        name: "tail_inside",
        file: "setup/repoint.rs",
        caller: None,
        handled: Handled::Option,
        why: "an early `None` means the value is not inside the moved folder",
    },
    Exempt {
        name: "moved_path",
        file: "setup/repoint.rs",
        caller: None,
        handled: Handled::Option,
        why: "an early `None` means the value is not inside the moved folder",
    },
    Exempt {
        name: "parse_value",
        file: "upgrade/autostart.rs",
        caller: None,
        handled: Handled::Option,
        why: "an early `None` means the text is not a plist Lumen can read, so not its entry",
    },
    Exempt {
        name: "parse_plist",
        file: "upgrade/autostart.rs",
        caller: None,
        handled: Handled::Option,
        why: "an early `None` means the text is not a plist Lumen can read, so not its entry",
    },
    Exempt {
        name: "agent_of",
        file: "upgrade/autostart.rs",
        caller: None,
        handled: Handled::Option,
        why: "an early `None` means the plist is not a Lumen entry",
    },
    Exempt {
        name: "plist_text",
        file: "upgrade/autostart.rs",
        caller: None,
        handled: Handled::Option,
        why: "an early `None` means the file or `plutil` gave no text, so not an entry",
    },
];

struct Body {
    file: usize,
    open: usize,
    close: usize,
    name: String,
}

#[derive(Default)]
struct Report {
    /// Each function reached, with the body that named it first.
    reached: BTreeMap<String, String>,
    violations: Vec<String>,
    /// Every `.unwrap()` in production code: listed, not pinned. The claim does not cover it.
    unwraps: Vec<String>,
    closure_lines: usize,
}

fn chain(reached: &BTreeMap<String, String>, name: &str) -> String {
    let mut path = vec![name.to_string()];
    let mut cur = name.to_string();
    while let Some(who) = reached.get(&cur) {
        path.push(who.clone());
        if who == CLOSURE {
            break;
        }
        cur = who.clone();
    }
    path.reverse();
    path.join(" -> ")
}

/// `None` when the call at `at..end` inside `body` deals with the error as `how` says.
fn unhandled(code: &[u8], at: usize, end: usize, body: &Body, how: &Handled) -> Option<String> {
    let start = statement_start(code, at, body.open + 1);
    match how {
        Handled::ByMatch => {
            if !has_word(&code[start..at], "match") {
                return Some("it is not the scrutinee of a `match`".into());
            }
            let Some(args_end) = matching(code, skip_ws(code, end)) else {
                return Some("its argument list does not balance".into());
            };
            let brace = skip_ws(code, args_end + 1);
            let arms = match matching(code, brace) {
                Some(close) if code.get(brace) == Some(&b'{') => &code[brace..close],
                _ => return Some("the call is not followed by the arms of a `match`".into()),
            };
            let diverges = ["return", "panic", "unreachable", "unwrap", "expect", "todo"]
                .into_iter()
                .find(|w| has_word(arms, w));
            if !has_word(arms, "Err") {
                Some("the `match` has no `Err` arm".into())
            } else if let Some(w) = diverges {
                Some(format!("the `match` arms use `{w}`"))
            } else if !tries(arms, 0, arms.len()).is_empty() {
                Some("the `match` arms use `?`".into())
            } else {
                None
            }
        }
        Handled::Forwarded | Handled::Option => None,
        Handled::By(adapter) => {
            let stmt = &code[at..statement_end(code, end, body.close)];
            if !has_word(stmt, adapter) {
                Some(format!("the statement does not go through `{adapter}`"))
            } else if ["unwrap", "expect"].iter().any(|w| has_word(stmt, w))
                || !tries(stmt, 0, stmt.len()).is_empty()
            {
                Some("the statement unwraps or propagates the result".into())
            } else {
                None
            }
        }
    }
}

/// `None` when the call at `at` is wrapped in `some!`/`tried!` inside an exempt function.
fn forwarded(code: &[u8], at: usize, body: &Body, exempt: &[Exempt]) -> Option<String> {
    let start = statement_start(code, at, body.open + 1);
    let wrapped = idents(code, start, at).into_iter().any(|(s, e)| {
        TRY_MACROS.iter().any(|m| m.as_bytes() == &code[s..e])
            && next_non_space(code, e) == Some(b'!')
    });
    if !wrapped {
        Some("the call is not wrapped in `some!` or `tried!`".into())
    } else if !exempt.iter().any(|y| y.name == body.name) {
        Some(format!(
            "`{}` is not itself exempt, so nothing deals with it",
            body.name
        ))
    } else {
        None
    }
}

/// The last place `needle` occurs in `b`, or 0.
fn rfind(b: &[u8], needle: &[u8]) -> usize {
    let mut last = 0;
    let mut from = 0;
    while let Some(i) = find(b, needle, from) {
        last = i;
        from = i + 1;
    }
    last
}

/// Scans `files` for the two startup claims. `files` must hold exactly one `.setup` closure.
fn startup(files: &[Source], exempt: &[Exempt]) -> Report {
    let mut r = Report::default();
    let mut defs: BTreeMap<String, Vec<Body>> = BTreeMap::new();
    let mut closures = Vec::new();
    for (fi, f) in files.iter().enumerate() {
        for func in functions(&f.code) {
            defs.entry(func.name.clone()).or_default().push(Body {
                file: fi,
                open: func.open,
                close: func.close,
                name: func.name,
            });
        }
        for (open, close) in setup_closures(&f.name, &f.code) {
            closures.push(Body {
                file: fi,
                open,
                close,
                name: CLOSURE.to_string(),
            });
        }
    }
    assert_eq!(
        closures.len(),
        1,
        "expected exactly one `.setup` closure in the app, found {}",
        closures.len()
    );
    let closure = &closures[0];
    let code = &files[closure.file].code;
    r.closure_lines = line_of(code, closure.close) - line_of(code, closure.open) + 1;

    // Everything startup can reach by naming it, breadth first.
    let mut scanned: Vec<&Body> = vec![closure];
    let mut queue: VecDeque<&Body> = VecDeque::from([closure]);
    while let Some(body) = queue.pop_front() {
        let f = &files[body.file];
        for (s, e) in idents(&f.code, body.open, body.close) {
            let id = text_of(&f.code, s, e);
            if r.reached.contains_key(&id) {
                continue;
            }
            if let Some(bodies) = defs.get(&id) {
                r.reached.insert(id, body.name.clone());
                for b in bodies {
                    scanned.push(b);
                    queue.push_back(b);
                }
            }
        }
    }

    for body in &scanned {
        let f = &files[body.file];
        if exempt
            .iter()
            .any(|x| x.name == body.name && x.file == f.name)
        {
            continue;
        }
        let who = if body.name == CLOSURE {
            CLOSURE.to_string()
        } else {
            format!("`{}`, reached {}", body.name, chain(&r.reached, &body.name))
        };
        for q in tries(&f.code, body.open, body.close) {
            r.violations.push(format!("{}: `?` in {who}", f.at(q)));
        }
    }

    // `expect`, in the whole crate: the one `Builder::build(..).expect(..)` in `run`, outside the
    // closure, and no other.
    let run = defs
        .get("run")
        .and_then(|v| v.iter().find(|b| files[b.file].name == "lib.rs"));
    let mut builder = 0;
    for (fi, f) in files.iter().enumerate() {
        for at in expects(&f.code, 0, f.code.len()) {
            let inside = |b: &Body| b.file == fi && b.open < at && at < b.close;
            let is_builder = run.is_some_and(inside)
                && !inside(closure)
                && receiver_call(&f.code, at).as_deref() == Some("build");
            if is_builder {
                builder += 1;
            } else {
                r.violations.push(format!(
                    "{}: `expect` in production code; the claim allows only \
                     `Builder::build(..).expect(..)` in `run`, outside the closure",
                    f.at(at)
                ));
            }
        }
        for at in unwraps(&f.code) {
            let line = line_of(&f.code, at);
            let shown = f.raw.lines().nth(line - 1).unwrap_or("").trim();
            r.unwraps.push(format!("{}:{line}  {shown}", f.name));
        }
    }
    if builder != 1 {
        r.violations.push(format!(
            "expected the one `Builder::build(..).expect(..)` in `run` that the claim names as \
             its exception, found {builder}"
        ));
    }

    // Each exemption is live, and its caller still deals with the error.
    for (xi, x) in exempt.iter().enumerate() {
        let own: Vec<&Body> = defs
            .get(x.name)
            .into_iter()
            .flatten()
            .filter(|b| files[b.file].name == x.file)
            .collect();
        let uses_try = own
            .iter()
            .any(|b| !tries(&files[b.file].code, b.open, b.close).is_empty());
        if !uses_try || !r.reached.contains_key(x.name) {
            r.violations.push(format!(
                "the exemption for `{}` in {} is stale: startup does not reach it, or it no \
                 longer uses `?`. Remove it from EXEMPT.",
                x.name, x.file
            ));
            continue;
        }
        if matches!(x.handled, Handled::Option) {
            for b in &own {
                let code = &files[b.file].code;
                let head = &code[..b.open];
                let from = rfind(head, format!("fn {}", x.name).as_bytes());
                if !squash(&head[from..]).contains("->Option<") {
                    r.violations.push(format!(
                        "{}: `{}` is exempt because it returns an `Option`, so its `?` is an \
                         early `None`, but it no longer does: {}",
                        files[b.file].at(b.open),
                        x.name,
                        x.why
                    ));
                }
            }
            continue;
        }
        // The calls are checked once per name, against whichever entry names the caller.
        if exempt[..xi].iter().any(|y| y.name == x.name) {
            continue;
        }
        let entries: Vec<&Exempt> = exempt.iter().filter(|y| y.name == x.name).collect();
        for body in &scanned {
            let f = &files[body.file];
            for (s, e) in idents(&f.code, body.open, body.close) {
                let is_call = &f.code[s..e] == x.name.as_bytes()
                    && next_non_space(&f.code, e) == Some(b'(')
                    && body.name != x.name;
                if !is_call {
                    continue;
                }
                let entry = entries.iter().find(|y| match y.caller {
                    None => body.name == CLOSURE,
                    Some(c) => body.name == c,
                });
                let problem = match entry {
                    Some(y) if matches!(y.handled, Handled::Forwarded) => {
                        forwarded(&f.code, s, body, exempt)
                    }
                    Some(y) => unhandled(&f.code, s, e, body, &y.handled),
                    None => Some(format!(
                        "it is called from {}, not from {}",
                        body.name,
                        entries
                            .iter()
                            .map(|y| y.caller.unwrap_or(CLOSURE))
                            .collect::<Vec<_>>()
                            .join(" or ")
                    )),
                };
                if let Some(p) = problem {
                    r.violations.push(format!(
                        "{}: `{}` uses `?` and is exempt because {}, but {p}",
                        f.at(s),
                        x.name,
                        entry.unwrap_or(&x).why
                    ));
                }
            }
        }
    }
    r.violations.sort();
    r.violations.dedup();
    r
}

// ── Claim 3: the log plugin, in every profile ─────────────────────────────────────────────────

/// What installs a logger of its own. A second one would make the plugin's registration fail, or
/// take the lines the file is meant to get.
const OTHER_LOGGERS: [&str; 11] = [
    "set_logger",
    "set_boxed_logger",
    "env_logger",
    "tracing_subscriber",
    "simple_logger",
    "simplelog",
    "flexi_logger",
    "pretty_env_logger",
    "stderrlog",
    "log4rs",
    "fern",
];

/// The unit test in `health.rs` that pins the release default of the log level, and its assertion
/// with whitespace removed.
const PINNED_TEST: &str = "the_log_level_defaults_differ_by_profile_but_are_never_off";
const PINNED_ASSERT: &str = "assert_eq!(log_level_from(None,false),log::LevelFilter::Warn)";

fn squash(b: &[u8]) -> String {
    String::from_utf8_lossy(b)
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect()
}

fn prev_word(code: &[u8], at: usize) -> String {
    let mut e = at;
    while e > 0 && code[e - 1].is_ascii_whitespace() {
        e -= 1;
    }
    let mut s = e;
    while s > 0 && is_ident(code[s - 1]) {
        s -= 1;
    }
    text_of(code, s, e)
}

/// Why the registration of the log plugin might not happen, or might not write the file, in some
/// profile. Empty when none of what this scan can see does.
fn logging(files: &[Source]) -> Vec<String> {
    let mut bad = Vec::new();
    for f in files {
        for w in OTHER_LOGGERS {
            if let Some(p) = find_word(&f.code, w, 0) {
                bad.push(format!("{}: `{w}`: a second logger", f.at(p)));
            }
        }
    }

    // One registration in the crate, and it is in the `.setup` closure.
    let mut regs = Vec::new();
    for (fi, f) in files.iter().enumerate() {
        let mut at = 0;
        while let Some(p) = find(&f.code, b".plugin", at) {
            at = p + 7;
            let paren = skip_ws(&f.code, p + 7);
            let end = matching(&f.code, paren).filter(|_| f.code.get(paren) == Some(&b'('));
            if let Some(end) = end
                && find(&f.code[..end], b"tauri_plugin_log", paren).is_some()
            {
                regs.push((fi, p, paren, end));
            }
        }
    }
    let [(fi, p, paren, end)] = regs[..] else {
        bad.push(format!(
            "expected one `.plugin(tauri_plugin_log::..)` registration in the app, found {}",
            regs.len()
        ));
        return bad;
    };
    let lib = &files[fi];
    let closures = setup_closures(&lib.name, &lib.code);
    let [(open, close)] = closures[..] else {
        bad.push("expected exactly one `.setup` closure".into());
        return bad;
    };
    if lib.name != "lib.rs" || !(open < p && p < close) {
        bad.push(format!(
            "{}: the registration is outside the `.setup` closure",
            lib.at(p)
        ));
        return bad;
    }
    let code = &lib.code[..];
    let here = |at: usize| lib.at(at);

    // At the top level of the closure, with nothing in front of it that depends on the profile,
    // an attribute, a condition or an earlier exit.
    if depth_at(code, open, p) != 1 {
        bad.push(format!(
            "{}: the registration is inside a nested block",
            here(p)
        ));
    }
    let start = statement_start(code, p, open + 1);
    let head = text_of(code, start, p)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let guarded = [
        "cfg", "cfg_attr", "match", "else", "while", "for", "loop", "unsafe",
    ]
    .into_iter()
    .any(|w| has_word(head.as_bytes(), w))
        || ["#[", "&&", "||", "!"]
            .into_iter()
            .any(|t| head.contains(t))
        || (has_word(head.as_bytes(), "if") && !head.starts_with("if let Err("));
    if guarded {
        bad.push(format!(
            "{}: the registration is conditional or attributed: `{head}`",
            here(start)
        ));
    }
    for w in [
        "return",
        "panic",
        "unreachable",
        "todo",
        "unimplemented",
        "exit",
        "abort",
    ] {
        if let Some(q) = find_word(&code[..p], w, open) {
            bad.push(format!(
                "{}: `{w}` before the registration can skip it",
                here(q)
            ));
        }
    }

    // It logs at the level `log_level_from` gives, to a file.
    let call = squash(&code[paren..=end]);
    for want in [".level(level)", "TargetKind::LogDir"] {
        if !call.contains(want) {
            bad.push(format!(
                "{}: the registration no longer has `{want}`",
                here(p)
            ));
        }
    }
    let lets: Vec<usize> = idents(code, open, p)
        .into_iter()
        .filter(|&(s, e)| &code[s..e] == b"level" && prev_word(code, s) == "let")
        .map(|(s, _)| s)
        .collect();
    let profile: Vec<usize> = idents(code, open, close)
        .into_iter()
        .filter(|&(s, e)| &code[s..e] == b"debug_assertions")
        .map(|(s, _)| s)
        .collect();
    match lets[..] {
        [l] => {
            let from = statement_start(code, l, open + 1);
            let to = statement_end(code, l, close);
            if depth_at(code, open, l) != 1 || text_of(code, from, l).trim() != "let" {
                bad.push(format!("{}: `let level` is nested or attributed", here(l)));
            }
            if !squash(&code[l..to]).contains("log_level_from(") {
                bad.push(format!(
                    "{}: `level` no longer comes from `log_level_from`",
                    here(l)
                ));
            }
            // The one place the profile is read, and it only picks the default level.
            if profile.len() != 1 || !(l < profile[0] && profile[0] < to) {
                bad.push(format!(
                    "{}: `debug_assertions` is read {} time(s) in the closure; it may be read \
                     once, inside `let level`, and nowhere else",
                    here(l),
                    profile.len()
                ));
            }
        }
        _ => bad.push(format!(
            "expected one `let level` before the registration, found {}",
            lets.len()
        )),
    }

    // A line that reaches the file at the release default, after the registration.
    let started = ["log::warn!", "log::error!"].iter().any(|m| {
        let mut at = end;
        while let Some(q) = find(code, m.as_bytes(), at).filter(|&q| q < close) {
            at = q + 1;
            let paren = skip_ws(code, q + m.len());
            let literal = text_of(&lib.text, paren + 1, (paren + 120).min(close));
            let top = depth_at(code, open, q) == 1;
            let plain = text_of(code, statement_start(code, q, open + 1), q)
                .trim()
                .is_empty();
            if top && plain && literal.trim_start().starts_with("\"STARTUP:") {
                return true;
            }
        }
        false
    });
    if !started {
        bad.push(format!(
            "{}: no top-level `log::warn!` or `log::error!` starting \"STARTUP:\" follows the \
             registration, so a release default of Warn would write nothing to prove it ran",
            here(end)
        ));
    }

    // The default the release profile gets is pinned by the app's own unit test. The scan cannot
    // run it, but it can see that it is still there, still a plain `#[test]`, and still says so.
    match files.iter().find(|f| f.name == "health.rs") {
        Some(h) => {
            let view = &h.with_tests[..];
            match functions(view).into_iter().find(|f| f.name == PINNED_TEST) {
                None => bad.push(format!("health.rs: the unit test `{PINNED_TEST}` is gone")),
                Some(f) => {
                    let head = squash(&view[statement_start(view, f.open, 0)..f.open]);
                    if head != format!("#[test]fn{PINNED_TEST}()") {
                        bad.push(format!(
                            "health.rs: `{PINNED_TEST}` is not a plain `#[test]` any more: `{head}`"
                        ));
                    }
                    if !squash(&view[f.open..=f.close]).contains(PINNED_ASSERT) {
                        bad.push(format!(
                            "health.rs: `{PINNED_TEST}` no longer asserts `{PINNED_ASSERT}`"
                        ));
                    }
                }
            }
        }
        None => bad.push("health.rs is not among the scanned files".into()),
    }
    bad
}

fn rel(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Every `Cargo.toml` in the workspace, as (path from the root, text).
fn manifests(root: &Path) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("cannot list {}: {e}", dir.display()));
        for entry in entries {
            let entry = entry.unwrap_or_else(|e| panic!("cannot list {}: {e}", dir.display()));
            let name = entry.file_name().to_string_lossy().into_owned();
            let kind = entry
                .file_type()
                .unwrap_or_else(|e| panic!("cannot stat {name}: {e}"));
            if kind.is_dir() {
                if !SKIP_DIRS.contains(&name.as_str()) {
                    stack.push(entry.path());
                }
            } else if name == "Cargo.toml" {
                out.push((rel(root, &entry.path()), read(&entry.path())));
            }
        }
    }
    out.sort();
    out
}

/// What a manifest can say that stops the log plugin writing: a compile-time cap on the `log`
/// level, the plugin made optional, or another logger in the app's dependencies.
fn manifest_violations(manifests: &[(String, String)]) -> Vec<String> {
    let mut bad = Vec::new();
    let mut plugin = false;
    for (name, body) in manifests {
        let mut section = String::new();
        for (i, line) in body.lines().enumerate() {
            let line = line.split('#').next().unwrap_or("").trim();
            if line.starts_with('[') {
                section = line
                    .trim_matches(|c| c == '[' || c == ']')
                    .trim()
                    .to_string();
                continue;
            }
            if line.contains("max_level_") {
                bad.push(format!(
                    "{name}:{}: `{line}` caps the log level when compiled",
                    i + 1
                ));
            }
            if name != APP_MANIFEST || !section.ends_with("dependencies") {
                continue;
            }
            let key = line
                .split('=')
                .next()
                .unwrap_or("")
                .trim()
                .replace('-', "_");
            if OTHER_LOGGERS.contains(&key.as_str()) {
                bad.push(format!("{name}:{}: `{key}` is a second logger", i + 1));
            }
            if key == "tauri_plugin_log" && section == "dependencies" {
                plugin = !line.contains("optional");
            }
        }
    }
    if !plugin {
        bad.push(format!(
            "{APP_MANIFEST}: `tauri-plugin-log` is not a plain, non-optional `[dependencies]` entry"
        ));
    }
    bad
}

/// App files that are neither production files nor declared test modules: code this scan would
/// otherwise miss.
fn unscanned(root: &Path, files: &[Source]) -> Vec<String> {
    let base = root.join(APP_SRC);
    let mut found = Vec::new();
    let mut stack = vec![base.clone()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display())) {
            let path = entry
                .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
                .path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                found.push(rel(&base, &path));
            }
        }
    }
    let mut declared = BTreeSet::new();
    for f in files {
        for m in &f.test_mods {
            let stem = f.name.trim_end_matches(".rs");
            let dir = if matches!(f.name.as_str(), "lib.rs" | "main.rs") {
                String::new()
            } else {
                format!("{stem}/")
            };
            declared.insert(format!("{dir}{m}.rs"));
            declared.insert(format!("{dir}{m}/"));
        }
    }
    found
        .into_iter()
        .filter(|p| {
            !APP_FILES.contains(&p.as_str())
                && !declared
                    .iter()
                    .any(|d| p == d || (d.ends_with('/') && p.starts_with(d.as_str())))
        })
        .collect()
}

// ── Tests of the scan itself ──────────────────────────────────────────────────────────────────

fn real() -> (PathBuf, Vec<Source>) {
    let root = workspace_root();
    let files = load(&root);
    (root, files)
}

#[test]
fn the_scan_reads_code_and_not_what_only_looks_like_it() {
    // (source, `?` operators, `expect` calls)
    let cases: [(&str, usize, usize); 17] = [
        (r#"fn f() -> R { g()?; Ok(()) }"#, 1, 0),
        (r#"fn f() -> R { g()?.h()?; Ok(()) }"#, 2, 0),
        (r#"fn f<T: ?Sized>(x: &T) {}"#, 0, 0),
        (r#"fn f<T: Debug + ?Sized>(x: &T) {}"#, 0, 0),
        (r#"fn f() { let s = "a? .expect(x)"; }"#, 0, 0),
        (r#"fn f() { // g()?; x.expect(1)"#, 0, 0),
        (r#"fn f() { /* g()? /* nested */ x.expect(1) */ }"#, 0, 0),
        (
            r#"fn f() { let c = '?'; let d = '\''; let e = '"'; g()?; }"#,
            1,
            0,
        ),
        (
            r#"fn f<'a>(x: &'a str) -> &'a str { x.parse().expect("n") }"#,
            0,
            1,
        ),
        (
            r####"fn f() { let s = r#"x?.expect("y")"#; h().expect_err("e"); }"####,
            0,
            1,
        ),
        (r#"#[expect(dead_code)] fn f() {}"#, 0, 0),
        (
            r#"fn f() { let expected = 1; let expect_target = 2; expected_value(); }"#,
            0,
            0,
        ),
        (r#"fn f() { Result::expect(r, "x"); }"#, 0, 1),
        (
            r#"fn f() { let b = b"?"; let c = c"?"; let r = br"?"; }"#,
            0,
            0,
        ),
        ("fn f() { x\r\n    .expect(\"crlf\")\r\n    ; }", 0, 1),
        (
            r#"fn f() { let s = "multi
            line? .expect(x)
            string"; g()?; }"#,
            1,
            0,
        ),
        (r#"fn f() { let tick = 'a; g()?; }"#, 1, 0),
    ];
    for (raw, q, e) in cases {
        let s = Source::new("x.rs", raw);
        assert_eq!(tries(&s.code, 0, s.code.len()).len(), q, "`?` in {raw:?}");
        assert_eq!(
            expects(&s.code, 0, s.code.len()).len(),
            e,
            "`expect` in {raw:?}"
        );
    }
}

#[test]
fn a_blanked_view_keeps_every_offset_and_line() {
    let raw = "fn f() { /* a\nb */ let s = \"x\ny\"; // é\n    g()?; let c = 'é'; }\r\n";
    let (code, text) = (blank(raw, true), blank(raw, false));
    assert_eq!((code.len(), text.len()), (raw.len(), raw.len()));
    let nl = |b: &[u8]| b.iter().filter(|&&x| x == b'\n').count();
    assert_eq!(
        (nl(&code), nl(&text)),
        (nl(raw.as_bytes()), nl(raw.as_bytes()))
    );
    assert!(
        String::from_utf8(code.clone()).is_ok(),
        "no multibyte character is cut"
    );
    assert!(find(&text, b"\"x", 0).is_some() && find(&code, b"\"x", 0).is_none());
    assert_eq!(find(&code, b"g()?", 0), raw.find("g()?"));
}

#[test]
fn test_modules_are_blanked_and_nothing_else_is() {
    let s = Source::new(
        "x.rs",
        r#"
#[cfg(test)]
mod tests { fn t() -> R { g()?; h().expect("e"); Ok(()) } }
#[cfg(test)]
mod hook_e2e;
#[cfg(test)] #[allow(dead_code)] pub(crate) mod more { fn m() { g()?; } }
#[cfg(not(test))]
fn real() -> R { g()?; Ok(()) }
#[cfg_attr(not(test), allow(dead_code))]
fn kept() -> R { g()?; Ok(()) }
"#,
    );
    assert_eq!(
        tries(&s.code, 0, s.code.len()).len(),
        2,
        "`real` and `kept` only"
    );
    assert_eq!(s.test_mods, ["hook_e2e"]);
    assert!(expects(&s.code, 0, s.code.len()).is_empty());
}

#[test]
#[should_panic(expected = "other than a module")]
fn a_cfg_test_on_something_other_than_a_module_is_refused() {
    Source::new("x.rs", "#[cfg(test)]\nfn helper() { g()?; }\n");
}

#[test]
#[should_panic(expected = "has not been taught")]
fn a_cfg_test_shape_the_scan_does_not_know_is_refused() {
    Source::new("x.rs", "#[cfg(any(test, feature = \"x\"))]\nmod m {}\n");
}

/// A stand-in for `lib.rs`: `@CLOSURE@`, `@FIRST@`, `@THIRD@` and `@ITEMS@` are filled in per test.
fn app(closure: &str, first: &str, third: &str, items: &str) -> Vec<Source> {
    let raw = r#"
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            first(app);
            @CLOSURE@
            Ok(())
        })
        .build(context())
        .expect("error while building");
}
fn first(app: A) { @FIRST@ }
fn second() { let _ = third(); }
fn third() -> Option<u8> { @THIRD@ }
fn lookup() -> Option<u8> { None }
@ITEMS@
#[cfg(test)]
mod tests { fn t() -> Option<u8> { lookup()?; None } }
"#
    .replace("@CLOSURE@", closure)
    .replace("@FIRST@", first)
    .replace("@THIRD@", third)
    .replace("@ITEMS@", items);
    vec![Source::new("lib.rs", &raw)]
}

fn clean_app() -> Vec<Source> {
    app("", "second();", "lookup()", "")
}

fn violations(files: &[Source], exempt: &[Exempt]) -> Vec<String> {
    startup(files, exempt).violations
}

fn mentions(v: &[String], needle: &str) -> bool {
    v.iter().any(|m| m.contains(needle))
}

#[test]
fn a_question_mark_is_found_where_startup_reaches_and_nowhere_else() {
    let r = startup(&clean_app(), &[]);
    assert!(r.violations.is_empty(), "{:?}", r.violations);
    assert_eq!(
        r.reached.keys().map(String::as_str).collect::<Vec<_>>(),
        ["first", "lookup", "second", "third"],
        "by name, through every call, and not into `run`, the test module or `never_called`"
    );

    // Three calls down, and the report says how startup gets there.
    let deep = app("", "second();", "let v = lookup()?; Some(v)", "");
    let v = violations(&deep, &[]);
    assert_eq!(v.len(), 1, "{v:?}");
    assert!(
        v[0].contains("`third`, reached the .setup closure -> first -> second -> third"),
        "{v:?}"
    );

    let direct = app("let _x = read()?;", "second();", "lookup()", "");
    assert!(mentions(
        &violations(&direct, &[]),
        "`?` in the .setup closure"
    ));

    // Not reached, not claimed: this is where the scan stops, and the docs say so.
    let unreached = app(
        "",
        "second();",
        "lookup()",
        "fn never() -> Option<u8> { lookup()?; None }",
    );
    assert!(violations(&unreached, &[]).is_empty());
}

#[test]
fn the_only_expect_allowed_is_builders_in_run() {
    let closure = app("x.expect(\"e\");", "second();", "lookup()", "");
    assert!(mentions(
        &violations(&closure, &[]),
        "`expect` in production code"
    ));
    let elsewhere = app(
        "",
        "second();",
        "lookup()",
        "fn other() { x.expect(\"e\"); }",
    );
    assert!(mentions(
        &violations(&elsewhere, &[]),
        "`expect` in production code"
    ));
    let second_build = app(
        "",
        "second();",
        "lookup()",
        "fn again() { b().build(c()).expect(\"x\"); }",
    );
    assert!(mentions(
        &violations(&second_build, &[]),
        "`expect` in production code"
    ));
    let err = app(
        "",
        "second();",
        "lookup()",
        "fn again() { r().expect_err(\"x\"); }",
    );
    assert!(mentions(
        &violations(&err, &[]),
        "`expect` in production code"
    ));

    // The exception is named, so it must exist.
    let mut none = clean_app();
    let raw = none[0]
        .raw
        .replace(".expect(\"error while building\")", ".unwrap_or_default()");
    none[0] = Source::new("lib.rs", &raw);
    assert!(mentions(&violations(&none, &[]), "found 0"));

    // `unwrap` is listed and is not part of the claim.
    let unwrapped = startup(&app("x.unwrap();", "second();", "lookup()", ""), &[]);
    assert!(unwrapped.violations.is_empty() && unwrapped.unwraps.len() == 1);
}

fn risky(caller: Option<&'static str>, handled: Handled) -> [Exempt; 1] {
    [Exempt {
        name: "risky",
        file: "lib.rs",
        caller,
        handled,
        why: "test",
    }]
}

const RISKY: &str = "fn risky() -> Result<u8, String> { let v = parse()?; Ok(v) } \
                     fn parse() -> Result<u8, String> { Ok(1) }";

#[test]
fn an_exempt_function_is_exempt_only_while_its_caller_deals_with_the_error() {
    let by_match = risky(None, Handled::ByMatch);
    let with = |closure: &str, items: &str| {
        violations(
            &app(
                closure,
                "second();",
                "lookup()",
                &format!("{RISKY} {items}"),
            ),
            &by_match,
        )
    };
    let handled = "match risky() { Ok(_) => {} Err(e) => { log(e); } }";
    assert!(with(handled, "").is_empty());
    for (closure, why) in [
        (
            "let _ = risky().unwrap();",
            "not the scrutinee of a `match`",
        ),
        ("match risky() { Ok(_) => {} _ => {} }", "no `Err` arm"),
        (
            "match risky() { Ok(_) => {} Err(e) => return Err(e.into()), }",
            "arms use `return`",
        ),
        (
            "match risky() { Ok(_) => {} Err(e) => { x(e).unwrap(); } }",
            "arms use `unwrap`",
        ),
        (
            "match risky() { Ok(_) => {} Err(e) => { x(e)?; } }",
            "arms use `?`",
        ),
    ] {
        assert!(
            mentions(&with(closure, ""), why),
            "{closure}: {:?}",
            with(closure, "")
        );
    }
    // Called from somewhere it was not agreed.
    let v = with(&format!("{handled} other();"), "fn other() { risky(); }");
    assert!(
        mentions(&v, "called from other, not from the .setup closure"),
        "{v:?}"
    );
    // No longer using `?`: the entry would exempt nothing, so it must go.
    let lean = violations(
        &app(
            handled,
            "second();",
            "lookup()",
            "fn risky() -> Result<u8, String> { Ok(1) }",
        ),
        &by_match,
    );
    assert!(mentions(&lean, "`risky` in lib.rs is stale"), "{lean:?}");

    // The other shape: the statement defaults the result.
    let by = risky(Some("first"), Handled::By("unwrap_or_default"));
    let first = |body: &str| violations(&app("", body, "lookup()", RISKY), &by);
    assert!(first("let _v = risky().map(|x| x + 1).unwrap_or_default(); second();").is_empty());
    assert!(mentions(
        &first("let _v = risky().unwrap(); second();"),
        "does not go through `unwrap_or_default`"
    ));
    assert!(mentions(
        &first("let _v = risky().ok(); second();"),
        "does not go through `unwrap_or_default`"
    ));
}

// ── The real sources ──────────────────────────────────────────────────────────────────────────

#[test]
fn the_scan_reads_every_production_file_of_the_app() {
    let (root, files) = real();
    assert_eq!(files.len(), APP_FILES.len());
    let missed = unscanned(&root, &files);
    assert!(
        missed.is_empty(),
        "{missed:?} under {APP_SRC} is neither one of {APP_FILES:?} nor a `#[cfg(test)] mod` \
         declared in one of them, so the scan would not read it. Add it to APP_FILES."
    );
}

#[test]
fn startup_has_no_unhandled_question_mark_and_one_expect() {
    let (_, files) = real();
    let r = startup(&files, &EXEMPT);
    println!(
        "the .setup closure is {} lines long and reaches {} functions by name:",
        r.closure_lines,
        r.reached.len()
    );
    for (name, from) in &r.reached {
        println!("  {name}  (named in {from})");
    }
    println!("`unwrap` in production code, which the claim does not cover:");
    for u in &r.unwraps {
        println!("  {u}");
    }
    assert!(r.violations.is_empty(), "\n{}", r.violations.join("\n"));

    // An empty report is worth something only if the scan looked at the right code.
    assert!(
        r.closure_lines >= 150,
        "the closure is {} lines long: has the scan lost it?",
        r.closure_lines
    );
    for name in [
        "render_tray_icon",
        "connect_daemon",
        "reveal_main_window",
        "log_level_from",
        "build_tray_menu_items",
        "ensure_scripts_fresh",
        "ensure_scripts_fresh_in",
        "write_atomic",
        "stamp_line",
    ] {
        assert!(
            r.reached.contains_key(name),
            "the scan no longer finds `{name}` reachable from the closure"
        );
    }
}

#[test]
fn the_log_plugin_is_registered_the_same_way_in_every_profile() {
    let (root, files) = real();
    let bad = logging(&files);
    assert!(bad.is_empty(), "\n{}", bad.join("\n"));
    let found = manifests(&root);
    assert!(
        found.iter().any(|(n, _)| n == APP_MANIFEST) && found.len() >= 6,
        "the manifests found: {:?}",
        found.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>()
    );
    let bad = manifest_violations(&found);
    assert!(bad.is_empty(), "\n{}", bad.join("\n"));
}

#[test]
fn a_windows_checkout_with_crlf_line_endings_is_read_the_same() {
    let (_, files) = real();
    let windows: Vec<Source> = files
        .iter()
        .map(|f| Source::new(&f.name, &f.raw.replace("\r\n", "\n").replace('\n', "\r\n")))
        .collect();
    let (lf, crlf) = (startup(&files, &EXEMPT), startup(&windows, &EXEMPT));
    assert!(crlf.violations.is_empty(), "{:?}", crlf.violations);
    assert_eq!((&lf.reached, &lf.unwraps), (&crlf.reached, &crlf.unwraps));
    assert!(logging(&windows).is_empty());
    must_say(startup_seeds(), &windows, |f| {
        startup(f, &EXEMPT).violations
    });
    must_say(logging_seeds(), &windows, logging);
}

// ── Seeded breakage: each edit to the real sources must turn the scan red, for the stated reason

/// One edit to a real file, and what the scan must say about it.
struct Seed {
    what: &'static str,
    file: &'static str,
    edit: fn(&Source) -> String,
    says: &'static str,
}

/// `s` with its one `from` replaced. Fails if the anchor is not there exactly once, so an edit
/// cannot turn into a no-op when the source moves.
fn once(s: &Source, from: &str, to: &str) -> String {
    let n = s.raw.matches(from).count();
    assert_eq!(n, 1, "{from:?} occurs {n} times in {}, expected 1", s.name);
    s.raw.replacen(from, to, 1)
}

/// `s` with the last `from` before the one `anchor` replaced.
fn before(s: &Source, anchor: &str, from: &str, to: &str) -> String {
    let n = s.raw.matches(anchor).count();
    assert_eq!(
        n, 1,
        "{anchor:?} occurs {n} times in {}, expected 1",
        s.name
    );
    let at = s.raw.find(anchor).unwrap_or(0);
    let p = s.raw[..at]
        .rfind(from)
        .unwrap_or_else(|| panic!("no {from:?} before {anchor:?} in {}", s.name));
    format!("{}{to}{}", &s.raw[..p], &s.raw[p + from.len()..])
}

/// `s` with `text` as the first thing in the body of `name`. Blanking keeps byte offsets, so a
/// position found in the code view is the same position in the raw text.
fn in_fn(s: &Source, name: &str, text: &str) -> String {
    let f = functions(&s.code)
        .into_iter()
        .find(|f| f.name == name)
        .unwrap_or_else(|| panic!("no fn `{name}` in {}", s.name));
    format!("{}{text}{}", &s.raw[..=f.open], &s.raw[f.open + 1..])
}

/// `s` with `text` as the first thing in the `.setup` closure.
fn in_closure(s: &Source, text: &str) -> String {
    let (open, _) = setup_closures(&s.name, &s.code)[0];
    format!("{}{text}{}", &s.raw[..=open], &s.raw[open + 1..])
}

fn appended(raw: &str, text: &str) -> String {
    format!("{raw}\n{text}\n")
}

/// `files` with `edit` applied to `file`, and nothing else changed.
fn mutated(files: &[Source], file: &str, edit: fn(&Source) -> String) -> Vec<Source> {
    files
        .iter()
        .map(|f| {
            if f.name != file {
                return Source::new(&f.name, &f.raw);
            }
            let raw = edit(f);
            assert_ne!(raw, f.raw, "the seeded edit changed nothing in {file}");
            Source::new(&f.name, &raw)
        })
        .collect()
}

fn must_say(seeds: Vec<Seed>, files: &[Source], scan: fn(&[Source]) -> Vec<String>) {
    for seed in seeds {
        let said = scan(&mutated(files, seed.file, seed.edit));
        assert!(
            said.iter().any(|m| m.contains(seed.says)),
            "{}: expected a report containing {:?}, got:\n{}",
            seed.what,
            seed.says,
            said.join("\n")
        );
    }
}

fn startup_seeds() -> Vec<Seed> {
    vec![
        Seed {
            what: "a `?` in the closure",
            file: "lib.rs",
            edit: |s| in_closure(s, " let _seed = std::env::var(\"X\")?; "),
            says: "`?` in the .setup closure",
        },
        Seed {
            what: "a `?` in a function the closure calls",
            file: "lib.rs",
            edit: |s| in_fn(s, "render_tray_icon", " let _seed = g()?; "),
            says: "`render_tray_icon`, reached the .setup closure -> render_tray_icon",
        },
        Seed {
            what: "a `?` several calls down, in another file",
            file: "setup.rs",
            edit: |s| in_fn(s, "stamp_line", " let _seed = g()?; "),
            says: "`stamp_line`, reached the .setup closure -> ",
        },
        Seed {
            what: "a `?` in a function the closure starts calling",
            file: "lib.rs",
            edit: |s| {
                appended(
                    &in_closure(s, " startup_extra(); "),
                    "fn startup_extra() -> Option<u8> { let v = g()?; Some(v) }",
                )
            },
            says: "`startup_extra`, reached the .setup closure -> startup_extra",
        },
        Seed {
            what: "an `expect` in the closure",
            file: "lib.rs",
            edit: |s| in_closure(s, " x().expect(\"e\"); "),
            says: "`expect` in production code",
        },
        Seed {
            what: "an `expect` in a function the closure calls",
            file: "lib.rs",
            edit: |s| in_fn(s, "connect_daemon", " x().expect(\"e\"); "),
            says: "`expect` in production code",
        },
        Seed {
            what: "an `expect` in a function startup never calls",
            file: "lib.rs",
            edit: |s| appended(&s.raw, "fn never_called() { x().expect(\"e\"); }"),
            says: "`expect` in production code",
        },
        Seed {
            what: "a second `expect` on the builder chain",
            file: "lib.rs",
            edit: |s| {
                once(
                    s,
                    ".build(app_context())",
                    ".build(app_context()).expect(\"again\")",
                )
            },
            says: "`expect` in production code",
        },
        Seed {
            what: "the builder's `expect` gone, so the stated exception is not there",
            file: "lib.rs",
            edit: |s| {
                once(
                    s,
                    ".expect(\"error while building tauri application\")",
                    ".unwrap_or_else(|e| panic!(\"{e}\"))",
                )
            },
            says: "found 0",
        },
        Seed {
            what: "the tray menu unwrapped where the closure matches it",
            file: "lib.rs",
            edit: |s| {
                once(
                    s,
                    "match health::build_tray_menu_items(app) {",
                    "health::build_tray_menu_items(app).unwrap(); if false {",
                )
            },
            says: "it is not the scrutinee of a `match`",
        },
        Seed {
            what: "a `tried!` in a function startup reaches: the macro is a `?`",
            file: "setup.rs",
            edit: |s| in_fn(s, "ensure_scripts_fresh_in", " tried!(g()); "),
            says: "`?` in `ensure_scripts_fresh_in`",
        },
        Seed {
            what: "a `some!` in a function the closure starts calling",
            file: "lib.rs",
            edit: |s| {
                appended(
                    &in_closure(s, " startup_extra(); "),
                    "fn startup_extra() -> Option<u8> { let v = some!(g()); Some(v) }",
                )
            },
            says: "`?` in `startup_extra`",
        },
        Seed {
            what: "write_atomic called from the closure, where nothing handles its error",
            file: "lib.rs",
            edit: |s| in_closure(s, " let _ = write_atomic(p, c, 0); "),
            says: "called from the .setup closure, not from ensure_scripts_fresh_in or repoint_file",
        },
        Seed {
            what: "write_atomic's caller returns on its error",
            file: "setup.rs",
            edit: |s| {
                once(
                    s,
                    "Err(e) => log::warn!(\"could not refresh {name}: {e}\"),",
                    "Err(e) => return Some(e.to_string()),",
                )
            },
            says: "the `match` arms use `return`",
        },
        Seed {
            what: "write_run_json called without handing its error on",
            file: "upgrade/autostart.rs",
            edit: |s| {
                once(
                    s,
                    "tried!(write_run_json(env, e));",
                    "write_run_json(env, e).unwrap();",
                )
            },
            says: "the call is not wrapped in `some!` or `tried!`",
        },
        Seed {
            what: "an Option helper that no longer returns an Option",
            file: "upgrade/autostart.rs",
            edit: |s| {
                once(
                    s,
                    "fn plist_text(path: &Path) -> Option<String> {",
                    "fn plist_text(path: &Path) -> Result<String, String> {",
                )
            },
            says: "it no longer does",
        },
        Seed {
            what: "stable_binary unwrapped where baked_paths defaults it",
            file: "setup.rs",
            edit: |s| {
                once(
                    s,
                    "let mcp = stable_binary(\"lumen-mcp\")",
                    "let mcp = stable_binary(\"lumen-mcp\").unwrap().into()",
                )
            },
            says: "the statement unwraps or propagates the result",
        },
    ]
}

fn logging_seeds() -> Vec<Seed> {
    const REGISTER: &str = "if let Err(e) = app.handle().plugin(";
    vec![
        Seed {
            what: "the registration behind a cfg attribute",
            file: "lib.rs",
            edit: |s| {
                once(
                    s,
                    REGISTER,
                    &format!("#[cfg(debug_assertions)]\n            {REGISTER}"),
                )
            },
            says: "conditional or attributed",
        },
        Seed {
            what: "the registration in the else branch of a profile test",
            file: "lib.rs",
            edit: |s| {
                once(
                    s,
                    REGISTER,
                    &format!("if cfg!(debug_assertions) {{ }} else {REGISTER}"),
                )
            },
            says: "conditional or attributed",
        },
        Seed {
            what: "a second read of the profile in the closure",
            file: "lib.rs",
            edit: |s| in_closure(s, " let _seed = cfg!(debug_assertions); "),
            says: "`debug_assertions` is read 2 time(s)",
        },
        Seed {
            what: "a file target that is not the log directory",
            file: "lib.rs",
            edit: |s| {
                once(
                    s,
                    "tauri_plugin_log::TargetKind::LogDir {",
                    "tauri_plugin_log::TargetKind::Webview {",
                )
            },
            says: "no longer has `TargetKind::LogDir`",
        },
        Seed {
            what: "a level that ignores the profile",
            file: "lib.rs",
            edit: |s| once(s, ".level(level)", ".level(log::LevelFilter::Off)"),
            says: "no longer has `.level(level)`",
        },
        Seed {
            what: "the STARTUP line demoted below the release default",
            file: "lib.rs",
            edit: |s| before(s, "\"STARTUP: Lumen", "log::warn!", "log::info!"),
            says: "no top-level `log::warn!` or `log::error!` starting \"STARTUP:\"",
        },
        Seed {
            what: "a second logger installed in the closure",
            file: "lib.rs",
            edit: |s| in_closure(s, " env_logger::init(); "),
            says: "`env_logger`: a second logger",
        },
        Seed {
            what: "an early return before the registration",
            file: "lib.rs",
            edit: |s| in_closure(s, " if std::env::var(\"X\").is_ok() { return Ok(()); } "),
            says: "`return` before the registration can skip it",
        },
        Seed {
            what: "the registration gone",
            file: "lib.rs",
            edit: |s| once(s, "app.handle().plugin(", "app.handle().plugin_gone("),
            says: "found 0",
        },
        Seed {
            what: "the release default test renamed",
            file: "health.rs",
            edit: |s| {
                once(
                    s,
                    "fn the_log_level_defaults_differ_by_profile_but_are_never_off()",
                    "fn renamed()",
                )
            },
            says: "the unit test `the_log_level_defaults_differ_by_profile_but_are_never_off` is gone",
        },
        Seed {
            what: "the release default test ignored",
            file: "health.rs",
            edit: |s| {
                once(
                    s,
                    "fn the_log_level_defaults_differ_by_profile_but_are_never_off()",
                    "#[ignore]\n    fn the_log_level_defaults_differ_by_profile_but_are_never_off()",
                )
            },
            says: "is not a plain `#[test]` any more",
        },
        Seed {
            what: "the release default test asserting Info",
            file: "health.rs",
            edit: |s| {
                once(
                    s,
                    "assert_eq!(log_level_from(None, false), log::LevelFilter::Warn);",
                    "assert_eq!(log_level_from(None, false), log::LevelFilter::Info);",
                )
            },
            says: "no longer asserts",
        },
    ]
}

#[test]
fn a_change_to_startup_that_breaks_a_claim_is_caught() {
    let (_, files) = real();
    must_say(startup_seeds(), &files, |f| startup(f, &EXEMPT).violations);
}

#[test]
fn a_change_to_the_log_registration_that_breaks_its_claim_is_caught() {
    let (_, files) = real();
    must_say(logging_seeds(), &files, logging);
}

#[test]
fn an_exemption_that_no_longer_exempts_anything_is_reported() {
    let (_, files) = real();
    let mut all = EXEMPT.to_vec();
    all.push(Exempt {
        name: "connect_daemon",
        file: "lib.rs",
        caller: None,
        handled: Handled::ByMatch,
        why: "a function that does not use `?`",
    });
    let v = startup(&files, &all).violations;
    assert!(
        mentions(&v, "the exemption for `connect_daemon` in lib.rs is stale"),
        "{v:?}"
    );
}

#[test]
fn a_question_mark_in_a_function_startup_never_names_is_not_found() {
    // The limit of a name-based walk, kept here so that it is stated and tested, not discovered.
    let (_, files) = real();
    let changed = mutated(&files, "lib.rs", |s| {
        appended(
            &s.raw,
            "fn never_called() -> Option<u8> { let v = g()?; Some(v) }",
        )
    });
    assert!(startup(&changed, &EXEMPT).violations.is_empty());
}

// ── The logging check on small sources, for the shapes the real ones do not have ─────────────

const LOG_ONE: &str = r#"
            let level = health::log_level_from(std::env::var("LUMEN_LOG").ok().as_deref(), cfg!(debug_assertions));
            if let Err(e) = app.handle().plugin(
                tauri_plugin_log::Builder::default()
                    .level(level)
                    .targets([tauri_plugin_log::Target::new(tauri_plugin_log::TargetKind::LogDir { file_name: None })])
                    .build(),
            ) {
                eprintln!("LOG: {e}");
            }
            log::warn!("STARTUP: Lumen {}", env!("CARGO_PKG_VERSION"));
"#;

const LOG_HEALTH: &str = r#"
#[cfg(test)]
mod tests {
    #[test]
    fn the_log_level_defaults_differ_by_profile_but_are_never_off() {
        assert_eq!(log_level_from(None, false), log::LevelFilter::Warn);
    }
}
"#;

fn log_app(closure: &str, outside: &str) -> Vec<Source> {
    let lib = r#"
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            @CLOSURE@
            Ok(())
        })
        .build(context())
        .expect("error while building");
}
@OUTSIDE@
"#
    .replace("@CLOSURE@", closure)
    .replace("@OUTSIDE@", outside);
    vec![
        Source::new("lib.rs", &lib),
        Source::new("health.rs", LOG_HEALTH),
    ]
}

#[test]
fn the_logging_check_names_each_way_the_registration_can_stop_happening() {
    let ok = logging(&log_app(LOG_ONE, ""));
    assert!(ok.is_empty(), "{ok:?}");

    let at = "if let Err(e) = app.handle().plugin(";
    let startup_line = "log::warn!(\"STARTUP: Lumen {}\", env!(\"CARGO_PKG_VERSION\"));";
    let cases: Vec<(&str, String, &str)> = vec![
        (
            "inside a block",
            LOG_ONE
                .replace(at, &format!("if ok() {{ {at}"))
                .replace(startup_line, &format!("}} {startup_line}")),
            "inside a nested block",
        ),
        (
            "as a match scrutinee",
            LOG_ONE.replace(at, "match app.handle().plugin("),
            "conditional or attributed",
        ),
        (
            "behind `&&`",
            LOG_ONE.replace(at, "let _r = ok() && app.handle().plugin("),
            "conditional or attributed",
        ),
        (
            "with a level that is not from log_level_from",
            LOG_ONE.replace("health::log_level_from(", "pick("),
            "no longer comes from `log_level_from`",
        ),
        (
            "with a second `let level`",
            format!("let level = 1; {LOG_ONE}"),
            "expected one `let level`",
        ),
        (
            "with the profile read after the level",
            LOG_ONE.replace("log::warn!", "let _p = cfg!(debug_assertions); log::warn!"),
            "`debug_assertions` is read 2 time(s)",
        ),
        (
            "with no STARTUP line",
            LOG_ONE.replace("\"STARTUP: Lumen", "\"Lumen"),
            "no top-level `log::warn!`",
        ),
        (
            "with the STARTUP line in a branch",
            LOG_ONE.replace(startup_line, &format!("if ok() {{ {startup_line} }}")),
            "no top-level `log::warn!`",
        ),
        ("registered twice", format!("{LOG_ONE}{LOG_ONE}"), "found 2"),
        (
            "with a panic before it",
            format!("if x() {{ panic!(\"no\"); }} {LOG_ONE}"),
            "`panic` before the registration can skip it",
        ),
        (
            "with an `exit` before it",
            format!("if x() {{ std::process::exit(1); }} {LOG_ONE}"),
            "`exit` before the registration can skip it",
        ),
    ];
    for (what, body, says) in cases {
        assert_ne!(body, LOG_ONE, "{what}: the case changed nothing");
        let said = logging(&log_app(&body, ""));
        assert!(
            mentions(&said, says),
            "{what}: expected {says:?} among {said:#?}"
        );
    }

    // The registration somewhere other than the closure.
    let outside = format!("fn elsewhere(app: A) {{ {LOG_ONE} }}");
    let said = logging(&log_app("", &outside));
    assert!(mentions(&said, "outside the `.setup` closure"), "{said:?}");
}

#[test]
fn the_pinned_release_default_is_read_from_a_test_that_runs() {
    let ok = |health: &str| {
        let mut files = log_app(LOG_ONE, "");
        files[1] = Source::new("health.rs", health);
        logging(&files)
    };
    assert!(ok(LOG_HEALTH).is_empty());
    for (what, health, says) in [
        (
            "in a comment",
            LOG_HEALTH
                .replace("    #[test]", "    /* #[test]")
                .replace("Warn);\n    }", "Warn);\n    } */"),
            "is gone",
        ),
        (
            "in a string",
            "const X: &str = \"fn the_log_level_defaults_differ_by_profile_but_are_never_off() {}\";"
                .to_string(),
            "is not a plain `#[test]`",
        ),
        (
            "ignored",
            LOG_HEALTH.replace("#[test]", "#[test]\n    #[ignore]"),
            "is not a plain `#[test]`",
        ),
        (
            "with another assertion",
            LOG_HEALTH.replace("Warn", "Off"),
            "no longer asserts",
        ),
        (
            "asserted for the debug profile only",
            LOG_HEALTH.replace("None, false", "None, true"),
            "no longer asserts",
        ),
    ] {
        let said = ok(&health);
        assert!(mentions(&said, says), "{what}: {said:?}");
    }
}

// ── The manifests ─────────────────────────────────────────────────────────────────────────────

const APP_TOML: &str = r#"
[package]
name = "Lumen"

[dependencies]
log = "0.4"
tauri-plugin-log = "2"

[target.'cfg(target_os = "macos")'.dependencies]
objc2 = "0.6"
"#;

fn toml_pair(app: &str, other: &str) -> Vec<(String, String)> {
    vec![
        ("crates/other/Cargo.toml".to_string(), other.to_string()),
        (APP_MANIFEST.to_string(), app.to_string()),
    ]
}

#[test]
fn a_manifest_that_caps_the_log_level_or_adds_a_logger_is_caught() {
    assert!(manifest_violations(&toml_pair(APP_TOML, "[package]\nname = \"x\"\n")).is_empty());

    let cases = [
        (
            "a compile-time cap on the app's `log`",
            APP_TOML.replace(
                "log = \"0.4\"",
                "log = { version = \"0.4\", features = [\"release_max_level_off\"] }",
            ),
            "[package]\n".to_string(),
            "caps the log level",
        ),
        (
            "the same cap in another crate of the workspace",
            APP_TOML.to_string(),
            "[dependencies]\nlog = { version = \"0.4\", features = [\"max_level_error\"] }\n"
                .to_string(),
            "crates/other/Cargo.toml:2:",
        ),
        (
            "the plugin made optional",
            APP_TOML.replace(
                "tauri-plugin-log = \"2\"",
                "tauri-plugin-log = { version = \"2\", optional = true }",
            ),
            String::new(),
            "not a plain, non-optional",
        ),
        (
            "the plugin only on one target",
            APP_TOML.replace(
                "tauri-plugin-log = \"2\"\n",
                "\n[target.'cfg(debug_assertions)'.dependencies]\ntauri-plugin-log = \"2\"\n",
            ),
            String::new(),
            "not a plain, non-optional",
        ),
        (
            "another logger in the app's dependencies",
            APP_TOML.replace("log = \"0.4\"", "log = \"0.4\"\nenv_logger = \"0.11\""),
            String::new(),
            "`env_logger` is a second logger",
        ),
        (
            "another logger in the app's dev-dependencies",
            format!("{APP_TOML}\n[dev-dependencies]\ntracing-subscriber = \"0.3\"\n"),
            String::new(),
            "`tracing_subscriber` is a second logger",
        ),
    ];
    for (what, app, other, says) in cases {
        let said = manifest_violations(&toml_pair(&app, &other));
        assert!(
            mentions(&said, says),
            "{what}: expected {says:?} among {said:?}"
        );
    }

    // A logger in some other crate is that crate's business, and a comment is not a setting.
    let other = "[dependencies]\nenv_logger = \"0.11\"\n# release_max_level_off would cap it\n";
    assert!(manifest_violations(&toml_pair(APP_TOML, other)).is_empty());
}
