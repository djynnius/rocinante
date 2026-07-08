//! The `lsp` tool: one tool, five actions, so the schema costs context
//! once. Positions are 1-based for the model and converted to the server's
//! negotiated encoding both directions.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

use lsp_types::request::{DocumentSymbolRequest, GotoDefinition, HoverRequest, References};
use lsp_types::{
    DocumentSymbol, DocumentSymbolParams, DocumentSymbolResponse, GotoDefinitionParams,
    GotoDefinitionResponse, HoverContents, HoverParams, HoverProviderCapability, Location,
    MarkedString, OneOf, Position, ReferenceContext, ReferenceParams, TextDocumentIdentifier,
    TextDocumentPositionParams,
};

use super::LspManager;
use super::client::{
    Encoding, LspClient, from_lsp_character, path_to_uri, to_lsp_position, uri_to_path,
};
use super::diagnostics::format_diagnostics;
use crate::tools::{Tool, ToolCtx, ToolKind, ToolOutput, truncate_output};

/// Explicit re-checks may wait longer than the post-edit hook: the user
/// asked, and the first cargo check on a cold workspace is slow.
const RECHECK_WAIT: Duration = Duration::from_secs(8);

pub struct LspTool {
    manager: Arc<LspManager>,
}

impl LspTool {
    pub fn new(manager: Arc<LspManager>) -> Self {
        Self { manager }
    }
}

#[derive(Deserialize)]
struct Args {
    action: String,
    path: String,
    line: Option<u32>,
    column: Option<u32>,
    query: Option<String>,
}

#[async_trait]
impl Tool for LspTool {
    fn name(&self) -> &'static str {
        "lsp"
    }
    fn description(&self) -> &'static str {
        "Query the language server. Actions: diagnostics (errors/warnings for a file), definition, references, hover (need line+column), symbols (optional query filter). line/column are 1-based."
    }
    fn schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["diagnostics", "definition", "references", "hover", "symbols"]
                },
                "path": { "type": "string", "description": "File to query" },
                "line": { "type": "integer", "description": "1-based line (definition/references/hover)" },
                "column": { "type": "integer", "description": "1-based column (definition/references/hover)" },
                "query": { "type": "string", "description": "Symbol name filter (symbols)" }
            },
            "required": ["action", "path"]
        })
    }
    fn kind(&self) -> ToolKind {
        ToolKind::ReadOnly
    }
    fn describe_call(&self, args: &serde_json::Value) -> String {
        let get = |k| args.get(k).and_then(|v| v.as_str()).unwrap_or("?");
        format!("lsp: {} {}", get("action"), get("path"))
    }

    async fn run(&self, args: serde_json::Value, ctx: &ToolCtx) -> ToolOutput {
        let args: Args = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return ToolOutput::error(format!("bad arguments: {e}")),
        };
        let path = ctx.resolve(&args.path);
        let content = match tokio::fs::read_to_string(&path).await {
            Ok(c) => c,
            Err(e) => return ToolOutput::error(format!("cannot read {}: {e}", path.display())),
        };
        let client = match self.manager.client_for(&path).await {
            Ok(Some(c)) => c,
            Ok(None) => {
                return ToolOutput::ok(
                    "no language server available for this file (unsupported filetype, binary not on PATH, or no project root marker)",
                );
            }
            Err(e) => return ToolOutput::error(format!("language server failed to start: {e}")),
        };
        let action = run_action(&args, &client, &path, &content);
        tokio::select! {
            out = action => out.unwrap_or_else(|e| ToolOutput::error(format!("lsp request failed: {e}"))),
            () = ctx.cancel.cancelled() => ToolOutput::error("lsp call cancelled"),
        }
    }
}

