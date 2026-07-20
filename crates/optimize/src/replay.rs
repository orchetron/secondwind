use std::collections::HashMap;

use secondwind_core::{Role, SegmentKind, Trace};
use serde_json::{Value, json};

use crate::{Optimizer, Outcome, resolve};

// An action the agent took with an offloadable result in its context, replayed on
// the uncompressed and compressed context to compare the next choice.
pub struct Decision {
    pub system: Option<String>,
    pub baseline: Vec<Value>,
    pub treatment: Vec<Value>,
    pub tools: Vec<Value>,
    pub markers: HashMap<String, String>,
    pub ground_truth: String,
}

pub struct Reply {
    pub tool: Option<String>,
    pub resolve_calls: usize,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub errored: bool,
}

pub struct Request<'a> {
    pub system: Option<&'a str>,
    pub messages: &'a [Value],
    pub tools: &'a [Value],
}

// A decision where compression flipped the tool choice, with enough recent
// context for a judge to rate whether the compressed choice was acceptable.
pub struct Changed {
    pub ground_truth: String,
    pub baseline_tool: Option<String>,
    pub treatment_tool: Option<String>,
    pub recent: String,
}

pub trait Model {
    fn decide(&self, req: Request, resolve: &dyn Fn(&str) -> Option<String>) -> Reply;
}

