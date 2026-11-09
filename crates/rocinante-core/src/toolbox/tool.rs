//! The `toolbox` tool (one tool, an action enum) and the `/toolbox` slash
//! command both frontends share.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use super::{Plan, ToolSpec};
use crate::agent::events::{AgentEvent, PermissionDecision};
use crate::config::{AutoInstall, Config, Mode};
use crate::tools::{Tool, ToolCtx, ToolKind, ToolOutput};

pub struct ToolboxTool {
    specs: Vec<ToolSpec>,
    /// Plans resolved for a permission preview, reused by the run.
    plans: Mutex<HashMap<String, Plan>>,
    description: &'static str,
}

#[derive(Deserialize)]
struct Args {
    action: String,
    #[serde(default)]
    name: Option<String>,
}

impl ToolboxTool {
    pub fn new(config: &Config) -> Self {
        let specs = super::registry(config);
        let names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();
        let description = format!(
            "Install, update, check or remove external command-line tools that skills need ({}). Use when a loaded skill says its tool is not installed. Installs go to {} (already on PATH for bash) after the user approves.",
            names.join(", "),
            super::display_dir(&super::bin_dir())
        );
        Self {
            specs,
            plans: Mutex::new(HashMap::new()),
            description: Box::leak(description.into_boxed_str()),
        }
    }

    fn spec(&self, name: Option<&str>) -> Result<&ToolSpec, String> {
        let names = || {
            self.specs
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        };
        let name = name.ok_or_else(|| format!("`name` is required (one of: {})", names()))?;
        self.specs
            .iter()
            .find(|s| s.name == name)
            .ok_or_else(|| format!("unknown tool `{name}` (known: {})", names()))
    }

    async fn plan_for(&self, spec: &ToolSpec) -> anyhow::Result<Plan> {
        if let Some(plan) = self.plans.lock().unwrap().get(&spec.name) {
            return Ok(plan.clone());
        }
        let plan = super::plan(spec).await?;
        self.plans
            .lock()
            .unwrap()
            .insert(spec.name.clone(), plan.clone());
        Ok(plan)
    }
}

/// Ask the user directly (used in auto mode, where the permission engine
/// does not prompt but `auto_install = "ask"` requires consent).
async fn confirm(ctx: &ToolCtx, summary: &str) -> bool {
    let request_id = Uuid::new_v4();
    let answer = ctx.router.register(request_id);
    ctx.events.send(AgentEvent::PermissionRequested {
        request_id,
        summary: summary.to_string(),
        tool_name: "toolbox".into(),
        detail: None,
    });
    tokio::select! {
        d = answer => matches!(d, Ok(PermissionDecision::Allow | PermissionDecision::AlwaysAllow)),
        () = ctx.cancel.cancelled() => {
            ctx.router.forget(request_id);
            false
        }
    }
}

