//! Repeat reads (1.7.0): answering a second `smart_read` / `recall_file` of the same thing
//! with less than the first answer, when the model provably still holds the first one.
//!
//! The base is what Lumen returned in this session, held in memory per `(session_id,
//! req_key)` — never git HEAD. A restart loses it, which falls back to the full answer.
//!
//! Content is addressed by hash at three levels, all after CRLF→LF normalisation so a Windows
//! autocrlf checkout does not read as changed:
//! 1. the file, in git blob format: equal means "unchanged";
//! 2. each outline symbol, as a signature hash and a body hash: a changed file is answered with
//!    the symbols that changed, were added, removed or moved;
//! 3. inside one symbol, for `recall_file`: a line diff scoped to that symbol.
//!
//! Whether the base is still in the model's context is not knowable here. Compaction and
//! rewind are detected from the transcript by the daemon (see `events`); anything else is
//! handled by telling the model, in every reply, how to ask again with `fresh=true`.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};

use crate::ranked::{Mode, path_hash};
use crate::structure::CodeItem;

pub const ROUTE_UNCHANGED: &str = "smart_read_unchanged";
pub const ROUTE_DELTA: &str = "smart_read_delta";
pub const ROUTE_RECALL_UNCHANGED: &str = "recall_file_unchanged";
pub const ROUTE_RECALL_DELTA: &str = "recall_file_delta";

pub const ARM_TREATMENT: &str = "treatment";
pub const ARM_CONTROL: &str = "control";

/// `repeat_plan` values. `fresh` is the dedup error rate's numerator: the model asked again
/// because the earlier read was not in its context.
pub const PLAN_FIRST: &str = "first";
pub const PLAN_FRESH: &str = "fresh";
pub const PLAN_UNCHANGED: &str = "unchanged";
pub const PLAN_DELTA: &str = "delta";
/// The call could not be tied to a conversation, so nothing was assumed about what the model
/// holds: the full answer, and no base kept for next time.
pub const PLAN_UNIDENTIFIED: &str = "unidentified";

/// Read the rollout mode from `LUMEN_REPEAT_READS`. Anything unrecognised is `Off`.
pub fn mode_from_env() -> Mode {
    parse_mode(&std::env::var("LUMEN_REPEAT_READS").unwrap_or_default())
}

pub fn parse_mode(v: &str) -> Mode {
    match v.trim().to_ascii_lowercase().as_str() {
        "on" | "1" | "true" => Mode::On,
        "ab" => Mode::Ab,
        _ => Mode::Off,
    }
}

/// Whether this path gets the repeat-read path. The salt keeps this split independent of the
/// ranked-outline split: with the same hash, "ranked" and "repeat" would be the same files,
/// and neither experiment could be read without the other.
pub fn treated(mode: Mode, path: &str) -> bool {
    match mode {
        Mode::Off => false,
        Mode::On => true,
        Mode::Ab => !path_hash(format!("repeat\0{path}").as_bytes()).is_multiple_of(2),
    }
}

// ── hashing ──────────────────────────────────────────────────────────────────

pub type Hash = [u8; 20];

/// CRLF → LF. Everything is hashed through this.
pub fn normalize(s: &str) -> Cow<'_, str> {
    if s.contains("\r\n") {
        Cow::Owned(s.replace("\r\n", "\n"))
    } else {
        Cow::Borrowed(s)
    }
}

struct Sha1 {
    h: [u32; 5],
    buf: [u8; 64],
    len: usize,
    total: u64,
}

impl Sha1 {
    fn new() -> Self {
        Sha1 {
            h: [
                0x6745_2301,
                0xEFCD_AB89,
                0x98BA_DCFE,
                0x1032_5476,
                0xC3D2_E1F0,
            ],
            buf: [0; 64],
            len: 0,
            total: 0,
        }
    }

    fn block(&mut self, b: &[u8; 64]) {
        let mut w = [0u32; 80];
        let (words, _) = b.as_chunks::<4>();
        for (wi, c) in w.iter_mut().zip(words) {
            *wi = u32::from_be_bytes(*c);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let [mut a, mut bb, mut c, mut d, mut e] = self.h;
        for (i, wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((bb & c) | (!bb & d), 0x5A82_7999),
                20..=39 => (bb ^ c ^ d, 0x6ED9_EBA1),
                40..=59 => ((bb & c) | (bb & d) | (c & d), 0x8F1B_BCDC),
                _ => (bb ^ c ^ d, 0xCA62_C1D6u32),
            };
            let t = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(*wi);
            e = d;
            d = c;
            c = bb.rotate_left(30);
            bb = a;
            a = t;
        }
        for (h, v) in self.h.iter_mut().zip([a, bb, c, d, e]) {
            *h = h.wrapping_add(v);
        }
    }

