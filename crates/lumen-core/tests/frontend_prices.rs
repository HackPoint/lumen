//! The frontend holds no price of its own: every dollar figure in the app is priced from the
//! table in `lumen_core::rates`, which reaches it through the `lumen_prices` command.
//!
//! Until 1.9.0 the app priced everything from a `RATE` object in
//! `lumenator/src/app/components/index.ts` that held one model's prices, Opus-class, so a
//! Sonnet session cost 1.7 times what it did and a Haiku one five times. It was a copy of the
//! Rust constants, kept in step by a comment. This fails on the shapes a second copy would
//! take, in any shipped TypeScript or template:
//!
//!   - the name `RATE`;
//!   - a number divided by a million, or written in millionths: a per-token price;
//!   - a price-named field set to a number other than zero (`input: 5`, `cacheWrite: 6.25`):
//!     a per-million table;
//!   - a number with a fraction equal to one the table prices at (6.25, 0.5, 3.75 …), which
//!     ordinary code has no reason to write, or to the multiplier US-only inference puts on
//!     them (1.1), which the table sends as `usOnly`;
//!   - two numbers in a row equal to a model's input and output price (`5, 25`, `3, 15`): a
//!     positional table.
//!
//! The last three are read in code only, with comments and string contents blanked, so a
//! version number in a comment or an SVG attribute in a template string is not a price. A
//! table of whole numbers under names this does not know would still pass: the case the guard
//! cannot see is a copy that hides, which is what `the_frontend_asks_the_backend_for_its_prices`
//! and the specs that price a made-up table through the fake bridge are for.
//!
//! A Rust test for a TypeScript concern, for the reason `frontend_styling.rs` gives: it needs
//! the filesystem, which the Angular test environment does not have. Spec files are not read,
//! since a spec has to feed the app a price table to test it, and neither is the fake bridge,
//! a test double that holds the specs' table; `the_fake_bridge_ships_in_nothing` checks that
//! nothing but a spec imports it.

use std::path::{Path, PathBuf};

use lumen_core::rates::{PRICES, US_ONLY, price_for, price_key};

fn workspace_root() -> Option<PathBuf> {
    let mut dir: Option<&Path> = Some(Path::new(env!("CARGO_MANIFEST_DIR")));
    while let Some(d) = dir {
        if d.join("Cargo.toml").is_file() && d.join("crates").is_dir() {
            return Some(d.to_path_buf());
        }
        dir = d.parent();
    }
    None
}

/// The test double the specs drive the app through. It holds their price table.
const FAKE_BRIDGE: &str = "tauri-bridge.fake.ts";

/// Every `.ts` and `.html` file under `dir` that ships in the app.
fn shipped_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned());
        let Some(name) = name else { continue };
        if path.is_dir() {
            shipped_sources(&path, out);
        } else if (name.ends_with(".ts") && !name.ends_with(".spec.ts") && name != FAKE_BRIDGE)
            || name.ends_with(".html")
        {
            out.push(path);
        }
    }
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$'
}

/// `RATE` as a whole identifier.
fn names_rate(line: &str) -> bool {
    let b = line.as_bytes();
    let mut from = 0;
    while let Some(i) = line[from..].find("RATE") {
        let at = from + i;
        let before = at == 0 || !is_word_byte(b[at - 1]);
        let after = at + 4 >= b.len() || !is_word_byte(b[at + 4]);
        if before && after {
            return true;
        }
        from = at + 4;
    }
    false
}

/// A number divided by a million (`5.0 / 1_000_000`, `25 / 1e6`) or written in millionths
/// (`5e-6`): the shape of a per-token price.
fn per_token_literal(line: &str) -> bool {
    let b = line.as_bytes();
    for million in ["1_000_000", "1000000", "1e6"] {
        let mut from = 0;
        while let Some(i) = line[from..].find(million) {
            let at = from + i;
            from = at + million.len();
            // Walk back over spaces to a '/', then over spaces to the numerator.
            let mut j = at;
            while j > 0 && b[j - 1] == b' ' {
                j -= 1;
            }
            if j == 0 || b[j - 1] != b'/' {
                continue;
            }
            j -= 1;
            while j > 0 && b[j - 1] == b' ' {
                j -= 1;
            }
            if j > 0 && (b[j - 1].is_ascii_digit() || b[j - 1] == b'.') {
                return true;
            }
        }
    }
    for millionths in ["e-6", "e-7", "E-6", "E-7"] {
        let mut from = 0;
        while let Some(i) = line[from..].find(millionths) {
            let at = from + i;
            from = at + millionths.len();
            if at > 0 && b[at - 1].is_ascii_digit() {
                return true;
            }
        }
    }
    false
}

