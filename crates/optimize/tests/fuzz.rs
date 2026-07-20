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
        if let Some(out) = secondwind_optimize::search::try_factor(&raw) {
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