    fn update(&mut self, mut data: &[u8]) {
        self.total += data.len() as u64;
        if self.len > 0 {
            let take = (64 - self.len).min(data.len());
            self.buf[self.len..self.len + take].copy_from_slice(&data[..take]);
            self.len += take;
            data = &data[take..];
            if self.len < 64 {
                return;
            }
            let b = self.buf;
            self.block(&b);
            self.len = 0;
        }
        let (blocks, rest) = data.as_chunks::<64>();
        for b in blocks {
            self.block(b);
        }
        self.buf[..rest.len()].copy_from_slice(rest);
        self.len = rest.len();
    }

    fn finish(mut self) -> Hash {
        let bits = self.total.wrapping_mul(8);
        self.update_pad(&[0x80]);
        while self.len != 56 {
            self.update_pad(&[0]);
        }
        self.update_pad(&bits.to_be_bytes());
        let mut out = [0u8; 20];
        let (words, _) = out.as_chunks_mut::<4>();
        for (o, h) in words.iter_mut().zip(self.h) {
            *o = h.to_be_bytes();
        }
        out
    }

    /// Padding does not count towards the message length.
    fn update_pad(&mut self, data: &[u8]) {
        let total = self.total;
        self.update(data);
        self.total = total;
    }
}

/// The hash `git hash-object` gives this content: SHA-1 of `blob <len>\0` + content, computed
/// in-process (no git binary on the hook or tool path). CRLF is normalised first.
pub fn blob_hash(content: &str) -> Hash {
    let c = normalize(content);
    let mut s = Sha1::new();
    s.update(format!("blob {}\0", c.len()).as_bytes());
    s.update(c.as_bytes());
    s.finish()
}

pub fn hex(h: &Hash) -> String {
    h.iter().map(|b| format!("{b:02x}")).collect()
}

// ── symbols ──────────────────────────────────────────────────────────────────

/// One outline symbol as delivered: where it was, and what its signature and body hashed to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sym {
    pub kind: String,
    pub name: Option<String>,
    pub start: usize,
    pub end: usize,
    pub sig: Hash,
    pub body: Hash,
    /// Whole-item hash: what identifies an anonymous item such as an import.
    pub whole: Hash,
}

/// Signature and body split at the item's body. An item with no body (an import, a const) has
/// its first line as the signature and the rest as the body, so a one-line item can only
/// change its signature.
pub fn syms<'a>(src: &str, items: impl IntoIterator<Item = &'a CodeItem>) -> Vec<Sym> {
    items
        .into_iter()
        .map(|i| {
            let text = src.get(i.start_byte..i.end_byte).unwrap_or("");
            let cut = i
                .body_start_byte
                .map(|b| b.saturating_sub(i.start_byte))
                .filter(|b| *b <= text.len() && text.is_char_boundary(*b))
                .unwrap_or_else(|| text.find('\n').unwrap_or(text.len()));
            let (sig, body) = text.split_at(cut);
            Sym {
                kind: i.kind.clone(),
                name: i.name.clone(),
                start: i.start_line,
                end: i.end_line,
                sig: blob_hash(sig),
                body: blob_hash(body),
                whole: blob_hash(text),
            }
        })
        .collect()
}

type Key = (String, Option<String>, Option<Hash>, usize);

