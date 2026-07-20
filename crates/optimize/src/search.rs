const HEADER: &str = "SWGREP";

pub struct Factored {
    pub wire: String,
    pub decoded: String,
}

enum Row<'a> {
    Match(usize, &'a str, usize),
    Path(usize, &'a str),
    Verbatim(&'a str),
}

// Factor repeated path and snippet into dictionaries, values kept literal; non-match lines verbatim. Gated on byte-exact decode.
pub fn try_factor(raw: &str) -> Option<Factored> {
    let mut paths = Dict::default();
    let mut snippets = Dict::default();
    let mut rows = Vec::new();
    let mut matches = 0;

    for line in raw.split('\n') {
        if let Some((path, line_no, content)) = split_match(line) {
            rows.push(Row::Match(
                paths.intern(path),
                line_no,
                snippets.intern(content),
            ));
            matches += 1;
        } else if let Some(path) = grep_path(line) {
            rows.push(Row::Path(paths.intern(path), &line[path.len()..]));
            matches += 1;
        } else {
            rows.push(Row::Verbatim(line));
        }
    }
    if matches < 2 {
        return None;
    }

    let mut wire = format!("{HEADER} {}", rows.len());
    write_dict(&mut wire, &paths.items);
    write_dict(&mut wire, &snippets.items);
    for row in &rows {
        wire.push('\n');
        match row {
            Row::Match(pi, ln, si) => wire.push_str(&format!("M\t{pi}\t{ln}\t{si}")),
            Row::Path(pi, suffix) => wire.push_str(&format!("P\t{pi}\t{suffix}")),
            Row::Verbatim(line) => wire.push_str(&format!("V\t{line}")),
        }
    }

    if wire.len() >= raw.len() {
        return None;
    }
    let decoded = decode(&wire)?;
    if decoded != raw {
        return None;
    }
    Some(Factored { wire, decoded })
}

pub fn decode(wire: &str) -> Option<String> {
    let (tag, rest) = wire.split_once(' ')?;
    if tag != HEADER {
        return None;
    }
    let mut lines = rest.split('\n');
    let count: usize = lines.next()?.trim().parse().ok()?;
    let paths = read_dict(&mut lines)?;
    let snippets = read_dict(&mut lines)?;

    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        let (tag, payload) = lines.next()?.split_once('\t')?;
        match tag {
            "M" => {
                let mut cols = payload.splitn(3, '\t');
                let path = paths.get(cols.next()?.parse::<usize>().ok()?)?;
                let line_no = cols.next()?;
                let snippet = snippets.get(cols.next()?.parse::<usize>().ok()?)?;
                out.push(format!("{path}:{line_no}:{snippet}"));
            }
            "P" => {
                let (idx, suffix) = payload.split_once('\t')?;
                let path = paths.get(idx.parse::<usize>().ok()?)?;
                out.push(format!("{path}{suffix}"));
            }
            "V" => out.push(payload.to_string()),
            _ => return None,
        }
    }
    if lines.next().is_some() {
        return None;
    }
    Some(out.join("\n"))
}

// Offload preview for a search: each file once with its match line numbers, spent greedily against the budget.
pub fn location_map(text: &str, budget: usize) -> Option<String> {
    let mut order: Vec<&str> = Vec::new();
    let mut lines_by_path: std::collections::HashMap<&str, Vec<&str>> =
        std::collections::HashMap::new();
    let mut matches = 0;

    for line in text.split('\n') {
        if let Some((path, line_no, _)) = split_match(line) {
            if !lines_by_path.contains_key(path) {
                order.push(path);
            }
            lines_by_path.entry(path).or_default().push(line_no);
            matches += 1;
        }
    }
    if matches < 2 {
        return None;
    }

    let mut out = format!("{matches} matches in {} files:", order.len());
    for (fi, path) in order.iter().enumerate() {
        if out.len() >= budget {
            out.push_str(&format!("\n+{} more files", order.len() - fi));
            break;
        }
        let nums = &lines_by_path[path];
        let mut shown: Vec<&str> = Vec::new();
        for n in nums {
            let width: usize = shown.iter().map(|s| s.len() + 2).sum();
            if !shown.is_empty() && out.len() + path.len() + 2 + width + n.len() > budget {
                break;
            }
            shown.push(n);
        }
        out.push_str(&format!("\n{path}: {}", shown.join(", ")));
        let more = nums.len() - shown.len();
        if more > 0 {
            out.push_str(&format!(", +{more} more"));
        }
    }
    Some(out)
}

#[derive(Default)]
struct Dict<'a> {
    items: Vec<&'a str>,
    index: std::collections::HashMap<&'a str, usize>,
}

