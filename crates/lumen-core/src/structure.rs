use tree_sitter::{Language, Node, Parser};

pub enum Lang {
    Rust,
    Python,
    TypeScript,
    Tsx,
    Unknown,
}

pub fn detect_lang(path: &str) -> Lang {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "rs" => Lang::Rust,
        "py" | "pyi" => Lang::Python,
        "ts" => Lang::TypeScript,
        "tsx" => Lang::Tsx,
        _ => Lang::Unknown,
    }
}

pub struct CodeItem {
    pub kind: String,
    pub name: Option<String>,
    pub start_line: usize,
    pub end_line: usize,
    pub start_byte: usize,
    pub end_byte: usize,
}

pub fn outline(src: &str, lang: Lang) -> Vec<CodeItem> {
    let ts_lang: Language = match lang {
        Lang::Unknown => return whole_file_item(src),
        Lang::Rust => tree_sitter_rust::LANGUAGE.into(),
        Lang::Python => tree_sitter_python::LANGUAGE.into(),
        Lang::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        Lang::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
    };

    let mut parser = Parser::new();
    if parser.set_language(&ts_lang).is_err() {
        return whole_file_item(src);
    }

    let tree = match parser.parse(src, None) {
        Some(t) => t,
        None => return whole_file_item(src),
    };

    let root = tree.root_node();
    let mut items = Vec::new();

    for i in 0..root.named_child_count() {
        let node = root.named_child(i as u32).unwrap();
        if node.is_error() || node.is_missing() {
            continue;
        }
        if let Some(item) = to_item(&node, src, &lang) {
            items.push(item);
        }
    }

    items
}

fn whole_file_item(src: &str) -> Vec<CodeItem> {
    let line_count = src.lines().count().max(1);
    vec![CodeItem {
        kind: "file".to_string(),
        name: None,
        start_line: 1,
        end_line: line_count,
        start_byte: 0,
        end_byte: src.len(),
    }]
}

fn to_item(node: &Node, src: &str, lang: &Lang) -> Option<CodeItem> {
    let (kind_str, name) = classify(node, src, lang)?;
    let start = node.start_position();
    let end = node.end_position();
    Some(CodeItem {
        kind: kind_str.to_string(),
        name,
        start_line: start.row + 1,
        end_line: end.row + 1,
        start_byte: node.start_byte(),
        end_byte: node.end_byte(),
    })
}

fn classify<'a>(node: &Node, src: &str, lang: &Lang) -> Option<(&'static str, Option<String>)> {
    match lang {
        Lang::Rust => classify_rust(node, src),
        Lang::Python => classify_python(node, src),
        Lang::TypeScript | Lang::Tsx => classify_ts(node, src),
        Lang::Unknown => None,
    }
}

fn classify_rust(node: &Node, src: &str) -> Option<(&'static str, Option<String>)> {
    let kind = match node.kind() {
        "function_item" => "function",
        "struct_item" => "struct",
        "enum_item" => "enum",
        "impl_item" => "impl",
        "mod_item" => "mod",
        "use_declaration" => "import",
        "const_item" => "const",
        "type_alias_item" => "type",
        "trait_item" => "trait",
        "macro_invocation" | "macro_definition" => "macro",
        _ => return None,
    };
    // impl_item names the type being implemented via the "type" field
    let name = extract_fields(node, src, &["name", "type"]);
    Some((kind, name))
}

fn classify_python(node: &Node, src: &str) -> Option<(&'static str, Option<String>)> {
    match node.kind() {
        "function_definition" => Some(("function", extract_fields(node, src, &["name"]))),
        "class_definition" => Some(("class", extract_fields(node, src, &["name"]))),
        "import_statement" | "import_from_statement" => {
            Some(("import", extract_fields(node, src, &["name"])))
        }
        // Delegate to the inner function_definition / class_definition
        "decorated_definition" => {
            let def = node.child_by_field_name("definition")?;
            classify_python(&def, src)
        }
        _ => None,
    }
}

