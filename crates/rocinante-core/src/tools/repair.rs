//! Tool-call repair: local models drift from the native tool-call format.
//! This module recovers calls that arrive as prose/markdown-JSON and
//! validates arguments against tool schemas so bad calls bounce back to the
//! model with a usable error instead of crashing the turn.

use rocinante_providers::ToolCall;
use serde_json::Value;

use super::registry::ToolRegistry;

/// Result of validating one native tool call.
pub enum Validation {
    Ok,
    /// Args don't fit the schema; message is written for the model.
    Invalid(String),
    UnknownTool(String),
}

pub fn validate_call(registry: &ToolRegistry, call: &ToolCall) -> Validation {
    let Some(tool) = registry.get(&call.name) else {
        return Validation::UnknownTool(format!(
            "unknown tool `{}`. Available tools: {}",
            call.name,
            registry.names().join(", ")
        ));
    };
    let schema = tool.schema();
    match jsonschema::validator_for(&schema) {
        Ok(validator) => {
            let errors: Vec<String> = validator
                .iter_errors(&call.arguments)
                .map(|e| e.to_string())
                .take(3)
                .collect();
            if errors.is_empty() {
                Validation::Ok
            } else {
                Validation::Invalid(format!(
                    "invalid arguments for `{}`: {}. Required schema: {}",
                    call.name,
                    errors.join("; "),
                    schema
                ))
            }
        }
        Err(e) => {
            tracing::error!(tool = call.name, error = %e, "tool schema itself is invalid");
            Validation::Ok // don't punish the model for our bug
        }
    }
}

/// Scrape tool calls out of assistant prose. Handles the classic failure
/// modes: fenced ```json blocks, ```tool_call blocks, and bare top-level
/// JSON objects, in shapes {"name": ..., "arguments": ...} or
/// {"tool": ..., "input"/"parameters": ...}.
pub fn scrape_tool_calls(registry: &ToolRegistry, text: &str) -> Vec<ToolCall> {
    let mut found: Vec<ToolCall> = Vec::new();
    let mut seen: std::collections::HashSet<String> = Default::default();
    for candidate in json_candidates(text) {
        let Some(value) = lenient_parse(&candidate) else {
            continue;
        };
        for obj in flatten_candidates(value) {
            if let Some(call) = call_from_value(registry, &obj) {
                // Fenced-block content is also visible to the bare-brace
                // scan; drop exact duplicates.
                let key = format!("{}\u{0}{}", call.name, call.arguments);
                if seen.insert(key) {
                    found.push(call);
                }
            }
        }
    }
    if !found.is_empty() {
        tracing::info!(
            count = found.len(),
            raw = text,
            "repaired tool calls from prose"
        );
    }
    found
}

/// Candidate JSON substrings: fenced code blocks first, then bare
/// brace-balanced runs.
fn json_candidates(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    // Fenced blocks: ```json ... ``` or ```tool_call ... ``` or bare ```.
    let mut rest = text;
    while let Some(start) = rest.find("```") {
        let after = &rest[start + 3..];
        let body_start = after.find('\n').map(|i| i + 1).unwrap_or(0);
        if let Some(end) = after[body_start..].find("```") {
            out.push(after[body_start..body_start + end].trim().to_string());
            rest = &after[body_start + end + 3..];
        } else {
            break;
        }
    }
    // Bare top-level {...} runs (brace-balanced scan, string-aware).
    out.extend(balanced_objects(text));
    out
}

fn balanced_objects(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'{' {
            let mut depth = 0usize;
            let mut in_str = false;
            let mut escape = false;
            let mut end = None;
            for (j, &b) in bytes[i..].iter().enumerate() {
                if escape {
                    escape = false;
                    continue;
                }
                match b {
                    b'\\' if in_str => escape = true,
                    b'"' => in_str = !in_str,
                    b'{' if !in_str => depth += 1,
                    b'}' if !in_str => {
                        depth -= 1;
                        if depth == 0 {
                            end = Some(i + j);
                            break;
                        }
                    }
                    _ => {}
                }
            }
            match end {
                Some(e) => {
                    out.push(text[i..=e].to_string());
                    i = e + 1;
                }
                None => break,
            }
        } else {
            i += 1;
        }
    }
    out
}

/// serde_json first; then a lenient pass fixing trailing commas and smart
/// quotes — the two most common local-model JSON mistakes.
fn lenient_parse(s: &str) -> Option<Value> {
    let s = s.trim();
    if !s.starts_with('{') && !s.starts_with('[') {
        return None;
    }
    if let Ok(v) = serde_json::from_str::<Value>(s) {
        return Some(v);
    }
    let cleaned = s
        .replace(['\u{201c}', '\u{201d}'], "\"")
        .replace(['\u{2018}', '\u{2019}'], "'");
    let cleaned = remove_trailing_commas(&cleaned);
    serde_json::from_str(&cleaned).ok()
}

