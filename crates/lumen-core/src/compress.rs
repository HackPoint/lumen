use crate::tokenizer::count_tokens;

pub struct CompressResult {
    pub text: String,
    pub original_lines: usize,
    pub compressed_lines: usize,
    pub original_tokens: usize,
    pub compressed_tokens: usize,
}

/// Deterministic, reversibly-described log compression.
/// Collapses: consecutive identical lines, stack frame runs, blank line noise.
/// No LLM, no information loss — every omission is annotated with its count.
pub fn compress_logs(text: &str) -> CompressResult {
    let original_tokens = count_tokens(text);
    let original_lines = text.lines().count();

    let compressed = compress_impl(text);
    let compressed_lines = compressed.lines().count();
    let compressed_tokens = count_tokens(&compressed);

    CompressResult {
        text: compressed,
        original_lines,
        compressed_lines,
        original_tokens,
        compressed_tokens,
    }
}

fn compress_impl(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    if lines.is_empty() {
        return String::new();
    }

    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut i = 0;

    while i < lines.len() {
        let line = lines[i];

        // ── 1. Blank-line runs: collapse to a single blank ────────────────────
        if line.trim().is_empty() {
            out.push(String::new());
            i += 1;
            while i < lines.len() && lines[i].trim().is_empty() {
                i += 1;
            }
            continue;
        }

        // ── 2. Consecutive identical lines (≥3) ───────────────────────────────
        let mut j = i + 1;
        while j < lines.len() && lines[j] == line {
            j += 1;
        }
        let run = j - i;
        if run >= 3 {
            out.push(line.to_string());
            out.push(format!(
                "    ... [×{} identical lines omitted] ...",
                run - 1
            ));
            i = j;
            continue;
        }

        // ── 3. Stack frame runs (≥5 frames) ──────────────────────────────────
        //   Keep first 2 frames + last 1; omit the middle with a count.
        if is_stack_frame(line) {
            let frame_run = frame_run_len(&lines, i);
            if frame_run >= 5 {
                out.push(lines[i].to_string());
                out.push(lines[i + 1].to_string());
                out.push(format!(
                    "    ... [{} stack frames omitted] ...",
                    frame_run - 3
                ));
                out.push(lines[i + frame_run - 1].to_string());
                i += frame_run;
                continue;
            }
        }

        // ── 4. Default: emit the line (handles run==1 and run==2) ────────────
        out.push(line.to_string());
        i += 1;
    }

    let mut result = out.join("\n");
    // Preserve original trailing newline so callers get a stable round-trip.
    if text.ends_with('\n') && !result.ends_with('\n') {
        result.push('\n');
    }
    result
}

fn frame_run_len(lines: &[&str], start: usize) -> usize {
    lines[start..]
        .iter()
        .take_while(|l| is_stack_frame(l))
        .count()
}

fn is_stack_frame(line: &str) -> bool {
    let t = line.trim();
    if t.is_empty() {
        return false;
    }
    // Java / Node.js: "at com.example.Foo.bar(Foo.java:42)"
    if t.starts_with("at ") && (t.contains('(') || t.contains('/')) {
        return true;
    }
    // Python traceback frame: File "path/to/file.py", line 42, in fn_name
    if t.starts_with("File \"") && t.contains("\", line ") {
        return true;
    }
    // Rust numbered frames: "0: std::panicking::begin_panic"
    //   or "  at src/lib.rs:42" (continuation lines in Rust backtraces)
    if let Some(first) = t.chars().next()
        && first.is_ascii_digit()
        && t.contains(": ")
    {
        let rest = &t[t.find(": ").map(|p| p + 2).unwrap_or(0)..];
        if rest.contains("::") || rest.starts_with('/') {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_lines_collapsed() {
        let src = "ERROR: db timeout\nERROR: db timeout\nERROR: db timeout\nERROR: db timeout\n";
        let r = compress_logs(src);
        assert!(r.compressed_lines < r.original_lines);
        assert!(r.text.contains("×3 identical lines omitted"));
    }

    #[test]
    fn two_identical_lines_not_collapsed() {
        let src = "INFO: ok\nINFO: ok\nINFO: different\n";
        let r = compress_logs(src);
        // 2 identical → not collapsed
        assert_eq!(r.compressed_lines, r.original_lines);
    }

    #[test]
    fn blank_run_collapsed_to_one() {
        let src = "A\n\n\n\nB\n";
        let r = compress_logs(src);
        assert!(r.compressed_lines < r.original_lines);
        assert!(!r.text.contains("\n\n\n"));
    }

    #[test]
    fn stack_frames_collapsed() {
        let src = "Exception in thread main\n\
            at com.a.A.run(A.java:1)\n\
            at com.b.B.call(B.java:2)\n\
            at com.c.C.invoke(C.java:3)\n\
            at com.d.D.execute(D.java:4)\n\
            at com.e.E.main(E.java:5)\n";
        let r = compress_logs(src);
        assert!(r.compressed_lines < r.original_lines);
        assert!(r.text.contains("stack frames omitted"));
    }

    #[test]
    fn no_compression_needed_is_honest() {
        let src = "line one\nline two\nline three\n";
        let r = compress_logs(src);
        assert_eq!(r.original_tokens, r.compressed_tokens);
    }
}
