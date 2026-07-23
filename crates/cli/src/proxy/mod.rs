use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::{FromRequestParts, Request, State, WebSocketUpgrade};
use axum::http::HeaderMap;
use axum::response::Response;
use secondwind_ledger::events;
use secondwind_optimize::Optimizer;
use secondwind_optimize::offload::{OffloadStore, Store};
use secondwind_optimize::prose::ProseShrinker;
use secondwind_optimize::proxy::FreezeState;
use secondwind_optimize::relevance::Embedder;
use secondwind_optimize::tokens::Tiktoken;
use serde_json::Value;

mod http;
mod pipeline;
mod ws;

// Per-caller optimizer overrides (relevance embedder, opt-in prose summary/shrink), shared
// across all request tasks.
#[derive(Clone, Default)]
pub struct Shaping {
    pub embedder: Option<Arc<dyn Embedder>>,
    pub prose_mode: bool,
    pub prose_shrinker: Option<Arc<dyn ProseShrinker>>,
    // Dry-run: book what each request would save, but forward the original body untouched.
    pub observe: bool,
    // Resolve tool the agent carries but doesn't expose inline (e.g. a lazy MCP tool). When set,
    // offload fires and stubs nudge the model to load and call it.
    pub resolver: Option<String>,
}

impl Shaping {
    pub(crate) fn apply(&self, mut optimizer: Optimizer) -> Optimizer {
        if let Some(embedder) = &self.embedder {
            optimizer = optimizer.with_embedder(embedder.clone());
        }
        if let Some(shrinker) = &self.prose_shrinker {
            optimizer = optimizer.with_prose_shrinker(shrinker.clone());
        } else if self.prose_mode {
            optimizer = optimizer.with_prose_mode(true);
        }
        optimizer
    }

    fn describe(&self) -> Option<String> {
        let mut on = Vec::new();
        if self.embedder.is_some() {
            on.push("relevance: embedding endpoint");
        }
        if self.prose_shrinker.is_some() {
            on.push("prose: token shrink, classifier endpoint");
        } else if self.prose_mode {
            on.push("prose: recoverable summary, coverage-ranked");
        }
        if self.resolver.is_some() {
            on.push("resolver: declared (offload active, model nudged to load it)");
        }
        (!on.is_empty()).then(|| on.join("; "))
    }
}

pub const OFFLOAD_TTL: Duration = Duration::from_secs(24 * 3600);

// Bounds the wire cache; clearing after an enormous session re-shapes a few live blocks once.
pub(crate) const FREEZE_CAP: usize = 100_000;

// Bounds the count-once set. Larger than FREEZE_CAP so a counted-once block survives many
// wire-cache clears before it can be re-booked.
pub(crate) const SEEN_CAP: usize = 500_000;

// Fallback upstream when `run` has no --upstream and no registry entry; a known
// single-provider agent supplies its own.
const DEFAULT_UPSTREAM: &str = "https://api.anthropic.com";

pub fn store_dir(home: &Path) -> std::path::PathBuf {
    home.join(".secondwind").join("store")
}

// A company registers custom platform labels in ~/.secondwind/config.json, e.g.
// {"platforms":[{"header":"user-agent","contains":"acme","label":"acme agent"}]}.
pub(crate) struct PlatformRule {
    header: String,
    contains: String,
    label: String,
}

fn load_platform_rules(home: &Path) -> Vec<PlatformRule> {
    let path = home.join(".secondwind").join("config.json");
    let Ok(cfg) = std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .ok_or(())
    else {
        return Vec::new();
    };
    let Some(rules) = cfg.get("platforms").and_then(Value::as_array) else {
        return Vec::new();
    };
    rules
        .iter()
        .filter_map(|r| {
            Some(PlatformRule {
                header: r.get("header")?.as_str()?.to_ascii_lowercase(),
                contains: r.get("contains")?.as_str()?.to_ascii_lowercase(),
                label: r.get("label")?.as_str()?.to_string(),
            })
        })
        .collect()
}

