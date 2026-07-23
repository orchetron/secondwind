const HEADER: &str = "SWTREE";
const BAR: &str = "\u{2502}   ";
const GAP: &str = "    ";
const TEE: &str = "\u{251c}\u{2500}\u{2500} ";
const ELL: &str = "\u{2514}\u{2500}\u{2500} ";

// Readable inline compression for box-drawing trees (cargo tree): the drawing (bars, tees) is fully
// determined by the depth sequence, so we store a markdown nested list (`- ` + 2-space indent, which
// even weak models read) and rebuild the exact unicode drawing on decode. Section headers (bars, no
// connector) are indented labels without a bullet. Byte-exact or it declines.
pub fn encode(raw: &str) -> Option<String> {
    let (body, trailing) = match raw.strip_suffix('\n') {
        Some(b) => (b, true),
        None => (raw, false),
    };
    if body.is_empty() {
        return None;
    }
    let lines: Vec<&str> = body.split('\n').collect();
    if lines.len() < 4 {
        return None;
    }
    let mut saw_tree = false;
    let mut out = format!("{HEADER}{}", trailing as u8);
    for line in &lines {
        let (depth, bare, content) = parse_line(line)?;
        if depth > 0 {
            saw_tree = true;
        }
        out.push('\n');
        let indent = if bare { depth } else { depth.saturating_sub(1) };
        for _ in 0..indent {
            out.push_str("  ");
        }
        if !bare && depth > 0 {
            out.push_str("- ");
        }
        out.push_str(content);
    }
    if !saw_tree {
        return None;
    }
    (decode(&out).as_deref() == Some(raw)).then_some(out)
}

// Returns (depth, is_bare, content). A connector line is a node at depth bars+1; a bars-only line is
// a bare label at depth bars; a line with no drawing is the depth-0 root.
fn parse_line(line: &str) -> Option<(usize, bool, &str)> {
    let mut rest = line;
    let mut bars = 0;
    while let Some(r) = rest.strip_prefix(BAR).or_else(|| rest.strip_prefix(GAP)) {
        rest = r;
        bars += 1;
    }
    if let Some(r) = rest.strip_prefix(TEE).or_else(|| rest.strip_prefix(ELL)) {
        Some((bars + 1, false, r))
    } else if bars == 0 {
        Some((0, false, rest))
    } else {
        Some((bars, true, rest))
    }
}

pub fn decode(wire: &str) -> Option<String> {
    let mut lines = wire.split('\n');
    let rest = lines.next()?.strip_prefix(HEADER)?;
    if rest != "0" && rest != "1" {
        return None;
    }
    let trailing = rest == "1";

    let mut depths = Vec::new();
    let mut bares = Vec::new();
    let mut contents = Vec::new();
    for line in lines {
        let indent = line.len() - line.trim_start_matches(' ').len();
        if indent % 2 != 0 {
            return None;
        }
        let after = &line[indent..];
        let (bare, depth, content) = if let Some(c) = after.strip_prefix("- ") {
            (false, indent / 2 + 1, c)
        } else if indent > 0 {
            (true, indent / 2, after)
        } else {
            (false, 0, after)
        };
        depths.push(depth);
        bares.push(bare);
        contents.push(content);
    }

    // A node is the last child when no later line returns to its depth before going shallower. A
    // section header (bars-only) at bar-count b ends the sibling group of depth-(b+1) nodes but is
    // invisible to any other depth.
    let n = depths.len();
    let mut is_last = vec![true; n];
    for i in 0..n {
        if bares[i] {
            continue;
        }
        let d = depths[i];
        for j in i + 1..n {
            if bares[j] {
                if depths[j] + 1 == d {
                    break;
                }
                continue;
            }
            if depths[j] < d {
                break;
            }
            if depths[j] == d {
                is_last[i] = false;
                break;
            }
        }
    }

    let mut out: Vec<String> = Vec::with_capacity(n);
    let mut stack: Vec<bool> = Vec::new();
    for i in 0..n {
        let d = depths[i];
        let mut line = String::new();
        if bares[i] {
            stack.truncate(d);
            for k in 0..d {
                line.push_str(if stack.get(k).copied().unwrap_or(true) {
                    GAP
                } else {
                    BAR
                });
            }
        } else if d > 0 {
            stack.truncate(d - 1);
            for k in 0..d - 1 {
                line.push_str(if stack.get(k).copied().unwrap_or(true) {
                    GAP
                } else {
                    BAR
                });
            }
            line.push_str(if is_last[i] { ELL } else { TEE });
            stack.push(is_last[i]);
        } else {
            stack.clear();
        }
        line.push_str(contents[i]);
        out.push(line);
    }
    let mut s = out.join("\n");
    if trailing {
        s.push('\n');
    }
    Some(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_a_box_drawing_tree() {
        let raw = "root v1\n\u{251c}\u{2500}\u{2500} a v2\n\u{2502}   \u{251c}\u{2500}\u{2500} c v4\n\u{2502}   \u{2514}\u{2500}\u{2500} d v5\n\u{2514}\u{2500}\u{2500} b v3\n";
        let wire = encode(raw).unwrap();
        assert!(
            wire.starts_with("SWTREE1") && wire.contains("- a v2") && !wire.contains('\u{251c}')
        );
        assert_eq!(decode(&wire).as_deref(), Some(raw));
    }

    #[test]
    fn round_trips_a_tree_with_a_section_header() {
        let raw = "r\n\u{251c}\u{2500}\u{2500} core v1\n\u{2502}   \u{2514}\u{2500}\u{2500} serde v1\n\u{2502}   [dev-dependencies]\n\u{2502}   \u{2514}\u{2500}\u{2500} serde_json v2\n\u{2514}\u{2500}\u{2500} b v3\n";
        let wire = encode(raw).unwrap();
        assert!(wire.contains("  [dev-dependencies]"));
        assert_eq!(decode(&wire).as_deref(), Some(raw));
    }

    #[test]
    fn declines_plain_text() {
        assert!(encode("just\nsome\nplain\ntext\n").is_none());
    }
}
