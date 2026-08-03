use secondwind_optimize::atom::canonicalize;
use secondwind_optimize::clmh::Clmh;
use secondwind_optimize::columnar::{Columnar, decode};
use secondwind_optimize::offload::Store;
use secondwind_optimize::transform::Transform;
use serde_json::Value;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

fn scalar_token(rng: &mut Rng) -> String {
    match rng.below(7) {
        0 => rng.next().to_string(),
        1 => format!("-{}", rng.below(1_000_000)),
        2 => format!("{}.{}0", rng.below(1000), rng.below(100)),
        3 => "true".into(),
        4 => "null".into(),
        5 => format!("{}e{}", rng.below(100), rng.below(30)),
        _ => {
            let tricky = [
                "a\tb",
                "c\nd",
                "e\"f",
                "g\\h",
                "un\u{00e9}",
                "\u{4f60}\u{597d}",
                "",
            ];
            let pick = tricky[rng.below(tricky.len() as u64) as usize];
            serde_json::to_string(pick).unwrap()
        }
    }
}

fn uniform_array(rng: &mut Rng) -> String {
    let n = 2 + rng.below(6);
    let k = 1 + rng.below(5);
    let keys: Vec<String> = (0..k).map(|i| format!("key_{i}")).collect();
    let rows: Vec<String> = (0..n)
        .map(|_| {
            let cells: Vec<String> = keys
                .iter()
                .map(|key| {
                    format!(
                        "{}:{}",
                        serde_json::to_string(key).unwrap(),
                        scalar_token(rng)
                    )
                })
                .collect();
            format!("{{{}}}", cells.join(","))
        })
        .collect();
    format!("[{}]", rows.join(","))
}