// The calling platform: custom config rules first, then the built-in header match.
pub(crate) fn detect_platform(headers: &HeaderMap, rules: &[PlatformRule]) -> String {
    if !rules.is_empty() {
        for (name, value) in headers {
            let name = name.as_str();
            let value = value.to_str().unwrap_or("").to_ascii_lowercase();
            for rule in rules {
                if name == rule.header && value.contains(&rule.contains) {
                    return rule.label.clone();
                }
            }
        }
    }
    let session = headers.contains_key("x-claude-code-session-id");
    let ua = headers
        .get("user-agent")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    let app = headers
        .get("x-app")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    classify_platform(session, &ua, &app).to_string()
}

// Per-tenant header so the ledger attributes savings to a team; absent = single-tenant. The
// store needs no per-tenant partition: markers are unguessable content hashes, so a tenant can
// only resolve what it already holds, and identical content dedups across tenants for free.
const TENANT_HEADER: &str = "x-secondwind-tenant";

pub(crate) fn detect_tenant(headers: &HeaderMap) -> String {
    headers
        .get(TENANT_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .unwrap_or_default()
        .to_string()
}

fn classify_platform(cc_session: bool, ua: &str, app: &str) -> &'static str {
    if cc_session || app == "cli" || ua.contains("claude-cli") {
        "claude code"
    } else if ua.contains("openai") || ua.contains("codex") {
        "openai / codex"
    } else if ua.contains("anthropic") || ua.contains("stainless") {
        "anthropic api"
    } else if ua.is_empty() {
        "unknown"
    } else {
        "api"
    }
}

// Process-lifetime signals no per-block event carries: a monotonic source for request ids, and a
// count of self-proof rejections (a rewrite the whole-request lossless gate refused to forward).
#[derive(Default)]
pub(crate) struct Counters {
    pub(crate) req_seq: std::sync::atomic::AtomicU64,
    pub(crate) self_proof_rejected: std::sync::atomic::AtomicU64,
}

// Shared by every request task. The HTTP client carries no decompression feature, so upstream
// bytes stream back untouched (byte-faithful).
pub(crate) struct AppState {
    pub(crate) counter: Arc<Tiktoken>,
    pub(crate) freeze: Arc<FreezeState>,
    pub(crate) counters: Arc<Counters>,
    // Built once, shared by every request task (the whole fleet on a shared backend).
    // Content-addressed, so concurrent writes are idempotent.
    pub(crate) store: Arc<dyn OffloadStore>,
    pub(crate) dir: PathBuf,
    pub(crate) home: PathBuf,
    pub(crate) upstream: String,
    pub(crate) rules: Vec<PlatformRule>,
    pub(crate) shaping: Shaping,
    pub(crate) http: reqwest::Client,
}

fn build_state(home: &Path, upstream: &str, shaping: Shaping) -> Arc<AppState> {
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        // Connect timeout + idle reaping, but NO total/read timeout: streamed replies have long
        // think-time gaps and must not be cut off.
        .connect_timeout(Duration::from_secs(30))
        .pool_idle_timeout(Duration::from_secs(90))
        .build()
        .expect("build http client");
    // Guard on by default; SW_CACHE_GUARD=off re-enables maturation, SW_HOLD_TURNS sets its hold count.
    let guard = std::env::var("SW_CACHE_GUARD").ok().as_deref() != Some("off");
    let freeze = match std::env::var("SW_HOLD_TURNS")
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
    {
        Some(n) => FreezeState::configured(guard, n),
        None if guard => FreezeState::default(),
        None => FreezeState::without_cache_guard(),
    };
    Arc::new(AppState {
        counter: Arc::new(Tiktoken::cl100k()),
        freeze: Arc::new(freeze),
        counters: Arc::new(Counters::default()),
        store: Arc::new(Store::persistent(store_dir(home), OFFLOAD_TTL)),
        dir: store_dir(home),
        home: home.to_path_buf(),
        upstream: upstream.trim_end_matches('/').to_string(),
        rules: load_platform_rules(home),
        shaping,
        http,
    })
}