/// `src` with comments and the contents of string and template literals blanked, so what
/// is left is code. Line breaks are kept, so a line number still points at the source.
///
/// A small lexer, not a parser: enough for the TypeScript this repository writes, including
/// `${…}` inside a template, whose contents are code. A regular expression literal is read as
/// code, which can only make the scan stricter.
fn code_only(src: &str) -> String {
    #[derive(Clone, Copy, PartialEq)]
    enum In {
        Code,
        Line,
        Block,
        Quote(u8),
        Template,
    }
    let b = src.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    // The template each open `${` returns to, with the brace depth inside it.
    let mut stack: Vec<usize> = Vec::new();
    let mut depth = 0usize;
    let mut state = In::Code;
    let mut i = 0;
    let blank = |c: u8| if c == b'\n' { b'\n' } else { b' ' };
    while i < b.len() {
        let c = b[i];
        let next = b.get(i + 1).copied();
        match state {
            In::Code => match c {
                b'/' if next == Some(b'/') => {
                    state = In::Line;
                    out.extend(*b"  ");
                    i += 2;
                    continue;
                }
                b'/' if next == Some(b'*') => {
                    state = In::Block;
                    out.extend(*b"  ");
                    i += 2;
                    continue;
                }
                b'\'' | b'"' => {
                    state = In::Quote(c);
                    out.push(c);
                }
                b'`' => {
                    state = In::Template;
                    out.push(c);
                }
                b'{' => {
                    depth += 1;
                    out.push(c);
                }
                b'}' if depth == 0 && !stack.is_empty() => {
                    depth = stack.pop().unwrap_or_default();
                    state = In::Template;
                    out.push(c);
                }
                b'}' => {
                    depth = depth.saturating_sub(1);
                    out.push(c);
                }
                _ => out.push(c),
            },
            In::Line => {
                if c == b'\n' {
                    state = In::Code;
                }
                out.push(blank(c));
            }
            In::Block => {
                if c == b'*' && next == Some(b'/') {
                    state = In::Code;
                    out.extend(*b"  ");
                    i += 2;
                    continue;
                }
                out.push(blank(c));
            }
            In::Quote(q) => {
                if c == b'\\' {
                    out.push(b' ');
                    if let Some(n) = next {
                        out.push(blank(n));
                    }
                    i += 2;
                    continue;
                }
                if c == q || c == b'\n' {
                    state = In::Code;
                    out.push(c);
                } else {
                    out.push(blank(c));
                }
            }
            In::Template => {
                if c == b'\\' {
                    out.push(b' ');
                    if let Some(n) = next {
                        out.push(blank(n));
                    }
                    i += 2;
                    continue;
                }
                if c == b'`' {
                    state = In::Code;
                    out.push(c);
                } else if c == b'$' && next == Some(b'{') {
                    stack.push(depth);
                    depth = 0;
                    state = In::Code;
                    out.extend(*b"${");
                    i += 2;
                    continue;
                } else {
                    out.push(blank(c));
                }
            }
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A numeric literal in code: where it starts and its value.
struct Num {
    at: usize,
    end: usize,
    value: f64,
    fractional: bool,
}

/// Every numeric literal in `code`, which must already be [`code_only`].
fn numbers(code: &str) -> Vec<Num> {
    let b = code.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let starts =
            b[i].is_ascii_digit() && (i == 0 || !(is_word_byte(b[i - 1]) || b[i - 1] == b'.'));
        if !starts {
            i += 1;
            continue;
        }
        let at = i;
        while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'_') {
            i += 1;
        }
        let mut fractional = false;
        if i + 1 < b.len() && b[i] == b'.' && b[i + 1].is_ascii_digit() {
            fractional = true;
            i += 1;
            while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'_') {
                i += 1;
            }
        }
        if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
            let mut j = i + 1;
            if j < b.len() && (b[j] == b'+' || b[j] == b'-') {
                j += 1;
            }
            if j < b.len() && b[j].is_ascii_digit() {
                i = j;
                while i < b.len() && b[i].is_ascii_digit() {
                    i += 1;
                }
            }
        }
        let text: String = code[at..i].chars().filter(|&c| c != '_').collect();
        if let Ok(value) = text.parse::<f64>() {
            out.push(Num {
                at,
                end: i,
                value,
                fractional,
            });
        }
    }
    out
}

/// Every price the table holds, at either speed.
fn shipped_prices() -> Vec<[f64; 5]> {
    PRICES
        .iter()
        .flat_map(|row| [row.prices, row.fast])
        .flatten()
        .map(|p| {
            [
                p.input,
                p.output,
                p.cache_write_5m,
                p.cache_write_1h,
                p.cache_read,
            ]
        })
        .collect()
}

const PRICE_FIELDS: [&str; 6] = [
    "input",
    "output",
    "cacheRead",
    "cacheWrite",
    "cacheWrite5m",
    "cacheWrite1h",
];

