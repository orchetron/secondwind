use std::collections::{HashMap, HashSet};

const K1: f64 = 1.5;
const B: f64 = 0.75;
const MIN_TERM: usize = 3;

// Pseudo-relevance feedback: mine expansion terms from the top-ranked rows, so a row related by
// shared vocabulary (not the query's words) still surfaces. Adapts to this block, not a global embedding.
const PRF_ROWS: usize = 3;
const PRF_TERMS: usize = 8;
const PRF_WEIGHT: f64 = 0.4;

// Char-trigram overlap catches morphological variants (token/tokens) BM25 misses. Thresholded so
// an unrelated row scores zero and never dilutes the kept set.
const TRIGRAM_WEIGHT: f64 = 0.6;
const TRIGRAM_MIN: f64 = 0.34;

// Pluggable per-row semantic scores. A dominant embedder ranks alone; a non-dominant one adds to
// the lexical base. Swappable by the caller.
pub trait Embedder: Send + Sync {
    fn semantic(&self, query: &str, rows: &[&str]) -> Vec<f64>;
    fn dominant(&self) -> bool {
        false
    }
}

// Lexical only (no vectors): the non-dominant baseline the additive path runs on.
pub struct StaticEmbedder;

impl Embedder for StaticEmbedder {
    fn semantic(&self, _query: &str, rows: &[&str]) -> Vec<f64> {
        vec![0.0; rows.len()]
    }
}

// Per-row relevance: BM25 over query + feedback terms, a trigram boost, plus the embedder's score.
pub fn rank(rows: &[&str], query: &str, embedder: &dyn Embedder) -> Vec<f64> {
    let q_terms: Vec<String> = unique(tokens(query).collect());
    if rows.is_empty() || q_terms.is_empty() {
        return vec![0.0; rows.len()];
    }
    if embedder.dominant() {
        return embedder.semantic(query, rows);
    }

    let row_tokens: Vec<Vec<String>> = rows.iter().map(|r| tokens(r).collect()).collect();
    let lengths: Vec<f64> = row_tokens.iter().map(|t| t.len() as f64).collect();
    let avg_len = (lengths.iter().sum::<f64>() / rows.len() as f64).max(1.0);

    let mut terms: Vec<(String, f64)> = q_terms.iter().map(|t| (t.clone(), 1.0)).collect();
    let base = bm25(&row_tokens, &lengths, avg_len, &terms);
    terms.extend(feedback(&base, &row_tokens, &q_terms));
    let mut scores = bm25(&row_tokens, &lengths, avg_len, &terms);

    let query_trigrams = trigrams(&q_terms);
    if !query_trigrams.is_empty() {
        for (score, tokens_row) in scores.iter_mut().zip(&row_tokens) {
            let row_trigrams = trigrams(tokens_row);
            let covered = query_trigrams.intersection(&row_trigrams).count() as f64
                / query_trigrams.len() as f64;
            if covered >= TRIGRAM_MIN {
                *score += TRIGRAM_WEIGHT * covered;
            }
        }
    }

    for (score, sem) in scores.iter_mut().zip(embedder.semantic(query, rows)) {
        *score += sem;
    }
    scores
}

// BM25 over the query terms alone: the baseline the full ranker is measured against.
pub fn rank_lexical(rows: &[&str], query: &str) -> Vec<f64> {
    let q_terms: Vec<String> = unique(tokens(query).collect());
    if rows.is_empty() || q_terms.is_empty() {
        return vec![0.0; rows.len()];
    }
    let row_tokens: Vec<Vec<String>> = rows.iter().map(|r| tokens(r).collect()).collect();
    let lengths: Vec<f64> = row_tokens.iter().map(|t| t.len() as f64).collect();
    let avg_len = (lengths.iter().sum::<f64>() / rows.len() as f64).max(1.0);
    let terms: Vec<(String, f64)> = q_terms.into_iter().map(|t| (t, 1.0)).collect();
    bm25(&row_tokens, &lengths, avg_len, &terms)
}

fn bm25(
    row_tokens: &[Vec<String>],
    lengths: &[f64],
    avg_len: f64,
    terms: &[(String, f64)],
) -> Vec<f64> {
    let n = row_tokens.len() as f64;
    let idf: HashMap<&str, f64> = terms
        .iter()
        .map(|(term, _)| {
            let df = row_tokens.iter().filter(|t| t.contains(term)).count() as f64;
            (term.as_str(), ((n - df + 0.5) / (df + 0.5) + 1.0).ln())
        })
        .collect();

    row_tokens
        .iter()
        .zip(lengths)
        .map(|(row, &len)| {
            let mut freq: HashMap<&str, usize> = HashMap::new();
            for tok in row {
                *freq.entry(tok.as_str()).or_insert(0) += 1;
            }
            let mut score = 0.0;
            for (term, weight) in terms {
                let tf = *freq.get(term.as_str()).unwrap_or(&0) as f64;
                if tf == 0.0 {
                    continue;
                }
                let denom = tf + K1 * (1.0 - B + B * len / avg_len);
                score += weight * idf[term.as_str()] * (tf * (K1 + 1.0)) / denom;
            }
            score
        })
        .collect()
}

