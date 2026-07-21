use std::sync::Arc;

use serde_json::{Map, Value};

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

enum ColumnCodec {
    Const(String),
    Dict {
        values: Vec<String>,
        index: Vec<usize>,
    },
    RawString,
    Json,
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
            .map(|c| choose_codec(&tokens[c], &is_string[c], self.counter.as_ref()))
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

fn choose_codec(tokens: &[String], is_string: &[bool], counter: &dyn TokenCounter) -> ColumnCodec {
    if tokens.iter().all(|t| t == &tokens[0]) {
        return ColumnCodec::Const(tokens[0].clone());
    }

    let mut order: Vec<String> = Vec::new();
    let mut seen: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    let mut index = Vec::with_capacity(tokens.len());
    for token in tokens {
        let pos = *seen.entry(token.as_str()).or_insert_with(|| {
            order.push(token.clone());
            order.len() - 1
        });
        index.push(pos);
    }

    let string_safe = is_string.iter().all(|s| *s)
        && tokens
            .iter()
            .all(|t| !unquote(t).contains('\t') && !unquote(t).contains('\n'));

    let mut candidates = vec![
        ColumnCodec::Dict {
            values: order,
            index,
        },
        ColumnCodec::Json,
    ];
    if string_safe {
        candidates.push(ColumnCodec::RawString);
    }
    candidates
        .into_iter()
        .min_by_key(|codec| column_cost(codec, tokens, counter))
        .expect("at least the json candidate is present")
}

// Counted cost of a column under a codec: header plus every rendered cell.
fn column_cost(codec: &ColumnCodec, tokens: &[String], counter: &dyn TokenCounter) -> usize {
    let mut cost = counter.count(&codec_header(codec));
    if let Some(cells) = render_cells(codec, tokens) {
        cost += cells.iter().map(|cell| counter.count(cell)).sum::<usize>();
    }
    cost
}

fn codec_header(codec: &ColumnCodec) -> String {
    match codec {
        ColumnCodec::Const(token) => format!("const\t{token}"),
        ColumnCodec::Dict { values, .. } => {
            let mut s = format!("dict\t{}", values.len());
            for v in values {
                s.push('\t');
                s.push_str(v);
            }
            s
        }
        ColumnCodec::RawString => "str".into(),
        ColumnCodec::Json => "json".into(),
    }
}

fn render_cells(codec: &ColumnCodec, tokens: &[String]) -> Option<Vec<String>> {
    match codec {
        ColumnCodec::Const(_) => None,
        ColumnCodec::Dict { index, .. } => Some(index.iter().map(usize::to_string).collect()),
        ColumnCodec::RawString => Some(tokens.iter().map(|t| unquote(t)).collect()),
        ColumnCodec::Json => Some(tokens.to_vec()),
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
    let n_cells = codecs
        .iter()
        .filter(|c| !matches!(c, DecodedCodec::Const(_)))
        .count();

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
            let value = match codec {
                DecodedCodec::Const(v) => v.clone(),
                DecodedCodec::Dict(values) => {
                    let idx: usize = cell_iter.next()?.parse().ok()?;
                    values.get(idx)?.clone()
                }
                DecodedCodec::RawString => Value::String(cell_iter.next()?.to_string()),
                DecodedCodec::Json => serde_json::from_str(cell_iter.next()?).ok()?,
            };
            obj.insert(key.clone(), value);
        }
        items.push(Value::Object(obj));
    }
    if lines.next().is_some() {
        return None;
    }
    Some(Value::Array(items))
}

enum DecodedCodec {
    Const(Value),
    Dict(Vec<Value>),
    RawString,
    Json,
}

fn parse_codec(header: &str) -> Option<DecodedCodec> {
    let mut fields = header.split('\t');
    match fields.next()? {
        "const" => Some(DecodedCodec::Const(
            serde_json::from_str(fields.next()?).ok()?,
        )),
        "dict" => {
            let n: usize = fields.next()?.parse().ok()?;
            let mut values = Vec::with_capacity(n);
            for _ in 0..n {
                values.push(serde_json::from_str(fields.next()?).ok()?);
            }
            if fields.next().is_some() {
                return None;
            }
            Some(DecodedCodec::Dict(values))
        }
        "str" => Some(DecodedCodec::RawString),
        "json" => Some(DecodedCodec::Json),
        _ => None,
    }
}

fn unquote(token: &str) -> String {
    serde_json::from_str::<String>(token).unwrap_or_else(|_| token.to_string())
}

fn is_scalar(value: &Value) -> bool {
    !value.is_object() && !value.is_array()
}

fn scalar_token(value: &Value) -> Option<String> {
    serde_json::to_string(value).ok()
}

fn string_token(s: &str) -> String {
    serde_json::to_string(s).expect("string serializes")
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
    fn low_cardinality_column_uses_a_dictionary() {
        round_trip(r#"[{"s":"ok"},{"s":"FAIL"},{"s":"ok"},{"s":"ok"},{"s":"FAIL"},{"s":"ok"}]"#);
    }

    #[test]
    fn high_cardinality_strings_drop_quotes() {
        round_trip(r#"[{"p":"/srv/app_0/x"},{"p":"/srv/app_1/y"},{"p":"/srv/app_2/z"}]"#);
    }

    #[test]
    fn constant_column_stored_once() {
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
}
