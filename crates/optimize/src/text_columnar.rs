// Lossless columnar compression of whitespace-aligned tabular text (ls, ps, docker ps, git status):
// columns store newline-delimited, repeated columns collapse to a const/dict, padding becomes a
// regeneration rule. Codecs scored on the caller's token-cost function, not bytes.

use std::collections::HashMap;

const HEADER: &str = "SWTC";
const MIN_BYTES: usize = 256;
const MIN_ROWS: usize = 4;
const MIN_COLS: usize = 2;

pub struct Encoded {
    pub wire: String,
    pub decoded: String,
}

// Encodes when tabular and strictly token-cheaper per `cost`; self-verifies byte-exact and abstains
// (None) on any mismatch. `cost` returns a string's token count (pass |s| s.len() for a byte proxy).
// Races a fixed-column encoding against tail encodings (free-text last column) and keeps the cheapest.
pub fn try_encode(raw: &str, cost: &dyn Fn(&str) -> usize) -> Option<Encoded> {
    if raw.len() < MIN_BYTES {
        return None;
    }
    let lines: Vec<&str> = raw.split('\n').collect();
    let parsed: Vec<(Vec<String>, Vec<String>)> = lines.iter().map(|l| tokenize(l)).collect();

    let mut best: Option<Encoded> = None;
    if let Some(n) = dominant_token_count(&parsed) {
        consider(&mut best, encode_fixed(&lines, &parsed, n, cost), raw, cost);
    }
    for f in tail_column_counts(&parsed) {
        consider(&mut best, encode_tail(&lines, &parsed, f, cost), raw, cost);
    }
    best
}

fn consider(
    best: &mut Option<Encoded>,
    candidate: Option<Encoded>,
    raw: &str,
    cost: &dyn Fn(&str) -> usize,
) {
    let Some(candidate) = candidate else {
        return;
    };
    if cost(&candidate.wire) >= cost(raw) || candidate.decoded != raw {
        return;
    }
    if best
        .as_ref()
        .is_none_or(|b| cost(&candidate.wire) < cost(&b.wire))
    {
        *best = Some(candidate);
    }
}

fn encode_fixed(
    lines: &[&str],
    parsed: &[(Vec<String>, Vec<String>)],
    n: usize,
    cost: &dyn Fn(&str) -> usize,
) -> Option<Encoded> {
    if n < MIN_COLS {
        return None;
    }
    let is_table: Vec<bool> = parsed.iter().map(|(_, t)| t.len() == n).collect();
    let rows: Vec<usize> = (0..lines.len()).filter(|&i| is_table[i]).collect();
    if rows.len() < MIN_ROWS {
        return None;
    }
    let token_cols: Vec<Vec<&str>> = (0..n)
        .map(|c| rows.iter().map(|&r| parsed[r].1[c].as_str()).collect())
        .collect();
    let gap_cols: Vec<Vec<&str>> = (0..=n)
        .map(|c| rows.iter().map(|&r| parsed[r].0[c].as_str()).collect())
        .collect();

    let mut wire = format!("{HEADER}\t{}\t{}\t{}\n", lines.len(), n, rows.len());
    emit_body(
        &mut wire,
        lines,
        &is_table,
        &token_cols,
        &gap_cols,
        None,
        cost,
    );
    let decoded = decode(&wire)?;
    Some(Encoded { wire, decoded })
}

// Fixed leading columns plus a free-text tail: any line with at least `f` tokens is a row whose last
// column is the exact remainder of the line after token f-1, so a ragged table (ps, docker) transposes.
fn encode_tail(
    lines: &[&str],
    parsed: &[(Vec<String>, Vec<String>)],
    f: usize,
    cost: &dyn Fn(&str) -> usize,
) -> Option<Encoded> {
    if f < MIN_COLS {
        return None;
    }
    let is_table: Vec<bool> = parsed.iter().map(|(_, t)| t.len() >= f).collect();
    let rows: Vec<usize> = (0..lines.len()).filter(|&i| is_table[i]).collect();
    if rows.len() < MIN_ROWS {
        return None;
    }
    let token_cols: Vec<Vec<&str>> = (0..f)
        .map(|c| rows.iter().map(|&r| parsed[r].1[c].as_str()).collect())
        .collect();
    let gap_cols: Vec<Vec<&str>> = (0..f)
        .map(|c| rows.iter().map(|&r| parsed[r].0[c].as_str()).collect())
        .collect();
    let tail: Vec<&str> = rows
        .iter()
        .map(|&r| {
            let (gaps, tokens) = &parsed[r];
            let prefix: usize = (0..f).map(|c| gaps[c].len() + tokens[c].len()).sum();
            &lines[r][prefix..]
        })
        .collect();

    let mut wire = format!("{HEADER}\t{}\t{}\t{}\t1\n", lines.len(), f, rows.len());
    emit_body(
        &mut wire,
        lines,
        &is_table,
        &token_cols,
        &gap_cols,
        Some(&tail),
        cost,
    );
    let decoded = decode(&wire)?;
    Some(Encoded { wire, decoded })
}