/// What in `code` (already [`code_only`]) looks like a price table, one message per finding,
/// each with the byte offset it was found at.
fn price_shapes(code: &str) -> Vec<(usize, String)> {
    let b = code.as_bytes();
    let prices = shipped_prices();
    let fractions: Vec<f64> = prices
        .iter()
        .flatten()
        .copied()
        .filter(|v| v.fract() != 0.0)
        .collect();
    let pairs: Vec<(f64, f64)> = prices.iter().map(|p| (p[0], p[1])).collect();
    let nums = numbers(code);
    let mut out = Vec::new();
    for (k, n) in nums.iter().enumerate() {
        // A price-named field set to this number.
        let mut j = n.at;
        while j > 0 && b[j - 1] == b' ' {
            j -= 1;
        }
        if j > 0 && b[j - 1] == b':' && n.value != 0.0 {
            let mut e = j - 1;
            while e > 0 && b[e - 1] == b' ' {
                e -= 1;
            }
            let mut s = e;
            while s > 0 && is_word_byte(b[s - 1]) {
                s -= 1;
            }
            let field = &code[s..e];
            if PRICE_FIELDS.contains(&field) {
                out.push((
                    n.at,
                    format!("a price field set to a number: {field}: {}", n.value),
                ));
            }
        }
        // A fraction the table prices at.
        if n.fractional && fractions.contains(&n.value) {
            out.push((n.at, format!("{} is a price the table holds", n.value)));
        }
        // What US-only inference multiplies a price by, which the table sends as `usOnly`.
        if n.fractional && n.value == US_ONLY {
            out.push((
                n.at,
                format!(
                    "{} is US-only inference's multiplier, which the table holds",
                    n.value
                ),
            ));
        }
        // An input and output price, side by side.
        if let Some(m) = nums.get(k + 1) {
            let between = code[n.end..m.at].trim();
            if between == "," && pairs.contains(&(n.value, m.value)) {
                out.push((
                    n.at,
                    format!(
                        "{}, {} is a model's input and output price",
                        n.value, m.value
                    ),
                ));
            }
        }
    }
    out
}