#[derive(Default)]
pub struct Stats {
    pub decisions: u64,
    pub errors: u64,
    pub baseline_match: u64,
    pub treatment_match: u64,
    pub same_choice: u64,
    pub same_baseline: u64,
    pub resolved_when_offloaded: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

impl Stats {
    fn merge(&mut self, other: &Stats) {
        self.decisions += other.decisions;
        self.errors += other.errors;
        self.baseline_match += other.baseline_match;
        self.treatment_match += other.treatment_match;
        self.same_choice += other.same_choice;
        self.same_baseline += other.same_baseline;
        self.resolved_when_offloaded += other.resolved_when_offloaded;
        self.input_tokens += other.input_tokens;
        self.output_tokens += other.output_tokens;
    }
}

pub fn decisions(trace: &Trace) -> Vec<Decision> {
    let mut out = Vec::new();
    for (i, turn) in trace.turns.iter().enumerate() {
        if turn.role != Role::Assistant {
            continue;
        }
        let Some(action) = tool_action(turn) else {
            continue;
        };
        let prior = &trace.turns[..i];
        if !prior.iter().any(has_offloadable) {
            continue;
        }
        let (system, baseline) = messages(prior, None);
        let mut opt = Optimizer::default();
        let mut markers = HashMap::new();
        let (_, treatment) = messages(prior, Some((&mut opt, &mut markers)));
        out.push(Decision {
            system,
            baseline,
            treatment,
            tools: base_tools(trace),
            markers,
            ground_truth: action,
        });
    }
    out
}

const WORKERS: usize = 4;

pub fn run<M: Model + Sync>(
    traces: &[Trace],
    model: &M,
    max_decisions: usize,
) -> (Stats, Vec<Changed>) {
    let mut all = Vec::new();
    for trace in traces {
        for d in decisions(trace) {
            all.push(d);
            if max_decisions != 0 && all.len() >= max_decisions {
                break;
            }
        }
        if max_decisions != 0 && all.len() >= max_decisions {
            break;
        }
    }

    let cursor = std::sync::atomic::AtomicUsize::new(0);
    let mut stats = Stats::default();
    let mut changed = Vec::new();
    std::thread::scope(|scope| {
        let workers: Vec<_> = (0..WORKERS.min(all.len().max(1)))
            .map(|_| {
                scope.spawn(|| {
                    let mut local = Stats::default();
                    let mut local_changed = Vec::new();
                    loop {
                        let i = cursor.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let Some(d) = all.get(i) else { break };
                        score(d, model, &mut local, &mut local_changed);
                    }
                    (local, local_changed)
                })
            })
            .collect();
        for worker in workers {
            let (local, local_changed) = worker.join().unwrap();
            stats.merge(&local);
            changed.extend(local_changed);
        }
    });
    (stats, changed)
}

fn score<M: Model>(d: &Decision, model: &M, stats: &mut Stats, changed: &mut Vec<Changed>) {
    let none = |_: &str| None;
    let base_req = || Request {
        system: d.system.as_deref(),
        messages: &d.baseline,
        tools: &d.tools,
    };
    let base = model.decide(base_req(), &none);
    // Identical control: self-agreement rate is the noise floor compression's effect is measured against.
    let base2 = model.decide(base_req(), &none);
    let mut treatment_tools = d.tools.clone();
    resolve::inject_once(&mut treatment_tools);
    let resolver = |m: &str| d.markers.get(m).cloned();
    let treat = model.decide(
        Request {
            system: d.system.as_deref(),
            messages: &d.treatment,
            tools: &treatment_tools,
        },
        &resolver,
    );

    stats.input_tokens += base.input_tokens + base2.input_tokens + treat.input_tokens;
    stats.output_tokens += base.output_tokens + base2.output_tokens + treat.output_tokens;
    if base.errored || base2.errored || treat.errored {
        stats.errors += 1;
        return;
    }

    stats.decisions += 1;
    if base.tool.as_deref() == Some(d.ground_truth.as_str()) {
        stats.baseline_match += 1;
    }
    if treat.tool.as_deref() == Some(d.ground_truth.as_str()) {
        stats.treatment_match += 1;
    }
    if base.tool == treat.tool {
        stats.same_choice += 1;
    } else {
        changed.push(Changed {
            ground_truth: d.ground_truth.clone(),
            baseline_tool: base.tool.clone(),
            treatment_tool: treat.tool.clone(),
            recent: render_recent(&d.baseline),
        });
    }
    if base.tool == base2.tool {
        stats.same_baseline += 1;
    }
    if treat.resolve_calls > 0 {
        stats.resolved_when_offloaded += 1;
    }
    stats.input_tokens += base.input_tokens + treat.input_tokens;
    stats.output_tokens += base.output_tokens + treat.output_tokens;
}

// Input tokens the decisions would send, for a cost projection with no model call.
pub fn dry_run_tokens(traces: &[Trace], max_decisions: usize) -> (u64, u64) {
    let mut decisions_seen = 0u64;
    let mut tokens = 0u64;
    for trace in traces {
        for d in decisions(trace) {
            if max_decisions != 0 && decisions_seen >= max_decisions as u64 {
                return (decisions_seen, tokens);
            }
            decisions_seen += 1;
            tokens += approx_tokens(&d.baseline) + approx_tokens(&d.treatment);
        }
    }
    (decisions_seen, tokens)
}

fn approx_tokens(messages: &[Value]) -> u64 {
    let bytes: usize = messages.iter().map(|m| m.to_string().len()).sum();
    (bytes / 4) as u64
}

// Uncompressed vs compressed context tokens over the decisions: the savings the treatment buys. No model call.
pub fn context_savings(traces: &[Trace], max_decisions: usize) -> (u64, u64) {
    let mut seen = 0u64;
    let (mut baseline, mut treatment) = (0u64, 0u64);
    for trace in traces {
        for d in decisions(trace) {
            if max_decisions != 0 && seen >= max_decisions as u64 {
                return (baseline, treatment);
            }
            seen += 1;
            baseline += approx_tokens(&d.baseline);
            treatment += approx_tokens(&d.treatment);
        }
    }
    (baseline, treatment)
}

// The IR records tool names but not schemas, so offer a permissive def per name seen, letting the
// model pick the same tool in either condition. Treatment additionally gets the resolve tool.
fn base_tools(trace: &Trace) -> Vec<Value> {
    let mut names: Vec<&str> = Vec::new();
    for turn in &trace.turns {
        for seg in &turn.segments {
            if let SegmentKind::ToolCall { name, .. } = &seg.kind
                && !names.contains(&name.as_str())
            {
                names.push(name);
            }
        }
    }
    names
        .into_iter()
        .map(|name| json!({"name": name, "input_schema": {"type": "object"}}))
        .collect()
}

const RECENT_CHARS: usize = 1500;

fn render_recent(messages: &[Value]) -> String {
    let mut out = String::new();
    let start = messages.len().saturating_sub(2);
    for msg in &messages[start..] {
        for block in blocks(msg) {
            if let Some(text) = block["text"].as_str() {
                out.push_str(text);
            } else if let Some(content) = block["content"].as_str() {
                out.push_str(content);
            }
            out.push('\n');
        }
    }
    out.chars().take(RECENT_CHARS).collect()
}

fn tool_action(turn: &secondwind_core::Turn) -> Option<String> {
    turn.segments.iter().find_map(|s| match &s.kind {
        SegmentKind::ToolCall { name, .. } => Some(name.clone()),
        _ => None,
    })
}

fn has_offloadable(turn: &secondwind_core::Turn) -> bool {
    turn.segments.iter().any(|s| {
        matches!(s.kind, SegmentKind::ToolResult { .. })
            && matches!(
                Optimizer::default().compress_block(&s.effective),
                Outcome::Offloaded { .. }
            )
    })
}

type Compress<'a> = Option<(&'a mut Optimizer, &'a mut HashMap<String, String>)>;

fn messages(
    turns: &[secondwind_core::Turn],
    mut compress: Compress,
) -> (Option<String>, Vec<Value>) {
    let mut system = String::new();
    let mut coalesced: Vec<(&str, Vec<Value>)> = Vec::new();

    for turn in turns {
        let (role, blocks) = match turn.role {
            Role::System => {
                for seg in &turn.segments {
                    system.push_str(&seg.effective);
                }
                continue;
            }
            Role::User | Role::Tool => {
                let mut b = text_blocks(turn);
                b.extend(tool_result_blocks(turn, &mut compress));
                ("user", b)
            }
            Role::Assistant => ("assistant", assistant_blocks(turn)),
        };
        if blocks.is_empty() {
            continue;
        }
        match coalesced.last_mut() {
            Some((last, acc)) if *last == role => acc.extend(blocks),
            _ => coalesced.push((role, blocks)),
        }
    }

    let messages = coalesced
        .into_iter()
        .map(|(role, content)| json!({"role": role, "content": content}))
        .collect();
    ((!system.is_empty()).then_some(system), repair(messages))
}

// Re-pair the interleaved transcript to what the provider requires: each tool_use
// answered by id in the next message, no orphan results, a leading user turn.
fn repair(messages: Vec<Value>) -> Vec<Value> {
    let mut result_by_id: HashMap<String, Value> = HashMap::new();
    for msg in &messages {
        for block in tool_result_blocks_of(msg) {
            if let Some(id) = block["tool_use_id"].as_str() {
                result_by_id.entry(id.to_string()).or_insert(block);
            }
        }
    }

    let mut out: Vec<Value> = Vec::new();
    for msg in messages {
        let ids = tool_use_ids(&msg);
        if ids.is_empty() {
            out.push(without_orphan_results(msg));
            continue;
        }
        out.push(msg);
        let results: Vec<Value> = ids
            .into_iter()
            .map(|id| {
                result_by_id.get(&id).cloned().unwrap_or_else(|| {
                    json!({"type": "tool_result", "tool_use_id": id, "content": "[result omitted]"})
                })
            })
            .collect();
        out.push(json!({"role": "user", "content": results}));
    }

    out.retain(|m| !m["content"].as_array().is_some_and(|c| c.is_empty()));
    while out.first().is_some_and(|m| m["role"] != "user") {
        out.remove(0);
    }
    out
}

fn tool_use_ids(msg: &Value) -> Vec<String> {
    blocks(msg)
        .iter()
        .filter(|b| b["type"] == "tool_use")
        .filter_map(|b| b["id"].as_str().map(str::to_string))
        .collect()
}

fn tool_result_blocks_of(msg: &Value) -> Vec<Value> {
    blocks(msg)
        .iter()
        .filter(|b| b["type"] == "tool_result")
        .cloned()
        .collect()
}

fn without_orphan_results(mut msg: Value) -> Value {
    if let Some(content) = msg["content"].as_array() {
        let kept: Vec<Value> = content
            .iter()
            .filter(|b| b["type"] != "tool_result")
            .cloned()
            .collect();
        msg["content"] = json!(kept);
    }
    msg
}

fn blocks(msg: &Value) -> &[Value] {
    msg["content"].as_array().map(Vec::as_slice).unwrap_or(&[])
}

fn text_blocks(turn: &secondwind_core::Turn) -> Vec<Value> {
    turn.segments
        .iter()
        .filter(|s| matches!(s.kind, SegmentKind::Text))
        .filter(|s| !s.effective.is_empty())
        .map(|s| json!({"type": "text", "text": s.effective}))
        .collect()
}

fn assistant_blocks(turn: &secondwind_core::Turn) -> Vec<Value> {
    let mut blocks = Vec::new();
    for seg in &turn.segments {
        match &seg.kind {
            SegmentKind::Text if !seg.effective.is_empty() => {
                blocks.push(json!({"type": "text", "text": seg.effective}));
            }
            SegmentKind::ToolCall { name, id } => {
                blocks.push(json!({
                    "type": "tool_use",
                    "id": id.clone().unwrap_or_else(|| format!("tu_{name}")),
                    "name": name,
                    "input": {},
                }));
            }
            _ => {}
        }
    }
    blocks
}

fn tool_result_blocks(turn: &secondwind_core::Turn, compress: &mut Compress) -> Vec<Value> {
    let mut blocks = Vec::new();
    for seg in &turn.segments {
        let SegmentKind::ToolResult { id, .. } = &seg.kind else {
            continue;
        };
        let content = match compress {
            Some((opt, markers)) => match opt.compress_block(&seg.effective) {
                Outcome::Offloaded { stub, marker, .. } => {
                    if let Some(body) = opt.resolve(&marker) {
                        markers.insert(marker.clone(), body.to_string());
                    }
                    stub
                }
                Outcome::Compressed { wire, .. } => wire,
                Outcome::KeptVerbatim { .. } => seg.effective.clone(),
            },
            None => seg.effective.clone(),
        };
        blocks.push(json!({
            "type": "tool_result",
            "tool_use_id": id.clone().unwrap_or_else(|| "tr".to_string()),
            "content": content,
        }));
    }
    blocks
}

#[cfg(test)]
mod tests {
    use super::*;
    use secondwind_core::{Origin, Party, Provenance, Segment, Turn};