async fn run_action(
    args: &Args,
    client: &LspClient,
    path: &Path,
    content: &str,
) -> anyhow::Result<ToolOutput> {
    match args.action.as_str() {
        "diagnostics" => diagnostics(client, path, content).await,
        "definition" | "references" | "hover" => {
            let (line, column) = match (args.line, args.column) {
                (Some(l), Some(c)) if l >= 1 && c >= 1 => (l, c),
                _ => {
                    return Ok(ToolOutput::error(format!(
                        "action={} requires `line` and `column` (1-based integers)",
                        args.action
                    )));
                }
            };
            if !supports(client, &args.action) {
                return Ok(ToolOutput::ok(format!(
                    "the {} server does not support {}",
                    client.name(),
                    args.action
                )));
            }
            client.sync_document(path, content).await?;
            let doc = TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: path_to_uri(path)?,
                },
                position: to_lsp_position(content, line, column, client.encoding()),
            };
            match args.action.as_str() {
                "definition" => definition(client, doc).await,
                "references" => references(client, doc).await,
                _ => hover(client, doc).await,
            }
        }
        "symbols" => {
            if !supports(client, "symbols") {
                return Ok(ToolOutput::ok(format!(
                    "the {} server does not support symbols",
                    client.name()
                )));
            }
            client.sync_document(path, content).await?;
            symbols(client, path, args.query.as_deref()).await
        }
        other => Ok(ToolOutput::error(format!(
            "unknown action `{other}`; expected diagnostics, definition, references, hover, or symbols"
        ))),
    }
}

fn supports(client: &LspClient, action: &str) -> bool {
    fn one_of<T>(p: &Option<OneOf<bool, T>>) -> bool {
        match p {
            Some(OneOf::Left(enabled)) => *enabled,
            Some(OneOf::Right(_)) => true,
            None => false,
        }
    }
    let caps = client.capabilities();
    match action {
        "definition" => one_of(&caps.definition_provider),
        "references" => one_of(&caps.references_provider),
        "symbols" => one_of(&caps.document_symbol_provider),
        "hover" => match &caps.hover_provider {
            Some(HoverProviderCapability::Simple(enabled)) => *enabled,
            Some(HoverProviderCapability::Options(_)) => true,
            None => false,
        },
        _ => true,
    }
}

async fn diagnostics(client: &LspClient, path: &Path, content: &str) -> anyhow::Result<ToolOutput> {
    let (uri, version, generation) = client.sync_document(path, content).await?;
    let display = path.display().to_string();
    let render = |diags: &[lsp_types::Diagnostic]| {
        format_diagnostics(&display, content, diags, client.encoding())
    };
    let text = match client
        .diagnostics()
        .wait_for(&uri, version, generation, RECHECK_WAIT, version == 1)
        .await
    {
        Some(diags) => render(&diags),
        None => match client.diagnostics().get(&uri) {
            Some(diags) if !diags.is_empty() => format!(
                "server is still analyzing; latest known state:\n{}",
                render(&diags)
            ),
            _ => "diagnostics pending — the server is still analyzing; re-run action=diagnostics shortly".into(),
        },
    };
    Ok(ToolOutput::ok(text))
}

