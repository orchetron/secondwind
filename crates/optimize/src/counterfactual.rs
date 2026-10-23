use std::collections::BTreeMap;
use std::collections::HashMap;
use std::collections::HashSet;

use secondwind_core::{Role, SegmentKind, Trace};

// Reopen rate from real traces: a block counts as reopened when the assistant later
// emits a token the block introduced (first appearance anywhere) that the preview hid.
// Requiring the block to introduce the token screens out identifiers that recur by chance.

#[derive(Default, Debug, Clone, Copy)]
pub struct ShapeStat {
    pub offloads: u64,
    pub reopens: u64,
}

impl ShapeStat {
    pub fn rate(&self) -> f64 {
        if self.offloads == 0 {
            0.0
        } else {
            self.reopens as f64 / self.offloads as f64
        }
    }
}

#[derive(Default, Debug)]
pub struct ReopenStats {
    pub by_shape: BTreeMap<String, ShapeStat>,
}

impl ReopenStats {
    pub fn merge(&mut self, other: &ReopenStats) {
        for (shape, stat) in &other.by_shape {
            let entry = self.by_shape.entry(shape.clone()).or_default();
            entry.offloads += stat.offloads;
            entry.reopens += stat.reopens;
        }
    }
}

const MIN_TOKEN: usize = 6;
const MAX_RESIDUAL_TOKENS: usize = 400;

pub fn measure(trace: &Trace) -> ReopenStats {
    let first_seen = first_seen_index(trace);
    let assistant_tokens = assistant_token_index(trace);
    let mut stats = ReopenStats::default();

    for (ti, turn) in trace.turns.iter().enumerate() {
        for segment in &turn.segments {
            if !matches!(segment.kind, SegmentKind::ToolResult { .. }) {
                continue;
            }
            let body = &segment.effective;
            let Some(preview) = crate::offload::preview_if_offloaded(body) else {
                continue;
            };
            let residual = residual_tokens(body, &preview);
            if residual.is_empty() {
                continue;
            }
            let reopened = residual.iter().any(|tok| {
                first_seen.get(tok) == Some(&ti) && references_after(&assistant_tokens, tok, ti)
            });
            let stat = stats.by_shape.entry(classify(body)).or_default();
            stat.offloads += 1;
            if reopened {
                stat.reopens += 1;
            }
        }
    }
    stats
}

fn first_seen_index(trace: &Trace) -> HashMap<String, usize> {
    let mut first: HashMap<String, usize> = HashMap::new();
    for (ti, turn) in trace.turns.iter().enumerate() {
        for segment in &turn.segments {
            for tok in significant(&segment.effective) {
                first.entry(tok.to_string()).or_insert(ti);
            }
        }
    }
    first
}

fn residual_tokens(body: &str, preview: &str) -> HashSet<String> {
    let shown: HashSet<&str> = significant(preview).collect();
    let mut residual = HashSet::new();
    for tok in significant(body) {
        if !shown.contains(tok) {
            residual.insert(tok.to_string());
            if residual.len() >= MAX_RESIDUAL_TOKENS {
                break;
            }
        }
    }
    residual
}

fn significant(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| c.is_whitespace())
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()))
        .filter(|w| w.len() >= MIN_TOKEN)
}

fn assistant_token_index(trace: &Trace) -> HashMap<String, Vec<usize>> {
    let mut index: HashMap<String, Vec<usize>> = HashMap::new();
    for (ti, turn) in trace.turns.iter().enumerate() {
        if turn.role != Role::Assistant {
            continue;
        }
        let mut seen = HashSet::new();
        for segment in &turn.segments {
            for tok in significant(&segment.effective) {
                if seen.insert(tok) {
                    index.entry(tok.to_string()).or_default().push(ti);
                }
            }
        }
    }
    index
}

fn references_after(index: &HashMap<String, Vec<usize>>, token: &str, after: usize) -> bool {
    index
        .get(token)
        .is_some_and(|turns| turns.iter().any(|&t| t > after))
}

