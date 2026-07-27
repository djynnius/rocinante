//! SKILL.md-compatible skills with three-tier progressive disclosure:
//! tier 1 = name+description index injected into the system prompt;
//! tier 2 = full SKILL.md body, loaded via the `skill` tool on activation;
//! tier 3 = sibling files the skill references, read with ordinary tools.
//!
//! Discovery (lowest → highest precedence, same name shadows):
//! `~/.claude/plugins` (deep walk), `~/.claude/skills`,
//! `<project>/.claude/skills`, `~/.rocinante/skills`,
//! `<project>/.rocinante/skills`, then `[skills].extra_dirs`.
//! The `skill` tool rescans on an unknown name, so skills installed
//! mid-session activate without a restart.

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
    /// Embedded body for built-in skills; `None` means read from `dir`.
    pub body: Option<String>,
}

impl Skill {
    /// Built-in skills are the only ones with an embedded body; discovered
    /// skills always read theirs from `dir`.
    pub fn is_builtin(&self) -> bool {
        self.body.is_some()
    }
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

/// How deep into `~/.claude/plugins` to look for SKILL.md dirs — plugin
/// caches nest skills at unpredictable depths.
const PLUGIN_SCAN_DEPTH: usize = 8;

pub fn discover(config: &Config, project_dir: &Path) -> Vec<Skill> {
    let home = dirs::home_dir();
    let mut skills: Vec<Skill> = Vec::new();

    // Lowest priority first; later tiers shadow earlier ones by name.
    // Claude Code compatibility tiers come before the native ones, and
    // explicit `[skills] extra_dirs` config wins over everything on disk.
    if let Some(home) = &home {
        for dir in scan_deep(&home.join(".claude/plugins"), PLUGIN_SCAN_DEPTH) {
            add_skill(&mut skills, dir);
        }
        scan_flat(&mut skills, &home.join(".claude/skills"));
    }
    scan_flat(&mut skills, &project_dir.join(".claude/skills"));
    if let Some(home) = &home {
        scan_flat(&mut skills, &home.join(".rocinante/skills"));
    }
    scan_flat(&mut skills, &project_dir.join(".rocinante/skills"));
    for extra in &config.skills.extra_dirs {
        let path = if let Some(rest) = extra.strip_prefix("~/") {
            match &home {
                Some(home) => home.join(rest),
                None => continue,
            }
        } else {
            PathBuf::from(extra)
        };
        scan_flat(&mut skills, &path);
    }
    // Built-in skills fill any name not already provided by a user skill
    // (user skills shadow built-ins).
    if config.defaults.builtin_skills {
        for skill in builtin_skills() {
            if !skills.iter().any(|s| s.name == skill.name) {
                skills.push(skill);
            }
        }
    }
    skills.sort_by(|a, b| a.name.cmp(&b.name));
    skills
}

/// Scan one directory whose immediate children are skill folders.
fn scan_flat(skills: &mut Vec<Skill>, dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        add_skill(skills, entry.path());
    }
}

/// Directories under `root`, at most `depth` levels down, that directly
/// contain a SKILL.md. Skips `node_modules` and hidden dirs other than
/// `.claude`; sorted traversal keeps shadowing deterministic.
fn scan_deep(root: &Path, depth: usize) -> Vec<PathBuf> {
    let mut out = Vec::new();
    fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
        if dir.join("SKILL.md").is_file() {
            out.push(dir.to_path_buf());
            return; // a skill dir doesn't nest further skills
        }
        if depth == 0 {
            return;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let mut subdirs: Vec<PathBuf> = entries
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .map(|e| e.path())
            .filter(|p| {
                let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                name != "node_modules" && (!name.starts_with('.') || name == ".claude")
            })
            .collect();
        subdirs.sort();
        for sub in subdirs {
            walk(&sub, depth - 1, out);
        }
    }
    walk(root, depth, &mut out);
    out
}

/// Parse `skill_dir/SKILL.md` into the list, shadowing any earlier skill of
/// the same name. Dirs without a SKILL.md are silently skipped.
fn add_skill(skills: &mut Vec<Skill>, skill_dir: PathBuf) {
    let manifest = skill_dir.join("SKILL.md");
    let Ok(content) = std::fs::read_to_string(&manifest) else {
        return;
    };
    match parse_frontmatter(&content) {
        Some(fm) => {
            tracing::debug!(name = %fm.name, license = ?fm.license, "discovered skill");
            skills.retain(|s| s.name != fm.name);
            skills.push(Skill {
                name: fm.name,
                description: fm.description,
                allowed_tools: fm.allowed_tools,
                model: fm.model,
                dir: skill_dir,
                body: None,
            });
        }
        None => {
            tracing::warn!(path = %manifest.display(), "SKILL.md missing name/description frontmatter");
        }
    }
}

