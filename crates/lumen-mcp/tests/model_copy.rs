//! The copy the model reads, audited for figures.
//!
//! The model routes on what it is told. The tool descriptions and this repository's
//! CLAUDE.md promised "~5-10% token cost", "typically 40-80% token reduction" and
//! "3000–4500 tokens" a read, and the compress_logs description promised "no information
//! loss" while it dropped stack frames. None of it came from the ledger. A figure the model
//! is given has to be one measured for the call in front of it, and every tool reply
//! carries that already, so this copy carries none. The intercept's message is checked the
//! same way in `hook.rs`.

use serde_json::Value;

/// Each figure in `text`: a percentage, an approximate number, a token count, or a
/// "typically".
fn figures_in(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    let chars: Vec<char> = text.chars().collect();
    let around = |i: usize| -> String {
        let from = i.saturating_sub(30);
        let to = (i + 30).min(chars.len());
        chars[from..to].iter().collect()
    };
    for (i, c) in chars.iter().enumerate() {
        let digit_before = i > 0 && chars[i - 1].is_ascii_digit();
        let digit_after = chars.get(i + 1).is_some_and(char::is_ascii_digit);
        if (*c == '%' && digit_before) || (*c == '~' && digit_after) {
            found.push(around(i));
        }
    }
    let words: Vec<&str> = text
        .split_whitespace()
        .map(|w| w.trim_matches(|c: char| c == '*' || c == ',' || c == '.'))
        .collect();
    for pair in words.windows(2) {
        if pair[1] == "tokens" && pair[0].ends_with(|c: char| c.is_ascii_digit()) {
            found.push(format!("{} {}", pair[0], pair[1]));
        }
    }
    if text.to_lowercase().contains("typically") {
        found.push("typically".to_string());
    }
    found
}

/// Every `description` in a tool list, with the path to it.
fn descriptions(v: &Value, at: &str, out: &mut Vec<(String, String)>) {
    match v {
        Value::Object(map) => {
            for (k, child) in map {
                let here = format!("{at}/{k}");
                match (k.as_str(), child) {
                    ("description", Value::String(s)) => out.push((here, s.clone())),
                    _ => descriptions(child, &here, out),
                }
            }
        }
        Value::Array(items) => {
            for (i, child) in items.iter().enumerate() {
                descriptions(child, &format!("{at}/{i}"), out);
            }
        }
        _ => {}
    }
}

#[test]
fn the_figure_check_finds_the_figures_this_copy_used_to_carry() {
    for old in [
        "returns an outline at ~5-10% token cost of reading the full file",
        "deterministically (typically 40-80% token reduction)",
        "Each smart_read on a large file saves **3000–4500 tokens** vs. Read.",
        "a session uses ~10% of the context",
    ] {
        assert!(!figures_in(old).is_empty(), "missed the figure in: {old}");
    }
    assert!(figures_in("files ≥300 lines, reported in _meta.saved_tokens").is_empty());
}

#[test]
fn the_tool_descriptions_carry_no_figures() {
    let mut found = Vec::new();
    descriptions(&lumen_mcp::handle_tools_list(), "", &mut found);
    assert!(found.len() > 4, "no descriptions found: {found:?}");
    for (at, text) in &found {
        let figures = figures_in(text);
        assert!(figures.is_empty(), "{at} carries {figures:?}: {text}");
    }
}

#[test]
fn compress_logs_does_not_claim_to_be_lossless() {
    let mut found = Vec::new();
    descriptions(&lumen_mcp::handle_tools_list(), "", &mut found);
    let (_, text) = found
        .iter()
        .find(|(at, _)| at == "/tools/3/description")
        .expect("compress_logs is the fourth tool");
    assert!(
        text.starts_with("Deterministically compact a log"),
        "{text}"
    );
    for claim in ["no information loss", "fully reversible", "just compaction"] {
        assert!(!text.contains(claim), "says {claim:?}: {text}");
    }
    assert!(
        text.contains("dropped"),
        "does not say what it drops: {text}"
    );
}

#[test]
fn this_repository_s_claude_md_carries_no_figures() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../CLAUDE.md");
    let text = std::fs::read_to_string(path).expect("CLAUDE.md at the workspace root");
    assert!(
        text.contains("lumen:smart_read"),
        "not the routing rules: {path}"
    );
    let figures = figures_in(&text);
    assert!(figures.is_empty(), "CLAUDE.md carries {figures:?}");
}