#[async_trait]
impl Tool for ToolboxTool {
    fn name(&self) -> &'static str {
        "toolbox"
    }
    fn description(&self) -> &'static str {
        self.description
    }
    fn schema(&self) -> serde_json::Value {
        let names: Vec<&str> = self.specs.iter().map(|s| s.name.as_str()).collect();
        json!({
            "type": "object",
            "properties": {
                "action": { "type": "string", "enum": ["status", "install", "update", "remove"] },
                "name": { "type": "string", "enum": names, "description": "Which tool (omit for status of all)" }
            },
            "required": ["action"]
        })
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Execute
    }
    fn describe_call(&self, args: &serde_json::Value) -> String {
        let action = args.get("action").and_then(|v| v.as_str()).unwrap_or("?");
        let name = args.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let repo = self
            .specs
            .iter()
            .find(|s| s.name == name)
            .map(|s| s.repo.clone())
            .unwrap_or_default();
        let dir = super::display_dir(&super::bin_dir());
        match action {
            "install" => format!("toolbox: install {name} from github.com/{repo} into {dir}"),
            "update" => format!("toolbox: update {name} from github.com/{repo}"),
            "remove" => format!("toolbox: remove {name} from {dir}"),
            _ => format!("toolbox: {action} {name}").trim_end().to_string(),
        }
    }

    async fn preview(&self, args: &serde_json::Value, _ctx: &ToolCtx) -> Option<String> {
        let action = args.get("action").and_then(|v| v.as_str())?;
        if !matches!(action, "install" | "update") {
            return None;
        }
        let spec = self.spec(args.get("name").and_then(|v| v.as_str())).ok()?;
        Some(match self.plan_for(spec).await {
            Ok(plan) => format!(
                "{}\nVerified against the SHA-256 in the release's dist-manifest.json; the upstream installer script is not run.",
                plan.describe(&super::bin_dir())
            ),
            Err(e) => format!("(could not resolve the latest release: {e:#})"),
        })
    }

    async fn run(&self, args: serde_json::Value, ctx: &ToolCtx) -> ToolOutput {
        let args: Args = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return ToolOutput::error(format!("bad arguments: {e}")),
        };
        match args.action.as_str() {
            "status" => {
                let specs: Vec<ToolSpec> = match args.name.as_deref() {
                    Some(n) => match self.spec(Some(n)) {
                        Ok(s) => vec![s.clone()],
                        Err(e) => return ToolOutput::error(e),
                    },
                    None => self.specs.clone(),
                };
                ToolOutput::ok(status_lines(&specs, true).await.join("\n"))
            }
            "install" | "update" => {
                let spec = match self.spec(args.name.as_deref()) {
                    Ok(s) => s.clone(),
                    Err(e) => return ToolOutput::error(e),
                };
                if super::auto_install() == AutoInstall::Never {
                    return ToolOutput::error(format!(
                        "installing is disabled ([toolbox] auto_install = \"never\"). Tell the user to install {} from https://github.com/{}/releases",
                        spec.name, spec.repo
                    ));
                }
                let installed = super::installed_version(&spec).await;
                if args.action == "install"
                    && let Some(path) = super::locate(&spec.name)
                {
                    return ToolOutput::ok(format!(
                        "{} {} is already installed at {} — use it.",
                        spec.name,
                        installed.as_deref().unwrap_or("(unknown version)"),
                        path.display()
                    ));
                }
                let plan = match self.plan_for(&spec).await {
                    Ok(p) => p,
                    Err(e) => return ToolOutput::error(format!("{e:#}")),
                };
                if let Some(current) = &installed
                    && !super::is_newer(&plan.selected.version, current)
                {
                    return ToolOutput::ok(format!(
                        "{} {current} is up to date (latest {}).",
                        spec.name, plan.selected.version
                    ));
                }
                let dir = super::bin_dir();
                // Normal mode already asked through the permission engine;
                // auto mode asks here unless `auto_install = "always"`.
                if ctx.mode == Mode::Auto
                    && super::auto_install() == AutoInstall::Ask
                    && !confirm(ctx, &plan.describe(&dir)).await
                {
                    return ToolOutput::error(
                        "The user declined the install. Ask them how to proceed.",
                    );
                }
                match install_now(&plan, &spec, &dir).await {
                    Ok(text) => {
                        self.plans.lock().unwrap().remove(&spec.name);
                        ToolOutput::ok(text)
                    }
                    Err(e) => ToolOutput::error(format!("install failed: {e:#}")),
                }
            }
            "remove" => {
                let spec = match self.spec(args.name.as_deref()) {
                    Ok(s) => s.clone(),
                    Err(e) => return ToolOutput::error(e),
                };
                match remove_now(&spec.name) {
                    Ok(text) => ToolOutput::ok(text),
                    Err(e) => ToolOutput::error(format!("{e:#}")),
                }
            }
            other => ToolOutput::error(format!(
                "unknown action `{other}` (status | install | update | remove)"
            )),
        }
    }
}

fn records() -> anyhow::Result<PathBuf> {
    super::records_path().ok_or_else(|| anyhow::anyhow!("no home directory"))
}

