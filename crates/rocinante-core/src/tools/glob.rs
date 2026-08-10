use std::path::Path;

use async_trait::async_trait;
use ignore::WalkBuilder;
use serde::Deserialize;
use serde_json::json;

use super::traits::{Tool, ToolCtx, ToolKind, ToolOutput, truncate_output};

/// List project files and directories under `root`, gitignore-aware, as
/// relative paths (directories end with `/`), sorted, capped at `cap`. Used to
/// seed the TUI `@`-file autocomplete. Best-effort: returns what it can.
pub fn list_project_paths(root: &Path, cap: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    // require_git(false): honor .gitignore even when root isn't a git repo.
    for entry in WalkBuilder::new(root)
        .hidden(true)
        .require_git(false)
        .build()
        .flatten()
    {
        let Ok(rel) = entry.path().strip_prefix(root) else {
            continue;
        };
        if rel.as_os_str().is_empty() {
            continue; // the root itself
        }
        let is_dir = entry.file_type().is_some_and(|t| t.is_dir());
        let mut s = rel.to_string_lossy().into_owned();
        if is_dir {
            s.push('/');
        }
        out.push(s);
        if out.len() >= cap {
            break;
        }
    }
    out.sort();
    out
}

pub struct GlobTool;

#[derive(Deserialize)]
struct Args {
    pattern: String,
    #[serde(default)]
    path: Option<String>,
}

const MAX_RESULTS: usize = 500;

#[async_trait]
impl Tool for GlobTool {
    fn name(&self) -> &'static str {
        "glob"
    }
    fn description(&self) -> &'static str {
        "Find files by name pattern, e.g. **/*.rs. Respects .gitignore. Newest first."
    }
    fn schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Glob pattern, e.g. **/*.toml" },
                "path": { "type": "string", "description": "Directory to search (default cwd)" }
            },
            "required": ["pattern"]
        })
    }
    fn kind(&self) -> ToolKind {
        ToolKind::ReadOnly
    }
    fn describe_call(&self, args: &serde_json::Value) -> String {
        format!(
            "glob: {}",
            args.get("pattern").and_then(|v| v.as_str()).unwrap_or("?")
        )
    }

    async fn run(&self, args: serde_json::Value, ctx: &ToolCtx) -> ToolOutput {
        let args: Args = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return ToolOutput::error(format!("bad arguments: {e}")),
        };
        let root = ctx.resolve(args.path.as_deref().unwrap_or("."));
        let cwd = ctx.cwd.clone();

        let result = tokio::task::spawn_blocking(move || {
            let glob = globset::GlobBuilder::new(&args.pattern)
                .literal_separator(false)
                .build()
                .map_err(|e| format!("bad glob `{}`: {e}", args.pattern))?
                .compile_matcher();

            let mut files: Vec<(std::path::PathBuf, std::time::SystemTime)> = Vec::new();
            for entry in WalkBuilder::new(&root).hidden(true).build().flatten() {
                if !entry.file_type().is_some_and(|t| t.is_file()) {
                    continue;
                }
                let rel = entry.path().strip_prefix(&root).unwrap_or(entry.path());
                if glob.is_match(rel) || glob.is_match(entry.file_name().to_string_lossy().as_ref())
                {
                    let mtime = entry
                        .metadata()
                        .ok()
                        .and_then(|m| m.modified().ok())
                        .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                    files.push((entry.path().to_path_buf(), mtime));
                }
            }
            files.sort_by_key(|(_, mtime)| std::cmp::Reverse(*mtime));
            files.truncate(MAX_RESULTS);
            let listing: Vec<String> = files
                .iter()
                .map(|(p, _)| p.strip_prefix(&cwd).unwrap_or(p).display().to_string())
                .collect();
            Ok::<_, String>(listing)
        })
        .await;

        match result {
            Ok(Ok(files)) if files.is_empty() => ToolOutput::ok("no files match"),
            Ok(Ok(files)) => {
                ToolOutput::ok(truncate_output(&files.join("\n"), MAX_RESULTS + 5, 50_000))
            }
            Ok(Err(e)) => ToolOutput::error(e),
            Err(e) => ToolOutput::error(format!("glob task failed: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn list_project_paths_lists_files_dirs_respects_gitignore_and_caps() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join(".gitignore"), "ignored/\nsecret.txt\n").unwrap();
        fs::create_dir(root.join("src")).unwrap();
        fs::write(root.join("src/main.rs"), "").unwrap();
        fs::write(root.join("secret.txt"), "").unwrap();
        fs::create_dir(root.join("ignored")).unwrap();
        fs::write(root.join("ignored/x.rs"), "").unwrap();

        let paths = list_project_paths(root, 10_000);
        assert!(
            paths.iter().any(|p| p == "src/"),
            "dirs included with slash"
        );
        assert!(paths.iter().any(|p| p == "src/main.rs"), "files included");
        assert!(
            !paths.iter().any(|p| p.starts_with("ignored")),
            "gitignored dir excluded"
        );
        assert!(
            !paths.iter().any(|p| p == "secret.txt"),
            "gitignored file excluded"
        );
        // sorted
        let mut sorted = paths.clone();
        sorted.sort();
        assert_eq!(paths, sorted);

        let capped = list_project_paths(root, 1);
        assert_eq!(capped.len(), 1, "respects cap");
    }
}
