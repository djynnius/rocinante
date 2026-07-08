use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

use super::traits::{Tool, ToolCtx, ToolKind, ToolOutput, truncate_output};

pub struct ReadTool;

#[derive(Deserialize)]
struct Args {
    path: String,
    #[serde(default)]
    offset: Option<usize>,
    #[serde(default)]
    limit: Option<usize>,
}

const DEFAULT_LIMIT: usize = 2000;

#[async_trait]
impl Tool for ReadTool {
    fn name(&self) -> &'static str {
        "read"
    }
    fn description(&self) -> &'static str {
        "Read a text file. Returns numbered lines. Use offset/limit for large files."
    }
    fn schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "File path (absolute or relative to cwd)" },
                "offset": { "type": "integer", "description": "1-based line to start from" },
                "limit": { "type": "integer", "description": "Max lines to return (default 2000)" }
            },
            "required": ["path"]
        })
    }
    fn kind(&self) -> ToolKind {
        ToolKind::ReadOnly
    }
    fn describe_call(&self, args: &serde_json::Value) -> String {
        format!(
            "read: {}",
            args.get("path").and_then(|v| v.as_str()).unwrap_or("?")
        )
    }

    async fn run(&self, args: serde_json::Value, ctx: &ToolCtx) -> ToolOutput {
        let args: Args = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return ToolOutput::error(format!("bad arguments: {e}")),
        };
        let path = ctx.resolve(&args.path);
        let content = match tokio::fs::read(&path).await {
            Ok(bytes) => match String::from_utf8(bytes) {
                Ok(s) => s,
                Err(_) => {
                    return ToolOutput::error(format!("{} is not UTF-8 text", path.display()));
                }
            },
            Err(e) => return ToolOutput::error(format!("cannot read {}: {e}", path.display())),
        };
        let start = args.offset.unwrap_or(1).max(1);
        let limit = args.limit.unwrap_or(DEFAULT_LIMIT);
        let total = content.lines().count();
        if total == 0 {
            return ToolOutput::ok("(empty file)");
        }
        let numbered: String = content
            .lines()
            .enumerate()
            .skip(start - 1)
            .take(limit)
            .map(|(i, line)| format!("{:>6}\t{line}\n", i + 1))
            .collect();
        if numbered.is_empty() {
            return ToolOutput::error(format!(
                "offset {start} is past end of file ({total} lines)"
            ));
        }
        let mut out = truncate_output(&numbered, DEFAULT_LIMIT + 10, 200_000);
        let shown = numbered.lines().count();
        if start - 1 + shown < total {
            out.push_str(&format!(
                "\n[showing lines {start}-{}, file has {total} lines]",
                start - 1 + shown
            ));
        }
        ToolOutput::ok(out)
    }
}