// Caps in-flight requests; excess is shed with 503, not queued, so a spike degrades gracefully
// instead of exhausting memory. Streaming replies free their buffer early, so this can be generous.
const MAX_IN_FLIGHT: usize = 4096;

fn build_router(state: Arc<AppState>) -> Router {
    Router::new()
        .fallback(entry)
        .layer(
            tower::ServiceBuilder::new()
                .layer(axum::error_handling::HandleErrorLayer::new(
                    |_: axum::BoxError| async { axum::http::StatusCode::SERVICE_UNAVAILABLE },
                ))
                .load_shed()
                .concurrency_limit(MAX_IN_FLIGHT),
        )
        .with_state(state)
}

// Single proxy entry: a WebSocket upgrade relays frame by frame (Codex carries its responses
// transport over WS); anything else is shrunk, forwarded, and streamed straight back untouched.
async fn entry(State(state): State<Arc<AppState>>, request: Request) -> Response {
    let (mut parts, body) = request.into_parts();
    match WebSocketUpgrade::from_request_parts(&mut parts, &()).await {
        Ok(upgrade) => {
            if std::env::var_os("SECONDWIND_WS_DEBUG").is_some() {
                eprintln!("secondwind entry: WS upgrade {}", parts.uri.path());
            }
            ws::handle(upgrade, parts, state)
        }
        Err(_) => {
            if std::env::var_os("SECONDWIND_WS_DEBUG").is_some() {
                eprintln!("secondwind entry: {} {}", parts.method, parts.uri.path());
            }
            http::forward(state, parts, body).await
        }
    }
}

// Serves the router on a fresh multi-threaded runtime until exit. Listener is bound before
// this call, so connections queue rather than refuse.
fn serve_on(listener: std::net::TcpListener, state: Arc<AppState>) -> ExitCode {
    // Bound the blocking pool near core count so concurrent compressions don't spawn hundreds of
    // CPU-thrashing threads (default cap 512 is sized for blocking I/O, not CPU work).
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .max_blocking_threads((cores * 2).clamp(4, 128))
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            eprintln!("secondwind: runtime: {err}");
            return ExitCode::FAILURE;
        }
    };
    runtime.block_on(async move {
        if let Err(err) = listener.set_nonblocking(true) {
            eprintln!("secondwind serve: {err}");
            return ExitCode::FAILURE;
        }
        let listener = match tokio::net::TcpListener::from_std(listener) {
            Ok(listener) => listener,
            Err(err) => {
                eprintln!("secondwind serve: {err}");
                return ExitCode::FAILURE;
            }
        };
        // Sweep the offload dir on a schedule (first tick fires at once), not per-request.
        let sweep_dir = state.dir.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(3600));
            loop {
                tick.tick().await;
                let dir = sweep_dir.clone();
                let _ = tokio::task::spawn_blocking(move || {
                    secondwind_optimize::offload::sweep(&dir, OFFLOAD_TTL);
                })
                .await;
            }
        });
        if let Err(err) = axum::serve(listener, build_router(state)).await {
            eprintln!("secondwind serve: {err}");
            return ExitCode::FAILURE;
        }
        ExitCode::SUCCESS
    })
}

pub fn serve(home: &Path, listen: &str, upstream: &str, shaping: Shaping) -> ExitCode {
    let listener = match std::net::TcpListener::bind(listen) {
        Ok(listener) => listener,
        Err(err) => {
            eprintln!("secondwind serve: {listen}: {err}");
            return ExitCode::FAILURE;
        }
    };
    eprintln!("secondwind serve on http://{listen} \u{2192} {upstream}");
    if shaping.observe {
        eprintln!(
            "OBSERVE MODE: every request body is forwarded UNCHANGED; secondwind only reports \
             the tokens it would have saved (booked under the `observe` surface)."
        );
    } else {
        eprintln!(
            "thin proxy: requests shrink, responses stream through untouched. Offload activates \
             automatically when the agent carries the secondwind resolve tool (secondwind setup)."
        );
    }
    if let Some(note) = shaping.describe() {
        eprintln!("{note}");
    }
    serve_on(listener, build_state(home, upstream, shaping))
}

