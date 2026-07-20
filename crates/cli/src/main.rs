#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Parser;
use secondwind_ledger::LedgerBuilder;
use secondwind_report::{Audit, Html, Json, Markdown, Reporter, Terminal};
use secondwind_sources::Enricher;

mod agents;
mod dashboard;
mod embedder;
mod mcp;
mod prose_shrinker;
mod proxy;
mod replay;
mod setup;

#[derive(Parser)]
#[command(
    name = "secondwind",
    version,
    about = "Audits what context optimization actually did to your agent sessions"
)]
struct Args {
    #[command(subcommand)]
    command: Option<Command>,
    #[arg(long, conflicts_with = "md")]
    json: bool,
    #[arg(long)]
    md: bool,
    #[arg(long, conflicts_with_all = ["json", "md"])]
    html: bool,
    #[arg(long, value_name = "DIR", global = true)]
    home: Option<PathBuf>,
}

#[derive(clap::Subcommand)]
enum Command {
    #[command(about = "Record original vs on-wire context through a local pass-through proxy")]
    Tap {
        #[arg(long, default_value = "127.0.0.1:8788")]
        listen: String,
        #[arg(long, default_value = "https://api.anthropic.com")]
        upstream: String,
    },
    #[command(about = "Re-run the detectors over a stored trace and print its findings")]
    Repro { fixture: PathBuf },
    #[command(
        about = "Aggregate a corpus of traces into scoreboard files (refuses unredacted input)"
    )]
    Scoreboard {
        #[arg(long, value_name = "DIR")]
        corpus: PathBuf,
        #[arg(long, value_name = "DIR")]
        out: PathBuf,
    },
    #[command(about = "Redact secrets and identifying paths from a trace, print sanitized JSON")]
    Redact { input: PathBuf },
    #[command(about = "Compress one tool-output block through the optimizer, print JSON result")]
    Optimize {
        input: PathBuf,
        #[arg(
            long,
            help = "Select codecs by real token cost instead of the byte proxy"
        )]
        tokens: bool,
        #[arg(
            long,
            help = "Summarize a prose block to a coherent, recoverable working summary (lossy inline)"
        )]
        prose: bool,
    },
    #[command(about = "Verify a compressed wire against a fidelity certificate hash")]
    Verify { wire: PathBuf, certificate: String },
    #[command(about = "Measure the offload reopen rate per shape across discovered traces")]
    ReopenRate {
        #[arg(long, default_value_t = 0)]
        limit: usize,
    },
    #[command(
        about = "Estimate the tokens the optimizer would remove across your discovered traffic"
    )]
    CacheSavings {
        #[arg(long, default_value_t = 0)]
        limit: usize,
    },
    #[command(
        about = "Run a command and losslessly compress its output, the way a shell filter would"
    )]
    Exec {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, required = true)]
        command: Vec<String>,
    },
    #[command(about = "Serve the optimizer over MCP (stdio): optimize and resolve tools")]
    Mcp,
    #[command(
        about = "Host the optimizing endpoint: point an agent's ANTHROPIC_BASE_URL at it, every value stays inline"
    )]
    Serve {
        #[arg(long, default_value = "127.0.0.1:8787")]
        listen: String,
        #[arg(long, default_value = "https://api.anthropic.com")]
        upstream: String,
        #[arg(
            long,
            help = "OpenAI-compatible /embeddings URL for stronger relevance ranking"
        )]
        embed: Option<String>,
        #[arg(long, default_value = "text-embedding-3-small")]
        embed_model: String,
        #[command(flatten)]
        prose: ProseArgs,
        #[arg(
            long,
            help = "Dry run: measure what each request would save but forward it unchanged"
        )]
        observe: bool,
        #[arg(
            long,
            value_name = "TOOL",
            help = "Name of a resolve tool the agent carries (e.g. via a registered MCP server) but does not expose inline, so offload fires and the model is nudged to load it"
        )]
        resolver: Option<String>,
    },
    #[command(
        about = "Run an agent through the optimizer and print a verified savings receipt for the session"
    )]
    Run {
        #[arg(long, help = "Upstream API; auto-selected per agent when omitted")]
        upstream: Option<String>,
        #[arg(
            long,
            help = "OpenAI-compatible /embeddings URL for stronger relevance ranking"
        )]
        embed: Option<String>,
        #[arg(long, default_value = "text-embedding-3-small")]
        embed_model: String,
        #[command(flatten)]
        prose: ProseArgs,
        #[arg(
            long,
            help = "Dry run: measure what each request would save but forward it unchanged"
        )]
        observe: bool,
        #[arg(
            long,
            value_name = "TOOL",
            help = "Resolve tool for recoverable offload; auto-enabled as `resolve` for subscription-plan agents (they carry the registered MCP tool), pass a name to override or `--resolver \"\"` to disable"
        )]
        resolver: Option<String>,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
    #[command(
        about = "Wire the agent: register the resolve tool with Claude Code (and the Bash hook with --hook)"
    )]
    Setup {
        #[arg(help = "Agent to route (e.g. cursor); omit to wire Claude Code")]
        target: Option<String>,
        #[arg(
            long,
            help = "Also install the PreToolUse hook that compresses Bash output"
        )]
        hook: bool,
        #[arg(
            long,
            help = "Wire the resolve tool into the agent (mcp + rules), reversible with --off"
        )]
        tool: bool,
        #[arg(
            long,
            help = "Point a subscription agent's plan traffic at the proxy (config file), reversible with --off"
        )]
        plan: bool,
        #[arg(long, help = "Remove secondwind's wiring from the agent")]
        off: bool,
    },
    #[command(
        about = "Live web view of what the optimizer saved, and proof every block stayed lossless"
    )]
    Proof {
        #[arg(long, default_value = "127.0.0.1:8789")]
        listen: String,
    },
    #[command(about = "Live terminal view of savings for a second pane, updates every second")]
    Watch,
    #[command(about = "Check the install: tokenizer, writable state, agent integration")]
    Check,
    #[command(
        about = "PreToolUse hook that wraps Bash commands in `exec` so their output is compressed"
    )]
    Hook,
    #[command(
        about = "Replay real decisions on compressed vs uncompressed context and compare the agent's choice"
    )]
    Replay {
        #[arg(long, default_value_t = 25)]
        decisions: usize,
        #[arg(long, help = "Model id; falls back to SECONDWIND_MODEL")]
        model: Option<String>,
        #[arg(long, help = "Estimate cost and exit without calling the model")]
        dry_run: bool,
        #[arg(
            long,
            default_value_t = 2.0,
            help = "Refuse to run if projected spend exceeds this"
        )]
        max_spend: f64,
    },
}

