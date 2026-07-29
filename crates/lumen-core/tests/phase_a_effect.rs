//! Phase A's effect on real files: does the budget bind, and what does it cost now?
//! Run with `--release -- --nocapture`.
use lumen_core::econ::Econ;
use lumen_core::ranked::*;
use lumen_core::structure::{detect_lang, outline};
use lumen_core::tokenizer::count_tokens;

fn root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}
fn walk(d: &std::path::Path, o: &mut Vec<std::path::PathBuf>) {
    let Ok(rd) = std::fs::read_dir(d) else { return };
    for e in rd.flatten() {
        let p = e.path();
        let n = p
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        if n.starts_with('.') || n == "target" || n == "node_modules" || n == "dist" {
            continue;
        }
        if p.is_dir() {
            walk(&p, o);
        } else if TagLang::detect(&p.to_string_lossy()).is_some() {
            o.push(p);
        }
    }
}
fn legacy_tokens(rel: &str, src: &str) -> usize {
    let items = outline(src, detect_lang(rel));
    count_tokens(&lumen_core::ranked::render(
        rel,
        src.lines().count(),
        src,
        &[],
        &[],
    )) + items
        .iter()
        .map(|i| {
            count_tokens(&format!(
                "{:>3}. {:<14} {:<32} L{}-{}\n",
                1,
                i.kind,
                i.name.as_deref().unwrap_or("(anon)"),
                i.start_line,
                i.end_line
            ))
        })
        .sum::<usize>()
}

#[test]
fn phase_a_effect() {
    let _ = count_tokens("warm");
    let mut files = Vec::new();
    walk(&root(), &mut files);
    files.sort();
    let econ = Econ::default();
    let count = |s: &str| count_tokens(s);
    println!(
        "\n{:<30} {:>7} {:>7} {:>7} {:>9} {:>8}",
        "file", "full", "legacy", "ranked", "k/n", "binds?"
    );
    println!("{}", "-".repeat(74));
    let (mut l, mut r, mut n_q, mut bound) = (0i64, 0i64, 0usize, 0usize);
    for p in &files {
        let Ok(src) = std::fs::read_to_string(p) else {
            continue;
        };
        let full = count_tokens(&src);
        let rel = p
            .strip_prefix(root())
            .unwrap()
            .to_string_lossy()
            .to_string();
        let d = ranked_outline(&rel, &src, full, &econ, &count);
        let Ok(f) = &d.outcome else { continue };
        let lt = legacy_tokens(&rel, &src);
        let b = f.k < f.n;
        if b {
            bound += 1;
        }
        println!(
            "{:<30} {:>7} {:>7} {:>7} {:>9} {:>8}",
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .chars()
                .take(29)
                .collect::<String>(),
            full,
            lt,
            f.returned_tokens,
            format!("{}/{}", f.k, f.n),
            if b { "yes" } else { "NO" }
        );
        l += lt as i64;
        r += f.returned_tokens as i64;
        n_q += 1;
    }
    println!("{}", "-".repeat(74));
    println!(
        "{n_q} qualifying: legacy={l} ranked={r} delta={:+}  budget bound in {bound}/{n_q}",
        r - l
    );
    println!();
}