fn remove_trailing_commas(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_str = false;
    let mut escape = false;
    let chars: Vec<char> = s.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if escape {
            escape = false;
            out.push(c);
            continue;
        }
        match c {
            '\\' if in_str => {
                escape = true;
                out.push(c);
            }
            '"' => {
                in_str = !in_str;
                out.push(c);
            }
            ',' if !in_str => {
                let next_significant = chars[i + 1..].iter().find(|ch| !ch.is_whitespace());
                if matches!(next_significant, Some('}') | Some(']')) {
                    continue; // drop the trailing comma
                }
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out
}

/// A candidate may be a single call object or {"tool_calls": [...]}.
fn flatten_candidates(value: Value) -> Vec<Value> {
    match value {
        Value::Array(items) => items,
        Value::Object(ref map) => {
            if let Some(Value::Array(items)) = map.get("tool_calls") {
                items.clone()
            } else {
                vec![value]
            }
        }
        _ => vec![],
    }
}

fn call_from_value(registry: &ToolRegistry, value: &Value) -> Option<ToolCall> {
    let obj = value.as_object()?;
    let name = obj
        .get("name")
        .or_else(|| obj.get("tool"))
        .or_else(|| obj.get("tool_name"))
        .or_else(|| obj.get("function"))
        .and_then(|v| match v {
            Value::String(s) => Some(s.clone()),
            // {"function": {"name": ..., "arguments": ...}} (OpenAI shape)
            Value::Object(f) => f.get("name").and_then(|n| n.as_str()).map(String::from),
            _ => None,
        })?;
    let tool = registry.get(&name)?; // only accept names we actually have
    let arguments = obj
        .get("arguments")
        .or_else(|| obj.get("input"))
        .or_else(|| obj.get("parameters"))
        .or_else(|| obj.get("args"))
        .or_else(|| obj.get("function").and_then(|f| f.get("arguments")))
        .cloned()
        .unwrap_or(Value::Object(Default::default()));
    // Arguments sometimes arrive as a JSON-encoded string.
    let arguments = match arguments {
        Value::String(s) => serde_json::from_str(&s).unwrap_or(Value::String(s)),
        other => other,
    };
    Some(ToolCall {
        id: format!("repaired_{}", tool.name()),
        name: tool.name().to_string(),
        arguments,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> ToolRegistry {
        ToolRegistry::core()
    }

    #[test]
    fn scrapes_fenced_json_block() {
        let text = "I'll read the file:\n```json\n{\"name\": \"read\", \"arguments\": {\"path\": \"src/main.rs\"}}\n```";
        let calls = scrape_tool_calls(&registry(), text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "read");
        assert_eq!(calls[0].arguments["path"], "src/main.rs");
    }

    #[test]
    fn scrapes_bare_object_with_tool_key() {
        let text = r#"Let me search. {"tool": "grep", "input": {"pattern": "fn main"}}"#;
        let calls = scrape_tool_calls(&registry(), text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "grep");
    }

    #[test]
    fn tolerates_trailing_comma_and_smart_quotes() {
        let text = "```json\n{\u{201c}name\u{201d}: \u{201c}glob\u{201d}, \u{201c}arguments\u{201d}: {\u{201c}pattern\u{201d}: \u{201c}**/*.rs\u{201d},}}\n```";
        let calls = scrape_tool_calls(&registry(), text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "glob");
    }

    #[test]
    fn ignores_json_that_is_not_a_tool_call() {
        let text = r#"Config looks like {"defaults": {"model": "main"}} which is fine."#;
        assert!(scrape_tool_calls(&registry(), text).is_empty());
    }

    #[test]
    fn ignores_unknown_tools() {
        let text = r#"{"name": "launch_missiles", "arguments": {}}"#;
        assert!(scrape_tool_calls(&registry(), text).is_empty());
    }

    #[test]
    fn openai_function_shape() {
        let text = r#"{"function": {"name": "read", "arguments": "{\"path\": \"x.rs\"}"}}"#;
        let calls = scrape_tool_calls(&registry(), text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].arguments["path"], "x.rs");
    }

    #[test]
    fn validation_rejects_missing_required_field() {
        let call = ToolCall {
            id: "1".into(),
            name: "read".into(),
            arguments: serde_json::json!({"offset": 3}),
        };
        assert!(matches!(
            validate_call(&registry(), &call),
            Validation::Invalid(_)
        ));
    }

    #[test]
    fn validation_accepts_good_call() {
        let call = ToolCall {
            id: "1".into(),
            name: "read".into(),
            arguments: serde_json::json!({"path": "src/lib.rs"}),
        };
        assert!(matches!(validate_call(&registry(), &call), Validation::Ok));
    }

    #[test]
    fn tool_calls_envelope_shape() {
        let text = r#"{"tool_calls": [{"name": "read", "arguments": {"path": "a"}}, {"name": "glob", "arguments": {"pattern": "*"}}]}"#;
        let calls = scrape_tool_calls(&registry(), text);
        assert_eq!(calls.len(), 2);
    }
}