// Opt-in prose shrink; off by default so the lossless path runs. --prose = built-in
// sentence summary; --prose-classifier = extractive keep/drop endpoint you own.
#[derive(clap::Args, Clone)]
struct ProseArgs {
    #[arg(
        long,
        help = "Summarize prose blocks to a coherent, recoverable working summary (lossy inline, original via resolve)"
    )]
    prose: bool,
    #[arg(
        long,
        value_name = "URL",
        help = "Extractive keep/drop token classifier endpoint for token-level prose shrink (implies --prose); you vet and own the model"
    )]
    prose_classifier: Option<String>,
    #[arg(
        long,
        default_value = "default",
        help = "Model id for --prose-classifier"
    )]
    prose_classifier_model: String,
}

fn main() -> ExitCode {
    let args = Args::parse();
    let home = args
        .home
        .clone()
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("."));

    match args.command {
        Some(Command::Tap { listen, upstream }) => {
            let config = secondwind_tap::TapConfig {
                listen,
                upstream,
                capture_dir: secondwind_tap::CaptureLog::default_dir(&home),
            };
            return match secondwind_tap::serve(config) {
                Ok(()) => ExitCode::SUCCESS,
                Err(err) => {
                    eprintln!("secondwind tap: {err}");
                    ExitCode::FAILURE
                }
            };
        }
        Some(Command::Repro { fixture }) => return repro(&fixture),
        Some(Command::Redact { input }) => return redact(&input),
        Some(Command::Scoreboard { corpus, out }) => return scoreboard(&corpus, &out),
        Some(Command::Optimize {
            input,
            tokens,
            prose,
        }) => return optimize(&input, tokens, prose),
        Some(Command::Verify { wire, certificate }) => return verify(&wire, &certificate),
        Some(Command::ReopenRate { limit }) => return reopen_rate(&home, limit),
        Some(Command::CacheSavings { limit }) => return cache_savings(&home, limit),
        Some(Command::Exec { command }) => return exec(&home, &command),
        Some(Command::Mcp) => return mcp::serve(&home),
        Some(Command::Serve {
            listen,
            upstream,
            embed,
            embed_model,
            prose,
            observe,
            resolver,
        }) => {
            let mut shaping = build_shaping(embed, embed_model, prose);
            shaping.observe = observe;
            shaping.resolver = resolver;
            return proxy::serve(&home, &listen, &upstream, shaping);
        }
        Some(Command::Run {
            upstream,
            embed,
            embed_model,
            prose,
            observe,
            resolver,
            command,
        }) => {
            let mut shaping = build_shaping(embed, embed_model, prose);
            shaping.observe = observe;
            shaping.resolver = resolver;
            return proxy::run(&home, upstream.as_deref(), &command, shaping);
        }
        Some(Command::Setup {
            target,
            hook,
            tool,
            plan,
            off,
        }) => {
            return setup::run(&home, hook, target.as_deref(), tool, off, plan);
        }
        Some(Command::Proof { listen }) => return dashboard::serve(&home, &listen),
        Some(Command::Watch) => return dashboard::watch(&home),
        Some(Command::Check) => return check(&home),
        Some(Command::Hook) => return hook(),
        Some(Command::Replay {
            decisions,
            model,
            dry_run,
            max_spend,
        }) => return replay::run(&home, decisions, model.as_deref(), dry_run, max_spend),
        None => {}
    }

    let sources = secondwind_sources::all();
    let detectors = secondwind_analyzers::all();
    let capture = secondwind_tap::CaptureLog::load(&secondwind_tap::CaptureLog::default_dir(&home))
        .ok()
        .filter(|log| !log.is_empty());

    let mut audit = Audit::default();
    let mut ledger = LedgerBuilder::default();
    let mut retention = secondwind_analyzers::Retention::default();
    let mut searched = Vec::new();
    let mut skipped: BTreeMap<String, usize> = BTreeMap::new();

    for source in &sources {
        searched.push((source.id(), source.search_root(&home)));
        let files = match source.discover(&home) {
            Ok(files) => files,
            Err(err) => {
                eprintln!("secondwind: {}: {err}", source.id());
                return ExitCode::FAILURE;
            }
        };
        audit.files_discovered += files.len();
        for file in &files {
            match source.read(file) {
                Ok(mut outcome) => {
                    audit.traces_read += outcome.traces.len();
                    audit.turns_read += outcome.traces.iter().map(|t| t.turns.len()).sum::<usize>();
                    for trace in &mut outcome.traces {
                        if let Some(log) = &capture {
                            audit.segments_paired += log.enrich(trace);
                        }
                    }
                    for trace in &outcome.traces {
                        for turn in &trace.turns {
                            if let Some(ts) = &turn.timestamp {
                                if audit.period_start.as_ref().is_none_or(|s| ts < s) {
                                    audit.period_start = Some(ts.clone());
                                }
                                if audit.period_end.as_ref().is_none_or(|e| ts > e) {
                                    audit.period_end = Some(ts.clone());
                                }
                            }
                        }
                        ledger.add(trace);
                        retention.add_trace(trace);
                        for detector in &detectors {
                            audit.findings.extend(detector.analyze(trace));
                        }
                    }
                    for (kind, count) in outcome.skipped_record_types {
                        *skipped.entry(kind).or_insert(0) += count;
                    }
                }
                Err(err) => {
                    audit.files_failed += 1;
                    eprintln!("secondwind: {err}");
                }
            }
        }
    }

    if audit.files_discovered == 0 {
        eprintln!("no agent session logs found");
        for (id, root) in &searched {
            eprintln!("  looked for {id} logs in {}", root.display());
        }
        return ExitCode::SUCCESS;
    }

    let plain_output = args.json || args.md;
    if !skipped.is_empty() && !plain_output {
        let summary = skipped
            .iter()
            .map(|(kind, count)| format!("{kind}({count})"))
            .collect::<Vec<_>>()
            .join(", ");
        eprintln!("skipped record types: {summary}");
    }
    if let Some(log) = &capture
        && !plain_output
    {
        eprintln!("capture log attached: {} on-wire tool results", log.len());
    }

    audit.ledger = Some(ledger.summary());
    audit.retention = retention.observed().then_some(retention);

    let color = std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none();
    let reporter: Box<dyn Reporter> = if args.json {
        Box::new(Json)
    } else if args.md {
        Box::new(Markdown)
    } else if args.html {
        Box::new(Html)
    } else {
        Box::new(Terminal { color })
    };
    print!("{}", reporter.render(&audit));
    ExitCode::SUCCESS
}

