use serde_json::{Map, Value};

use crate::column::string_token;
use crate::nested::{self, Nested};
use crate::transform::{Encoded, Transform};

const HEADER: &str = "SWDOC";
const MAP_MIN: usize = 8;

// Readable document codec for a top-level object: scalar fields stay literal, and each nested record
// collection (array of objects, or a map of objects/scalars/arrays) becomes an embedded readable
// table, so object-map responses (registries, lockfiles) compress inline instead of only offloading.
pub struct Doc;

impl Transform for Doc {
    fn id(&self) -> &'static str {
        "doc"
    }

    // Reuses the trusted nested table codec and self-verifies the whole document below.
    fn trusted(&self) -> bool {
        true
    }

    fn try_encode(&self, value: &Value) -> Option<Encoded> {
        let obj = value.as_object()?;
        if obj.is_empty() {
            return None;
        }
        let mut entries: Vec<String> = Vec::with_capacity(obj.len());
        let mut tablified = 0;
        for (k, v) in obj {
            if let Some((shape, keycol, records)) = tablify(v) {
                // Normalize (pull inner collections into side tables) when it beats a plain table.
                let plain = Nested
                    .try_encode(&Value::Array(records.clone()))
                    .map(|e| e.wire);
                let normed = crate::norm::encode(&records);
                let table = match (normed, plain) {
                    (Some(a), Some(b)) => Some(if a.len() < b.len() { a } else { b }),
                    (a, b) => a.or(b),
                };
                if let Some(tw) = table
                    && tw.len() < serde_json::to_string(v).map(|s| s.len()).unwrap_or(0)
                {
                    let nlines = tw.split('\n').count();
                    let kc = keycol.map(|c| string_token(&c)).unwrap_or_default();
                    entries.push(format!(
                        "T\t{}\t{shape}\t{kc}\t{nlines}\n{tw}",
                        string_token(k)
                    ));
                    tablified += 1;
                    continue;
                }
            }
            entries.push(format!(
                "L\t{}\t{}",
                string_token(k),
                serde_json::to_string(v).ok()?
            ));
        }
        if tablified == 0 {
            return None;
        }
        let wire = format!("{HEADER} {}\n{}", obj.len(), entries.join("\n"));
        let decoded = decode(&wire)?;
        if crate::atom::canonicalize(&decoded) != crate::atom::canonicalize(value) {
            return None;
        }
        Some(Encoded { wire, decoded })
    }
}

// A tablifiable field returns (shape, key-column for a spread map, records to feed the table codec).
fn tablify(v: &Value) -> Option<(char, Option<String>, Vec<Value>)> {
    match v {
        Value::Array(items) if items.len() >= 2 && items.iter().all(Value::is_object) => {
            Some(('a', None, items.clone()))
        }
        Value::Object(m) if m.len() >= MAP_MIN && m.values().all(Value::is_object) => {
            let keycol = pick_keycol(m);
            let records = m
                .iter()
                .map(|(k, val)| {
                    let mut r = val.as_object().cloned().unwrap_or_default();
                    r.insert(keycol.clone(), Value::String(k.clone()));
                    Value::Object(r)
                })
                .collect();
            Some(('o', Some(keycol), records))
        }
        Value::Object(m) if m.len() >= MAP_MIN => {
            let records = m
                .iter()
                .map(|(k, val)| {
                    let mut r = Map::new();
                    r.insert("key".into(), Value::String(k.clone()));
                    r.insert("value".into(), val.clone());
                    Value::Object(r)
                })
                .collect();
            Some(('v', None, records))
        }
        _ => None,
    }
}

fn pick_keycol(m: &Map<String, Value>) -> String {
    let keys: std::collections::HashSet<&String> = m
        .values()
        .filter_map(Value::as_object)
        .flat_map(Map::keys)
        .collect();
    let mut c = String::from("key");
    while keys.contains(&c) {
        c.insert(0, '_');
    }
    c
}

