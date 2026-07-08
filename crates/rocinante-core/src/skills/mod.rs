//! SKILL.md-compatible skills with three-tier progressive disclosure:
//! tier 1 = name+description index injected into the system prompt;
//! tier 2 = full SKILL.md body, loaded via the `skill` tool on activation;
//! tier 3 = sibling files the skill references, read with ordinary tools.
//!
//! Discovery: `~/.rocinante/skills/*/SKILL.md`, `<project>/.rocinante/skills/`,
//! plus `[skills].extra_dirs` (e.g. `~/.claude/skills` for compatibility).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

use crate::config::Config;
use crate::tools::{Tool, ToolCtx, ToolKind, ToolOutput};

#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub description: String,
    /// Advisory tool allowlist from `allowed-tools` frontmatter (v1: suggested, not enforced).
    pub allowed_tools: Option<Vec<String>>,
    /// Advisory model suggestion from `model` frontmatter.
    pub model: Option<String>,
    /// Directory containing SKILL.md (for tier-3 file references).
    pub dir: PathBuf,
}

/// Agent Skills spec frontmatter. Unknown fields are ignored for forward
/// compatibility (the spec allows arbitrary extra metadata).
#[derive(Debug, Deserialize)]
struct SkillFrontmatter {
    name: String,
    description: String,
    #[serde(rename = "allowed-tools")]
    allowed_tools: Option<Vec<String>>,
    model: Option<String>,
    license: Option<String>,
}

/// Split a SKILL.md into (frontmatter, body) at the `---` fences.
/// Tolerates `\r\n` line endings.
fn split_frontmatter(content: &str) -> Option<(&str, &str)> {
    let rest = content.strip_prefix("---")?;
    rest.split_once("\n---")
}

/// Parse the YAML frontmatter of a SKILL.md via serde_yaml_ng. Returns None
/// (caller skips the skill) when the fences are absent, the YAML is malformed,
/// or `name`/`description` are missing. Spec limits are warned, not rejected.
fn parse_frontmatter(content: &str) -> Option<SkillFrontmatter> {
    let (frontmatter, _) = split_frontmatter(content)?;
    let yaml = frontmatter.replace("\r\n", "\n");
    let fm: SkillFrontmatter = match serde_yaml_ng::from_str(&yaml) {
        Ok(fm) => fm,
        Err(e) => {
            tracing::warn!(error = %e, "invalid SKILL.md frontmatter YAML");
            return None;
        }
    };
    if fm.name.len() > 64
        || !fm
            .name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        tracing::warn!(
            name = %fm.name,
            "skill name should be lowercase-hyphenated (a-z, 0-9, -) and at most 64 chars"
        );
    }
    if fm.description.len() > 1024 {
        tracing::warn!(
            name = %fm.name,
            len = fm.description.len(),
            "skill description exceeds the spec limit of 1024 chars"
        );
    }
    Some(fm)
}

pub fn discover(config: &Config, project_dir: &Path) -> Vec<Skill> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(home) = dirs::home_dir() {
        dirs.push(home.join(".rocinante/skills"));
    }
    dirs.push(project_dir.join(".rocinante/skills"));
    for extra in &config.skills.extra_dirs {
        let path = if let Some(rest) = extra.strip_prefix("~/") {
            match dirs::home_dir() {
                Some(home) => home.join(rest),
                None => continue,
            }
        } else {
            PathBuf::from(extra)
        };
        dirs.push(path);
    }

    let mut skills: Vec<Skill> = Vec::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let skill_dir = entry.path();
            let manifest = skill_dir.join("SKILL.md");
            let Ok(content) = std::fs::read_to_string(&manifest) else {
                continue;
            };
            match parse_frontmatter(&content) {
                Some(fm) => {
                    tracing::debug!(name = %fm.name, license = ?fm.license, "discovered skill");
                    // Later dirs (project, extra) shadow earlier ones by name.
                    skills.retain(|s| s.name != fm.name);
                    skills.push(Skill {
                        name: fm.name,
                        description: fm.description,
                        allowed_tools: fm.allowed_tools,
                        model: fm.model,
                        dir: skill_dir,
                    });
                }
                None => {
                    tracing::warn!(path = %manifest.display(), "SKILL.md missing name/description frontmatter");
                }
            }
        }
    }
    skills.sort_by(|a, b| a.name.cmp(&b.name));
    skills
}

/// Tier-1 index for the system prompt. Empty string when no skills exist.
pub fn preamble(skills: &[Skill]) -> String {
    if skills.is_empty() {
        return String::new();
    }
    let mut out = String::from(
        "\n\nSkills (load one with the `skill` tool when its description matches the task):\n",
    );
    for s in skills {
        out.push_str(&format!("- {}: {}\n", s.name, s.description));
    }
    out
}

