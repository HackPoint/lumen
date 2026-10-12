//! Two deterministic choices the benchmark makes per file, both from the file's own outline:
//! the item `recall_file` is asked for, and the small edit that turns a repeat read into a delta.
//!
//! Neither is random and neither depends on the machine: the outline comes from the same
//! tree-sitter grammars the server uses, so a corpus at a given commit yields the same choices
//! everywhere. Both are single rules and the README states them; a different rule would give
//! different figures, which is why only one is used.

use crate::corpus::Lang;
use lumen_core::repeat::{Sym, syms};
use lumen_core::structure::{self, CodeItem};

/// Item kinds a person would ask `recall_file` for by name.
const ITEM_KINDS: &[&str] = &[
    "function",
    "class",
    "struct",
    "enum",
    "trait",
    "interface",
    "const",
];

/// An item shorter than this is a declaration, not something worth recalling.
const MIN_ITEM_LINES: usize = 3;

/// How many lines the edit tries before giving up on a file.
const MAX_EDIT_TRIES: usize = 60;

fn structure_lang(l: Lang) -> structure::Lang {
    match l {
        Lang::Rust => structure::Lang::Rust,
        Lang::Python => structure::Lang::Python,
        Lang::TypeScript => structure::Lang::TypeScript,
        Lang::Tsx => structure::Lang::Tsx,
    }
}

pub fn items(src: &str, lang: Lang) -> Vec<CodeItem> {
    structure::outline(src, structure_lang(lang))
}

/// The item chosen for `recall_file`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rep {
    /// Index into the file's outline items.
    pub idx: usize,
    pub name: String,
    pub start: usize,
    pub end: usize,
}

/// The representative item: among the file's top-level items that are a function, class,
/// struct, enum, trait, interface or named const, span at least three lines, and whose name
/// (compared case-insensitively, as `recall_file` does) no other item in the file shares, the
/// one in the MIDDLE when ordered by line span (the lower median; ties by position in the file).
///
/// Unique, so the call returns exactly one item. Median-sized, so the figure is neither the
/// best case (a three-line helper) nor the worst (the file's largest class). `None` when the
/// file has no such item, and the file then has no recall or delta figure.
pub fn representative(items: &[CodeItem]) -> Option<Rep> {
    let mut count = std::collections::BTreeMap::<String, usize>::new();
    for i in items {
        if let Some(n) = &i.name {
            *count.entry(n.to_lowercase()).or_default() += 1;
        }
    }
    let mut cands: Vec<(usize, usize, usize)> = items
        .iter()
        .enumerate()
        .filter(|(_, i)| ITEM_KINDS.contains(&i.kind.as_str()))
        .filter(|(_, i)| i.end_line + 1 - i.start_line >= MIN_ITEM_LINES)
        .filter(|(_, i)| {
            i.name
                .as_ref()
                .is_some_and(|n| count.get(&n.to_lowercase()) == Some(&1))
        })
        .map(|(idx, i)| (i.end_line + 1 - i.start_line, i.start_line, idx))
        .collect();
    if cands.is_empty() {
        return None;
    }
    cands.sort_unstable();
    let (_, _, idx) = cands[(cands.len() - 1) / 2];
    let it = &items[idx];
    Some(Rep {
        idx,
        name: it.name.clone()?,
        start: it.start_line,
        end: it.end_line,
    })
}

/// The edit applied to a file between the second and third read.
#[derive(Debug, Clone)]
pub struct Edit {
    pub new_src: String,
    /// The 1-based line that was changed.
    pub line: usize,
}

fn shape(items: &[CodeItem]) -> Vec<(String, Option<String>, usize, usize)> {
    items
        .iter()
        .map(|i| (i.kind.clone(), i.name.clone(), i.start_line, i.end_line))
        .collect()
}

/// The small edit: a trailing comment, `// lumen-bench edit` (`# ...` in Python), on one line of
/// the representative item's body, the line nearest the middle of the item that works.
///
/// "Works" is checked, not assumed. The edited file must parse to the same outline (same kinds,
/// names and line ranges) and differ from the original in exactly one symbol, the representative
/// item. Anything else (a line inside a string, a line continuation) is skipped and the next
/// nearest line is tried. Line endings are left alone: a CRLF line keeps its CR after the
/// comment. `None` when no line within the item qualifies; the file then has no delta figure.
pub fn small_edit(src: &str, lang: Lang, rep: &Rep) -> Option<Edit> {
    let old_items = items(src, lang);
    let item = old_items.get(rep.idx)?;
    let old_syms = syms(src, &old_items);
    let old_shape = shape(&old_items);

    let lines: Vec<&str> = src.split('\n').collect();
    let mut starts = Vec::with_capacity(lines.len());
    let mut at = 0usize;
    for l in &lines {
        starts.push(at);
        at += l.len() + 1;
    }

    let first = item.start_line;
    let last = item.end_line;
    let mid = first + (last - first) / 2;
    let mut order: Vec<usize> = (first..=last)
        .filter(|&n| {
            let in_body = match item.body_start_byte {
                Some(b) => starts.get(n - 1).is_some_and(|s| *s >= b),
                None => n > first,
            };
            let text = lines.get(n - 1).map_or("", |l| l.trim());
            in_body
                && !text.is_empty()
                && !text.ends_with('\\')
                && !text.contains("lumen-bench edit")
        })
        .collect();
    order.sort_by_key(|&n| (n.abs_diff(mid), n));

    for &n in order.iter().take(MAX_EDIT_TRIES) {
        let line = lines[n - 1];
        let (body, cr) = match line.strip_suffix('\r') {
            Some(b) => (b, "\r"),
            None => (line, ""),
        };
        let edited = format!("{body}  {} lumen-bench edit{cr}", lang.comment());
        let mut out = String::with_capacity(src.len() + 32);
        for (i, l) in lines.iter().enumerate() {
            if i > 0 {
                out.push('\n');
            }
            out.push_str(if i + 1 == n { &edited } else { l });
        }
        let new_items = items(&out, lang);
        if shape(&new_items) != old_shape {
            continue;
        }
        let new_syms = syms(&out, &new_items);
        if differs_only_at(&old_syms, &new_syms, rep.idx) {
            return Some(Edit {
                new_src: out,
                line: n,
            });
        }
    }
    None
}