fn check(home: &Path) -> ExitCode {
    use secondwind_optimize::tokens::{Tiktoken, TokenCounter};

    let ok = "\u{2713}";
    let no = "\u{2717}";
    println!("secondwind {}", env!("CARGO_PKG_VERSION"));

    let tokens = Tiktoken::cl100k().count("secondwind") > 0;
    println!(
        "  {} tokenizer   {}",
        if tokens { ok } else { no },
        if tokens {
            "cl100k loaded, pricing on the billed unit"
        } else {
            "failed to load"
        }
    );

    let state = home.join(".secondwind");
    let writable = std::fs::create_dir_all(&state).is_ok();
    println!(
        "  {} state       {} ({})",
        if writable { ok } else { no },
        state.display(),
        if writable { "writable" } else { "not writable" }
    );

    let events = secondwind_ledger::events::load(home).len();
    println!("  {ok} events      {events} optimizations recorded");

    let claude = home.join(".claude");
    let has_claude = claude.is_dir();
    println!(
        "  {} claude code {}",
        if has_claude { ok } else { no },
        if has_claude {
            "detected"
        } else {
            "not found in ~/.claude"
        }
    );

    let mcp_registered = std::fs::read_to_string(home.join(".claude.json"))
        .ok()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        .and_then(|root| root.get("mcpServers").map(|s| s.to_string()))
        .map(|servers| servers.contains("secondwind"))
        .unwrap_or(false);
    println!(
        "  {} resolve     {}",
        if mcp_registered { ok } else { no },
        if mcp_registered {
            "mcp server registered, serve/run can offload"
        } else {
            "not registered (run: secondwind setup), proxy stays inline-only"
        }
    );

    let settings = claude.join("settings.json");
    let hooked = std::fs::read_to_string(&settings)
        .map(|s| s.contains("secondwind hook"))
        .unwrap_or(false);
    println!(
        "  {} bash hook   {}",
        if hooked { ok } else { no },
        if hooked {
            "installed, Bash output compresses transparently"
        } else {
            "not installed (run: secondwind setup --hook)"
        }
    );

    println!();
    if !mcp_registered {
        println!("next: `secondwind setup` wires Claude Code in one step");
    }
    println!("watch it live: `secondwind proof`");
    ExitCode::SUCCESS
}

