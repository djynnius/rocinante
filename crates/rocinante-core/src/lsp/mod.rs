//! LSP integration: session-lifetime language-server clients spawned
//! lazily per (server, workspace root), post-edit diagnostics appended to
//! edit/write results, and one `lsp` tool. Mirrors the MCP manager pattern:
//! external processes degrade to warnings, never failures.

mod client;
mod diagnostics;
mod tool;
mod transport;

pub use client::LspClient;
pub use tool::LspTool;

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crate::config::{Config, LspServerConfig};
use diagnostics::{PENDING_NOTE, format_diagnostics};

/// Post-edit diagnostics wait cap: a slow first index must not stall edits.
const DIAGNOSTICS_WAIT: Duration = Duration::from_secs(3);
/// Concurrent language servers cap — rust-analyzer RSS is real on a
/// machine also running a 30B model.
const MAX_CLIENTS: usize = 4;

/// A resolved server definition (builtin defaults merged with `[lsp.*]`).
#[derive(Debug, Clone)]
struct ServerDef {
    name: String,
    /// Candidate commands in priority order; the first one on PATH wins
    /// (basedpyright falls back to pyright).
    commands: Vec<(String, Vec<String>)>,
    /// File extensions (no dot) routed to this server.
    filetypes: Vec<String>,
    /// Files/dirs whose presence, walking upward, marks a workspace root.
    root_markers: Vec<String>,
    env: BTreeMap<String, String>,
}

fn builtin_servers() -> Vec<ServerDef> {
    fn def(
        name: &str,
        commands: &[(&str, &[&str])],
        filetypes: &[&str],
        root_markers: &[&str],
    ) -> ServerDef {
        ServerDef {
            name: name.into(),
            commands: commands
                .iter()
                .map(|(c, a)| (c.to_string(), a.iter().map(|s| s.to_string()).collect()))
                .collect(),
            filetypes: filetypes.iter().map(|s| s.to_string()).collect(),
            root_markers: root_markers.iter().map(|s| s.to_string()).collect(),
            env: BTreeMap::new(),
        }
    }
    vec![
        def("rust", &[("rust-analyzer", &[])], &["rs"], &["Cargo.toml"]),
        def(
            "typescript",
            &[("typescript-language-server", &["--stdio"])],
            &["ts", "tsx", "js", "jsx", "mts", "cts", "mjs", "cjs"],
            &["tsconfig.json", "package.json"],
        ),
        def(
            "python",
            &[
                ("basedpyright-langserver", &["--stdio"]),
                ("pyright-langserver", &["--stdio"]),
            ],
            &["py", "pyi"],
            &[
                "pyproject.toml",
                "setup.py",
                "setup.cfg",
                "requirements.txt",
            ],
        ),
        def("go", &[("gopls", &[])], &["go"], &["go.mod", "go.work"]),
    ]
}

/// Merge `[lsp.<name>]` config over the builtins: same-key entries override
/// field-by-field (empty lists fall back to the builtin), `disabled = true`
/// opts out, new keys add servers.
fn resolve_servers(config: &BTreeMap<String, LspServerConfig>) -> Vec<ServerDef> {
    let builtins = builtin_servers();
    let builtin_names: Vec<String> = builtins.iter().map(|s| s.name.clone()).collect();
    let mut out = Vec::new();
    for builtin in builtins {
        match config.get(&builtin.name) {
            None => out.push(builtin),
            Some(user) if user.disabled => {}
            Some(user) => {
                let mut merged = builtin;
                if let Some(command) = &user.command {
                    merged.commands = vec![(command.clone(), user.args.clone())];
                }
                if !user.filetypes.is_empty() {
                    merged.filetypes = user.filetypes.clone();
                }
                if !user.root_markers.is_empty() {
                    merged.root_markers = user.root_markers.clone();
                }
                merged.env.extend(user.env.clone());
                out.push(merged);
            }
        }
    }
    for (name, user) in config {
        if user.disabled || builtin_names.contains(name) {
            continue;
        }
        let Some(command) = &user.command else {
            tracing::warn!(server = name, "[lsp.{name}] has no command; skipping");
            continue;
        };
        if user.filetypes.is_empty() {
            tracing::warn!(
                server = name,
                "[lsp.{name}] has no filetypes to route; skipping"
            );
            continue;
        }
        let root_markers = if user.root_markers.is_empty() {
            vec![".git".into()]
        } else {
            user.root_markers.clone()
        };
        out.push(ServerDef {
            name: name.clone(),
            commands: vec![(command.clone(), user.args.clone())],
            filetypes: user.filetypes.clone(),
            root_markers,
            env: user.env.clone(),
        });
    }
    out
}

