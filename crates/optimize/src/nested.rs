use serde_json::{Map, Value};
use std::sync::Arc;

use crate::column::string_token;
use crate::tokens::{ByteCounter, TokenCounter};
use crate::transform::{Encoded, Transform};

const HEADER: &str = "SWNEST";
const SEP: char = '.';

// Row-major readable table for nested/ragged record arrays: flatten scalar paths to dotted columns,
// keep every value literal so the model reads it, union schema with an empty cell for an absent field.
// A column that is present and identical in every record is stated once as a constant, not per row.
pub struct Nested {
    // Nested currently has one readable wire shape, but keeping the configured counter with it
    // makes callers compose it consistently with the other structured codecs.
    _counter: Arc<dyn TokenCounter>,
}

impl Default for Nested {
    fn default() -> Self {
        Self {
            _counter: Arc::new(ByteCounter),
        }
    }
}

impl Nested {
    pub fn with_counter(counter: Arc<dyn TokenCounter>) -> Self {
        Self { _counter: counter }
    }
}

impl Transform for Nested {
    fn id(&self) -> &'static str {
        "nested"
    }

    // Constant-hoisting reorders keys on decode, so the wire is not a byte-identical fixed point; its
    // per-block losslessness is proven by the self-verify below plus admission's CLMH + inverse witness.
    fn trusted(&self) -> bool {
        true
    }

    fn try_encode(&self, value: &Value) -> Option<Encoded> {
        let items = value.as_array()?;
        if items.len() < 2 {
            return None;
        }
        let empty = std::collections::HashSet::new();
        let flats1: Vec<Map<String, Value>> = items
            .iter()
            .map(|it| it.as_object().and_then(|o| flatten(o, &empty)))
            .collect::<Option<Vec<_>>>()?;
        // A sub-object whose keys vary per record explodes the union into mostly-empty columns; keep
        // those fields as json cells so the dense columns still tablify instead of the wire blowing up.
        let stop = sparse_prefixes(&flats1);
        let flats: Vec<Map<String, Value>> = if stop.is_empty() {
            flats1
        } else {
            items
                .iter()
                .map(|it| it.as_object().and_then(|o| flatten(o, &stop)))
                .collect::<Option<Vec<_>>>()?
        };

        let mut order: Vec<String> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for f in &flats {
            for k in f.keys() {
                if seen.insert(k.clone()) {
                    order.push(k.clone());
                }
            }
        }
        if order.is_empty() {
            return None;
        }

        let (consts, vary): (Vec<String>, Vec<String>) = order.into_iter().partition(|k| {
            flats[0].contains_key(k) && flats.iter().all(|f| f.get(k) == flats[0].get(k))
        });

        let types: Vec<char> = vary
            .iter()
            .map(|k| {
                let raw_safe = flats.iter().all(|f| {
                    f.get(k)
                        .is_some_and(|v| v.as_str().is_some_and(|s| !s.contains(['\t', '\n'])))
                });
                if raw_safe { 's' } else { 'j' }
            })
            .collect();

        let mut wire = format!("{HEADER} {} {} {}", flats.len(), consts.len(), vary.len());
        for k in &consts {
            wire.push('\n');
            wire.push_str(&string_token(k));
            wire.push('\t');
            wire.push_str(&serde_json::to_string(&flats[0][k]).unwrap_or_default());
        }
        wire.push('\n');
        wire.push_str(
            &vary
                .iter()
                .map(|k| string_token(k))
                .collect::<Vec<_>>()
                .join("\t"),
        );
        wire.push('\n');
        wire.push_str(&types.iter().collect::<String>());
        for f in &flats {
            wire.push('\n');
            let cells: Vec<String> = vary
                .iter()
                .zip(&types)
                .map(|(k, t)| cell_for(f.get(k), *t))
                .collect();
            wire.push_str(&cells.join("\t"));
        }

        // Self-verify so an edge case (empty object, leaf/parent collision) refuses cleanly and the
        // block falls through to offload, rather than blocking it at the admission gate.
        let decoded = decode(&wire)?;
        if crate::atom::canonicalize(&decoded) != crate::atom::canonicalize(value) {
            return None;
        }
        Some(Encoded { wire, decoded })
    }
}

