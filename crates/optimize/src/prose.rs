use std::collections::HashSet;

const MIN_SENTENCES: usize = 3;
const MIN_ATOMS: usize = 8;
const MAX_SENTENCES: usize = 1000;

// Digest is the payload not an index, so larger than a structural preview; sized to the measured reopen rate (0.58).
const PROSE_FRACTION: f64 = 0.25;
const MIN_DIGEST_BYTES: usize = 400;

pub fn budget(body_len: usize) -> usize {
    ((body_len as f64 * PROSE_FRACTION) as usize).max(MIN_DIGEST_BYTES)
}

const MIN_SENTENCE_WORDS: usize = 8;

// Orders a block's sentences best-first; the caller keeps a budget-bounded prefix.
pub trait ProseScorer: Send + Sync {
    fn rank(&self, sentences: &[&str]) -> Vec<usize>;
}

// Greedily orders sentences by new-term coverage, most distinct first.
pub struct CoverageScorer;

impl ProseScorer for CoverageScorer {
    fn rank(&self, sentences: &[&str]) -> Vec<usize> {
        let atoms: Vec<HashSet<&str>> = sentences.iter().map(|s| atoms(s).collect()).collect();
        let mut order = Vec::with_capacity(sentences.len());
        let mut covered: HashSet<&str> = HashSet::new();
        let mut used = vec![false; sentences.len()];
        for _ in 0..sentences.len() {
            let mut best: Option<(usize, usize)> = None;
            for (i, taken) in used.iter().enumerate() {
                if *taken {
                    continue;
                }
                let marginal = atoms[i].difference(&covered).count();
                if best.is_none_or(|(m, _)| marginal > m) {
                    best = Some((marginal, i));
                }
            }
            let (marginal, i) = best.unwrap();
            used[i] = true;
            order.push(i);
            if marginal > 0 {
                covered.extend(&atoms[i]);
            }
        }
        order
    }
}

// Sentences kept for a summary, with the key-term coverage they carry.
struct Selection<'a> {
    sentences: Vec<&'a str>,
    chosen: Vec<usize>,
    covered: usize,
    total: usize,
}

// None when the block has no prose structure, so it falls through untouched.
fn select<'a>(text: &'a str, budget: usize, scorer: &dyn ProseScorer) -> Option<Selection<'a>> {
    let sentences: Vec<&str> = sentences(text)
        .take(MAX_SENTENCES)
        .filter(|s| is_prose(s))
        .collect();
    if sentences.len() < MIN_SENTENCES {
        return None;
    }
    let prose_bytes: usize = sentences.iter().map(|s| s.len()).sum();
    if prose_bytes * 2 < text.len() {
        return None;
    }
    let atoms: Vec<HashSet<&str>> = sentences.iter().map(|s| atoms(s).collect()).collect();
    let total: HashSet<&str> = atoms.iter().flatten().copied().collect();
    if total.len() < MIN_ATOMS {
        return None;
    }

    let mut chosen: Vec<usize> = Vec::new();
    let mut covered: HashSet<&str> = HashSet::new();
    let mut used = 0;
    for i in scorer.rank(&sentences) {
        let add = sentences[i].len() + 1;
        if !chosen.is_empty() && used + add > budget {
            break;
        }
        chosen.push(i);
        used += add;
        covered.extend(&atoms[i]);
    }
    if chosen.is_empty() {
        return None;
    }
    chosen.sort_unstable();
    Some(Selection {
        sentences,
        chosen,
        covered: covered.len(),
        total: total.len(),
    })
}

// Offload preview: a coverage-ranked index of the block's own sentences.
pub fn digest(text: &str, budget: usize) -> Option<String> {
    let sel = select(text, budget, &CoverageScorer)?;
    let shown: Vec<&str> = sel.chosen.iter().map(|&i| sel.sentences[i]).collect();
    let label = format!(
        "{} sentences, {} shown covering {} of {} key terms",
        sel.sentences.len(),
        sel.chosen.len(),
        sel.covered,
        sel.total
    );
    Some(format!("{label}\n{}", shown.join("\n")))
}

// Opt-in working summary: top-ranked sentences inline, the rest recoverable via the marker.
pub fn summary(text: &str, budget: usize, scorer: &dyn ProseScorer) -> Option<String> {
    let sel = select(text, budget, scorer)?;
    let dropped = sel.sentences.len() - sel.chosen.len();
    if dropped == 0 {
        return None;
    }
    let shown: Vec<&str> = sel.chosen.iter().map(|&i| sel.sentences[i]).collect();
    let pct = sel.covered * 100 / sel.total.max(1);
    let header = format!(
        "[prose summary: {} of {} sentences kept, ~{pct}% of key terms; {dropped} dropped, call resolve for the full text]",
        sel.chosen.len(),
        sel.sentences.len(),
    );
    Some(format!("{header}\n{}", shown.join("\n")))
}

