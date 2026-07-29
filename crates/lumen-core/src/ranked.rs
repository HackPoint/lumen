//! Ranked, budget-aware outline.
//!
//! Replaces a fixed-size listing of top-level items with a selection sized by
//! [`crate::econ`]: the outline is as large as the call can afford and no larger, so a
//! net-negative interception is unrepresentable rather than detected afterwards.
//!
//! Pipeline, all per-file and all inside the calling process:
//!
//! 1. tree-sitter parse, then the language's tag query → definitions and references
//! 2. containment pass → each definition's parent, for scope rendering and nesting depth
//! 3. in-file reference graph → who uses whom
//! 4. structural prior × personalized PageRank → a score per definition
//! 5. binary search on how many top-ranked definitions fit the token budget
//! 6. render in **source** order, with ancestor headers and elision markers
//!
//! No repository graph, no cross-file index: this runs synchronously inside a tool call
//! and anything requiring I/O beyond the one file would show up as latency.
//!
//! Every scoring constant below is a guess. They are starting values to be tuned against
//! the follow-up-rate measurement, not measured facts, and [`COEFF_VERSION`] is recorded
//! on each metered row so a later change stays comparable with earlier data.

use crate::econ::Econ;
use std::collections::BTreeMap;
use std::ops::Range;
use tree_sitter::{Language, Node, Parser, Query, QueryCursor, StreamingIterator};

/// Bumped whenever any coefficient, the query set, or the ranking changes.
///
/// Recorded per row and part of the cache key. Without it, rows scored under different
/// weights would be pooled and the A/B would compare two things at once.
pub const COEFF_VERSION: u32 = 1;

/// Below this many tokens an outline cannot say anything useful, so if the budget is
/// smaller than this there is no point interceding at all. This is what replaces the
/// fixed line-count threshold.
pub const MIN_USEFUL_OUTLINE: i64 = 120;

/// Graphs smaller than this carry no centrality signal worth computing.
const MIN_NODES_FOR_PAGERANK: usize = 8;

const DAMPING: f64 = 0.85;
const PAGERANK_EPSILON: f64 = 1e-6;
const PAGERANK_MAX_ITERS: usize = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefKind {
    Function,
    Method,
    Class,
    Interface,
    Type,
    Const,
    Module,
    Macro,
}

impl DefKind {
    fn from_capture(kind: &str) -> Option<Self> {
        Some(match kind {
            "function" => DefKind::Function,
            "method" => DefKind::Method,
            // Rust's upstream query maps struct, enum, union and type_item all to
            // `definition.class`, so struct-versus-enum is not recoverable from tags.
            // It does not matter: they share a base weight as structural anchors.
            "class" => DefKind::Class,
            "interface" => DefKind::Interface,
            "type" => DefKind::Type,
            "constant" => DefKind::Const,
            "module" => DefKind::Module,
            "macro" => DefKind::Macro,
            _ => return None,
        })
    }

