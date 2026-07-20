#![forbid(unsafe_code)]

use std::fmt::Write as _;

use secondwind_analyzers::{Finding, Retention};
use secondwind_ledger::LedgerSummary;
use serde::Serialize;

pub mod scoreboard;

#[derive(Debug, Default, Serialize)]
pub struct Audit {
    pub files_discovered: usize,
    pub files_failed: usize,
    pub traces_read: usize,
    pub turns_read: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub period_start: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub period_end: Option<String>,
    pub segments_paired: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ledger: Option<LedgerSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retention: Option<Retention>,
    pub findings: Vec<Finding>,
}

pub trait Reporter {
    fn id(&self) -> &'static str;
    fn render(&self, audit: &Audit) -> String;
}

pub fn commas(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

pub fn money(usd: f64) -> String {
    let cents = (usd * 100.0).round() as u64;
    format!("${}.{:02}", commas(cents / 100), cents % 100)
}

fn date_of(timestamp: &str) -> &str {
    timestamp.get(..10).unwrap_or(timestamp)
}

fn kept_ratio(kept: u64, total: u64) -> String {
    format!("{}/{}", commas(kept), commas(total))
}

fn percent_kept(kept: u64, total: u64) -> String {
    if total == 0 {
        return "n/a".into();
    }
    format!("{}% kept", kept * 100 / total)
}

pub struct Terminal {
    pub color: bool,
}

impl Terminal {
    fn bold(&self, s: &str) -> String {
        if self.color {
            format!("\x1b[1m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }

    fn green(&self, s: &str) -> String {
        if self.color {
            format!("\x1b[32m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }

    fn red(&self, s: &str) -> String {
        if self.color {
            format!("\x1b[31m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }

    fn dim(&self, s: &str) -> String {
        if self.color {
            format!("\x1b[2m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }
}

impl Reporter for Terminal {
    fn id(&self) -> &'static str {
        "terminal"
    }

    fn render(&self, audit: &Audit) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "{}", self.bold("secondwind audit"));
        out.push('\n');

        let period = match (&audit.period_start, &audit.period_end) {
            (Some(start), Some(end)) => format!("  ({} \u{2192} {})", date_of(start), date_of(end)),
            _ => String::new(),
        };
        let _ = writeln!(
            out,
            "  sessions          {}{period}",
            commas(audit.traces_read as u64)
        );
        let _ = writeln!(
            out,
            "  turns             {}",
            commas(audit.turns_read as u64)
        );
        let _ = writeln!(
            out,
            "  files             {}",
            commas(audit.files_discovered as u64)
        );
        if audit.files_failed > 0 {
            let _ = writeln!(
                out,
                "  files failed      {}",
                self.red(&commas(audit.files_failed as u64))
            );
        }

        if let Some(ledger) = &audit.ledger {
            out.push('\n');
            let _ = writeln!(out, "{}", self.bold("cost (api list prices)"));
            let _ = writeln!(out, "  usage             {}", money(ledger.actual_usd));
            let _ = writeln!(
                out,
                "  without caching   {}",
                money(ledger.without_caching_usd)
            );
            let percent = if ledger.without_caching_usd > 0.0 {
                ledger.caching_saved_usd / ledger.without_caching_usd * 100.0
            } else {
                0.0
            };
            let saved = format!("{} ({percent:.0}%)", money(ledger.caching_saved_usd));
            let _ = writeln!(out, "  caching saved     {}", self.green(&saved));

            let mut models: Vec<_> = ledger.by_model.iter().collect();
            models.sort_by(|a, b| b.1.actual_usd.total_cmp(&a.1.actual_usd));
            for (model, spend) in models.iter().take(4) {
                let _ = writeln!(out, "  {model:<17} {}", self.dim(&money(spend.actual_usd)));
            }
            for (model, tokens) in &ledger.unpriced_models {
                let note = format!("{model} unpriced ({} tokens)", commas(*tokens));
                let _ = writeln!(out, "  {}", self.dim(&note));
            }
        }

        out.push('\n');
        let _ = writeln!(out, "{}", self.bold("fidelity"));
        if audit.segments_paired == 0 {
            let _ = writeln!(
                out,
                "  no before/after pairs available (no optimizer store found)"
            );
        } else {
            if let Some(retention) = &audit.retention
                && retention.big_drop_segments > 0
            {
                let headline = format!(
                    "{} tool result(s) lost up to {}% of their records with no retrieval marker",
                    commas(retention.big_drop_segments),
                    retention.worst_drop_percent
                );
                let _ = writeln!(out, "  {}", self.red(&headline));
            }
            let _ = writeln!(
                out,
                "  originals paired  {} segments",
                commas(audit.segments_paired as u64)
            );
            if let Some(retention) = &audit.retention {
                let _ = writeln!(
                    out,
                    "  numerics kept     {} ({})",
                    kept_ratio(retention.numerics_kept, retention.numerics_total),
                    percent_kept(retention.numerics_kept, retention.numerics_total),
                );
                let _ = writeln!(
                    out,
                    "  artifacts kept    {} ({})",
                    kept_ratio(retention.artifacts_kept, retention.artifacts_total),
                    percent_kept(retention.artifacts_kept, retention.artifacts_total),
                );
            }
            let label = format!("{} violations", audit.findings.len());
            let colored = if audit.findings.is_empty() {
                self.green(&label)
            } else {
                self.red(&label)
            };
            let _ = writeln!(out, "  findings          {colored}");
        }

        for finding in audit.findings.iter().take(10) {
            let head = format!(
                "  [{}] {}#{}  {}",
                finding.class, finding.trace_id, finding.turn, finding.detail
            );
            let _ = writeln!(out, "{}", self.red(&head));
            if !finding.original.is_empty() {
                let _ = writeln!(out, "      original   {}", self.dim(&finding.original));
            }
            if !finding.effective.is_empty() {
                let _ = writeln!(out, "      effective  {}", self.dim(&finding.effective));
            }
        }
        if audit.findings.len() > 10 {
            let _ = writeln!(out, "  ... and {} more", audit.findings.len() - 10);
        }
        out
    }
}

pub struct Markdown;

impl Reporter for Markdown {
    fn id(&self) -> &'static str {
        "markdown"
    }

    fn render(&self, audit: &Audit) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "### secondwind audit\n");
        let _ = writeln!(out, "| metric | value |");
        let _ = writeln!(out, "|---|---|");
        let _ = writeln!(out, "| sessions | {} |", commas(audit.traces_read as u64));
        let _ = writeln!(out, "| turns | {} |", commas(audit.turns_read as u64));
        if let (Some(start), Some(end)) = (&audit.period_start, &audit.period_end) {
            let _ = writeln!(
                out,
                "| period | {} \u{2192} {} |",
                date_of(start),
                date_of(end)
            );
        }
        if let Some(ledger) = &audit.ledger {
            let _ = writeln!(out, "| usage (api list) | {} |", money(ledger.actual_usd));
            let _ = writeln!(
                out,
                "| without caching | {} |",
                money(ledger.without_caching_usd)
            );
            let percent = if ledger.without_caching_usd > 0.0 {
                ledger.caching_saved_usd / ledger.without_caching_usd * 100.0
            } else {
                0.0
            };
            let _ = writeln!(
                out,
                "| caching saved | {} ({percent:.0}%) |",
                money(ledger.caching_saved_usd)
            );
            for (model, tokens) in &ledger.unpriced_models {
                let _ = writeln!(out, "| unpriced | {model} ({} tokens) |", commas(*tokens));
            }
        }
        let _ = writeln!(out, "| paired segments | {} |", audit.segments_paired);
        let _ = writeln!(out, "| violations | {} |", audit.findings.len());

        if !audit.findings.is_empty() {
            let _ = writeln!(out, "\n**findings**\n");
            for finding in &audit.findings {
                let _ = writeln!(
                    out,
                    "- `{}` {}#{}: {}",
                    finding.class, finding.trace_id, finding.turn, finding.detail
                );
                if !finding.original.is_empty() {
                    let _ = writeln!(out, "  - original: `{}`", finding.original);
                }
                if !finding.effective.is_empty() {
                    let _ = writeln!(out, "  - effective: `{}`", finding.effective);
                }
            }
        }
        out
    }
}

pub struct Html;

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

impl Reporter for Html {
    fn id(&self) -> &'static str {
        "html"
    }

    fn render(&self, audit: &Audit) -> String {
        let mut rows = String::new();
        let mut row = |label: &str, value: String| {
            let _ = write!(rows, "<tr><td>{label}</td><td>{value}</td></tr>");
        };
        row("sessions", commas(audit.traces_read as u64));
        row("turns", commas(audit.turns_read as u64));
        if let (Some(start), Some(end)) = (&audit.period_start, &audit.period_end) {
            row(
                "period",
                format!("{} \u{2192} {}", date_of(start), date_of(end)),
            );
        }
        if let Some(ledger) = &audit.ledger {
            row("usage (api list)", money(ledger.actual_usd));
            row("without caching", money(ledger.without_caching_usd));
            let percent = if ledger.without_caching_usd > 0.0 {
                ledger.caching_saved_usd / ledger.without_caching_usd * 100.0
            } else {
                0.0
            };
            row(
                "caching saved",
                format!("{} ({percent:.0}%)", money(ledger.caching_saved_usd)),
            );
        }
        row("paired segments", commas(audit.segments_paired as u64));
        row("violations", commas(audit.findings.len() as u64));

        let mut findings = String::new();
        for finding in &audit.findings {
            let _ = write!(
                findings,
                "<li><code>{}</code> {}#{}: {}</li>",
                finding.class,
                escape(&finding.trace_id),
                finding.turn,
                escape(&finding.detail)
            );
        }
        let findings_block = if findings.is_empty() {
            String::new()
        } else {
            format!("<h2>findings</h2><ul>{findings}</ul>")
        };

        format!(
            "<!doctype html><html><head><meta charset=\"utf-8\">\
<title>secondwind audit</title><style>\
body{{font:15px/1.5 system-ui,sans-serif;max-width:720px;margin:2rem auto;padding:0 1rem;color:#1a1a1a}}\
@media(prefers-color-scheme:dark){{body{{background:#111;color:#eee}}td{{border-color:#333}}}}\
h1{{font-size:1.4rem}}table{{border-collapse:collapse;width:100%}}\
td{{padding:.4rem .6rem;border-bottom:1px solid #ddd}}td:last-child{{text-align:right;font-variant-numeric:tabular-nums}}\
code{{background:rgba(128,128,128,.15);padding:.1em .3em;border-radius:3px}}\
</style></head><body><h1>secondwind audit</h1><table>{rows}</table>{findings_block}</body></html>"
        )
    }
}

pub struct Json;

impl Reporter for Json {
    fn id(&self) -> &'static str {
        "json"
    }

    fn render(&self, audit: &Audit) -> String {
        serde_json::to_string_pretty(audit).expect("audit serializes")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn audit() -> Audit {
        Audit {
            files_discovered: 2,
            traces_read: 1,
            turns_read: 1234,
            period_start: Some("2026-06-01T10:00:00Z".into()),
            period_end: Some("2026-07-16T10:00:00Z".into()),
            ..Default::default()
        }
    }

    #[test]
    fn commas_and_money_format() {
        assert_eq!(commas(1234567), "1,234,567");
        assert_eq!(money(55702.814), "$55,702.81");
        assert_eq!(money(0.5), "$0.50");
    }

    #[test]
    fn terminal_without_color_has_no_escape_codes() {
        let text = Terminal { color: false }.render(&audit());
        assert!(!text.contains('\x1b'));
        assert!(text.contains("sessions          1  (2026-06-01 \u{2192} 2026-07-16)"));
        assert!(text.contains("no before/after pairs available"));
    }

    #[test]
    fn markdown_is_paste_ready() {
        let text = Markdown.render(&audit());
        assert!(text.starts_with("### secondwind audit"));
        assert!(text.contains("| turns | 1,234 |"));
    }
}