fn build_embedder(
    embed: Option<String>,
    model: String,
) -> Option<std::sync::Arc<dyn secondwind_optimize::relevance::Embedder>> {
    embed.map(|url| {
        let key = std::env::var("SECONDWIND_EMBED_KEY").ok();
        std::sync::Arc::new(embedder::EndpointEmbedder::new(url, model, key))
            as std::sync::Arc<dyn secondwind_optimize::relevance::Embedder>
    })
}

// Builds the proxy's optimizer shaping: relevance embedder + opt-in prose shrink.
// --prose-classifier reads SECONDWIND_PROSE_KEY.
fn build_shaping(embed: Option<String>, embed_model: String, prose: ProseArgs) -> proxy::Shaping {
    let embedder = build_embedder(embed, embed_model);
    let prose_shrinker = prose.prose_classifier.map(|url| {
        let key = std::env::var("SECONDWIND_PROSE_KEY").ok();
        std::sync::Arc::new(prose_shrinker::EndpointShrinker::new(
            url,
            prose.prose_classifier_model,
            key,
        )) as std::sync::Arc<dyn secondwind_optimize::prose::ProseShrinker>
    });
    proxy::Shaping {
        embedder,
        prose_mode: prose.prose || prose_shrinker.is_some(),
        prose_shrinker,
        observe: false,
        resolver: None,
    }
}