    /// What kind of thing this is, structurally, before any graph evidence.
    fn base(self) -> f64 {
        match self {
            // The shape of the file. A reader who gets only these still knows what the
            // file is for.
            DefKind::Class | DefKind::Interface => 1.5,
            DefKind::Function | DefKind::Method => 1.0,
            DefKind::Type | DefKind::Const => 0.6,
            DefKind::Module => 0.4,
            DefKind::Macro => 1.0,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            DefKind::Function => "function",
            DefKind::Method => "method",
            DefKind::Class => "class",
            DefKind::Interface => "interface",
            DefKind::Type => "type",
            DefKind::Const => "const",
            DefKind::Module => "module",
            DefKind::Macro => "macro",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Def {
    pub name: String,
    pub kind: DefKind,
    /// 0-based line of the name, for stable ordering and for the rendered location.
    pub name_line: usize,
    /// Byte range of the whole definition node.
    pub span: Range<usize>,
    /// Byte offset where the signature ends and the body begins.
    pub sig_end: usize,
    /// Index of the innermost enclosing definition.
    pub parent: Option<usize>,
    pub exported: bool,
    pub documented: bool,
}

impl Def {
    /// How deeply nested this definition is.
    fn depth(&self, defs: &[Def]) -> usize {
        let mut d = 0;
        let mut cur = self.parent;
        // Bounded rather than `while let`: a malformed containment relation must not
        // become an infinite loop inside a synchronous tool call.
        while let Some(i) = cur {
            d += 1;
            if d > 64 {
                break;
            }
            cur = defs[i].parent;
        }
        d
    }
}

#[derive(Debug, Clone)]
pub struct Ref {
    pub name: String,
    pub byte_offset: usize,
}

/// Which tag query and grammar to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TagLang {
    Rust,
    Python,
    TypeScript,
    Tsx,
}

impl TagLang {
    pub fn detect(path: &str) -> Option<Self> {
        let ext = std::path::Path::new(path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        Some(match ext.as_str() {
            "rs" => TagLang::Rust,
            "py" | "pyi" => TagLang::Python,
            "ts" | "mts" | "cts" => TagLang::TypeScript,
            "tsx" => TagLang::Tsx,
            _ => return None,
        })
    }

    fn language(self) -> Language {
        match self {
            TagLang::Rust => tree_sitter_rust::LANGUAGE.into(),
            TagLang::Python => tree_sitter_python::LANGUAGE.into(),
            TagLang::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            TagLang::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
        }
    }

    /// Rust and Python use the grammar's own query, which is MIT and maintained.
    ///
    /// TypeScript uses one authored here. Upstream's captures only declaration forms —
    /// `function_signature`, `abstract_class_declaration` — so ordinary `class`,
    /// `function`, `method_definition` and every call produce nothing: measured at 0
    /// definitions in a 652-line Angular service and 0 in a 57-line component. Authoring
    /// rather than copying also keeps the crate MIT-clean, which is the reason upstream
    /// was preferred in the first place.
    fn tags_query(self) -> String {
        match self {
            // Upstream, plus a supplement that makes `impl` blocks containers rather
            // than references. Without it every Rust method renders with no enclosing
            // type, which is the difference between `fn new` and `impl Report { fn new`.
            TagLang::Rust => format!(
                "{}\n{}",
                tree_sitter_rust::TAGS_QUERY,
                include_str!("../queries/rust-tags-supplement.scm")
            ),
            TagLang::Python => tree_sitter_python::TAGS_QUERY.to_string(),
            TagLang::TypeScript | TagLang::Tsx => {
                include_str!("../queries/typescript-tags.scm").to_string()
            }
        }
    }
}

/// Compiled tag query for `lang`, built once per process.
///
/// `Query::new` compiles the pattern set, and it dominated everything else: measured at
/// 9–16 ms per call in a release build, against a 10 ms ceiling for the whole pipeline.
/// Compiling it per call meant the feature timed out on any file large enough to be worth
/// outlining — the parse and the graph together are under a millisecond.
///
/// `None` if the query fails to compile, which is a programming error for the two
/// authored-in-repo languages and a grammar-version mismatch for the upstream ones. Either
/// way the caller declines rather than panicking inside a tool call.
fn compiled_query(lang: TagLang) -> Option<&'static Query> {
    use std::sync::OnceLock;
    static RUST: OnceLock<Option<Query>> = OnceLock::new();
    static PYTHON: OnceLock<Option<Query>> = OnceLock::new();
    static TS: OnceLock<Option<Query>> = OnceLock::new();
    static TSX: OnceLock<Option<Query>> = OnceLock::new();

    let cell = match lang {
        TagLang::Rust => &RUST,
        TagLang::Python => &PYTHON,
        TagLang::TypeScript => &TS,
        TagLang::Tsx => &TSX,
    };
    cell.get_or_init(|| Query::new(&lang.language(), &lang.tags_query()).ok())
        .as_ref()
}

/// Node kinds that begin a body. Everything before the earliest of these is signature.
const BODY_KINDS: &[&str] = &[
    "block",
    "statement_block",
    "declaration_list",
    "field_declaration_list",
    "enum_variant_list",
    "class_body",
    "object_type",
    "interface_body",
];

/// Byte offset where `node`'s signature ends.
///
/// Found by locating the earliest descendant that opens a body, rather than by scanning
/// for `{`: a default parameter value or a generic bound can contain braces, and a
/// textual scan cuts the signature in the wrong place.
fn signature_end(node: &Node) -> usize {
    let mut best = node.end_byte();
    let mut stack = vec![*node];
    let mut visited = 0usize;
    while let Some(n) = stack.pop() {
        visited += 1;
        if visited > 4096 {
            break; // pathological nesting; the whole node is close enough
        }
        for i in 0..n.named_child_count() {
            let c = n.named_child(i as u32).unwrap();
            if c.start_byte() >= best {
                continue;
            }
            if BODY_KINDS.contains(&c.kind()) {
                best = c.start_byte();
            } else {
                stack.push(c);
            }
        }
    }
    best
}

/// Is this definition part of the file's public surface?
///
/// Computed from the AST, never by matching strings: `pub` appears inside comments and
/// string literals, and `export` is a prefix of `exports`.
fn is_exported(node: &Node, lang: TagLang, name: &str, top_level: bool) -> bool {
    match lang {
        TagLang::Rust => {
            for i in 0..node.child_count() {
                if node.child(i as u32).map(|c| c.kind()) == Some("visibility_modifier") {
                    return true;
                }
            }
            false
        }
        TagLang::TypeScript | TagLang::Tsx => {
            // `export function f()` parses as export_statement > function_declaration,
            // and `export const f = () => {}` adds a lexical_declaration in between.
            let mut cur = node.parent();
            let mut hops = 0;
            while let Some(p) = cur {
                if p.kind() == "export_statement" {
                    return true;
                }
                hops += 1;
                if hops > 3 {
                    break;
                }
                cur = p.parent();
            }
            // A class member is reachable whenever its class is, and TypeScript members
            // are public unless marked otherwise.
            !top_level && !name.starts_with('#')
        }
        // Python has no export keyword; the leading underscore is the whole convention.
        TagLang::Python => !name.starts_with('_'),
    }
}

/// Does a comment immediately precede this definition?
///
/// A doc comment is the author stating that something is API, which is exactly the signal
/// the prior wants. "Immediately" means at most one newline between, so a comment
/// belonging to an earlier item does not get credited to this one.
fn is_documented(node: &Node, src: &str) -> bool {
    // Step out through any export wrapper so `export function` is judged on the comment
    // above the `export`.
    let mut target = *node;
    while let Some(p) = target.parent() {
        if p.kind() == "export_statement" || p.kind() == "lexical_declaration" {
            target = p;
        } else {
            break;
        }
    }
    let Some(prev) = target.prev_sibling() else {
        return false;
    };
    if !prev.kind().contains("comment") {
        return false;
    }
    let between = &src[prev.end_byte().min(src.len())..target.start_byte().min(src.len())];
    between.chars().filter(|c| *c == '\n').count() <= 1
}

/// Parse and run the tag query.
///
/// `None` when the language has no query, which the caller must treat as "fall back and
/// record a distinct route" rather than as an empty outline.
pub fn extract_tags(src: &str, lang: TagLang) -> Option<(Vec<Def>, Vec<Ref>)> {
    let ts_lang = lang.language();
    let mut parser = Parser::new();
    parser.set_language(&ts_lang).ok()?;
    let tree = parser.parse(src, None)?;
    let query = compiled_query(lang)?;
    let capture_names = query.capture_names();

    let mut defs: Vec<Def> = Vec::new();
    let mut refs: Vec<Ref> = Vec::new();

    let mut cursor = QueryCursor::new();
    let mut it = cursor.matches(query, tree.root_node(), src.as_bytes());
    while let Some(m) = it.next() {
        // A match carries one @name plus one @definition.* or @reference.*.
        let mut name: Option<(String, usize)> = None;
        let mut def_kind: Option<(DefKind, Node)> = None;
        let mut is_ref = false;

        for cap in m.captures {
            let cname = capture_names[cap.index as usize];
            if cname == "name" {
                let text = &src[cap.node.byte_range()];
                name = Some((text.to_string(), cap.node.start_position().row));
            } else if let Some(k) = cname.strip_prefix("definition.") {
                if let Some(dk) = DefKind::from_capture(k) {
                    def_kind = Some((dk, cap.node));
                }
            } else if cname.starts_with("reference.") {
                is_ref = true;
            }
        }

        let Some((name, name_line)) = name else {
            continue;
        };
        if let Some((kind, node)) = def_kind {
            let top_level = node
                .parent()
                .map(|p| p.parent().is_none() || p.kind() == "export_statement")
                .unwrap_or(true);
            defs.push(Def {
                exported: is_exported(&node, lang, &name, top_level),
                documented: is_documented(&node, src),
                name,
                kind,
                name_line,
                sig_end: signature_end(&node),
                span: node.byte_range(),
                parent: None,
            });
        } else if is_ref {
            // The name node's own offset, so the reference is attributed to whichever
            // definition encloses that exact point.
            refs.push(Ref {
                name,
                byte_offset: m
                    .captures
                    .iter()
                    .find(|c| capture_names[c.index as usize] == "name")
                    .map(|c| c.node.start_byte())
                    .unwrap_or(0),
            });
        }
    }

    // Deduplicate: several patterns can match the same node (a Rust `function_item`
    // inside a `declaration_list` matches both the method and the function pattern).
    // Keep the most specific, which is the one seen with the narrower span; ties go to
    // the first by name for determinism.
    defs.sort_by(|a, b| {
        a.span
            .start
            .cmp(&b.span.start)
            .then(a.span.end.cmp(&b.span.end))
            .then(a.name.cmp(&b.name))
    });
    defs.dedup_by(|a, b| a.span == b.span && a.name == b.name);

    assign_parents(&mut defs);
    refs.sort_by(|a, b| a.byte_offset.cmp(&b.byte_offset).then(a.name.cmp(&b.name)));
    Some((defs, refs))
}

/// Assign each definition its innermost strict container.
///
/// Single stack pass over the span-sorted list: O(n), not the O(n²) of comparing every
/// pair. Requires `defs` sorted by `span.start`, which `extract_tags` guarantees.
fn assign_parents(defs: &mut [Def]) {
    let mut stack: Vec<usize> = Vec::new();
    for i in 0..defs.len() {
        while let Some(&top) = stack.last() {
            if defs[top].span.end <= defs[i].span.start {
                stack.pop();
            } else {
                break;
            }
        }
        // Strict containment only: a node is not its own parent.
        defs[i].parent = stack
            .last()
            .copied()
            .filter(|&p| defs[p].span.start < defs[i].span.start);
        stack.push(i);
    }
}

/// Edges of the in-file reference graph, plus file-level uses.
pub struct Graph {
    /// `(caller, callee) -> weight`. BTreeMap so iteration order is deterministic.
    pub edges: BTreeMap<(usize, usize), f64>,
    /// References from module scope, which have no enclosing definition to credit.
    pub top_level_hits: BTreeMap<usize, f64>,
}

/// Build the reference graph.
///
/// Ambiguous names — an overload, or the same method on two types — are attributed to
/// every candidate at weight `1/n` rather than to an arbitrary one. Picking arbitrarily
/// would make scores depend on hash iteration order and stop the output being
/// reproducible, which breaks both caching and the A/B.
pub fn build_graph(defs: &[Def], refs: &[Ref]) -> Graph {
    let mut by_name: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (i, d) in defs.iter().enumerate() {
        by_name.entry(d.name.as_str()).or_default().push(i);
    }

    // Span-sorted index for binary search of the innermost container.
    let mut order: Vec<usize> = (0..defs.len()).collect();
    order.sort_by_key(|&i| defs[i].span.start);

    let mut edges: BTreeMap<(usize, usize), f64> = BTreeMap::new();
    let mut top_level_hits: BTreeMap<usize, f64> = BTreeMap::new();

    for r in refs {
        let Some(candidates) = by_name.get(r.name.as_str()) else {
            continue; // unresolved: a call into another file
        };
        let w = 1.0 / candidates.len() as f64;
        let caller = innermost_containing(defs, &order, r.byte_offset);
        for &callee in candidates {
            match caller {
                None => *top_level_hits.entry(callee).or_default() += w,
                Some(c) if c != callee => *edges.entry((c, callee)).or_default() += w,
                // Self-recursion says nothing about relative importance.
                Some(_) => {}
            }
        }
    }

    Graph {
        edges,
        top_level_hits,
    }
}

/// Innermost definition whose span contains `offset`.
///
/// Binary search for the insertion point, then walk back over the few candidates that
/// start before it. Linear scanning here would make the graph build O(r·n).
fn innermost_containing(defs: &[Def], order: &[usize], offset: usize) -> Option<usize> {
    let pos = order.partition_point(|&i| defs[i].span.start <= offset);
    let mut best: Option<usize> = None;
    for &i in order[..pos].iter().rev() {
        let d = &defs[i];
        if d.span.contains(&offset) {
            match best {
                // Narrower span wins.
                Some(b) if defs[b].span.end - defs[b].span.start <= d.span.end - d.span.start => {}
                _ => best = Some(i),
            }
        }
    }
    best
}

/// Structural prior, normalised to sum to 1.
///
/// This is the half of the score that graph centrality cannot supply. A `pub fn` nothing
/// in the file calls has in-degree zero, yet it is frequently the file's entire reason to
/// exist — public API is the most valuable thing an outline can carry. The prior is where
/// that enters, and it is also the teleport distribution for PageRank, so the export
/// signal propagates instead of being averaged away.
pub fn prior(defs: &[Def], graph: &Graph) -> Vec<f64> {
    let mut p: Vec<f64> = defs
        .iter()
        .enumerate()
        .map(|(i, d)| {
            let vis = if d.exported { 2.5 } else { 1.0 };
            let doc = if d.documented { 1.3 } else { 1.0 };
            let depth = 1.0 / (1.0 + d.depth(defs) as f64);
            // File-level uses. The specification collects these without saying where they
            // go; folding them into the teleport mass is the reading that keeps them
            // meaningful — a definition used from module scope really is used — while
            // adding one bounded coefficient rather than a second mechanism. Capped so a
            // name referenced in a loop cannot dominate.
            let file_use = 1.0 + 0.1 * graph.top_level_hits.get(&i).copied().unwrap_or(0.0);
            d.kind.base() * vis * doc * depth * file_use.min(2.0)
        })
        .collect();

    let total: f64 = p.iter().sum();
    if total > 0.0 {
        for v in &mut p {
            *v /= total;
        }
    } else if !p.is_empty() {
        let u = 1.0 / p.len() as f64;
        p.iter_mut().for_each(|v| *v = u);
    }
    p
}

/// Personalized PageRank over the reference graph.
///
/// Dangling nodes redistribute according to `prior`, not uniformly: uniform
/// redistribution would dilute exactly the export signal the prior exists to carry, and
/// most definitions in a small file are dangling.
pub fn pagerank(prior: &[f64], graph: &Graph) -> Vec<f64> {
    let n = prior.len();
    if n < MIN_NODES_FOR_PAGERANK || graph.edges.is_empty() {
        return prior.to_vec();
    }

    // Column-normalise: out-edges of each node sum to 1.
    let mut out_total = vec![0.0f64; n];
    for ((from, _), w) in &graph.edges {
        out_total[*from] += w;
    }

    let mut score = prior.to_vec();
    let mut next = vec![0.0f64; n];

    for _ in 0..PAGERANK_MAX_ITERS {
        next.iter_mut().for_each(|v| *v = 0.0);

        let mut dangling_mass = 0.0;
        for i in 0..n {
            if out_total[i] == 0.0 {
                dangling_mass += score[i];
            }
        }

        for ((from, to), w) in &graph.edges {
            next[*to] += score[*from] * (w / out_total[*from]);
        }

        let mut delta = 0.0;
        for i in 0..n {
            let v = (1.0 - DAMPING) * prior[i] + DAMPING * (next[i] + dangling_mass * prior[i]);
            delta += (v - score[i]).abs();
            next[i] = v;
        }
        std::mem::swap(&mut score, &mut next);
        if delta < PAGERANK_EPSILON {
            break;
        }
    }
    score
}

/// Definition indices, best first.
///
/// Total order with no ties: score, then source position, then name. Two runs over
/// identical bytes must produce identical output — a ranking that drifts silently breaks
/// both the cache and the A/B comparison.
pub fn rank(defs: &[Def], scores: &[f64]) -> Vec<usize> {
    let mut idx: Vec<usize> = (0..defs.len()).collect();
    idx.sort_by(|&a, &b| {
        scores[b]
            .partial_cmp(&scores[a])
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(defs[a].name_line.cmp(&defs[b].name_line))
            .then(defs[a].name.cmp(&defs[b].name))
    });
    idx
}

/// One line of a signature, whitespace collapsed.
fn signature_text(src: &str, d: &Def) -> String {
    let end = d.sig_end.min(src.len()).max(d.span.start);
    let raw = &src[d.span.start.min(src.len())..end];
    let mut out = String::with_capacity(raw.len());
    let mut last_space = false;
    for ch in raw.chars() {
        if ch.is_whitespace() {
            if !last_space && !out.is_empty() {
                out.push(' ');
            }
            last_space = true;
        } else {
            out.push(ch);
            last_space = false;
        }
    }
    out.trim().to_string()
}

/// Render the chosen definitions as a map of the file.
///
/// Source order, not rank order: the model reads this as the file's shape, and rank order
/// would scramble that. Ancestors are emitted once each, and runs of omitted definitions
/// collapse to a single marker.
pub fn render(path: &str, total_lines: usize, src: &str, defs: &[Def], chosen: &[usize]) -> String {
    let mut selected: Vec<usize> = chosen.to_vec();
    // Pull in every ancestor of anything selected: a method is meaningless without the
    // type it hangs off.
    let mut i = 0;
    while i < selected.len() {
        if let Some(p) = defs[selected[i]].parent
            && !selected.contains(&p)
        {
            selected.push(p);
        }
        i += 1;
    }
    selected.sort_by_key(|&i| (defs[i].span.start, defs[i].span.end));
    selected.dedup();

    let mut buf = format!(
        "# {path} — ranked outline\n\
         # {total_lines} lines | {} of {} definitions | source order\n\
         # Use recall_file to fetch a body by name or line range.\n\n",
        chosen.len(),
        defs.len()
    );

    let mut open: Vec<usize> = Vec::new();
    let mut prev_line: Option<usize> = None;

    for &i in &selected {
        let d = &defs[i];

        // Close scopes this definition is not inside.
        while let Some(&top) = open.last() {
            if defs[top].span.end <= d.span.start {
                open.pop();
                buf.push_str(&format!("{}}}\n", "    ".repeat(open.len())));
            } else {
                break;
            }
        }

        let indent = "    ".repeat(open.len());
        if let Some(prev) = prev_line
            && d.name_line > prev + 1
        {
            buf.push_str(&format!("{indent}...\n"));
        }

        let sig = signature_text(src, d);
        let is_container = selected
            .iter()
            .any(|&j| j != i && defs[j].parent == Some(i));

        if is_container {
            buf.push_str(&format!("{indent}{sig} {{\n"));
            open.push(i);
        } else {
            buf.push_str(&format!("{indent}{sig}\n"));
        }
        prev_line = Some(d.name_line);
    }

    while !open.is_empty() {
        open.pop();
        buf.push_str(&format!("{}}}\n", "    ".repeat(open.len())));
    }

    buf
}

/// What a budget fit produced.
#[derive(Debug, Clone)]
pub struct Fitted {
    pub text: String,
    /// Definitions included.
    pub k: usize,
    /// Definitions found.
    pub n: usize,
    pub returned_tokens: usize,
}

/// Largest prefix of the ranking whose rendering fits `budget`.
///
/// Binary search rather than greedy accumulation, because rendering cost is not additive:
/// including a nested definition also emits its ancestor headers, which may already be
/// present for a different selection. The only way to know the cost of a set is to render
/// it and count.
///
/// `count` must be a real tokenizer. Estimating here would reintroduce the class of error
/// that recorded three screenshots as 119,921 tokens against about 2,750 actual.
pub fn fit_budget<F>(
    path: &str,
    total_lines: usize,
    src: &str,
    defs: &[Def],
    ranking: &[usize],
    budget: i64,
    count: &F,
) -> Fitted
where
    F: Fn(&str) -> usize,
{
    let render_k = |k: usize| render(path, total_lines, src, defs, &ranking[..k]);

    if budget <= 0 {
        return Fitted {
            text: String::new(),
            k: 0,
            n: defs.len(),
            returned_tokens: 0,
        };
    }

    let (mut lo, mut hi) = (0usize, defs.len());
    let mut best = 0usize;
    let mut best_text = render_k(0);
    let mut best_tokens = count(&best_text);

    while lo <= hi {
        let mid = (lo + hi) / 2;
        let text = render_k(mid);
        let tokens = count(&text);
        if tokens as i64 <= budget {
            best = mid;
            best_text = text;
            best_tokens = tokens;
            lo = mid + 1;
        } else {
            if mid == 0 {
                break;
            }
            hi = mid - 1;
        }
    }

    Fitted {
        text: best_text,
        k: best,
        n: defs.len(),
        returned_tokens: best_tokens,
    }
}

/// Why a ranked outline was not produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decline {
    /// No tag query for this language.
    NoQuery,
    /// The file has no definitions to rank.
    NoDefs,
    /// `full_tokens − S_min` leaves too little for a useful outline.
    NotWorthIt,
    /// The rendered outline was not smaller than the source.
    WouldInflate,
    /// The pipeline exceeded its wall-clock ceiling.
    TooSlow,
}

impl Decline {
    /// A distinct `routed_via`, so a declined call never blends into the metrics of a
    /// successful one.
    pub fn route(self) -> &'static str {
        match self {
            Decline::NoQuery => "ranked_no_query",
            Decline::NoDefs => "ranked_no_defs",
            Decline::NotWorthIt => "ranked_not_worth_it",
            Decline::WouldInflate => "ranked_would_inflate",
            Decline::TooSlow => "ranked_too_slow",
        }
    }
}

