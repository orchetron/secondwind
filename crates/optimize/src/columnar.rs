use std::sync::Arc;

use serde_json::{Map, Value};

use crate::column::{
    ColumnCodec, choose_codec, codec_header, decode_cell, is_scalar, parse_codec, produces_cells,
    render_cells, scalar_token, string_token,
};
use crate::tokens::{ByteCounter, TokenCounter};
use crate::transform::{Encoded, Transform};

const HEADER: &str = "SWCOL";

/// Tab is a safe cell delimiter (JSON escapes tabs inside strings to \t), newline a
/// safe row delimiter. Each column's codec is chosen by the counter's unit: bytes by
/// default, token cost with a real tokenizer.
pub struct Columnar {
    counter: Arc<dyn TokenCounter>,
}

impl Default for Columnar {
    fn default() -> Self {
        Self {
            counter: Arc::new(ByteCounter),
        }
    }
}

impl Columnar {
    pub fn with_counter(counter: Arc<dyn TokenCounter>) -> Self {
        Self { counter }
    }
}

impl Transform for Columnar {
    fn id(&self) -> &'static str {
        "columnar"
    }

    // Built-in, fuzzed round trip (see tests/fuzz.rs): admission skips the per-block idempotence
    // re-encode, which roughly halved this codec's cost since it was running once in the search and
    // again in the check. Losslessness is unchanged: CLMH + inverse witness still run every block.
    fn trusted(&self) -> bool {
        true
    }

    fn try_encode(&self, value: &Value) -> Option<Encoded> {
        let items = value.as_array()?;
        if items.len() < 2 {
            return None;
        }
        let first = items[0].as_object()?;
        if first.is_empty() || first.values().any(|v| !is_scalar(v)) {
            return None;
        }
        let mut keys: Vec<&String> = first.keys().collect();
        keys.sort();

        let mut tokens: Vec<Vec<String>> = vec![Vec::with_capacity(items.len()); keys.len()];
        let mut is_string: Vec<Vec<bool>> = vec![Vec::with_capacity(items.len()); keys.len()];
        for item in items {
            let obj = item.as_object()?;
            if obj.len() != keys.len() || !keys.iter().all(|k| obj.contains_key(*k)) {
                return None;
            }
            for (col, key) in keys.iter().enumerate() {
                let cell = &obj[*key];
                if !is_scalar(cell) {
                    return None;
                }
                tokens[col].push(scalar_token(cell)?);
                is_string[col].push(cell.is_string());
            }
        }

        let codecs: Vec<ColumnCodec> = (0..keys.len())
            .map(|c| choose_codec(&tokens[c], &is_string[c], self.counter.as_ref(), true))
            .collect();

        let mut wire = format!("{HEADER} {}\n{}", items.len(), keys.len());
        for key in &keys {
            wire.push('\t');
            wire.push_str(&string_token(key));
        }
        for codec in &codecs {
            wire.push('\n');
            wire.push_str(&codec_header(codec));
        }
        let rendered: Vec<Vec<String>> = codecs
            .iter()
            .enumerate()
            .filter_map(|(c, codec)| render_cells(codec, &tokens[c]))
            .collect();
        for row in 0..items.len() {
            wire.push('\n');
            let cells: Vec<&str> = rendered.iter().map(|col| col[row].as_str()).collect();
            wire.push_str(&cells.join("\t"));
        }

        let decoded = decode(&wire)?;
        Some(Encoded { wire, decoded })
    }
}