fn flatten(
    obj: &Map<String, Value>,
    stop: &std::collections::HashSet<String>,
) -> Option<Map<String, Value>> {
    let mut out = Map::new();
    flatten_into("", obj, &mut out, stop)?;
    Some(out)
}

// First-segment fields whose flattening yields many low-coverage columns (a per-record-varying map);
// leaving them as json cells avoids a sparse-column blowup that makes the wire larger than the input.
fn sparse_prefixes(flats: &[Map<String, Value>]) -> std::collections::HashSet<String> {
    let n = flats.len();
    let mut presence: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for f in flats {
        for k in f.keys() {
            *presence.entry(k).or_insert(0) += 1;
        }
    }
    let mut by_prefix: std::collections::HashMap<String, Vec<usize>> =
        std::collections::HashMap::new();
    for (col, &c) in &presence {
        if let Some((pre, _)) = col.split_once(SEP) {
            by_prefix.entry(pre.to_string()).or_default().push(c);
        }
    }
    by_prefix
        .into_iter()
        .filter(|(_, cs)| cs.len() >= 4 && cs.iter().sum::<usize>() * 5 < cs.len() * n * 2)
        .map(|(pre, _)| pre)
        .collect()
}

// Scalars, arrays, empty objects, and any object with a dotted or deep-dotted key become leaves;
// only a cleanly-dotted-path object recurses. A dotted key at the record root is unrecoverable and
// refuses the block; deeper it just keeps that object as a json cell so the rest still tablifies.
fn flatten_into(
    prefix: &str,
    obj: &Map<String, Value>,
    out: &mut Map<String, Value>,
    stop: &std::collections::HashSet<String>,
) -> Option<()> {
    for (k, v) in obj {
        if prefix.is_empty() && k.contains(SEP) {
            return None;
        }
        let path = if prefix.is_empty() {
            k.clone()
        } else {
            format!("{prefix}{SEP}{k}")
        };
        if flattenable(v) && !(prefix.is_empty() && stop.contains(k)) {
            flatten_into(&path, v.as_object()?, out, stop)?;
        } else {
            out.insert(path, v.clone());
        }
    }
    Some(())
}

fn flattenable(v: &Value) -> bool {
    matches!(v, Value::Object(m) if !m.is_empty()
        && m.iter().all(|(k, val)| !k.contains(SEP)
            && (!matches!(val, Value::Object(o) if !o.is_empty()) || flattenable(val))))
}

fn cell_for(value: Option<&Value>, ty: char) -> String {
    match (value, ty) {
        (None, _) => String::new(),
        (Some(Value::String(s)), 's') => s.clone(),
        (Some(v), _) => serde_json::to_string(v).unwrap_or_default(),
    }
}

