use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

// One optimization the pipeline performed; every live surface (exec, mcp) appends
// to the same log so one dashboard reads all. verified = passed admission+coverage
// gates (anything but a verbatim pass-through).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Event {
    pub at_ms: u64,
    pub surface: String,
    pub transform: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    // Internal only: never persisted or served. The surface talks tokens, not dollars.
    #[serde(skip)]
    pub saved_usd: f64,
    pub verified: bool,
    pub inline: bool,
    // blake3 fidelity certificate + atom count for this block. Defaulted so ledgers
    // written before these fields still load.
    #[serde(default)]
    pub atoms: u64,
    #[serde(default)]
    pub cert: String,
    // Model whose published rates priced saved_usd.
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub platform: String,
    // Tenant from the gateway's tenant header, for per-team breakdown. Empty for single-tenant.
    #[serde(default)]
    pub tenant: String,
    // Empty for a compressed/offloaded block. A reason slug (e.g. refused_clmh, no_saving,
    // no_resolver, self_proof_reject) when the block was seen but left verbatim, so the trail shows
    // what the gate refused and why, not only what it compressed.
    #[serde(default)]
    pub kept_reason: String,
    // Correlation id shared by every block booked for one request, so a request's decisions join up.
    #[serde(default)]
    pub req_id: String,
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn dir(home: &Path) -> PathBuf {
    home.join(".secondwind").join("events")
}

fn log_path(home: &Path) -> PathBuf {
    dir(home).join("events.jsonl")
}

pub fn record(home: &Path, event: &Event) {
    if fs::create_dir_all(dir(home)).is_err() {
        return;
    }
    let Ok(line) = serde_json::to_string(event) else {
        return;
    };
    if let Ok(mut file) = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path(home))
    {
        let _ = writeln!(file, "{line}");
    }
}

pub fn load(home: &Path) -> Vec<Event> {
    let Ok(raw) = fs::read_to_string(log_path(home)) else {
        return Vec::new();
    };
    raw.lines()
        .filter_map(|line| serde_json::from_str::<Event>(line).ok())
        .collect()
}

#[derive(Debug, Clone, Serialize)]
pub struct Point {
    pub at_ms: u64,
    #[serde(skip)]
    pub cumulative_usd: f64,
    pub cumulative_tokens: u64,
}

#[derive(Debug, Default, Serialize)]
pub struct Summary {
    pub blocks: u64,
    // Blocks the gate saw but left verbatim, and the breakdown by reason. seen = blocks + kept, so
    // the trail answers "of everything seen, how much was compressed vs left alone, and why".
    pub kept: u64,
    pub seen: u64,
    pub by_kept_reason: BTreeMap<String, u64>,
    pub inline: u64,
    pub offloaded: u64,
    pub verified: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(skip)]
    pub saved_usd: f64,
    pub by_transform: BTreeMap<String, u64>,
    pub transform_saved: BTreeMap<String, u64>,
    pub by_surface: BTreeMap<String, u64>,
    pub by_model: BTreeMap<String, u64>,
    pub by_platform: BTreeMap<String, u64>,
    // Tokens removed: last hour (rolling), and current day/week(from Monday)/month
    // in the viewer's timezone.
    pub saved_hour: u64,
    pub saved_today: u64,
    pub saved_week: u64,
    pub saved_month: u64,
    pub recent: Vec<Event>,
    pub curve: Vec<Point>,
}

const RECENT: usize = 40;
const CURVE_POINTS: usize = 120;

// Civil (year, month, day) from a day index since 1970-01-01, and the inverse, so
// calendar-bucket boundaries need no date dependency. Hinnant's algorithms.
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

pub fn summarize(events: &[Event], tz_offset_min: i64) -> Summary {
    let mut summary = Summary::default();
    let now = now_ms();
    // Calendar boundaries in the viewer's timezone (local = utc - offset).
    let tz = tz_offset_min * 60_000;
    let day = (now as i64 - tz).div_euclid(86_400_000);
    let today_start = day * 86_400_000 + tz;
    let week_start = (day - (day + 3).rem_euclid(7)) * 86_400_000 + tz;
    let (cy, cm, _) = civil_from_days(day);
    let month_start = days_from_civil(cy, cm, 1) * 86_400_000 + tz;
    let mut cumulative = 0.0;
    let mut cumulative_tokens = 0u64;
    let mut full_curve = Vec::with_capacity(events.len());
    for event in events {
        // A kept/refused block changed nothing, so it stays out of the compression aggregates (it
        // would only dilute the ratio) and is counted on its own axis with its reason.
        if !event.kept_reason.is_empty() {
            summary.kept += 1;
            *summary
                .by_kept_reason
                .entry(event.kept_reason.clone())
                .or_insert(0) += 1;
            continue;
        }
        summary.blocks += 1;
        if event.verified {
            summary.verified += 1;
        }
        if event.inline {
            summary.inline += 1;
        } else {
            summary.offloaded += 1;
        }
        summary.input_tokens += event.input_tokens;
        summary.output_tokens += event.output_tokens;
        summary.saved_usd += event.saved_usd;
        *summary
            .by_transform
            .entry(event.transform.clone())
            .or_insert(0) += 1;
        *summary
            .transform_saved
            .entry(event.transform.clone())
            .or_insert(0) += event.input_tokens.saturating_sub(event.output_tokens);
        *summary.by_surface.entry(event.surface.clone()).or_insert(0) += 1;
        if !event.model.is_empty() {
            *summary.by_model.entry(event.model.clone()).or_insert(0) += 1;
        }
        if !event.platform.is_empty() {
            *summary
                .by_platform
                .entry(event.platform.clone())
                .or_insert(0) += 1;
        }
        let removed = event.input_tokens.saturating_sub(event.output_tokens);
        let at = event.at_ms as i64;
        if now.saturating_sub(event.at_ms) <= 3_600_000 {
            summary.saved_hour += removed;
        }
        if at >= today_start {
            summary.saved_today += removed;
        }
        if at >= week_start {
            summary.saved_week += removed;
        }
        if at >= month_start {
            summary.saved_month += removed;
        }
        cumulative += event.saved_usd;
        cumulative_tokens += event.input_tokens.saturating_sub(event.output_tokens);
        full_curve.push(Point {
            at_ms: event.at_ms,
            cumulative_usd: cumulative,
            cumulative_tokens,
        });
    }

    summary.seen = summary.blocks + summary.kept;
    summary.recent = events.iter().rev().take(RECENT).cloned().collect();
    summary.curve = downsample(full_curve);
    summary
}