pub fn decode(wire: &str) -> Option<Value> {
    let mut lines = wire.split('\n');
    let mut head = lines.next()?.strip_prefix(HEADER)?.split_whitespace();
    let n: usize = head.next()?.parse().ok()?;
    if head.next().is_some() {
        return None;
    }
    let mut obj = Map::new();
    for _ in 0..n {
        let mut f = lines.next()?.splitn(2, '\t');
        match f.next()? {
            "L" => {
                let (k, v) = f.next()?.split_once('\t')?;
                obj.insert(serde_json::from_str(k).ok()?, serde_json::from_str(v).ok()?);
            }
            "T" => {
                let parts: Vec<&str> = f.next()?.split('\t').collect();
                if parts.len() != 4 {
                    return None;
                }
                let key: String = serde_json::from_str(parts[0]).ok()?;
                let shape = parts[1].chars().next()?;
                let keycol: Option<String> = (!parts[2].is_empty())
                    .then(|| serde_json::from_str(parts[2]))
                    .transpose()
                    .ok()?;
                let nlines: usize = parts[3].parse().ok()?;
                let mut sub = String::new();
                for i in 0..nlines {
                    if i > 0 {
                        sub.push('\n');
                    }
                    sub.push_str(lines.next()?);
                }
                let items = crate::norm::decode(&sub).or_else(|| nested::decode(&sub))?;
                let items = items.as_array()?;
                obj.insert(key, refold(shape, keycol.as_deref(), items)?);
            }
            _ => return None,
        }
    }
    if lines.next().is_some() {
        return None;
    }
    Some(Value::Object(obj))
}

fn refold(shape: char, keycol: Option<&str>, items: &[Value]) -> Option<Value> {
    match shape {
        'a' => Some(Value::Array(items.to_vec())),
        'o' => {
            let kc = keycol?;
            let mut map = Map::new();
            for it in items {
                let mut r = it.as_object()?.clone();
                let k = r.remove(kc)?;
                map.insert(k.as_str()?.to_string(), Value::Object(r));
            }
            Some(Value::Object(map))
        }
        'v' => {
            let mut map = Map::new();
            for it in items {
                let r = it.as_object()?;
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
        if let Some(enc) = Doc.try_encode(&value) {
            assert_eq!(decode(&enc.wire).unwrap(), value, "wire:\n{}", enc.wire);
        }
    }

    fn map_of_objects(n: usize) -> String {
        let rows: Vec<String> = (0..n)
            .map(|i| format!(r#""1.0.{i}":{{"main":"index.js","license":"MIT","size":{i}}}"#))
            .collect();
        format!(r#"{{"name":"pkg","versions":{{{}}}}}"#, rows.join(","))
    }

    #[test]
    fn tablifies_a_map_of_objects_and_round_trips() {
        let value: Value = serde_json::from_str(&map_of_objects(10)).unwrap();
        let enc = Doc.try_encode(&value).unwrap();
        assert!(enc.wire.contains("SWDOC") && enc.wire.contains("index.js"));
        assert_eq!(decode(&enc.wire).unwrap(), value);
    }

    #[test]
    fn tablifies_an_array_of_objects_field() {
        let rows: Vec<String> = (0..10)
            .map(|i| format!(r#"{{"name":"crate-{i}","version":"0.1.{i}"}}"#))
            .collect();
        round_trip(&format!(
            r#"{{"packages":[{}],"count":10}}"#,
            rows.join(",")
        ));
    }

    #[test]
    fn tablifies_a_map_of_scalars_and_arrays() {
        let times: Vec<String> = (0..10)
            .map(|i| format!(r#""1.0.{i}":"2020-01-{i:02}T00:00:00Z""#))
            .collect();
        round_trip(&format!(r#"{{"time":{{{}}}}}"#, times.join(",")));
        let rel: Vec<String> = (0..10)
            .map(|i| format!(r#""1.0.{i}":[{{"url":"u{i}","size":{i}}}]"#))
            .collect();
        round_trip(&format!(r#"{{"releases":{{{}}}}}"#, rel.join(",")));
    }

    #[test]
    fn refuses_a_plain_struct_with_no_collection() {
        let value: Value = serde_json::from_str(r#"{"name":"pkg","version":"1.0.0"}"#).unwrap();
        assert!(Doc.try_encode(&value).is_none());
    }
}