// RESOLVE_TOOL: registered MCP resolve tool name, declared so offload fires and the model is
// nudged to load the host's read-only resolver (approved once) to rehydrate an offloaded block.
const RESOLVE_TOOL: &str = "resolve";

// Resolver `run` declares: explicit --resolver wins, `--resolver ""` opts out, a plan agent
// carrying the resolve tool auto-enables it so offload fires by default.
fn plan_resolver(explicit: Option<&str>, carries_resolve_tool: bool) -> Option<String> {
    match explicit {
        Some("") => None,
        Some(name) => Some(name.to_string()),
        None if carries_resolve_tool => Some(RESOLVE_TOOL.to_string()),
        None => None,
    }
}

// `run` routes a child agent through an optimizer on an ephemeral port and prints a receipt; nothing
// on disk changes. No command = route the one known agent on PATH; upstream and launch tweaks come
// from the agent registry when --upstream is unset.
pub fn run(
    home: &Path,
    upstream: Option<&str>,
    command: &[String],
    mut shaping: Shaping,
) -> ExitCode {
    // Resolver decided once below (see plan_resolver).
    let explicit_resolver = shaping.resolver.take();
    let mut carries_resolve_tool = false;
    let resolved: Vec<String> = if command.is_empty() {
        match detect_agents().as_slice() {
            [] => {
                eprintln!(
                    "secondwind run: no known agent found on PATH; name one, e.g. `secondwind run -- <agent>`"
                );
                return ExitCode::FAILURE;
            }
            [only] => {
                eprintln!("secondwind run: detected {only}, routing it through the optimizer");
                vec![only.clone()]
            }
            many => {
                eprintln!(
                    "secondwind run: found {}; pick one, e.g. `secondwind run {}`",
                    many.join(", "),
                    many[0]
                );
                return ExitCode::SUCCESS;
            }
        }
    } else {
        command.to_vec()
    };
    let Some((program, args)) = resolved.split_first() else {
        eprintln!("secondwind run: no command given");
        return ExitCode::FAILURE;
    };

    let listener = match std::net::TcpListener::bind("127.0.0.1:0") {
        Ok(listener) => listener,
        Err(err) => {
            eprintln!("secondwind run: {err}");
            return ExitCode::FAILURE;
        }
    };
    let addr = match listener.local_addr() {
        Ok(addr) => addr,
        Err(err) => {
            eprintln!("secondwind run: could not resolve a local port: {err}");
            return ExitCode::FAILURE;
        }
    };
    let base = format!("http://{addr}");
    let start = events::now_ms();

    // Resolve the upstream and any launch tweak from the registry when --upstream is unset.
    let agent = crate::agents::get(program);
    let mut extra_args: Vec<String> = Vec::new();
    let effective_upstream = if let Some(u) = upstream {
        u.to_string()
    } else if let Some(a) = agent {
        if let Some(pc) = &a.plan
            && codex_plan_mode(home)
        {
            // Plan traffic ignores the base-URL env; route it with ephemeral -c overrides (no
            // file touched). The resolve tool rides along the same way, so blocks rehydrate.
            for route in pc.routes {
                extra_args.push("-c".to_string());
                extra_args.push(format!("{}={}", route.key, route.toml_value(&base)));
            }
            if pc.resolve_tool {
                for arg in mcp_override_args(home) {
                    extra_args.push("-c".to_string());
                    extra_args.push(arg);
                }
                carries_resolve_tool = true;
            }
            eprintln!(
                "secondwind run: {program} on a subscription plan, routing its WebSocket through the proxy"
            );
            pc.upstream.to_string()
        } else {
            a.upstream.unwrap_or(DEFAULT_UPSTREAM).to_string()
        }
    } else {
        DEFAULT_UPSTREAM.to_string()
    };
    // Declaring the resolver makes offload fire; the model rehydrates via tool_search (one-time
    // approval). Explicit --resolver / `--resolver ""` wins over the plan-agent default.
    shaping.resolver = plan_resolver(explicit_resolver.as_deref(), carries_resolve_tool);
    let observing = shaping.observe;
    let offload_active = shaping.resolver.is_some();

    let state = build_state(home, &effective_upstream, shaping);
    std::thread::spawn(move || {
        let _ = serve_on(listener, state);
    });

    let mode = if observing {
        " (observe: forwarding unchanged)"
    } else {
        ""
    };
    eprintln!("secondwind run: {program} through {base} \u{2192} {effective_upstream}{mode}");
    if offload_active {
        eprintln!(
            "secondwind: recoverable offload active; approve the read-only `resolve` tool once so \
             large aged outputs rehydrate on demand (or `--resolver \"\"` to keep everything inline)"
        );
    }
    eprintln!("secondwind: live view with `secondwind proof` in another terminal");
    let status = std::process::Command::new(program)
        .args(&extra_args)
        .args(args)
        .env("ANTHROPIC_BASE_URL", &base)
        .env("OPENAI_BASE_URL", &base)
        .env("OPENAI_API_BASE", &base)
        .status();

    receipt(home, start);

    match status {
        Ok(status) if status.success() => ExitCode::SUCCESS,
        Ok(_) => ExitCode::FAILURE,
        Err(err) => {
            eprintln!("secondwind run: {program}: {err}");
            ExitCode::FAILURE
        }
    }
}