async fn definition(
    client: &LspClient,
    doc: TextDocumentPositionParams,
) -> anyhow::Result<ToolOutput> {
    let response = client
        .request::<GotoDefinition>(GotoDefinitionParams {
            text_document_position_params: doc,
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
        .await?;
    let targets: Vec<(String, Position)> = match response {
        None => vec![],
        Some(GotoDefinitionResponse::Scalar(loc)) => {
            vec![(loc.uri.as_str().to_string(), loc.range.start)]
        }
        Some(GotoDefinitionResponse::Array(locs)) => locs
            .into_iter()
            .map(|l| (l.uri.as_str().to_string(), l.range.start))
            .collect(),
        Some(GotoDefinitionResponse::Link(links)) => links
            .into_iter()
            .map(|l| {
                (
                    l.target_uri.as_str().to_string(),
                    l.target_selection_range.start,
                )
            })
            .collect(),
    };
    if targets.is_empty() {
        return Ok(ToolOutput::ok("no definition found at this position"));
    }
    let mut lines = Vec::new();
    for (uri, pos) in targets.iter().take(10) {
        lines.push(format_location(uri, *pos, client.encoding()).await);
    }
    if targets.len() > 10 {
        lines.push(format!("… and {} more", targets.len() - 10));
    }
    Ok(ToolOutput::ok(lines.join("\n")))
}

async fn references(
    client: &LspClient,
    doc: TextDocumentPositionParams,
) -> anyhow::Result<ToolOutput> {
    let response = client
        .request::<References>(ReferenceParams {
            text_document_position: doc,
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: ReferenceContext {
                include_declaration: true,
            },
        })
        .await?;
    let locations: Vec<Location> = response.unwrap_or_default();
    if locations.is_empty() {
        return Ok(ToolOutput::ok("no references found"));
    }
    let mut lines = vec![format!("{} reference(s):", locations.len())];
    for loc in locations.iter().take(20) {
        lines.push(format_location(loc.uri.as_str(), loc.range.start, client.encoding()).await);
    }
    if locations.len() > 20 {
        lines.push(format!("… and {} more", locations.len() - 20));
    }
    Ok(ToolOutput::ok(lines.join("\n")))
}

async fn hover(client: &LspClient, doc: TextDocumentPositionParams) -> anyhow::Result<ToolOutput> {
    let response = client
        .request::<HoverRequest>(HoverParams {
            text_document_position_params: doc,
            work_done_progress_params: Default::default(),
        })
        .await?;
    let marked = |m: MarkedString| match m {
        MarkedString::String(s) => s,
        MarkedString::LanguageString(l) => l.value,
    };
    let text = match response {
        None => String::new(),
        Some(h) => match h.contents {
            HoverContents::Scalar(m) => marked(m),
            HoverContents::Array(v) => v.into_iter().map(marked).collect::<Vec<_>>().join("\n\n"),
            HoverContents::Markup(m) => m.value,
        },
    };
    if text.trim().is_empty() {
        return Ok(ToolOutput::ok("no hover information at this position"));
    }
    Ok(ToolOutput::ok(truncate_output(&text, 80, 8_000)))
}

async fn symbols(
    client: &LspClient,
    path: &Path,
    query: Option<&str>,
) -> anyhow::Result<ToolOutput> {
    let response = client
        .request::<DocumentSymbolRequest>(DocumentSymbolParams {
            text_document: TextDocumentIdentifier {
                uri: path_to_uri(path)?,
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
        .await?;
    // (name for filtering, rendered line)
    let mut entries: Vec<(String, String)> = Vec::new();
    match response {
        None => {}
        Some(DocumentSymbolResponse::Flat(list)) => {
            for s in list {
                entries.push((
                    s.name.clone(),
                    format!(
                        "{}: {:?} {}",
                        s.location.range.start.line + 1,
                        s.kind,
                        s.name
                    ),
                ));
            }
        }
        Some(DocumentSymbolResponse::Nested(list)) => flatten_symbols(&list, 0, &mut entries),
    }
    if let Some(query) = query {
        let needle = query.to_lowercase();
        entries.retain(|(name, _)| name.to_lowercase().contains(&needle));
    }
    if entries.is_empty() {
        return Ok(ToolOutput::ok(match query {
            Some(q) => format!("no symbols matching `{q}`"),
            None => "no symbols in this file".into(),
        }));
    }
    let text: String = entries
        .into_iter()
        .map(|(_, line)| line)
        .collect::<Vec<_>>()
        .join("\n");
    Ok(ToolOutput::ok(truncate_output(&text, 100, 10_000)))
}

fn flatten_symbols(symbols: &[DocumentSymbol], depth: usize, out: &mut Vec<(String, String)>) {
    for s in symbols {
        out.push((
            s.name.clone(),
            format!(
                "{}{}: {:?} {}",
                "  ".repeat(depth),
                s.selection_range.start.line + 1,
                s.kind,
                s.name
            ),
        ));
        if let Some(children) = &s.children {
            flatten_symbols(children, depth + 1, out);
        }
    }
}

/// `path:line:col` with the column converted back to 1-based UTF-8 bytes
/// (needs the target file's line text; falls back to the raw offset).
async fn format_location(uri: &str, pos: Position, encoding: Encoding) -> String {
    match uri_to_path(uri) {
        Some(p) => {
            let col = match tokio::fs::read_to_string(&p).await {
                Ok(c) => c
                    .lines()
                    .nth(pos.line as usize)
                    .map(|l| from_lsp_character(l, pos.character, encoding))
                    .unwrap_or(pos.character + 1),
                Err(_) => pos.character + 1,
            };
            format!("{}:{}:{}", p.display(), pos.line + 1, col)
        }
        None => format!("{uri}:{}:{}", pos.line + 1, pos.character + 1),
    }
}
