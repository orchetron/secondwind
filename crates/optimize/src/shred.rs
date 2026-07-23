use std::collections::BTreeSet;
use std::sync::Arc;

use serde_json::{Map, Value};

use crate::column::{
    choose_codec, codec_header, decode_cell, parse_codec, produces_cells, render_cells,
    scalar_token, string_token,
};
use crate::tokens::{ByteCounter, TokenCounter};
use crate::transform::{Encoded, Transform};

const HEADER: &str = "SWSHR";

// Recursive transpose for nested json arrays: an array of objects or arrays is shredded into
// per-path columns so every repeated key and structural byte is stored once, alongside presence
// and length streams that rebuild the exact nesting. Leaf columns reuse the columnar value codecs.
pub struct Shred {
    counter: Arc<dyn TokenCounter>,
}

impl Default for Shred {
    fn default() -> Self {
        Self {
            counter: Arc::new(ByteCounter),
        }
    }
}

impl Shred {
    pub fn with_counter(counter: Arc<dyn TokenCounter>) -> Self {
        Self { counter }
    }
}

impl Transform for Shred {
    fn id(&self) -> &'static str {
        "shred"
    }

    // Deterministic + exhaustively fuzzed (tests/fuzz.rs, 8000 nested cases): a lossless decode
    // implies an identical re-encode, so admission may skip the idempotence re-encode. CLMH + the
    // inverse witness still prove losslessness every block.
    fn trusted(&self) -> bool {
        true
    }

    fn try_encode(&self, value: &Value) -> Option<Encoded> {
        if !worth_shredding(value) {
            return None;
        }
        let mut wire = String::new();
        match value.as_array() {
            Some(items) if items.len() >= 2 => {
                let refs: Vec<&Value> = items.iter().collect();
                wire.push_str(&format!("{HEADER} A {}\n", items.len()));
                encode_node(&refs, self.counter.as_ref(), &mut wire)?;
            }
            _ => {
                wire.push_str(&format!("{HEADER} V\n"));
                encode_node(&[value], self.counter.as_ref(), &mut wire)?;
            }
        }
        let decoded = decode(&wire)?;
        Some(Encoded { wire, decoded })
    }
}

// A block is worth transposing only if it holds an array of two or more elements somewhere: that is
// the only place repeated keys collapse. A top-level object qualifies through its array fields.
fn worth_shredding(value: &Value) -> bool {
    match value {
        Value::Array(items) => items.len() >= 2 || items.iter().any(worth_shredding),
        Value::Object(map) => map.values().any(worth_shredding),
        _ => false,
    }
}

// A column of values is a struct (all objects), a list (all arrays), or a leaf (anything else,
// each value tokenized as json). Nulls in an otherwise-container column are lifted into a mask so
// the non-null remainder still transposes.
fn encode_node(vals: &[&Value], counter: &dyn TokenCounter, out: &mut String) -> Option<()> {
    if vals.is_empty() {
        out.push_str("E\n");
        return Some(());
    }
    let any_null = vals.iter().any(|v| v.is_null());
    let non_null: Vec<&Value> = vals.iter().copied().filter(|v| !v.is_null()).collect();

    if !non_null.is_empty() && non_null.iter().all(|v| v.is_object()) {
        if any_null {
            encode_nullmask(vals, counter, out)
        } else {
            encode_obj(vals, counter, out)
        }
    } else if !non_null.is_empty() && non_null.iter().all(|v| v.is_array()) {
        if any_null {
            encode_nullmask(vals, counter, out)
        } else {
            encode_arr(vals, counter, out)
        }
    } else {
        encode_leaf(vals, counter, out)
    }
}