    fn turn(index: usize, role: Role, segs: Vec<Segment>) -> Turn {
        Turn {
            index,
            role,
            timestamp: None,
            model: None,
            sidechain: false,
            segments: segs,
            billing: None,
        }
    }

    fn text(t: &str) -> Segment {
        Segment {
            kind: SegmentKind::Text,
            original: None,
            effective: t.to_string(),
        }
    }

    fn tool_result(id: &str, body: &str) -> Segment {
        Segment {
            kind: SegmentKind::ToolResult {
                tool: "grep".into(),
                id: Some(id.into()),
            },
            original: None,
            effective: body.to_string(),
        }
    }

    fn tool_call(name: &str) -> Segment {
        Segment {
            kind: SegmentKind::ToolCall {
                name: name.into(),
                id: Some("tu1".into()),
            },
            original: None,
            effective: String::new(),
        }
    }

    fn offloadable_result() -> String {
        let mut out = String::new();
        for i in 0..12 {
            out.push_str(&format!(
                "The handler_{i} module validates every inbound request against the shared session store before dispatch. \
When the token has expired the gateway returns a rejection and records the client address for the audit trail. \
"
            ));
        }
        out
    }

    fn sample_trace() -> Trace {
        Trace {
            id: "t".into(),
            source: "test".into(),
            optimizer: None,
            provenance: Provenance {
                origin: Origin::RealWork,
                party: Party::FirstParty,
            },
            turns: vec![
                turn(0, Role::User, vec![text("find the handler")]),
                turn(1, Role::Assistant, vec![tool_call("grep")]),
                turn(
                    2,
                    Role::Tool,
                    vec![tool_result("tu1", &offloadable_result())],
                ),
                turn(3, Role::Assistant, vec![tool_call("read_file")]),
            ],
        }
    }

