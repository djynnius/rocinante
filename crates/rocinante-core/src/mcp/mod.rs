//! MCP (Model Context Protocol) client: connects configured servers and
//! exposes their tools through the ordinary [`Tool`](crate::tools::Tool)
//! registry as `mcp__<server>__<tool>` (the Claude Code convention), so
//! permissions, subagent tool subsets, and the repair pipeline all apply
//! unchanged. Tools only in v1 — no resources/prompts/sampling.

use std::collections::BTreeMap;
use std::time::Duration;

use async_trait::async_trait;
use rmcp::ServiceExt;
use rmcp::model::CallToolRequestParams;
use rmcp::service::{Peer, RoleClient, RunningService};
use rmcp::transport::{ConfigureCommandExt, StreamableHttpClientTransport, TokioChildProcess};

use crate::config::{Config, McpServerConfig};
use crate::tools::{Tool, ToolCtx, ToolKind, ToolOutput, truncate_output};

/// Per-call timeout; a hung MCP server must not hang the turn.
const CALL_TIMEOUT: Duration = Duration::from_secs(60);
/// Above this many total tools, warn: schemas cost context and calling
/// accuracy on a ~30B main model.
pub const TOOL_COUNT_WARNING: usize = 25;

/// Owns the running server connections for the session. Servers shut down
/// when the process exits (their stdio closes); `shutdown()` is the
/// graceful version for callers that want it.
pub struct McpManager {
    services: Vec<(String, RunningService<RoleClient, ()>)>,
}

impl McpManager {
    /// Connect every configured server and collect their tools. A server
    /// that fails to start or list degrades to a warning — Rocinante still
    /// starts.
    pub async fn connect_all(config: &Config) -> (Self, Vec<McpTool>) {
        let mut services = Vec::new();
        let mut tools: Vec<McpTool> = Vec::new();
        for (name, server) in &config.mcp {
            match connect_one(name, server).await {
                Ok((service, mut server_tools)) => {
                    tracing::info!(
                        server = name,
                        tools = server_tools.len(),
                        "mcp server connected"
                    );
                    services.push((name.clone(), service));
                    tools.append(&mut server_tools);
                }
                Err(e) => {
                    tracing::warn!(server = name, error = %e, "mcp server failed to start; skipping");
                }
            }
        }
        (Self { services }, tools)
    }

    pub async fn shutdown(self) {
        for (name, service) in self.services {
            if let Err(e) = service.cancel().await {
                tracing::debug!(server = name, error = %e, "mcp shutdown");
            }
        }
    }
}

async fn connect_one(
    name: &str,
    server: &McpServerConfig,
) -> anyhow::Result<(RunningService<RoleClient, ()>, Vec<McpTool>)> {
    let service = match (&server.command, &server.url) {
        (Some(command), None) => {
            let env = resolve_env(server)?;
            let transport =
                TokioChildProcess::new(tokio::process::Command::new(command).configure(|c| {
                    c.args(&server.args);
                    for (k, v) in &env {
                        c.env(k, v);
                    }
                }))?;
            ().serve(transport).await?
        }
        (None, Some(url)) => {
            let transport = StreamableHttpClientTransport::from_uri(url.clone());
            ().serve(transport).await?
        }
        _ => anyhow::bail!("exactly one of command/url required"),
    };

    let listed = service.list_all_tools().await?;
    let peer = service.peer().clone();
    let tools = listed
        .into_iter()
        .filter(|t| match &server.include {
            Some(include) => include.iter().any(|i| i == t.name.as_ref()),
            None => true,
        })
        .map(|t| McpTool::new(name, peer.clone(), t))
        .collect();
    Ok((service, tools))
}