/// Named symbols match by `(kind, name, nth occurrence)`, anonymous ones by `(kind, text,
/// nth)`: an inserted import must not turn every import below it into an edit.
fn keys(items: &[Sym]) -> Vec<Key> {
    let mut seen: HashMap<(String, Option<String>, Option<Hash>), usize> = HashMap::new();
    items
        .iter()
        .map(|s| {
            let base = (
                s.kind.clone(),
                s.name.clone(),
                if s.name.is_none() {
                    Some(s.whole)
                } else {
                    None
                },
            );
            let nth = seen.entry(base.clone()).or_insert(0);
            let key = (base.0, base.1, base.2, *nth);
            *nth += 1;
            key
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Changed {
    pub sym: Sym,
    pub sig: bool,
    pub body: bool,
    /// Its line range differs from the one the model holds.
    pub range: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Moved {
    One(Sym),
    /// Consecutive symbols that all kept their text and moved by the same offset.
    Run {
        offset: i64,
        first: Sym,
        last: Sym,
        count: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Delta {
    pub changed: Vec<Changed>,
    pub added: Vec<Sym>,
    pub removed: Vec<Sym>,
    pub moved: Vec<Moved>,
    pub unchanged: usize,
    /// `(kind, name)` pairs that occur more than once, so their entries carry a line.
    pub dup: HashSet<(String, Option<String>)>,
}

impl Delta {
    pub fn is_empty(&self) -> bool {
        self.changed.is_empty()
            && self.added.is_empty()
            && self.removed.is_empty()
            && self.moved.is_empty()
    }
}

/// Smallest run of identical shifts worth collapsing into one entry.
const MIN_RUN: usize = 3;

pub fn diff(old: &[Sym], new: &[Sym]) -> Delta {
    let (ok, nk) = (keys(old), keys(new));
    let by_key: HashMap<&Key, usize> = ok.iter().enumerate().map(|(i, k)| (k, i)).collect();
    let mut matched = vec![false; old.len()];
    let mut d = Delta::default();

    // Per new symbol, in order: how it relates to the base.
    enum Rel {
        Added,
        Same,
        Shifted(i64),
        Edited(i64),
    }
    let mut rel: Vec<Rel> = Vec::with_capacity(new.len());
    for (ni, k) in nk.iter().enumerate() {
        let n = &new[ni];
        match by_key.get(k) {
            None => {
                d.added.push(n.clone());
                rel.push(Rel::Added);
            }
            Some(&oi) => {
                matched[oi] = true;
                let o = &old[oi];
                let off = n.start as i64 - o.start as i64;
                let (sig, body) = (o.sig != n.sig, o.body != n.body);
                let range = (o.start, o.end) != (n.start, n.end);
                if sig || body {
                    d.changed.push(Changed {
                        sym: n.clone(),
                        sig,
                        body,
                        range,
                    });
                    rel.push(Rel::Edited(off));
                } else if range {
                    rel.push(Rel::Shifted(off));
                } else {
                    d.unchanged += 1;
                    rel.push(Rel::Same);
                }
            }
        }
    }
    for (oi, m) in matched.iter().enumerate() {
        if !m {
            d.removed.push(old[oi].clone());
        }
    }

    // Moved symbols, collapsing a run of identical shifts. A symbol that was edited but
    // shifted by the same offset sits inside a run without ending it.
    let mut run: Vec<usize> = vec![];
    let mut run_off = 0i64;
    let flush = |run: &mut Vec<usize>, off: i64, d: &mut Delta| {
        if run.len() >= MIN_RUN {
            d.moved.push(Moved::Run {
                offset: off,
                first: new[run[0]].clone(),
                last: new[*run.last().unwrap()].clone(),
                count: run.len(),
            });
        } else {
            for &i in run.iter() {
                d.moved.push(Moved::One(new[i].clone()));
            }
        }
        d.unchanged += run.len();
        run.clear();
    };
    for (i, r) in rel.iter().enumerate() {
        match r {
            Rel::Shifted(off) => {
                if !run.is_empty() && *off != run_off {
                    flush(&mut run, run_off, &mut d);
                }
                run_off = *off;
                run.push(i);
            }
            Rel::Edited(off) if !run.is_empty() && *off == run_off => {}
            _ => flush(&mut run, run_off, &mut d),
        }
    }
    flush(&mut run, run_off, &mut d);

    // A name used twice within one version of the file needs its line to be told apart.
    for side in [old, new] {
        let mut count: HashMap<(&str, &str), usize> = HashMap::new();
        for s in side {
            if let Some(n) = &s.name {
                *count.entry((&s.kind, n)).or_insert(0) += 1;
            }
        }
        d.dup.extend(
            count
                .into_iter()
                .filter(|(_, c)| *c > 1)
                .map(|((k, n), _)| (k.to_string(), Some(n.to_string()))),
        );
    }
    d
}

// ── what the model is told ───────────────────────────────────────────────────

/// The sentence every unchanged answer carries. The hook names the caller's context, but what
/// happens inside it cannot all be seen (a compaction in a subagent, an event not yet
/// ingested), so a model can be told "unchanged" about a read it no longer holds; this is how
/// it gets out.
pub fn unchanged_text(tool: &str) -> String {
    if tool == "smart_read" {
        "Unchanged since an earlier read of this file in this session. If that read is not in \
         your context, call smart_read(path, fresh=true).\n"
            .to_string()
    } else {
        format!(
            "Unchanged since an earlier read of this file in this session. If that read is not \
             in your context, call {tool}(path, fresh=true) with the same arguments.\n"
        )
    }
}

fn changed_header(tool: &str) -> String {
    format!(
        "Changed since an earlier read of this file in this session. If that read is not in \
         your context, call {tool}(path, fresh=true).\n"
    )
}

fn disp(s: &Sym, dup: &HashSet<(String, Option<String>)>) -> String {
    match &s.name {
        None => format!("{}@L{}", s.kind, s.start),
        Some(n) if dup.contains(&(s.kind.clone(), s.name.clone())) => format!("{n}@L{}", s.start),
        Some(n) => n.clone(),
    }
}

/// Entries are grouped by marker, one line each, names comma-separated: an entry costs its
/// name and a separator, not a line of an outline. A range appears only where it differs from
/// the one the model already holds, and always for an added symbol.
pub fn format_delta(new_lines: usize, d: &Delta) -> String {
    let mut b = changed_header("smart_read");
    b.push_str(&format!("{} unchanged; {new_lines} lines.\n", d.unchanged));
    let range = |s: &Sym| format!(" L{}-{}", s.start, s.end);
    // Where the range is printed it already tells two same-named symbols apart.
    let named = |s: &Sym| match &s.name {
        Some(n) => n.clone(),
        None => s.kind.clone(),
    };
    for (label, pick) in [
        ("changed [body]", (false, true)),
        ("changed [signature]", (true, false)),
        ("changed [signature, body]", (true, true)),
    ] {
        let items: Vec<String> = d
            .changed
            .iter()
            .filter(|c| (c.sig, c.body) == pick)
            .map(|c| {
                if c.range {
                    format!("{}{}", named(&c.sym), range(&c.sym))
                } else {
                    disp(&c.sym, &d.dup)
                }
            })
            .collect();
        if !items.is_empty() {
            b.push_str(&format!("{label}: {}\n", items.join(", ")));
        }
    }
    if !d.added.is_empty() {
        let items: Vec<String> = d
            .added
            .iter()
            .map(|s| format!("{}{}", named(s), range(s)))
            .collect();
        b.push_str(&format!("added: {}\n", items.join(", ")));
    }
    if !d.removed.is_empty() {
        let items: Vec<String> = d.removed.iter().map(|s| disp(s, &d.dup)).collect();
        b.push_str(&format!("removed: {}\n", items.join(", ")));
    }
    let singles: Vec<String> = d
        .moved
        .iter()
        .filter_map(|m| match m {
            Moved::One(s) => Some(format!("{}{}", named(s), range(s))),
            Moved::Run { .. } => None,
        })
        .collect();
    if !singles.is_empty() {
        b.push_str(&format!("moved: {}\n", singles.join(", ")));
    }
    for m in &d.moved {
        if let Moved::Run {
            offset,
            first,
            last,
            count,
        } = m
        {
            b.push_str(&format!(
                "moved {offset:+} lines: {}..{} ({count} symbols)\n",
                disp(first, &d.dup),
                disp(last, &d.dup)
            ));
        }
    }
    if d.is_empty() {
        b.push_str(
            "No symbol changed: the edit is between symbols, in a comment, or whitespace.\n",
        );
    }
    b
}

// ── inside a symbol ──────────────────────────────────────────────────────────

/// Longest cell count the line diff will fill. Past it the caller sends the whole body.
const MAX_DIFF_CELLS: usize = 4_000_000;

/// Zero-context line diff, hunks headed `@@ L<new line>`. `None` when the two are too large
/// to diff cheaply. `first_line` is the line number of `new[0]` in the file.
pub fn line_diff(old: &[&str], new: &[&str], first_line: usize) -> Option<String> {
    let pre = old.iter().zip(new).take_while(|(a, b)| a == b).count();
    let suf = old[pre..]
        .iter()
        .rev()
        .zip(new[pre..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let (o, n) = (&old[pre..old.len() - suf], &new[pre..new.len() - suf]);
    if o.len().saturating_mul(n.len()) > MAX_DIFF_CELLS {
        return None;
    }
    // LCS table over the differing middle.
    let w = n.len() + 1;
    let mut t = vec![0u32; (o.len() + 1) * w];
    for i in (0..o.len()).rev() {
        for j in (0..n.len()).rev() {
            t[i * w + j] = if o[i] == n[j] {
                t[(i + 1) * w + j + 1] + 1
            } else {
                t[(i + 1) * w + j].max(t[i * w + j + 1])
            };
        }
    }
    let mut out = String::new();
    let (mut i, mut j) = (0, 0);
    let mut open = false;
    while i < o.len() || j < n.len() {
        if i < o.len() && j < n.len() && o[i] == n[j] {
            open = false;
            i += 1;
            j += 1;
            continue;
        }
        if !open {
            out.push_str(&format!("@@ L{}\n", first_line + pre + j));
            open = true;
        }
        if j >= n.len() || (i < o.len() && t[(i + 1) * w + j] >= t[i * w + j + 1]) {
            out.push_str(&format!("-{}\n", o[i]));
            i += 1;
        } else {
            out.push_str(&format!("+{}\n", n[j]));
            j += 1;
        }
    }
    Some(out)
}

/// The header sentence for a recall_file delta, for the caller to put in front of its blocks.
pub fn recall_changed_header(tool: &str) -> String {
    changed_header(tool)
}

/// A range request (`recall_file(start_line, end_line)`) as a piece: the requested range is
/// its identity, the lines it returned are its content.
pub fn range_sym(start: usize, end: usize, text: &str) -> Sym {
    Sym {
        kind: "range".into(),
        name: Some(format!("L{start}-{end}")),
        start,
        end,
        sig: blob_hash(""),
        body: blob_hash(text),
        whole: blob_hash(text),
    }
}

/// The delta for a `recall_file` repeat: each requested piece that changed, as a diff scoped
/// to that piece under its own first line, or whole when the diff is not smaller; pieces that
/// were added, only moved or went missing; a count of the rest.
///
/// `gutter` renders one source line the way the first answer did, for a piece sent whole.
pub fn recall_delta(
    tool: &str,
    old: &RecallBase,
    new: &[(Sym, String)],
    gutter: &dyn Fn(usize, &str) -> String,
) -> String {
    let old_syms: Vec<Sym> = old.pieces.iter().map(|(s, _)| s.clone()).collect();
    let new_syms: Vec<Sym> = new.iter().map(|(s, _)| s.clone()).collect();
    let (ok, nk) = (keys(&old_syms), keys(&new_syms));
    let by_key: HashMap<&Key, usize> = ok.iter().enumerate().map(|(i, k)| (k, i)).collect();
    let mut matched = vec![false; old.pieces.len()];
    let label = |s: &Sym| match (s.kind.as_str(), &s.name) {
        ("range", Some(n)) => n.clone(),
        (_, Some(n)) => format!("{n} L{}-{}", s.start, s.end),
        (k, None) => format!("{k} L{}-{}", s.start, s.end),
    };
    let whole = |s: &Sym, text: &str| {
        text.lines()
            .enumerate()
            .map(|(i, l)| gutter(s.start + i, l))
            .collect::<String>()
    };

    let mut unchanged = 0usize;
    let mut moved: Vec<String> = vec![];
    let mut entries = String::new();
    for (i, (s, text)) in new.iter().enumerate() {
        let Some(&oi) = by_key.get(&nk[i]) else {
            entries.push_str(&format!("added {}:\n{}", label(s), whole(s, text)));
            continue;
        };
        matched[oi] = true;
        let (o, otext) = &old.pieces[oi];
        let (sig, body) = (o.sig != s.sig, o.body != s.body);
        if !sig && !body {
            if (o.start, o.end) == (s.start, s.end) {
                unchanged += 1;
            } else {
                moved.push(label(s));
            }
            continue;
        }
        let what = match (sig, body) {
            (true, true) => "signature, body",
            (true, false) => "signature",
            _ => "body",
        };
        let hunks = otext.as_ref().and_then(|ot| {
            let ol: Vec<&str> = ot.lines().collect();
            let nl: Vec<&str> = text.lines().collect();
            line_diff(&ol, &nl, s.start).filter(|h| h.len() < text.len())
        });
        match hunks {
            Some(h) => {
                let head = text.lines().next().unwrap_or("").trim_end();
                entries.push_str(&format!("changed [{what}] {}: {head}\n{h}", label(s)));
            }
            None => entries.push_str(&format!(
                "changed [{what}] {}:\n{}",
                label(s),
                whole(s, text)
            )),
        }
    }
    let removed: Vec<String> = old_syms
        .iter()
        .zip(&matched)
        .filter(|(_, m)| !**m)
        .map(|(s, _)| label(s))
        .collect();

    let mut b = changed_header(tool);
    b.push_str(&format!("{unchanged} requested pieces unchanged.\n"));
    if !moved.is_empty() {
        b.push_str(&format!("moved: {}\n", moved.join(", ")));
    }
    if !removed.is_empty() {
        b.push_str(&format!("removed: {}\n", removed.join(", ")));
    }
    b.push_str(&entries);
    b
}

// ── the base ─────────────────────────────────────────────────────────────────

/// What `smart_read` last delivered for a path.
#[derive(Debug, Clone)]
pub struct OutlineBase {
    pub when_ms: i64,
    pub file: Hash,
    pub lines: usize,
    /// Empty when the whole file was delivered: there is nothing to diff against.
    pub syms: Vec<Sym>,
}

/// What `recall_file` last delivered for one request: the hash of the reply, and each piece
/// served, with its text so a changed one can be diffed.
#[derive(Debug, Clone)]
pub struct RecallBase {
    pub when_ms: i64,
    pub text: Hash,
    pub pieces: Vec<(Sym, Option<String>)>,
}

#[derive(Debug, Clone)]
pub enum Base {
    Outline(OutlineBase),
    Recall(RecallBase),
}

impl Base {
    pub fn when_ms(&self) -> i64 {
        match self {
            Base::Outline(b) => b.when_ms,
            Base::Recall(b) => b.when_ms,
        }
    }
}

/// Entries kept. A session that reads more distinct things than this loses the oldest, which
/// only costs those a repeat their saving.
const CAP: usize = 512;
/// Piece text kept per recall entry; past it the pieces are kept without text.
const MAX_PIECE_BYTES: usize = 256 * 1024;

#[derive(Default)]
pub struct Store {
    map: HashMap<(String, String), (u64, Base)>,
    seq: u64,
    /// The conversation the bases in `map` were delivered to, when it is known.
    conversation: Option<String>,
}

impl Store {
    /// Declare whose call is about to be answered. Bases belong to one conversation, so a call
    /// from any other, or from one that could not be identified, drops every base before it
    /// is looked at: a `/clear` or a `/resume` keeps this process alive and gives the model
    /// an empty context, and a call that cannot be placed may be from either. Returns how many
    /// bases were dropped.
    ///
    /// Subagents share their parent's session id and are told apart by the key, not here: a
    /// subagent starting must not cost the main agent its bases.
    pub fn enter(&mut self, session: Option<&str>) -> usize {
        if session.is_some() && self.conversation.as_deref() == session {
            return 0;
        }
        let dropped = self.map.len();
        // A fresh map, not `clear()`: the point is to give the memory back.
        self.map = HashMap::new();
        self.conversation = session.map(str::to_string);
        dropped
    }

    pub fn get(&self, owner: &str, key: &str) -> Option<&Base> {
        self.map
            .get(&(owner.to_string(), key.to_string()))
            .map(|(_, b)| b)
    }

    pub fn put(&mut self, owner: &str, key: &str, base: Base) {
        self.seq += 1;
        self.map
            .insert((owner.to_string(), key.to_string()), (self.seq, base));
        if self.map.len() > CAP
            && let Some(oldest) = self
                .map
                .iter()
                .min_by_key(|(_, (s, _))| *s)
                .map(|(k, _)| k.clone())
        {
            self.map.remove(&oldest);
        }
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

/// The process-wide base. A stdio server is per Claude Code process, not per conversation:
/// `/clear` starts a new conversation in the same process. So the base is keyed by an owner
/// (the session, and inside a subagent which one: see `identity::Identity::owner`), and
/// [`Store::enter`] empties it when the conversation changes.
pub fn store() -> &'static Mutex<Store> {
    static S: OnceLock<Mutex<Store>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(Store::default()))
}

/// Truncate piece text to the per-entry budget.
pub fn bound_pieces(pieces: Vec<(Sym, String)>) -> Vec<(Sym, Option<String>)> {
    let mut used = 0usize;
    pieces
        .into_iter()
        .map(|(s, t)| {
            used += t.len();
            (s, (used <= MAX_PIECE_BYTES).then_some(t))
        })
        .collect()
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or_else(
            |e| -(e.duration().as_millis() as i64),
            |d| d.as_millis() as i64,
        )
}

// ── invalidation ─────────────────────────────────────────────────────────────

/// Whether the session has a compaction or a rewind recorded after `since_ms`.
///
/// `Err` when the ledger cannot say (no ledger, or no events table): the caller treats that
/// as "possibly", the same as a yes.
pub fn event_since(
    db: &std::path::Path,
    session: &str,
    since_ms: i64,
) -> rusqlite::Result<Option<String>> {
    use rusqlite::OptionalExtension;
    let conn = crate::meter::open_read_only(db)?;
    conn.query_row(
        "SELECT kind FROM session_events WHERE session_id = ?1 AND ts_ms >= ?2 \
         ORDER BY ts_ms LIMIT 1",
        rusqlite::params![session, since_ms],
        |r| r.get(0),
    )
    .optional()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::structure::{detect_lang, outline};

    // Vectors from `git hash-object`.
    #[test]
    fn blob_hash_matches_git() {
        assert_eq!(
            hex(&blob_hash("")),
            "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391"
        );
        assert_eq!(
            hex(&blob_hash("hello\n")),
            "ce013625030ba8dba906f756967f9e9ca394464a"
        );
        assert_eq!(
            hex(&blob_hash("what is up, doc?")),
            "bd9dbf5aae1a3862dd1526723246b20206e5fc37"
        );
    }

    #[test]
    fn sha1_handles_block_boundaries() {
        // `blob <n>\0` is 7-8 bytes, so these straddle the 56- and 64-byte padding edges.
        // Vectors are `git hash-object` of n 'a's.
        for (n, want) in [
            (1, "2e65efe2a145dda7ee51d1741299f848e5bf752e"),
            (43, "94c8020b1a20b3d8d05f83911e8904a2dca45114"),
            (44, "e2a756bc77aa071621b4728b3e707a0a2de1dba1"),
            (55, "d1985ddc2983785702b9a90effd5aff2f7cfdca4"),
            (56, "1f973e890f52da1f22fa7e5620a628bc4ee74cb3"),
            (57, "d28c6cde5a5ab0b138bd53a288c23e8c011c4171"),
            (63, "487e57f9763ebddbed2027c4452510c0ae0f95ff"),
            (64, "71b7a71962774fa5c721e1163f935cb61a0e09e6"),
            (65, "44171c1993ae70a544a8e910b00347eed4b8e8f2"),
            (119, "1325c3d76ea6a66e75dce490a0b8cb8b12233ee0"),
            (120, "253af86ee859905852357aee8bd83b08601364ae"),
            (128, "b9ec37e648dcec4d4f60dd267d05079a696d3267"),
            (1000, "a50be72b20f0e3f078d252e8e56b11b4bec67509"),
        ] {
            assert_eq!(hex(&blob_hash(&"a".repeat(n))), want, "length {n}");
        }
    }

    // SHA-1 pads with 0x80 then a 64-bit length, so the message (the `blob <n>\0`
    // header plus the content) changes shape at 55/56 and 63/64 bytes, and again at
    // 119/120. Keyed by that total length, with the content length derived from it.
    // Vectors are `git hash-object --stdin` of n 'a's.
    #[test]
    fn blob_hash_matches_git_at_padding_boundaries() {
        for (total, want) in [
            (55, "5e3bf7e629b4908cce461530c17a64365cb47303"),
            (56, "12d42395b020f44bb7710113b31f688d0ebeda7c"),
            (63, "d1985ddc2983785702b9a90effd5aff2f7cfdca4"),
            (64, "1f973e890f52da1f22fa7e5620a628bc4ee74cb3"),
            (119, "1fcb14c5910397220c4f8d0dd2203c70116bd4e0"),
            (120, "d64a843f44190f041194ba87abf3cedc76eeb30e"),
        ] {
            let n = (0..total)
                .find(|n| format!("blob {n}\0").len() + n == total)
                .expect("a content length for this total");
            assert_eq!(hex(&blob_hash(&"a".repeat(n))), want, "total {total}");
        }
    }

    // `blob_hash` reads CRLF as LF, so both forms hash to git's hash of the LF bytes
    // (`printf 'fn a() {\n    1\n}\n' | git hash-object --stdin`).
    #[test]
    fn crlf_and_lf_match_git_hash_of_the_lf_form() {
        let want = "1b39008c1d5d99135aa9a38e91862276809edcc4";
        assert_eq!(hex(&blob_hash("fn a() {\n    1\n}\n")), want);
        assert_eq!(hex(&blob_hash("fn a() {\r\n    1\r\n}\r\n")), want);
    }

    #[test]
    fn crlf_and_lf_copies_hash_equal() {
        let lf = "fn a() {\n    1\n}\n";
        let crlf = lf.replace('\n', "\r\n");
        assert_eq!(blob_hash(lf), blob_hash(&crlf));
        assert_ne!(blob_hash(lf), blob_hash("fn a() {\n    2\n}\n"));
    }

    #[test]
    fn crlf_and_lf_copies_have_equal_symbols() {
        let lf = "fn a() {\n    1\n}\n\nfn b(x: u8) {\n    x\n}\n";
        let crlf = lf.replace('\n', "\r\n");
        let (i1, i2) = (
            outline(lf, detect_lang("a.rs")),
            outline(&crlf, detect_lang("a.rs")),
        );
        assert_eq!(syms(lf, &i1), syms(&crlf, &i2));
    }

    fn rs(src: &str) -> Vec<Sym> {
        syms(src, &outline(src, detect_lang("a.rs")))
    }

    #[test]
    fn a_body_edit_a_signature_edit_add_remove_and_move_are_told_apart() {
        let old = "fn keep() {\n    1\n}\n\nfn body() {\n    1\n}\n\nfn sig(a: u8) {\n    1\n}\n\nfn gone() {\n    1\n}\n\nfn tail() {\n    9\n}\n";
        let new = "fn added() {\n    0\n}\n\nfn keep() {\n    1\n}\n\nfn body() {\n    2\n}\n\nfn sig(a: u16) {\n    1\n}\n\nfn tail() {\n    9\n}\n";
        let d = diff(&rs(old), &rs(new));
        let names = |v: &[Sym]| v.iter().filter_map(|s| s.name.clone()).collect::<Vec<_>>();
        assert_eq!(names(&d.added), ["added"]);
        assert_eq!(names(&d.removed), ["gone"]);
        let ch: Vec<_> = d
            .changed
            .iter()
            .map(|c| (c.sym.name.clone().unwrap(), c.sig, c.body))
            .collect();
        assert_eq!(
            ch,
            [
                ("body".to_string(), false, true),
                ("sig".to_string(), true, false)
            ]
        );
        // keep was only shifted: moved, not changed. tail kept its lines: unchanged.
        let moved: Vec<_> = d
            .moved
            .iter()
            .flat_map(|m| match m {
                Moved::One(s) => vec![s.name.clone().unwrap()],
                Moved::Run { .. } => vec![],
            })
            .collect();
        assert_eq!(moved, ["keep"]);
        let text = format_delta(20, &d);
        assert!(text.contains("changed [body]: body"), "{text}");
        assert!(text.contains("changed [signature]: sig"), "{text}");
        assert!(text.contains("added: added L1-3"), "{text}");
        assert!(text.contains("removed: gone"), "{text}");
        assert!(text.contains("moved: keep"), "{text}");
        assert!(text.contains("fresh=true"), "{text}");
        assert!(!text.contains("changed [body]: keep"), "{text}");
    }

    #[test]
    fn a_long_run_of_shifted_symbols_is_one_entry() {
        let mut old = String::new();
        for i in 0..20 {
            old.push_str(&format!("fn f{i}() {{\n    {i}\n}}\n\n"));
        }
        let new = format!("fn inserted() {{}}\n\n{old}");
        let d = diff(&rs(&old), &rs(&new));
        assert_eq!(d.added.len(), 1);
        assert_eq!(d.moved.len(), 1, "{:?}", d.moved);
        assert!(matches!(
            d.moved[0],
            Moved::Run {
                count: 20,
                offset: 2,
                ..
            }
        ));
        assert_eq!(d.unchanged, 20);
        let t = format_delta(82, &d);
        assert!(t.contains("moved +2 lines: f0..f19 (20 symbols)"), "{t}");
    }

    #[test]
    fn line_diff_names_only_the_changed_lines() {
        let o = ["a", "b", "c", "d", "e"];
        let n = ["a", "b", "X", "d", "e", "f"];
        let d = line_diff(&o, &n, 10).unwrap();
        assert_eq!(d, "@@ L12\n-c\n+X\n@@ L15\n+f\n");
        assert_eq!(line_diff(&o, &o, 1).unwrap(), "");
    }

    #[test]
    fn a_recall_delta_is_a_diff_inside_the_changed_piece_only() {
        let g = |n: usize, l: &str| format!("{n}|{l}\n");
        let old_src = "fn a() {\n    1;\n    2;\n    3;\n}\n\nfn b() {\n    9;\n}\n";
        let new_src = "fn a() {\n    1;\n    5;\n    3;\n}\n\nfn b() {\n    9;\n}\n";
        let piece = |src: &str, name: &str| {
            let items = outline(src, detect_lang("a.rs"));
            let it: Vec<&CodeItem> = items
                .iter()
                .filter(|i| i.name.as_deref() == Some(name))
                .collect();
            let sym = syms(src, it.iter().copied()).remove(0);
            let lines: Vec<&str> = src.lines().collect();
            let text = lines[sym.start - 1..sym.end].join("\n");
            (sym, text)
        };
        let base = RecallBase {
            when_ms: 0,
            text: [0; 20],
            pieces: ["a", "b"]
                .map(|n| {
                    let (s, t) = piece(old_src, n);
                    (s, Some(t))
                })
                .to_vec(),
        };
        let new: Vec<_> = ["a", "b"].map(|n| piece(new_src, n)).to_vec();
        let d = recall_delta("recall_file", &base, &new, &g);
        assert!(d.contains("1 requested pieces unchanged"), "{d}");
        assert!(d.contains("changed [body] a L1-5: fn a() {"), "{d}");
        assert!(d.contains("@@ L3\n-    2;\n+    5;\n"), "{d}");
        assert!(!d.contains("fn b"), "{d}");
        assert!(d.contains("fresh=true"), "{d}");
    }

    #[test]
    fn the_store_is_bounded() {
        let mut s = Store::default();
        for i in 0..(CAP + 10) {
            s.put(
                "s",
                &i.to_string(),
                Base::Outline(OutlineBase {
                    when_ms: 0,
                    file: [0; 20],
                    lines: 0,
                    syms: vec![],
                }),
            );
        }
        assert_eq!(s.len(), CAP);
        assert!(s.get("s", "0").is_none() && s.get("s", &(CAP + 9).to_string()).is_some());
    }

    fn a_base() -> Base {
        Base::Outline(OutlineBase {
            when_ms: 0,
            file: [0; 20],
            lines: 0,
            syms: vec![],
        })
    }

    #[test]
    fn a_new_conversation_empties_the_store_and_the_same_one_does_not() {
        let mut s = Store::default();
        assert_eq!(s.enter(Some("one")), 0);
        s.put("one", "k", a_base());
        s.put("one\0agent", "k", a_base());
        assert_eq!(
            s.enter(Some("one")),
            0,
            "the same conversation keeps its bases"
        );
        assert_eq!(s.len(), 2);

        assert_eq!(s.enter(Some("two")), 2);
        assert!(
            s.is_empty(),
            "/clear: nothing of the old conversation is held"
        );
        s.put("two", "k", a_base());
        // Coming back to the first is /resume: its bases went when it was left.
        assert_eq!(s.enter(Some("one")), 1);
        assert!(s.get("one", "k").is_none() && s.get("two", "k").is_none());
    }

    #[test]
    fn a_call_that_cannot_be_placed_drops_everything_and_so_does_the_next_one() {
        let mut s = Store::default();
        s.enter(Some("one"));
        s.put("one", "k", a_base());
        assert_eq!(s.enter(None), 1);
        assert_eq!(s.enter(None), 0);
        // Even the conversation it was in: the unplaced call may have been in another.
        s.enter(Some("one"));
        assert!(s.is_empty());
        s.put("one", "k", a_base());
        assert_eq!(s.enter(Some("one")), 0);
        assert!(s.get("one", "k").is_some());
    }

    #[test]
    fn mode_parses() {
        assert_eq!(parse_mode("on"), Mode::On);
        assert_eq!(parse_mode("AB"), Mode::Ab);
        assert_eq!(parse_mode(""), Mode::Off);
        assert_eq!(parse_mode("nonsense"), Mode::Off);
    }
}
