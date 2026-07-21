use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AtomClass {
    Key,
    Number,
    Text,
    Bool,
    Null,
}

impl AtomClass {
    fn tag(self) -> u8 {
        match self {
            AtomClass::Key => 1,
            AtomClass::Number => 2,
            AtomClass::Text => 3,
            AtomClass::Bool => 4,
            AtomClass::Null => 5,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Atom {
    pub class: AtomClass,
    pub bytes: String,
}

impl Atom {
    pub fn fingerprint(&self) -> [u8; 32] {
        let mut hasher = blake3::Hasher::new();
        hasher.update(&[self.class.tag()]);
        hasher.update(self.bytes.as_bytes());
        *hasher.finalize().as_bytes()
    }
}

pub fn leaves(value: &Value) -> Vec<Atom> {
    let mut atoms = Vec::new();
    walk(value, &mut atoms);
    atoms
}

fn walk(value: &Value, out: &mut Vec<Atom>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                out.push(Atom {
                    class: AtomClass::Key,
                    bytes: key.clone(),
                });
                walk(child, out);
            }
        }
        Value::Array(items) => {
            for child in items {
                walk(child, out);
            }
        }
        Value::String(s) => out.push(Atom {
            class: AtomClass::Text,
            bytes: s.clone(),
        }),
        Value::Number(n) => out.push(Atom {
            class: AtomClass::Number,
            bytes: n.to_string(),
        }),
        Value::Bool(b) => out.push(Atom {
            class: AtomClass::Bool,
            bytes: b.to_string(),
        }),
        Value::Null => out.push(Atom {
            class: AtomClass::Null,
            bytes: "null".into(),
        }),
    }
}

pub fn canonicalize(value: &Value) -> String {
    let mut out = String::new();
    write_canonical(value, &mut out);
    out
}

fn write_canonical(value: &Value, out: &mut String) {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (i, key) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_json_string(key, out);
                out.push(':');
                write_canonical(&map[*key], out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (i, child) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(child, out);
            }
            out.push(']');
        }
        Value::String(s) => write_json_string(s, out),
        Value::Number(n) => out.push_str(&n.to_string()),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Null => out.push_str("null"),
    }
}

fn write_json_string(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

// True if any JSON object in `text` has a duplicate key. The standard parse keeps last-wins and
// silently drops a value, so such an ambiguous block is kept verbatim, never certified compressed.
pub fn has_duplicate_keys(text: &str) -> bool {
    use serde::de::{DeserializeSeed, Deserializer, Error, MapAccess, SeqAccess, Visitor};
    use std::collections::HashSet;
    use std::fmt;

    struct Walk;
    impl<'de> Visitor<'de> for Walk {
        type Value = bool;
        fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
            f.write_str("any JSON value")
        }
        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<bool, A::Error> {
            let mut seen = HashSet::new();
            let mut dup = false;
            while let Some(key) = map.next_key::<String>()? {
                dup |= !seen.insert(key);
                dup |= map.next_value_seed(Walk)?;
            }
            Ok(dup)
        }
        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<bool, A::Error> {
            let mut dup = false;
            while let Some(child) = seq.next_element_seed(Walk)? {
                dup |= child;
            }
            Ok(dup)
        }
        fn visit_bool<E: Error>(self, _: bool) -> Result<bool, E> {
            Ok(false)
        }
        fn visit_i64<E: Error>(self, _: i64) -> Result<bool, E> {
            Ok(false)
        }
        fn visit_u64<E: Error>(self, _: u64) -> Result<bool, E> {
            Ok(false)
        }
        fn visit_f64<E: Error>(self, _: f64) -> Result<bool, E> {
            Ok(false)
        }
        fn visit_str<E: Error>(self, _: &str) -> Result<bool, E> {
            Ok(false)
        }
        fn visit_none<E: Error>(self) -> Result<bool, E> {
            Ok(false)
        }
        fn visit_unit<E: Error>(self) -> Result<bool, E> {
            Ok(false)
        }
    }
    impl<'de> DeserializeSeed<'de> for Walk {
        type Value = bool;
        fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<bool, D::Error> {
            d.deserialize_any(Walk)
        }
    }
    let mut de = serde_json::Deserializer::from_str(text);
    Walk.deserialize(&mut de).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_duplicate_keys() {
        assert!(has_duplicate_keys(r#"{"a":1,"a":2}"#));
        assert!(has_duplicate_keys(r#"[{"id":1},{"x":1,"y":2,"x":3}]"#));
        assert!(!has_duplicate_keys(r#"{"a":1,"b":2}"#));
        assert!(!has_duplicate_keys(r#"[{"a":1},{"a":2}]"#));
        assert!(!has_duplicate_keys(r#"{"a":{"b":1},"c":[1,2,3],"d":1.50}"#));
        assert!(!has_duplicate_keys("not json at all"));
    }

    #[test]
    fn number_bytes_are_preserved_exactly() {
        let v: Value = serde_json::from_str(r#"{"a": 1.0, "b": 1, "c": 1.50}"#).unwrap();
        let atoms = leaves(&v);
        let nums: Vec<&str> = atoms
            .iter()
            .filter(|a| a.class == AtomClass::Number)
            .map(|a| a.bytes.as_str())
            .collect();
        assert_eq!(nums, vec!["1.0", "1", "1.50"]);
    }

    #[test]
    fn canonical_sorts_keys_and_round_trips_numbers() {
        let v: Value = serde_json::from_str(r#"{"b":2,"a":1.0}"#).unwrap();
        assert_eq!(canonicalize(&v), r#"{"a":1.0,"b":2}"#);
    }
}