// One tool result as the optimizer would have handled it, and whether the model later needed what
// it hid. Only an offload can hide anything: an inline codec keeps every value in view.
#[derive(Debug, Clone)]
pub struct BlockOutcome {
    pub tool: String,
    pub class: String,
    pub shape: String,
    pub outcome: &'static str,
    pub turn: usize,
    pub raw_bytes: usize,
    pub raw_tokens: u64,
    pub sent_tokens: u64,
    pub reopened: bool,
    pub faithful: bool,
    // Context the model held when it read this result: what one more turn re-reads, from billing.
    pub context_tokens: u64,
    // Assistant turns after this result: how many times its tokens are read again.
    pub turns_after: usize,
    // For an offload, the kind of preview the model saw in its place.
    pub preview: &'static str,
}

#[derive(Default, Clone, Copy)]
pub struct EvalOptions<'a> {
    pub gate: Option<&'a crate::policy::ReopenPolicy>,
    // Pass the user's latest message as the query, as the proxy does with live traffic.
    pub query: bool,
}

// Runs the optimizer over every tool result in a trace. Takes a fresh optimizer per trace so the
// offload store never outlives it. With a gate, a block whose predicted reopen rate is at or over its
// threshold is compressed with offloading off, so only inline codecs apply. A hidden token counts as needed only when the assistant uses it
// before any later tool result or user message supplies it again, so a grep followed by a read of
// the same file charges the read, not the grep.
pub fn evaluate(
    trace: &Trace,
    optimizer: &mut crate::Optimizer,
    counter: &dyn crate::tokens::TokenCounter,
    options: EvalOptions,
) -> Vec<BlockOutcome> {
    let gate = options.gate;
    let mut prompt = vec![""; trace.turns.len()];
    let mut latest = "";
    for (ti, turn) in trace.turns.iter().enumerate() {
        if turn.role == Role::User {
            let text = turn
                .segments
                .iter()
                .filter(|s| matches!(s.kind, SegmentKind::Text))
                .map(|s| s.effective.as_str())
                .find(|t| !t.trim().is_empty());
            if let Some(text) = text {
                latest = text;
            }
        }
        prompt[ti] = latest;
    }
    let first_seen = first_seen_index(trace);
    let assistant_tokens = assistant_token_index(trace);
    let supplied = supply_index(trace);
    let needed = |tok: &str, ti: usize| {
        let resupplied = supplied
            .get(tok)
            .and_then(|turns| turns.iter().copied().find(|&s| s > ti))
            .unwrap_or(usize::MAX);
        assistant_tokens
            .get(tok)
            .is_some_and(|turns| turns.iter().any(|&t| t > ti && t < resupplied))
    };
    let calls: HashMap<&str, (&str, &str)> = trace
        .turns
        .iter()
        .flat_map(|turn| &turn.segments)
        .filter_map(|segment| match &segment.kind {
            SegmentKind::ToolCall { name, id: Some(id) } => {
                Some((id.as_str(), (name.as_str(), segment.effective.as_str())))
            }
            _ => None,
        })
        .collect();

    let mut next_context = vec![0u64; trace.turns.len()];
    let mut assistant_after = vec![0usize; trace.turns.len()];
    let mut seen = 0;
    let mut later = 0;
    for (ti, turn) in trace.turns.iter().enumerate().rev() {
        assistant_after[ti] = later;
        if turn.role == Role::Assistant {
            later += 1;
        }
        if let Some(b) = turn.billing.filter(|_| turn.role == Role::Assistant) {
            seen = b.input_tokens
                + b.cache_read_tokens
                + b.cache_write_5m_tokens
                + b.cache_write_1h_tokens;
        }
        next_context[ti] = seen;
    }

    let mut out = Vec::new();
    for (ti, turn) in trace.turns.iter().enumerate() {
        for segment in &turn.segments {
            let SegmentKind::ToolResult { tool, id } = &segment.kind else {
                continue;
            };
            let body = segment.original.as_deref().unwrap_or(&segment.effective);
            if body.is_empty() {
                continue;
            }
            let input = id.as_deref().and_then(|id| calls.get(id)).map(|c| c.1);
            let raw_tokens = counter.count(body) as u64;
            let class = command_class(tool, input);
            let mode = optimizer.offload_mode();
            if gate.is_some_and(|g| !g.allows_offload(&class, raw_tokens)) {
                optimizer.set_offload_mode(crate::OffloadMode::Off);
            }
            // Faithful is checked here independently of the optimizer's own gate: an inline wire
            // must decode to the certified content, an offload must resolve to the original.
            let compressed = if options.query && !prompt[ti].is_empty() {
                optimizer.compress_block_with_query(body, prompt[ti])
            } else {
                optimizer.compress_block(body)
            };
            let (outcome, sent, hidden_from, faithful) = match compressed {
                crate::Outcome::Compressed { wire, .. } => {
                    let ok = crate::certificate::verify(&wire, &crate::certificate::certify(body));
                    ("inline", wire, None, ok)
                }
                crate::Outcome::Offloaded { stub, marker, .. } => {
                    let ok = optimizer.resolve(&marker).is_some_and(|back| {
                        same_content(&back, body) || split_covers(&stub, &back, body)
                    });
                    ("offload", stub.clone(), Some(stub), ok)
                }
                crate::Outcome::KeptVerbatim { .. } => ("verbatim", body.to_string(), None, true),
            };
            optimizer.set_offload_mode(mode);
            let reopened = hidden_from.is_some_and(|stub| {
                residual_tokens(body, &stub)
                    .iter()
                    .any(|tok| first_seen.get(tok) == Some(&ti) && needed(tok, ti))
            });
            out.push(BlockOutcome {
                tool: tool.clone(),
                class,
                shape: classify(body),
                outcome,
                turn: ti,
                raw_bytes: body.len(),
                raw_tokens,
                sent_tokens: counter.count(&sent) as u64,
                reopened,
                faithful,
                context_tokens: next_context[ti],
                turns_after: assistant_after[ti],
                preview: if outcome == "offload" {
                    preview_kind(&sent)
                } else {
                    ""
                },
            });
        }
    }
    out
}