fn load_trace(path: &PathBuf) -> Result<secondwind_core::Trace, String> {
    let raw = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_str(&raw).map_err(|e| format!("{}: {e}", path.display()))
}

fn repro(fixture: &PathBuf) -> ExitCode {
    let trace = match load_trace(fixture) {
        Ok(trace) => trace,
        Err(err) => {
            eprintln!("secondwind repro: {err}");
            return ExitCode::FAILURE;
        }
    };
    let findings: Vec<_> = secondwind_analyzers::all()
        .iter()
        .flat_map(|a| a.analyze(&trace))
        .collect();
    println!("trace {} \u{2192} {} findings", trace.id, findings.len());
    for finding in &findings {
        println!(
            "  [{}] turn {}  {}",
            finding.class, finding.turn, finding.detail
        );
        if !finding.original.is_empty() {
            println!("      original   {}", finding.original);
        }
        if !finding.effective.is_empty() {
            println!("      effective  {}", finding.effective);
        }
    }
    ExitCode::SUCCESS
}

const METHOD_VERSION: &str = "v0.1.0";

fn scoreboard(corpus: &PathBuf, out: &PathBuf) -> ExitCode {
    let entries = match std::fs::read_dir(corpus) {
        Ok(entries) => entries,
        Err(err) => {
            eprintln!("secondwind scoreboard: {}: {err}", corpus.display());
            return ExitCode::FAILURE;
        }
    };
    let mut traces = Vec::new();
    let mut unclean = Vec::new();
    let redactor = secondwind_redact::Redactor::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        let trace = match load_trace(&path) {
            Ok(trace) => trace,
            Err(err) => {
                eprintln!("secondwind scoreboard: {err}");
                return ExitCode::FAILURE;
            }
        };
        let mut probe = trace.clone();
        let report = redactor.redact_trace(&mut probe);
        if report.total() > 0 {
            let kinds = report
                .by_kind
                .iter()
                .map(|(kind, count)| format!("{kind}({count})"))
                .collect::<Vec<_>>()
                .join(", ");
            unclean.push(format!("{}: {kinds}", path.display()));
        }
        traces.push(trace);
    }
    if !unclean.is_empty() {
        eprintln!(
            "secondwind scoreboard: publish gate refused {} file(s):",
            unclean.len()
        );
        for line in &unclean {
            eprintln!("  {line}");
        }
        eprintln!("sanitize first: secondwind redact <file>");
        return ExitCode::FAILURE;
    }
    if traces.is_empty() {
        eprintln!(
            "secondwind scoreboard: no trace files in {}",
            corpus.display()
        );
        return ExitCode::FAILURE;
    }

    let rows = secondwind_report::scoreboard::build(&traces);
    if let Err(err) = std::fs::create_dir_all(out) {
        eprintln!("secondwind scoreboard: {}: {err}", out.display());
        return ExitCode::FAILURE;
    }
    let markdown = secondwind_report::scoreboard::to_markdown(&rows, METHOD_VERSION);
    let json = secondwind_report::scoreboard::to_json(&rows);
    for (name, content) in [("README.md", markdown), ("results.json", json)] {
        if let Err(err) = std::fs::write(out.join(name), content) {
            eprintln!("secondwind scoreboard: {name}: {err}");
            return ExitCode::FAILURE;
        }
    }
    println!(
        "scoreboard written: {} rows, {} traces \u{2192} {}",
        rows.len(),
        traces.len(),
        out.display()
    );
    ExitCode::SUCCESS
}