    #[test]
    fn finds_a_decision_after_an_offloadable_result() {
        let ds = decisions(&sample_trace());
        assert_eq!(ds.len(), 1);
        assert_eq!(ds[0].ground_truth, "read_file");
    }

    #[test]
    fn treatment_offloads_the_result_and_keeps_it_resolvable() {
        let d = &decisions(&sample_trace())[0];
        let baseline = serde_json::to_string(&d.baseline).unwrap();
        let treatment = serde_json::to_string(&d.treatment).unwrap();
        assert!(treatment.contains("swload:"));
        assert!(treatment.len() < baseline.len());
        assert_eq!(d.markers.len(), 1);
        let (marker, body) = d.markers.iter().next().unwrap();
        assert!(treatment.contains(marker));
        assert!(body.contains("handler_0"));
    }

    #[test]
    fn scores_agreement_against_ground_truth() {
        struct Fixed;
        impl Model for Fixed {
            fn decide(&self, _: Request, _: &dyn Fn(&str) -> Option<String>) -> Reply {
                Reply {
                    tool: Some("read_file".into()),
                    resolve_calls: 1,
                    input_tokens: 10,
                    output_tokens: 2,
                    errored: false,
                }
            }
        }
        let (stats, _changed) = run(&[sample_trace()], &Fixed, 0);
        assert_eq!(stats.decisions, 1);
        assert_eq!(stats.baseline_match, 1);
        assert_eq!(stats.treatment_match, 1);
        assert_eq!(stats.resolved_when_offloaded, 1);
    }

    #[test]
    fn dry_run_counts_without_a_model() {
        let (n, tokens) = dry_run_tokens(&[sample_trace()], 0);
        assert_eq!(n, 1);
        assert!(tokens > 0);
    }
}