// A relevance split keeps some rows inline in the stub and stores the rest: faithful when the inline
// rows and the stored ones are, together, exactly the original's rows.
fn split_covers(stub: &str, back: &str, original: &str) -> bool {
    use serde_json::Value;
    let Some(cut) = stub.rfind("\n[") else {
        return false;
    };
    let inline = &stub[..cut];
    let rows = |v: Option<Value>| -> Option<Vec<String>> {
        let mut rows: Vec<String> = v?
            .as_array()?
            .iter()
            .map(crate::atom::canonicalize)
            .collect();
        rows.sort();
        Some(rows)
    };
    let shown = serde_json::from_str::<Value>(inline)
        .ok()
        .or_else(|| crate::columnar::decode(inline));
    let (Some(mut shown), Some(stored), Some(whole)) = (
        rows(shown),
        rows(serde_json::from_str(back).ok()),
        rows(serde_json::from_str(original).ok()),
    ) else {
        return false;
    };
    shown.extend(stored);
    shown.sort();
    shown == whole
}

// Byte-identical text, or JSON holding the same value; an offload stores JSON canonicalized.
fn same_content(back: &str, original: &str) -> bool {
    if back == original {
        return true;
    }
    match (
        serde_json::from_str::<serde_json::Value>(back),
        serde_json::from_str::<serde_json::Value>(original),
    ) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

// The preview an offload stub carries, read from its label, so it names what the model saw.
pub fn preview_kind(stub: &str) -> &'static str {
    let label = stub.lines().next().unwrap_or("");
    const KINDS: [(&str, &str); 7] = [
        ("offloaded, diff summary", "diff summary"),
        ("offloaded, match map", "match map"),
        ("offloaded, code outline", "code outline"),
        ("offloaded, content table", "content table"),
        ("offloaded, json index", "json index"),
        ("offloaded, prose digest", "prose digest"),
        ("offloaded, preview:", "head"),
    ];
    if let Some((_, kind)) = KINDS.iter().find(|(tag, _)| label.contains(tag)) {
        return kind;
    }
    if stub.contains("less relevant to the query") {
        return "relevance split";
    }
    "other"
}

