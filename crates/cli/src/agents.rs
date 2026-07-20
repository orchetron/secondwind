// The agents secondwind can route through the proxy. Adding one is a single AGENTS entry;
// `run` and `setup` both read this registry, so neither carries a per-agent branch.

// How an agent reaches the proxy.
pub enum Route {
    // Terminal agent that honors a base-URL env var. `run` routes it for one session;
    // nothing on disk changes, so there's nothing to undo.
    Launch,
    // GUI app whose only sanctioned routing is a base-URL override the user sets in its own settings;
    // secondwind prints steps + endpoint, never writes the app's config (it holds auth tokens, so a blind
    // write risks corruption). --tool wires the resolve tool via `mcp_config` + `rules_file`; --off reverses.
    Guided {
        app: &'static str,
        steps: &'static [&'static str],
        mcp_config: &'static str,
        rules_file: &'static str,
    },
}

// Subscription config seam: a CLI whose plan traffic ignores base-URL env vars and reads a config file.
// `run` injects these as ephemeral `-c` overrides; `setup <agent> --plan` writes them reversibly. Codex
// needs a custom provider (its `supports_websockets` flag captures the responses WS) plus a base-URL override.
pub struct PlanConfig {
    // Config file under home, e.g. ".codex/config.toml".
    pub config: &'static str,
    // The keys that point this agent's plan traffic at the proxy.
    pub routes: &'static [PlanRoute],
    // Whether to also register the resolve MCP server, so offloaded blocks rehydrate.
    pub resolve_tool: bool,
    // The upstream a `serve` in front of this plan traffic should target.
    pub upstream: &'static str,
}

// One config override that routes plan traffic. `value` resolves against the live proxy URL at
// wire time, so the registry stays port-agnostic.
pub struct PlanRoute {
    pub key: &'static str,
    pub value: PlanValue,
}

pub enum PlanValue {
    // The proxy base URL, e.g. "http://127.0.0.1:8787".
    Proxy,
    // A fixed string, e.g. the provider name.
    Text(&'static str),
    // A boolean flag set true.
    Flag,
}

impl PlanRoute {
    // The TOML value this route sets, resolved against the live proxy URL.
    pub fn toml_value(&self, proxy: &str) -> String {
        match &self.value {
            PlanValue::Proxy => format!("\"{proxy}\""),
            PlanValue::Text(text) => format!("\"{text}\""),
            PlanValue::Flag => "true".to_string(),
        }
    }
}

pub struct Agent {
    // How the user names it: `run codex`, `setup cursor`.
    pub name: &'static str,
    // The PATH binary, for detection and launch.
    pub bin: &'static str,
    pub route: Route,
    pub plan: Option<PlanConfig>,
    // Direct/key-mode model backend, so `run` needs no --upstream. None for multi-provider
    // agents (aider, goose), which require --upstream.
    pub upstream: Option<&'static str>,
}

pub const AGENTS: &[Agent] = &[
    Agent {
        name: "claude",
        bin: "claude",
        route: Route::Launch,
        plan: None,
        upstream: Some("https://api.anthropic.com"),
    },
    Agent {
        name: "codex",
        bin: "codex",
        route: Route::Launch,
        plan: Some(PlanConfig {
            config: ".codex/config.toml",
            // Custom provider carries the traffic: `supports_websockets` makes Codex send its
            // responses WebSocket to our base_url, `requires_openai_auth` keeps the plan's OAuth
            // token attached, top-level `openai_base_url` is the override subscription auth honors.
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
                    key: "model_providers.secondwind.name",
                    value: PlanValue::Text("secondwind"),
                },
                PlanRoute {
                    key: "model_providers.secondwind.base_url",
                    value: PlanValue::Proxy,
                },
                PlanRoute {
                    key: "model_providers.secondwind.supports_websockets",
                    value: PlanValue::Flag,
                },
                PlanRoute {
                    key: "model_providers.secondwind.requires_openai_auth",
                    value: PlanValue::Flag,
                },
            ],
            resolve_tool: true,
            upstream: "https://chatgpt.com/backend-api/codex",
        }),
        upstream: Some("https://api.openai.com"),
    },
    Agent {
        name: "aider",
        bin: "aider",
        route: Route::Launch,
        plan: None,
        upstream: None,
    },
    Agent {
        name: "goose",
        bin: "goose",
        route: Route::Launch,
        plan: None,
        upstream: None,
    },
    Agent {
        name: "opencode",
        bin: "opencode",
        route: Route::Launch,
        plan: None,
        upstream: None,
    },
    Agent {
        name: "cursor",
        bin: "cursor",
        route: Route::Guided {
            app: "/Applications/Cursor.app",
            steps: &[
                "open Cursor Settings (Cmd+,) and go to Models",
                "turn on Override OpenAI Base URL and set it to the endpoint below",
                "add your OpenAI key there, then keep the endpoint running",
            ],
            mcp_config: ".cursor/mcp.json",
            rules_file: ".cursorrules",
        },
        plan: None,
        upstream: None,
    },
];

pub fn get(name: &str) -> Option<&'static Agent> {
    AGENTS.iter().find(|agent| agent.name == name)
}

// Binaries of the launch-routed agents, for `run` to detect what is installed.
pub fn launch_bins() -> Vec<&'static str> {
    AGENTS
        .iter()
        .filter(|agent| matches!(agent.route, Route::Launch))
        .map(|agent| agent.bin)
        .collect()
}

pub fn names() -> Vec<&'static str> {
    AGENTS.iter().map(|agent| agent.name).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_lookups_work() {
        assert!(matches!(
            get("codex").map(|a| &a.route),
            Some(Route::Launch)
        ));
        assert!(matches!(
            get("cursor").map(|a| &a.route),
            Some(Route::Guided { .. })
        ));
        assert!(get("nope").is_none());
        assert!(launch_bins().contains(&"codex"));
        assert!(
            !launch_bins().contains(&"cursor"),
            "guided agents are not launch-routed"
        );
        assert!(names().contains(&"cursor"));
    }
}
