//! Permission rules in Claude Code's notation: `Tool(matcher)`.
//!
//! - `Bash(cargo test:*)` — commands whose first token sequence starts with
//!   "cargo test" (the `:*` suffix means "and anything after").
//! - `Bash(git status)` — that exact command only.
//! - `Read(./.env)` / `Read(**/*.pem)` — path glob for file tools.
//! - `Edit` — every call to a tool, no matcher.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rule {
    pub tool: String,
    /// None = matches every call to the tool.
    pub matcher: Option<String>,
}

impl Rule {
    /// Parse "Bash(cargo test:*)" / "Edit" style strings. Returns None on
    /// malformed input (unbalanced parens).
    pub fn parse(s: &str) -> Option<Rule> {
        let s = s.trim();
        match s.split_once('(') {
            None => {
                if s.is_empty() || s.contains(')') {
                    None
                } else {
                    Some(Rule {
                        tool: s.to_string(),
                        matcher: None,
                    })
                }
            }
            Some((tool, rest)) => {
                let matcher = rest.strip_suffix(')')?;
                if tool.is_empty() {
                    return None;
                }
                Some(Rule {
                    tool: tool.to_string(),
                    matcher: Some(matcher.to_string()),
                })
            }
        }
    }

    pub fn matches(&self, tool_name: &str, args: &serde_json::Value) -> bool {
        if !self.tool.eq_ignore_ascii_case(tool_name) {
            return false;
        }
        let Some(matcher) = &self.matcher else {
            return true; // bare tool rule matches all calls
        };
        match tool_name.to_ascii_lowercase().as_str() {
            "bash" => {
                let command = args.get("command").and_then(|v| v.as_str()).unwrap_or("");
                command_matches(matcher, command)
            }
            // File tools: glob match on the path argument.
            _ => {
                let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
                globset::GlobBuilder::new(matcher)
                    .literal_separator(false)
                    .build()
                    .map(|g| g.compile_matcher().is_match(path))
                    .unwrap_or(false)
            }
        }
    }
}

/// `cargo test:*` matches "cargo test" and "cargo test --all";
/// `git status` matches exactly "git status".
fn command_matches(matcher: &str, command: &str) -> bool {
    let command = command.trim();
    match matcher.strip_suffix(":*") {
        Some(prefix) => {
            let prefix = prefix.trim();
            command == prefix || command.starts_with(&format!("{prefix} "))
        }
        None => command == matcher.trim(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_bare_and_matcher_forms() {
        assert_eq!(
            Rule::parse("Edit").unwrap(),
            Rule {
                tool: "Edit".into(),
                matcher: None
            }
        );
        assert_eq!(
            Rule::parse("Bash(cargo test:*)").unwrap(),
            Rule {
                tool: "Bash".into(),
                matcher: Some("cargo test:*".into())
            }
        );
        assert!(Rule::parse("Bash(unclosed").is_none());
        assert!(Rule::parse("").is_none());
    }

    #[test]
    fn bash_prefix_matching() {
        let rule = Rule::parse("Bash(cargo test:*)").unwrap();
        assert!(rule.matches("bash", &json!({"command": "cargo test"})));
        assert!(rule.matches("bash", &json!({"command": "cargo test --all"})));
        assert!(!rule.matches("bash", &json!({"command": "cargo testx"})));
        assert!(!rule.matches("bash", &json!({"command": "cargo build"})));
    }

    #[test]
    fn bash_exact_matching() {
        let rule = Rule::parse("Bash(git status)").unwrap();
        assert!(rule.matches("bash", &json!({"command": "git status"})));
        assert!(!rule.matches("bash", &json!({"command": "git status --short"})));
    }

    #[test]
    fn path_glob_matching() {
        let rule = Rule::parse("Read(**/*.pem)").unwrap();
        assert!(rule.matches("read", &json!({"path": "certs/server.pem"})));
        assert!(!rule.matches("read", &json!({"path": "src/main.rs"})));
    }
}