fn emit_body(
    wire: &mut String,
    lines: &[&str],
    is_table: &[bool],
    token_cols: &[Vec<&str>],
    gap_cols: &[Vec<&str>],
    tail: Option<&Vec<&str>>,
    cost: &dyn Fn(&str) -> usize,
) {
    wire.push_str(&run_length_line_map(is_table));
    wire.push('\n');
    for (i, line) in lines.iter().enumerate() {
        if !is_table[i] {
            wire.push_str(line);
            wire.push('\n');
        }
    }
    for col in token_cols {
        wire.push_str(&cheapest(generic_candidates(col), cost));
    }
    for (c, gap) in gap_cols.iter().enumerate() {
        let mut candidates = generic_candidates(gap);
        if c < token_cols.len()
            && let Some(rule) = align_candidate(gap, &token_cols[c], '<')
        {
            candidates.push(rule);
        }
        if c >= 1
            && let Some(rule) = align_candidate(gap, &token_cols[c - 1], '>')
        {
            candidates.push(rule);
        }
        wire.push_str(&cheapest(candidates, cost));
    }
    if let Some(tail) = tail {
        wire.push_str(&cheapest(generic_candidates(tail), cost));
    }
}

// Fixed-column counts to try for a tail split: the shared floor (min tokens) and the dominant count,
// each also one lower so a column that is always present but sometimes free-text moves into the tail.
fn tail_column_counts(parsed: &[(Vec<String>, Vec<String>)]) -> Vec<usize> {
    let counts: Vec<usize> = parsed
        .iter()
        .map(|(_, t)| t.len())
        .filter(|&c| c >= MIN_COLS)
        .collect();
    let Some(&min) = counts.iter().min() else {
        return Vec::new();
    };
    let dom = dominant_token_count(parsed).unwrap_or(min);
    let mut cands = vec![min, min.saturating_sub(1), dom, dom.saturating_sub(1)];
    cands.retain(|&f| f >= MIN_COLS);
    cands.sort_unstable();
    cands.dedup();
    cands
}

// Splits a line into gaps (whitespace runs) and tokens; invariant gaps.len() == tokens.len()+1 and
// gap[0]+tok[0]+...+tok[n-1]+gap[n] == line.
fn tokenize(line: &str) -> (Vec<String>, Vec<String>) {
    let mut gaps = Vec::new();
    let mut tokens = Vec::new();
    let mut cur = String::new();
    let mut in_ws = true;
    for ch in line.chars() {
        let is_ws = ch == ' ' || ch == '\t';
        if is_ws != in_ws {
            if in_ws {
                gaps.push(std::mem::take(&mut cur));
            } else {
                tokens.push(std::mem::take(&mut cur));
            }
            in_ws = is_ws;
        }
        cur.push(ch);
    }
    if in_ws {
        gaps.push(cur);
    } else {
        tokens.push(cur);
        gaps.push(String::new());
    }
    (gaps, tokens)
}

fn dominant_token_count(parsed: &[(Vec<String>, Vec<String>)]) -> Option<usize> {
    let mut counts: HashMap<usize, usize> = HashMap::new();
    for (_, tokens) in parsed {
        if tokens.len() >= MIN_COLS {
            *counts.entry(tokens.len()).or_default() += 1;
        }
    }
    counts
        .into_iter()
        .max_by_key(|&(_, freq)| freq)
        .map(|(n, _)| n)
}