fn redact(input: &PathBuf) -> ExitCode {
    let mut trace = match load_trace(input) {
        Ok(trace) => trace,
        Err(err) => {
            eprintln!("secondwind redact: {err}");
            return ExitCode::FAILURE;
        }
    };
    let report = secondwind_redact::Redactor::new().redact_trace(&mut trace);
    match serde_json::to_string_pretty(&trace) {
        Ok(json) => println!("{json}"),
        Err(err) => {
            eprintln!("secondwind redact: {err}");
            return ExitCode::FAILURE;
        }
    }
    let summary = report
        .by_kind
        .iter()
        .map(|(kind, count)| format!("{kind}({count})"))
        .collect::<Vec<_>>()
        .join(", ");
    if summary.is_empty() {
        eprintln!("redacted: nothing found");
    } else {
        eprintln!("redacted: {summary}");
    }
    ExitCode::SUCCESS
}

fn optimize(input: &PathBuf, tokens: bool, prose: bool) -> ExitCode {
    let raw = match std::fs::read_to_string(input) {
        Ok(raw) => raw,
        Err(err) => {
            eprintln!("secondwind optimize: {}: {err}", input.display());
            return ExitCode::FAILURE;
        }
    };
    use secondwind_optimize::Outcome;
    let mut optimizer = secondwind_optimize::Optimizer::default();
    optimizer.set_model(&configured_model());
    if tokens {
        optimizer = optimizer.with_counter(std::sync::Arc::new(
            secondwind_optimize::tokens::Tiktoken::cl100k(),
        ));
    }
    if prose {
        optimizer = optimizer.with_prose_mode(true);
    }
    let (effective, transform, recovered) = match optimizer.compress_block(&raw) {
        Outcome::Compressed {
            wire, transform, ..
        } => (wire, transform.to_string(), None),
        Outcome::Offloaded { stub, marker, .. } => {
            let body = optimizer.resolve(&marker);
            (stub, "offload".into(), body)
        }
        Outcome::KeptVerbatim { .. } => (raw.clone(), "verbatim".into(), None),
    };
    let available = match &recovered {
        Some(body) => format!("{effective}\n{body}"),
        None => effective.clone(),
    };
    let richness = secondwind_optimize::richness::score(&raw, &available);
    let certificate = secondwind_optimize::certificate::certify(&raw);
    use secondwind_optimize::tokens::TokenCounter;
    let counter = secondwind_optimize::tokens::Tiktoken::cl100k();
    let result = serde_json::json!({
        "transform": transform,
        "input_bytes": raw.len(),
        "output_bytes": effective.len(),
        "input_tokens": counter.count(&raw),
        "output_tokens": counter.count(&effective),
        "richness": richness.retained,
        "values_kept": format!("{}/{}", richness.kept, richness.atoms),
        "certificate": certificate.hash,
        "effective": effective,
        "recovered": recovered,
    });
    println!(
        "{}",
        serde_json::to_string(&result).expect("result serializes")
    );
    ExitCode::SUCCESS
}

fn verify(wire: &PathBuf, certificate: &str) -> ExitCode {
    use secondwind_optimize::certificate::{Certificate, verify as check};

    let wire = match std::fs::read_to_string(wire) {
        Ok(wire) => wire,
        Err(err) => {
            eprintln!("secondwind verify: {}: {err}", wire.display());
            return ExitCode::FAILURE;
        }
    };
    let cert = Certificate {
        hash: certificate.to_string(),
    };
    if check(&wire, &cert) {
        println!("PASS: wire is faithful to the certificate");
        ExitCode::SUCCESS
    } else {
        println!("FAIL: wire does not match the certificate");
        ExitCode::FAILURE
    }
}