/// Skills embedded in the binary. Each SKILL.md is compiled in; a user
/// skill of the same name shadows it.
pub fn builtin_skills() -> Vec<Skill> {
    const EMBEDDED: &[&str] = &[
        include_str!("builtin/deep-research.md"),
        include_str!("builtin/code-review.md"),
        include_str!("builtin/debugging.md"),
        include_str!("builtin/writing-tests.md"),
        include_str!("builtin/proof-reading.md"),
        include_str!("builtin/plagiarism-check.md"),
        include_str!("builtin/peer-review.md"),
        include_str!("builtin/skill-maker.md"),
        include_str!("builtin/exploratory-data-analysis.md"),
        include_str!("builtin/statistical-modeling.md"),
        include_str!("builtin/sql-analytics.md"),
        include_str!("builtin/data-wrangling.md"),
        include_str!("builtin/git-rescue.md"),
        include_str!("builtin/duckdb.md"),
        include_str!("builtin/ggplot.md"),
        include_str!("builtin/sqlalchemy.md"),
        include_str!("builtin/flask.md"),
        include_str!("builtin/vuejs.md"),
        include_str!("builtin/d3js.md"),
        include_str!("builtin/mermaidjs.md"),
        include_str!("builtin/lxc.md"),
        include_str!("builtin/ollama.md"),
        include_str!("builtin/postgresql.md"),
        include_str!("builtin/frontend-design.md"),
        include_str!("builtin/ml-preprocessing.md"),
        include_str!("builtin/ml-modeling.md"),
        include_str!("builtin/ml-evaluation.md"),
        include_str!("builtin/web-research.md"),
        include_str!("builtin/medallion-architecture.md"),
        include_str!("builtin/ducklake.md"),
        include_str!("builtin/quarto.md"),
    ];
    EMBEDDED
        .iter()
        .filter_map(|content| {
            let fm = parse_frontmatter(content)?;
            let body = split_frontmatter(content)
                .map(|(_, b)| b.trim_start_matches("---").trim().to_string())
                .unwrap_or_default();
            Some(Skill {
                name: fm.name,
                description: fm.description,
                allowed_tools: fm.allowed_tools,
                model: fm.model,
                dir: PathBuf::new(),
                body: Some(body),
            })
        })
        .collect()
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
    skills: std::sync::RwLock<Arc<Vec<Skill>>>,
    /// Discovery inputs for the on-miss rescan; `None` = fixed catalog.
    rescan: Option<(Arc<Config>, PathBuf)>,
}

impl SkillTool {
    pub fn new(skills: Arc<Vec<Skill>>) -> Self {
        Self {
            skills: std::sync::RwLock::new(skills),
            rescan: None,
        }
    }

    /// A tool that re-runs discovery when asked for a name it doesn't know,
    /// so skills installed or created mid-session activate without a restart.
    pub fn with_rescan(skills: Arc<Vec<Skill>>, config: Arc<Config>, project_dir: PathBuf) -> Self {
        Self {
            skills: std::sync::RwLock::new(skills),
            rescan: Some((config, project_dir)),
        }
    }