// `-c` values registering the resolve MCP server for a plan agent, pinned to this home so its
// offload store matches the proxy's. Ephemeral: nothing on disk changes.
fn mcp_override_args(home: &Path) -> Vec<String> {
    let exe = std::env::current_exe()
        .ok()
        .and_then(|p| p.to_str().map(str::to_string))
        .unwrap_or_else(|| "secondwind".to_string());
    let home_abs = std::fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf());
    let home_abs = home_abs.to_string_lossy();
    vec![
        format!("mcp_servers.secondwind.command=\"{exe}\""),
        format!("mcp_servers.secondwind.args=[\"--home\",\"{home_abs}\",\"mcp\"]"),
    ]
}

// Detects a subscription CLI whose plan traffic reads a config key rather than a base-URL
// env var: Codex on a ChatGPT plan carries an OAuth token and a null API key in auth.json.
fn codex_plan_mode(home: &Path) -> bool {
    let Ok(raw) = std::fs::read_to_string(home.join(".codex/auth.json")) else {
        return false;
    };
    let Ok(json) = serde_json::from_str::<Value>(&raw) else {
        return false;
    };
    let has_key = json
        .get("OPENAI_API_KEY")
        .and_then(Value::as_str)
        .is_some_and(|k| !k.is_empty());
    let has_token = json
        .pointer("/tokens/access_token")
        .and_then(Value::as_str)
        .is_some();
    has_token && !has_key
}

fn agents_in_dirs(dirs: &[PathBuf]) -> Vec<String> {
    crate::agents::launch_bins()
        .into_iter()
        .filter(|bin| dirs.iter().any(|dir| dir.join(bin).is_file()))
        .map(|bin| bin.to_string())
        .collect()
}

// Known agents found on PATH, so `run` with no argument routes what's installed.
fn detect_agents() -> Vec<String> {
    let Ok(path) = std::env::var("PATH") else {
        return Vec::new();
    };
    let dirs: Vec<PathBuf> = path.split(':').map(PathBuf::from).collect();
    agents_in_dirs(&dirs)
}