fn classify_ts(node: &Node, src: &str) -> Option<(&'static str, Option<String>)> {
    match node.kind() {
        "function_declaration" => Some(("function", extract_fields(node, src, &["name"]))),
        "class_declaration" => Some(("class", extract_fields(node, src, &["name"]))),
        "interface_declaration" => Some(("interface", extract_fields(node, src, &["name"]))),
        "type_alias_declaration" => Some(("type", extract_fields(node, src, &["name"]))),
        "enum_declaration" => Some(("enum", extract_fields(node, src, &["name"]))),
        "import_statement" => Some(("import", None)),
        "export_statement" => {
            // Prefer the inner declaration's kind/name when present
            if let Some(decl) = node.child_by_field_name("declaration") {
                if let Some(result) = classify_ts(&decl, src) {
                    return Some(result);
                }
            }
            Some(("export", None))
        }
        "lexical_declaration" => {
            // const / let: name is in the first variable_declarator child
            let name = node
                .named_child(0)
                .and_then(|d| d.child_by_field_name("name"))
                .map(|n| node_text(&n, src));
            Some(("const", name))
        }
        "variable_statement" => Some(("var", None)),
        _ => None,
    }
}

fn extract_fields(node: &Node, src: &str, fields: &[&str]) -> Option<String> {
    for &field in fields {
        if let Some(child) = node.child_by_field_name(field) {
            return Some(node_text(&child, src));
        }
    }
    None
}

