use std::collections::HashSet;

use serde_json::Value;

// Locates tool-output blocks so one compression path works across wire formats. A new format is
// one impl of this trait plus a branch in pick_shaper.
pub trait RequestShaper {
    fn resolver(&self, body: &Value) -> Option<String>;
    fn latest_query(&self, body: &Value) -> Option<String>;
    fn exempt_ids(&self, body: &Value, resolver: &str) -> HashSet<String>;
    fn rewrite_tool_outputs(
        &self,
        body: &mut Value,
        exempt: &HashSet<String>,
        f: &mut dyn FnMut(&str, u32) -> Option<String>,
    );
}

// Model turns after each item, so a tool output ages by turns-since-produced: 0 = freshest (not yet
// responded to). Derived from the request itself, so aging needs no cross-request state.
fn turns_after(items: &[Value], is_turn: impl Fn(&Value) -> bool) -> Vec<u32> {
    let mut out = vec![0u32; items.len()];
    let mut after = 0u32;
    for i in (0..items.len()).rev() {
        out[i] = after;
        if is_turn(&items[i]) {
            after += 1;
        }
    }
    out
}

fn is_assistant(item: &Value) -> bool {
    item.get("role").and_then(Value::as_str) == Some("assistant")
}

// Detects wire shape by structure: role "tool" (chat), converse blocks or toolConfig (Bedrock),
// top-level `input` (responses), else Anthropic content-block tool_results.
pub(crate) fn pick_shaper(body: &Value) -> Box<dyn RequestShaper> {
    if let Some(messages) = body.get("messages").and_then(Value::as_array) {
        if messages
            .iter()
            .any(|m| m.get("role").and_then(Value::as_str) == Some("tool"))
        {
            return Box::new(OpenAiShaper);
        }
        if body.get("toolConfig").is_some() || messages.iter().any(is_converse_message) {
            return Box::new(BedrockShaper);
        }
        return Box::new(AnthropicShaper);
    }
    if body.get("input").and_then(Value::as_array).is_some() {
        return Box::new(ResponsesShaper);
    }
    Box::new(AnthropicShaper)
}

// Converse (camelCase toolResult/toolUse blocks) vs Anthropic Messages.
fn is_converse_message(message: &Value) -> bool {
    message
        .get("content")
        .and_then(Value::as_array)
        .is_some_and(|blocks| {
            blocks
                .iter()
                .any(|b| b.get("toolResult").is_some() || b.get("toolUse").is_some())
        })
}

// The resolver tool, matched by name or by the marker form in its description (so a rename counts).
pub fn sense_resolver(body: &Value) -> Option<String> {
    let tools = body.get("tools")?.as_array()?;
    tools
        .iter()
        .find(|tool| {
            let name = tool
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_ascii_lowercase();
            let description = tool
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or_default();
            (name.contains("secondwind") && name.contains("resolve"))
                || (name.contains("resolve") && description.contains("swload"))
        })
        .and_then(|tool| tool.get("name").and_then(Value::as_str))
        .map(str::to_string)
}

// tool_use ids of resolver calls, so their results are never rewritten: re-offloading content the
// model just resolved would loop it back through resolve.
fn resolver_result_ids(body: &Value, resolver: &str) -> HashSet<String> {
    let mut ids = HashSet::new();
    let Some(messages) = body.get("messages").and_then(Value::as_array) else {
        return ids;
    };
    for message in messages {
        let Some(blocks) = message.get("content").and_then(Value::as_array) else {
            continue;
        };
        for block in blocks {
            if block.get("type").and_then(Value::as_str) == Some("tool_use")
                && block.get("name").and_then(Value::as_str) == Some(resolver)
                && let Some(id) = block.get("id").and_then(Value::as_str)
            {
                ids.insert(id.to_string());
            }
        }
    }
    ids
}

// Most recent user turn with actual text (skipping tool-result-only turns): what the output answers.
fn latest_message_query(body: &Value) -> Option<String> {
    let messages = body.get("messages")?.as_array()?;
    for message in messages.iter().rev() {
        if message.get("role").and_then(Value::as_str) != Some("user") {
            continue;
        }
        let text = user_text(message.get("content"));
        if !text.trim().is_empty() {
            return Some(text);
        }
    }
    None
}

fn user_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join(" "),
        _ => String::new(),
    }
}

// Content-block form: tool outputs are content blocks of type tool_result.
struct AnthropicShaper;

