//! Every class a frontend template uses must have styles that can reach it.
//!
//! A Rust test for a TypeScript concern, for one reason: it needs the filesystem, and the
//! Angular test environment is browser-shaped — `node:fs` does not resolve there. This
//! repository already checks non-Rust files from Rust (see `version_sync.rs`), so the
//! placement is at least consistent.
//!
//! The bug family it guards appeared twice. Angular scopes component CSS, and this
//! codebase reuses class names across routes:
//!
//!   - `.home`, `.home__inner` and `.tab-nav` were defined only in the Home component
//!     while Hotspots reused them, so that route painted no background (near-white text
//!     on a white window), had no navigation styling, and sat flush against the window
//!     edge.
//!   - `gauge-stage__project`, `gauge-stage__count`, `hero__warn` and `panel__project`
//!     were styled nowhere at all.
//!
//! Neither was caught by a test, because both look correct in whichever component owns
//! the stylesheet. This walks the templates instead.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

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

/// Class names appearing in a `class="…"` attribute.
fn classes_in(html: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut rest = html;
    while let Some(i) = rest.find("class=\"") {
        rest = &rest[i + 7..];
        let Some(end) = rest.find('"') else { break };
        for c in rest[..end].split_whitespace() {
            // Skip interpolated values: `class="{{ x }}"` names no literal class.
            if !c.is_empty() && !c.starts_with('{') && !c.contains('}') {
                out.insert(c.to_string());
            }
        }
        rest = &rest[end..];
    }
    out
}

/// Class names a stylesheet mentions as a selector.
fn selectors_in(css: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let b = css.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'.' {
            let start = i + 1;
            let mut j = start;
            while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'_' || b[j] == b'-') {
                j += 1;
            }
            if j > start {
                out.insert(css[start..j].to_string());
            }
            i = j;
        } else {
            i += 1;
        }
    }
    out
}

#[test]
fn every_template_class_has_styles_that_can_reach_it() {
    let Some(root) = workspace_root() else { return };
    let pages_dir = root.join("lumenator/src/app/pages");
    let Ok(entries) = std::fs::read_dir(&pages_dir) else {
        return; // No frontend checked out.
    };
    let global = std::fs::read_to_string(root.join("lumenator/src/styles.css"))
        .map(|c| selectors_in(&c))
        .unwrap_or_default();

    let mut checked = 0;
    let mut problems: Vec<String> = Vec::new();

    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let name = dir.file_name().unwrap().to_string_lossy().into_owned();
        let Ok(html) = std::fs::read_to_string(dir.join(format!("{name}.html"))) else {
            continue;
        };
        let own = std::fs::read_to_string(dir.join(format!("{name}.css")))
            .map(|c| selectors_in(&c))
            .unwrap_or_default();

        checked += 1;
        for c in classes_in(&html) {
            if !own.contains(&c) && !global.contains(&c) {
                problems.push(format!(
                    "  {name}.html uses .{c}, styled neither in {name}.css nor globally"
                ));
            }
        }
    }

    // A walk that silently matched nothing would make this test vacuous.
    assert!(
        checked >= 4,
        "expected at least 4 page templates, checked {checked}"
    );
    assert!(
        problems.is_empty(),
        "classes with nowhere to get their styles from — Angular scopes component CSS, so \
         a definition in another component cannot reach them:\n{}",
        problems.join("\n")
    );
}

/// The window chrome shared by every full-window route has to be global.
#[test]
fn the_shared_app_shell_is_defined_globally() {
    let Some(root) = workspace_root() else { return };
    let Ok(css) = std::fs::read_to_string(root.join("lumenator/src/styles.css")) else {
        return;
    };
    let global = selectors_in(&css);

    for cls in [
        "home",
        "home__inner",
        "home__head",
        "brand__tag",
        "tab-nav",
        "tab-nav__tab",
    ] {
        assert!(
            global.contains(cls),
            ".{cls} is reused across routes, so it must live in styles.css — a component \
             copy reaches only that component, which is how Hotspots ended up with no \
             background and no nav"
        );
    }
}
