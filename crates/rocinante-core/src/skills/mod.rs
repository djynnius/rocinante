//! SKILL.md-compatible skills with three-tier progressive disclosure:
//! tier 1 = name+description index injected into the system prompt;
//! tier 2 = full SKILL.md body, loaded via the `skill` tool on activation;
//! tier 3 = sibling files the skill references, read with ordinary tools.
//! Built-ins have no folder of their own; the few that ship files (see
//! [`BundledFile`]) write them to `~/.rocinante/skill-files/<name>/` on load.
//!
//! Discovery (lowest → highest precedence, same name shadows):
//! `~/.claude/plugins` (deep walk), `~/.claude/skills`,
//! `<project>/.claude/skills`, `~/.rocinante/skills`,
//! `<project>/.rocinante/skills`, then `[skills].extra_dirs`; names listed in
//! `[skills].hide` are then dropped, and built-ins fill the remaining names.
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
    /// Files a built-in ships alongside its body; empty for everything else.
    pub files: &'static [BundledFile],
}

/// A file compiled into the binary for a built-in skill. The `skill` tool
/// writes it to disk when the skill loads, so the instructions can point the
/// model at a real path (e.g. a JS library copied next to generated HTML).
#[derive(Clone, Copy)]
pub struct BundledFile {
    pub name: &'static str,
    pub bytes: &'static [u8],
}

// Not derived: a derived Debug would print every byte of a ~800KB bundle.
impl std::fmt::Debug for BundledFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({} bytes)", self.name, self.bytes.len())
    }
}

/// Regenerate with `scripts/vendor-three.sh <version>`.
const THREEJS_FILES: &[BundledFile] = &[
    BundledFile {
        name: "three.min.js",
        bytes: include_bytes!("builtin/threejs/three.min.js"),
    },
    BundledFile {
        name: "LICENSE",
        bytes: include_bytes!("builtin/threejs/LICENSE"),
    },
];

/// Regenerate with `scripts/vendor-vue.sh <version>`.
const VUEJS_FILES: &[BundledFile] = &[
    BundledFile {
        name: "vue.global.js",
        bytes: include_bytes!("builtin/vuejs/vue.global.js"),
    },
    BundledFile {
        name: "vue.global.prod.js",
        bytes: include_bytes!("builtin/vuejs/vue.global.prod.js"),
    },
    BundledFile {
        name: "LICENSE",
        bytes: include_bytes!("builtin/vuejs/LICENSE"),
    },
];

fn builtin_files(skill: &str) -> &'static [BundledFile] {
    match skill {
        "threejs" => THREEJS_FILES,
        "vuejs" => VUEJS_FILES,
        _ => &[],
    }
}

/// Write `files` into `dir`, leaving any already byte-identical copy alone.
/// Each write goes through a uniquely named temp file + rename, so a reader —
/// or a concurrent load of the same skill — never sees a half-written file.
pub fn write_bundled_files(dir: &Path, files: &[BundledFile]) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    for file in files {
        let target = dir.join(file.name);
        if std::fs::read(&target).is_ok_and(|on_disk| on_disk == file.bytes) {
            continue;
        }
        let tmp = dir.join(format!(".{}.{}.tmp", file.name, uuid::Uuid::new_v4()));
        std::fs::write(&tmp, file.bytes)
            .and_then(|()| std::fs::rename(&tmp, &target))
            .inspect_err(|_| {
                let _ = std::fs::remove_file(&tmp);
            })?;
    }
    Ok(())
}

fn default_files_root() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".rocinante").join("skill-files"))
}