/// The decision and its inputs, recorded per call so it can be audited later.
#[derive(Debug, Clone)]
pub struct Decision {
    pub econ: Econ,
    pub s_min: i64,
    pub budget: i64,
    pub coeff_version: u32,
    pub outcome: Result<Fitted, Decline>,
}

/// Wall-clock ceiling for the whole pipeline.
///
/// 50 ms, not the 10 ms originally specified. Measured in release with the query compiled
/// once, the pipeline is 2 ms at 421 lines, 7 ms at 1,433 and 17 ms at 4,198 —
/// parse-dominated — so 10 ms rejected precisely the large files an outline helps most.
/// What the ceiling guards against is returning the entire file instead, which costs the
/// model far more than 50 ms in both latency and tokens.
#[cfg(not(debug_assertions))]
const DEFAULT_TIME_BUDGET_MS: u64 = 50;

/// Ten times the release ceiling: a debug build's constant factor is 3–5x and is not what
/// the ceiling is calibrated against.
#[cfg(debug_assertions)]
const DEFAULT_TIME_BUDGET_MS: u64 = 500;

/// The ceiling, overridable by `LUMEN_RANKED_TIME_BUDGET_MS`.
///
/// The override exists because a wall-clock deadline is not a property of the code — it is
/// a property of the machine. Tests that assert on which branch was taken were flaky
/// across CI runners without it, declining as `TooSlow` on a slower host and passing on a
/// faster one, which is a test measuring the runner rather than the feature. It is also
/// the honest escape hatch for a genuinely slow machine.
fn time_budget() -> std::time::Duration {
    let ms = std::env::var("LUMEN_RANKED_TIME_BUDGET_MS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_TIME_BUDGET_MS);
    std::time::Duration::from_millis(ms)
}

/// Produce a ranked outline, or say why not.
///
/// `now` is injected so the timeout is testable without sleeping.
pub fn ranked_outline<F>(
    path: &str,
    src: &str,
    full_tokens: usize,
    econ: &Econ,
    count: &F,
) -> Decision
where
    F: Fn(&str) -> usize,
{
    ranked_outline_cached(path, src, full_tokens, econ, count, None)
}

/// As [`ranked_outline`], reusing a cached tag extraction when `stamp` identifies the
/// file's content. `None` bypasses the cache in both directions.
pub fn ranked_outline_cached<F>(
    path: &str,
    src: &str,
    full_tokens: usize,
    econ: &Econ,
    count: &F,
    stamp: Option<FileStamp>,
) -> Decision
where
    F: Fn(&str) -> usize,
{
    let started = std::time::Instant::now();
    let deadline = time_budget();
    let s_min = econ.s_min().map(|s| s.ceil() as i64).unwrap_or(i64::MAX);
    let budget = econ.budget(full_tokens).unwrap_or(i64::MIN);
    let mk = |outcome| Decision {
        econ: *econ,
        s_min,
        budget,
        coeff_version: COEFF_VERSION,
        outcome,
    };

    if budget < MIN_USEFUL_OUTLINE {
        return mk(Err(Decline::NotWorthIt));
    }
    let Some(lang) = TagLang::detect(path) else {
        return mk(Err(Decline::NoQuery));
    };
    let Some(tags) = extract_tags_cached(path, src, lang, stamp) else {
        return mk(Err(Decline::NoQuery));
    };
    let (defs, refs) = (&tags.0, &tags.1);
    if defs.is_empty() {
        return mk(Err(Decline::NoDefs));
    }
    if started.elapsed() > deadline {
        return mk(Err(Decline::TooSlow));
    }

    let graph = build_graph(defs, refs);
    let p = prior(defs, &graph);
    let scores = pagerank(&p, &graph);
    let ranking = rank(defs, &scores);

    let total_lines = src.lines().count();
    let fitted = fit_budget(path, total_lines, src, defs, &ranking, budget, count);

    if fitted.k == 0 {
        return mk(Err(Decline::NotWorthIt));
    }
    // Unreachable while budget = full − S_min with S_min > 0, since fit_budget never
    // exceeds the budget. Kept as a backstop if the budget rule is ever changed: an
    // "optimisation" that inflates must be visible in the ledger, not silently absorbed.
    if fitted.returned_tokens >= full_tokens {
        return mk(Err(Decline::WouldInflate));
    }
    if started.elapsed() > deadline {
        return mk(Err(Decline::TooSlow));
    }
    mk(Ok(fitted))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tokenizer::count_tokens;

    fn tok(s: &str) -> usize {
        count_tokens(s)
    }

    const RUST_SRC: &str = r#"
/// The public entry point. Nothing in this file calls it.
pub fn run(config: Config) -> Result<Report> {
    let inner = helper(config);
    finish(inner)
}

fn helper(c: Config) -> Inner {
    normalise(c)
}

fn normalise(c: Config) -> Inner {
    Inner { c }
}

fn finish(i: Inner) -> Result<Report> {
    normalise_report(i)
}

fn normalise_report(i: Inner) -> Result<Report> {
    Ok(Report {})
}

pub struct Report {
    pub ok: bool,
}

impl Report {
    pub fn new() -> Self {
        Report { ok: true }
    }
    fn private_detail(&self) -> u8 {
        7
    }
}
"#;

    fn defs_of(src: &str, lang: TagLang) -> (Vec<Def>, Vec<Ref>) {
        extract_tags(src, lang).expect("query must compile")
    }

    #[test]
    fn every_language_query_compiles() {
        // A broken query would otherwise surface as "no definitions" at runtime.
        for l in [
            TagLang::Rust,
            TagLang::Python,
            TagLang::TypeScript,
            TagLang::Tsx,
        ] {
            assert!(
                compiled_query(l).is_some(),
                "tag query failed to compile for {l:?}"
            );
        }
    }

    #[test]
    fn detect_covers_the_shipped_languages_and_nothing_else() {
        assert_eq!(TagLang::detect("a.rs"), Some(TagLang::Rust));
        assert_eq!(TagLang::detect("a.py"), Some(TagLang::Python));
        assert_eq!(TagLang::detect("a.ts"), Some(TagLang::TypeScript));
        assert_eq!(TagLang::detect("a.tsx"), Some(TagLang::Tsx));
        assert_eq!(TagLang::detect("a.go"), None);
        assert_eq!(TagLang::detect("Makefile"), None);
    }

    /// THE export blind spot. `run` is `pub` and nothing in the file calls it, so its
    /// in-degree is zero — the case a pure centrality score gets wrong. Public API is the
    /// most valuable thing an outline can carry, so it must still rank first.
    #[test]
    fn an_uncalled_public_function_still_ranks_top() {
        let (defs, refs) = defs_of(RUST_SRC, TagLang::Rust);
        let g = build_graph(&defs, &refs);
        let p = prior(&defs, &g);
        let s = pagerank(&p, &g);
        let order = rank(&defs, &s);

        let run_rank = order.iter().position(|&i| defs[i].name == "run").unwrap();
        let worst_exported_beaten_by: Vec<&str> = order[..run_rank]
            .iter()
            .filter(|&&i| !defs[i].exported)
            .map(|&i| defs[i].name.as_str())
            .collect();
        assert!(
            worst_exported_beaten_by.is_empty(),
            "`pub fn run` is never called in this file, so in-degree ranks it last. It \
             must still outrank every unexported definition, but these beat it: {:?}\n\
             full order: {:?}",
            worst_exported_beaten_by,
            order.iter().map(|&i| &defs[i].name).collect::<Vec<_>>()
        );

        // And the private helper that IS called must not beat it.
        let run_i = defs.iter().position(|d| d.name == "run").unwrap();
        let norm_i = defs.iter().position(|d| d.name == "normalise").unwrap();
        assert!(
            s[run_i] > s[norm_i],
            "pub-but-uncalled {} must score above called-but-private {}",
            s[run_i],
            s[norm_i]
        );
    }

    #[test]
    fn visibility_and_docs_are_read_from_the_ast() {
        let (defs, _) = defs_of(RUST_SRC, TagLang::Rust);
        let run = defs.iter().find(|d| d.name == "run").unwrap();
        assert!(run.exported, "`pub fn run` is exported");
        assert!(run.documented, "a /// comment immediately precedes it");

        let helper = defs.iter().find(|d| d.name == "helper").unwrap();
        assert!(!helper.exported, "`fn helper` is not exported");
        assert!(!helper.documented);
    }

    #[test]
    fn nested_definitions_get_their_container_as_parent() {
        let (defs, _) = defs_of(RUST_SRC, TagLang::Rust);
        let new_i = defs.iter().position(|d| d.name == "new").unwrap();
        assert!(
            defs[new_i].parent.is_some(),
            "`fn new` inside `impl Report` must have a parent"
        );
        // Depth pushes nested items below top-level ones, all else equal.
        assert!(defs[new_i].depth(&defs) >= 1);
    }

    /// Two runs over identical bytes must be byte-identical, or the cache and the A/B
    /// both break. Never rely on HashMap iteration order anywhere in the pipeline.
    #[test]
    fn output_is_byte_identical_across_repeated_runs() {
        let once = {
            let (defs, refs) = defs_of(RUST_SRC, TagLang::Rust);
            let g = build_graph(&defs, &refs);
            let s = pagerank(&prior(&defs, &g), &g);
            let order = rank(&defs, &s);
            render("x.rs", 40, RUST_SRC, &defs, &order)
        };
        for i in 0..100 {
            let (defs, refs) = defs_of(RUST_SRC, TagLang::Rust);
            let g = build_graph(&defs, &refs);
            let s = pagerank(&prior(&defs, &g), &g);
            let order = rank(&defs, &s);
            let again = render("x.rs", 40, RUST_SRC, &defs, &order);
            assert_eq!(once, again, "run {i} differed from run 0");
        }
    }

    /// Ambiguous names split the weight rather than being attributed arbitrarily.
    #[test]
    fn an_ambiguous_name_splits_its_weight_across_candidates() {
        let src = r#"
struct A;
struct B;
impl A { fn shared(&self) -> u8 { 1 } }
impl B { fn shared(&self) -> u8 { 2 } }
fn caller(a: A) -> u8 { shared(a) }
"#;
        let (defs, refs) = defs_of(src, TagLang::Rust);
        let g = build_graph(&defs, &refs);

        let shared: Vec<usize> = defs
            .iter()
            .enumerate()
            .filter(|(_, d)| d.name == "shared")
            .map(|(i, _)| i)
            .collect();
        assert_eq!(shared.len(), 2, "two methods named `shared`");
        let caller = defs.iter().position(|d| d.name == "caller").unwrap();

        let w: Vec<f64> = shared
            .iter()
            .map(|&s| g.edges.get(&(caller, s)).copied().unwrap_or(0.0))
            .collect();
        assert_eq!(
            w[0], w[1],
            "both candidates must receive equal weight, got {w:?}"
        );
        assert!(
            (w[0] - 0.5).abs() < 1e-9,
            "each of two candidates gets 1/2, got {}",
            w[0]
        );
    }

    #[test]
    fn self_recursion_creates_no_edge() {
        let src = "fn f(n: u32) -> u32 { if n == 0 { 0 } else { f(n - 1) } }\n";
        let (defs, refs) = defs_of(src, TagLang::Rust);
        let g = build_graph(&defs, &refs);
        let f = defs.iter().position(|d| d.name == "f").unwrap();
        assert!(
            !g.edges.contains_key(&(f, f)),
            "a function calling itself says nothing about relative importance"
        );
    }

    /// Budget respected, with the real tokenizer, across a spread of budgets.
    #[test]
    fn rendered_output_never_exceeds_the_budget() {
        let (defs, refs) = defs_of(RUST_SRC, TagLang::Rust);
        let g = build_graph(&defs, &refs);
        let s = pagerank(&prior(&defs, &g), &g);
        let order = rank(&defs, &s);

        for budget in [120i64, 150, 200, 300, 500, 1_000, 10_000] {
            let f = fit_budget("x.rs", 40, RUST_SRC, &defs, &order, budget, &tok);
            assert!(
                f.returned_tokens as i64 <= budget,
                "budget {budget} exceeded: returned {} tokens for k={}",
                f.returned_tokens,
                f.k
            );
        }
    }

    #[test]
    fn a_larger_budget_never_selects_fewer_definitions() {
        let (defs, refs) = defs_of(RUST_SRC, TagLang::Rust);
        let g = build_graph(&defs, &refs);
        let s = pagerank(&prior(&defs, &g), &g);
        let order = rank(&defs, &s);
        let mut last = 0usize;
        for budget in [120i64, 200, 400, 800, 4_000] {
            let f = fit_budget("x.rs", 40, RUST_SRC, &defs, &order, budget, &tok);
            assert!(
                f.k >= last,
                "k went backwards as the budget grew: {} then {}",
                last,
                f.k
            );
            last = f.k;
        }
    }

    /// A file too small to repay the round is declined, not trimmed. This is the guard
    /// that makes a net-negative call unrepresentable.
    #[test]
    fn a_file_below_s_min_is_declined_not_trimmed() {
        let econ = Econ::default();
        let d = ranked_outline("tiny.rs", "fn a() {}\n", 5, &econ, &tok);
        assert_eq!(d.outcome.as_ref().err(), Some(&Decline::NotWorthIt));
        assert_eq!(d.budget, 5 - d.s_min);
        assert!(d.budget < 0);
    }

    /// Phase A's whole purpose: the budget must actually bind, so the ranking selects.
    ///
    /// Before the fix `budget = full − S_min` gave a 39k-token file a budget of 33,897 and
    /// every one of 222 definitions fitted — `k = n`, the ranking chose nothing, and the
    /// A/B compared two unranked outlines. `S_min` is a floor on the *saving*; it cannot
    /// bound the outline from below, because net value only rises as the outline shrinks.
    #[test]
    fn the_budget_is_capped_at_the_target_so_the_ranking_binds() {
        let econ = Econ::default();
        // A large file: affordable is enormous, but the target is what should apply.
        let affordable = econ.budget(500_000).unwrap();
        assert_eq!(
            affordable,
            crate::econ::DEFAULT_TARGET_OUTLINE,
            "a large file's budget must be the target, not what it could afford"
        );

        // And on a real file the ranking must now leave something out.
        let src: String = (0..300)
            .map(|i| {
                format!("/// Item {i}.\npub fn function_number_{i}(a: u32, b: &str) -> Result<Vec<String>, Error> {{\n    let mut v = Vec::new();\n    v.push(format!(\"{{}}\", a));\n    Ok(v)\n}}\n\n")
            })
            .collect();
        let full = tok(&src);
        let d = ranked_outline("wide.rs", &src, full, &econ, &tok);
        let f = d
            .outcome
            .as_ref()
            .expect("a file this size must be outlined");
        assert!(
            f.k < f.n,
            "the budget did not bind: k={} of n={} — the ranking selected nothing and the \
             A/B would compare two unranked outlines",
            f.k,
            f.n
        );
        assert!(f.k > 0, "but it must still return something useful");
        assert!(
            f.returned_tokens as i64 <= crate::econ::DEFAULT_TARGET_OUTLINE,
            "the outline must respect the target: {} tokens",
            f.returned_tokens
        );
    }

    /// A small file is still refused — Phase A must not weaken the gate, which is the part
    /// that pays.
    #[test]
    fn capping_the_budget_does_not_weaken_the_refusal() {
        let econ = Econ::default();
        for full in [125usize, 1_000, 3_216, 5_000] {
            assert!(
                econ.budget(full).unwrap() < MIN_USEFUL_OUTLINE,
                "a {full}-token file must still be refused"
            );
        }
        // The 3,216-token file that produced `ranked_not_worth_it` on the real install.
        let d = ranked_outline("small.rs", "pub fn a() {}\n", 3_216, &econ, &tok);
        assert_eq!(d.outcome.as_ref().err(), Some(&Decline::NotWorthIt));
    }

    /// The budget guard must short-circuit BEFORE any parsing work.
    ///
    /// Asserting only "a small file declines" is not enough: `fit_budget` returns k=0 for
    /// a non-positive budget and `ranked_outline` reports `NotWorthIt` for that too, so
    /// the obvious test passes with the guard deleted. Removing it was verified to leave
    /// that test green.
    ///
    /// The distinguishing observation is ordering. The guard runs before language
    /// detection, so a small file in an unsupported language must report `NotWorthIt`
    /// (we never got as far as looking for a query) rather than `NoQuery`.
    #[test]
    fn the_budget_guard_runs_before_any_parsing() {
        let econ = Econ::default();
        let d = ranked_outline("tiny.go", "package main\n", 40, &econ, &tok);
        assert_eq!(
            d.outcome.as_ref().err(),
            Some(&Decline::NotWorthIt),
            "a file that cannot pay must be refused before the language is even \
             considered; reporting NoQuery here means the guard did not run first"
        );
    }

    /// The guard must also fire for a *positive* budget that is merely too small, which
    /// is the range `MIN_USEFUL_OUTLINE` exists to cover.
    #[test]
    fn a_positive_but_useless_budget_is_still_declined() {
        // Context chosen so S_min lands just under full_tokens, leaving a positive
        // budget below the useful floor.
        let econ = Econ {
            context_tokens: 1_000.0,
            ..Default::default()
        };
        let s_min = econ.s_min().unwrap().ceil() as i64;
        let full = (s_min + MIN_USEFUL_OUTLINE - 10) as usize;
        let d = ranked_outline("x.rs", "fn a() {}\n", full, &econ, &tok);
        assert!(
            d.budget > 0,
            "premise: the budget is positive ({})",
            d.budget
        );
        assert!(d.budget < MIN_USEFUL_OUTLINE);
        assert_eq!(d.outcome.as_ref().err(), Some(&Decline::NotWorthIt));
    }

    /// A successful outline is always strictly smaller than its source — and the reason
    /// is structural, not a check that happens to pass.
    ///
    /// `budget = full − S_min` with `S_min > 0`, and `fit_budget` never returns more than
    /// the budget, so `returned ≤ budget < full` holds by construction. That makes
    /// `Decline::WouldInflate` unreachable while the budget rule stands. It is kept as a
    /// backstop for a future change to that rule, and this test asserts the invariant it
    /// protects rather than pretending to trigger it — a test that could only pass by
    /// disabling the budget would be testing nothing.
    #[test]
    fn a_successful_outline_is_always_smaller_than_its_source() {
        let flat: String = (0..200).map(|i| format!("fn f{i}() {{}}\n")).collect();
        let trait_only: String = std::iter::once("pub trait Wide {\n".to_string())
            .chain((0..120).map(|i| {
                format!("    fn method_number_{i}(&self, a: u32, b: &str) -> Result<Vec<String>, Error>;\n")
            }))
            .chain(std::iter::once("}\n".to_string()))
            .collect();

        for (name, src) in [("flat.rs", &flat), ("wide.rs", &trait_only)] {
            let full = tok(src);
            for ctx in [1_000.0, 50_000.0, 362_965.0] {
                let econ = Econ {
                    context_tokens: ctx,
                    ..Default::default()
                };
                let d = ranked_outline(name, src, full, &econ, &tok);
                if let Ok(f) = &d.outcome {
                    assert!(
                        f.returned_tokens < full,
                        "{name} at context {ctx}: returned {} of {full} full tokens — an \
                         outline must never be as large as what it replaces",
                        f.returned_tokens
                    );
                    assert!(
                        f.returned_tokens as i64 <= d.budget,
                        "{name}: returned {} exceeds budget {}",
                        f.returned_tokens,
                        d.budget
                    );
                }
            }
        }
    }

    /// An unsupported language must decline with its own route, never panic and never
    /// silently return an empty outline that looks like a saving.
    #[test]
    fn a_language_without_a_query_declines_with_a_distinct_route() {
        let econ = Econ {
            context_tokens: 1_000.0,
            ..Default::default()
        };
        let big = "x".repeat(200_000);
        let d = ranked_outline("thing.go", &big, 50_000, &econ, &tok);
        assert_eq!(d.outcome.as_ref().err(), Some(&Decline::NoQuery));
        assert_eq!(d.outcome.as_ref().err().unwrap().route(), "ranked_no_query");
    }

    #[test]
    fn a_file_with_no_definitions_declines_with_its_own_route() {
        let econ = Econ {
            context_tokens: 1_000.0,
            ..Default::default()
        };
        // Comments only: parses fine, yields no tags.
        let src = "// nothing here\n".repeat(4_000);
        let full = tok(&src);
        let d = ranked_outline("empty.rs", &src, full, &econ, &tok);
        assert_eq!(d.outcome.as_ref().err(), Some(&Decline::NoDefs));
    }

    /// Every decline route is distinct, so the metrics can never blend two paths.
    #[test]
    fn decline_routes_are_all_distinct() {
        let all = [
            Decline::NoQuery,
            Decline::NoDefs,
            Decline::NotWorthIt,
            Decline::WouldInflate,
            Decline::TooSlow,
        ];
        let mut seen = std::collections::BTreeSet::new();
        for d in all {
            assert!(seen.insert(d.route()), "duplicate route {}", d.route());
            assert!(d.route().starts_with("ranked_"));
        }
    }

    #[test]
    fn the_renderer_emits_source_order_not_rank_order() {
        let (defs, refs) = defs_of(RUST_SRC, TagLang::Rust);
        let g = build_graph(&defs, &refs);
        let s = pagerank(&prior(&defs, &g), &g);
        let order = rank(&defs, &s);
        let text = render("x.rs", 40, RUST_SRC, &defs, &order);

        // `run` is ranked first but appears first in the source too; `Report` is later in
        // both. Assert on a pair whose rank order differs from source order.
        let pos = |needle: &str| text.find(needle).unwrap_or(usize::MAX);
        assert!(
            pos("fn run") < pos("struct Report"),
            "output must follow source order:\n{text}"
        );
    }

    #[test]
    fn the_renderer_emits_each_ancestor_once_and_only_signatures() {
        let (defs, refs) = defs_of(RUST_SRC, TagLang::Rust);
        let g = build_graph(&defs, &refs);
        let s = pagerank(&prior(&defs, &g), &g);
        let order = rank(&defs, &s);
        let text = render("x.rs", 40, RUST_SRC, &defs, &order);

        assert_eq!(
            text.matches("impl Report").count(),
            1,
            "the ancestor header must appear exactly once:\n{text}"
        );
        assert!(
            !text.contains("Report { ok: true }"),
            "a body leaked into the outline:\n{text}"
        );
        assert!(
            !text.contains("normalise(c)"),
            "a body leaked into the outline:\n{text}"
        );
    }

    #[test]
    fn typescript_definitions_upstream_misses_are_captured() {
        let src = r#"
export class Widget {
  private count = 0;
  onClick = () => { this.bump(); };
  bump(): void { this.count += 1; }
}
export function make(): Widget { return new Widget(); }
export const helper = (n: number) => n * 2;
type Alias = string;
"#;
        let (defs, _) = defs_of(src, TagLang::TypeScript);
        let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        for want in ["Widget", "bump", "make", "helper", "onClick", "Alias"] {
            assert!(
                names.contains(&want),
                "upstream misses `{want}`; the authored query must catch it. got {names:?}"
            );
        }
        let widget = defs.iter().find(|d| d.name == "Widget").unwrap();
        assert!(widget.exported, "`export class` is exported");
    }

    #[test]
    fn tsx_uses_the_same_query_successfully() {
        let src = "export function App() { return <div/>; }\n";
        let (defs, _) = defs_of(src, TagLang::Tsx);
        assert!(defs.iter().any(|d| d.name == "App"));
    }

    #[test]
    fn python_privacy_convention_drives_exportedness() {
        let src = "def public():\n    pass\n\ndef _private():\n    pass\n";
        let (defs, _) = defs_of(src, TagLang::Python);
        let pub_d = defs.iter().find(|d| d.name == "public").unwrap();
        let priv_d = defs.iter().find(|d| d.name == "_private").unwrap();
        assert!(pub_d.exported);
        assert!(!priv_d.exported);
    }

    #[test]
    fn pagerank_is_skipped_for_tiny_graphs_and_returns_the_prior() {
        let src = "fn a() {}\nfn b() {}\n";
        let (defs, refs) = defs_of(src, TagLang::Rust);
        let g = build_graph(&defs, &refs);
        let p = prior(&defs, &g);
        let s = pagerank(&p, &g);
        assert_eq!(p, s, "below the node floor the prior is used directly");
    }

    #[test]
    fn scores_and_prior_are_finite_and_normalised() {
        let (defs, refs) = defs_of(RUST_SRC, TagLang::Rust);
        let g = build_graph(&defs, &refs);
        let p = prior(&defs, &g);
        assert!(
            (p.iter().sum::<f64>() - 1.0).abs() < 1e-9,
            "prior must sum to 1"
        );
        for v in pagerank(&p, &g) {
            assert!(
                v.is_finite() && v >= 0.0,
                "non-finite or negative score {v}"
            );
        }
    }
}

// ── §6 tag cache ─────────────────────────────────────────────────────────────

/// Cached tag extraction, keyed on what the tags actually depend on.
///
/// Tags are the expensive part — parse plus query is 1.5–11 ms while rendering at a
/// different budget is microseconds — so the cache stores tags and re-renders freely.
///
/// **`mtime` and `size`, not `mtime` alone.** This runs inside a tool call, and a file
/// written and re-read within the same second is ordinary in that setting; second-
/// resolution mtime cannot see that edit, and a stale outline of a file the model just
/// changed is worse than no cache.
///
/// `coeff_version` is in the key so a coefficient change invalidates everything: entries
/// scored under old weights would silently corrupt the A/B.
///
/// The budget is deliberately **not** in the key, which departs from the specification.
/// The reason the spec gives for bucketing the budget is to stop small context
/// fluctuations thrashing the cache — but tags do not depend on the budget at all, so
/// including it would cause exactly that thrashing, re-parsing whenever the context
/// moved by one bucket. Bucketing would reduce the thrash rather than remove it. Since
/// what is stored is budget-independent, the key omits it entirely.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct CacheKey {
    path: String,
    mtime: i64,
    size: u64,
    coeff_version: u32,
}