fn sentences(text: &str) -> impl Iterator<Item = &str> {
    let mut spans = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((idx, c)) = chars.next() {
        if matches!(c, '.' | '!' | '?' | '\n') {
            let end = idx + c.len_utf8();
            let span = text[start..end].trim();
            if !span.is_empty() {
                spans.push(span);
            }
            start = end;
            while let Some(&(nidx, nc)) = chars.peek() {
                if nc.is_whitespace() {
                    start = nidx + nc.len_utf8();
                    chars.next();
                } else {
                    break;
                }
            }
        }
    }
    let tail = text[start..].trim();
    if !tail.is_empty() {
        spans.push(tail);
    }
    spans.into_iter()
}

// A natural-language sentence: terminal punctuation, enough words, mostly letters. The terminator
// screens out newline-delimited tables and log lines whose rows would otherwise pass.
fn is_prose(sentence: &str) -> bool {
    if !sentence.ends_with(['.', '!', '?']) {
        return false;
    }
    if sentence.split_whitespace().count() < MIN_SENTENCE_WORDS {
        return false;
    }
    let letters = sentence
        .chars()
        .filter(|c| c.is_alphabetic() || c.is_whitespace())
        .count();
    letters * 4 >= sentence.len() * 3
}

fn atoms(sentence: &str) -> impl Iterator<Item = &str> {
    sentence
        .split(|c: char| c.is_whitespace())
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()))
        .filter(|w| {
            w.len() >= 6
                || w.chars().any(|c| c.is_ascii_digit())
                || (w.len() >= 3 && w.chars().next().is_some_and(char::is_uppercase))
        })
}

pub struct Span {
    pub start: usize,
    pub end: usize,
}

// Spans to keep, not rewritten text: the caller extracts them verbatim.
pub trait ProseShrinker: Send + Sync {
    fn keep(&self, text: &str) -> Option<Vec<Span>>;
}