/// The path gets pasted into shell commands, where an unquoted backslash is
/// an escape; `C:/...` works in Git Bash and PowerShell alike.
fn shell_path(path: &Path) -> String {
    let s = path.display().to_string();
    if cfg!(windows) {
        s.replace('\\', "/")
    } else {
        s
    }
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
    // Before built-ins fill in: a hidden skill that shared a built-in's name
    // lets the built-in through instead of hiding both.
    skills.retain(|s| !config.skills.hide.contains(&s.name));
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
                files: &[],
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
        include_str!("builtin/spacy.md"),
        include_str!("builtin/nltk.md"),
        include_str!("builtin/recommender-systems.md"),
        include_str!("builtin/docx.md"),
        include_str!("builtin/xlsx.md"),
        include_str!("builtin/pptx.md"),
        include_str!("builtin/pdf.md"),
        include_str!("builtin/rocinante-config.md"),
        include_str!("builtin/github-cli.md"),
        include_str!("builtin/threejs.md"),
    ];
    EMBEDDED
        .iter()
        .filter_map(|content| {
            let fm = parse_frontmatter(content)?;
            let body = split_frontmatter(content)
                .map(|(_, b)| b.trim_start_matches("---").trim().to_string())
                .unwrap_or_default();
            Some(Skill {
                files: builtin_files(&fm.name),
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
        out.push_str(&index_line(s));
    }
    out
}

/// One index entry — shared by the prompt preamble and the `/context`
/// dashboard sizing so they can never drift apart.
pub fn index_line(s: &Skill) -> String {
    format!("- {}: {}\n", s.name, short_desc(&s.description))
}

/// Names-only index for subagents: full discoverability of what exists, at
/// a fraction of the per-request cost of the described index.
pub fn preamble_compact(skills: &[Skill]) -> String {
    if skills.is_empty() {
        return String::new();
    }
    let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
    format!(
        "\n\nSkills — load full instructions by exact name with the `skill` tool when one matches the task: {}\n",
        names.join(", ")
    )
}

/// A compact one-line trigger for the skills index: the first clause of the
/// description, hard-capped, so the standing preamble stays small. The full
/// description and body still load when the `skill` tool activates the skill.
pub fn short_desc(desc: &str) -> String {
    const CAP: usize = 72;
    let desc = desc.trim();
    // First clause: the earliest sentence/clause boundary (keep the period,
    // drop the other separators).
    let clause = [(". ", 1), (": ", 0), (" — ", 0)]
        .iter()
        .filter_map(|(sep, keep)| desc.find(sep).map(|i| i + keep))
        .min()
        .unwrap_or(desc.len());
    if clause <= CAP {
        // Clean cut at the clause boundary (or the whole short desc).
        let mut cut = clause;
        while !desc.is_char_boundary(cut) {
            cut -= 1;
        }
        return desc[..cut].to_string();
    }
    // Long with no early break: hard-cap with an ellipsis.
    let mut cut = CAP;
    while !desc.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}…", desc[..cut].trim_end())
}

/// The `skill` tool: loads a skill's full instructions into context.
pub struct SkillTool {
    skills: std::sync::RwLock<Arc<Vec<Skill>>>,
    /// Discovery inputs for the on-miss rescan; `None` = fixed catalog.
    rescan: Option<(Arc<Config>, PathBuf)>,
    /// Where built-ins' bundled files are written; `None` = no home dir.
    files_root: Option<PathBuf>,
}

impl SkillTool {
    pub fn new(skills: Arc<Vec<Skill>>) -> Self {
        Self {
            skills: std::sync::RwLock::new(skills),
            rescan: None,
            files_root: default_files_root(),
        }
    }

    /// A tool that re-runs discovery when asked for a name it doesn't know,
    /// so skills installed or created mid-session activate without a restart.
    pub fn with_rescan(skills: Arc<Vec<Skill>>, config: Arc<Config>, project_dir: PathBuf) -> Self {
        Self {
            skills: std::sync::RwLock::new(skills),
            rescan: Some((config, project_dir)),
            files_root: default_files_root(),
        }
    }

    /// Write built-ins' bundled files under `root` instead of
    /// `~/.rocinante/skill-files`.
    pub fn with_files_root(mut self, root: PathBuf) -> Self {
        self.files_root = Some(root);
        self
    }

    fn snapshot(&self) -> Arc<Vec<Skill>> {
        Arc::clone(&self.skills.read().expect("skills lock"))
    }

    /// Put a built-in's bundled files on disk and return their directory.
    /// Writes only under Rocinante's own data dir, never the project, which
    /// is why the tool can stay `ReadOnly` (no permission prompt).
    async fn write_files(&self, skill: &Skill) -> Result<PathBuf, String> {
        let root = self
            .files_root
            .clone()
            .ok_or("no home directory to write them to")?;
        let dir = root.join(&skill.name);
        let target = dir.clone();
        let files = skill.files;
        tokio::task::spawn_blocking(move || write_bundled_files(&target, files))
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| format!("{}: {e}", dir.display()))?;
        Ok(dir)
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
        // No name enum: the valid names already sit in the prompt's skills
        // index, and repeating all of them here costs schema tokens on every
        // request. Unknown names get a rescan + clear error from run().
        json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "Exact name of a skill from the skills list in your instructions" }
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
                let where_files = if skill.body.is_none() {
                    format!("Files it mentions live in {}", skill.dir.display())
                } else if skill.files.is_empty() {
                    "(built-in skill)".to_string()
                } else {
                    match self.write_files(skill).await {
                        Ok(dir) => format!(
                            "(built-in skill) Files it mentions live in {}",
                            shell_path(&dir)
                        ),
                        Err(e) => format!(
                            "(built-in skill) Its bundled files could not be written ({e}) — \
                             report this to the user."
                        ),
                    }
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

    #[test]
    fn short_desc_trims_to_first_clause_and_caps() {
        // First sentence kept, trailing sentences dropped.
        let d = short_desc("Do the thing well. Use when the user asks for X, Y, or Z at length.");
        assert_eq!(d, "Do the thing well.");
        // Colon and em-dash also end the clause (separator dropped).
        assert_eq!(
            short_desc("Debugging loop: hypothesize, probe, fix. More text."),
            "Debugging loop"
        );
        assert_eq!(
            short_desc("Run the disciplined loop — reproduce first, then bisect."),
            "Run the disciplined loop"
        );
        // No clause break → hard cap with ellipsis.
        let long = "a".repeat(300);
        let out = short_desc(&long);
        assert!(out.chars().count() <= 73, "len {}", out.chars().count());
        assert!(out.ends_with('…'));
        // Multi-byte input never panics at the cap.
        let accents = "é".repeat(200);
        assert!(short_desc(&accents).ends_with('…'));
        // Short description unchanged.
        assert_eq!(short_desc("Tiny."), "Tiny.");
    }

    #[test]
    fn preamble_compact_lists_names_only() {
        let mk = |name: &str| Skill {
            name: name.into(),
            description: "A very long description that must not appear in the compact index."
                .into(),
            allowed_tools: None,
            model: None,
            dir: PathBuf::new(),
            body: Some("body".into()),
            files: &[],
        };
        let skills = vec![mk("alpha"), mk("beta")];
        let pre = preamble_compact(&skills);
        assert!(pre.contains("alpha, beta"));
        assert!(pre.contains("`skill` tool"));
        assert!(!pre.contains("long description"));
        assert_eq!(preamble_compact(&[]), "");
    }

    #[test]
    fn skill_tool_schema_has_no_name_enum() {
        let tool = SkillTool::new(Arc::new(vec![]));
        let schema = tool.schema();
        assert!(schema["properties"]["name"]["enum"].is_null());
        assert_eq!(schema["properties"]["name"]["type"], "string");
    }

    #[test]
    fn preamble_uses_short_descriptions() {
        let skill = Skill {
            name: "demo".into(),
            description: "Run demos. Use when the user asks about anything demo related, \
                          including long trigger phrases that should not bloat the index."
                .into(),
            allowed_tools: None,
            model: None,
            dir: PathBuf::new(),
            body: Some("body".into()),
            files: &[],
        };
        let pre = preamble(std::slice::from_ref(&skill));
        assert!(pre.contains("- demo: Run demos."));
        assert!(
            !pre.contains("long trigger phrases"),
            "full desc must be trimmed"
        );
    }

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
        assert_eq!(skills.len(), 41, "expected 41 embedded skills");
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
            "spacy",
            "nltk",
            "recommender-systems",
            "docx",
            "xlsx",
            "pptx",
            "pdf",
            "rocinante-config",
            "github-cli",
            "threejs",
        ] {
            assert!(skills.iter().any(|s| s.name == name), "missing {name}");
        }
    }

    /// Each bundled library must be a classic script defining its global
    /// (ES modules don't load on a page opened from disk — the reason these
    /// are bundled), ship its LICENSE, and be the version its skill names.
    #[test]
    fn bundled_libraries_are_classic_scripts_matching_their_skill() {
        // (skill, file, global definition, version header in the file,
        //  how the skill body names that version)
        let cases = [
            ("threejs", "three.min.js", "var THREE=(", "three.js r", "r"),
            (
                "vuejs",
                "vue.global.js",
                "var Vue = (function",
                "vue v",
                "Vue ",
            ),
            (
                "vuejs",
                "vue.global.prod.js",
                "var Vue=function(",
                "vue v",
                "Vue ",
            ),
        ];
        let skills = builtin_skills();
        for (name, file, global, header, named_as) in cases {
            let skill = skills.iter().find(|s| s.name == name).unwrap();
            let bytes = skill
                .files
                .iter()
                .find(|f| f.name == file)
                .unwrap_or_else(|| panic!("{name} lacks {file}"))
                .bytes;
            let js = std::str::from_utf8(bytes).unwrap();
            assert!(js.contains(global), "{file}: no classic-script global");
            let at = js
                .find(header)
                .unwrap_or_else(|| panic!("{file}: no version header"))
                + header.len();
            let version: String = js[at..]
                .chars()
                .take_while(|c| c.is_ascii_digit() || *c == '.')
                .collect();
            let body = skill.body.as_ref().unwrap();
            assert!(
                body.contains(&format!("{named_as}{version}")),
                "{name}.md must name the bundled version {named_as}{version}"
            );
            assert!(skill.files.iter().any(|f| f.name == "LICENSE"));
        }
        let three = skills.iter().find(|s| s.name == "threejs").unwrap();
        let js = std::str::from_utf8(three.files[0].bytes).unwrap();
        for addon in ["OrbitControls", "CSS2DRenderer", "CSS2DObject"] {
            assert!(js.contains(addon), "three bundle lacks {addon}");
            assert!(three.body.as_ref().unwrap().contains(addon));
        }
        assert!(
            skills
                .iter()
                .all(|s| matches!(s.name.as_str(), "threejs" | "vuejs") || s.files.is_empty()),
            "an unexpected built-in ships files"
        );
    }

    #[test]
    fn write_bundled_files_creates_and_repairs() {
        static FILES: &[BundledFile] = &[BundledFile {
            name: "a.txt",
            bytes: b"alpha",
        }];
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("skill");
        write_bundled_files(&target, FILES).unwrap();
        assert_eq!(std::fs::read(target.join("a.txt")).unwrap(), b"alpha");
        // A stale copy (older Rocinante, hand-edited) is replaced.
        std::fs::write(target.join("a.txt"), b"stale").unwrap();
        write_bundled_files(&target, FILES).unwrap();
        assert_eq!(std::fs::read(target.join("a.txt")).unwrap(), b"alpha");
        write_bundled_files(&target, FILES).unwrap();
        let names: Vec<_> = std::fs::read_dir(&target)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["a.txt"], "temp files left behind");
    }

    #[test]
    fn bundled_file_debug_omits_bytes() {
        let f = BundledFile {
            name: "big.js",
            bytes: &[0u8; 4096],
        };
        assert_eq!(format!("{f:?}"), "big.js (4096 bytes)");
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
            "spacy",
            "nltk",
            "recommender-systems",
            "docx",
            "xlsx",
            "pptx",
            "pdf",
            "rocinante-config",
            "github-cli",
            "threejs",
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
    fn hidden_skills_are_skipped_but_builtins_survive() {
        let dir = tempfile::tempdir().unwrap();
        let skills_dir = dir.path().join(".rocinante/skills");
        write_skill(&skills_dir, "migrations", "How we write DB migrations");
        write_skill(&skills_dir, "deep-research", "my override");
        write_skill(&skills_dir, "kept", "Still visible");
        let user = dir.path().join("user.toml");
        std::fs::write(
            &user,
            "[skills]\nhide = [\"migrations\", \"deep-research\"]\n",
        )
        .unwrap();
        let config = crate::config::load_from(&user, Path::new("/nonexistent/x.toml")).unwrap();
        let skills = discover(&config, dir.path());
        assert!(!skills.iter().any(|s| s.name == "migrations"));
        assert!(skills.iter().any(|s| s.name == "kept"));
        // Hiding the override brings back the built-in it was shadowing.
        let dr: Vec<_> = skills
            .iter()
            .filter(|s| s.name == "deep-research")
            .collect();
        assert_eq!(dr.len(), 1);
        assert!(dr[0].is_builtin(), "built-in should replace the hidden one");
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

    #[tokio::test]
    async fn skill_tool_writes_bundled_files_and_names_their_dir() {
        let root = tempfile::tempdir().unwrap();
        let tool =
            SkillTool::new(Arc::new(builtin_skills())).with_files_root(root.path().to_path_buf());
        let ctx = test_ctx(root.path());
        let out = tool.run(json!({"name": "threejs"}), &ctx).await;
        assert!(!out.is_error, "{}", out.content);
        let dir = root.path().join("threejs");
        let banner = format!("Files it mentions live in {}", shell_path(&dir));
        assert!(out.content.contains(&banner), "{}", out.content);
        for file in THREEJS_FILES {
            assert_eq!(std::fs::read(dir.join(file.name)).unwrap(), file.bytes);
        }
        // A built-in without files writes nothing and names no directory.
        let out = tool.run(json!({"name": "d3js"}), &ctx).await;
        assert!(!out.is_error);
        assert!(!out.content.contains("Files it mentions"));
        assert!(!root.path().join("d3js").exists());
    }

    #[tokio::test]
    async fn skill_tool_reports_unwritable_files_dir() {
        let root = tempfile::tempdir().unwrap();
        // A plain file where the directory should go makes create_dir_all fail.
        let blocker = root.path().join("not-a-dir");
        std::fs::write(&blocker, b"x").unwrap();
        let tool = SkillTool::new(Arc::new(builtin_skills())).with_files_root(blocker);
        let out = tool
            .run(json!({"name": "threejs"}), &test_ctx(root.path()))
            .await;
        assert!(!out.is_error, "the instructions still load");
        assert!(
            out.content.contains("could not be written"),
            "{}",
            out.content
        );
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
            files: &[],
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
            files: &[],
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