// Keep the newest point exact, thin older ones by stride: bounded curve that never
// drops the current total.
fn downsample(mut points: Vec<Point>) -> Vec<Point> {
    if points.len() <= CURVE_POINTS {
        return points;
    }
    let last = points.pop();
    let stride = points.len().div_ceil(CURVE_POINTS - 1);
    let mut thinned: Vec<Point> = points.into_iter().step_by(stride).collect();
    if let Some(last) = last {
        thinned.push(last);
    }
    thinned
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(saved: f64, inline: bool, verified: bool, transform: &str) -> Event {
        Event {
            at_ms: 0,
            surface: "test".into(),
            transform: transform.into(),
            input_tokens: 100,
            output_tokens: 40,
            saved_usd: saved,
            verified,
            inline,
            ..Default::default()
        }
    }

    #[test]
    fn summarize_totals_and_splits() {
        let events = vec![
            event(0.10, true, true, "columnar"),
            event(0.05, false, true, "offload"),
            event(0.0, true, false, "verbatim"),
        ];
        let summary = summarize(&events, 0);
        assert_eq!(summary.blocks, 3);
        assert_eq!(summary.verified, 2);
        assert_eq!(summary.inline, 2);
        assert_eq!(summary.offloaded, 1);
        assert!((summary.saved_usd - 0.15).abs() < 1e-9);
        assert_eq!(summary.by_transform.get("columnar"), Some(&1));
        assert_eq!(summary.recent.len(), 3);
    }

    #[test]
    fn summarize_counts_kept_blocks_apart_from_compressed() {
        let refused = Event {
            kept_reason: "no_saving".into(),
            input_tokens: 50,
            output_tokens: 50,
            verified: true,
            ..Default::default()
        };
        let summary = summarize(&[event(0.10, true, true, "columnar"), refused], 0);
        assert_eq!(
            summary.blocks, 1,
            "only the compressed block counts as a compression"
        );
        assert_eq!(summary.kept, 1);
        assert_eq!(summary.seen, 2);
        assert_eq!(summary.by_kept_reason.get("no_saving"), Some(&1));
        // The kept block's equal in/out tokens must not dilute the compression totals.
        assert_eq!(summary.input_tokens, 100);
        assert_eq!(summary.output_tokens, 40);
        assert_eq!(
            summary.by_transform.get("kept"),
            None,
            "a kept block is not a transform"
        );
    }

    #[test]
    fn curve_stays_bounded_and_keeps_the_final_total() {
        let events: Vec<Event> = (0..1000).map(|_| event(0.001, true, true, "log")).collect();
        let summary = summarize(&events, 0);
        assert!(summary.curve.len() <= CURVE_POINTS);
        let last = summary.curve.last().unwrap();
        assert!((last.cumulative_usd - summary.saved_usd).abs() < 1e-9);
    }

    #[test]
    fn recent_is_newest_first() {
        let events = vec![event(0.01, true, true, "a"), event(0.02, true, true, "b")];
        let summary = summarize(&events, 0);
        assert_eq!(summary.recent[0].transform, "b");
    }

    #[test]
    fn civil_calendar_round_trips() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(days_from_civil(2026, 7, 18)), (2026, 7, 18));
        let d = days_from_civil(2026, 7, 18);
        let (y, m, _) = civil_from_days(d);
        assert_eq!(days_from_civil(y, m, 1), days_from_civil(2026, 7, 1));
    }

    #[test]
    fn calendar_buckets_place_events_by_period() {
        let now = now_ms();
        let mk = |ago: u64| Event {
            at_ms: now.saturating_sub(ago),
            input_tokens: 100,
            output_tokens: 40,
            verified: true,
            inline: true,
            ..Default::default()
        };
        // A fresh block and one from 40 days ago: only the fresh one is in every
        // calendar bucket; both are in lifetime.
        let s = summarize(&[mk(0), mk(40 * 86_400_000)], 0);
        assert_eq!(s.saved_hour, 60);
        assert_eq!(s.saved_today, 60);
        assert_eq!(s.saved_month, 60);
        assert_eq!(s.input_tokens - s.output_tokens, 120);
    }
}
