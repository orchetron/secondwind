use std::collections::HashMap;

use serde_json::Value;

use crate::tokens::TokenCounter;

// Per-column value codec shared by the flat transpose (columnar) and the recursive transpose (shred):
// one column of scalar tokens is stored as a constant, a dictionary of distinct values, raw unquoted
// strings, or verbatim json, whichever the counter prices cheapest.
pub(crate) enum ColumnCodec {
    Const(String),
    Dict {
        values: Vec<String>,
        index: Vec<usize>,
    },
    RawString,
    Affix {
        prefix: String,
        suffix: String,
    },
    Json,
}

pub(crate) fn choose_codec(
    tokens: &[String],
    is_string: &[bool],
    counter: &dyn TokenCounter,
    readable: bool,
) -> ColumnCodec {
    // Inline wire must stay model-readable: every value literal in its own cell, never a fragment
    // (affix), index (dict), or hoisted constant the model would have to reconstruct.
    if readable {
        let string_safe = is_string.iter().all(|s| *s)
            && tokens
                .iter()
                .all(|t| !unquote(t).contains('\t') && !unquote(t).contains('\n'));
        return if string_safe {
            ColumnCodec::RawString
        } else {
            ColumnCodec::Json
        };
    }
    if tokens.iter().all(|t| t == &tokens[0]) {
        return ColumnCodec::Const(tokens[0].clone());
    }

    let mut order: Vec<String> = Vec::new();
    let mut seen: HashMap<&str, usize> = HashMap::new();
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
        let unquoted: Vec<String> = tokens.iter().map(|t| unquote(t)).collect();
        let (prefix, suffix) = common_affix(&unquoted);
        if !prefix.is_empty() || !suffix.is_empty() {
            candidates.push(ColumnCodec::Affix { prefix, suffix });
        }
    }
    candidates
        .into_iter()
        .min_by_key(|codec| column_cost(codec, tokens, counter))
        .expect("at least the json candidate is present")
}

// Longest prefix and suffix shared by every cell, char-aligned so no cell is sliced mid-codepoint
// and capped so the two never overlap in the shortest cell.
fn common_affix(values: &[String]) -> (String, String) {
    if values.is_empty() {
        return (String::new(), String::new());
    }
    let first = values[0].as_bytes();
    let mut plen = first.len();
    for v in &values[1..] {
        plen = first
            .iter()
            .zip(v.as_bytes())
            .take(plen)
            .take_while(|(a, b)| a == b)
            .count();
    }
    while plen > 0 && !values[0].is_char_boundary(plen) {
        plen -= 1;
    }
    let mut slen = first.len();
    for v in values {
        let b = v.as_bytes();
        slen = slen.min(b.len() - plen);
        slen = (0..slen)
            .take_while(|k| first[first.len() - 1 - k] == b[b.len() - 1 - k])
            .count();
    }
    while slen > 0 && !values[0].is_char_boundary(first.len() - slen) {
        slen -= 1;
    }
    (
        values[0][..plen].to_string(),
        values[0][first.len() - slen..].to_string(),
    )
}

pub(crate) fn column_cost(
    codec: &ColumnCodec,
    tokens: &[String],
    counter: &dyn TokenCounter,
) -> usize {
    let mut cost = counter.count(&codec_header(codec));
    if let Some(cells) = render_cells(codec, tokens) {
        cost += cells.iter().map(|cell| counter.count(cell)).sum::<usize>();
    }
    cost
}

pub(crate) fn codec_header(codec: &ColumnCodec) -> String {
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
        ColumnCodec::Affix { prefix, suffix } => {
            format!("affix\t{}\t{}", string_token(prefix), string_token(suffix))
        }
        ColumnCodec::Json => "json".into(),
    }
}

pub(crate) fn render_cells(codec: &ColumnCodec, tokens: &[String]) -> Option<Vec<String>> {
    match codec {
        ColumnCodec::Const(_) => None,
        ColumnCodec::Dict { index, .. } => Some(index.iter().map(usize::to_string).collect()),
        ColumnCodec::RawString => Some(tokens.iter().map(|t| unquote(t)).collect()),
        ColumnCodec::Affix { prefix, suffix } => Some(
            tokens
                .iter()
                .map(|t| {
                    let v = unquote(t);
                    v[prefix.len()..v.len() - suffix.len()].to_string()
                })
                .collect(),
        ),
        ColumnCodec::Json => Some(tokens.to_vec()),
    }
}

pub(crate) enum DecodedCodec {
    Const(Value),
    Dict(Vec<Value>),
    RawString,
    Affix { prefix: String, suffix: String },
    Json,
}

pub(crate) fn parse_codec(header: &str) -> Option<DecodedCodec> {
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
        "affix" => {
            let prefix: String = serde_json::from_str(fields.next()?).ok()?;
            let suffix: String = serde_json::from_str(fields.next()?).ok()?;
            if fields.next().is_some() {
                return None;
            }
            Some(DecodedCodec::Affix { prefix, suffix })
        }
        "json" => Some(DecodedCodec::Json),
        _ => None,
    }
}

// Const carries its value in the header and consumes no per-row cell; every other codec reads one.
pub(crate) fn produces_cells(codec: &DecodedCodec) -> bool {
    !matches!(codec, DecodedCodec::Const(_))
}

pub(crate) fn decode_cell(codec: &DecodedCodec, cell: Option<&str>) -> Option<Value> {
    Some(match codec {
        DecodedCodec::Const(v) => v.clone(),
        DecodedCodec::Dict(values) => {
            let idx: usize = cell?.parse().ok()?;
            values.get(idx)?.clone()
        }
        DecodedCodec::RawString => Value::String(cell?.to_string()),
        DecodedCodec::Affix { prefix, suffix } => {
            Value::String(format!("{prefix}{}{suffix}", cell?))
        }
        DecodedCodec::Json => serde_json::from_str(cell?).ok()?,
    })
}

pub(crate) fn is_scalar(value: &Value) -> bool {
    !value.is_object() && !value.is_array()
}

pub(crate) fn scalar_token(value: &Value) -> Option<String> {
    serde_json::to_string(value).ok()
}

pub(crate) fn string_token(s: &str) -> String {
    serde_json::to_string(s).expect("string serializes")
}

pub(crate) fn unquote(token: &str) -> String {
    serde_json::from_str::<String>(token).unwrap_or_else(|_| token.to_string())
}