// Terms recurring in the top rows (minus the query's own), weighted by freq*idf so common words
// drop out and only content-bearing terms expand the query.
fn feedback(base: &[f64], row_tokens: &[Vec<String>], q_terms: &[String]) -> Vec<(String, f64)> {
    let query: HashSet<&str> = q_terms.iter().map(String::as_str).collect();
    let mut top: Vec<usize> = (0..base.len()).filter(|&i| base[i] > 0.0).collect();
    top.sort_by(|&a, &b| base[b].partial_cmp(&base[a]).unwrap());
    top.truncate(PRF_ROWS);
    if top.is_empty() {
        return Vec::new();
    }

    let n = row_tokens.len() as f64;
    let mut tf: HashMap<&str, f64> = HashMap::new();
    for &i in &top {
        for term in &row_tokens[i] {
            if !query.contains(term.as_str()) {
                *tf.entry(term.as_str()).or_insert(0.0) += 1.0;
            }
        }
    }
    let mut candidates: Vec<(&str, f64)> = tf
        .into_iter()
        .map(|(term, f)| {
            let df = row_tokens
                .iter()
                .filter(|t| t.iter().any(|x| x == term))
                .count() as f64;
            let idf = ((n - df + 0.5) / (df + 0.5) + 1.0).ln();
            (term, f * idf.max(0.0))
        })
        .filter(|(_, w)| *w > 0.0)
        .collect();
    candidates.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
    candidates.truncate(PRF_TERMS);
    let max = candidates.first().map(|(_, w)| *w).unwrap_or(1.0).max(1e-9);
    candidates
        .into_iter()
        .map(|(term, w)| (term.to_string(), PRF_WEIGHT * w / max))
        .collect()
}

// Keep only rows within this fraction of the top score, so a merely-common term (near-zero idf)
// can't pull unrelated rows into the kept set.
const SELECT_RATIO: f64 = 0.15;

// Indices to keep inline: within SELECT_RATIO of the best, capped at `keep`, in original order.
pub fn select(rows: &[&str], query: &str, keep: usize, embedder: &dyn Embedder) -> Vec<usize> {
    let scores = rank(rows, query, embedder);
    let max = scores.iter().copied().fold(0.0_f64, f64::max);
    if max <= 0.0 {
        return Vec::new();
    }
    let floor = max * SELECT_RATIO;
    let mut ranked: Vec<usize> = (0..rows.len()).filter(|&i| scores[i] >= floor).collect();
    ranked.sort_by(|&a, &b| scores[b].partial_cmp(&scores[a]).unwrap());
    ranked.truncate(keep);
    ranked.sort_unstable();
    ranked
}

fn tokens(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.len() >= MIN_TERM)
        .map(str::to_lowercase)
}

fn unique(mut terms: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    terms.retain(|t| seen.insert(t.clone()));
    terms
}

fn trigrams(terms: &[String]) -> HashSet<[char; 3]> {
    let mut set = HashSet::new();
    for term in terms {
        let chars: Vec<char> = term.chars().collect();
        for window in chars.windows(3) {
            set.insert([window[0], window[1], window[2]]);
        }
    }
    set
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROWS: &[&str] = &[
        "the authentication service validates every request",
        "the payment gateway charges the card on file",
        "authentication tokens expire after one hour",
        "the shipping module computes delivery estimates",
    ];

    #[test]
    fn ranks_rows_about_the_query_higher() {
        let scores = rank(ROWS, "authentication token", &StaticEmbedder);
        assert!(scores[2] > scores[1]);
        assert!(scores[0] > scores[3]);
    }

    #[test]
    fn selects_the_relevant_rows_in_original_order() {
        let kept = select(ROWS, "authentication", 3, &StaticEmbedder);
        assert!(kept.contains(&0));
        assert!(kept.contains(&2));
        assert!(!kept.contains(&1));
        assert!(kept.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn an_empty_query_ranks_everything_zero() {
        assert_eq!(rank(ROWS, "", &StaticEmbedder), vec![0.0; ROWS.len()]);
        assert!(select(ROWS, "", 3, &StaticEmbedder).is_empty());
    }

    #[test]
    fn trigram_matches_a_morphological_variant() {
        // "token" (query) vs "tokens" (row) share no exact token, but the trigram
        // boost still surfaces the row over an unrelated one.
        let rows = ["the tokens were rotated", "shipping labels were printed"];
        let scores = rank(&rows, "token", &StaticEmbedder);
        assert!(scores[0] > 0.0);
        assert!(scores[0] > scores[1]);
    }

    #[test]
    fn feedback_surfaces_a_row_that_shares_no_query_term() {
        // Row 1 carries none of the query's words but co-occurs with the top hit's
        // vocabulary (password), so feedback ranks it above the unrelated row 2.
        let rows = [
            "login failed for user admin due to a bad password",
            "the password was rejected and the account was locked",
            "invoice emailed to the buyer yesterday afternoon",
        ];
        let scores = rank(&rows, "login failure", &StaticEmbedder);
        assert!(scores[1] > 0.0, "feedback lifted a row with no query term");
        assert!(scores[1] > scores[2]);
    }

    #[test]
    fn keep_cap_bounds_the_inline_set() {
        let kept = select(ROWS, "the", 2, &StaticEmbedder);
        assert!(kept.len() <= 2);
    }
}
