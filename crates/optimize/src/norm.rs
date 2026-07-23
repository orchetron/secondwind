use std::collections::{HashMap, HashSet};

use serde_json::{Map, Value};

use crate::column::string_token;
use crate::nested::{self, Nested};
use crate::transform::{Encoded, Transform};

const HEADER: &str = "SWNORM";

// Relational normalization: a record's inner record-collections (dependency arrays, dependency maps,
// release file-lists) are pulled into side tables keyed by a detected unique field, so repeating
// groups compress into their own table instead of a per-row json blob. The model reads it as a
// foreign-key join, which it can do (unlike a positional join). Reversible; self-verified in admit.
pub struct Normalize;

impl Transform for Normalize {
    fn id(&self) -> &'static str {
        "norm"
    }

    fn trusted(&self) -> bool {
        true
    }

    fn try_encode(&self, value: &Value) -> Option<Encoded> {
        let items = value.as_array()?;
        let wire = encode(items)?;
        let decoded = decode(&wire)?;
        if crate::atom::canonicalize(&decoded) != crate::atom::canonicalize(value) {
            return None;
        }
        Some(Encoded { wire, decoded })
    }
}

fn detect_fk(records: &[&Map<String, Value>]) -> Option<String> {
    for k in records[0].keys() {
        let scalar = records
            .iter()
            .all(|r| r.get(k).is_some_and(|v| v.is_string() || v.is_number()));
        if !scalar {
            continue;
        }
        let mut seen = HashSet::new();
        if records
            .iter()
            .all(|r| seen.insert(serde_json::to_string(&r[k]).unwrap_or_default()))
        {
            return Some(k.clone());
        }
    }
    None
}

// A non-empty array of objects ('a') or a map of >= 4 entries ('m') is a repeating group worth
// extracting; anything else stays a main-table cell.
fn as_inner_collection(v: &Value) -> Option<(char, Vec<Value>)> {
    match v {
        Value::Array(a) if !a.is_empty() && a.iter().all(Value::is_object) => {
            Some(('a', a.clone()))
        }
        Value::Object(m) if m.len() >= 4 => Some((
            'm',
            m.iter()
                .map(|(k, val)| {
                    let mut r = Map::new();
                    r.insert("key".into(), Value::String(k.clone()));
                    r.insert("value".into(), val.clone());
                    Value::Object(r)
                })
                .collect(),
        )),
        _ => None,
    }
}

fn pick_sidefk(rows: &[(Value, Value)]) -> String {
    let keys: HashSet<&String> = rows
        .iter()
        .filter_map(|(_, r)| r.as_object())
        .flat_map(Map::keys)
        .collect();
    let mut c = String::from("_fk");
    while keys.contains(&c) {
        c.insert(0, '_');
    }
    c
}

pub fn encode(items: &[Value]) -> Option<String> {
    if items.len() < 2 {
        return None;
    }
    let records: Vec<&Map<String, Value>> =
        items.iter().map(Value::as_object).collect::<Option<_>>()?;
    let fk = detect_fk(&records)?;

    let mut main: Vec<Value> = Vec::with_capacity(records.len());
    let mut order: Vec<String> = Vec::new();
    let mut shapes: HashMap<String, char> = HashMap::new();
    let mut rows: HashMap<String, Vec<(Value, Value)>> = HashMap::new();
    for rec in &records {
        let fkval = rec[&fk].clone();
        let mut m = Map::new();
        for (k, v) in *rec {
            let collection = if k == &fk {
                None
            } else {
                as_inner_collection(v)
            };
            match collection {
                Some((shape, inner)) => {
                    match shapes.get(k) {
                        None => {
                            order.push(k.clone());
                            shapes.insert(k.clone(), shape);
                        }
                        Some(&s) if s != shape => {
                            shapes.insert(k.clone(), 'x');
                        }
                        _ => {}
                    }
                    let e = rows.entry(k.clone()).or_default();
                    for row in inner {
                        e.push((fkval.clone(), row));
                    }
                }
                None => {
                    m.insert(k.clone(), v.clone());
                }
            }
        }
        main.push(Value::Object(m));
    }
    if order.is_empty() || order.iter().any(|k| shapes[k] == 'x') {
        return None;
    }

    let main_wire = Nested.try_encode(&Value::Array(main))?.wire;
    let mut parts = vec![
        format!("{HEADER}\t{}\t{}", string_token(&fk), order.len()),
        format!("@main\t{}", main_wire.split('\n').count()),
        main_wire,
    ];
    for k in &order {
        let side_rows = &rows[k];
        let sidefk = pick_sidefk(side_rows);
        let side_records: Vec<Value> = side_rows
            .iter()
            .map(|(fv, row)| {
                let mut r = row.as_object().cloned().unwrap_or_default();
                r.insert(sidefk.clone(), fv.clone());
                Value::Object(r)
            })
            .collect();
        let side_wire = Nested.try_encode(&Value::Array(side_records))?.wire;
        parts.push(format!(
            "@side\t{}\t{}\t{}\t{}",
            string_token(k),
            shapes[k],
            string_token(&sidefk),
            side_wire.split('\n').count()
        ));
        parts.push(side_wire);
    }
    Some(parts.join("\n"))
}

