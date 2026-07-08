use async_trait::async_trait;
use grep_regex::RegexMatcher;
use grep_searcher::sinks::UTF8;
use grep_searcher::{BinaryDetection, SearcherBuilder};
use ignore::WalkBuilder;
use serde::Deserialize;
use serde_json::json;

use super::traits::{Tool, ToolCtx, ToolKind, ToolOutput, truncate_output};

pub struct GrepTool;

#[derive(Deserialize)]
struct Args {
    pattern: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    glob: Option<String>,
    #[serde(default)]
    case_insensitive: bool,
}

const MAX_MATCHES: usize = 500;

#[async_trait]
impl Tool for GrepTool {
    fn name(&self) -> &'static str {
        "grep"
    }
    fn description(&self) -> &'static str {
        "Search file contents with a regex. Respects .gitignore. Returns file:line matches."
    }
    fn schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Regular expression" },
                "path": { "type": "string", "description": "Directory or file to search (default cwd)" },
                "glob": { "type": "string", "description": "Only files matching this glob, e.g. *.rs" },
                "case_insensitive": { "type": "boolean" }
            },
            "required": ["pattern"]
        })
    }
    fn kind(&self) -> ToolKind {
        ToolKind::ReadOnly
    }
    fn describe_call(&self, args: &serde_json::Value) -> String {
        format!(
            "grep: {}",
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

        // File walking + regex search are blocking; keep them off the runtime.
        let result = tokio::task::spawn_blocking(move || search(&args, &root, &cwd)).await;
        match result {
            Ok(Ok(out)) => out,
            Ok(Err(e)) => ToolOutput::error(e),
            Err(e) => ToolOutput::error(format!("search task failed: {e}")),
        }
    }
}

fn search(
    args: &Args,
    root: &std::path::Path,
    cwd: &std::path::Path,
) -> Result<ToolOutput, String> {
    let pattern = if args.case_insensitive {
        format!("(?i){}", args.pattern)
    } else {
        args.pattern.clone()
    };
    let matcher = RegexMatcher::new_line_matcher(&pattern)
        .map_err(|e| format!("bad regex `{}`: {e}", args.pattern))?;

    let glob = match &args.glob {
        Some(g) => Some(
            globset::GlobBuilder::new(g)
                .literal_separator(false)
                .build()
                .map_err(|e| format!("bad glob `{g}`: {e}"))?
                .compile_matcher(),
        ),
        None => None,
    };

    let mut searcher = SearcherBuilder::new()
        .binary_detection(BinaryDetection::quit(b'\x00'))
        .line_number(true)
        .build();

    let mut hits: Vec<String> = Vec::new();
    for entry in WalkBuilder::new(root).hidden(true).build() {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        if let Some(g) = &glob {
            let name = entry.file_name().to_string_lossy();
            if !g.is_match(name.as_ref()) && !g.is_match(entry.path()) {
                continue;
            }
        }
        if hits.len() >= MAX_MATCHES {
            break;
        }
        let display = entry
            .path()
            .strip_prefix(cwd)
            .unwrap_or(entry.path())
            .display()
            .to_string();
        let _ = searcher.search_path(
            &matcher,
            entry.path(),
            UTF8(|line_num, line| {
                hits.push(format!("{display}:{line_num}: {}", line.trim_end()));
                Ok(hits.len() < MAX_MATCHES)
            }),
        );
    }

    if hits.is_empty() {
        return Ok(ToolOutput::ok("no matches"));
    }
    let capped = hits.len() >= MAX_MATCHES;
    let mut out = truncate_output(&hits.join("\n"), MAX_MATCHES + 10, 100_000);
    if capped {
        out.push_str("\n[result capped at 500 matches — narrow the pattern]");
    }
    Ok(ToolOutput::ok(out))
}
