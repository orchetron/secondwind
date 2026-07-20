use std::collections::HashMap;
use std::sync::OnceLock;

use crate::relevance::Embedder;

// Distilled static sentence embedder: transformer output vectors baked into a lookup table
// (all-MiniLM-L6-v2 via model2vec, Apache 2.0, see data/NOTICE.md). Wordpiece + mean pool, no
// runtime model: thin, deterministic, offline, and ranks far better than co-occurrence vectors.
pub const DIM: usize = 256;

const VOCAB: &str = include_str!("../data/static_vocab.txt");
const TABLE: &[u8] = include_bytes!("../data/static_vectors.bin");
const ROW: usize = 4 + DIM; // f32 scale + DIM int8

struct Model {
    ids: HashMap<String, u32>,
    unk: u32,
    count: usize,
}

static MODEL: OnceLock<Model> = OnceLock::new();

fn model() -> &'static Model {
    MODEL.get_or_init(|| {
        let mut ids = HashMap::new();
        for (i, token) in VOCAB.lines().enumerate() {
            if !token.is_empty() {
                ids.insert(token.to_string(), i as u32);
            }
        }
        let count = if TABLE.len() >= 8 {
            u32::from_le_bytes(TABLE[0..4].try_into().unwrap()) as usize
        } else {
            0
        };
        let unk = ids.get("[UNK]").copied().unwrap_or(0);
        Model { ids, unk, count }
    })
}

fn vector(id: u32, into: &mut [f32; DIM]) {
    let id = id as usize;
    let base = 8 + id * ROW;
    if id >= model().count || base + ROW > TABLE.len() {
        into.fill(0.0);
        return;
    }
    let scale = f32::from_le_bytes(TABLE[base..base + 4].try_into().unwrap());
    for (d, slot) in into.iter_mut().enumerate() {
        *slot = TABLE[base + 4 + d] as i8 as f32 * scale;
    }
}

// Mean-pooled, unit-normalized sentence vector; empty input maps to the zero vector (ranks last).
fn encode(text: &str) -> [f32; DIM] {
    let ids = tokenize(text);
    if ids.is_empty() {
        return [0.0; DIM];
    }
    let mut sum = [0.0f32; DIM];
    let mut row = [0.0f32; DIM];
    for id in &ids {
        vector(*id, &mut row);
        for (s, r) in sum.iter_mut().zip(&row) {
            *s += r;
        }
    }
    let inv = 1.0 / ids.len() as f32;
    for s in &mut sum {
        *s *= inv;
    }
    let norm = sum.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for s in &mut sum {
            *s /= norm;
        }
    }
    sum
}

// BERT-uncased wordpiece: lowercase, split punctuation, greedy longest subword with ## continuation,
// [UNK] for anything uncovered. No accent stripping (corpora are ascii; accented words hit [UNK]).
fn tokenize(text: &str) -> Vec<u32> {
    let model = model();
    let mut ids = Vec::new();
    for word in basic_tokens(text) {
        wordpiece(&word, model, &mut ids);
    }
    ids
}

fn basic_tokens(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for raw in text.chars() {
        for c in raw.to_lowercase() {
            if c.is_whitespace() {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            } else if is_punct(c) {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
                out.push(c.to_string());
            } else {
                cur.push(c);
            }
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn is_punct(c: char) -> bool {
    c.is_ascii_punctuation() || (!c.is_alphanumeric() && !c.is_whitespace())
}

fn wordpiece(word: &str, model: &Model, out: &mut Vec<u32>) {
    let chars: Vec<char> = word.chars().collect();
    if chars.len() > 100 {
        out.push(model.unk);
        return;
    }
    let mut pieces = Vec::new();
    let mut start = 0;
    while start < chars.len() {
        let mut end = chars.len();
        let mut matched = None;
        while start < end {
            let mut sub: String = chars[start..end].iter().collect();
            if start > 0 {
                sub = format!("##{sub}");
            }
            if let Some(&id) = model.ids.get(&sub) {
                matched = Some(id);
                break;
            }
            end -= 1;
        }
        match matched {
            Some(id) => {
                pieces.push(id);
                start = end;
            }
            None => {
                out.push(model.unk);
                return;
            }
        }
    }
    out.extend(pieces);
}

// Ranks alone (dominant): its cosine ordering is the relevance, not the lexical base.
pub struct DistilledEmbedder;

impl Embedder for DistilledEmbedder {
    fn dominant(&self) -> bool {
        true
    }

    fn semantic(&self, query: &str, rows: &[&str]) -> Vec<f64> {
        let q = encode(query);
        rows.iter()
            .map(|row| {
                let r = encode(row);
                q.iter().zip(&r).map(|(a, b)| a * b).sum::<f32>() as f64
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cos(a: &str, b: &str) -> f32 {
        let (x, y) = (encode(a), encode(b));
        x.iter().zip(&y).map(|(p, q)| p * q).sum()
    }

    #[test]
    fn distilled_semantics_rank_a_synonym_over_an_unrelated_row() {
        // no shared word; only the distilled vectors connect them
        assert!(
            cos("cancel the purchase", "the order was refunded")
                > cos("cancel the purchase", "the weather is sunny")
        );
    }

    #[test]
    fn embedder_scores_the_relevant_row_highest() {
        let scores = DistilledEmbedder.semantic(
            "database connection errors",
            &[
                "the swimming pool was drained",
                "could not connect to the database",
                "a report finished quickly",
            ],
        );
        let best = scores
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .unwrap()
            .0;
        assert_eq!(best, 1);
    }
}