fn differs_only_at(a: &[Sym], b: &[Sym], idx: usize) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b)
            .enumerate()
            .all(|(i, (x, y))| (x == y) != (i == idx))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rust(defs: usize) -> String {
        let mut s = String::from("use std::fmt;\n\n");
        for i in 0..defs {
            s.push_str(&format!(
                "pub fn f{i}(x: &str) -> usize {{\n    let a = x.len() + {i};\n    let b = a * 2;\n    b\n}}\n\n"
            ));
        }
        s
    }

    #[test]
    fn the_representative_is_the_median_sized_unique_item() {
        let mut s = String::new();
        // spans 3, 5, 9 lines: the middle one is `mid`.
        s.push_str("fn small() {\n    1;\n}\n");
        s.push_str("fn mid() {\n    1;\n    2;\n    3;\n}\n");
        s.push_str("fn large() {\n    1;\n    2;\n    3;\n    4;\n    5;\n    6;\n    7;\n}\n");
        let it = items(&s, Lang::Rust);
        let rep = representative(&it).unwrap();
        assert_eq!(rep.name, "mid");
        assert_eq!((rep.start, rep.end), (4, 8));
    }

    #[test]
    fn an_item_whose_name_is_shared_is_not_representative() {
        let s = "fn twice() {\n    1;\n    2;\n}\nfn twice() {\n    1;\n    2;\n}\n";
        assert_eq!(representative(&items(s, Lang::Rust)), None);
    }

    #[test]
    fn a_file_with_nothing_to_recall_has_no_representative() {
        assert_eq!(representative(&items("use std::fmt;\n", Lang::Rust)), None);
    }

    #[test]
    fn the_edit_changes_one_body_line_and_nothing_else() {
        let s = rust(6);
        let it = items(&s, Lang::Rust);
        let rep = representative(&it).unwrap();
        let e = small_edit(&s, Lang::Rust, &rep).unwrap();
        let (a, b): (Vec<&str>, Vec<&str>) =
            (s.split('\n').collect(), e.new_src.split('\n').collect());
        assert_eq!(a.len(), b.len());
        let changed: Vec<usize> = (0..a.len()).filter(|&i| a[i] != b[i]).collect();
        assert_eq!(changed, vec![e.line - 1]);
        assert!(b[e.line - 1].ends_with("  // lumen-bench edit"));
        assert!(e.line > rep.start && e.line <= rep.end);
    }

    #[test]
    fn the_edit_is_deterministic() {
        let s = rust(9);
        let rep = representative(&items(&s, Lang::Rust)).unwrap();
        let a = small_edit(&s, Lang::Rust, &rep).unwrap();
        let b = small_edit(&s, Lang::Rust, &rep).unwrap();
        assert_eq!((a.line, a.new_src), (b.line, b.new_src));
    }

    #[test]
    fn python_gets_a_hash_comment_and_crlf_is_preserved() {
        let s = "def work(x):\r\n    a = x + 1\r\n    b = a * 2\r\n    return b\r\n";
        let rep = representative(&items(s, Lang::Python)).unwrap();
        let e = small_edit(s, Lang::Python, &rep).unwrap();
        let line = e.new_src.split('\n').nth(e.line - 1).unwrap();
        assert!(line.ends_with("  # lumen-bench edit\r"), "{line:?}");
        assert_eq!(
            e.new_src.matches("\r\n").count(),
            4,
            "no line ending changed"
        );
    }

    #[test]
    fn a_line_continuation_is_never_the_edit() {
        let s = "def work(x):\n    a = x + \\\n        1\n    b = a * 2\n    return b\n";
        let rep = representative(&items(s, Lang::Python)).unwrap();
        let e = small_edit(s, Lang::Python, &rep).unwrap();
        assert_ne!(e.line, 2, "the continuation line must be skipped");
    }
}
