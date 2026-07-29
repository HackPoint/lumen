//! Does the budget rule leave anything to do?
//!
//! `S_min` scales with context size, so at the measured mean context the bar is high. This
//! walks every source file in the repository and reports what fraction would be
//! intercepted at several context sizes, and what the ranked outline actually saves on the
//! ones that qualify. Run with `--release -- --nocapture`.

use lumen_core::econ::Econ;
use lumen_core::ranked::*;
use lumen_core::tokenizer::count_tokens;

fn root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        let name = p
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        if name.starts_with('.') || name == "target" || name == "node_modules" || name == "dist" {
            continue;
        }
        if p.is_dir() {
            walk(&p, out);
        } else if TagLang::detect(&p.to_string_lossy()).is_some() {
            out.push(p);
        }
    }
}

#[test]
fn coverage_and_savings_by_context_size() {
    let _ = count_tokens("warm");
    let mut files = Vec::new();
    walk(&root(), &mut files);
    files.sort();

    // Token count per file, once.
    let mut sized: Vec<(std::path::PathBuf, String, usize)> = Vec::new();
    for p in files {
        if let Ok(src) = std::fs::read_to_string(&p) {
            let t = count_tokens(&src);
            sized.push((p, src, t));
        }
    }
    println!("\n{} source files scanned\n", sized.len());

    println!(
        "{:>9} {:>9} {:>10} {:>9} {:>12} {:>12}",
        "context", "S_min", "qualifying", "%", "tok saved", "avg k/n"
    );
    println!("{}", "-".repeat(66));

    for ctx in [50_000.0, 100_000.0, 200_000.0, 362_965.0, 600_000.0] {
        let econ = Econ {
            context_tokens: ctx,
            ..Default::default()
        };
        let s_min = econ.s_min().unwrap();
        let count = |s: &str| count_tokens(s);

        let mut qualifying = 0usize;
        let mut saved_total: i64 = 0;
        let mut k_sum = 0usize;
        let mut n_sum = 0usize;

        for (p, src, full) in &sized {
            let rel = p
                .strip_prefix(root())
                .unwrap()
                .to_string_lossy()
                .to_string();
            let d = ranked_outline(&rel, src, *full, &econ, &count);
            if let Ok(f) = &d.outcome {
                qualifying += 1;
                saved_total += *full as i64 - f.returned_tokens as i64;
                k_sum += f.k;
                n_sum += f.n;
            }
        }

        println!(
            "{:>9.0} {:>9.0} {:>10} {:>8.1}% {:>12} {:>11}",
            ctx,
            s_min,
            qualifying,
            100.0 * qualifying as f64 / sized.len() as f64,
            saved_total,
            format!(
                "{}/{}",
                k_sum.checked_div(qualifying).unwrap_or(0),
                n_sum.checked_div(qualifying).unwrap_or(0)
            ),
        );
    }

    // Where does the size distribution sit relative to the bar?
    let mut tokens: Vec<usize> = sized.iter().map(|(_, _, t)| *t).collect();
    tokens.sort_unstable();
    let pct = |q: f64| tokens[((tokens.len() as f64 - 1.0) * q) as usize];
    println!(
        "\nfile size percentiles (tokens): p50={} p75={} p90={} p95={} max={}",
        pct(0.50),
        pct(0.75),
        pct(0.90),
        pct(0.95),
        tokens.last().unwrap()
    );
    println!();
}