// What produced a result: the tool, and for a shell call the program whose output it is.
pub fn command_class(tool: &str, input: Option<&str>) -> String {
    if tool != "Bash" {
        return tool.to_string();
    }
    let command = input
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
        .and_then(|v| v["command"].as_str().map(str::to_string))
        .unwrap_or_default();
    let command = without_heredoc_bodies(&command);
    let words: Vec<&str> = command
        .split(|c: char| c.is_whitespace() || c == ';')
        .collect();
    let opens_loop = words
        .iter()
        .any(|w| matches!(*w, "for" | "while" | "until"));
    if opens_loop && words.contains(&"do") || words.first() == Some(&"if") {
        return "Bash:loop".to_string();
    }
    let last = command
        .split(['\n', ';'])
        .flat_map(|part| part.split("&&"))
        .flat_map(|part| part.split("||"))
        .map(str::trim)
        .filter(|part| {
            let head = part.split_whitespace().next().unwrap_or("");
            !part.is_empty()
                && !head.starts_with('#')
                && !matches!(
                    head,
                    "cd" | "echo" | "printf" | "export" | "set" | "source" | "true" | "sleep"
                )
        })
        .last()
        .unwrap_or("");
    let first_stage = last.split('|').next().unwrap_or("").trim();
    let mut words = first_stage
        .split_whitespace()
        .skip_while(|w| {
            (w.contains('=') && !w.starts_with('-'))
                || matches!(*w, "sudo" | "time" | "nohup" | "env" | "command")
        })
        .peekable();
    if words.peek() == Some(&"timeout") {
        words.next();
        words.next();
    }
    let program = words
        .next()
        .unwrap_or("")
        .trim_matches(['"', '\''])
        .rsplit('/')
        .next()
        .unwrap_or("");
    let program = match program {
        "" => return "Bash:other".to_string(),
        "python" | "python3" | "uv" | "npx" | "bunx" => {
            let next = words
                .find(|w| !w.starts_with('-') && !w.starts_with('<'))
                .unwrap_or("-");
            return format!("Bash:{program} {}", next.rsplit('/').next().unwrap_or(""));
        }
        "git" | "cargo" | "npm" | "gh" | "pnpm" | "bun" | "docker" | "kubectl" => {
            let mut sub = "";
            while let Some(w) = words.next() {
                if matches!(
                    w,
                    "-C" | "-c" | "--git-dir" | "--work-tree" | "--manifest-path"
                ) {
                    words.next();
                } else if !w.starts_with('-') {
                    sub = w;
                    break;
                }
            }
            return format!("Bash:{program} {sub}");
        }
        other => other,
    };
    let piped = if last.contains('|') { " |" } else { "" };
    format!("Bash:{program}{piped}")
}

// Drops the body of every `<<WORD` heredoc so its lines are not read as commands.
fn without_heredoc_bodies(command: &str) -> String {
    let mut out = String::new();
    let mut end: Option<String> = None;
    for line in command.lines() {
        if let Some(word) = &end {
            if line.trim() == word {
                end = None;
            }
            continue;
        }
        out.push_str(line);
        out.push('\n');
        if let Some(at) = line.find("<<") {
            let word: String = line[at + 2..]
                .trim_start_matches('-')
                .trim_start()
                .trim_start_matches(['\'', '"'])
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            if !word.is_empty() {
                end = Some(word);
            }
        }
    }
    out
}

// Turns where a token reaches the model from outside: tool results and user messages.
fn supply_index(trace: &Trace) -> HashMap<String, Vec<usize>> {
    let mut index: HashMap<String, Vec<usize>> = HashMap::new();
    for (ti, turn) in trace.turns.iter().enumerate() {
        if turn.role == Role::Assistant {
            continue;
        }
        let mut seen = HashSet::new();
        for segment in &turn.segments {
            for tok in significant(&segment.effective) {
                if seen.insert(tok) {
                    index.entry(tok.to_string()).or_default().push(ti);
                }
            }
        }
    }
    index
}