fn flat_object(rng: &mut Rng) -> String {
    let n = 30 + rng.below(120);
    let cells: Vec<String> = (0..n)
        .map(|i| format!(r#""k{i}":{}"#, scalar_token(rng)))
        .collect();
    format!("{{{}}}", cells.join(","))
}

#[test]
fn columnar_round_trips_or_refuses_on_random_uniform_arrays() {
    let mut rng = Rng(0x2545_f491_4f6c_dd1d);
    for _ in 0..4000 {
        let raw = uniform_array(&mut rng);
        let value: Value = match serde_json::from_str(&raw) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if let Some(encoded) = Columnar::default().try_encode(&value) {
            let decoded = decode(&encoded.wire).expect("wire decodes");
            assert_eq!(
                canonicalize(&decoded),
                canonicalize(&value),
                "columnar broke byte-exactness on: {raw}"
            );
            assert_eq!(
                Clmh::of(&secondwind_optimize::atom::leaves(&decoded)),
                Clmh::of(&secondwind_optimize::atom::leaves(&value))
            );
        }
    }
}

fn log_block(rng: &mut Rng) -> String {
    let n = 2 + rng.below(60);
    let lines: Vec<String> = (0..n)
        .map(|_| {
            let words = [
                "worker", "GET", "job", "/srv/app", "status", "done", "err\ttab",
            ];
            let w = words[rng.below(words.len() as u64) as usize];
            format!(
                "{w} {} took {}ms code {}",
                rng.below(9999),
                rng.below(500),
                rng.below(600)
            )
        })
        .collect();
    lines.join("\n")
}

#[test]
fn log_templating_round_trips_byte_exact() {
    let mut rng = Rng(0xdead_beef_cafe_0001);
    for _ in 0..3000 {
        let raw = log_block(&mut rng);
        if let Some(out) = secondwind_optimize::log::try_template(&raw) {
            assert_eq!(
                secondwind_optimize::log::decode(&out.wire).as_deref(),
                Some(raw.as_str()),
                "log templating broke byte-exactness on: {raw:?}"
            );
        }
    }
}

fn grep_block(rng: &mut Rng) -> String {
    let paths = [
        "src/a.rs",
        "src/b.rs",
        "crates/x/mod.rs",
        "README.md",
        "deep/nested/path.ts",
    ];
    let snippets = [
        "// TODO fix this",
        "let url = \"http://h:8080\";",
        "fn handler() {}",
        "return ok",
        "matched: value",
        "",
    ];
    let n = 1 + rng.below(40);
    let lines: Vec<String> = (0..n)
        .map(|_| {
            let path = paths[rng.below(paths.len() as u64) as usize];
            let snip = snippets[rng.below(snippets.len() as u64) as usize];
            match rng.below(3) {
                0 => format!("{path}:{}:{snip}", rng.below(9999)),
                1 => format!("{path}:{snip}"),
                _ => format!("some prose without a path {}", rng.below(100)),
            }
        })
        .collect();
    lines.join("\n")
}

#[test]
fn search_factoring_round_trips_byte_exact_or_refuses() {
    let mut rng = Rng(0x1234_5678_9abc_def0);
    for _ in 0..6000 {
        let raw = grep_block(&mut rng);
        if let Some(out) = secondwind_optimize::search::try_factor(&raw, &|s: &str| s.len()) {
            assert_eq!(
                secondwind_optimize::search::decode(&out.wire).as_deref(),
                Some(raw.as_str()),
                "search factoring broke byte-exactness on: {raw:?}"
            );
            assert!(
                out.wire.len() < raw.len(),
                "search factoring must save bytes: {raw:?}"
            );
        }
    }
}

#[test]
fn offload_resolves_to_exact_bytes_on_random_objects() {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    for _ in 0..2000 {
        let raw = flat_object(&mut rng);
        let value: Value = match serde_json::from_str(&raw) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let _ = &value;
        let store = Store::default();
        if let Ok(out) = store.offload(&raw) {
            assert!(store.covers(&out.marker, &raw));
            assert_eq!(store.resolve(&out.marker).as_deref(), Some(raw.as_str()));
        }
    }
}

fn leaf_value(rng: &mut Rng) -> Value {
    serde_json::from_str(&scalar_token(rng)).unwrap_or(Value::Null)
}

// Keys are drawn from a small shared pool so records overlap partially, exercising presence masks,
// nullable containers, ragged arrays, and mixed types at a path.
fn nested_value(rng: &mut Rng, depth: u64) -> Value {
    if depth == 0 {
        return leaf_value(rng);
    }
    match rng.below(6) {
        0 | 1 => {
            let pool = ["a", "b", "c", "user", "id", "labels"];
            let count = rng.below(pool.len() as u64 + 1);
            let mut map = serde_json::Map::new();
            for i in 0..count as usize {
                let key = pool[(i + rng.below(pool.len() as u64) as usize) % pool.len()];
                map.insert(key.to_string(), nested_value(rng, depth - 1));
            }
            Value::Object(map)
        }
        2 => {
            let len = rng.below(4);
            Value::Array((0..len).map(|_| nested_value(rng, depth - 1)).collect())
        }
        3 => Value::Null,
        _ => leaf_value(rng),
    }
}

#[test]
fn recordcol_and_kv_round_trip_or_abstain_and_never_panic() {
    use secondwind_optimize::{kv, recordcol};
    let mut rng = Rng(0x5ec0_11ab_7e57_0001);
    let words = [
        "commit",
        "Author:",
        "Date:",
        "",
        "    msg",
        "KEY=v",
        "a=b=c",
        "no pair",
        "x",
        "caf\u{00e9}=1",
    ];
    let bytes = |s: &str| s.len();
    for _ in 0..6000 {
        let n = rng.below(30);
        let lines: Vec<&str> = (0..n)
            .map(|_| words[rng.below(words.len() as u64) as usize])
            .collect();
        let raw = lines.join("\n");
        if let Some(e) = recordcol::try_encode(&raw, &bytes) {
            assert_eq!(
                recordcol::decode(&e.wire).as_deref(),
                Some(raw.as_str()),
                "recordcol {raw:?}"
            );
        }
        if let Some(e) = kv::try_encode(&raw, &bytes) {
            assert_eq!(
                kv::decode(&e.wire).as_deref(),
                Some(raw.as_str()),
                "kv {raw:?}"
            );
        }
        // The portable verify path feeds arbitrary text to every decoder: it must never panic.
        let _ = recordcol::decode(&raw);
        let _ = kv::decode(&raw);
    }
}

#[test]
fn frontlines_round_trips_random_line_lists() {
    use secondwind_optimize::frontlines::{decode, encode};
    let mut rng = Rng(0x0ff1_cede_adbe_ef01);
    let segs = [
        "crates",
        "src",
        "lib.rs",
        "mod",
        "a",
        "caf\u{00e9}",
        "",
        "  sp",
        "x.y",
    ];
    for _ in 0..6000 {
        let n = 1 + rng.below(20);
        let lines: Vec<String> = (0..n)
            .map(|_| {
                let depth = rng.below(5);
                (0..depth)
                    .map(|_| segs[rng.below(segs.len() as u64) as usize])
                    .collect::<Vec<_>>()
                    .join("/")
            })
            .collect();
        let raw = lines.join("\n");
        if let Some(wire) = encode(&raw) {
            assert_eq!(
                decode(&wire).as_deref(),
                Some(raw.as_str()),
                "frontlines broke on {raw:?}"
            );
        }
    }
}

#[test]
fn shred_round_trips_or_refuses_on_random_nested_arrays() {
    use secondwind_optimize::shred::{Shred, decode as shred_decode};
    let mut rng = Rng(0x51ed_09b1_c33f_2a77);
    for _ in 0..8000 {
        let n = 2 + rng.below(8);
        let items: Vec<Value> = (0..n)
            .map(|_| {
                let depth = 1 + rng.below(4);
                nested_value(&mut rng, depth)
            })
            .collect();
        // Half the runs wrap the array in an object to exercise the single-value top-level path.
        let value = if rng.below(2) == 0 {
            Value::Array(items)
        } else {
            let mut map = serde_json::Map::new();
            map.insert("meta".into(), nested_value(&mut rng, 2));
            map.insert("rows".into(), Value::Array(items));
            Value::Object(map)
        };
        if let Some(encoded) = Shred::default().try_encode(&value) {
            let decoded = shred_decode(&encoded.wire).expect("wire decodes");
            assert_eq!(
                canonicalize(&decoded),
                canonicalize(&value),
                "shred broke byte-exactness on: {value}"
            );
            assert_eq!(
                Clmh::of(&secondwind_optimize::atom::leaves(&decoded)),
                Clmh::of(&secondwind_optimize::atom::leaves(&value))
            );
        }
    }
}

#[test]
fn grouped_text_is_byte_exact_or_declines() {
    use secondwind_optimize::grouped::{decode, encode};
    let mut rng = Rng(0x6720_7570_6564_0001);
    let dirs = ["src/a/", "src/b/", "lib/", ""];
    let files = ["x.rs", "y.rs", "mod.rs", "a.txt"];
    for _ in 0..4000 {
        let n = 2 + rng.below(10);
        let mut lines = Vec::new();
        for _ in 0..n {
            let kind = rng.below(3);
            let d = dirs[rng.below(4) as usize];
            let f = files[rng.below(4) as usize];
            let line = match kind {
                0 => {
                    let ln = rng.below(500);
                    format!("{d}{f}:{ln}:some content: with colons")
                }
                1 => format!("{d}{f}"),
                _ => {
                    let x = rng.below(100);
                    format!("random {x} text")
                }
            };
            lines.push(line);
        }
        let mut raw = lines.join("\n");
        if rng.below(2) == 0 {
            raw.push('\n');
        }
        if let Some(wire) = encode(&raw) {
            assert_eq!(
                decode(&wire).as_deref(),
                Some(raw.as_str()),
                "grouped broke on:\n{raw}"
            );
        }
    }
}

#[test]
fn norm_round_trips_or_refuses_on_random_records_with_collections() {
    use secondwind_optimize::norm::{decode as norm_decode, encode as norm_encode};
    let mut rng = Rng(0x1234_5678_9abc_def0);
    for _ in 0..4000 {
        let n = 2 + rng.below(6);
        let mut items = Vec::new();
        for i in 0..n {
            let mut r = serde_json::Map::new();
            r.insert("id".into(), Value::String(format!("id{i}")));
            let nd = rng.below(4);
            let mut deps = Vec::new();
            for k in 0..nd {
                let req = rng.below(10);
                deps.push(serde_json::json!({"name": format!("d{k}"), "req": req}));
            }
            if !deps.is_empty() {
                r.insert("deps".into(), Value::Array(deps));
            }
            let nm = rng.below(6);
            let mut m = serde_json::Map::new();
            for j in 0..nm {
                let val = rng.below(100);
                m.insert(format!("k{j}"), Value::from(val));
            }
            if m.len() >= 4 {
                r.insert("map".into(), Value::Object(m));
            }
            items.push(Value::Object(r));
        }
        if let Some(wire) = norm_encode(&items) {
            assert_eq!(
                canonicalize(&norm_decode(&wire).unwrap()),
                canonicalize(&Value::Array(items.clone())),
                "norm broke byte-exactness"
            );
        }
    }
}

#[test]
fn doc_round_trips_or_refuses_on_random_documents() {
    use secondwind_optimize::doc::{Doc, decode as doc_decode};
    let mut rng = Rng(0xd0c_5eed_1234_abcd);
    for _ in 0..4000 {
        let mut obj = serde_json::Map::new();
        obj.insert(
            "name".into(),
            Value::String(format!("pkg-{}", rng.below(50))),
        );
        let mut m = serde_json::Map::new();
        let entries = 2 + rng.below(10);
        for i in 0..entries {
            let depth = 1 + rng.below(2);
            m.insert(format!("1.0.{i}"), nested_value(&mut rng, depth));
        }
        obj.insert("collection".into(), Value::Object(m));
        if rng.below(2) == 0 {
            let arr: Vec<Value> = (0..2 + rng.below(6))
                .map(|_| nested_value(&mut rng, 1))
                .collect();
            obj.insert("list".into(), Value::Array(arr));
        }
        let value = Value::Object(obj);
        if let Some(enc) = Doc::default().try_encode(&value) {
            assert_eq!(
                canonicalize(&doc_decode(&enc.wire).unwrap()),
                canonicalize(&value),
                "doc broke byte-exactness on: {value}"
            );
        }
    }
}

#[test]
fn nested_round_trips_or_refuses_on_random_object_arrays() {
    use secondwind_optimize::nested::{Nested, decode as nested_decode};
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    for _ in 0..8000 {
        let n = 2 + rng.below(6);
        let items: Vec<Value> = (0..n)
            .map(|_| {
                let mut map = serde_json::Map::new();
                let fields = 1 + rng.below(4);
                for _ in 0..fields {
                    // Overlapping keys across records exercise the ragged union and absent cells.
                    let key = format!("k{}", rng.below(5));
                    let depth = 1 + rng.below(2);
                    map.insert(key, nested_value(&mut rng, depth));
                }
                Value::Object(map)
            })
            .collect();
        let value = Value::Array(items);
        if let Some(encoded) = Nested::default().try_encode(&value) {
            let decoded = nested_decode(&encoded.wire).expect("wire decodes");
            assert_eq!(
                canonicalize(&decoded),
                canonicalize(&value),
                "nested broke byte-exactness on: {value}"
            );
        }
    }
}
