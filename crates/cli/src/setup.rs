use std::path::{Path, PathBuf};
use std::process::ExitCode;

use serde_json::{Value, json};

const RULES_START: &str = "<!-- secondwind:start -->";
const RULES_END: &str = "<!-- secondwind:end -->";
const RESOLVE_RULE: &str = "When a tool result contains a `<<swload:...>>` marker, its full content was offloaded by secondwind to save tokens. Call the `resolve` tool with that exact marker to fetch the original verbatim.";

// Turnkey agent wiring. With a target it routes that agent; with none it registers the secondwind MCP
// server (so the resolve tool rides every request and `serve`/`run` can offload) plus the PreToolUse Bash
// hook under --hook. Edits are idempotent and back up first; a file that does not parse is left alone.
pub fn run(
    home: &Path,
    with_hook: bool,
    target: Option<&str>,
    tool: bool,
    off: bool,
    plan: bool,
) -> ExitCode {
    if let Some(name) = target {
        return route_agent(home, name, tool, off, plan);
    }
    let ok = "\u{2713}";
    let mut failed = false;

    let config = home.join(".claude.json");
    match register_mcp(&config, home) {
        Ok(true) => println!(
            "  {ok} mcp   registered `secondwind mcp` in {}",
            config.display()
        ),
        Ok(false) => println!("  {ok} mcp   already registered"),
        Err(err) => {
            eprintln!("  \u{2717} mcp   {err}");
            eprintln!("        register manually: claude mcp add secondwind -- secondwind mcp");
            failed = true;
        }
    }

    if with_hook {
        let settings = home.join(".claude").join("settings.json");
        match install_hook(&settings) {
            Ok(true) => println!(
                "  {ok} hook  Bash outputs compress via {}",
                settings.display()
            ),
            Ok(false) => println!("  {ok} hook  already installed"),
            Err(err) => {
                eprintln!("  \u{2717} hook  {err}");
                failed = true;
            }
        }
    } else {
        println!("  - hook  skipped (add with: secondwind setup --hook)");
    }

    println!();
    println!("restart Claude Code to pick up the tools, then `secondwind check`");
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

// Routes a registered agent. Launch agents just use `run`; guided (GUI) agents get the base-URL
// steps and endpoint. Never write a GUI app's config: an internal store holding auth tokens, so
// blind edits risk corruption.
fn route_agent(home: &Path, name: &str, tool: bool, off: bool, plan: bool) -> ExitCode {
    let Some(agent) = crate::agents::get(name) else {
        eprintln!(
            "secondwind setup: unknown agent `{name}`; known: {}",
            crate::agents::names().join(", ")
        );
        return ExitCode::FAILURE;
    };
    if plan {
        return plan_agent(home, name, agent, off);
    }
    let crate::agents::Route::Guided {
        app,
        steps,
        mcp_config,
        rules_file,
    } = &agent.route
    else {
        println!("{name} is a terminal agent, so route it at launch, nothing to install:");
        println!("  secondwind run -- {name}");
        return ExitCode::SUCCESS;
    };

    if off {
        return unwire_agent(home, name, mcp_config, rules_file);
    }
    if tool {
        return wire_agent(home, name, mcp_config, rules_file);
    }

    if Path::new(app).exists() {
        println!("secondwind setup {name}: found {app}");
    } else {
        println!("secondwind setup {name}: {app} not found; install {name} first, then:");
    }
    println!();
    println!("Route {name} through secondwind, one time, in its own settings:");
    for (i, step) in steps.iter().enumerate() {
        println!("  {}. {step}", i + 1);
    }
    println!();
    println!("  endpoint:  http://127.0.0.1:8787/v1");
    println!("  serve it:  secondwind serve --upstream https://api.openai.com");
    println!();
    println!("Then wire the resolve tool: `secondwind setup {name} --tool` (undo with --off).");
    println!();
    println!("The base-URL step covers {name}'s custom-key traffic; its subscription models route");
    println!(
        "through {name}'s own servers and cannot be optimized locally. We never edit {name}'s"
    );
    println!(
        "internal store, only its documented mcp and rules files, and only when you pass --tool."
    );
    ExitCode::SUCCESS
}

// Wires the resolve tool into a guided agent through documented files only: MCP config gives the
// tool, rules file nudges use on a marker. Reversible with --off.
fn wire_agent(home: &Path, name: &str, mcp_config: &str, rules_file: &str) -> ExitCode {
    let ok = "\u{2713}";
    let mut failed = false;

    let mcp_path = home.join(mcp_config);
    match register_mcp(&mcp_path, home) {
        Ok(true) => println!(
            "  {ok} tool  registered `secondwind mcp` in {}",
            mcp_path.display()
        ),
        Ok(false) => println!("  {ok} tool  already registered in {}", mcp_path.display()),
        Err(err) => {
            eprintln!("  \u{2717} tool  {err}");
            failed = true;
        }
    }

    let rules_path = PathBuf::from(rules_file);
    match write_rules_block(&rules_path, RESOLVE_RULE) {
        Ok(true) => println!(
            "  {ok} rule  added the resolve rule to {}",
            rules_path.display()
        ),
        Ok(false) => println!("  {ok} rule  already present in {}", rules_path.display()),
        Err(err) => {
            eprintln!("  \u{2717} rule  {err}");
            failed = true;
        }
    }

    println!();
    println!(
        "restart {name} to pick up the resolve tool. Undo with `secondwind setup {name} --off`."
    );
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn unwire_agent(home: &Path, name: &str, mcp_config: &str, rules_file: &str) -> ExitCode {
    let ok = "\u{2713}";
    let mcp_path = home.join(mcp_config);
    match unregister_mcp(&mcp_path) {
        Ok(true) => println!(
            "  {ok} tool  removed `secondwind` from {}",
            mcp_path.display()
        ),
        Ok(false) => println!("  - tool  nothing registered in {}", mcp_path.display()),
        Err(err) => eprintln!("  \u{2717} tool  {err}"),
    }
    let rules_path = PathBuf::from(rules_file);
    match remove_rules_block(&rules_path) {
        Ok(true) => println!(
            "  {ok} rule  removed the resolve rule from {}",
            rules_path.display()
        ),
        Ok(false) => println!("  - rule  no resolve rule in {}", rules_path.display()),
        Err(err) => eprintln!("  \u{2717} rule  {err}"),
    }
    println!();
    println!("restart {name} to drop the resolve tool.");
    ExitCode::SUCCESS
}

// Points a subscription CLI's plan traffic at the proxy by writing the keys its registry declares
// plus the resolve MCP server (so offloaded blocks rehydrate). Reversible with --off; backs up
// first and leaves a file that does not parse untouched.
fn plan_agent(home: &Path, name: &str, agent: &crate::agents::Agent, off: bool) -> ExitCode {
    let Some(pc) = &agent.plan else {
        eprintln!("secondwind setup: {name} has no subscription-config seam");
        return ExitCode::FAILURE;
    };
    let path = home.join(pc.config);
    let ok = "\u{2713}";
    if off {
        match remove_plan_config(&path, pc) {
            Ok(true) => println!(
                "  {ok} removed the secondwind provider from {}",
                path.display()
            ),
            Ok(false) => println!("  - no secondwind provider in {}", path.display()),
            Err(err) => {
                eprintln!("  \u{2717} {err}");
                return ExitCode::FAILURE;
            }
        }
        println!("\nrestart {name} to route its plan traffic directly again.");
        return ExitCode::SUCCESS;
    }
    let endpoint = "http://127.0.0.1:8787";
    match write_plan_config(&path, pc, endpoint, home) {
        Ok(true) => println!(
            "  {ok} routed {name}'s plan traffic through {endpoint} in {}",
            path.display()
        ),
        Ok(false) => println!(
            "  {ok} {name} already routed through {endpoint} in {}",
            path.display()
        ),
        Err(err) => {
            eprintln!("  \u{2717} {err}");
            return ExitCode::FAILURE;
        }
    }
    println!();
    println!("Start the proxy in front of {name}'s plan backend (dry run first):");
    println!("  secondwind serve --observe --upstream {}", pc.upstream);
    println!();
    println!("--observe measures the savings and forwards every request UNCHANGED, so you can");
    println!("confirm it works before it touches a byte. Drop --observe to compress for real.");
    if pc.resolve_tool {
        println!();
        println!("`secondwind run {name}` activates recoverable offload of large aged outputs");
        println!(
            "automatically (add `--resolver resolve` if you drive `serve` yourself): {name} loads"
        );
        println!("the registered resolve tool on demand and rehydrates them (you approve that one");
        println!("read-only tool once). Inline compression needs no approval.");
    }
    println!("Undo with `secondwind setup {name} --plan --off`.");
    ExitCode::SUCCESS
}

// Writes every plan route (and the resolve MCP server when the agent offloads), preserving the
// file's comments and layout, backing up first. Idempotent; refuses a file that does not parse.
fn write_plan_config(
    path: &Path,
    pc: &crate::agents::PlanConfig,
    proxy: &str,
    home: &Path,
) -> Result<bool, String> {
    let existing = std::fs::read_to_string(path).unwrap_or_default();
    let mut doc: toml_edit::DocumentMut = existing
        .parse()
        .map_err(|e| format!("{} does not parse, not touching it: {e}", path.display()))?;
    let before = doc.to_string();
    for route in pc.routes {
        set_dotted(&mut doc, route.key, &route.toml_value(proxy))?;
    }
    if pc.resolve_tool {
        let (exe, home_abs) = mcp_command(home);
        set_dotted(
            &mut doc,
            "mcp_servers.secondwind.command",
            &toml_string(&exe),
        )?;
        set_dotted(
            &mut doc,
            "mcp_servers.secondwind.args",
            &format!(
                "[{}, {}, {}]",
                toml_string("--home"),
                toml_string(&home_abs),
                toml_string("mcp")
            ),
        )?;
    }
    if doc.to_string() == before {
        return Ok(false);
    }
    backup_and_write(path, &doc.to_string())
}

fn remove_plan_config(path: &Path, pc: &crate::agents::PlanConfig) -> Result<bool, String> {
    let Ok(existing) = std::fs::read_to_string(path) else {
        return Ok(false);
    };
    let mut doc: toml_edit::DocumentMut = existing
        .parse()
        .map_err(|e| format!("{} does not parse, not touching it: {e}", path.display()))?;
    let before = doc.to_string();
    // Collect the routes' ancestor tables so pruning deepest-first leaves no empty
    // [model_providers]/[mcp_servers] shells behind.
    let mut ancestors: Vec<String> = Vec::new();
    for route in pc.routes {
        remove_dotted(&mut doc, route.key);
        let parts: Vec<&str> = route.key.split('.').collect();
        for depth in (1..parts.len()).rev() {
            ancestors.push(parts[..depth].join("."));
        }
    }
    if pc.resolve_tool {
        remove_dotted(&mut doc, "mcp_servers.secondwind");
        ancestors.push("mcp_servers".to_string());
    }
    ancestors.sort_by_key(|path| std::cmp::Reverse(path.matches('.').count()));
    ancestors.dedup();
    for path in &ancestors {
        prune_empty_dotted(&mut doc, path);
    }
    if doc.to_string() == before {
        return Ok(false);
    }
    backup_and_write(path, &doc.to_string())
}

// The resolve MCP command and absolute home its store lives under, so the launched agent and the
// proxy share one offload store.
fn mcp_command(home: &Path) -> (String, String) {
    let exe = std::env::current_exe()
        .ok()
        .and_then(|p| p.to_str().map(str::to_string))
        .unwrap_or_else(|| "secondwind".to_string());
    let home_abs = std::fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf());
    (exe, home_abs.to_string_lossy().into_owned())
}

// A string as a valid TOML basic string: escape backslashes and quotes, so a Windows path like
// `C:\a\b` becomes `"C:\\a\\b"` rather than an invalid escape sequence.
fn toml_string(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

// Sets a dotted TOML key (foo.bar.baz), creating intermediate tables, from a value written as
// TOML source (quoted string, bare bool, array).
fn set_dotted(doc: &mut toml_edit::DocumentMut, dotted: &str, value: &str) -> Result<(), String> {
    let parsed: toml_edit::Value = value
        .parse()
        .map_err(|e| format!("bad plan value `{value}`: {e}"))?;
    let parts: Vec<&str> = dotted.split('.').collect();
    let (leaf, tables) = parts.split_last().expect("non-empty key");
    let mut node = doc.as_table_mut();
    for part in tables {
        let entry = node.entry(part).or_insert_with(|| {
            let mut table = toml_edit::Table::new();
            // Implicit, so a table holding only sub-tables prints no empty [parent] header.
            table.set_implicit(true);
            toml_edit::Item::Table(table)
        });
        node = entry
            .as_table_mut()
            .ok_or_else(|| format!("{part} in {dotted} is not a table"))?;
    }
    node.insert(leaf, toml_edit::Item::Value(parsed));
    Ok(())
}

fn remove_dotted(doc: &mut toml_edit::DocumentMut, dotted: &str) {
    let parts: Vec<&str> = dotted.split('.').collect();
    let (leaf, tables) = parts.split_last().expect("non-empty key");
    let mut node = doc.as_table_mut();
    for part in tables {
        match node.get_mut(part).and_then(toml_edit::Item::as_table_mut) {
            Some(table) => node = table,
            None => return,
        }
    }
    node.remove(leaf);
}

// Removes the table at a dotted path if it has become empty, so no shell tables linger.
fn prune_empty_dotted(doc: &mut toml_edit::DocumentMut, dotted: &str) {
    let parts: Vec<&str> = dotted.split('.').collect();
    let (leaf, tables) = parts.split_last().expect("non-empty key");
    let mut node = doc.as_table_mut();
    for part in tables {
        match node.get_mut(part).and_then(toml_edit::Item::as_table_mut) {
            Some(table) => node = table,
            None => return,
        }
    }
    if node
        .get(leaf)
        .and_then(toml_edit::Item::as_table)
        .is_some_and(toml_edit::Table::is_empty)
    {
        node.remove(leaf);
    }
}

fn backup_and_write(path: &Path, content: &str) -> Result<bool, String> {
    if path.exists() {
        std::fs::copy(path, rules_backup(path)).map_err(|e| format!("backup failed: {e}"))?;
    } else if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    std::fs::write(path, content).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(true)
}

fn rules_backup(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".secondwind-bak");
    PathBuf::from(name)
}

// Writes or replaces one marked secondwind block in a rules file, preserving other content and
// backing up first. Idempotent: an unchanged file is left untouched.
fn write_rules_block(path: &Path, body: &str) -> Result<bool, String> {
    let existing = std::fs::read_to_string(path).unwrap_or_default();
    let block = format!("{RULES_START}\n{body}\n{RULES_END}");
    let updated = match (existing.find(RULES_START), existing.find(RULES_END)) {
        (Some(s), Some(e)) if e > s => {
            let end = e + RULES_END.len();
            format!("{}{}{}", &existing[..s], block, &existing[end..])
        }
        _ if existing.trim().is_empty() => format!("{block}\n"),
        _ => format!("{}\n\n{block}\n", existing.trim_end()),
    };
    if updated == existing {
        return Ok(false);
    }
    if path.exists() {
        std::fs::copy(path, rules_backup(path)).map_err(|e| format!("backup failed: {e}"))?;
    }
    std::fs::write(path, updated).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(true)
}

fn remove_rules_block(path: &Path) -> Result<bool, String> {
    let Ok(existing) = std::fs::read_to_string(path) else {
        return Ok(false);
    };
    let (Some(s), Some(e)) = (existing.find(RULES_START), existing.find(RULES_END)) else {
        return Ok(false);
    };
    if e <= s {
        return Ok(false);
    }
    let end = e + RULES_END.len();
    let rest = format!("{}{}", &existing[..s], &existing[end..]);
    std::fs::copy(path, rules_backup(path)).map_err(|e| format!("backup failed: {e}"))?;
    let trimmed = rest.trim();
    if trimmed.is_empty() {
        std::fs::remove_file(path).map_err(|e| format!("{}: {e}", path.display()))?;
    } else {
        std::fs::write(path, format!("{trimmed}\n"))
            .map_err(|e| format!("{}: {e}", path.display()))?;
    }
    Ok(true)
}

fn unregister_mcp(config: &Path) -> Result<bool, String> {
    let Ok(raw) = std::fs::read_to_string(config) else {
        return Ok(false);
    };
    let mut root: Value = serde_json::from_str(&raw)
        .map_err(|e| format!("{} does not parse, not touching it: {e}", config.display()))?;
    let Some(servers) = root.get_mut("mcpServers").and_then(Value::as_object_mut) else {
        return Ok(false);
    };
    if servers.remove("secondwind").is_none() {
        return Ok(false);
    }
    save(config, &root).map(|()| true)
}

fn load(path: &Path) -> Result<Value, String> {
    match std::fs::read_to_string(path) {
        Ok(raw) => serde_json::from_str(&raw)
            .map_err(|e| format!("{} does not parse, not touching it: {e}", path.display())),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(json!({})),
        Err(err) => Err(format!("{}: {err}", path.display())),
    }
}

fn save(path: &Path, value: &Value) -> Result<(), String> {
    if path.exists() {
        let backup = path.with_extension("json.secondwind-bak");
        std::fs::copy(path, &backup).map_err(|e| format!("backup failed: {e}"))?;
    } else if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    let pretty = serde_json::to_string_pretty(value).map_err(|e| e.to_string())?;
    std::fs::write(path, pretty).map_err(|e| format!("{}: {e}", path.display()))
}

// Returns whether anything changed. The command pins the home dir the host agent launches the
// server with, so its store matches `serve`/`run`'s and resolve never misses across processes.
fn register_mcp(config: &Path, home: &Path) -> Result<bool, String> {
    let mut root = load(config)?;
    if !root.is_object() {
        return Err(format!("{} is not a JSON object", config.display()));
    }
    let servers = root
        .as_object_mut()
        .expect("checked object")
        .entry("mcpServers")
        .or_insert_with(|| json!({}));
    if !servers.is_object() {
        return Err("mcpServers is not an object".into());
    }
    if servers.get("secondwind").is_some() {
        return Ok(false);
    }
    let args = match std::fs::canonicalize(home) {
        Ok(abs) => json!(["--home", abs.to_string_lossy(), "mcp"]),
        Err(_) => json!(["mcp"]),
    };
    servers["secondwind"] = json!({"command": "secondwind", "args": args});
    save(config, &root)?;
    Ok(true)
}

fn install_hook(settings: &Path) -> Result<bool, String> {
    let mut root = load(settings)?;
    if !root.is_object() {
        return Err(format!("{} is not a JSON object", settings.display()));
    }
    let entries = root
        .as_object_mut()
        .expect("checked object")
        .entry("hooks")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or("hooks is not an object")?
        .entry("PreToolUse")
        .or_insert_with(|| json!([]));
    let list = entries
        .as_array_mut()
        .ok_or("hooks.PreToolUse is not an array")?;
    let present = list
        .iter()
        .any(|entry| entry.to_string().contains("secondwind hook"));
    if present {
        return Ok(false);
    }
    list.push(json!({
        "matcher": "Bash",
        "hooks": [{"type": "command", "command": "secondwind hook"}]
    }));
    save(settings, &root)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("sw-setup-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn toml_string_escapes_a_windows_path() {
        // A Windows exe path has backslashes; unescaped it is invalid TOML (`\a`, `\s` are not
        // escapes), which broke `setup --plan` on Windows. Escaped, it parses back to the exact path.
        let path = r"D:\a\secondwind\target\debug\secondwind.exe";
        let doc: toml_edit::DocumentMut = format!("command = {}", toml_string(path))
            .parse()
            .expect("escaped path is valid TOML");
        assert_eq!(
            doc["command"].as_str(),
            Some(path),
            "roundtrips to the exact path"
        );
    }

    #[test]
    fn rules_block_roundtrips_and_preserves_other_content() {
        let path = scratch("rules").join(".cursorrules");
        std::fs::write(&path, "keep me\n").unwrap();

        assert!(write_rules_block(&path, "USE RESOLVE").unwrap());
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(after.contains("keep me"), "other content is preserved");
        assert!(after.contains("USE RESOLVE"));
        assert!(after.contains(RULES_START) && after.contains(RULES_END));

        assert!(
            !write_rules_block(&path, "USE RESOLVE").unwrap(),
            "an unchanged file is a no-op"
        );

        assert!(remove_rules_block(&path).unwrap());
        let cleaned = std::fs::read_to_string(&path).unwrap();
        assert!(cleaned.contains("keep me"));
        assert!(!cleaned.contains("USE RESOLVE"));
        assert!(!cleaned.contains(RULES_START));
        assert!(
            !remove_rules_block(&path).unwrap(),
            "removing again is a no-op"
        );
    }

    #[test]
    fn plan_config_writes_provider_and_reverses_preserving_comments() {
        use crate::agents::{PlanConfig, PlanRoute, PlanValue};
        let dir = scratch("plan");
        let path = dir.join("config.toml");
        std::fs::write(
            &path,
            "# my codex config\nmodel = \"gpt-5\"\n\n[projects.foo]\ntrust = \"trusted\"\n",
        )
        .unwrap();
        let pc = PlanConfig {
            config: "config.toml",
            routes: &[
                PlanRoute {
                    key: "model_provider",
                    value: PlanValue::Text("secondwind"),
                },
                PlanRoute {
                    key: "openai_base_url",
                    value: PlanValue::Proxy,
                },
                PlanRoute {
                    key: "model_providers.secondwind.base_url",
                    value: PlanValue::Proxy,
                },
                PlanRoute {
                    key: "model_providers.secondwind.supports_websockets",
                    value: PlanValue::Flag,
                },
            ],
            resolve_tool: true,
            upstream: "https://example.test",
        };

        assert!(write_plan_config(&path, &pc, "http://127.0.0.1:8787", &dir).unwrap());
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(after.contains("# my codex config"), "comments preserved");
        assert!(after.contains("model = \"gpt-5\""), "other keys preserved");
        assert!(after.contains("[projects.foo]"), "tables preserved");
        assert!(after.contains("model_provider = \"secondwind\""));
        assert!(
            after.contains("supports_websockets = true"),
            "the flag Codex needs for the WebSocket"
        );
        assert!(
            after.contains("[mcp_servers.secondwind]"),
            "resolve tool wired for offload"
        );

        assert!(
            !write_plan_config(&path, &pc, "http://127.0.0.1:8787", &dir).unwrap(),
            "idempotent"
        );

        assert!(remove_plan_config(&path, &pc).unwrap());
        let cleaned = std::fs::read_to_string(&path).unwrap();
        assert!(!cleaned.contains("secondwind"), "provider fully removed");
        assert!(!cleaned.contains("supports_websockets"));
        assert!(
            cleaned.contains("model = \"gpt-5\""),
            "removal preserves the rest"
        );
        assert!(cleaned.contains("# my codex config"));
    }

    #[test]
    fn registers_the_mcp_server_once_and_preserves_existing_config() {
        let dir = scratch("mcp");
        let config = dir.join(".claude.json");
        std::fs::write(
            &config,
            r#"{"theme":"dark","mcpServers":{"other":{"command":"x"}}}"#,
        )
        .unwrap();

        assert!(register_mcp(&config, &dir).unwrap());
        assert!(!register_mcp(&config, &dir).unwrap(), "idempotent");

        let root: Value = serde_json::from_str(&std::fs::read_to_string(&config).unwrap()).unwrap();
        assert_eq!(root["theme"], "dark");
        assert_eq!(root["mcpServers"]["other"]["command"], "x");
        assert_eq!(root["mcpServers"]["secondwind"]["command"], "secondwind");
        let args = root["mcpServers"]["secondwind"]["args"].as_array().unwrap();
        assert_eq!(args.last().unwrap(), "mcp");
        assert!(args.iter().any(|a| a == "--home"), "home is pinned");
        assert!(config.with_extension("json.secondwind-bak").exists());
    }

    #[test]
    fn refuses_to_touch_a_config_that_does_not_parse() {
        let dir = scratch("corrupt");
        let config = dir.join(".claude.json");
        std::fs::write(&config, "{not json").unwrap();
        assert!(register_mcp(&config, &dir).is_err());
        assert_eq!(std::fs::read_to_string(&config).unwrap(), "{not json");
    }

    #[test]
    fn installs_the_hook_once() {
        let dir = scratch("hook");
        let settings = dir.join("settings.json");
        assert!(install_hook(&settings).unwrap());
        assert!(!install_hook(&settings).unwrap());
        let root: Value =
            serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
        assert_eq!(root["hooks"]["PreToolUse"][0]["matcher"], "Bash");
    }
}