#[test]
fn the_shapes_are_recognised() {
    // The scan is only as good as these: the first lines are ones the old frontend had.
    assert!(names_rate("export { RATE };"));
    assert!(names_rate("t.input * RATE.input"));
    assert!(
        !names_rate("const RATE_LIMIT = 3;"),
        "a longer name is not the rate table"
    );
    assert!(!names_rate("separate"));
    assert!(per_token_literal("    input: 5.0 / 1_000_000,"));
    assert!(per_token_literal(
        "cacheWrite: 6.25 / 1_000_000,   // 1.25× input"
    ));
    assert!(per_token_literal("const p = 25 / 1e6;"));
    assert!(per_token_literal("INPUT = 5e-6"));
    assert!(
        !per_token_literal("['claude-opus-4-8', 1_000_000],"),
        "a context window"
    );
    assert!(
        !per_token_literal("tokens * p.input / 1_000_000"),
        "a price from the table"
    );

    let shapes = |src: &str| price_shapes(&code_only(src)).len();
    // A per-million table: every field flagged, and its fractions twice over.
    assert!(shapes("const T = { input: 5, output: 25, cacheRead: 0.5, cacheWrite: 6.25 };") >= 4);
    assert_eq!(
        shapes("const T = { input: 3, output: 15 };"),
        2,
        "both fields"
    );
    assert!(
        shapes("const opus = [5, 25, 6.25, 10, 0.5];") >= 3,
        "a positional table"
    );
    assert!(shapes("x = 3.75;") == 1, "a bare price fraction");
    assert_eq!(
        shapes("const p = row.prices.input * 1.1;"),
        1,
        "US-only's multiplier"
    );
    // What ordinary code writes is not a table.
    assert_eq!(
        shapes("const out = { input: 0, output: 0, cacheRead: 0 };"),
        0
    );
    assert_eq!(shapes("if (out.length >= 5) return 0.95;"), 0);
    assert_eq!(
        shapes("readonly cost = computed(() => p.input * t.input);"),
        0
    );
    assert_eq!(
        shapes(r#"<ellipse rx="3" ry="2.5"/>"#),
        0,
        "inside a string"
    );
    assert_eq!(
        shapes("const html = `<ellipse ry=\"2.5\"/>`;"),
        0,
        "inside a template"
    );
    assert_eq!(
        shapes("// priced at 6.25 per million\nx = 1;"),
        0,
        "inside a comment"
    );
    assert_eq!(shapes("/* 1.25 and 0.5 */ y = 2;"), 0);
    assert_eq!(
        shapes("const s = `${a ? 6.25 : 0}`;"),
        1,
        "the code inside a template is read"
    );
}

#[test]
fn no_frontend_source_carries_a_price_of_its_own() {
    let Some(root) = workspace_root() else { return };
    let app = root.join("lumenator/src");
    if !app.is_dir() {
        return; // No frontend checked out.
    }
    let mut files = Vec::new();
    shipped_sources(&app, &mut files);
    files.sort();

    let mut problems = Vec::new();
    for file in &files {
        let Ok(text) = std::fs::read_to_string(file) else {
            continue;
        };
        let rel = file
            .strip_prefix(&root)
            .unwrap_or(file)
            .display()
            .to_string();
        for (n, line) in text.lines().enumerate() {
            if names_rate(line) {
                problems.push(format!("  {rel}:{}: names RATE: {}", n + 1, line.trim()));
            } else if per_token_literal(line) {
                problems.push(format!(
                    "  {rel}:{}: a per-token price: {}",
                    n + 1,
                    line.trim()
                ));
            }
        }
        if rel.ends_with(".ts") {
            let code = code_only(&text);
            for (at, what) in price_shapes(&code) {
                let line = code[..at].matches('\n').count() + 1;
                problems.push(format!("  {rel}:{line}: {what}"));
            }
        }
    }

    // A walk that matched nothing would pass without having looked.
    assert!(
        files.len() >= 20,
        "expected at least 20 frontend sources under {}, found {}",
        app.display(),
        files.len()
    );
    assert!(
        problems.is_empty(),
        "the frontend has prices of its own. Price from the `lumen_prices` command, which \
         serves lumen_core::rates::PRICES:\n{}",
        problems.join("\n")
    );
}

#[test]
fn the_fake_bridge_ships_in_nothing() {
    // It is left out of the scan above because it holds the specs' price table. That is only
    // sound while nothing the app is built from imports it.
    let Some(root) = workspace_root() else { return };
    let app = root.join("lumenator/src");
    if !app.is_dir() {
        return;
    }
    let mut files = Vec::new();
    shipped_sources(&app, &mut files);
    let importers: Vec<String> = files
        .iter()
        .filter(|f| std::fs::read_to_string(f).is_ok_and(|t| t.contains("tauri-bridge.fake")))
        .map(|f| f.display().to_string())
        .collect();
    assert!(
        importers.is_empty(),
        "shipped code imports the fake bridge: {importers:?}"
    );
}

#[test]
fn the_frontend_asks_the_backend_for_its_prices() {
    let Some(root) = workspace_root() else { return };
    let app = root.join("lumenator/src");
    if !app.is_dir() {
        return;
    }
    let mut files = Vec::new();
    shipped_sources(&app, &mut files);
    let asks = files
        .iter()
        .any(|f| std::fs::read_to_string(f).is_ok_and(|t| t.contains("'lumen_prices'")));
    assert!(asks, "no shipped frontend source invokes 'lumen_prices'");

    let backend = std::fs::read_to_string(root.join("lumenator/src-tauri/src/lib.rs"))
        .expect("the Tauri backend is in the same checkout as its frontend");
    let handlers = backend
        .split("generate_handler![")
        .nth(1)
        .and_then(|rest| rest.split(']').next())
        .expect("lib.rs registers its commands with generate_handler!");
    assert!(
        handlers.split(',').any(|h| h.trim() == "lumen_prices"),
        "lumen_prices is not registered with the Tauri invoke handler"
    );
}

#[test]
fn rust_and_the_frontend_resolve_model_ids_from_one_case_list() {
    // `priceKey` in lumenator/src/app/prices.ts mirrors `price_key`. Both are tested against
    // this file, so a change to one that the other does not make fails one side.
    let Some(root) = workspace_root() else { return };
    let cases_path = root.join("lumenator/src/app/price-key.cases.json");
    let Ok(text) = std::fs::read_to_string(&cases_path) else {
        return; // No frontend checked out.
    };
    let doc: serde_json::Value = serde_json::from_str(&text).expect("the case list is JSON");
    let cases = doc["cases"].as_array().expect("a cases array");
    assert!(cases.len() > 10, "too few cases to mean anything");
    for case in cases {
        let id = case["id"].as_str().expect("id");
        let key = case["key"].as_str().expect("key");
        let priced = case["priced"].as_bool().expect("priced");
        assert_eq!(price_key(id), key, "price_key({id:?})");
        assert_eq!(price_for(id).is_some(), priced, "price_for({id:?})");
    }

    let spec = std::fs::read_to_string(root.join("lumenator/src/app/prices.spec.ts"))
        .expect("the frontend's price spec");
    assert!(
        spec.contains("from './price-key.cases.json'"),
        "prices.spec.ts no longer reads the shared case list"
    );
}