/// Env for a stdio child: literal `env` entries plus `env_from` entries
/// resolved from our own environment (so secrets stay out of config).
fn resolve_env(server: &McpServerConfig) -> anyhow::Result<BTreeMap<String, String>> {
    let mut env = server.env.clone();
    for (child_key, host_var) in &server.env_from {
        match std::env::var(host_var) {
            Ok(value) => {
                env.insert(child_key.clone(), value);
            }
            Err(_) => anyhow::bail!("env_from: host variable `{host_var}` is not set"),
        }
    }
    Ok(env)
}

/// One remote tool, adapted to the local [`Tool`] trait.
pub struct McpTool {
    /// `mcp__<server>__<tool>`, leaked once (the trait wants &'static str).
    name: &'static str,
    description: &'static str,
    remote_name: String,
    schema: serde_json::Value,
    peer: Peer<RoleClient>,
}

impl McpTool {
    fn new(server: &str, peer: Peer<RoleClient>, tool: rmcp::model::Tool) -> Self {
        let name: &'static str =
            Box::leak(format!("mcp__{server}__{}", tool.name).into_boxed_str());
        let description: &'static str = Box::leak(
            tool.description
                .as_deref()
                .unwrap_or("(no description provided by server)")
                .to_string()
                .into_boxed_str(),
        );
        Self {
            name,
            description,
            remote_name: tool.name.to_string(),
            schema: serde_json::Value::Object((*tool.input_schema).clone()),
            peer,
        }
    }
}

#[async_trait]
impl Tool for McpTool {
    fn name(&self) -> &'static str {
        self.name
    }
    fn description(&self) -> &'static str {
        self.description
    }
    fn schema(&self) -> serde_json::Value {
        self.schema.clone()
    }
    fn kind(&self) -> ToolKind {
        // Remote effects are opaque; treat like command execution so Normal
        // and Auto modes ask unless an allow rule covers the tool name.
        ToolKind::Execute
    }
    fn describe_call(&self, args: &serde_json::Value) -> String {
        let preview: String = args.to_string().chars().take(80).collect();
        format!("{}: {preview}", self.name)
    }

    async fn run(&self, args: serde_json::Value, ctx: &ToolCtx) -> ToolOutput {
        let arguments = match args {
            serde_json::Value::Object(map) => Some(map),
            serde_json::Value::Null => None,
            other => {
                return ToolOutput::error(format!("arguments must be a JSON object, got: {other}"));
            }
        };
        let mut request = CallToolRequestParams::new(self.remote_name.clone());
        if let Some(arguments) = arguments {
            request = request.with_arguments(arguments);
        }

        let call = self.peer.call_tool(request);
        let result = tokio::select! {
            r = tokio::time::timeout(CALL_TIMEOUT, call) => r,
            () = ctx.cancel.cancelled() => return ToolOutput::error("mcp call cancelled"),
        };

        match result {
            Err(_) => ToolOutput::error(format!(
                "mcp tool timed out after {}s",
                CALL_TIMEOUT.as_secs()
            )),
            Ok(Err(e)) => ToolOutput::error(format!("mcp call failed: {e}")),
            Ok(Ok(outcome)) => {
                let text = flatten_content(&outcome.content);
                let text = truncate_output(&text, 400, 40_000);
                if outcome.is_error.unwrap_or(false) {
                    ToolOutput::error(if text.is_empty() {
                        "mcp tool reported an error".into()
                    } else {
                        text
                    })
                } else {
                    ToolOutput::ok(if text.is_empty() {
                        "(empty result)".into()
                    } else {
                        text
                    })
                }
            }
        }
    }
}

/// Flatten MCP content blocks into plain text for the model: text blocks
/// verbatim; everything else (images, resources) as a JSON placeholder.
fn flatten_content(content: &[rmcp::model::ContentBlock]) -> String {
    content
        .iter()
        .map(|item| match item.as_text() {
            Some(text) => text.text.clone(),
            None => serde_json::to_string(item)
                .map(|j| format!("[non-text content: {j}]"))
                .unwrap_or_else(|_| "[non-text content]".into()),
        })
        .collect::<Vec<_>>()
        .join("\n")
}