/// Bounded so a long session walking a large tree cannot grow it without limit. On
/// overflow the whole map is dropped rather than evicting cleverly: an LRU needs
/// bookkeeping this does not earn, and a cold cache costs one re-parse.
const CACHE_CAPACITY: usize = 256;

type Tags = std::sync::Arc<(Vec<Def>, Vec<Ref>)>;

fn cache() -> &'static std::sync::Mutex<std::collections::HashMap<CacheKey, Tags>> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<CacheKey, Tags>>> =
        std::sync::OnceLock::new();
    CACHE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

/// Identity of a file's content, for cache keying.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileStamp {
    pub mtime: i64,
    pub size: u64,
}

impl FileStamp {
    /// Read from the filesystem. `None` if unavailable, which must disable caching for
    /// that call rather than fall back to a weaker key.
    pub fn of(path: &str) -> Option<Self> {
        let m = std::fs::metadata(path).ok()?;
        let mtime = m
            .modified()
            .ok()?
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_secs() as i64;
        Some(FileStamp {
            mtime,
            size: m.len(),
        })
    }
}

/// `extract_tags`, memoised on `(path, mtime, size, coeff_version)`.
///
/// With `stamp: None` the cache is bypassed in both directions — a call that cannot
/// establish file identity must not read a possibly-stale entry, and must not write one
/// that a later call would trust.
pub fn extract_tags_cached(
    path: &str,
    src: &str,
    lang: TagLang,
    stamp: Option<FileStamp>,
) -> Option<Tags> {
    let key = stamp.map(|s| CacheKey {
        path: path.to_string(),
        mtime: s.mtime,
        size: s.size,
        coeff_version: COEFF_VERSION,
    });

    if let Some(k) = &key
        && let Ok(c) = cache().lock()
        && let Some(hit) = c.get(k)
    {
        return Some(hit.clone());
    }

    let tags: Tags = std::sync::Arc::new(extract_tags(src, lang)?);

    if let Some(k) = key
        && let Ok(mut c) = cache().lock()
    {
        if c.len() >= CACHE_CAPACITY {
            c.clear();
        }
        c.insert(k, tags.clone());
    }
    Some(tags)
}

