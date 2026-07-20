use serde_json::{Value, json};

use crate::offload::Store;

pub const TOOL_NAME: &str = "secondwind_resolve";

pub fn tool_def() -> Value {
    json!({
        "name": TOOL_NAME,
        "description": "Fetch the full original content of a block shown as an \
            offload marker of the form <<swload:...>>. Call this when you need the \
            complete data behind such a marker.",
        "input_schema": {
            "type": "object",
            "properties": {
                "marker": {
                    "type": "string",
                    "description": "The <<swload:...>> marker to expand."
                }
            },
            "required": ["marker"]
        }
    })
}

// Injected once into the stable prefix; re-injecting must not reshuffle the
// tools array or it would bust the provider cache.
pub fn inject_once(tools: &mut Vec<Value>) {
    let present = tools
        .iter()
        .any(|t| t.get("name").and_then(Value::as_str) == Some(TOOL_NAME));
    if !present {
        tools.push(tool_def());
    }
}

pub fn handle(store: &Store, marker: &str) -> Option<String> {
    store.resolve(marker)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn injection_is_idempotent() {
        let mut tools = vec![json!({"name": "Read"})];
        inject_once(&mut tools);
        inject_once(&mut tools);
        let count = tools
            .iter()
            .filter(|t| t.get("name").and_then(Value::as_str) == Some(TOOL_NAME))
            .count();
        assert_eq!(count, 1);
    }

    #[test]
    fn injection_preserves_prior_tool_order() {
        let mut tools = vec![json!({"name": "Read"}), json!({"name": "Bash"})];
        inject_once(&mut tools);
        assert_eq!(tools[0]["name"], "Read");
        assert_eq!(tools[1]["name"], "Bash");
    }

    #[test]
    fn handle_returns_the_stored_body() {
        let store = Store::default();
        let raw = format!(
            "{{{}}}",
            (0..200)
                .map(|i| format!(r#""k{i}":{i}"#))
                .collect::<Vec<_>>()
                .join(",")
        );
        let out = store.offload(&raw).unwrap();
        assert!(handle(&store, &out.marker).is_some());
        assert_eq!(handle(&store, "<<swload:missing>>"), None);
    }
}