async fn install_now(
    plan: &Plan,
    spec: &ToolSpec,
    dir: &std::path::Path,
) -> anyhow::Result<String> {
    let done = super::install(plan, spec, dir, &records()?).await?;
    let mut text = format!(
        "installed {} {} → {}",
        plan.name,
        done.version,
        done.bins
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
    if let Some(doctor) = &done.doctor {
        text.push_str(&format!("\n{} doctor:\n{doctor}", plan.name));
    }
    Ok(text)
}

fn remove_now(name: &str) -> anyhow::Result<String> {
    let removed = super::remove(name, &super::bin_dir(), &records()?)?;
    Ok(format!(
        "removed {name}: {}",
        removed
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

/// One line per tool: installed version and where, vs the latest release.
pub async fn status_lines(specs: &[ToolSpec], check_latest: bool) -> Vec<String> {
    let records = super::records_path()
        .map(|p| super::load_records(&p))
        .unwrap_or_default();
    let mut out = Vec::new();
    for spec in specs {
        let installed = super::installed_version(spec).await;
        let location = super::locate(&spec.name).map(|p| {
            if records.tools.contains_key(&spec.name) {
                "toolbox".to_string()
            } else {
                super::display_dir(&p)
            }
        });
        let latest = if check_latest {
            tokio::time::timeout(
                std::time::Duration::from_secs(10),
                crate::selfupdate::latest_release(&spec.repo),
            )
            .await
            .ok()
            .and_then(Result::ok)
            .map(|r| r.version().to_string())
        } else {
            None
        };
        let mut line = match (&installed, &location) {
            (Some(v), Some(at)) => format!("{} {v} ({at})", spec.name),
            (None, Some(at)) => format!("{} (version unknown, {at})", spec.name),
            _ => format!("{}: not installed", spec.name),
        };
        match (&installed, &latest) {
            (Some(v), Some(l)) if super::is_newer(l, v) => {
                line.push_str(&format!(" · latest {l} — /toolbox update {}", spec.name));
            }
            (Some(_), Some(_)) => line.push_str(" · up to date"),
            (None, Some(l)) => {
                line.push_str(&format!(" · latest {l} — /toolbox install {}", spec.name))
            }
            (_, None) if check_latest => line.push_str(" · latest unknown (offline?)"),
            _ => {}
        }
        out.push(line);
    }
    out
}

/// `/toolbox [install|update|remove] [name]`, typed by the user: no
/// permission prompt (the user asked), output as notice lines.
pub async fn user_command(config: &Config, args: &str) -> Vec<String> {
    let specs = super::registry(config);
    let mut words = args.split_whitespace();
    let action = words.next().unwrap_or("status");
    let name = words.next();
    let find = |n: &str| specs.iter().find(|s| s.name == n).cloned();
    match (action, name) {
        ("status", None) | ("", None) => status_lines(&specs, true).await,
        ("status", Some(n)) => match find(n) {
            Some(s) => status_lines(&[s], true).await,
            None => vec![format!("unknown tool `{n}`")],
        },
        ("install", Some(n)) | ("update", Some(n)) => {
            let Some(spec) = find(n) else {
                return vec![format!("unknown tool `{n}`")];
            };
            let installed = super::installed_version(&spec).await;
            if action == "install" && super::locate(n).is_some() {
                return vec![format!(
                    "{n} {} is already installed",
                    installed.as_deref().unwrap_or("")
                )];
            }
            let plan = match super::plan(&spec).await {
                Ok(p) => p,
                Err(e) => return vec![format!("{n}: {e:#}")],
            };
            if let Some(current) = &installed
                && !super::is_newer(&plan.selected.version, current)
            {
                return vec![format!("{n} {current} is up to date")];
            }
            let dir = super::bin_dir();
            let mut lines = vec![plan.describe(&dir)];
            match install_now(&plan, &spec, &dir).await {
                Ok(text) => lines.extend(text.lines().map(String::from)),
                Err(e) => lines.push(format!("install failed: {e:#}")),
            }
            lines
        }
        ("update", None) => {
            let records = super::records_path()
                .map(|p| super::load_records(&p))
                .unwrap_or_default();
            if records.tools.is_empty() {
                return vec!["nothing installed by the toolbox to update".into()];
            }
            let mut lines = Vec::new();
            for name in records.tools.keys() {
                lines.extend(Box::pin(user_command(config, &format!("update {name}"))).await);
            }
            lines
        }
        ("remove", Some(n)) => match remove_now(n) {
            Ok(text) => vec![text],
            Err(e) => vec![format!("{e:#}")],
        },
        _ => vec!["usage: /toolbox [status|install|update|remove] [name]".into()],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool() -> ToolboxTool {
        let config = crate::config::load_from(
            std::path::Path::new("/nonexistent/a.toml"),
            std::path::Path::new("/nonexistent/b.toml"),
        )
        .unwrap();
        ToolboxTool::new(&config)
    }

    #[test]
    fn schema_and_summary() {
        let t = tool();
        assert_eq!(t.kind(), ToolKind::Execute);
        let schema = t.schema();
        assert_eq!(
            schema["properties"]["name"]["enum"],
            json!(["ferraus", "pixelox"])
        );
        let s = t.describe_call(&json!({"action": "install", "name": "ferraus"}));
        assert!(
            s.starts_with("toolbox: install ferraus from github.com/djynnius/ferraus into "),
            "{s}"
        );
        assert_eq!(
            t.describe_call(&json!({"action": "status"})),
            "toolbox: status"
        );
    }

    #[tokio::test]
    async fn unknown_names_and_actions_are_errors() {
        let t = tool();
        let ctx = ToolCtx {
            cwd: std::env::temp_dir(),
            events: crate::agent::events::EventSender::new(tokio::sync::broadcast::channel(8).0),
            cancel: Default::default(),
            depth: 0,
            router: Default::default(),
            lsp: None,
            mode: Mode::Normal,
        };
        let out = t
            .run(json!({"action": "install", "name": "nope"}), &ctx)
            .await;
        assert!(out.is_error && out.content.contains("unknown tool"));
        let out = t.run(json!({"action": "install"}), &ctx).await;
        assert!(out.is_error && out.content.contains("required"));
        let out = t
            .run(json!({"action": "explode", "name": "ferraus"}), &ctx)
            .await;
        assert!(out.is_error && out.content.contains("unknown action"));
    }

    #[tokio::test]
    async fn status_lines_report_missing_tools_offline() {
        let spec = ToolSpec {
            name: "rocinante-test-surely-missing-tool".into(),
            repo: "nobody/nothing".into(),
            version_cmd: vec![
                "rocinante-test-surely-missing-tool".into(),
                "--version".into(),
            ],
            doctor_cmd: None,
            mcp_args: None,
            mcp: false,
        };
        let lines = status_lines(&[spec], false).await;
        assert_eq!(
            lines,
            vec!["rocinante-test-surely-missing-tool: not installed"]
        );
    }
}