/// Owns the running language-server clients for the session, spawning them
/// lazily on first use. Construction is cheap — binaries and workspace
/// roots are checked at spawn time, not startup.
pub struct LspManager {
    servers: Vec<ServerDef>,
    clients: tokio::sync::Mutex<HashMap<(String, PathBuf), Arc<LspClient>>>,
}

impl LspManager {
    pub fn new(config: &Config) -> Self {
        Self {
            servers: resolve_servers(&config.lsp),
            clients: tokio::sync::Mutex::new(HashMap::new()),
        }
    }

    /// True when some enabled server's binary is on PATH — whether the
    /// `lsp` tool is worth its schema cost in context.
    pub fn any_available(&self) -> bool {
        self.servers
            .iter()
            .any(|s| s.commands.iter().any(|(c, _)| find_binary(c).is_some()))
    }

    /// Lazily spawned client for this file, or None when no configured
    /// server applies (wrong filetype, no root marker, binary missing).
    pub async fn client_for(&self, path: &Path) -> anyhow::Result<Option<Arc<LspClient>>> {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .unwrap_or_default();
        let Some(def) = self.servers.iter().find(|s| s.filetypes.contains(&ext)) else {
            return Ok(None);
        };
        let Some(root) = find_root(path, &def.root_markers) else {
            tracing::debug!(server = def.name, path = %path.display(), "no root marker; lsp inactive");
            return Ok(None);
        };
        let mut clients = self.clients.lock().await;
        if let Some(client) = clients.get(&(def.name.clone(), root.clone())) {
            return Ok(Some(Arc::clone(client)));
        }
        if clients.len() >= MAX_CLIENTS {
            tracing::warn!(
                server = def.name,
                "lsp client cap ({MAX_CLIENTS}) reached; not spawning another"
            );
            return Ok(None);
        }
        let Some((binary, args)) = def
            .commands
            .iter()
            .find_map(|(c, a)| find_binary(c).map(|b| (b, a.clone())))
        else {
            tracing::debug!(server = def.name, "lsp binary not on PATH; lsp inactive");
            return Ok(None);
        };
        tracing::info!(server = def.name, binary = %binary.display(), root = %root.display(), "spawning lsp server");
        let client = Arc::new(LspClient::spawn(&def.name, &binary, &args, &def.env, &root).await?);
        clients.insert((def.name.clone(), root), Arc::clone(&client));
        Ok(Some(client))
    }

