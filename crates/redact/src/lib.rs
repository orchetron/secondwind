#![forbid(unsafe_code)]

use std::collections::BTreeMap;

use regex::Regex;
use secondwind_core::Trace;

pub struct Redactor {
    rules: Vec<Rule>,
    assignment: Regex,
    home_path: Regex,
}

struct Rule {
    kind: &'static str,
    pattern: Regex,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub by_kind: BTreeMap<String, usize>,
}

impl Report {
    pub fn total(&self) -> usize {
        self.by_kind.values().sum()
    }
}

const PATTERNS: &[(&str, &str)] = &[
    ("anthropic-key", r"sk-ant-[A-Za-z0-9_-]{10,}"),
    ("openai-key", r"sk-[A-Za-z0-9]{20,}"),
    (
        "github-token",
        r"(?:ghp|gho|ghu|ghs|ghr)_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,}",
    ),
    ("aws-key", r"AKIA[0-9A-Z]{16}"),
    ("slack-token", r"xox[baprs]-[A-Za-z0-9-]{10,}"),
    ("google-key", r"AIza[0-9A-Za-z_-]{30,}"),
    (
        "private-key",
        r"-----BEGIN [A-Z ]*PRIVATE KEY-----[\s\S]*?-----END [A-Z ]*PRIVATE KEY-----",
    ),
    ("bearer", r"(?i)\bbearer\s+[A-Za-z0-9._~+/=-]{16,}"),
];

impl Default for Redactor {
    fn default() -> Self {
        Self::new()
    }
}

impl Redactor {
    pub fn new() -> Self {
        Self {
            rules: PATTERNS
                .iter()
                .map(|(kind, pattern)| Rule {
                    kind,
                    pattern: Regex::new(pattern).expect("pattern compiles"),
                })
                .collect(),
            assignment: Regex::new(
                r#"(?i)\b(api_key|apikey|api-key|token|secret|password|passwd)(["']?\s*[:=]\s*["']?)([^\s"']{8,})"#,
            )
            .expect("pattern compiles"),
            home_path: Regex::new(r"/(?:Users|home)/[A-Za-z0-9._-]+").expect("pattern compiles"),
        }
    }

    pub fn redact_text(&self, text: &str, report: &mut Report) -> String {
        let mut out = text.to_string();
        for rule in &self.rules {
            out = self.replace_counted(&out, &rule.pattern, rule.kind, report, |m| {
                mask(rule.kind, m)
            });
        }
        let assignment = &self.assignment;
        if assignment.is_match(&out) {
            let mut hits = 0;
            out = assignment
                .replace_all(&out, |caps: &regex::Captures<'_>| {
                    hits += 1;
                    format!("{}{}{}", &caps[1], &caps[2], mask("credential", &caps[3]))
                })
                .into_owned();
            *report.by_kind.entry("credential".into()).or_insert(0) += hits;
        }
        if self.home_path.is_match(&out) {
            let count = self.home_path.find_iter(&out).count();
            out = self.home_path.replace_all(&out, "/home/user").into_owned();
            *report.by_kind.entry("home-path".into()).or_insert(0) += count;
        }
        redact_high_entropy(&out, report)
    }

    fn replace_counted(
        &self,
        text: &str,
        pattern: &Regex,
        kind: &str,
        report: &mut Report,
        replacement: impl Fn(&str) -> String,
    ) -> String {
        if !pattern.is_match(text) {
            return text.to_string();
        }
        let count = pattern.find_iter(text).count();
        *report.by_kind.entry(kind.to_string()).or_insert(0) += count;
        pattern
            .replace_all(text, |caps: &regex::Captures<'_>| replacement(&caps[0]))
            .into_owned()
    }

    pub fn redact_trace(&self, trace: &mut Trace) -> Report {
        let mut report = Report::default();
        for turn in &mut trace.turns {
            for segment in &mut turn.segments {
                segment.effective = self.redact_text(&segment.effective, &mut report);
                if let Some(original) = &segment.original {
                    segment.original = Some(self.redact_text(original, &mut report));
                }
            }
        }
        report
    }
}

fn redact_high_entropy(text: &str, report: &mut Report) -> String {
    let mut out = String::with_capacity(text.len());
    let mut hits = 0;
    for line in text.split_inclusive('\n') {
        for (i, token) in line.split(' ').enumerate() {
            if i > 0 {
                out.push(' ');
            }
            let trimmed = token.trim_end_matches('\n');
            if is_high_entropy(trimmed) {
                hits += 1;
                out.push_str(&mask("high-entropy", trimmed));
                if token.ends_with('\n') {
                    out.push('\n');
                }
            } else {
                out.push_str(token);
            }
        }
    }
    if hits > 0 {
        *report.by_kind.entry("high-entropy".into()).or_insert(0) += hits;
    }
    out
}

