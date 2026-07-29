//! Where does the 10 ms go? Run with `--release -- --nocapture`.

use lumen_core::ranked::*;
use lumen_core::tokenizer::count_tokens;
use std::time::Instant;

fn root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

#[test]
fn stage_timings() {
    let files = [
        "crates/lumen-core/src/structure.rs",
        "crates/lumen-mcp/src/lib.rs",
        "lumenator/src-tauri/src/setup.rs",
        "lumenator/src/app/session.service.ts",
    ];
    // Warm both one-time costs before measuring: the tokenizer loads its vocabulary on
    // first use, and each language's tag query compiles once per process. In the MCP
    // server both are already paid before this code runs — full_tokens is counted first,
    // and the server is long-lived — so including them here would measure startup, not
    // the per-call cost the 10 ms ceiling is about.
    let _ = count_tokens("warm");
    for l in [
        TagLang::Rust,
        TagLang::Python,
        TagLang::TypeScript,
        TagLang::Tsx,
    ] {
        let _ = extract_tags("", l);
    }

    println!(
        "\n{:<26} {:>6} {:>8} {:>8} {:>8} {:>8} {:>8} {:>6}",
        "file", "lines", "parse+q", "graph", "prank", "fit", "TOTAL", "counts"
    );

    for rel in files {
        let p = root().join(rel);
        let Ok(src) = std::fs::read_to_string(&p) else {
            continue;
        };
        let lines = src.lines().count();
        let lang = TagLang::detect(rel).unwrap();

        let t0 = Instant::now();
        let (defs, refs) = extract_tags(&src, lang).unwrap();
        let t_parse = t0.elapsed();

        let t1 = Instant::now();
        let graph = build_graph(&defs, &refs);
        let t_graph = t1.elapsed();

        let t2 = Instant::now();
        let pr = prior(&defs, &graph);
        let scores = pagerank(&pr, &graph);
        let ranking = rank(&defs, &scores);
        let t_prank = t2.elapsed();

        let calls = std::cell::Cell::new(0usize);
        let count = |s: &str| {
            calls.set(calls.get() + 1);
            count_tokens(s)
        };
        let t3 = Instant::now();
        let _ = fit_budget(rel, lines, &src, &defs, &ranking, 100_000, &count);
        let t_fit = t3.elapsed();

        println!(
            "{:<26} {:>6} {:>7.2}ms {:>6.2}ms {:>6.2}ms {:>6.2}ms {:>6.2}ms {:>6}",
            p.file_name().unwrap().to_string_lossy(),
            lines,
            t_parse.as_secs_f64() * 1e3,
            t_graph.as_secs_f64() * 1e3,
            t_prank.as_secs_f64() * 1e3,
            t_fit.as_secs_f64() * 1e3,
            (t_parse + t_graph + t_prank + t_fit).as_secs_f64() * 1e3,
            calls.get(),
        );
    }
    println!();
}