fn reopen_rate(home: &Path, limit: usize) -> ExitCode {
    use secondwind_optimize::counterfactual::{ReopenStats, measure};

    let mut stats = ReopenStats::default();
    let mut traces = 0usize;
    let mut files_read = 0usize;
    let mut files_failed = 0usize;
    for source in secondwind_sources::all() {
        let files = match source.discover(home) {
            Ok(files) => files,
            Err(err) => {
                eprintln!("secondwind reopen-rate: {}: {err}", source.id());
                return ExitCode::FAILURE;
            }
        };
        for file in &files {
            if limit != 0 && files_read >= limit {
                break;
            }
            match source.read(file) {
                Ok(outcome) => {
                    files_read += 1;
                    for trace in &outcome.traces {
                        stats.merge(&measure(trace));
                        traces += 1;
                    }
                }
                Err(_) => files_failed += 1,
            }
        }
    }

    let shapes: serde_json::Map<String, serde_json::Value> = stats
        .by_shape
        .iter()
        .map(|(shape, stat)| {
            (
                shape.clone(),
                serde_json::json!({
                    "offloads": stat.offloads,
                    "reopens": stat.reopens,
                    "reopen_rate": stat.rate(),
                }),
            )
        })
        .collect();
    let result = serde_json::json!({
        "files_read": files_read,
        "files_failed": files_failed,
        "traces": traces,
        "by_shape": shapes,
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&result).expect("result serializes")
    );
    ExitCode::SUCCESS
}

const HOOK_SETUP: &str = r#"Add this to .claude/settings.json to compress Bash output transparently:

  {
    "hooks": {
      "PreToolUse": [
        { "matcher": "Bash", "hooks": [ { "type": "command", "command": "secondwind hook" } ] }
      ]
    }
  }

Then Bash commands run through `secondwind exec`, keeping every value present or
recoverable. Verify the rewrite lands in a live session; if the field name differs
in your build, it is a one-line change in `secondwind hook`."#;

fn hook() -> ExitCode {
    use std::io::Read;

    let stdin = std::io::stdin();
    if stdin.is_terminal() {
        println!("{HOOK_SETUP}");
        return ExitCode::SUCCESS;
    }
    let mut input = String::new();
    if stdin.lock().read_to_string(&mut input).is_err() {
        return ExitCode::SUCCESS;
    }
    let Ok(event) = serde_json::from_str::<serde_json::Value>(&input) else {
        return ExitCode::SUCCESS;
    };
    let tool = event["tool_name"].as_str().unwrap_or_default();
    let command = event["tool_input"]["command"].as_str().unwrap_or_default();
    if tool == "Bash" && !command.is_empty() && !command.trim_start().starts_with("secondwind") {
        let rewrite = serde_json::json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "allow",
                "updatedInput": { "command": format!("secondwind exec {command}") }
            }
        });
        println!("{rewrite}");
    }
    ExitCode::SUCCESS
}