fn receipt(home: &Path, since_ms: u64) {
    let session: Vec<_> = events::load(home)
        .into_iter()
        .filter(|e| e.at_ms >= since_ms)
        .collect();
    if session.is_empty() {
        eprintln!("secondwind: no tool-output blocks were optimized this session");
        return;
    }
    let summary = events::summarize(&session, 0);
    let saved_tokens = summary.input_tokens.saturating_sub(summary.output_tokens);
    let pct = if summary.input_tokens == 0 {
        0.0
    } else {
        100.0 * saved_tokens as f64 / summary.input_tokens as f64
    };
    eprintln!(
        "secondwind receipt: {} blocks, {saved_tokens} tokens saved ({pct:.1}%), {}/{} verified lossless",
        summary.blocks, summary.verified, summary.blocks
    );
    // The refusal side of the trail: blocks the gate saw but chose to leave verbatim, and why.
    if summary.kept > 0 {
        let reasons: Vec<String> = summary
            .by_kept_reason
            .iter()
            .map(|(r, n)| format!("{n} {r}"))
            .collect();
        eprintln!(
            "secondwind trail: {} blocks seen, {} compressed, {} kept verbatim ({})",
            summary.seen,
            summary.blocks,
            summary.kept,
            reasons.join(", ")
        );
    }
    // Saved tokens = context the agent no longer carries, so auto-compact fires that much later.
    // No version-specific threshold assumed; nothing was summarized away.
    if saved_tokens > 0 {
        eprintln!(
            "secondwind wall: your context is ~{saved_tokens} tokens lighter, so the agent's auto-compact fires that much later, losslessly (nothing summarized away)"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{RESOLVE_TOOL, agents_in_dirs, classify_platform, plan_resolver};

    #[test]
    fn a_plan_agent_that_carries_the_resolve_tool_auto_enables_offload() {
        // `secondwind run codex` (no flag) fires offload by default.
        assert_eq!(plan_resolver(None, true).as_deref(), Some(RESOLVE_TOOL));
        // An explicit name overrides the default.
        assert_eq!(
            plan_resolver(Some("myresolve"), true).as_deref(),
            Some("myresolve")
        );
        // `--resolver ""` opts out even for a plan agent.
        assert_eq!(plan_resolver(Some(""), true), None);
        // A non-plan agent (no registered resolve tool) stays inline unless named explicitly.
        assert_eq!(plan_resolver(None, false), None);
        assert_eq!(
            plan_resolver(Some("resolve"), false).as_deref(),
            Some("resolve")
        );
    }

    #[test]
    fn detects_a_known_agent_binary_in_a_dir() {
        let dir = std::env::temp_dir().join(format!("sw_run_detect_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("codex"), b"").unwrap();
        std::fs::write(dir.join("not-an-agent"), b"").unwrap();

        let found = agents_in_dirs(std::slice::from_ref(&dir));
        assert!(found.contains(&"codex".to_string()));
        assert!(!found.iter().any(|a| a == "not-an-agent"));
        assert!(
            !found.contains(&"claude".to_string()),
            "absent binaries are not reported"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tenant_comes_from_the_header_and_defaults_empty() {
        use axum::http::HeaderMap;
        let mut headers = HeaderMap::new();
        assert_eq!(
            super::detect_tenant(&headers),
            "",
            "single-tenant when the header is absent"
        );
        headers.insert("x-secondwind-tenant", "acme".parse().unwrap());
        assert_eq!(super::detect_tenant(&headers), "acme");
        headers.insert("x-secondwind-tenant", "  ".parse().unwrap());
        assert_eq!(
            super::detect_tenant(&headers),
            "",
            "blank is treated as absent"
        );
    }

    #[test]
    fn classifies_real_captured_client_headers() {
        // Real agent-CLI headers, captured live.
        assert_eq!(
            classify_platform(true, "claude-cli/2.1.214 (external, sdk-cli)", "cli"),
            "claude code"
        );
        // The session-id header alone is enough.
        assert_eq!(classify_platform(true, "", ""), "claude code");
        // A direct SDK call, not the CLI.
        assert_eq!(
            classify_platform(false, "anthropic-sdk-python/0.42.0", ""),
            "anthropic api"
        );
        assert_eq!(
            classify_platform(false, "openai-python/1.0", ""),
            "openai / codex"
        );
        assert_eq!(classify_platform(false, "", ""), "unknown");
    }
}