impl RequestShaper for AnthropicShaper {
    fn resolver(&self, body: &Value) -> Option<String> {
        sense_resolver(body)
    }
    fn latest_query(&self, body: &Value) -> Option<String> {
        latest_message_query(body)
    }
    fn exempt_ids(&self, body: &Value, resolver: &str) -> HashSet<String> {
        resolver_result_ids(body, resolver)
    }
    fn rewrite_tool_outputs(
        &self,
        body: &mut Value,
        exempt: &HashSet<String>,
        f: &mut dyn FnMut(&str, u32) -> Option<String>,
    ) {
        let Some(items) = body.get("messages").and_then(Value::as_array) else {
            return;
        };
        let ages = turns_after(items, is_assistant);
        let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) else {
            return;
        };
        for (i, message) in messages.iter_mut().enumerate() {
            let Some(blocks) = message.get_mut("content").and_then(Value::as_array_mut) else {
                continue;
            };
            for block in blocks {
                if block.get("type").and_then(Value::as_str) != Some("tool_result") {
                    continue;
                }
                if let Some(id) = block.get("tool_use_id").and_then(Value::as_str)
                    && exempt.contains(id)
                {
                    continue;
                }
                let Some(content) = block.get("content") else {
                    continue;
                };
                let raw = match content {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                if let Some(new) = f(&raw, ages[i]) {
                    block["content"] = Value::String(new);
                }
            }
        }
    }
}

// Role-tool form: outputs are role "tool" messages; resolver is a function tool, its replies exempt.
struct OpenAiShaper;

impl RequestShaper for OpenAiShaper {
    fn resolver(&self, body: &Value) -> Option<String> {
        let tools = body.get("tools")?.as_array()?;
        tools.iter().find_map(|tool| {
            let function = tool.get("function")?;
            let name = function.get("name")?.as_str()?;
            let lname = name.to_ascii_lowercase();
            let desc = function
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or_default();
            ((lname.contains("secondwind") && lname.contains("resolve"))
                || (lname.contains("resolve") && desc.contains("swload")))
            .then(|| name.to_string())
        })
    }
    fn latest_query(&self, body: &Value) -> Option<String> {
        latest_message_query(body)
    }
    fn exempt_ids(&self, body: &Value, resolver: &str) -> HashSet<String> {
        let mut ids = HashSet::new();
        let Some(messages) = body.get("messages").and_then(Value::as_array) else {
            return ids;
        };
        for message in messages {
            if message.get("role").and_then(Value::as_str) != Some("assistant") {
                continue;
            }
            let Some(calls) = message.get("tool_calls").and_then(Value::as_array) else {
                continue;
            };
            for call in calls {
                if call
                    .get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(Value::as_str)
                    == Some(resolver)
                    && let Some(id) = call.get("id").and_then(Value::as_str)
                {
                    ids.insert(id.to_string());
                }
            }
        }
        ids
    }
    fn rewrite_tool_outputs(
        &self,
        body: &mut Value,
        exempt: &HashSet<String>,
        f: &mut dyn FnMut(&str, u32) -> Option<String>,
    ) {
        let Some(items) = body.get("messages").and_then(Value::as_array) else {
            return;
        };
        let ages = turns_after(items, is_assistant);
        let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) else {
            return;
        };
        for (i, message) in messages.iter_mut().enumerate() {
            if message.get("role").and_then(Value::as_str) != Some("tool") {
                continue;
            }
            if let Some(id) = message.get("tool_call_id").and_then(Value::as_str)
                && exempt.contains(id)
            {
                continue;
            }
            let Some(content) = message.get("content") else {
                continue;
            };
            let raw = match content {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            if let Some(new) = f(&raw, ages[i]) {
                message["content"] = Value::String(new);
            }
        }
    }
}

// Input-item form: `input` array; outputs are function_call_output; tools are flat ({type,name}).
struct ResponsesShaper;