/// The `skill` tool: loads a skill's full instructions into context.
pub struct SkillTool {
    skills: Arc<Vec<Skill>>,
}

impl SkillTool {
    pub fn new(skills: Arc<Vec<Skill>>) -> Self {
        Self { skills }
    }
}

#[async_trait]
impl Tool for SkillTool {
    fn name(&self) -> &'static str {
        "skill"
    }
    fn description(&self) -> &'static str {
        "Load a skill's full instructions. Use when a listed skill matches the current task."
    }
    fn schema(&self) -> serde_json::Value {
        let names: Vec<&String> = self.skills.iter().map(|s| &s.name).collect();
        json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "enum": names, "description": "Skill to load" }
            },
            "required": ["name"]
        })
    }
    fn kind(&self) -> ToolKind {
        ToolKind::ReadOnly
    }
    fn describe_call(&self, args: &serde_json::Value) -> String {
        format!(
            "skill: {}",
            args.get("name").and_then(|v| v.as_str()).unwrap_or("?")
        )
    }

    async fn run(&self, args: serde_json::Value, _ctx: &ToolCtx) -> ToolOutput {
        let Some(name) = args.get("name").and_then(|v| v.as_str()) else {
            return ToolOutput::error("missing `name`");
        };
        let Some(skill) = self.skills.iter().find(|s| s.name == name) else {
            return ToolOutput::error(format!(
                "unknown skill `{name}`. Available: {}",
                self.skills
                    .iter()
                    .map(|s| s.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        };
        match tokio::fs::read_to_string(skill.dir.join("SKILL.md")).await {
            Ok(content) => {
                let body = split_frontmatter(&content)
                    .map(|(_, body)| body.trim_start_matches("---").trim())
                    .unwrap_or(&content);
                let mut banner = format!(
                    "[Skill `{}` loaded — follow these instructions. Files it mentions live in {}]",
                    skill.name,
                    skill.dir.display(),
                );
                if let Some(tools) = &skill.allowed_tools {
                    banner.push_str(&format!(
                        "\n[This skill suggests using only these tools: {}]",
                        tools.join(", ")
                    ));
                }
                if let Some(model) = &skill.model {
                    banner.push_str(&format!("\n[This skill suggests model: {model}]"));
                }
                ToolOutput::ok(format!("{banner}\n\n{body}"))
            }
            Err(e) => ToolOutput::error(format!("cannot read skill: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_skill(root: &Path, name: &str, desc: &str) {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {desc}\n---\n\nDo the thing carefully.\n"),
        )
        .unwrap();
    }

    #[test]
    fn frontmatter_parses() {
        let fm = parse_frontmatter("---\nname: deploy\ndescription: \"How we deploy\"\n---\nbody")
            .unwrap();
        assert_eq!(fm.name, "deploy");
        assert_eq!(fm.description, "How we deploy");
        assert_eq!(fm.allowed_tools, None);
        assert_eq!(fm.model, None);
        assert_eq!(fm.license, None);
    }

    #[test]
    fn frontmatter_requires_both_fields() {
        assert!(parse_frontmatter("---\nname: x\n---\nbody").is_none());
        assert!(parse_frontmatter("---\ndescription: y\n---\nbody").is_none());
        assert!(parse_frontmatter("no frontmatter at all").is_none());
    }

    #[test]
    fn frontmatter_folded_multiline_description() {
        let fm = parse_frontmatter(
            "---\nname: deploy\ndescription: >\n  Ship the app\n  to production safely.\n---\nbody",
        )
        .unwrap();
        assert_eq!(fm.description.trim(), "Ship the app to production safely.");
    }

    #[test]
    fn frontmatter_literal_multiline_description() {
        let fm = parse_frontmatter(
            "---\nname: deploy\ndescription: |\n  Line one.\n  Line two.\n---\nbody",
        )
        .unwrap();
        assert_eq!(fm.description, "Line one.\nLine two.");
    }

    #[test]
    fn frontmatter_quoted_values() {
        let fm = parse_frontmatter(
            "---\nname: 'deploy'\ndescription: \"Deploys: with a colon\"\nmodel: \"claude-x\"\n---\nbody",
        )
        .unwrap();
        assert_eq!(fm.name, "deploy");
        assert_eq!(fm.description, "Deploys: with a colon");
        assert_eq!(fm.model.as_deref(), Some("claude-x"));
    }

    #[test]
    fn frontmatter_allowed_tools_block_list() {
        let fm = parse_frontmatter(
            "---\nname: deploy\ndescription: d\nallowed-tools:\n  - bash\n  - read_file\n---\nbody",
        )
        .unwrap();
        assert_eq!(
            fm.allowed_tools,
            Some(vec!["bash".to_string(), "read_file".to_string()])
        );
    }

    #[test]
    fn frontmatter_allowed_tools_flow_list() {
        let fm = parse_frontmatter(
            "---\nname: deploy\ndescription: d\nallowed-tools: [bash, read_file]\n---\nbody",
        )
        .unwrap();
        assert_eq!(
            fm.allowed_tools,
            Some(vec!["bash".to_string(), "read_file".to_string()])
        );
    }

    #[test]
    fn frontmatter_unknown_fields_ignored() {
        let fm = parse_frontmatter(
            "---\nname: deploy\ndescription: d\nlicense: MIT\nversion: 1.2.3\nmetadata:\n  author: someone\n---\nbody",
        )
        .unwrap();
        assert_eq!(fm.name, "deploy");
        assert_eq!(fm.license.as_deref(), Some("MIT"));
    }

    #[test]
    fn frontmatter_malformed_yaml_skipped() {
        assert!(parse_frontmatter("---\nname: [unclosed\ndescription: d\n---\nbody").is_none());
        assert!(parse_frontmatter("---\nname: {a: b}\ndescription: d\n---\nbody").is_none());
    }

    #[test]
    fn frontmatter_uppercase_name_still_parses() {
        // Spec-invalid names warn but are not rejected.
        let fm = parse_frontmatter("---\nname: My Skill\ndescription: d\n---\nbody").unwrap();
        assert_eq!(fm.name, "My Skill");
    }

    #[test]
    fn frontmatter_crlf_line_endings() {
        let fm =
            parse_frontmatter("---\r\nname: deploy\r\ndescription: How we deploy\r\n---\r\nbody")
                .unwrap();
        assert_eq!(fm.name, "deploy");
        assert_eq!(fm.description, "How we deploy");
    }

    #[test]
    fn discovers_project_skills_and_builds_preamble() {
        let dir = tempfile::tempdir().unwrap();
        write_skill(
            &dir.path().join(".rocinante/skills"),
            "migrations",
            "How we write DB migrations",
        );
        let config = crate::config::load_from(
            Path::new("/nonexistent/x.toml"),
            Path::new("/nonexistent/x.toml"),
        )
        .unwrap();
        let skills = discover(&config, dir.path());
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "migrations");
        let pre = preamble(&skills);
        assert!(pre.contains("migrations: How we write DB migrations"));
    }

    fn test_ctx(cwd: &Path) -> ToolCtx {
        ToolCtx {
            cwd: cwd.to_path_buf(),
            events: crate::agent::events::EventSender::new(tokio::sync::broadcast::channel(8).0),
            cancel: Default::default(),
            depth: 0,
            router: Default::default(),
            lsp: None,
        }
    }

    #[tokio::test]
    async fn skill_tool_loads_body() {
        let dir = tempfile::tempdir().unwrap();
        write_skill(dir.path(), "deploy", "Deploying");
        let skills = Arc::new(vec![Skill {
            name: "deploy".into(),
            description: "Deploying".into(),
            allowed_tools: None,
            model: None,
            dir: dir.path().join("deploy"),
        }]);
        let tool = SkillTool::new(skills);
        let ctx = test_ctx(dir.path());
        let out = tool.run(json!({"name": "deploy"}), &ctx).await;
        assert!(!out.is_error);
        assert!(out.content.contains("Do the thing carefully"));
        assert!(!out.content.contains("suggests using only these tools"));
        assert!(!out.content.contains("suggests model"));
    }

    #[tokio::test]
    async fn skill_tool_banner_includes_advisory_tools_and_model() {
        let dir = tempfile::tempdir().unwrap();
        write_skill(dir.path(), "deploy", "Deploying");
        let skills = Arc::new(vec![Skill {
            name: "deploy".into(),
            description: "Deploying".into(),
            allowed_tools: Some(vec!["bash".into(), "read_file".into()]),
            model: Some("claude-x".into()),
            dir: dir.path().join("deploy"),
        }]);
        let tool = SkillTool::new(skills);
        let ctx = test_ctx(dir.path());
        let out = tool.run(json!({"name": "deploy"}), &ctx).await;
        assert!(!out.is_error);
        assert!(
            out.content
                .contains("[This skill suggests using only these tools: bash, read_file]")
        );
        assert!(
            out.content
                .contains("[This skill suggests model: claude-x]")
        );
    }
}