fn encode_obj(vals: &[&Value], counter: &dyn TokenCounter, out: &mut String) -> Option<()> {
    let objs: Vec<&Map<String, Value>> = vals
        .iter()
        .map(|v| v.as_object())
        .collect::<Option<Vec<_>>>()?;
    let mut keyset: BTreeSet<&str> = BTreeSet::new();
    for o in &objs {
        for k in o.keys() {
            keyset.insert(k.as_str());
        }
    }
    let keys: Vec<&str> = keyset.into_iter().collect();

    out.push_str("O\n");
    out.push_str(&keys.len().to_string());
    out.push('\n');
    let mut presence: Vec<Vec<bool>> = Vec::with_capacity(keys.len());
    for key in &keys {
        let mask: Vec<bool> = objs.iter().map(|o| o.contains_key(*key)).collect();
        out.push_str(&string_token(key));
        out.push('\t');
        out.push_str(&encode_mask(&mask));
        out.push('\n');
        presence.push(mask);
    }
    for (ki, key) in keys.iter().enumerate() {
        let child: Vec<&Value> = objs
            .iter()
            .enumerate()
            .filter_map(|(r, o)| if presence[ki][r] { o.get(*key) } else { None })
            .collect();
        encode_node(&child, counter, out)?;
    }
    Some(())
}

fn encode_arr(vals: &[&Value], counter: &dyn TokenCounter, out: &mut String) -> Option<()> {
    let arrs: Vec<&Vec<Value>> = vals
        .iter()
        .map(|v| v.as_array())
        .collect::<Option<Vec<_>>>()?;
    out.push_str("A\n");
    let lens: Vec<String> = arrs.iter().map(|a| a.len().to_string()).collect();
    out.push_str(&lens.join(","));
    out.push('\n');
    let flat: Vec<&Value> = arrs.iter().flat_map(|a| a.iter()).collect();
    encode_node(&flat, counter, out)
}

fn encode_nullmask(vals: &[&Value], counter: &dyn TokenCounter, out: &mut String) -> Option<()> {
    let is_null: Vec<bool> = vals.iter().map(|v| v.is_null()).collect();
    out.push_str("N\n");
    out.push_str(&encode_mask(&is_null));
    out.push('\n');
    let non_null: Vec<&Value> = vals.iter().copied().filter(|v| !v.is_null()).collect();
    encode_node(&non_null, counter, out)
}

fn encode_leaf(vals: &[&Value], counter: &dyn TokenCounter, out: &mut String) -> Option<()> {
    let tokens: Vec<String> = vals
        .iter()
        .map(|v| scalar_token(v))
        .collect::<Option<Vec<_>>>()?;
    let is_string: Vec<bool> = vals.iter().map(|v| v.is_string()).collect();
    let codec = choose_codec(&tokens, &is_string, counter, false);
    out.push_str("L\n");
    out.push_str(&codec_header(&codec));
    out.push('\n');
    if let Some(cells) = render_cells(&codec, &tokens) {
        // A leading space merges into the next token under BPE, so a space delimiter is near-free; use
        // it when no cell contains a space (else tab), and record the choice as the line's first byte.
        let sep = if cells.iter().any(|c| c.contains(' ')) {
            "\t"
        } else {
            " "
        };
        out.push_str(sep);
        out.push_str(&cells.join(sep));
        out.push('\n');
    }
    Some(())
}

// `*` selects every index; otherwise `+`/`-` names the smaller side (present or absent).
fn encode_mask(selected: &[bool]) -> String {
    let n = selected.len();
    let set: Vec<usize> = (0..n).filter(|&i| selected[i]).collect();
    if set.len() == n {
        return "*".to_string();
    }
    let join = |idx: &[usize]| {
        idx.iter()
            .map(usize::to_string)
            .collect::<Vec<_>>()
            .join(",")
    };
    if set.len() <= n - set.len() {
        format!("+{}", join(&set))
    } else {
        let unset: Vec<usize> = (0..n).filter(|&i| !selected[i]).collect();
        format!("-{}", join(&unset))
    }
}

pub fn decode(wire: &str) -> Option<Value> {
    let mut lines = wire.split('\n');
    let mut header = lines.next()?.strip_prefix(HEADER)?.trim().split(' ');
    let value = match header.next()? {
        "A" => {
            let n: usize = header.next()?.parse().ok()?;
            if header.next().is_some() {
                return None;
            }
            Value::Array(decode_node(&mut lines, n)?)
        }
        "V" => {
            if header.next().is_some() {
                return None;
            }
            let mut items = decode_node(&mut lines, 1)?;
            if items.len() != 1 {
                return None;
            }
            items.pop()?
        }
        _ => return None,
    };
    if lines.next().is_some_and(|s| !s.is_empty()) {
        return None;
    }
    Some(value)
}

