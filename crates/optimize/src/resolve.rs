use serde_json::{Value, json};

use crate::offload::Store;

pub const TOOL_NAME: &str = "secondwind_resolve";

pub fn tool_def() -> Value {
    json!({
        "name": TOOL_NAME,
        "description": "Fetch content behind an offload marker of the form <<swload:...>>. Omit \
            `select` for the whole block, or pass a selector to fetch just one part.",
        "input_schema": {
            "type": "object",
            "properties": {
                "marker": {
                    "type": "string",
                    "description": "The <<swload:...>> marker to expand."
                },
                "select": {
                    "type": "string",
                    "description": "Optional slice: a '/'-separated path where a segment is \
                        `field=value` (the record whose field equals value), `[n]` (array index), \
                        or a field name; or `L<a>-<b>` for a line range. E.g. `number=13932/body`."
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

pub fn handle(store: &Store, marker: &str, select: &str) -> Option<String> {
    let block = store.resolve(marker)?;
    if select.trim().is_empty() {
        return Some(block);
    }
    crate::select::select(&block, select.trim())
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
        assert_eq!(
            handle(&store, &out.marker, "").as_deref(),
            Some(raw.as_str())
        );
        assert_eq!(handle(&store, "<<swload:missing>>", ""), None);
    }

    #[test]
    fn handle_returns_one_selected_field() {
        let store = Store::default();
        let raw = format!(
            "[{}]",
            (0..40)
                .map(|i| format!(r#"{{"n":{i},"body":"detail for row {i} that is long enough"}}"#))
                .collect::<Vec<_>>()
                .join(",")
        );
        let out = store.offload(&raw).unwrap();
        assert_eq!(
            handle(&store, &out.marker, "n=7/body").as_deref(),
            Some("detail for row 7 that is long enough")
        );
    }
}
