use std::sync::Mutex;

use crate::config::{Mode, PermissionsConfig};
use crate::tools::{Tool, ToolKind};

use super::rules::Rule;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Deny { reason: String },
    Ask,
}

pub struct PermissionEngine {
    allow: Vec<Rule>,
    deny: Vec<Rule>,
    /// "Always allow" answers accumulated this session.
    session_allow: Mutex<Vec<Rule>>,
}

impl PermissionEngine {
    pub fn from_config(config: &PermissionsConfig) -> Self {
        let parse_all = |rules: &[String]| {
            rules
                .iter()
                .filter_map(|s| {
                    let rule = Rule::parse(s);
                    if rule.is_none() {
                        tracing::warn!(rule = %s, "ignoring malformed permission rule");
                    }
                    rule
                })
                .collect()
        };
        Self {
            allow: parse_all(&config.allow),
            deny: parse_all(&config.deny),
            session_allow: Mutex::new(Vec::new()),
        }
    }

    /// Remember an "always allow" answer for the rest of the session.
    /// For bash we remember the exact command; for file tools the exact path.
    pub fn remember_allow(&self, tool_name: &str, args: &serde_json::Value) {
        let matcher = match tool_name.to_ascii_lowercase().as_str() {
            "bash" => args
                .get("command")
                .and_then(|v| v.as_str())
                .map(String::from),
            _ => args.get("path").and_then(|v| v.as_str()).map(String::from),
        };
        self.session_allow.lock().unwrap().push(Rule {
            tool: tool_name.to_string(),
            matcher,
        });
    }

    pub fn evaluate(&self, mode: Mode, tool: &dyn Tool, args: &serde_json::Value) -> Decision {
        let name = tool.name();

        // Explicit deny always wins, in every mode.
        if let Some(rule) = self.deny.iter().find(|r| r.matches(name, args)) {
            return Decision::Deny {
                reason: format!(
                    "blocked by deny rule {}({})",
                    rule.tool,
                    rule.matcher.as_deref().unwrap_or("*")
                ),
            };
        }

        match (mode, tool.kind()) {
            // Plan mode: read-only world. Denials are worded for the model.
            (Mode::Plan, ToolKind::ReadOnly) => Decision::Allow,
            (Mode::Plan, _) => Decision::Deny {
                reason: "plan mode is read-only; describe this step in your plan instead of executing it"
                    .into(),
            },

            (_, ToolKind::ReadOnly) => Decision::Allow,
            (Mode::Auto, ToolKind::Edit) => Decision::Allow,

            // Everything else: allowed if a config or session rule covers it.
            _ => {
                let allowed = self.allow.iter().any(|r| r.matches(name, args))
                    || self.session_allow.lock().unwrap().iter().any(|r| r.matches(name, args));
                if allowed { Decision::Allow } else { Decision::Ask }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::{BashTool, EditTool, ReadTool};
    use serde_json::json;

    fn engine() -> PermissionEngine {
        PermissionEngine::from_config(&PermissionsConfig {
            allow: vec!["Bash(cargo test:*)".into()],
            deny: vec!["Bash(rm -rf:*)".into(), "Read(**/*.pem)".into()],
        })
    }

    #[test]
    fn deny_beats_everything_even_in_auto() {
        let e = engine();
        let d = e.evaluate(Mode::Auto, &BashTool, &json!({"command": "rm -rf /tmp/x"}));
        assert!(matches!(d, Decision::Deny { .. }));
        let d = e.evaluate(Mode::Auto, &ReadTool, &json!({"path": "certs/key.pem"}));
        assert!(matches!(d, Decision::Deny { .. }));
    }

    #[test]
    fn plan_mode_is_read_only() {
        let e = engine();
        assert_eq!(
            e.evaluate(Mode::Plan, &ReadTool, &json!({"path": "x"})),
            Decision::Allow
        );
        assert!(matches!(
            e.evaluate(Mode::Plan, &EditTool, &json!({"path": "x"})),
            Decision::Deny { .. }
        ));
    }

    #[test]
    fn auto_allows_edits_but_asks_for_commands() {
        let e = engine();
        assert_eq!(
            e.evaluate(Mode::Auto, &EditTool, &json!({"path": "x"})),
            Decision::Allow
        );
        assert_eq!(
            e.evaluate(Mode::Auto, &BashTool, &json!({"command": "cargo build"})),
            Decision::Ask
        );
        // ...unless an allow rule covers the command.
        assert_eq!(
            e.evaluate(
                Mode::Auto,
                &BashTool,
                &json!({"command": "cargo test --all"})
            ),
            Decision::Allow
        );
    }

    #[test]
    fn normal_asks_for_edits() {
        let e = engine();
        assert_eq!(
            e.evaluate(Mode::Normal, &EditTool, &json!({"path": "x"})),
            Decision::Ask
        );
    }

    #[test]
    fn remembered_answers_stick() {
        let e = engine();
        let args = json!({"command": "cargo build"});
        assert_eq!(e.evaluate(Mode::Normal, &BashTool, &args), Decision::Ask);
        e.remember_allow("bash", &args);
        assert_eq!(e.evaluate(Mode::Normal, &BashTool, &args), Decision::Allow);
    }
}
