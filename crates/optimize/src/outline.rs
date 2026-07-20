// Grammar-free structural outline of source: declarations with line numbers, nested by
// brace depth (brace languages) or indentation (the rest). Serves as the code offload
// preview: the model sees shape and line ranges, then resolves the marker for the body.

const MIN_DECLS: usize = 3;

struct Decl {
    line: usize,
    depth: usize,
    text: String,
}

pub fn declaration_count(code: &str) -> usize {
    declarations(code).len()
}

pub fn outline(code: &str, budget: usize) -> Option<String> {
    let decls = declarations(code);
    if decls.len() < MIN_DECLS {
        return None;
    }
    Some(render(&decls, code.lines().count(), budget))
}

fn declarations(code: &str) -> Vec<Decl> {
    let brace_regime = code.matches('{').count() * 8 > code.lines().count();
    let mut depth_before: i32 = 0;
    let mut decls = Vec::new();
    for (i, raw) in code.lines().enumerate() {
        let trimmed = raw.trim_start();
        if is_declaration(trimmed) {
            let depth = if brace_regime {
                depth_before.max(0) as usize
            } else {
                indent_depth(raw)
            };
            decls.push(Decl {
                line: i + 1,
                depth,
                text: header(trimmed),
            });
        }
        depth_before += raw.matches('{').count() as i32 - raw.matches('}').count() as i32;
    }
    decls
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

fn header(trimmed: &str) -> String {
    trimmed
        .trim_end()
        .trim_end_matches(['{', '(', ':'])
        .trim_end()
        .chars()
        .take(90)
        .collect()
}

fn indent_depth(raw: &str) -> usize {
    let mut spaces = 0;
    let mut tabs = 0;
    for c in raw.chars() {
        match c {
            ' ' => spaces += 1,
            '\t' => tabs += 1,
            _ => break,
        }
    }
    tabs + spaces / 4
}

fn is_declaration(line: &str) -> bool {
    const MODIFIERS: &[&str] = &[
        "pub ",
        "pub(crate) ",
        "pub(super) ",
        "public ",
        "private ",
        "protected ",
        "internal ",
        "export ",
        "export default ",
        "default ",
        "async ",
        "static ",
        "final ",
        "abstract ",
        "sealed ",
        "open ",
        "override ",
        "unsafe ",
        "const ",
    ];
    const LEADS: &[&str] = &[
        "fn ",
        "def ",
        "func ",
        "function ",
        "class ",
        "struct ",
        "enum ",
        "trait ",
        "impl ",
        "interface ",
        "protocol ",
        "extension ",
        "type ",
        "module ",
        "mod ",
        "namespace ",
        "package ",
        "object ",
        "record ",
        "macro_rules!",
        "import ",
        "from ",
        "use ",
        "#include",
        "let ",
        "var ",
        "val ",
    ];
    let mut rest = line;
    while let Some(m) = MODIFIERS.iter().find(|m| rest.starts_with(**m)) {
        rest = &rest[m.len()..];
    }
    LEADS.iter().any(|lead| rest.starts_with(lead))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nests_methods_under_their_container_in_a_brace_language() {
        let code = "\
pub struct Optimizer {
    counter: u32,
}
impl Optimizer {
    pub fn new() -> Self {
        Self { counter: 0 }
    }
    fn tick(&mut self) {
        self.counter += 1;
    }
}
fn main() {}
";
        let out = outline(code, 4096).unwrap();
        assert!(out.contains("L1 pub struct Optimizer"));
        assert!(out.contains("L4 impl Optimizer"));
        // methods sit one level in from impl
        assert!(out.contains("  L5 pub fn new() -> Self"));
        assert!(out.contains("  L8 fn tick(&mut self)"));
        assert!(out.contains("L12 fn main"));
    }

    #[test]
    fn nests_by_indentation_in_a_braceless_language() {
        let code = "\
class Service:
    def start(self):
        return True
    def stop(self):
        return False
def helper():
    pass
";
        let out = outline(code, 4096).unwrap();
        assert!(out.contains("L1 class Service"));
        assert!(out.contains("  L2 def start(self)"));
        assert!(out.contains("  L4 def stop(self)"));
        assert!(out.contains("L6 def helper"));
    }

    #[test]
    fn prose_is_not_code() {
        let text = "The quick brown fox. Jumps over the lazy dog. Nothing to see here.";
        assert!(outline(text, 4096).is_none());
    }

    #[test]
    fn budget_truncates_with_a_remainder() {
        let code: String = (0..50)
            .map(|i| format!("fn handler_{i}() {{}}\n"))
            .collect();
        let out = outline(&code, 120).unwrap();
        assert!(out.contains("more declarations"));
    }
}
