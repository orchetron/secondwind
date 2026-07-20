use std::collections::HashSet;

use secondwind_optimize::distilled::DistilledEmbedder;
use secondwind_optimize::relevance::{rank, rank_lexical};
use serde_json::Value;

struct Triple {
    query: String,
    rows: Vec<String>,
    relevant: HashSet<usize>,
}

fn corpus() -> Vec<Triple> {
    parse(include_str!("relevance_corpus.jsonl"))
}

fn hard_corpus() -> Vec<Triple> {
    parse(include_str!("../../../bench/relevance/hard_corpus.jsonl"))
}

fn parse(raw: &str) -> Vec<Triple> {
    raw.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|line| {
            let v: Value = serde_json::from_str(line).expect("corpus line parses");
            Triple {
                query: v["query"].as_str().unwrap().to_string(),
                rows: v["rows"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|r| r.as_str().unwrap().to_string())
                    .collect(),
                relevant: v["relevant"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|i| i.as_u64().unwrap() as usize)
                    .collect(),
            }
        })
        .collect()
}

// Row indices best-first by score, ties broken by original order.
fn ranking(scores: &[f64]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..scores.len()).collect();
    order.sort_by(|&a, &b| scores[b].partial_cmp(&scores[a]).unwrap().then(a.cmp(&b)));
    order
}

#[derive(Default)]
struct Metrics {
    mrr: f64,
    p_at_1: f64,
    p_at_3: f64,
    recall_at_3: f64,
    n: f64,
}

impl Metrics {
    fn add(&mut self, ranked: &[usize], relevant: &HashSet<usize>) {
        let rr = ranked
            .iter()
            .position(|i| relevant.contains(i))
            .map(|p| 1.0 / (p + 1) as f64)
            .unwrap_or(0.0);
        let hit_at = |k: usize| {
            ranked
                .iter()
                .take(k)
                .filter(|i| relevant.contains(i))
                .count()
        };
        self.mrr += rr;
        self.p_at_1 += hit_at(1) as f64;
        self.p_at_3 += hit_at(3) as f64 / 3.0;
        self.recall_at_3 += hit_at(3) as f64 / relevant.len() as f64;
        self.n += 1.0;
    }
    fn line(&self, name: &str) -> String {
        format!(
            "{name:8}  MRR {:.3}   P@1 {:.3}   P@3 {:.3}   R@3 {:.3}",
            self.mrr / self.n,
            self.p_at_1 / self.n,
            self.p_at_3 / self.n,
            self.recall_at_3 / self.n,
        )
    }
}

#[test]
fn measures_relevance_quality_against_a_lexical_baseline() {
    let corpus = corpus();
    let mut full = Metrics::default();
    let mut lexical = Metrics::default();
    for t in &corpus {
        let rows: Vec<&str> = t.rows.iter().map(String::as_str).collect();
        full.add(
            &ranking(&rank(&rows, &t.query, &DistilledEmbedder)),
            &t.relevant,
        );
        lexical.add(&ranking(&rank_lexical(&rows, &t.query)), &t.relevant);
    }

    eprintln!("relevance benchmark over {} labeled queries", corpus.len());
    eprintln!("  {}", lexical.line("lexical"));
    eprintln!("  {}", full.line("distilled"));

    let full_mrr = full.mrr / full.n;
    let lexical_mrr = lexical.mrr / lexical.n;
    assert!(
        full_mrr > lexical_mrr,
        "the semantic stack ({full_mrr:.3}) must beat lexical ({lexical_mrr:.3})"
    );
    assert!(
        full.recall_at_3 / full.n > lexical.recall_at_3 / lexical.n,
        "the semantic stack must recall more relevant rows in the top 3"
    );
    assert!(full_mrr >= 0.8, "full stack MRR {full_mrr:.3} below floor");
}

#[test]
fn reports_the_static_default_on_the_hard_corpus() {
    let corpus = hard_corpus();
    let mut distilled = Metrics::default();
    let mut lexical = Metrics::default();
    for t in &corpus {
        let rows: Vec<&str> = t.rows.iter().map(String::as_str).collect();
        distilled.add(
            &ranking(&rank(&rows, &t.query, &DistilledEmbedder)),
            &t.relevant,
        );
        lexical.add(&ranking(&rank_lexical(&rows, &t.query)), &t.relevant);
    }
    eprintln!("hard corpus: {} labeled queries", corpus.len());
    eprintln!("  {}", lexical.line("lexical"));
    eprintln!("  {}", distilled.line("distilled"));
    assert!(distilled.mrr / distilled.n > lexical.mrr / lexical.n);
}