// Alternating T (table row) / E (exception) runs as <type><count> pairs, e.g. "E1T923".
fn run_length_line_map(is_table: &[bool]) -> String {
    let mut out = String::new();
    let mut i = 0;
    while i < is_table.len() {
        let table = is_table[i];
        let mut j = i;
        while j < is_table.len() && is_table[j] == table {
            j += 1;
        }
        out.push(if table { 'T' } else { 'E' });
        out.push_str(&(j - i).to_string());
        i = j;
    }
    out
}

fn parse_line_map(s: &str, total: usize) -> Option<Vec<bool>> {
    let mut out = Vec::with_capacity(total);
    let mut chars = s.chars().peekable();
    while let Some(t) = chars.next() {
        let is_table = match t {
            'T' => true,
            'E' => false,
            _ => return None,
        };
        let mut num = String::new();
        while chars.peek().is_some_and(char::is_ascii_digit) {
            num.push(chars.next()?);
        }
        let count: usize = num.parse().ok()?;
        out.resize(out.len() + count, is_table);
    }
    (out.len() == total).then_some(out)
}

pub(crate) fn cheapest(candidates: Vec<String>, cost: &dyn Fn(&str) -> usize) -> String {
    candidates
        .into_iter()
        .min_by_key(|c| cost(c))
        .expect("raw is always a candidate")
}

// Per-column value codecs: `C` const, `D` dictionary (uniques + indices), `F` front coding (shared
// prefix len + suffix, crushes sorted columns), `R` raw. Values are newline-free, so all parse positionally.
pub(crate) fn generic_candidates(values: &[&str]) -> Vec<String> {
    let mut candidates: Vec<String> = Vec::with_capacity(4);

    if values.iter().all(|v| Some(v) == values.first()) {
        candidates.push(
            values
                .first()
                .map(|v| format!("C\t{v}\n"))
                .unwrap_or_else(|| "C\t\n".into()),
        );
    }

    let mut uniques: Vec<&str> = Vec::new();
    let mut index_of: HashMap<&str, usize> = HashMap::new();
    let mut indices: Vec<usize> = Vec::with_capacity(values.len());
    for &v in values {
        let idx = *index_of.entry(v).or_insert_with(|| {
            uniques.push(v);
            uniques.len() - 1
        });
        indices.push(idx);
    }
    if uniques.len() < values.len() {
        let mut dict = format!("D\t{}\n", uniques.len());
        for u in &uniques {
            dict.push_str(u);
            dict.push('\n');
        }
        dict.push_str(
            &indices
                .iter()
                .map(usize::to_string)
                .collect::<Vec<_>>()
                .join(" "),
        );
        dict.push('\n');
        candidates.push(dict);
    }

    let mut front = String::from("F\n");
    let mut prev = "";
    for &v in values {
        let shared = shared_prefix_chars(prev, v);
        front.push_str(&shared.to_string());
        front.push(' ');
        front.push_str(&v[byte_offset(v, shared)..]);
        front.push('\n');
        prev = v;
    }
    candidates.push(front);

    let mut raw = String::from("R\n");
    for &v in values {
        raw.push_str(v);
        raw.push('\n');
    }
    candidates.push(raw);

    candidates
}

// A `P` alignment rule for a gap column that is pure constant-width padding (all spaces, len(gap)+
// chars(neighbour) equal every row). dir '<' left-pads the following token, '>' right-pads the
// preceding one; decode regenerates the exact spaces, so per-row padding costs zero tokens.
fn align_candidate(gaps: &[&str], neighbour: &[&str], dir: char) -> Option<String> {
    let mut width: Option<usize> = None;
    for (g, t) in gaps.iter().zip(neighbour) {
        if !g.bytes().all(|b| b == b' ') {
            return None;
        }
        let w = g.chars().count() + t.chars().count();
        match width {
            None => width = Some(w),
            Some(x) if x == w => {}
            _ => return None,
        }
    }
    width.map(|w| format!("P\t{dir}\t{w}\n"))
}

pub(crate) fn shared_prefix_chars(a: &str, b: &str) -> usize {
    a.chars().zip(b.chars()).take_while(|(x, y)| x == y).count()
}

pub(crate) fn byte_offset(s: &str, chars: usize) -> usize {
    s.char_indices()
        .nth(chars)
        .map(|(i, _)| i)
        .unwrap_or(s.len())
}