pub fn decode(wire: &str) -> Option<Value> {
    let mut lines = wire.split('\n').peekable();
    let head: Vec<&str> = lines.next()?.split('\t').collect();
    if head.len() != 3 || head[0] != HEADER {
        return None;
    }
    let fk: String = serde_json::from_str(head[1]).ok()?;
    let n_sides: usize = head[2].parse().ok()?;

    let mainhdr: Vec<&str> = lines.next()?.split('\t').collect();
    if mainhdr.len() != 2 || mainhdr[0] != "@main" {
        return None;
    }
    let main = nested::decode(&take(&mut lines, mainhdr[1].parse().ok()?)?)?;
    let mut records: Vec<Map<String, Value>> = main
        .as_array()?
        .iter()
        .map(|v| v.as_object().cloned())
        .collect::<Option<_>>()?;

    for _ in 0..n_sides {
        let sh: Vec<&str> = lines.next()?.split('\t').collect();
        if sh.len() != 5 || sh[0] != "@side" {
            return None;
        }
        let field: String = serde_json::from_str(sh[1]).ok()?;
        let shape = sh[2].chars().next()?;
        let sidefk: String = serde_json::from_str(sh[3]).ok()?;
        let side = nested::decode(&take(&mut lines, sh[4].parse().ok()?)?)?;

        let mut grouped: HashMap<String, Vec<Value>> = HashMap::new();
        for row in side.as_array()? {
            let mut r = row.as_object()?.clone();
            let key = serde_json::to_string(&r.remove(&sidefk)?).ok()?;
            grouped.entry(key).or_default().push(Value::Object(r));
        }
        for rec in &mut records {
            let key = serde_json::to_string(rec.get(&fk)?).ok()?;
            let Some(inner) = grouped.get(&key) else {
                continue;
            };
            rec.insert(field.clone(), refold(shape, inner)?);
        }
    }
    if lines.next().is_some() {
        return None;
    }
    Some(Value::Array(
        records.into_iter().map(Value::Object).collect(),
    ))
}

fn take<'a>(lines: &mut impl Iterator<Item = &'a str>, n: usize) -> Option<String> {
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        out.push(lines.next()?);
    }
    Some(out.join("\n"))
}

fn refold(shape: char, inner: &[Value]) -> Option<Value> {
    match shape {
        'a' => Some(Value::Array(inner.to_vec())),
        'm' => {
            let mut map = Map::new();
            for row in inner {
                let r = row.as_object()?;
                map.insert(r.get("key")?.as_str()?.to_string(), r.get("value")?.clone());
            }
            Some(Value::Object(map))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(json: &str) {
        let value: Value = serde_json::from_str(json).unwrap();
        if let Some(wire) = encode(value.as_array().unwrap()) {
            assert_eq!(decode(&wire).unwrap(), value, "wire:\n{wire}");
        }
    }

    #[test]
    fn extracts_an_array_inner_collection_and_round_trips() {
        let items: Vec<String> = (0..6)
            .map(|i| format!(
                r#"{{"id":"pkg{i}","version":"1.0.{i}","dependencies":[{{"name":"serde","req":"^1.{i}"}},{{"name":"tokio","req":"^1.0"}}]}}"#
            ))
            .collect();
        let json = format!("[{}]", items.join(","));
        let value: Value = serde_json::from_str(&json).unwrap();
        let wire = encode(value.as_array().unwrap()).unwrap();
        assert!(wire.contains("@side") && wire.contains("serde"));
        assert_eq!(decode(&wire).unwrap(), value);
    }

    #[test]
    fn extracts_a_map_inner_collection() {
        round_trip(
            r#"[{"id":"a","deps":{"x":"1","y":"2","z":"3","w":"4"}},{"id":"b","deps":{"x":"5","y":"6","z":"7","w":"8","v":"9"}}]"#,
        );
    }

    #[test]
    fn records_without_the_collection_keep_their_own_value() {
        round_trip(r#"[{"id":"a","labels":[{"n":"bug"}]},{"id":"b","labels":[]},{"id":"c"}]"#);
    }

    #[test]
    fn refuses_without_a_unique_key() {
        let value: Value =
            serde_json::from_str(r#"[{"s":"x","d":[{"a":1}]},{"s":"x","d":[{"a":2}]}]"#).unwrap();
        assert!(encode(value.as_array().unwrap()).is_none());
    }
}