pub fn decode(wire: &str) -> Option<Value> {
    let mut lines = wire.split('\n');
    let mut head = lines.next()?.strip_prefix(HEADER)?.split_whitespace();
    let count: usize = head.next()?.parse().ok()?;
    let n_const: usize = head.next()?.parse().ok()?;
    let n_vary: usize = head.next()?.parse().ok()?;
    if head.next().is_some() {
        return None;
    }

    let mut consts: Vec<(String, Value)> = Vec::with_capacity(n_const);
    for _ in 0..n_const {
        let (p, v) = lines.next()?.split_once('\t')?;
        consts.push((serde_json::from_str(p).ok()?, serde_json::from_str(v).ok()?));
    }

    let vary_line = lines.next()?;
    let paths: Vec<String> = if n_vary == 0 {
        Vec::new()
    } else {
        vary_line
            .split('\t')
            .map(serde_json::from_str)
            .collect::<Result<_, _>>()
            .ok()?
    };
    let types: Vec<char> = lines.next()?.chars().collect();
    if paths.len() != n_vary || types.len() != n_vary {
        return None;
    }

    let mut items = Vec::with_capacity(count);
    for _ in 0..count {
        let row = lines.next()?;
        let cells: Vec<&str> = if n_vary == 0 {
            Vec::new()
        } else {
            row.split('\t').collect()
        };
        if cells.len() != n_vary {
            return None;
        }
        let mut obj = Map::new();
        for (p, v) in &consts {
            insert_path(&mut obj, p, v.clone())?;
        }
        for i in 0..n_vary {
            let val = match types[i] {
                's' => Value::String(cells[i].to_string()),
                'j' if cells[i].is_empty() => continue,
                'j' => serde_json::from_str(cells[i]).ok()?,
                _ => return None,
            };
            insert_path(&mut obj, &paths[i], val)?;
        }
        items.push(Value::Object(obj));
    }
    if lines.next().is_some() {
        return None;
    }
    Some(Value::Array(items))
}

// Rebuilds a nested object from a dotted path; a segment that collides with an existing scalar fails,
// so the gate falls back to verbatim rather than emit a wrong shape.
fn insert_path(root: &mut Map<String, Value>, path: &str, val: Value) -> Option<()> {
    let segs: Vec<&str> = path.split(SEP).collect();
    let mut cur = root;
    for seg in &segs[..segs.len() - 1] {
        let entry = cur
            .entry(seg.to_string())
            .or_insert_with(|| Value::Object(Map::new()));
        cur = entry.as_object_mut()?;
    }
    cur.insert(segs.last()?.to_string(), val);
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(json: &str) {
        let value: Value = serde_json::from_str(json).unwrap();
        let encoded = Nested::default().try_encode(&value).unwrap();
        assert_eq!(
            decode(&encoded.wire).unwrap(),
            value,
            "wire:\n{}",
            encoded.wire
        );
    }

    #[test]
    fn round_trips_scalar_subobjects() {
        round_trip(r#"[{"n":1,"user":{"login":"a","id":7}},{"n":2,"user":{"login":"b","id":8}}]"#);
    }

    #[test]
    fn ragged_records_use_empty_cells() {
        round_trip(r#"[{"a":1,"b":"x"},{"a":2},{"a":3,"b":"y","c":true}]"#);
    }

    #[test]
    fn null_and_empty_string_stay_distinct_from_absent() {
        round_trip(r#"[{"a":null,"b":""},{"a":"","b":null},{"c":1}]"#);
    }

    #[test]
    fn array_valued_fields_stay_json_cells() {
        round_trip(r#"[{"n":1,"labels":["p1","bug"]},{"n":2,"labels":[]}]"#);
    }

    #[test]
    fn constant_columns_are_hoisted_once() {
        let value: Value = serde_json::from_str(
            r#"[{"n":1,"repo":"cli/cli","kind":"pr"},{"n":2,"repo":"cli/cli","kind":"pr"}]"#,
        )
        .unwrap();
        let wire = Nested::default().try_encode(&value).unwrap().wire;
        assert!(
            wire.starts_with("SWNEST 2 2 1"),
            "two constants, one varying: {wire}"
        );
        assert_eq!(decode(&wire).unwrap(), value);
    }

    #[test]
    fn string_values_are_literal_and_readable() {
        let value: Value =
            serde_json::from_str(r#"[{"u":{"login":"alice"}},{"u":{"login":"bob"}}]"#).unwrap();
        let wire = Nested::default().try_encode(&value).unwrap().wire;
        assert!(wire.contains("alice") && wire.contains("bob") && !wire.contains("\"alice\""));
    }

    #[test]
    fn keys_with_the_separator_are_refused() {
        let value: Value = serde_json::from_str(r#"[{"a.b":1},{"a.b":2}]"#).unwrap();
        assert!(Nested::default().try_encode(&value).is_none());
    }
}