fn exec(home: &Path, command: &[String]) -> ExitCode {
    use secondwind_optimize::{Optimizer, Outcome};

    let joined = command.join(" ");
    let output = match std::process::Command::new("sh")
        .arg("-c")
        .arg(&joined)
        .output()
    {
        Ok(output) => output,
        Err(err) => {
            eprintln!("secondwind exec: {joined}: {err}");
            return ExitCode::FAILURE;
        }
    };
    eprint!("{}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let original = stdout.len();

    let model = configured_model();
    let mut optimizer = Optimizer::default()
        .with_counter(std::sync::Arc::new(
            secondwind_optimize::tokens::Tiktoken::cl100k(),
        ))
        .with_model(&model)
        .with_store(secondwind_optimize::offload::Store::persistent(
            proxy::store_dir(home),
            proxy::OFFLOAD_TTL,
        ));
    let (effective, note, transform, saved_usd, inline) = match optimizer.compress_block(&stdout) {
        Outcome::Compressed {
            wire,
            transform,
            saved_usd,
            ..
        } => (
            wire,
            format!("{transform}, every value kept inline"),
            transform.to_string(),
            saved_usd,
            true,
        ),
        Outcome::Offloaded {
            stub,
            marker,
            saved_usd,
        } => {
            let body = optimizer.resolve(&marker).unwrap_or_default();
            let recovery = match persist_offload(home, &marker, &body) {
                Some(path) => format!("offload, full {original} bytes at {}", path.display()),
                None => "offload".to_string(),
            };
            (stub, recovery, "offload".to_string(), saved_usd, false)
        }
        Outcome::KeptVerbatim { .. } => (
            stdout.clone(),
            "kept verbatim".to_string(),
            "verbatim".to_string(),
            0.0,
            true,
        ),
    };

    if transform != "verbatim" {
        record_event(
            home, "exec", &transform, &stdout, &effective, saved_usd, inline, &model,
        );
    }

    print!("{effective}");
    let pct = if original == 0 {
        0.0
    } else {
        100.0 * (1.0 - effective.len() as f64 / original as f64)
    };
    eprintln!(
        "\nsecondwind: {original} -> {} bytes, {pct:.1}% smaller ({note})",
        effective.len()
    );

    if output.status.success() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

// Pricing model for surfaces that name none in-request (exec, mcp): SECONDWIND_MODEL
// if we can price it, else a default.
fn configured_model() -> String {
    std::env::var("SECONDWIND_MODEL")
        .ok()
        .filter(|model| secondwind_ledger::rates_for(model).is_some())
        .unwrap_or_else(|| "claude-sonnet-4-5".to_string())
}

// Append one optimization to the shared event log the dashboard reads. Only gated
// outcomes reach here (verified always true); real tokenizer counts = billed unit.
#[allow(clippy::too_many_arguments)]
fn record_event(
    home: &Path,
    surface: &str,
    transform: &str,
    before: &str,
    after: &str,
    saved_usd: f64,
    inline: bool,
    model: &str,
) {
    use secondwind_optimize::tokens::{Tiktoken, TokenCounter};
    let counter = Tiktoken::cl100k();
    let (atoms, cert) = secondwind_optimize::proof(before);
    secondwind_ledger::events::record(
        home,
        &secondwind_ledger::events::Event {
            at_ms: secondwind_ledger::events::now_ms(),
            surface: surface.to_string(),
            transform: transform.to_string(),
            input_tokens: counter.count(before) as u64,
            output_tokens: counter.count(after) as u64,
            saved_usd,
            verified: true,
            inline,
            atoms,
            cert,
            model: model.to_string(),
            platform: surface.to_string(),
            tenant: String::new(),
            kept_reason: String::new(),
            req_id: String::new(),
        },
    );
}

fn persist_offload(home: &Path, marker: &str, body: &str) -> Option<PathBuf> {
    let name: String = marker.chars().filter(char::is_ascii_alphanumeric).collect();
    let dir = home.join(".secondwind").join("exec");
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join(format!("{name}.txt"));
    std::fs::write(&path, body).ok()?;
    Some(path)
}

fn cache_savings(home: &Path, limit: usize) -> ExitCode {
    use secondwind_optimize::cachecost::{CacheSavings, estimate};

    const DEFAULT_MODEL: &str = "claude-sonnet-4-5";
    let mut total = CacheSavings::default();
    let mut traces = 0usize;
    for source in secondwind_sources::all() {
        let Ok(files) = source.discover(home) else {
            continue;
        };
        for file in &files {
            if limit != 0 && traces >= limit {
                break;
            }
            let Ok(outcome) = source.read(file) else {
                continue;
            };
            for trace in &outcome.traces {
                let model = trace
                    .turns
                    .iter()
                    .find_map(|t| t.model.as_deref())
                    .unwrap_or(DEFAULT_MODEL);
                let rates = secondwind_ledger::rates_for(model)
                    .or_else(|| secondwind_ledger::rates_for(DEFAULT_MODEL));
                if let Some(rates) = rates {
                    total.merge(&estimate(trace, rates));
                }
                traces += 1;
            }
        }
    }

    let token_reduction = if total.original_tokens == 0 {
        0.0
    } else {
        1.0 - total.compressed_tokens as f64 / total.original_tokens as f64
    };
    let result = serde_json::json!({
        "traces": traces,
        "blocks_compressed": total.blocks,
        "original_tokens": total.original_tokens,
        "compressed_tokens": total.compressed_tokens,
        "tokens_removed": total.original_tokens.saturating_sub(total.compressed_tokens),
        "token_reduction": token_reduction,
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&result).expect("result serializes")
    );
    ExitCode::SUCCESS
}
