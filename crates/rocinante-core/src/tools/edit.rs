use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

use super::traits::{Tool, ToolCtx, ToolKind, ToolOutput};

pub struct EditTool;

#[derive(Deserialize)]
struct Args {
    path: String,
    old_string: String,
    new_string: String,
    #[serde(default)]
    replace_all: bool,
}

#[async_trait]
impl Tool for EditTool {
    fn name(&self) -> &'static str {
        "edit"
    }
    fn description(&self) -> &'static str {
        "Replace old_string with new_string in a file. old_string must match the file exactly and be unique (or set replace_all)."
    }
    fn schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" },
                "old_string": { "type": "string", "description": "Exact text to find" },
                "new_string": { "type": "string", "description": "Replacement text" },
                "replace_all": { "type": "boolean", "description": "Replace every occurrence (default false)" }
            },
            "required": ["path", "old_string", "new_string"]
        })
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Edit
    }
    fn describe_call(&self, args: &serde_json::Value) -> String {
        format!(
            "edit: {}",
            args.get("path").and_then(|v| v.as_str()).unwrap_or("?")
        )
    }

    async fn preview(&self, args: &serde_json::Value, ctx: &ToolCtx) -> Option<String> {
        let args: Args = serde_json::from_value(args.clone()).ok()?;
        let path = ctx.resolve(&args.path);
        let content = tokio::fs::read_to_string(&path).await.ok()?;
        let (updated, _) =
            exact_replace(&content, &args).or_else(|| fuzzy_replace(&content, &args))?;
        Some(super::traits::render_diff(&content, &updated, &args.path))
    }

    async fn run(&self, args: serde_json::Value, ctx: &ToolCtx) -> ToolOutput {
        let args: Args = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return ToolOutput::error(format!("bad arguments: {e}")),
        };
        if args.old_string == args.new_string {
            return ToolOutput::error("old_string and new_string are identical");
        }
        let path = ctx.resolve(&args.path);
        let content = match tokio::fs::read_to_string(&path).await {
            Ok(s) => s,
            Err(e) => return ToolOutput::error(format!("cannot read {}: {e}", path.display())),
        };

        let (updated, replaced) = match exact_replace(&content, &args) {
            Some(r) => r,
            None => match fuzzy_replace(&content, &args) {
                Some(r) => r,
                None => return ToolOutput::error(no_match_message(&content, &args.old_string)),
            },
        };

        match tokio::fs::write(&path, &updated).await {
            Ok(()) => ToolOutput::ok(format!(
                "replaced {replaced} occurrence(s) in {}",
                path.display()
            )),
            Err(e) => ToolOutput::error(format!("cannot write {}: {e}", path.display())),
        }
    }
}

fn exact_replace(content: &str, args: &Args) -> Option<(String, usize)> {
    let count = content.matches(&args.old_string).count();
    match count {
        0 => None,
        1 => Some((content.replacen(&args.old_string, &args.new_string, 1), 1)),
        _ if args.replace_all => Some((content.replace(&args.old_string, &args.new_string), count)),
        // Ambiguous: fall through; fuzzy also rejects multi-match, and the
        // error message explains the ambiguity.
        _ => None,
    }
}

/// Whitespace-normalized line matching: tolerates the model getting
/// indentation slightly wrong, the classic local-model edit failure.
fn fuzzy_replace(content: &str, args: &Args) -> Option<(String, usize)> {
    let norm = |s: &str| {
        s.lines()
            .map(str::trim_end)
            .collect::<Vec<_>>()
            .join("\n")
            .trim_end()
            .to_string()
    };
    let target = norm(&args.old_string);
    if target.is_empty() {
        return None;
    }
    let content_lines: Vec<&str> = content.lines().collect();
    let target_len = args.old_string.lines().count().max(1);

    let mut matches: Vec<usize> = Vec::new();
    for start in 0..content_lines.len().saturating_sub(target_len - 1) {
        let window = content_lines[start..start + target_len].join("\n");
        if norm(&window) == target {
            matches.push(start);
        }
    }
    if matches.len() != 1 {
        return None;
    }
    let start = matches[0];
    let mut out_lines: Vec<String> = content_lines[..start]
        .iter()
        .map(|s| s.to_string())
        .collect();
    out_lines.extend(args.new_string.lines().map(|s| s.to_string()));
    out_lines.extend(
        content_lines[start + target_len..]
            .iter()
            .map(|s| s.to_string()),
    );
    let mut out = out_lines.join("\n");
    if content.ends_with('\n') {
        out.push('\n');
    }
    Some((out, 1))
}

fn no_match_message(content: &str, old: &str) -> String {
    let count = content.matches(old).count();
    if count > 1 {
        return format!(
            "old_string matches {count} times; make it unique by including surrounding lines, or set replace_all"
        );
    }
    // Help the model self-correct: show the closest line.
    let first_line = old.lines().next().unwrap_or("").trim();
    let near = content
        .lines()
        .find(|l| {
            !first_line.is_empty() && l.contains(first_line.split_whitespace().next().unwrap_or(""))
        })
        .unwrap_or("");
    if near.is_empty() {
        "old_string not found in file. Re-read the file and copy the text exactly.".into()
    } else {
        format!(
            "old_string not found. Closest line in file: `{near}`. Re-read the file and copy the text exactly."
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(old: &str, new: &str) -> Args {
        Args {
            path: "x".into(),
            old_string: old.into(),
            new_string: new.into(),
            replace_all: false,
        }
    }

    #[test]
    fn exact_single_replacement() {
        let (out, n) = exact_replace("fn a() {}\nfn b() {}", &args("fn a()", "fn c()")).unwrap();
        assert_eq!(n, 1);
        assert!(out.contains("fn c()"));
    }

    #[test]
    fn ambiguous_without_replace_all_fails() {
        assert!(exact_replace("x\nx", &args("x", "y")).is_none());
    }

    #[test]
    fn fuzzy_tolerates_trailing_whitespace() {
        let content = "line one   \nline two\n";
        let (out, _) = fuzzy_replace(content, &args("line one\nline two", "replaced")).unwrap();
        assert_eq!(out, "replaced\n");
    }

    #[test]
    fn fuzzy_rejects_multiple_matches() {
        let content = "a\nb\na\nb\n";
        assert!(fuzzy_replace(content, &args("a\nb", "c")).is_none());
    }
}
