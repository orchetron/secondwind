use serde_json::{Map, Value};

use crate::nested::{self, Nested};
use crate::transform::Transform;

const HEADER: &str = "SWLOCK";
const SCALARS: &[&str] = &["name", "version", "source", "checksum"];

// Readable inline compression for Cargo.lock: the repeating [[package]] blocks become one record
// table (constant `source` hoisted once, field names stated once), and the exact TOML is rebuilt on
// decode. The model reads a package table. Byte-exact or it declines.
pub fn encode(raw: &str) -> Option<String> {
    let first = raw.find("[[package]]\n")?;
    let preamble = &raw[..first];
    let trailing = raw.ends_with('\n');
    let body = raw[first..].trim_end_matches('\n');

    let mut records = Vec::new();
    for block in body.split("\n\n") {
        records.push(parse_block(block)?);
    }
    if records.len() < 3 {
        return None;
    }
    let table = Nested::default().try_encode(&Value::Array(records))?.wire;
    let wire = format!(
        "{HEADER} {} {}\n{preamble}{table}",
        trailing as u8,
        preamble.len()
    );
    (decode(&wire).as_deref() == Some(raw)).then_some(wire)
}

fn parse_block(block: &str) -> Option<Value> {
    let mut lines = block.split('\n');
    if lines.next()? != "[[package]]" {
        return None;
    }
    let mut obj = Map::new();
    while let Some(line) = lines.next() {
        let (key, val) = line.split_once(" = ")?;
        if key == "dependencies" && val == "[" {
            let mut deps = Vec::new();
            for dl in lines.by_ref() {
                if dl == "]" {
                    break;
                }
                let dep: String = serde_json::from_str(dl.trim().trim_end_matches(',')).ok()?;
                deps.push(Value::String(dep));
            }
            obj.insert("dependencies".into(), Value::Array(deps));
        } else {
            obj.insert(key.to_string(), serde_json::from_str(val).ok()?);
        }
    }
    Some(Value::Object(obj))
}

pub fn decode(wire: &str) -> Option<String> {
    let nl = wire.find('\n')?;
    let mut head = wire[..nl].split(' ');
    if head.next()? != HEADER {
        return None;
    }
    let trailing = head.next()? == "1";
    let plen: usize = head.next()?.parse().ok()?;
    let after = &wire[nl + 1..];
    if after.len() < plen || !after.is_char_boundary(plen) {
        return None;
    }
    let (preamble, table) = after.split_at(plen);

    let arr = nested::decode(table)?;
    let blocks: Vec<String> = arr
        .as_array()?
        .iter()
        .map(|r| build_block(r.as_object()?))
        .collect::<Option<_>>()?;
    let mut s = format!("{preamble}{}", blocks.join("\n\n"));
    if trailing {
        s.push('\n');
    }
    Some(s)
}

fn build_block(rec: &Map<String, Value>) -> Option<String> {
    let mut s = String::from("[[package]]");
    for f in SCALARS {
        if let Some(v) = rec.get(*f) {
            s.push_str(&format!("\n{f} = {}", serde_json::to_string(v).ok()?));
        }
    }
    if let Some(Value::Array(deps)) = rec.get("dependencies") {
        s.push_str("\ndependencies = [");
        for d in deps {
            s.push_str(&format!("\n {},", serde_json::to_string(d).ok()?));
        }
        s.push_str("\n]");
    }
    Some(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOCK: &str = "# @generated\nversion = 4\n\n[[package]]\nname = \"adler2\"\nversion = \"2.0.1\"\nsource = \"registry+x\"\nchecksum = \"abc\"\n\n[[package]]\nname = \"aho\"\nversion = \"1.1.4\"\nsource = \"registry+x\"\nchecksum = \"def\"\ndependencies = [\n \"memchr\",\n]\n\n[[package]]\nname = \"local\"\nversion = \"0.1.0\"\n";

    #[test]
    fn round_trips_a_cargo_lock() {
        let wire = encode(LOCK).unwrap();
        assert!(wire.starts_with("SWLOCK 1 ") && wire.contains("adler2"));
        assert_eq!(decode(&wire).as_deref(), Some(LOCK));
    }

    #[test]
    fn declines_non_lock_text() {
        assert!(encode("some\nplain\ntext\nhere\n").is_none());
    }
}
