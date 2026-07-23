use crate::text_columnar::{cheapest, decode_value_column, generic_candidates};

const HEADER: &str = "SWGREP";

pub struct Factored {
    pub wire: String,
    pub decoded: String,
}

// Columnar grep/rg output: each line splits into path, line, and content columns, each picking its own
// codec (front coding folds shared path prefixes; content stays raw when unique). Non-match lines stay
// verbatim by position. Gated on a real saving and a byte-exact decode.
pub fn try_factor(raw: &str, cost: &dyn Fn(&str) -> usize) -> Option<Factored> {
    let mut types: Vec<u8> = Vec::new();
    let mut paths: Vec<&str> = Vec::new();
    let mut linenos: Vec<&str> = Vec::new();
    let mut contents: Vec<&str> = Vec::new();
    let mut suffixes: Vec<&str> = Vec::new();
    let mut verbatims: Vec<&str> = Vec::new();

    for line in raw.split('\n') {
        if let Some((path, line_no, content)) = split_match(line) {
            types.push(b'M');
            paths.push(path);
            linenos.push(line_no);
            contents.push(content);
        } else if let Some(path) = grep_path(line) {
            types.push(b'P');
            paths.push(path);
            suffixes.push(&line[path.len()..]);
        } else {
            types.push(b'V');
            verbatims.push(line);
        }
    }
    if paths.len() < 2 {
        return None;
    }

    let mut wire = format!(
        "{HEADER}\t{}\t{}\t{}\n",
        types.len(),
        linenos.len(),
        suffixes.len()
    );
    wire.push_str(&encode_types(&types));
    wire.push('\n');
    for v in &verbatims {
        wire.push_str(v);
        wire.push('\n');
    }
    wire.push_str(&cheapest(generic_candidates(&paths), cost));
    wire.push_str(&cheapest(generic_candidates(&linenos), cost));
    wire.push_str(&cheapest(generic_candidates(&contents), cost));
    wire.push_str(&cheapest(generic_candidates(&suffixes), cost));

    if cost(&wire) >= cost(raw) {
        return None;
    }
    let decoded = decode(&wire)?;
    if decoded != raw {
        return None;
    }
    Some(Factored { wire, decoded })
}

pub fn decode(wire: &str) -> Option<String> {
    let mut lines = wire.split('\n');
    let mut head = lines.next()?.split('\t');
    if head.next()? != HEADER {
        return None;
    }
    let n_rows: usize = head.next()?.parse().ok()?;
    let n_m: usize = head.next()?.parse().ok()?;
    let n_p: usize = head.next()?.parse().ok()?;

    let types = decode_types(lines.next()?, n_rows)?;
    let n_v = types.iter().filter(|&&t| t == b'V').count();
    let mut verbatims = Vec::with_capacity(n_v);
    for _ in 0..n_v {
        verbatims.push(lines.next()?);
    }

    let tag = lines.next()?;
    let paths = decode_value_column(tag, &mut lines, n_m + n_p)?;
    let tag = lines.next()?;
    let linenos = decode_value_column(tag, &mut lines, n_m)?;
    let tag = lines.next()?;
    let contents = decode_value_column(tag, &mut lines, n_m)?;
    let tag = lines.next()?;
    let suffixes = decode_value_column(tag, &mut lines, n_p)?;

    let (mut pi, mut mi, mut si, mut vi) = (0usize, 0usize, 0usize, 0usize);
    let mut out = Vec::with_capacity(n_rows);
    for &t in &types {
        match t {
            b'M' => {
                out.push(format!(
                    "{}:{}:{}",
                    paths.get(pi)?,
                    linenos.get(mi)?,
                    contents.get(mi)?
                ));
                pi += 1;
                mi += 1;
            }
            b'P' => {
                out.push(format!("{}{}", paths.get(pi)?, suffixes.get(si)?));
                pi += 1;
                si += 1;
            }
            _ => {
                out.push(verbatims.get(vi)?.to_string());
                vi += 1;
            }
        }
    }
    Some(out.join("\n"))
}

// Alternating M (match) / P (path only) / V (verbatim) runs as <type><count>, e.g. "M40V1M9".
fn encode_types(types: &[u8]) -> String {
    let mut out = String::new();
    let mut i = 0;
    while i < types.len() {
        let t = types[i];
        let mut j = i;
        while j < types.len() && types[j] == t {
            j += 1;
        }
        out.push(t as char);
        out.push_str(&(j - i).to_string());
        i = j;
    }
    out
}

fn decode_types(s: &str, total: usize) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(total);
    let mut chars = s.chars().peekable();
    while let Some(t) = chars.next() {
        let ty = match t {
            'M' => b'M',
            'P' => b'P',
            'V' => b'V',
            _ => return None,
        };
        let mut num = String::new();
        while chars.peek().is_some_and(char::is_ascii_digit) {
            num.push(chars.next()?);
        }
        out.resize(out.len() + num.parse::<usize>().ok()?, ty);
    }
    (out.len() == total).then_some(out)
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

    fn bytes(s: &str) -> usize {
        s.len()
    }

    #[test]
    fn factors_repeated_paths_and_shared_prefixes() {
        let mut lines = Vec::new();
        for i in 0..12 {
            lines.push(format!(
                "crates/report/src/scoreboard.rs:{}:let acme = find();",
                10 + i
            ));
            lines.push(format!(
                "crates/report/src/lib.rs:{}:let acme = find();",
                20 + i
            ));
        }
        let raw = lines.join("\n");
        let out = try_factor(&raw, &bytes).unwrap();
        assert!(
            out.wire.len() < raw.len(),
            "wire {} !< raw {}",
            out.wire.len(),
            raw.len()
        );
        assert!(out.wire.contains("crates/report/src/scoreboard.rs"));
        assert_eq!(decode(&out.wire).unwrap(), raw);
    }

    #[test]
    fn every_path_and_snippet_stays_literally_present() {
        let raw = (0..12)
            .map(|i| format!("src/very/long/module_path.rs:{i}:the same matched line here"))
            .collect::<Vec<_>>()
            .join("\n");
        let out = try_factor(&raw, &bytes).unwrap();
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
        let out = try_factor(&raw, &bytes).unwrap();
        assert_eq!(decode(&out.wire).unwrap(), raw);
    }

    #[test]
    fn factors_paths_without_line_numbers() {
        let raw = (0..12)
            .map(|i| format!("src/some/long/path.rs:matched fragment number {i} here"))
            .collect::<Vec<_>>()
            .join("\n");
        let out = try_factor(&raw, &bytes).unwrap();
        assert_eq!(decode(&out.wire).unwrap(), raw);
    }

    #[test]
    fn preserves_colons_and_tabs_inside_content() {
        let raw = (0..8)
            .map(|i| format!("p/q.rs:{i}:\tlet url = \"http://x:8080/path\";"))
            .collect::<Vec<_>>()
            .join("\n");
        let out = try_factor(&raw, &bytes).unwrap();
        assert_eq!(decode(&out.wire).unwrap(), raw);
    }

    #[test]
    fn refuses_prose_and_incompressible_input() {
        assert!(try_factor("just some prose\nwith no paths", &bytes).is_none());
        assert!(try_factor("a/b.rs:1:x\nc/d.rs:2:y", &bytes).is_none());
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
