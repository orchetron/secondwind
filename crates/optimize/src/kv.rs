// Columnar key=value output (env, .env, ini dumps): each line splits at the first '=' into a key and a
// value column, each picking its own codec; non-pair lines stay verbatim. Self-gated, so it ships only
// when keys or values share real structure (a raw process environment usually does not).

use crate::text_columnar::{cheapest, decode_value_column, generic_candidates};

const HEADER: &str = "SWKV";
const MIN_PAIRS: usize = 4;

pub struct Encoded {
    pub wire: String,
    pub decoded: String,
}

pub fn try_encode(raw: &str, cost: &dyn Fn(&str) -> usize) -> Option<Encoded> {
    let mut types: Vec<u8> = Vec::new();
    let mut keys: Vec<&str> = Vec::new();
    let mut values: Vec<&str> = Vec::new();
    let mut verbatims: Vec<&str> = Vec::new();

    for line in raw.split('\n') {
        match line.split_once('=') {
            Some((k, v)) => {
                types.push(b'K');
                keys.push(k);
                values.push(v);
            }
            None => {
                types.push(b'V');
                verbatims.push(line);
            }
        }
    }
    if keys.len() < MIN_PAIRS {
        return None;
    }

    let mut wire = format!("{HEADER}\t{}\t{}\n", types.len(), keys.len());
    wire.push_str(&encode_types(&types));
    wire.push('\n');
    for v in &verbatims {
        wire.push_str(v);
        wire.push('\n');
    }
    wire.push_str(&cheapest(generic_candidates(&keys), cost));
    wire.push_str(&cheapest(generic_candidates(&values), cost));

    if cost(&wire) >= cost(raw) {
        return None;
    }
    let decoded = decode(&wire)?;
    if decoded != raw {
        return None;
    }
    Some(Encoded { wire, decoded })
}

pub fn decode(wire: &str) -> Option<String> {
    let mut lines = wire.split('\n');
    let mut head = lines.next()?.split('\t');
    if head.next()? != HEADER {
        return None;
    }
    let n_rows: usize = head.next()?.parse().ok()?;
    let n_kv: usize = head.next()?.parse().ok()?;

    let types = decode_types(lines.next()?, n_rows)?;
    let n_v = types.iter().filter(|&&t| t == b'V').count();
    let mut verbatims = Vec::with_capacity(n_v);
    for _ in 0..n_v {
        verbatims.push(lines.next()?);
    }

    let tag = lines.next()?;
    let keys = decode_value_column(tag, &mut lines, n_kv)?;
    let tag = lines.next()?;
    let values = decode_value_column(tag, &mut lines, n_kv)?;

    let (mut ki, mut vi) = (0usize, 0usize);
    let mut out = Vec::with_capacity(n_rows);
    for &t in &types {
        if t == b'K' {
            out.push(format!("{}={}", keys.get(ki)?, values.get(ki)?));
            ki += 1;
        } else {
            out.push(verbatims.get(vi)?.to_string());
            vi += 1;
        }
    }
    Some(out.join("\n"))
}

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
            'K' => b'K',
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

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes(s: &str) -> usize {
        s.len()
    }

    #[test]
    fn compresses_shared_key_prefixes_and_repeated_values() {
        let raw = "npm_config_cache=/x\nnpm_config_prefix=/x\nnpm_config_registry=/x\nnpm_config_loglevel=/x\nHOME=/root";
        let out = try_encode(raw, &bytes).expect("shared prefixes compress");
        assert!(out.wire.len() < raw.len());
        assert_eq!(decode(&out.wire).as_deref(), Some(raw));
    }

    #[test]
    fn preserves_equals_inside_values_and_non_kv_lines() {
        let raw = "A=x=y=z\nB=1\nnot a pair\nC=2\nD=3";
        if let Some(out) = try_encode(raw, &bytes) {
            assert_eq!(decode(&out.wire).as_deref(), Some(raw));
        }
    }

    #[test]
    fn abstains_when_keys_and_values_are_all_unique() {
        let raw = "AAAA=1111\nBBBB=2222\nCCCC=3333\nDDDD=4444\nEEEE=5555";
        assert!(try_encode(raw, &bytes).is_none());
    }
}