pub fn decode(wire: &str) -> Option<String> {
    let mut lines = wire.split('\n');
    let head = lines.next()?;
    let mut parts = head.split('\t');
    if parts.next()? != HEADER {
        return None;
    }
    let total: usize = parts.next()?.parse().ok()?;
    let n: usize = parts.next()?.parse().ok()?;
    let n_rows: usize = parts.next()?.parse().ok()?;
    let tail_mode = parts.next() == Some("1");

    let types = parse_line_map(lines.next()?, total)?;

    let n_exceptions = types.iter().filter(|&&t| !t).count();
    let mut exceptions = Vec::with_capacity(n_exceptions);
    for _ in 0..n_exceptions {
        exceptions.push(lines.next()?.to_string());
    }

    let mut token_cols: Vec<Vec<String>> = Vec::with_capacity(n);
    for _ in 0..n {
        let tag = lines.next()?;
        token_cols.push(decode_value_column(tag, &mut lines, n_rows)?);
    }
    let n_gaps = if tail_mode { n } else { n + 1 };
    let mut gap_cols: Vec<Vec<String>> = Vec::with_capacity(n_gaps);
    for c in 0..n_gaps {
        let tag = lines.next()?;
        if let Some(rest) = tag.strip_prefix("P\t") {
            let mut r = rest.split('\t');
            let dir = r.next()?;
            let w: usize = r.next()?.parse().ok()?;
            let neighbour = match dir {
                "<" => token_cols.get(c)?,
                ">" => token_cols.get(c.checked_sub(1)?)?,
                _ => return None,
            };
            gap_cols.push(
                neighbour
                    .iter()
                    .map(|t| " ".repeat(w.saturating_sub(t.chars().count())))
                    .collect(),
            );
        } else {
            gap_cols.push(decode_value_column(tag, &mut lines, n_rows)?);
        }
    }
    let tail_col = if tail_mode {
        Some(decode_value_column(lines.next()?, &mut lines, n_rows)?)
    } else {
        None
    };

    let mut out: Vec<String> = Vec::with_capacity(total);
    let (mut row, mut exc) = (0usize, 0usize);
    for &is_table in &types {
        if is_table {
            if row >= n_rows {
                return None;
            }
            let mut line = String::new();
            for col in 0..n {
                line.push_str(gap_cols[col].get(row)?);
                line.push_str(token_cols[col].get(row)?);
            }
            match &tail_col {
                Some(tail) => line.push_str(tail.get(row)?),
                None => line.push_str(gap_cols[n].get(row)?),
            }
            out.push(line);
            row += 1;
        } else {
            out.push(exceptions.get(exc)?.clone());
            exc += 1;
        }
    }
    Some(out.join("\n"))
}