fn classify(body: &str) -> String {
    if body.contains("diff --git") {
        "diff".to_string()
    } else if crate::search::location_map(body, usize::MAX).is_some() {
        "search".to_string()
    } else {
        "other".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use secondwind_core::{Origin, Party};
    use secondwind_core::{Provenance, Segment, Turn};

    fn tool_turn(index: usize, body: &str) -> Turn {
        Turn {
            index,
            role: Role::Tool,
            timestamp: None,
            model: None,
            sidechain: false,
            segments: vec![Segment {
                kind: SegmentKind::ToolResult {
                    tool: "grep".into(),
                    id: None,
                },
                original: None,
                effective: body.to_string(),
            }],
            billing: None,
        }
    }

    fn assistant_turn(index: usize, text: &str) -> Turn {
        Turn {
            index,
            role: Role::Assistant,
            timestamp: None,
            model: None,
            sidechain: false,
            segments: vec![Segment {
                kind: SegmentKind::Text,
                original: None,
                effective: text.to_string(),
            }],
            billing: None,
        }
    }

    fn trace(turns: Vec<Turn>) -> Trace {
        Trace {
            id: "t".into(),
            source: "test".into(),
            optimizer: None,
            provenance: Provenance {
                origin: Origin::RealWork,
                party: Party::FirstParty,
            },
            turns,
        }
    }

    fn big_search_body(marker: &str) -> String {
        let mut out = String::new();
        for i in 0..40 {
            out.push_str(&format!(
                "src/module_{i}.rs:{i}:snippet {marker}_{i} here\n"
            ));
        }
        out
    }

    // A body the optimizer offloads as a code outline, so later lines stay hidden.
    fn code_body() -> String {
        (0..200)
            .map(|i| {
                format!(
                    "fn handler_{i}(input: &str) -> Result<usize> {{ let key = uniquetoken{i} ; parse(input).map(|v| v * {i}) }}\n"
                )
            })
            .collect()
    }

    fn evaluated(turns: Vec<Turn>) -> Vec<BlockOutcome> {
        let mut optimizer = crate::Optimizer::default();
        evaluate(
            &trace(turns),
            &mut optimizer,
            &crate::tokens::ByteCounter,
            EvalOptions::default(),
        )
    }

    #[test]
    fn eval_charges_a_reopen_to_the_result_that_hid_the_token() {
        let out = evaluated(vec![
            tool_turn(0, &code_body()),
            assistant_turn(1, "I will use uniquetoken137 from the results"),
        ]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].outcome, "offload");
        assert!(out[0].reopened);
        assert!(out[0].faithful);
        assert!(out[0].sent_tokens < out[0].raw_tokens);
    }

    #[test]
    fn eval_charges_a_later_result_that_supplies_the_token_again() {
        let out = evaluated(vec![
            tool_turn(0, &code_body()),
            tool_turn(1, "the file holds uniquetoken137 on its first line"),
            assistant_turn(2, "I will use uniquetoken137 from the file"),
        ]);
        assert_eq!(out[0].outcome, "offload");
        assert!(!out[0].reopened, "the model read it from the second result");
    }

    #[test]
    fn a_gate_keeps_a_block_it_expects_reopened_in_view() {
        let mut policy = crate::policy::ReopenPolicy::learn(std::iter::empty(), 1);
        policy.prior = 0.9;
        let mut optimizer = crate::Optimizer::default();
        let out = evaluate(
            &trace(vec![
                tool_turn(0, &code_body()),
                assistant_turn(1, "I will use uniquetoken137 from the results"),
            ]),
            &mut optimizer,
            &crate::tokens::ByteCounter,
            EvalOptions {
                gate: Some(&policy),
                ..EvalOptions::default()
            },
        );
        assert_ne!(out[0].outcome, "offload");
        assert!(!out[0].reopened, "nothing was hidden, so nothing to reopen");
        assert!(out[0].faithful);
        assert_eq!(optimizer.offload_mode(), crate::OffloadMode::Auto);
    }

    #[test]
    fn every_shape_the_optimizer_touches_is_faithful() {
        let json = format!(
            "[{}]",
            (0..60)
                .map(|i| format!(
                    r#"{{"id":{i},"name":"item {i}","tags":["a","b"],"score":{i}.5}}"#
                ))
                .collect::<Vec<_>>()
                .join(",")
        );
        let log: String = (0..300)
            .map(|i| {
                format!(
                    "2026-10-23T01:{:02}:00Z INFO worker-{} handled job {i} in {}ms\n",
                    i % 60,
                    i % 7,
                    i * 3
                )
            })
            .collect();
        let out = evaluated(vec![
            tool_turn(0, &json),
            tool_turn(1, &log),
            tool_turn(2, &big_search_body("needle")),
        ]);
        assert_eq!(out.len(), 3);
        for block in &out {
            assert!(block.faithful, "{} came back different", block.outcome);
        }
        assert!(out.iter().any(|b| b.outcome != "verbatim"));
    }

    #[test]
    fn a_relevance_split_is_faithful_across_stub_and_store() {
        let rows: Vec<String> = (0..40)
            .map(|i| {
                let status = if i % 7 == 0 { "failing" } else { "healthy" };
                format!(r#"{{"id":{i},"name":"service number {i}","status":"{status}","region":"zone-{}"}}"#, i % 3)
            })
            .collect();
        let body = format!("[{}]", rows.join(","));
        let mut optimizer = crate::Optimizer::default();
        let crate::Outcome::Offloaded { stub, marker, .. } =
            optimizer.compress_block_with_query(&body, "which services are failing")
        else {
            panic!("expected a relevance split");
        };
        let back = optimizer.resolve(&marker).unwrap();
        assert!(!same_content(&back, &body), "the store holds only the rest");
        assert!(split_covers(&stub, &back, &body));
        let missing = format!("[{}]", rows[1..].join(","));
        assert!(
            !split_covers(&stub, &back, &missing),
            "a row too many is caught"
        );
        let extra = format!("[{},{}]", rows.join(","), rows[0]);
        assert!(
            !split_covers(&stub, &back, &extra),
            "a row missing is caught"
        );
    }

    #[test]
    fn same_content_accepts_reformatted_json_and_nothing_else() {
        assert!(same_content(
            r#"{"a":1,"b":[2,3]}"#,
            "{ \"b\": [2, 3], \"a\": 1 }"
        ));
        assert!(same_content("plain text\n", "plain text\n"));
        assert!(!same_content("plain text", "plain text\n"));
        assert!(!same_content(r#"{"a":1}"#, r#"{"a":2}"#));
    }

    #[test]
    fn command_class_names_the_program_whose_output_it_is() {
        let class = |command: &str| {
            command_class(
                "Bash",
                Some(&serde_json::json!({ "command": command }).to_string()),
            )
        };
        assert_eq!(class("cd repo && cat src/main.rs"), "Bash:cat");
        assert_eq!(class("grep -rn foo src | head -40"), "Bash:grep |");
        assert_eq!(class("git -C repo log --oneline -5"), "Bash:git log");
        assert_eq!(
            class("RUST_LOG=1 timeout 60 cargo test -p x"),
            "Bash:cargo test"
        );
        assert_eq!(class("\"$HOME/bin/tool\" --flag"), "Bash:tool");
        assert_eq!(
            class("python3 - <<'EOF'\nimport os\nprint(os.getcwd())\nEOF"),
            "Bash:python3 -"
        );
        assert_eq!(
            class("cat > f.txt <<EOF\nfor x in y; do\nEOF\nwc -l f.txt"),
            "Bash:wc"
        );
        assert_eq!(class("cd x; for f in *.rs; do wc -l $f; done"), "Bash:loop");
        assert_eq!(class("echo hi"), "Bash:other");
        assert_eq!(command_class("Read", None), "Read");
    }

    #[test]
    fn residual_referenced_later_counts_as_a_reopen() {
        let body = big_search_body("uniquetoken");
        let t = trace(vec![
            tool_turn(0, &body),
            assistant_turn(1, "I will use uniquetoken_7 from the results"),
        ]);
        let stats = measure(&t);
        let s = stats.by_shape.get("search").unwrap();
        assert_eq!(s.offloads, 1);
        assert_eq!(s.reopens, 1);
    }

    #[test]
    fn residual_never_referenced_is_not_a_reopen() {
        let body = big_search_body("uniquetoken");
        let t = trace(vec![
            tool_turn(0, &body),
            assistant_turn(1, "thanks, moving on to something unrelated entirely"),
        ]);
        let stats = measure(&t);
        let s = stats.by_shape.get("search").unwrap();
        assert_eq!(s.offloads, 1);
        assert_eq!(s.reopens, 0);
    }

    #[test]
    fn a_reference_before_the_result_does_not_count() {
        let body = big_search_body("uniquetoken");
        let t = trace(vec![
            assistant_turn(0, "please search for uniquetoken_7"),
            tool_turn(1, &body),
        ]);
        let stats = measure(&t);
        let s = stats.by_shape.get("search").unwrap();
        assert_eq!(s.reopens, 0);
    }

    #[test]
    fn a_token_the_block_did_not_introduce_does_not_count() {
        let body = big_search_body("uniquetoken");
        let t = trace(vec![
            assistant_turn(0, "context mentions uniquetoken_7 up front"),
            tool_turn(1, &body),
            assistant_turn(2, "later I still cite uniquetoken_7"),
        ]);
        let stats = measure(&t);
        let s = stats.by_shape.get("search").unwrap();
        assert_eq!(s.offloads, 1);
        assert_eq!(s.reopens, 0);
    }
}
