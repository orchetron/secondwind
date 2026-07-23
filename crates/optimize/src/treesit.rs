// Real-AST code outline via tree-sitter, feature-gated so the lean edge builds stay grammar-free.
// Extracts declarations with full multi-line signatures and their attributes/decorators, and no
// local-variable noise. Language is auto-detected by best-parse, so no filename hint is needed.

use tree_sitter::{Node, Parser};

const MIN_DECLS: usize = 3;
const MAX_SIG: usize = 200;

#[derive(Clone, Copy)]
enum Lang {
    Rust,
    Python,
}

impl Lang {
    fn language(self) -> tree_sitter::Language {
        match self {
            Lang::Rust => tree_sitter_rust::LANGUAGE.into(),
            Lang::Python => tree_sitter_python::LANGUAGE.into(),
        }
    }
    fn decl_kinds(self) -> &'static [&'static str] {
        match self {
            Lang::Rust => &[
                "function_item",
                "struct_item",
                "enum_item",
                "trait_item",
                "impl_item",
                "mod_item",
                "type_item",
                "const_item",
                "static_item",
                "macro_definition",
                "use_declaration",
            ],
            Lang::Python => &[
                "function_definition",
                "class_definition",
                "import_statement",
                "import_from_statement",
            ],
        }
    }
    fn body_kinds(self) -> &'static [&'static str] {
        match self {
            Lang::Rust => &[
                "block",
                "declaration_list",
                "field_declaration_list",
                "enum_variant_list",
            ],
            Lang::Python => &["block"],
        }
    }
}

pub fn outline(code: &str, budget: usize) -> Option<String> {
    let lang = detect(code)?;
    let tree = parse(code, lang)?;
    let mut decls = Vec::new();
    collect(tree.root_node(), code.as_bytes(), lang, 0, &mut decls);
    if decls.len() < MIN_DECLS {
        return None;
    }
    Some(render(&decls, code.lines().count(), budget))
}

fn parse(code: &str, lang: Lang) -> Option<tree_sitter::Tree> {
    let mut parser = Parser::new();
    parser.set_language(&lang.language()).ok()?;
    parser.parse(code, None)
}

// Pick the grammar that parses with the fewest error/missing nodes and finds enough declarations,
// so prose (few decls under every grammar) is rejected rather than mis-detected as code.
fn detect(code: &str) -> Option<Lang> {
    let mut best: Option<(Lang, usize, usize)> = None;
    for lang in [Lang::Rust, Lang::Python] {
        let Some(tree) = parse(code, lang) else {
            continue;
        };
        let (errors, decls) = score(tree.root_node(), lang);
        if decls < MIN_DECLS {
            continue;
        }
        let better = best.is_none_or(|(_, be, bd)| errors < be || (errors == be && decls > bd));
        if better {
            best = Some((lang, errors, decls));
        }
    }
    best.map(|(lang, ..)| lang)
}

fn score(node: Node, lang: Lang) -> (usize, usize) {
    let mut errors = usize::from(node.is_error() || node.is_missing());
    let mut decls = usize::from(lang.decl_kinds().contains(&node.kind()));
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        let (e, d) = score(child, lang);
        errors += e;
        decls += d;
    }
    (errors, decls)
}

struct Decl {
    line: usize,
    depth: usize,
    text: String,
}

fn collect(node: Node, src: &[u8], lang: Lang, depth: usize, out: &mut Vec<Decl>) {
    let next_depth = if lang.decl_kinds().contains(&node.kind()) {
        let start = decl_start(node);
        out.push(Decl {
            line: start.start_position().row + 1,
            depth,
            text: signature(node, start, src, lang),
        });
        depth + 1
    } else {
        depth
    };
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect(child, src, lang, next_depth, out);
    }
}

// The node where the declaration's rendered text starts: the wrapping decorated_definition (Python)
// or the earliest contiguous preceding attribute (Rust), so decorators/attributes are kept.
fn decl_start(node: Node) -> Node {
    if let Some(parent) = node.parent()
        && parent.kind() == "decorated_definition"
    {
        return parent;
    }
    let mut start = node;
    while let Some(prev) = start.prev_sibling() {
        if prev.kind() == "attribute_item" {
            start = prev;
        } else {
            break;
        }
    }
    start
}

fn signature(node: Node, start: Node, src: &[u8], lang: Lang) -> String {
    let mut cursor = node.walk();
    let body = node
        .children(&mut cursor)
        .find(|c| lang.body_kinds().contains(&c.kind()))
        .map(|b| b.start_byte())
        .unwrap_or(node.end_byte());
    let text = std::str::from_utf8(&src[start.start_byte()..body]).unwrap_or("");
    let flattened = text.split_whitespace().collect::<Vec<_>>().join(" ");
    flattened.chars().take(MAX_SIG).collect()
}

fn render(decls: &[Decl], total_lines: usize, budget: usize) -> String {
    let mut out = format!("{total_lines} lines");
    for (i, decl) in decls.iter().enumerate() {
        let indent = "  ".repeat(decl.depth.min(6));
        let line = format!("\n{indent}L{} {}", decl.line, decl.text);
        if i > 0 && out.len() + line.len() > budget {
            out.push_str(&format!("\n+{} more declarations", decls.len() - i));
            break;
        }
        out.push_str(&line);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_full_multiline_signatures_and_attributes_no_local_noise() {
        let code = "\
use std::fmt;

#[derive(Debug, Clone)]
pub struct Widget {
    id: u64,
}

#[inline]
pub fn build(
    name: &str,
    size: usize,
) -> Result<Widget, Error> {
    let local = 1;
    let noise = 2;
    Ok(Widget { id: 0 })
}

pub const MAX: usize = 10;
";
        let out = outline(code, 8192).unwrap();
        assert!(
            out.contains("#[derive(Debug, Clone)] pub struct Widget"),
            "{out}"
        );
        assert!(
            out.contains(
                "#[inline] pub fn build( name: &str, size: usize, ) -> Result<Widget, Error>"
            ),
            "full signature + attribute, got:\n{out}"
        );
        assert!(
            !out.contains("local"),
            "must not leak local variables:\n{out}"
        );
        assert!(
            !out.contains("noise"),
            "must not leak local variables:\n{out}"
        );
    }

    #[test]
    fn python_keeps_decorators_and_nesting() {
        let code = "\
import os

class Service:
    @property
    def name(self):
        x = 1
        return self._name

    def start(self, port):
        return True

def helper():
    pass
";
        let out = outline(code, 8192).unwrap();
        assert!(out.contains("class Service"), "{out}");
        assert!(
            out.contains("@property def name(self)"),
            "decorator kept:\n{out}"
        );
        assert!(out.contains("def start(self, port)"), "{out}");
        assert!(!out.contains("x = 1"), "no local noise:\n{out}");
    }

    #[test]
    fn abstains_on_prose() {
        assert!(
            outline(
                "The quick brown fox jumps over the lazy dog. Nothing here.",
                8192
            )
            .is_none()
        );
    }
}
