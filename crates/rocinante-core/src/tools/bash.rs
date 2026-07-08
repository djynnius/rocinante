use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

use super::traits::{Tool, ToolCtx, ToolKind, ToolOutput, truncate_output};
use crate::agent::events::AgentEvent;

pub struct BashTool;

#[derive(Deserialize)]
struct Args {
    command: String,
    #[serde(default)]
    timeout_secs: Option<u64>,
}

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_TIMEOUT: Duration = Duration::from_secs(600);

#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &'static str {
        "bash"
    }
    fn description(&self) -> &'static str {
        "Run a shell command in the workspace. Returns stdout+stderr and exit code. Not interactive."
    }
    fn schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "Shell command to run" },
                "timeout_secs": { "type": "integer", "description": "Timeout in seconds (default 120, max 600)" }
            },
            "required": ["command"]
        })
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Execute
    }
    fn describe_call(&self, args: &serde_json::Value) -> String {
        format!(
            "bash: {}",
            args.get("command").and_then(|v| v.as_str()).unwrap_or("?")
        )
    }

    async fn run(&self, args: serde_json::Value, ctx: &ToolCtx) -> ToolOutput {
        let args: Args = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return ToolOutput::error(format!("bad arguments: {e}")),
        };
        let timeout = args
            .timeout_secs
            .map(Duration::from_secs)
            .unwrap_or(DEFAULT_TIMEOUT)
            .min(MAX_TIMEOUT);

        let mut cmd = shell_command(&args.command);
        cmd.current_dir(&ctx.cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // Own process group so cancellation kills the whole tree, not just the shell.
        #[cfg(unix)]
        cmd.process_group(0);

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => return ToolOutput::error(format!("cannot spawn shell: {e}")),
        };

        let stdout = child.stdout.take().expect("piped");
        let stderr = child.stderr.take().expect("piped");
        let events = ctx.events.clone();
        let call_tag = format!(
            "bash:{}",
            &args.command.chars().take(24).collect::<String>()
        );

        let mut collected = String::new();
        let mut out_lines = BufReader::new(stdout).lines();
        let mut err_lines = BufReader::new(stderr).lines();
        let mut out_done = false;
        let mut err_done = false;

        let deadline = tokio::time::sleep(timeout);
        tokio::pin!(deadline);

        let status = loop {
            tokio::select! {
                line = out_lines.next_line(), if !out_done => match line {
                    Ok(Some(l)) => {
                        events.send(AgentEvent::ToolProgress { call_id: call_tag.clone(), chunk: l.clone() });
                        collected.push_str(&l);
                        collected.push('\n');
                    }
                    _ => out_done = true,
                },
                line = err_lines.next_line(), if !err_done => match line {
                    Ok(Some(l)) => {
                        events.send(AgentEvent::ToolProgress { call_id: call_tag.clone(), chunk: l.clone() });
                        collected.push_str(&l);
                        collected.push('\n');
                    }
                    _ => err_done = true,
                },
                status = child.wait(), if out_done && err_done => {
                    break status;
                }
                () = &mut deadline => {
                    kill_tree(&mut child).await;
                    let out = truncate_output(&collected, 400, 40_000);
                    return ToolOutput::error(format!(
                        "command timed out after {}s\n{out}", timeout.as_secs()
                    ));
                }
                () = ctx.cancel.cancelled() => {
                    kill_tree(&mut child).await;
                    return ToolOutput::error("command cancelled by user");
                }
            }
        };

        let out = truncate_output(&collected, 400, 40_000);
        match status {
            Ok(s) if s.success() => ToolOutput::ok(if out.is_empty() {
                "(no output, exit 0)".into()
            } else {
                out
            }),
            Ok(s) => ToolOutput::error(format!("exit code {}\n{out}", s.code().unwrap_or(-1))),
            Err(e) => ToolOutput::error(format!("wait failed: {e}\n{out}")),
        }
    }
}

fn shell_command(command: &str) -> Command {
    #[cfg(unix)]
    {
        let mut cmd = Command::new("bash");
        cmd.arg("-c").arg(command);
        cmd
    }
    #[cfg(windows)]
    {
        // Prefer Git Bash when present; fall back to PowerShell.
        let git_bash = [
            "C:\\Program Files\\Git\\bin\\bash.exe",
            "C:\\Program Files (x86)\\Git\\bin\\bash.exe",
        ]
        .iter()
        .find(|p| std::path::Path::new(p).exists());
        match git_bash {
            Some(bash) => {
                let mut cmd = Command::new(bash);
                cmd.arg("-c").arg(command);
                cmd
            }
            None => {
                let mut cmd = Command::new("powershell.exe");
                cmd.arg("-NoProfile").arg("-Command").arg(command);
                cmd
            }
        }
    }
}

async fn kill_tree(child: &mut tokio::process::Child) {
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        // Negative pid = the whole process group we created with process_group(0).
        unsafe {
            libc::kill(-(pid as i32), libc::SIGKILL);
        }
    }
    let _ = child.kill().await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::Tool;
    use std::time::{Duration, Instant};

    // windows-latest CI runners have Git Bash, so unix-style commands work
    // on all three CI OSes through shell_command's resolution order.
    fn ctx() -> ToolCtx {
        ToolCtx {
            cwd: std::env::temp_dir(),
            events: crate::agent::events::EventSender::new(tokio::sync::broadcast::channel(64).0),
            cancel: Default::default(),
            depth: 0,
            router: Default::default(),
            lsp: None,
        }
    }

    #[tokio::test]
    async fn echo_round_trip() {
        let out = BashTool
            .run(serde_json::json!({ "command": "echo hi" }), &ctx())
            .await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("hi"));
    }

    #[tokio::test]
    async fn exit_code_propagates() {
        let out = BashTool
            .run(serde_json::json!({ "command": "exit 3" }), &ctx())
            .await;
        assert!(out.is_error);
        assert!(out.content.contains("exit code 3"), "{}", out.content);
    }

    #[tokio::test]
    async fn timeout_kills_the_tree() {
        let start = Instant::now();
        let out = BashTool
            .run(
                serde_json::json!({ "command": "sleep 30", "timeout_secs": 1 }),
                &ctx(),
            )
            .await;
        assert!(out.is_error);
        assert!(out.content.contains("timed out"), "{}", out.content);
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "kill took {:?}",
            start.elapsed()
        );
    }

    #[tokio::test]
    async fn cancellation_stops_the_command() {
        let ctx = ctx();
        let cancel = ctx.cancel.clone();
        let start = Instant::now();
        let run = BashTool.run(serde_json::json!({ "command": "sleep 30" }), &ctx);
        tokio::pin!(run);
        let out = tokio::select! {
            out = &mut run => out,
            () = async {
                tokio::time::sleep(Duration::from_millis(300)).await;
                cancel.cancel();
                std::future::pending::<()>().await
            } => unreachable!(),
        };
        assert!(out.is_error);
        assert!(out.content.contains("cancelled"), "{}", out.content);
        assert!(start.elapsed() < Duration::from_secs(10));
    }
}