impl<'a> Dict<'a> {
    fn intern(&mut self, s: &'a str) -> usize {
        if let Some(&pos) = self.index.get(s) {
            return pos;
        }
        let pos = self.items.len();
        self.items.push(s);
        self.index.insert(s, pos);
        pos
    }
}

fn write_dict(wire: &mut String, items: &[&str]) {
    wire.push('\n');
    wire.push_str(&items.len().to_string());
    for it in items {
        wire.push('\n');
        wire.push_str(it);
    }
}

fn read_dict<'a>(lines: &mut std::str::Split<'a, char>) -> Option<Vec<&'a str>> {
    let n: usize = lines.next()?.parse().ok()?;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        out.push(lines.next()?);
    }
    Some(out)
}

// `path:line:content`: path (contains / or ., no spaces) up to the first colon,
// an all-digit line number up to the second colon, content (any bytes) after.
fn split_match(line: &str) -> Option<(&str, &str, &str)> {
    let path = grep_path(line)?;
    let after_path = &line[path.len() + 1..];
    let colon = after_path.find(':')?;
    let line_no = &after_path[..colon];
    if line_no.is_empty() || !line_no.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((path, line_no, &after_path[colon + 1..]))
}

fn grep_path(line: &str) -> Option<&str> {
    let colon = line.find(':')?;
    let path = &line[..colon];
    if path.is_empty() || path.contains(' ') || !(path.contains('/') || path.contains('.')) {
        return None;
    }
    Some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn factors_both_repeated_paths_and_repeated_snippets() {
        let raw = "src/a.rs:10:// TODO fix\nsrc/a.rs:44:// TODO fix\nsrc/b.rs:3:// TODO fix\nsrc/b.rs:9:done";
        let out = try_factor(raw).unwrap();
        assert_eq!(out.wire.matches("src/a.rs").count(), 1);
        assert_eq!(out.wire.matches("// TODO fix").count(), 1);
        assert!(out.wire.len() < raw.len());
        assert_eq!(decode(&out.wire).unwrap(), raw);
    }

    #[test]
    fn every_path_and_snippet_stays_literally_present() {
        let raw = (0..12)
            .map(|i| format!("src/very/long/module_path.rs:{i}:the same matched line here"))
            .collect::<Vec<_>>()
            .join("\n");
        let out = try_factor(&raw).unwrap();
        assert!(out.wire.contains("src/very/long/module_path.rs"));
        assert!(out.wire.contains("the same matched line here"));
        assert_eq!(decode(&out.wire).unwrap(), raw);
    }

    #[test]
    fn tolerates_a_trailing_newline_and_blank_separators() {
        let raw = (0..8)
            .map(|i| format!("src/a.rs:{i}:hit\n\nsrc/b.rs:{i}:hit\n--"))
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        let out = try_factor(&raw).unwrap();
        assert_eq!(decode(&out.wire).unwrap(), raw);
    }

    #[test]
    fn factors_paths_without_line_numbers() {
        let raw = (0..12)
            .map(|i| format!("src/some/long/path.rs:matched fragment number {i} here"))
            .collect::<Vec<_>>()
            .join("\n");
        let out = try_factor(&raw).unwrap();
        assert_eq!(decode(&out.wire).unwrap(), raw);
    }

    #[test]
    fn preserves_colons_and_tabs_inside_content() {
        let raw = (0..8)
            .map(|i| format!("p/q.rs:{i}:\tlet url = \"http://x:8080/path\";"))
            .collect::<Vec<_>>()
            .join("\n");
        let out = try_factor(&raw).unwrap();
        assert_eq!(decode(&out.wire).unwrap(), raw);
    }

    #[test]
    fn refuses_prose_and_incompressible_input() {
        assert!(try_factor("just some prose\nwith no paths").is_none());
        assert!(try_factor("a/b.rs:1:x\nc/d.rs:2:y").is_none());
    }

    #[test]
    fn location_map_lists_every_file_and_its_match_lines() {
        let raw = "src/a.rs:10:x\nsrc/a.rs:22:y\nsrc/b.rs:5:z\nnoise line";
        let map = location_map(raw, 4096).unwrap();
        assert!(map.starts_with("3 matches in 2 files:"));
        assert!(map.contains("src/a.rs: 10, 22"));
        assert!(map.contains("src/b.rs: 5"));
    }

    #[test]
    fn location_map_stays_within_a_tight_budget() {
        let raw = (0..200)
            .map(|i| format!("src/file_{i}.rs:{i}:match"))
            .collect::<Vec<_>>()
            .join("\n");
        let map = location_map(&raw, 200).unwrap();
        assert!(map.len() <= 200 + 40);
        assert!(map.contains("more files"));
    }
}