impl RequestShaper for ResponsesShaper {
    fn resolver(&self, body: &Value) -> Option<String> {
        let tools = body.get("tools")?.as_array()?;
        tools.iter().find_map(|tool| {
            if tool.get("type").and_then(Value::as_str) != Some("function") {
                return None;
            }
            let name = tool.get("name")?.as_str()?;
            let lname = name.to_ascii_lowercase();
            let desc = tool
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or_default();
            ((lname.contains("secondwind") && lname.contains("resolve"))
                || (lname.contains("resolve") && desc.contains("swload")))
            .then(|| name.to_string())
        })
    }
    fn latest_query(&self, body: &Value) -> Option<String> {
        let input = body.get("input")?.as_array()?;
        for item in input.iter().rev() {
            if item.get("role").and_then(Value::as_str) != Some("user") {
                continue;
            }
            let text = match item.get("content") {
                Some(Value::String(s)) => s.clone(),
                Some(Value::Array(parts)) => parts
                    .iter()
                    .filter_map(|p| p.get("text").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join(" "),
                _ => String::new(),
            };
            if !text.trim().is_empty() {
                return Some(text);
            }
        }
        None
    }
    fn exempt_ids(&self, body: &Value, resolver: &str) -> HashSet<String> {
        let mut ids = HashSet::new();
        let Some(input) = body.get("input").and_then(Value::as_array) else {
            return ids;
        };
        for item in input {
            if item.get("type").and_then(Value::as_str) == Some("function_call")
                && item.get("name").and_then(Value::as_str) == Some(resolver)
                && let Some(id) = item.get("call_id").and_then(Value::as_str)
            {
                ids.insert(id.to_string());
            }
        }
        ids
    }
    fn rewrite_tool_outputs(
        &self,
        body: &mut Value,
        exempt: &HashSet<String>,
        f: &mut dyn FnMut(&str, u32) -> Option<String>,
    ) {
        let Some(items) = body.get("input").and_then(Value::as_array) else {
            return;
        };
        let ages = turns_after(items, |it| {
            it.get("type").and_then(Value::as_str) == Some("function_call") || is_assistant(it)
        });
        let Some(input) = body.get_mut("input").and_then(Value::as_array_mut) else {
            return;
        };
        for (i, item) in input.iter_mut().enumerate() {
            if item.get("type").and_then(Value::as_str) != Some("function_call_output") {
                continue;
            }
            if let Some(id) = item.get("call_id").and_then(Value::as_str)
                && exempt.contains(id)
            {
                continue;
            }
            let Some(content) = item.get("output") else {
                continue;
            };
            let raw = match content {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            if let Some(new) = f(&raw, ages[i]) {
                item["output"] = Value::String(new);
            }
        }
    }
}

// Bedrock Converse: camelCase content blocks; outputs are `toolResult` (toolUseId), tools under
// toolConfig.tools[].toolSpec. A camelCase cousin of Anthropic.
struct BedrockShaper;

impl RequestShaper for BedrockShaper {
    fn resolver(&self, body: &Value) -> Option<String> {
        let tools = body.get("toolConfig")?.get("tools")?.as_array()?;
        tools.iter().find_map(|tool| {
            let spec = tool.get("toolSpec")?;
            let name = spec.get("name")?.as_str()?;
            let lname = name.to_ascii_lowercase();
            let desc = spec
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or_default();
            ((lname.contains("secondwind") && lname.contains("resolve"))
                || (lname.contains("resolve") && desc.contains("swload")))
            .then(|| name.to_string())
        })
    }
    fn latest_query(&self, body: &Value) -> Option<String> {
        let messages = body.get("messages")?.as_array()?;
        for message in messages.iter().rev() {
            if message.get("role").and_then(Value::as_str) != Some("user") {
                continue;
            }
            let Some(blocks) = message.get("content").and_then(Value::as_array) else {
                continue;
            };
            let text = blocks
                .iter()
                .filter_map(|b| b.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join(" ");
            if !text.trim().is_empty() {
                return Some(text);
            }
        }
        None
    }
    fn exempt_ids(&self, body: &Value, resolver: &str) -> HashSet<String> {
        let mut ids = HashSet::new();
        let Some(messages) = body.get("messages").and_then(Value::as_array) else {
            return ids;
        };
        for message in messages {
            let Some(blocks) = message.get("content").and_then(Value::as_array) else {
                continue;
            };
            for block in blocks {
                if let Some(call) = block.get("toolUse")
                    && call.get("name").and_then(Value::as_str) == Some(resolver)
                    && let Some(id) = call.get("toolUseId").and_then(Value::as_str)
                {
                    ids.insert(id.to_string());
                }
            }
        }
        ids
    }
    fn rewrite_tool_outputs(
        &self,
        body: &mut Value,
        exempt: &HashSet<String>,
        f: &mut dyn FnMut(&str, u32) -> Option<String>,
    ) {
        let Some(items) = body.get("messages").and_then(Value::as_array) else {
            return;
        };
        let ages = turns_after(items, is_assistant);
        let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) else {
            return;
        };
        for (i, message) in messages.iter_mut().enumerate() {
            let Some(blocks) = message.get_mut("content").and_then(Value::as_array_mut) else {
                continue;
            };
            for block in blocks {
                let Some(result) = block.get("toolResult") else {
                    continue;
                };
                if let Some(id) = result.get("toolUseId").and_then(Value::as_str)
                    && exempt.contains(id)
                {
                    continue;
                }
                // Only a lone text item is rewritable; a structured `json` item, images, and
                // multi-item content stay verbatim (flattening json to text would change its type).
                let Some(items) = result.get("content").and_then(Value::as_array) else {
                    continue;
                };
                if items.len() != 1 {
                    continue;
                }
                let Some(text) = items[0].get("text").and_then(Value::as_str) else {
                    continue;
                };
                let raw = text.to_string();
                if let Some(new) = f(&raw, ages[i]) {
                    block["toolResult"]["content"] = serde_json::json!([{ "text": new }]);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn senses_the_mcp_resolve_tool_by_name() {
        let body = json!({"tools": [
            {"name": "Read"},
            {"name": "mcp__secondwind__resolve", "description": "Fetch content"}
        ]});
        assert_eq!(
            sense_resolver(&body).as_deref(),
            Some("mcp__secondwind__resolve")
        );
    }

    #[test]
    fn senses_a_renamed_resolver_by_its_marker_description() {
        let body = json!({"tools": [
            {"name": "mcp__sw__resolve", "description": "Fetch the body behind a <<swload:...>> marker"}
        ]});
        assert_eq!(sense_resolver(&body).as_deref(), Some("mcp__sw__resolve"));
    }

    #[test]
    fn no_resolver_is_sensed_from_ordinary_tools() {
        assert!(sense_resolver(&json!({"tools": [{"name": "Read"}, {"name": "Bash"}]})).is_none());
        assert!(sense_resolver(&json!({"messages": []})).is_none());
    }

    #[test]
    fn pick_shaper_detects_each_wire_shape() {
        let anthropic = json!({"messages": [{"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": "t1", "content": "x"}
        ]}]});
        assert!(pick_shaper(&anthropic).resolver(&anthropic).is_none());

        let openai = json!({"messages": [{"role": "tool", "tool_call_id": "c1", "content": "x"}]});
        let mut body = openai.clone();
        // The role-tool shaper reads role "tool" outputs; a marker-carrying rewrite hits it.
        pick_shaper(&openai).rewrite_tool_outputs(&mut body, &HashSet::new(), &mut |_, _| {
            Some("touched".to_string())
        });
        assert_eq!(body["messages"][0]["content"], "touched");

        let responses = json!({"input": [
            {"type": "function_call_output", "call_id": "fc1", "output": "x"}
        ]});
        let mut body = responses.clone();
        pick_shaper(&responses).rewrite_tool_outputs(&mut body, &HashSet::new(), &mut |_, _| {
            Some("touched".to_string())
        });
        assert_eq!(body["input"][0]["output"], "touched");
    }

    #[test]
    fn bedrock_leaves_a_structured_json_tool_result_verbatim() {
        let mut body = json!({"messages": [{"role": "user", "content": [{
            "toolResult": {"toolUseId": "t1", "content": [{"json": {"rows": [{"id": 1}, {"id": 2}]}}]}
        }]}]});
        let before = body.clone();
        BedrockShaper.rewrite_tool_outputs(&mut body, &HashSet::new(), &mut |raw, _| {
            Some(format!("SWCOL:{}", raw.len()))
        });
        assert_eq!(
            body, before,
            "a json tool-result must stay verbatim, not flatten to text"
        );
    }

    #[test]
    fn bedrock_still_compresses_a_text_tool_result() {
        let mut body = json!({"messages": [{"role": "user", "content": [{
            "toolResult": {"toolUseId": "t1", "content": [{"text": "a long tool output here"}]}
        }]}]});
        BedrockShaper.rewrite_tool_outputs(&mut body, &HashSet::new(), &mut |_, _| {
            Some("COMPRESSED".to_string())
        });
        assert_eq!(
            body["messages"][0]["content"][0]["toolResult"]["content"][0]["text"],
            "COMPRESSED"
        );
    }
}