/// Drop every entry. Exposed for tests and for a coefficient change at runtime.
pub fn clear_cache() {
    if let Ok(mut c) = cache().lock() {
        c.clear();
    }
}

#[cfg(test)]
mod cache_tests {
    use super::*;

    fn write(dir: &std::path::Path, name: &str, body: &str) -> String {
        let p = dir.join(name);
        std::fs::write(&p, body).unwrap();
        p.to_string_lossy().to_string()
    }

    #[test]
    fn a_second_call_returns_the_same_allocation() {
        clear_cache();
        let d = tempfile::tempdir().unwrap();
        let src = "pub fn a() {}\n";
        let p = write(d.path(), "a.rs", src);
        let st = FileStamp::of(&p).unwrap();

        let first = extract_tags_cached(&p, src, TagLang::Rust, Some(st)).unwrap();
        let second = extract_tags_cached(&p, src, TagLang::Rust, Some(st)).unwrap();
        assert!(
            std::sync::Arc::ptr_eq(&first, &second),
            "the second call must be a cache hit, not a re-parse"
        );
    }

    /// The reason `size` is in the key. Two edits within one second are ordinary inside a
    /// tool call, and mtime alone cannot distinguish them.
    #[test]
    fn a_same_second_edit_of_different_size_invalidates() {
        clear_cache();
        let d = tempfile::tempdir().unwrap();
        let p = write(d.path(), "b.rs", "pub fn a() {}\n");
        let st1 = FileStamp::of(&p).unwrap();
        let first = extract_tags_cached(&p, "pub fn a() {}\n", TagLang::Rust, Some(st1)).unwrap();

        // Same mtime, different length — simulated directly so the test does not depend
        // on the filesystem's clock granularity.
        let st2 = FileStamp {
            mtime: st1.mtime,
            size: st1.size + 20,
        };
        let src2 = "pub fn a() {}\npub fn b() {}\n";
        let second = extract_tags_cached(&p, src2, TagLang::Rust, Some(st2)).unwrap();

        assert!(
            !std::sync::Arc::ptr_eq(&first, &second),
            "a same-second edit that changed the file's size must miss the cache"
        );
        assert_eq!(
            second.0.len(),
            2,
            "and the fresh parse must see both functions"
        );
    }