    /// Post-edit hook: sync the new content, wait (bounded) for fresh
    /// diagnostics, render them for the tool result. None = no server
    /// applies or it failed — degrade silently, the edit itself succeeded.
    pub async fn diagnostics_after_change(&self, path: &Path, content: &str) -> Option<String> {
        let client = match self.client_for(path).await {
            Ok(Some(c)) => c,
            Ok(None) => return None,
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "lsp unavailable; skipping post-edit diagnostics");
                return None;
            }
        };
        let (uri, version, generation) = match client.sync_document(path, content).await {
            Ok(x) => x,
            Err(e) => {
                tracing::warn!(error = %e, "lsp document sync failed");
                return None;
            }
        };
        match client
            .diagnostics()
            .wait_for(&uri, version, generation, DIAGNOSTICS_WAIT, version == 1)
            .await
        {
            Some(diags) => Some(format_diagnostics(
                &path.display().to_string(),
                content,
                &diags,
                client.encoding(),
            )),
            None => Some(PENDING_NOTE.into()),
        }
    }

    /// Graceful shutdown of every running server: shutdown request, exit
    /// notification, kill backstop. Leaves no orphan processes.
    pub async fn shutdown(&self) {
        let clients: Vec<_> = self.clients.lock().await.drain().collect();
        for ((name, root), client) in clients {
            tracing::debug!(server = name, root = %root.display(), "lsp shutdown");
            client.shutdown().await;
        }
    }
}

fn find_root(path: &Path, markers: &[String]) -> Option<PathBuf> {
    let mut dir = path.parent()?;
    loop {
        if markers.iter().any(|m| dir.join(m).exists()) {
            return Some(dir.to_path_buf());
        }
        dir = dir.parent()?;
    }
}

/// PATH lookup; explicit paths (containing a separator) checked directly.
fn find_binary(command: &str) -> Option<PathBuf> {
    if command.contains(std::path::MAIN_SEPARATOR) {
        let p = PathBuf::from(command);
        return p.is_file().then_some(p);
    }
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(command))
        .find(|p| p.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_present_by_default() {
        let servers = resolve_servers(&BTreeMap::new());
        let names: Vec<&str> = servers.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["rust", "typescript", "python", "go"]);
        assert_eq!(servers[0].commands[0].0, "rust-analyzer");
        assert_eq!(servers[0].root_markers, ["Cargo.toml"]);
    }

    #[test]
    fn disabled_removes_a_builtin() {
        let mut cfg = BTreeMap::new();
        cfg.insert(
            "rust".to_string(),
            LspServerConfig {
                disabled: true,
                ..Default::default()
            },
        );
        let servers = resolve_servers(&cfg);
        assert!(!servers.iter().any(|s| s.name == "rust"));
        assert!(servers.iter().any(|s| s.name == "go"));
    }

    #[test]
    fn override_keeps_unset_builtin_fields() {
        let mut cfg = BTreeMap::new();
        cfg.insert(
            "rust".to_string(),
            LspServerConfig {
                command: Some("ra-custom".into()),
                args: vec!["--flag".into()],
                ..Default::default()
            },
        );
        let servers = resolve_servers(&cfg);
        let rust = servers.iter().find(|s| s.name == "rust").unwrap();
        assert_eq!(
            rust.commands,
            [("ra-custom".to_string(), vec!["--flag".to_string()])]
        );
        // Filetypes and root markers fall back to the builtin.
        assert_eq!(rust.filetypes, ["rs"]);
        assert_eq!(rust.root_markers, ["Cargo.toml"]);
    }

    #[test]
    fn custom_server_needs_command_and_filetypes() {
        let mut cfg = BTreeMap::new();
        cfg.insert(
            "zig".to_string(),
            LspServerConfig {
                command: Some("zls".into()),
                filetypes: vec!["zig".into()],
                ..Default::default()
            },
        );
        cfg.insert("broken".to_string(), LspServerConfig::default());
        let servers = resolve_servers(&cfg);
        let zig = servers.iter().find(|s| s.name == "zig").unwrap();
        assert_eq!(zig.root_markers, [".git"]); // sensible default
        assert!(!servers.iter().any(|s| s.name == "broken"));
    }

    #[test]
    fn root_marker_walk() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src/nested")).unwrap();
        std::fs::write(root.join("Cargo.toml"), "").unwrap();
        let file = root.join("src/nested/main.rs");
        assert_eq!(
            find_root(&file, &["Cargo.toml".into()]),
            Some(root.to_path_buf())
        );
        assert_eq!(find_root(&file, &["go.mod".into()]), None);
    }
}