pub fn decode(wire: &str) -> Option<Value> {
    let mut lines = wire.split('\n');
    let count: usize = lines.next()?.strip_prefix(HEADER)?.trim().parse().ok()?;

    let key_fields: Vec<&str> = lines.next()?.split('\t').collect();
    let n_keys: usize = key_fields.first()?.parse().ok()?;
    if key_fields.len() != 1 + n_keys {
        return None;
    }
    let keys: Vec<String> = key_fields[1..]
        .iter()
        .map(|t| serde_json::from_str(t))
        .collect::<Result<_, _>>()
        .ok()?;

    let mut codecs = Vec::with_capacity(n_keys);
    for _ in 0..n_keys {
        codecs.push(parse_codec(lines.next()?)?);
    }
    let n_cells = codecs.iter().filter(|c| produces_cells(c)).count();

    let mut items = Vec::with_capacity(count);
    for _ in 0..count {
        let cells: Vec<&str> = if n_cells == 0 {
            lines.next()?;
            Vec::new()
        } else {
            lines.next()?.split('\t').collect()
        };
        if cells.len() != n_cells {
            return None;
        }
        let mut obj = Map::new();
        let mut cell_iter = cells.into_iter();
        for (key, codec) in keys.iter().zip(&codecs) {
            let cell = if produces_cells(codec) {
                Some(cell_iter.next()?)
            } else {
                None
            };
            obj.insert(key.clone(), decode_cell(codec, cell)?);
        }
        items.push(Value::Object(obj));
    }
    if lines.next().is_some() {
        return None;
    }
    Some(Value::Array(items))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(json: &str) {
        let value: Value = serde_json::from_str(json).unwrap();
        let encoded = Columnar::default().try_encode(&value).unwrap();
        assert_eq!(
            decode(&encoded.wire).unwrap(),
            value,
            "wire:\n{}",
            encoded.wire
        );
    }

    #[test]
    fn round_trips_a_uniform_array() {
        round_trip(r#"[{"id":1,"svc":"a","ok":true},{"id":2,"svc":"b","ok":false}]"#);
    }

    #[test]
    fn low_cardinality_column_round_trips() {
        round_trip(r#"[{"s":"ok"},{"s":"FAIL"},{"s":"ok"},{"s":"ok"},{"s":"FAIL"},{"s":"ok"}]"#);
    }

    #[test]
    fn high_cardinality_strings_drop_quotes() {
        round_trip(r#"[{"p":"/srv/app_0/x"},{"p":"/srv/app_1/y"},{"p":"/srv/app_2/z"}]"#);
    }

    #[test]
    fn constant_column_round_trips() {
        round_trip(r#"[{"id":1,"kind":"host"},{"id":2,"kind":"host"},{"id":3,"kind":"host"}]"#);
    }

    #[test]
    fn refuses_non_uniform_or_nested() {
        let non_uniform: Value = serde_json::from_str(r#"[{"a":1},{"a":1,"b":2}]"#).unwrap();
        assert!(Columnar::default().try_encode(&non_uniform).is_none());
        let nested: Value = serde_json::from_str(r#"[{"a":[1]},{"a":[2]}]"#).unwrap();
        assert!(Columnar::default().try_encode(&nested).is_none());
    }

    #[test]
    fn strings_with_tabs_stay_json_encoded() {
        round_trip("[{\"k\":\"a\\tb\"},{\"k\":\"c\\nd\"},{\"k\":\"a\\tb\"}]");
    }

    #[test]
    fn unicode_and_mixed_types_round_trip() {
        round_trip("[{\"k\":\"caf\u{00e9}\",\"n\":1.50},{\"k\":\"th\u{00e9}\",\"n\":2}]");
    }

    #[test]
    fn string_value_that_looks_numeric_stays_a_string() {
        round_trip(r#"[{"k":"007"},{"k":"008"},{"k":"007"}]"#);
    }

    #[test]
    fn url_column_stays_literal_and_readable() {
        let json = r#"[{"u":"https://api.github.com/repos/cli/cli/issues/1"},{"u":"https://api.github.com/repos/cli/cli/issues/2"},{"u":"https://api.github.com/repos/cli/cli/issues/33"}]"#;
        let value: Value = serde_json::from_str(json).unwrap();
        let encoded = Columnar::default().try_encode(&value).unwrap();
        assert!(
            !encoded.wire.contains("affix\t") && encoded.wire.contains("issues/33"),
            "inline wire must keep values literal, wire:\n{}",
            encoded.wire
        );
        assert_eq!(decode(&encoded.wire).unwrap(), value);
    }

    #[test]
    fn shared_prefix_and_suffix_round_trip() {
        round_trip(r#"[{"f":"src/mod_a.rs"},{"f":"src/mod_bb.rs"},{"f":"src/mod_ccc.rs"}]"#);
    }

    #[test]
    fn unicode_values_round_trip() {
        round_trip(
            "[{\"s\":\"caf\u{00e9}_1\"},{\"s\":\"caf\u{00e9}_22\"},{\"s\":\"caf\u{00e9}_333\"}]",
        );
    }
}