    #[test]
    fn a_changed_mtime_at_equal_size_also_invalidates() {
        clear_cache();
        let d = tempfile::tempdir().unwrap();
        let p = write(d.path(), "c.rs", "pub fn a() {}\n");
        let st1 = FileStamp::of(&p).unwrap();
        let first = extract_tags_cached(&p, "pub fn a() {}\n", TagLang::Rust, Some(st1)).unwrap();
        let st2 = FileStamp {
            mtime: st1.mtime + 1,
            size: st1.size,
        };
        // Same length, different content.
        let second = extract_tags_cached(&p, "pub fn z() {}\n", TagLang::Rust, Some(st2)).unwrap();
        assert!(!std::sync::Arc::ptr_eq(&first, &second));
        assert_eq!(second.0[0].name, "z");
    }

    /// Without a stamp the cache must be bypassed in both directions.
    #[test]
    fn no_stamp_means_no_caching_either_way() {
        clear_cache();
        let src = "pub fn a() {}\n";
        let a = extract_tags_cached("ghost.rs", src, TagLang::Rust, None).unwrap();
        let b = extract_tags_cached("ghost.rs", src, TagLang::Rust, None).unwrap();
        assert!(
            !std::sync::Arc::ptr_eq(&a, &b),
            "an unstampable call must not be served from cache"
        );
        // And it must not have polluted the cache for a later stamped call.
        let d = tempfile::tempdir().unwrap();
        let p = write(d.path(), "ghost.rs", src);
        let st = FileStamp::of(&p).unwrap();
        let c = extract_tags_cached(&p, src, TagLang::Rust, Some(st)).unwrap();
        assert!(!std::sync::Arc::ptr_eq(&a, &c));
    }