fn decode_node(lines: &mut std::str::Split<'_, char>, n: usize) -> Option<Vec<Value>> {
    match lines.next()? {
        "E" => Some(Vec::new()),
        "L" => decode_leaf(lines, n),
        "O" => decode_obj(lines, n),
        "A" => decode_arr(lines, n),
        "N" => decode_nullmask(lines, n),
        _ => None,
    }
}

fn decode_leaf(lines: &mut std::str::Split<'_, char>, n: usize) -> Option<Vec<Value>> {
    let codec = parse_codec(lines.next()?)?;
    if !produces_cells(&codec) {
        return (0..n)
            .map(|_| decode_cell(&codec, None))
            .collect::<Option<Vec<Value>>>();
    }
    let line = lines.next()?;
    if n == 0 {
        return if line.is_empty() {
            Some(Vec::new())
        } else {
            None
        };
    }
    let delim = line.chars().next()?;
    let cells: Vec<&str> = line[delim.len_utf8()..].split(delim).collect();
    if cells.len() != n {
        return None;
    }
    cells
        .into_iter()
        .map(|c| decode_cell(&codec, Some(c)))
        .collect()
}

fn decode_obj(lines: &mut std::str::Split<'_, char>, n: usize) -> Option<Vec<Value>> {
    let n_keys: usize = lines.next()?.parse().ok()?;
    let mut keys = Vec::with_capacity(n_keys);
    let mut masks = Vec::with_capacity(n_keys);
    for _ in 0..n_keys {
        let (kt, spec) = lines.next()?.split_once('\t')?;
        keys.push(serde_json::from_str::<String>(kt).ok()?);
        masks.push(decode_mask(spec, n)?);
    }
    let mut cols: Vec<Vec<Value>> = Vec::with_capacity(n_keys);
    for mask in &masks {
        let present = mask.iter().filter(|b| **b).count();
        cols.push(decode_node(lines, present)?);
    }
    let mut cursors = vec![0usize; n_keys];
    let mut out = Vec::with_capacity(n);
    // r walks rows while ki selects columns, so masks[ki][r] is a transposed read, not a range index.
    #[allow(clippy::needless_range_loop)]
    for r in 0..n {
        let mut obj = Map::new();
        for ki in 0..n_keys {
            if masks[ki][r] {
                obj.insert(keys[ki].clone(), cols[ki].get(cursors[ki])?.clone());
                cursors[ki] += 1;
            }
        }
        out.push(Value::Object(obj));
    }
    Some(out)
}

fn decode_arr(lines: &mut std::str::Split<'_, char>, n: usize) -> Option<Vec<Value>> {
    let line = lines.next()?;
    let lens: Vec<usize> = if line.is_empty() {
        Vec::new()
    } else {
        line.split(',')
            .map(|x| x.parse().ok())
            .collect::<Option<_>>()?
    };
    if lens.len() != n {
        return None;
    }
    let total: usize = lens.iter().sum();
    let mut flat = decode_node(lines, total)?.into_iter();
    let mut out = Vec::with_capacity(n);
    for len in lens {
        let mut arr = Vec::with_capacity(len);
        for _ in 0..len {
            arr.push(flat.next()?);
        }
        out.push(Value::Array(arr));
    }
    Some(out)
}

fn decode_nullmask(lines: &mut std::str::Split<'_, char>, n: usize) -> Option<Vec<Value>> {
    let nulls = decode_mask(lines.next()?, n)?;
    let present = nulls.iter().filter(|b| !**b).count();
    let mut child = decode_node(lines, present)?.into_iter();
    let mut out = Vec::with_capacity(n);
    for is_null in nulls {
        out.push(if is_null { Value::Null } else { child.next()? });
    }
    Some(out)
}