fn node_text(node: &Node, src: &str) -> String {
    src[node.start_byte()..node.end_byte()].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_lang_by_extension() {
        assert!(matches!(detect_lang("foo.rs"), Lang::Rust));
        assert!(matches!(detect_lang("foo.py"), Lang::Python));
        assert!(matches!(detect_lang("foo.pyi"), Lang::Python));
        assert!(matches!(detect_lang("foo.ts"), Lang::TypeScript));
        assert!(matches!(detect_lang("foo.tsx"), Lang::Tsx));
        assert!(matches!(detect_lang("foo.txt"), Lang::Unknown));
        assert!(matches!(detect_lang("Makefile"), Lang::Unknown));
        assert!(matches!(detect_lang("path/to/file.rs"), Lang::Rust));
    }

    #[test]
    fn unknown_lang_yields_whole_file_item() {
        let src = "some content\non two lines";
        let items = outline(src, Lang::Unknown);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].kind, "file");
        assert_eq!(items[0].start_line, 1);
        assert_eq!(items[0].end_line, 2);
        assert_eq!(items[0].start_byte, 0);
        assert_eq!(items[0].end_byte, src.len());
    }

    #[test]
    fn rust_outline() {
        let src = r#"use std::io;
use std::path::Path;

struct Point {
    x: f64,
    y: f64,
}

enum Color { Red, Green, Blue }

trait Shape {
    fn area(&self) -> f64;
}

impl Shape for Point {
    fn area(&self) -> f64 { 0.0 }
}

fn distance(a: &Point, b: &Point) -> f64 {
    0.0
}

const MAX: usize = 100;
"#;
        let items = outline(src, Lang::Rust);
        assert!(!items.is_empty());

        let kinds: Vec<&str> = items.iter().map(|i| i.kind.as_str()).collect();
        assert!(kinds.contains(&"import"), "missing import, got: {kinds:?}");
        assert!(kinds.contains(&"struct"), "missing struct");
        assert!(kinds.contains(&"enum"), "missing enum");
        assert!(kinds.contains(&"trait"), "missing trait");
        assert!(kinds.contains(&"impl"), "missing impl");
        assert!(kinds.contains(&"function"), "missing function");
        assert!(kinds.contains(&"const"), "missing const");

        let names: Vec<Option<&str>> = items.iter().map(|i| i.name.as_deref()).collect();
        assert!(names.contains(&Some("Point")), "missing struct name");
        assert!(names.contains(&Some("Color")), "missing enum name");
        assert!(names.contains(&Some("distance")), "missing fn name");
        assert!(names.contains(&Some("MAX")), "missing const name");

        for item in &items {
            assert!(item.start_line >= 1, "start_line must be >= 1");
            assert!(
                item.end_line >= item.start_line,
                "end_line must be >= start_line for {:?}",
                item.kind
            );
            assert!(item.start_byte <= item.end_byte);
        }

        // No overlapping ranges
        let mut prev_end = 0usize;
        let mut sorted = items.iter().collect::<Vec<_>>();
        sorted.sort_by_key(|i| i.start_byte);
        for item in sorted {
            assert!(
                item.start_byte >= prev_end,
                "overlapping items at byte {}",
                item.start_byte
            );
            prev_end = item.end_byte;
        }
    }

    #[test]
    fn python_outline() {
        let src = r#"import os
from pathlib import Path

class Greeter:
    def greet(self, name: str) -> str:
        return f"hello {name}"

@staticmethod
def standalone():
    pass

def another():
    x = 1
    return x
"#;
        let items = outline(src, Lang::Python);
        assert!(!items.is_empty());

        let kinds: Vec<&str> = items.iter().map(|i| i.kind.as_str()).collect();
        assert!(kinds.contains(&"import"), "missing import, got: {kinds:?}");
        assert!(kinds.contains(&"class"), "missing class");
        assert!(kinds.contains(&"function"), "missing function");

        let names: Vec<Option<&str>> = items.iter().map(|i| i.name.as_deref()).collect();
        assert!(names.contains(&Some("Greeter")), "missing class name");
        assert!(names.contains(&Some("another")), "missing function name");
        // decorated function should also appear
        assert!(names.contains(&Some("standalone")), "missing decorated fn name");

        for item in &items {
            assert!(item.start_line >= 1);
            assert!(item.end_line >= item.start_line);
            assert!(item.start_byte <= item.end_byte);
        }
    }

    #[test]
    fn typescript_outline() {
        let src = r#"import { readFile } from 'fs';
import type { Foo } from './types';

interface Shape {
    area(): number;
}

type Color = 'red' | 'green' | 'blue';

class Circle implements Shape {
    constructor(public radius: number) {}
    area(): number { return Math.PI * this.radius ** 2; }
}

function greet(name: string): string {
    return `Hello ${name}`;
}

export function exported(): void {}

const MAX_SIZE = 100;
"#;
        let items = outline(src, Lang::TypeScript);
        assert!(!items.is_empty());

        let kinds: Vec<&str> = items.iter().map(|i| i.kind.as_str()).collect();
        assert!(kinds.contains(&"import"), "missing import, got: {kinds:?}");
        assert!(kinds.contains(&"interface"), "missing interface");
        assert!(kinds.contains(&"type"), "missing type alias");
        assert!(kinds.contains(&"class"), "missing class");
        assert!(kinds.contains(&"function"), "missing function");

        let names: Vec<Option<&str>> = items.iter().map(|i| i.name.as_deref()).collect();
        assert!(names.contains(&Some("Shape")), "missing interface name");
        assert!(names.contains(&Some("Color")), "missing type alias name");
        assert!(names.contains(&Some("Circle")), "missing class name");
        assert!(names.contains(&Some("greet")), "missing function name");
        // exported function should appear as "function" with name "exported"
        assert!(names.contains(&Some("exported")), "missing exported fn name");

        for item in &items {
            assert!(item.start_line >= 1);
            assert!(item.end_line >= item.start_line);
            assert!(item.start_byte <= item.end_byte);
        }
    }

    #[test]
    fn tsx_outline() {
        let src = r#"import React from 'react';

interface Props {
    name: string;
}

function App({ name }: Props): JSX.Element {
    return <div>{name}</div>;
}

export default App;
"#;
        let items = outline(src, Lang::Tsx);
        let kinds: Vec<&str> = items.iter().map(|i| i.kind.as_str()).collect();
        assert!(kinds.contains(&"import"), "missing import in tsx");
        assert!(kinds.contains(&"interface") || kinds.contains(&"function"),
            "tsx should find interface or function, got: {kinds:?}");
    }

    #[test]
    fn error_nodes_are_skipped() {
        // Severely broken Rust source — tree-sitter still produces a tree but
        // ERROR nodes should be skipped gracefully, never panic.
        let src = "fn {{{{{ broken @@@ source";
        let items = outline(src, Lang::Rust);
        // Should not panic. May return empty or partial.
        let _ = items;
    }

    #[test]
    fn line_ranges_within_source_bounds() {
        let src = "fn foo() {}\nfn bar() {}\n";
        let total_lines = src.lines().count();
        let items = outline(src, Lang::Rust);
        for item in &items {
            assert!(item.start_line >= 1);
            assert!(item.end_line <= total_lines, "end_line {} > total {}", item.end_line, total_lines);
            assert!(item.end_byte <= src.len());
        }
    }
}