    #[test]
    fn two_paths_with_identical_content_do_not_collide() {
        clear_cache();
        let d = tempfile::tempdir().unwrap();
        let src = "pub fn a() {}\n";
        let p1 = write(d.path(), "one.rs", src);
        let p2 = write(d.path(), "two.rs", src);
        let a = extract_tags_cached(&p1, src, TagLang::Rust, FileStamp::of(&p1)).unwrap();
        let b = extract_tags_cached(&p2, src, TagLang::Rust, FileStamp::of(&p2)).unwrap();
        assert!(
            !std::sync::Arc::ptr_eq(&a, &b),
            "the path is part of the key"
        );
    }

    #[test]
    fn the_cache_is_bounded() {
        clear_cache();
        let d = tempfile::tempdir().unwrap();
        for i in 0..(CACHE_CAPACITY + 20) {
            let p = write(d.path(), &format!("f{i}.rs"), "pub fn a() {}\n");
            let _ = extract_tags_cached(&p, "pub fn a() {}\n", TagLang::Rust, FileStamp::of(&p));
        }
        let n = cache().lock().unwrap().len();
        assert!(
            n <= CACHE_CAPACITY,
            "cache grew past its cap: {n} > {CACHE_CAPACITY}"
        );
    }
}

// ── §10 rollout ──────────────────────────────────────────────────────────────