fn is_high_entropy(token: &str) -> bool {
    let clean = token.trim_matches(|c: char| "\"'(),;[]{}<>`".contains(c));
    if clean.len() < 20
        || clean.starts_with("[redacted:")
        || clean.starts_with('/')
        || clean.starts_with("./")
        || clean.starts_with("~/")
        || clean.starts_with("http://")
        || clean.starts_with("https://")
    {
        return false;
    }
    let lower = clean.chars().filter(|c| c.is_ascii_lowercase()).count();
    let upper = clean.chars().filter(|c| c.is_ascii_uppercase()).count();
    let digit = clean.chars().filter(|c| c.is_ascii_digit()).count();
    let classes = [lower, upper, digit].iter().filter(|n| **n > 0).count();
    classes == 3 && shannon_bits(clean) > 4.0
}

fn shannon_bits(s: &str) -> f64 {
    let mut counts = [0usize; 256];
    let bytes = s.as_bytes();
    for b in bytes {
        counts[*b as usize] += 1;
    }
    let len = bytes.len() as f64;
    counts
        .iter()
        .filter(|c| **c > 0)
        .map(|c| {
            let p = *c as f64 / len;
            -p * p.log2()
        })
        .sum()
}

fn mask(kind: &str, value: &str) -> String {
    format!("[redacted:{kind}:{:04x}]", fnv16(value))
}

fn fnv16(value: &str) -> u16 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in value.as_bytes() {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    (hash & 0xffff) as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    fn redact(text: &str) -> (String, Report) {
        let mut report = Report::default();
        let out = Redactor::new().redact_text(text, &mut report);
        (out, report)
    }

    #[test]
    fn known_secret_shapes_are_masked() {
        let (out, report) = redact(
            "key sk-ant-api03-abc123def456ghi789 and ghp_abcdefghij1234567890KLMNOP and AKIAIOSFODNN7EXAMPLE",
        );
        assert!(!out.contains("sk-ant-api03"));
        assert!(!out.contains("ghp_"));
        assert!(!out.contains("AKIAIOSFODNN7EXAMPLE"));
        assert_eq!(report.by_kind.get("anthropic-key"), Some(&1));
        assert_eq!(report.by_kind.get("github-token"), Some(&1));
        assert_eq!(report.by_kind.get("aws-key"), Some(&1));
    }

    #[test]
    fn assignments_keep_key_names_and_mask_values() {
        let (out, report) = redact("config: password = hunter2secret and port = 8443");
        assert!(out.contains("password = [redacted:credential:"));
        assert!(out.contains("port = 8443"));
        assert_eq!(report.by_kind.get("credential"), Some(&1));
    }

    #[test]
    fn home_paths_are_anonymized() {
        let (out, report) = redact("read /Users/janedoe/project/src/main.rs fully");
        assert_eq!(out, "read /home/user/project/src/main.rs fully");
        assert_eq!(report.by_kind.get("home-path"), Some(&1));
    }

    #[test]
    fn high_entropy_tokens_are_masked_but_prose_and_paths_survive() {
        let (out, report) = redact(
            "value A8f3kZ9qLmXv2Rp7TqWy4Nb6 stays hidden, /home/user/some/long/path/name.rs stays, plain sentences with ordinary words stay",
        );
        assert!(out.contains("[redacted:high-entropy:"));
        assert!(out.contains("/home/user/some/long/path/name.rs"));
        assert!(out.contains("ordinary words stay"));
        assert_eq!(report.by_kind.get("high-entropy"), Some(&1));
    }

    #[test]
    fn masking_is_deterministic_for_cross_references() {
        let (a, _) = redact("token=abcdefgh12345678secret");
        let (b, _) = redact("token=abcdefgh12345678secret");
        assert_eq!(a, b);
    }

    #[test]
    fn redact_trace_covers_both_sides() {
        use secondwind_core::{Origin, Party, Provenance, Role, Segment, SegmentKind, Trace, Turn};
        let mut trace = Trace {
            id: "t".into(),
            source: "test".into(),
            optimizer: None,
            provenance: Provenance {
                origin: Origin::Synthetic,
                party: Party::FirstParty,
            },
            turns: vec![Turn {
                index: 0,
                role: Role::User,
                timestamp: None,
                model: None,
                sidechain: false,
                segments: vec![Segment {
                    kind: SegmentKind::Text,
                    original: Some("original with sk-ant-api03-abc123def456ghi789".into()),
                    effective: "effective in /Users/janedoe/repo".into(),
                }],
                billing: None,
            }],
        };
        let report = Redactor::new().redact_trace(&mut trace);
        assert!(report.total() >= 2);
        let segment = &trace.turns[0].segments[0];
        assert!(!segment.original.as_deref().unwrap().contains("sk-ant"));
        assert!(segment.effective.contains("/home/user/repo"));
    }
}