// Spans out of range or splitting a codepoint are dropped, so a bad reply can't splice foreign bytes.
pub fn shrink(text: &str, spans: &[Span]) -> Option<String> {
    let mut kept: Vec<(usize, usize)> = spans
        .iter()
        .filter(|s| s.start < s.end && s.end <= text.len())
        .filter(|s| text.is_char_boundary(s.start) && text.is_char_boundary(s.end))
        .map(|s| (s.start, s.end))
        .collect();
    if kept.is_empty() {
        return None;
    }
    kept.sort_unstable();
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for (start, end) in kept {
        match merged.last_mut() {
            Some(last) if start <= last.1 => last.1 = last.1.max(end),
            _ => merged.push((start, end)),
        }
    }
    let kept_bytes: usize = merged.iter().map(|(s, e)| e - s).sum();
    if kept_bytes >= text.len() {
        return None;
    }

    let pieces: Vec<&str> = merged.iter().map(|(s, e)| text[*s..*e].trim()).collect();
    let mut body = pieces.join(" … ");
    let mut dropped = merged.len() - 1;
    if merged.first().is_some_and(|(s, _)| *s > 0) {
        body = format!("… {body}");
        dropped += 1;
    }
    if merged.last().is_some_and(|(_, e)| *e < text.len()) {
        body = format!("{body} …");
        dropped += 1;
    }
    let pct = kept_bytes * 100 / text.len().max(1);
    let header = format!(
        "[prose shrunk: ~{pct}% of characters kept, {dropped} spans dropped, call resolve for the full text]"
    );
    Some(format!("{header}\n{}", body.trim()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PARAGRAPH: &str = "The authentication service validates every request against the session store. \
Tokens expire after 3600 seconds and are rotated on each privileged action. \
When a token is missing the gateway returns a 401 and logs the client address. \
Rotation is handled by the background worker in a separate process. \
The store is a Redis cluster with three replicas for durability.";

    #[test]
    fn every_shown_sentence_is_literally_in_the_body() {
        let digest = digest(PARAGRAPH, 200).unwrap();
        for line in digest.lines().skip(1) {
            assert!(PARAGRAPH.contains(line), "not literal: {line:?}");
        }
    }

    #[test]
    fn the_digest_stays_within_budget() {
        let digest = digest(PARAGRAPH, 160).unwrap();
        let body: usize = digest.lines().skip(1).map(|l| l.len() + 1).sum();
        assert!(body <= 160);
    }

    #[test]
    fn a_bigger_budget_covers_at_least_as_many_terms() {
        let small = covered_terms(&digest(PARAGRAPH, 120).unwrap());
        let big = covered_terms(&digest(PARAGRAPH, 400).unwrap());
        assert!(big >= small);
    }

    #[test]
    fn refuses_input_without_prose_structure() {
        assert!(digest("one short line", 200).is_none());
        assert!(digest("a b c d e f g h", 200).is_none());
    }

    #[test]
    fn refuses_a_newline_delimited_table() {
        let table = "NAMESPACE     LAST SEEN   TYPE      REASON            OBJECT\n\
default       5m          Normal    Scheduled         pod/api-server-7d\n\
kube-system   12m         Warning   FailedMount       pod/metrics-agent\n\
production    2m          Normal    Pulled            pod/worker-queue-3";
        assert!(digest(table, 400).is_none());
    }

    fn covered_terms(digest: &str) -> usize {
        let header = digest.lines().next().unwrap();
        let of = header.split(" of ").next().unwrap();
        of.rsplit(' ').next().unwrap().parse().unwrap()
    }

    #[test]
    fn summary_keeps_a_prefix_verbatim_and_reports_the_drop() {
        let summary = summary(PARAGRAPH, 200, &CoverageScorer).unwrap();
        assert!(summary.lines().next().unwrap().contains("dropped"));
        assert!(summary.lines().next().unwrap().contains("call resolve"));
        for line in summary.lines().skip(1) {
            assert!(
                PARAGRAPH.contains(line),
                "kept sentence not literal: {line:?}"
            );
        }
        let body: usize = summary.lines().skip(1).map(|l| l.len() + 1).sum();
        assert!(body <= 200);
    }

    #[test]
    fn a_summary_that_drops_nothing_is_none() {
        // Budget past the whole block keeps every sentence: nothing to summarize.
        assert!(summary(PARAGRAPH, 10_000, &CoverageScorer).is_none());
    }

    // Ranks sentences in source order, standing in for a swapped-in backend.
    struct SourceOrder;
    impl ProseScorer for SourceOrder {
        fn rank(&self, sentences: &[&str]) -> Vec<usize> {
            (0..sentences.len()).collect()
        }
    }

    #[test]
    fn a_swapped_scorer_decides_what_is_kept() {
        let summary = summary(PARAGRAPH, 200, &SourceOrder).unwrap();
        let first_kept = summary.lines().nth(1).unwrap();
        assert!(
            first_kept.starts_with("The authentication service"),
            "the source-order scorer keeps the first sentence first, got {first_kept:?}"
        );
    }

    fn span_of(text: &str, needle: &str) -> Span {
        let start = text.find(needle).unwrap();
        Span {
            start,
            end: start + needle.len(),
        }
    }

    #[test]
    fn shrink_keeps_only_the_chosen_spans_verbatim_and_marks_elisions() {
        let text = "the authentication service validates every incoming request";
        let spans = vec![
            span_of(text, "authentication service"),
            span_of(text, "request"),
        ];
        let out = shrink(text, &spans).unwrap();
        let body = out.lines().nth(1).unwrap();
        assert!(body.contains("authentication service"));
        assert!(body.contains("request"));
        // The dropped middle ("validates every incoming") is gone, elided.
        assert!(!body.contains("validates"));
        assert!(body.contains('…'));
        assert!(out.lines().next().unwrap().contains("dropped"));
    }

    #[test]
    fn shrink_is_none_when_the_spans_keep_everything() {
        let text = "the authentication service validates every request";
        let spans = vec![Span {
            start: 0,
            end: text.len(),
        }];
        assert!(shrink(text, &spans).is_none());
    }

    #[test]
    fn shrink_never_panics_and_keeps_only_source_substrings() {
        let texts = [
            "café résumé naïve, the database cluster survived the failover",
            "ascii only text with several words and repeated repeated words here",
            "emoji 🚀 rocket and accents àéîõü mixed with 12345 numbers inline",
            "",
            "short",
        ];
        let mut seed = 0x9e3779b9u32;
        let mut rng = || {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            seed
        };
        for text in texts {
            for _ in 0..3000 {
                let n = (rng() % 6) as usize;
                let spans: Vec<Span> = (0..n)
                    .map(|_| {
                        let a = rng() as usize % (text.len() + 4);
                        let b = rng() as usize % (text.len() + 4);
                        Span {
                            start: a.min(b),
                            end: a.max(b),
                        }
                    })
                    .collect();
                let Some(out) = shrink(text, &spans) else {
                    continue;
                };
                let body = out.split_once('\n').map(|(_, b)| b).unwrap_or("");
                for piece in body.split(" … ") {
                    let piece = piece.trim_matches('…').trim();
                    assert!(
                        piece.is_empty() || text.contains(piece),
                        "spliced non-source text {piece:?} from {text:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn shrink_discards_out_of_range_and_mid_codepoint_spans() {
        // 'é' occupies two bytes, so a span ending inside it is not a char boundary.
        let text = "café resume naive text about the database cluster";
        let out_of_range = Span {
            start: 0,
            end: text.len() + 40,
        };
        let mid_codepoint = Span { start: 3, end: 4 };
        let valid = span_of(text, "database cluster");
        let out = shrink(text, &[out_of_range, mid_codepoint, valid]).unwrap();
        let body = out.lines().nth(1).unwrap();
        assert!(body.contains("database cluster"));
        // Nothing spliced from the bad spans: the only kept run is the valid one.
        assert!(!body.contains("café"));
    }
}