    fn snapshot(&self) -> Arc<Vec<Skill>> {
        Arc::clone(&self.skills.read().expect("skills lock"))
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
        let catalog = self.snapshot();
        let names: Vec<&String> = catalog.iter().map(|s| &s.name).collect();
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
        let mut catalog = self.snapshot();
        let mut found = catalog.iter().find(|s| s.name == name).cloned();
        // Unknown name: rescan the skill dirs so a skill installed or
        // created mid-session activates without a restart.
        if found.is_none()
            && let Some((config, project_dir)) = &self.rescan
        {
            let config = Arc::clone(config);
            let project_dir = project_dir.clone();
            if let Ok(fresh) =
                tokio::task::spawn_blocking(move || discover(&config, &project_dir)).await
            {
                let fresh = Arc::new(fresh);
                *self.skills.write().expect("skills lock") = Arc::clone(&fresh);
                catalog = fresh;
                found = catalog.iter().find(|s| s.name == name).cloned();
            }
        }
        let Some(skill) = found else {
            return ToolOutput::error(format!(
                "unknown skill `{name}`. Available: {}",
                catalog
                    .iter()
                    .map(|s| s.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        };
        let skill = &skill;
        // Built-in skills carry their body inline; filesystem skills read it.
        let loaded: Result<String, String> = match &skill.body {
            Some(body) => Ok(body.clone()),
            None => tokio::fs::read_to_string(skill.dir.join("SKILL.md"))
                .await
                .map(|content| {
                    split_frontmatter(&content)
                        .map(|(_, body)| body.trim_start_matches("---").trim().to_string())
                        .unwrap_or(content)
                })
                .map_err(|e| format!("cannot read skill: {e}")),
        };
        match loaded {
            Ok(body) => {
                let where_files = if skill.body.is_some() {
                    "(built-in skill)".to_string()
                } else {
                    format!("Files it mentions live in {}", skill.dir.display())
                };
                let mut banner = format!(
                    "[Skill `{}` loaded — follow these instructions. {where_files}]",
                    skill.name,
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
            Err(e) => ToolOutput::error(e),
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
        assert!(skills.iter().any(|s| s.name == "migrations"));
        // Built-ins merge in alongside the project skill.
        assert!(skills.iter().any(|s| s.name == "deep-research"));
        let pre = preamble(&skills);
        assert!(pre.contains("migrations: How we write DB migrations"));
    }

    #[test]
    fn all_builtin_skills_parse() {
        let skills = builtin_skills();
        assert_eq!(skills.len(), 31, "expected 31 embedded skills");
        for s in &skills {
            assert!(!s.name.is_empty());
            assert!(!s.description.is_empty());
            assert!(s.body.as_ref().is_some_and(|b| !b.trim().is_empty()));
        }
        assert!(skills.iter().any(|s| s.name == "deep-research"));
        assert!(skills.iter().any(|s| s.name == "peer-review"));
        assert!(skills.iter().any(|s| s.name == "skill-maker"));
        for name in [
            "exploratory-data-analysis",
            "statistical-modeling",
            "sql-analytics",
            "data-wrangling",
            "git-rescue",
            "duckdb",
            "ggplot",
            "sqlalchemy",
            "flask",
            "vuejs",
            "d3js",
            "mermaidjs",
            "lxc",
            "ollama",
            "postgresql",
            "frontend-design",
            "ml-preprocessing",
            "ml-modeling",
            "ml-evaluation",
            "web-research",
            "medallion-architecture",
            "ducklake",
            "quarto",
        ] {
            assert!(skills.iter().any(|s| s.name == name), "missing {name}");
        }
    }

    /// The skills aimed at weak local models must keep the checklist shape:
    /// a Rules section and at least one copy-paste code block.
    #[test]
    fn hardened_skills_keep_checklist_structure() {
        let skills = builtin_skills();
        for name in [
            "exploratory-data-analysis",
            "statistical-modeling",
            "sql-analytics",
            "data-wrangling",
            "skill-maker",
            "git-rescue",
            "duckdb",
            "ggplot",
            "sqlalchemy",
            "flask",
            "vuejs",
            "d3js",
            "mermaidjs",
            "lxc",
            "ollama",
            "postgresql",
            "ml-preprocessing",
            "ml-modeling",
            "ml-evaluation",
            "web-research",
            "medallion-architecture",
            "ducklake",
            "quarto",
            // frontend-design is third-party (Apache-2.0) and keeps its own format
        ] {
            let body = skills
                .iter()
                .find(|s| s.name == name)
                .unwrap_or_else(|| panic!("missing {name}"))
                .body
                .as_ref()
                .unwrap();
            assert!(body.contains("## Rules"), "{name}: no Rules section");
            assert!(body.contains("```"), "{name}: no code block");
        }
    }

    #[test]
    fn builtin_skills_disabled_by_flag() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("p.toml");
        std::fs::write(&project, "[defaults]\nbuiltin_skills = false\n").unwrap();
        let config = crate::config::load_from(Path::new("/nonexistent/x.toml"), &project).unwrap();
        let skills = discover(&config, dir.path());
        assert!(!skills.iter().any(|s| s.name == "deep-research"));
    }

    #[test]
    fn user_skill_shadows_builtin() {
        let dir = tempfile::tempdir().unwrap();
        write_skill(
            &dir.path().join(".rocinante/skills"),
            "deep-research",
            "my override",
        );
        let config = crate::config::load_from(
            Path::new("/nonexistent/x.toml"),
            Path::new("/nonexistent/x.toml"),
        )
        .unwrap();
        let skills = discover(&config, dir.path());
        let dr: Vec<_> = skills
            .iter()
            .filter(|s| s.name == "deep-research")
            .collect();
        assert_eq!(dr.len(), 1, "no duplicate deep-research");
        assert_eq!(dr[0].description, "my override");
        assert!(
            dr[0].body.is_none(),
            "user skill reads from disk, not embedded"
        );
    }

    #[test]
    fn scan_deep_finds_nested_skills_and_respects_bounds() {
        let dir = tempfile::tempdir().unwrap();
        // Nested like a plugin cache: cache/<market>/<plugin>/skills/<name>.
        write_skill(
            &dir.path().join("cache/market/plugin/skills"),
            "nested",
            "d",
        );
        // Inside a .claude dir — must still be found.
        write_skill(&dir.path().join("repo/.claude/skills"), "dotclaude", "d");
        // Skipped subtrees.
        write_skill(&dir.path().join("repo/node_modules/pkg"), "nm", "d");
        write_skill(&dir.path().join("repo/.git/hooks"), "githook", "d");
        // Beyond the depth cap.
        write_skill(&dir.path().join("a/b/c/d/e/f/g/h/i"), "toodeep", "d");

        let found = scan_deep(dir.path(), PLUGIN_SCAN_DEPTH);
        let names: Vec<String> = found
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert!(names.contains(&"nested".to_string()), "got: {names:?}");
        assert!(names.contains(&"dotclaude".to_string()), "got: {names:?}");
        assert!(!names.contains(&"nm".to_string()), "node_modules scanned");
        assert!(!names.contains(&"githook".to_string()), ".git scanned");
        assert!(!names.contains(&"toodeep".to_string()), "depth cap ignored");
    }

    #[test]
    fn discovers_project_claude_skills_with_rocinante_precedence() {
        let dir = tempfile::tempdir().unwrap();
        write_skill(
            &dir.path().join(".claude/skills"),
            "compat",
            "from claude dir",
        );
        write_skill(
            &dir.path().join(".claude/skills"),
            "shared",
            "claude version",
        );
        write_skill(
            &dir.path().join(".rocinante/skills"),
            "shared",
            "rocinante version",
        );
        let config = crate::config::load_from(
            Path::new("/nonexistent/x.toml"),
            Path::new("/nonexistent/x.toml"),
        )
        .unwrap();
        let skills = discover(&config, dir.path());
        assert!(skills.iter().any(|s| s.name == "compat"));
        let shared: Vec<_> = skills.iter().filter(|s| s.name == "shared").collect();
        assert_eq!(shared.len(), 1);
        assert_eq!(shared[0].description, "rocinante version");
    }

    #[tokio::test]
    async fn skill_tool_rescans_on_unknown_name() {
        let dir = tempfile::tempdir().unwrap();
        let config = Arc::new(
            crate::config::load_from(
                Path::new("/nonexistent/x.toml"),
                Path::new("/nonexistent/x.toml"),
            )
            .unwrap(),
        );
        // Tool built before the skill exists on disk.
        let tool = SkillTool::with_rescan(Arc::new(vec![]), config, dir.path().to_path_buf());
        write_skill(
            &dir.path().join(".rocinante/skills"),
            "fresh",
            "Just installed",
        );
        let ctx = test_ctx(dir.path());
        let out = tool.run(json!({"name": "fresh"}), &ctx).await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("Do the thing carefully"));
        // Still-unknown names error against the refreshed catalog.
        let out = tool.run(json!({"name": "nope"}), &ctx).await;
        assert!(out.is_error);
        assert!(out.content.contains("fresh"), "{}", out.content);
    }

    #[tokio::test]
    async fn skill_tool_without_rescan_stays_fixed() {
        let dir = tempfile::tempdir().unwrap();
        let tool = SkillTool::new(Arc::new(vec![]));
        write_skill(&dir.path().join(".rocinante/skills"), "fresh", "d");
        let ctx = test_ctx(dir.path());
        let out = tool.run(json!({"name": "fresh"}), &ctx).await;
        assert!(out.is_error, "fixed catalog must not rescan");
    }

    #[tokio::test]
    async fn skill_tool_loads_embedded_body() {
        let skills = Arc::new(builtin_skills());
        let tool = SkillTool::new(skills);
        let dir = tempfile::tempdir().unwrap();
        let ctx = test_ctx(dir.path());
        let out = tool.run(json!({"name": "deep-research"}), &ctx).await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("built-in skill"));
        assert!(out.content.to_lowercase().contains("research"));
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
            body: None,
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
            body: None,
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