fn decode_mask(spec: &str, n: usize) -> Option<Vec<bool>> {
    if spec == "*" {
        return Some(vec![true; n]);
    }
    let rest = &spec[1..];
    let idxs: Vec<usize> = if rest.is_empty() {
        Vec::new()
    } else {
        rest.split(',')
            .map(|x| x.parse().ok())
            .collect::<Option<_>>()?
    };
    let (mut mask, set) = match spec.as_bytes().first()? {
        b'+' => (vec![false; n], true),
        b'-' => (vec![true; n], false),
        _ => return None,
    };
    for i in idxs {
        if i >= n {
            return None;
        }
        mask[i] = set;
    }
    Some(mask)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn round_trip(value: &Value) {
        let encoded = Shred::default()
            .try_encode(value)
            .unwrap_or_else(|| panic!("no encode for {value}"));
        assert_eq!(
            decode(&encoded.wire).unwrap(),
            *value,
            "wire:\n{}",
            encoded.wire
        );
    }

    #[test]
    fn nested_object_column() {
        round_trip(&json!([
            {"id": 1, "user": {"login": "a", "id": 10}},
            {"id": 2, "user": {"login": "b", "id": 20}}
        ]));
    }

    #[test]
    fn variable_length_arrays() {
        round_trip(&json!([
            {"n": 1, "labels": ["bug", "p1"]},
            {"n": 2, "labels": []},
            {"n": 3, "labels": ["docs"]}
        ]));
    }

    #[test]
    fn nullable_object_field() {
        round_trip(&json!([
            {"id": 1, "assignee": {"login": "a"}},
            {"id": 2, "assignee": null},
            {"id": 3, "assignee": {"login": "c"}}
        ]));
    }

    #[test]
    fn nullable_array_field() {
        round_trip(&json!([
            {"tags": ["x"]},
            {"tags": null},
            {"tags": ["y", "z"]}
        ]));
    }

    #[test]
    fn heterogeneous_keys_use_presence() {
        round_trip(&json!([
            {"a": 1, "b": 2},
            {"a": 3},
            {"b": 4, "c": 5}
        ]));
    }

    #[test]
    fn mixed_types_at_a_path_fall_to_leaf() {
        round_trip(&json!([
            {"v": 1},
            {"v": "two"},
            {"v": {"k": 3}},
            {"v": [4]}
        ]));
    }

    #[test]
    fn arrays_of_objects_nested() {
        round_trip(&json!([
            {"commit": {"author": {"name": "a"}, "message": "x"}, "parents": [{"sha": "1"}]},
            {"commit": {"author": {"name": "b"}, "message": "y"}, "parents": [{"sha": "2"}, {"sha": "3"}]}
        ]));
    }

    #[test]
    fn deep_nesting_and_scalars() {
        round_trip(&json!([
            {"a": {"b": {"c": {"d": [1, 2, 3]}}}, "e": true, "f": null, "n": 1.50},
            {"a": {"b": {"c": {"d": []}}}, "e": false, "f": "here", "n": 2}
        ]));
    }

    #[test]
    fn scalar_array_becomes_a_leaf() {
        round_trip(&json!([1, 2, 2, 3, "x", "x", null, true]));
    }

    #[test]
    fn empty_objects_and_arrays() {
        round_trip(&json!([{}, {}, {"a": []}]));
    }

    #[test]
    fn strings_with_delimiters_survive() {
        round_trip(&json!([
            {"s": "a\tb\nc", "t": "plain"},
            {"s": "d,e", "t": "x\ty"}
        ]));
    }

    #[test]
    fn top_level_object_shreds_its_inner_arrays() {
        round_trip(&json!({
            "metadata": {"version": 1, "ok": true},
            "packages": [
                {"name": "a", "deps": [{"name": "x"}, {"name": "y"}]},
                {"name": "b", "deps": []},
                {"name": "c", "deps": [{"name": "z"}]}
            ]
        }));
    }

    #[test]
    fn refuses_blocks_with_no_repeated_array() {
        assert!(Shred::default().try_encode(&json!([{"a": 1}])).is_none());
        assert!(Shred::default().try_encode(&json!({"a": 1})).is_none());
        assert!(Shred::default().try_encode(&json!(42)).is_none());
        assert!(Shred::default().try_encode(&json!({"one": [1]})).is_none());
    }
}
