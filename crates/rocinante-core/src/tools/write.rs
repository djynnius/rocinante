use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

use super::traits::{Tool, ToolCtx, ToolKind, ToolOutput};

pub struct WriteTool;

#[derive(Deserialize)]
struct Args {
    path: String,
    content: String,
}

#[async_trait]
impl Tool for WriteTool {
    fn name(&self) -> &'static str {
        "write"
    }
    fn description(&self) -> &'static str {
        "Create or overwrite a file with the given content."
    }
    fn schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "File path" },
                "content": { "type": "string", "description": "Full file content" }
            },
            "required": ["path", "content"]
        })
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Edit
    }
    fn describe_call(&self, args: &serde_json::Value) -> String {
        let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("?");
        let bytes = args
            .get("content")
            .and_then(|v| v.as_str())
            .map(str::len)
            .unwrap_or(0);
        format!("write: {path} ({bytes} bytes)")
    }

    async fn preview(&self, args: &serde_json::Value, ctx: &ToolCtx) -> Option<String> {
        let args: Args = serde_json::from_value(args.clone()).ok()?;
        let path = ctx.resolve(&args.path);
        match tokio::fs::read_to_string(&path).await {
            Ok(old) => Some(super::traits::render_diff(&old, &args.content, &args.path)),
            Err(_) => {
                let head: Vec<&str> = args.content.lines().take(40).collect();
                let more = args.content.lines().count().saturating_sub(head.len());
                let mut out = format!("new file: {}\n{}", args.path, head.join("\n"));
                if more > 0 {
                    out.push_str(&format!("\n… (+{more} more lines)"));
                }
                Some(out)
            }
        }
    }

    async fn run(&self, args: serde_json::Value, ctx: &ToolCtx) -> ToolOutput {
        let args: Args = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return ToolOutput::error(format!("bad arguments: {e}")),
        };
        let path = ctx.resolve(&args.path);
        if let Some(parent) = path.parent()
            && let Err(e) = tokio::fs::create_dir_all(parent).await
        {
            return ToolOutput::error(format!("cannot create {}: {e}", parent.display()));
        }
        match tokio::fs::write(&path, &args.content).await {
            Ok(()) => ToolOutput::ok(format!(
                "wrote {} bytes to {}",
                args.content.len(),
                path.display()
            )),
            Err(e) => ToolOutput::error(format!("cannot write {}: {e}", path.display())),
        }
    }
}