pub(crate) fn decode_value_column<'a>(
    tag: &str,
    lines: &mut impl Iterator<Item = &'a str>,
    n_rows: usize,
) -> Option<Vec<String>> {
    if let Some(value) = tag.strip_prefix("C\t") {
        Some(vec![value.to_string(); n_rows])
    } else if let Some(count) = tag.strip_prefix("D\t") {
        let k: usize = count.parse().ok()?;
        let mut uniques = Vec::with_capacity(k);
        for _ in 0..k {
            uniques.push(lines.next()?.to_string());
        }
        let index_line = lines.next()?;
        let mut out = Vec::with_capacity(n_rows);
        for tok in index_line.split(' ').filter(|s| !s.is_empty()) {
            out.push(uniques.get(tok.parse::<usize>().ok()?)?.clone());
        }
        (out.len() == n_rows).then_some(out)
    } else if tag == "F" {
        let mut out = Vec::with_capacity(n_rows);
        let mut prev = String::new();
        for _ in 0..n_rows {
            let line = lines.next()?;
            let space = line.find(' ')?;
            let shared: usize = line[..space].parse().ok()?;
            let value: String = prev
                .chars()
                .take(shared)
                .chain(line[space + 1..].chars())
                .collect();
            out.push(value.clone());
            prev = value;
        }
        Some(out)
    } else if tag == "R" {
        let mut out = Vec::with_capacity(n_rows);
        for _ in 0..n_rows {
            out.push(lines.next()?.to_string());
        }
        Some(out)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes(s: &str) -> usize {
        s.len()
    }

    fn roundtrips(raw: &str) -> bool {
        match try_encode(raw, &bytes) {
            Some(enc) => enc.decoded == raw && decode(&enc.wire).as_deref() == Some(raw),
            None => true,
        }
    }

    fn ls_la(files: &[&str]) -> String {
        let mut out = String::from("total 171824\n");
        for (i, f) in files.iter().enumerate() {
            out.push_str(&format!(
                "-rwxr-xr-x  1 root  wheel  {:>7} Jan  1 12:00 {f}\n",
                100 + i * 37
            ));
        }
        out.trim_end().to_string()
    }

    #[test]
    fn compresses_ls_la_losslessly() {
        let files: Vec<String> = (0..200).map(|i| format!("binary-{i}")).collect();
        let raw = ls_la(&files.iter().map(String::as_str).collect::<Vec<_>>());
        let enc = try_encode(&raw, &bytes).expect("tabular ls output should compress");
        assert_eq!(enc.decoded, raw, "byte-exact");
        assert!(
            enc.wire.len() < raw.len(),
            "wire {} < raw {}",
            enc.wire.len(),
            raw.len()
        );
    }

    #[test]
    fn alignment_padding_is_regenerated_exactly() {
        // Interior right-aligned size column (real ls -la): must round-trip byte-exact and elect the P rule.
        let mut raw = String::new();
        for i in 0..30 {
            raw.push_str(&format!("-rw-r--r-- {:>7} item-{i}\n", 10i64.pow(i % 7)));
        }
        let raw = raw.trim_end();
        let enc = try_encode(raw, &bytes).expect("aligned table should compress");
        assert_eq!(enc.decoded, raw, "byte-exact incl. exact padding");
        assert!(enc.wire.contains("P\t"), "should elect an alignment rule");
    }

    #[test]
    fn preserves_headers_blanks_and_exact_spacing() {
        let raw = "NAME       STATUS   AGE\npod-a      Running  3d\n\npod-b      Pending  1h\n  indented weird line\npod-c      Running  9d";
        assert!(roundtrips(raw));
    }

    #[test]
    fn abstains_on_non_tabular() {
        assert!(
            try_encode(
                "just a sentence of prose without any columnar structure at all here",
                &bytes
            )
            .is_none()
        );
        assert!(try_encode("", &bytes).is_none());
        assert!(try_encode("a\nb\nc", &bytes).is_none());
    }

    #[test]
    fn tabs_and_trailing_whitespace_survive() {
        let raw = "a\tb\tc   \nd\te\tf   \ng\th\ti   \nj\tk\tl   ";
        assert!(roundtrips(raw));
    }

    #[test]
    fn run_length_line_map_survives_scattered_exceptions() {
        let mut raw = String::from("HEADER LINE HERE\n");
        for i in 0..20 {
            raw.push_str(&format!("col-a col-b col-{i}\n"));
            if i % 7 == 3 {
                raw.push_str("a stray non-conforming line with many many extra tokens here now\n");
            }
        }
        assert!(roundtrips(raw.trim_end()));
    }

    // SW_RATIO_FILE=/path cargo test -p secondwind-optimize --features tiktoken measure_ratio -- --nocapture
    #[test]
    fn measure_ratio() {
        let Some(path) = std::env::var_os("SW_RATIO_FILE") else {
            return;
        };
        let raw = std::fs::read_to_string(path).unwrap();
        #[cfg(feature = "tiktoken")]
        {
            use crate::tokens::TokenCounter;
            let counter = crate::tokens::Tiktoken::cl100k();
            let cost = |s: &str| counter.count(s);
            match try_encode(&raw, &cost) {
                Some(e) => {
                    assert_eq!(e.decoded, raw, "must be byte-exact");
                    let bpct = 100.0 * (raw.len() - e.wire.len()) as f64 / raw.len() as f64;
                    let (rt, wt) = (cost(&raw), cost(&e.wire));
                    let tpct = 100.0 * (rt - wt) as f64 / rt as f64;
                    eprintln!(
                        "\nRATIO bytes: {} -> {} = {bpct:.1}% | tokens: {rt} -> {wt} = {tpct:.1}% smaller, LOSSLESS\n",
                        raw.len(),
                        e.wire.len()
                    );
                }
                None => eprintln!("\nRATIO: abstained\n"),
            }
        }
        #[cfg(not(feature = "tiktoken"))]
        {
            let _ = try_encode(&raw, &bytes);
        }
    }

    #[test]
    fn fuzz_encode_is_lossless_or_abstains() {
        // Adversarial tables. Invariant: try_encode either round-trips byte-exact or abstains, never
        // corrupts silently.
        let alphabets = ["abc", "a b", "  ", "\t", "café🚀", "0123456789", "root", ""];
        let mut seed = 0x9e3779b9u32;
        let mut rng = || {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            seed
        };
        for _ in 0..8000 {
            let rows = (rng() % 12) as usize;
            let width = 1 + (rng() % 5) as usize;
            let pad = (rng() % 9) as usize;
            let mut raw = String::new();
            for _ in 0..rows {
                if rng() % 9 == 0 {
                    raw.push_str(
                        "an exceptional line with a wholly different token shape entirely",
                    );
                } else {
                    for c in 0..width {
                        if c > 0 {
                            raw.push_str(&" ".repeat(1 + (rng() as usize % (pad + 1))));
                        }
                        let a = alphabets[rng() as usize % alphabets.len()];
                        raw.push_str(a);
                        if rng() % 4 == 0 {
                            raw.push_str(&" ".repeat(pad));
                        }
                    }
                }
                raw.push('\n');
            }
            let raw = raw.trim_end_matches('\n');
            if let Some(enc) = try_encode(raw, &bytes) {
                assert_eq!(enc.decoded, raw, "internal decode must equal raw");
                assert_eq!(
                    decode(&enc.wire).as_deref(),
                    Some(raw),
                    "wire must decode to raw"
                );
            }
        }
    }

    #[test]
    fn ragged_table_with_free_text_tail_transposes() {
        let mut raw = String::from("USER          PID STAT COMMAND\n");
        for i in 0..60 {
            let cmd = match i % 3 {
                0 => "claude".to_string(),
                1 => "/usr/bin/some daemon --flag".to_string(),
                _ => "/Applications/App.app/Contents/MacOS/App --a --b --c".to_string(),
            };
            raw.push_str(&format!("user{:<3} {:>8}  S   {cmd}\n", i % 9, 100 + i * 7));
        }
        let raw = raw.trim_end();
        let enc = try_encode(raw, &bytes).expect("ragged table should compress");
        assert_eq!(enc.decoded, raw, "byte-exact");
        assert!(
            enc.wire.lines().next().unwrap().ends_with("\t1"),
            "expected tail mode, header: {}",
            enc.wire.lines().next().unwrap()
        );
        assert!(enc.wire.len() < raw.len());
    }

    #[test]
    fn fuzz_ragged_tail_is_lossless_or_abstains() {
        let alphabets = ["abc", "a b", "root", "12345", "caf\u{00e9}\u{1f680}", ""];
        let mut seed = 0x1234_5678u32;
        let mut rng = || {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            seed
        };
        for _ in 0..8000 {
            let rows = (rng() % 14) as usize;
            let fixed = 1 + (rng() % 4) as usize;
            let mut raw = String::new();
            for _ in 0..rows {
                for c in 0..fixed {
                    if c > 0 {
                        raw.push_str(&" ".repeat(1 + (rng() as usize % 4)));
                    }
                    raw.push_str(alphabets[rng() as usize % alphabets.len()]);
                }
                for _ in 0..(rng() % 5) {
                    raw.push(' ');
                    raw.push_str(alphabets[rng() as usize % alphabets.len()]);
                }
                raw.push('\n');
            }
            let raw = raw.trim_end_matches('\n');
            if let Some(enc) = try_encode(raw, &bytes) {
                assert_eq!(enc.decoded, raw, "internal decode must equal raw");
                assert_eq!(
                    decode(&enc.wire).as_deref(),
                    Some(raw),
                    "wire must decode to raw"
                );
            }
        }
    }

    #[test]
    fn tokenize_reconstructs_every_line() {
        for line in [
            "a b c",
            "  leading",
            "trailing  ",
            "a\tb",
            "",
            "   ",
            "x",
            "-rw-r--r-- 1 root wheel 12 f",
        ] {
            let (gaps, tokens) = tokenize(line);
            assert_eq!(gaps.len(), tokens.len() + 1);
            let mut rebuilt = String::new();
            for i in 0..tokens.len() {
                rebuilt.push_str(&gaps[i]);
                rebuilt.push_str(&tokens[i]);
            }
            rebuilt.push_str(&gaps[tokens.len()]);
            assert_eq!(rebuilt, line);
        }
    }
}