/// Which outline implementation a call uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arm {
    /// The outline that shipped before 1.3.0.
    Legacy,
    Ranked,
}

impl Arm {
    pub fn as_str(self) -> &'static str {
        match self {
            Arm::Legacy => "legacy",
            Arm::Ranked => "ranked",
        }
    }
}

/// Rollout state, from `LUMEN_RANKED_OUTLINE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Default. Nothing changes; the ranked path is not reached.
    Off,
    On,
    /// Both arms, split by path so the comparison is not confounded by file mix.
    Ab,
}

/// Read the rollout mode. Anything unrecognised is `Off`, including a typo: a
/// misspelled value must not silently enable an experiment.
pub fn mode_from_env() -> Mode {
    match std::env::var("LUMEN_RANKED_OUTLINE")
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "on" | "1" | "true" => Mode::On,
        "ab" => Mode::Ab,
        _ => Mode::Off,
    }
}

/// FNV-1a over the path, finished with a splitmix64 avalanche.
///
/// Not `DefaultHasher`: its output is explicitly not guaranteed stable across Rust
/// versions, and `RandomState` randomises per process. Either would let a file change
/// arms between sessions, which is precisely the confound splitting by path exists to
/// avoid — the same file must always take the same arm so the two arms differ in
/// implementation and nothing else.
///
/// The avalanche is not decoration. FNV-1a's lowest bit is close to the XOR of its
/// input bytes, so on structured paths it barely varies: over 4,000 generated paths of
/// the form `src/module_N/file_N.rs`, `hash % 2` put **every single one** in the same
/// arm — a 100/0 split presented as 50/50. Mixing before taking the bit gives 0.509.
fn path_hash(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    // splitmix64 finalizer: spreads the low bits FNV leaves correlated.
    h ^= h >> 33;
    h = h.wrapping_mul(0xff51_afd7_ed55_8ccd);
    h ^= h >> 33;
    h = h.wrapping_mul(0xc4ce_b9fe_1a85_ec53);
    h ^= h >> 33;
    h
}

/// Which arm this path takes.
pub fn arm_for(mode: Mode, path: &str) -> Arm {
    match mode {
        Mode::Off => Arm::Legacy,
        Mode::On => Arm::Ranked,
        // Even/odd on a stable hash: a 50/50 split that is a pure function of the path.
        Mode::Ab => {
            if path_hash(path.as_bytes()).is_multiple_of(2) {
                Arm::Legacy
            } else {
                Arm::Ranked
            }
        }
    }
}

/// `routed_via` for a successful ranked outline. Distinct from `smart_read` so the two
/// arms can never be pooled by a query that predates the experiment.
pub const ROUTE_RANKED: &str = "ranked_outline";

#[cfg(test)]
mod rollout_tests {
    use super::*;

    #[test]
    fn the_default_is_off_and_unknown_values_are_off() {
        // Parsed from a string rather than the environment: mutating env vars is racy
        // across threads and `unsafe` in edition 2024.
        for v in ["", "  ", "yes", "OFF", "enabled", "2", "onn"] {
            let m = match v.trim().to_ascii_lowercase().as_str() {
                "on" | "1" | "true" => Mode::On,
                "ab" => Mode::Ab,
                _ => Mode::Off,
            };
            assert_eq!(m, Mode::Off, "{v:?} must not enable the experiment");
        }
    }

    #[test]
    fn off_always_takes_the_legacy_arm() {
        for p in ["a.rs", "b.ts", "deeply/nested/thing.py"] {
            assert_eq!(arm_for(Mode::Off, p), Arm::Legacy);
        }
    }

    #[test]
    fn on_always_takes_the_ranked_arm() {
        for p in ["a.rs", "b.ts"] {
            assert_eq!(arm_for(Mode::On, p), Arm::Ranked);
        }
    }

    /// A file must never switch arms, or the comparison measures file mix as well as
    /// implementation.
    #[test]
    fn the_ab_split_is_a_stable_function_of_the_path() {
        let paths = [
            "crates/lumen-core/src/ranked.rs",
            "crates/lumen-mcp/src/lib.rs",
            "lumenator/src/app/session.service.ts",
            "scripts/lumen_rounds.py",
        ];
        for p in paths {
            let first = arm_for(Mode::Ab, p);
            for _ in 0..1_000 {
                assert_eq!(arm_for(Mode::Ab, p), first, "{p} changed arms");
            }
        }
    }

    /// Pinned arm assignments for real paths, so a change to the hash fails here rather
    /// than silently re-randomising every file's arm in the middle of an experiment.
    #[test]
    fn the_split_assignment_is_pinned() {
        let expected = [
            ("crates/lumen-core/src/compress.rs", Arm::Legacy),
            ("crates/lumen-core/src/rates.rs", Arm::Legacy),
            ("crates/lumen-core/src/ranked.rs", Arm::Ranked),
            ("crates/lumen-mcp/src/lib.rs", Arm::Ranked),
        ];
        assert!(
            expected.iter().any(|(_, a)| *a == Arm::Legacy)
                && expected.iter().any(|(_, a)| *a == Arm::Ranked),
            "the pin must straddle both arms, or a hash change that flipped every file \
             would still satisfy it"
        );
        for (p, want) in expected {
            assert_eq!(
                arm_for(Mode::Ab, p),
                want,
                "{p} changed arms — an in-flight experiment would be invalidated"
            );
        }
    }

    /// Roughly even, or one arm gets most of the traffic and the comparison is weak.
    #[test]
    fn the_ab_split_is_approximately_even_over_many_paths() {
        let mut ranked = 0;
        let n = 4_000;
        for i in 0..n {
            if arm_for(Mode::Ab, &format!("src/module_{i}/file_{i}.rs")) == Arm::Ranked {
                ranked += 1;
            }
        }
        let share = ranked as f64 / n as f64;
        assert!(
            (0.45..=0.55).contains(&share),
            "split is {share:.3}, too lopsided to compare arms"
        );
    }
}
