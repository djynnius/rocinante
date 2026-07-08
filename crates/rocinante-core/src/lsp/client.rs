//! One running language-server process: spawn, initialize handshake,
//! document sync, typed requests. One client per (server, workspace root).

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use lsp_types::notification::{
    DidChangeTextDocument, DidOpenTextDocument, DidSaveTextDocument, Notification,
    PublishDiagnostics,
};
use lsp_types::request::{Initialize, Request};
use lsp_types::{
    ClientCapabilities, ClientInfo, DidChangeTextDocumentParams, DidOpenTextDocumentParams,
    DidSaveTextDocumentParams, GeneralClientCapabilities, InitializeParams, InitializeResult,
    PositionEncodingKind, PublishDiagnosticsClientCapabilities, PublishDiagnosticsParams,
    ServerCapabilities, TextDocumentClientCapabilities, TextDocumentContentChangeEvent,
    TextDocumentIdentifier, TextDocumentItem, TextDocumentSyncClientCapabilities,
    VersionedTextDocumentIdentifier, WindowClientCapabilities, WorkspaceClientCapabilities,
    WorkspaceFolder,
};
use serde_json::{Value, json};

use super::diagnostics::DiagnosticsStore;
use super::transport::Transport;

const INIT_TIMEOUT: Duration = Duration::from_secs(20);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Negotiated position encoding: what `Position.character` counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    Utf8,
    Utf16,
}

pub struct LspClient {
    name: String,
    transport: Transport,
    child: tokio::sync::Mutex<tokio::process::Child>,
    diagnostics: Arc<DiagnosticsStore>,
    capabilities: ServerCapabilities,
    encoding: Encoding,
    /// Per-document version counters (didOpen = 1).
    versions: std::sync::Mutex<HashMap<String, i32>>,
}

impl LspClient {
    pub async fn spawn(
        name: &str,
        binary: &Path,
        args: &[String],
        env: &BTreeMap<String, String>,
        root: &Path,
    ) -> anyhow::Result<Self> {
        let mut child = tokio::process::Command::new(binary)
            .args(args)
            .envs(env)
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            // Backstop only; shutdown() is the graceful path.
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| anyhow::anyhow!("cannot spawn {}: {e}", binary.display()))?;
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");

        let diagnostics = Arc::new(DiagnosticsStore::default());
        let store = Arc::clone(&diagnostics);
        let transport = Transport::new(stdin, stdout, name.to_string(), move |method, params| {
            if method == PublishDiagnostics::METHOD {
                match serde_json::from_value::<PublishDiagnosticsParams>(params) {
                    Ok(p) => store.update(p.uri.as_str().to_string(), p.version, p.diagnostics),
                    Err(e) => tracing::debug!(error = %e, "bad publishDiagnostics payload"),
                }
            }
        });

        let root_uri = path_to_uri(root)?;
        #[allow(deprecated)] // root_uri: older servers still want it
        let params = InitializeParams {
            process_id: Some(std::process::id()),
            root_uri: Some(root_uri.clone()),
            workspace_folders: Some(vec![WorkspaceFolder {
                uri: root_uri,
                name: root
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "workspace".into()),
            }]),
            capabilities: client_capabilities(),
            client_info: Some(ClientInfo {
                name: "rocinante".into(),
                version: Some(env!("CARGO_PKG_VERSION").into()),
            }),
            ..Default::default()
        };
        let result = transport
            .request(
                Initialize::METHOD,
                serde_json::to_value(params)?,
                INIT_TIMEOUT,
            )
            .await?;
        let result: InitializeResult = serde_json::from_value(result)?;
        let encoding = match result
            .capabilities
            .position_encoding
            .as_ref()
            .map(PositionEncodingKind::as_str)
        {
            Some("utf-8") => Encoding::Utf8,
            None | Some("utf-16") => Encoding::Utf16,
            Some(other) => {
                tracing::warn!(
                    server = name,
                    encoding = other,
                    "unexpected position encoding; assuming utf-16"
                );
                Encoding::Utf16
            }
        };
        transport.notify("initialized", json!({})).await?;
        tracing::info!(server = name, root = %root.display(), ?encoding, "lsp server initialized");

        Ok(Self {
            name: name.to_string(),
            transport,
            child: tokio::sync::Mutex::new(child),
            diagnostics,
            capabilities: result.capabilities,
            encoding,
            versions: std::sync::Mutex::new(HashMap::new()),
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn capabilities(&self) -> &ServerCapabilities {
        &self.capabilities
    }

    pub fn encoding(&self) -> Encoding {
        self.encoding
    }

    pub fn diagnostics(&self) -> &DiagnosticsStore {
        &self.diagnostics
    }

    pub async fn request<R>(&self, params: R::Params) -> anyhow::Result<R::Result>
    where
        R: Request,
    {
        let value = self
            .transport
            .request(R::METHOD, serde_json::to_value(params)?, REQUEST_TIMEOUT)
            .await?;
        Ok(serde_json::from_value(value)?)
    }

    async fn notify<N>(&self, params: N::Params) -> anyhow::Result<()>
    where
        N: Notification,
    {
        self.transport
            .notify(N::METHOD, serde_json::to_value(params)?)
            .await
    }

    /// Push the document's current content to the server: didOpen the first
    /// time, then rangeless (full-text) didChange — valid regardless of the
    /// server's sync kind. didSave follows so save-triggered analyzers
    /// (rust-analyzer's cargo check) run. Returns the URI, the version to
    /// gate the diagnostics wait on, and the store generation from *before*
    /// the sync (for servers that publish without versions).
    pub async fn sync_document(
        &self,
        path: &Path,
        content: &str,
    ) -> anyhow::Result<(String, i32, u64)> {
        let uri = path_to_uri(path)?;
        let uri_str = uri.as_str().to_string();
        let generation = self.diagnostics.generation();
        let version = {
            let mut versions = self.versions.lock().unwrap();
            let v = versions.entry(uri_str.clone()).or_insert(0);
            *v += 1;
            *v
        };
        if version == 1 {
            self.notify::<DidOpenTextDocument>(DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: uri.clone(),
                    language_id: language_id(path).into(),
                    version,
                    text: content.into(),
                },
            })
            .await?;
        } else {
            self.notify::<DidChangeTextDocument>(DidChangeTextDocumentParams {
                text_document: VersionedTextDocumentIdentifier {
                    uri: uri.clone(),
                    version,
                },
                content_changes: vec![TextDocumentContentChangeEvent {
                    range: None,
                    range_length: None,
                    text: content.into(),
                }],
            })
            .await?;
        }
        self.notify::<DidSaveTextDocument>(DidSaveTextDocumentParams {
            text_document: TextDocumentIdentifier { uri },
            text: None,
        })
        .await?;
        Ok((uri_str, version, generation))
    }

    /// Graceful shutdown handshake (shutdown request, exit notification);
    /// kill is the backstop if the server ignores it.
    pub async fn shutdown(&self) {
        let _ = self
            .transport
            .request("shutdown", Value::Null, Duration::from_secs(2))
            .await;
        let _ = self.transport.notify("exit", Value::Null).await;
        let mut child = self.child.lock().await;
        if tokio::time::timeout(Duration::from_secs(2), child.wait())
            .await
            .is_err()
        {
            tracing::warn!(server = self.name, "lsp server ignored shutdown; killing");
            let _ = child.start_kill();
        }
    }
}

fn client_capabilities() -> ClientCapabilities {
    ClientCapabilities {
        general: Some(GeneralClientCapabilities {
            position_encodings: Some(vec![
                PositionEncodingKind::UTF8,
                PositionEncodingKind::UTF16,
            ]),
            ..Default::default()
        }),
        text_document: Some(TextDocumentClientCapabilities {
            publish_diagnostics: Some(PublishDiagnosticsClientCapabilities {
                version_support: Some(true),
                ..Default::default()
            }),
            synchronization: Some(TextDocumentSyncClientCapabilities {
                did_save: Some(true),
                ..Default::default()
            }),
            ..Default::default()
        }),
        window: Some(WindowClientCapabilities {
            work_done_progress: Some(true),
            ..Default::default()
        }),
        workspace: Some(WorkspaceClientCapabilities {
            configuration: Some(true),
            workspace_folders: Some(true),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn language_id(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "rs" => "rust",
        "ts" | "mts" | "cts" => "typescript",
        "tsx" => "typescriptreact",
        "js" | "mjs" | "cjs" => "javascript",
        "jsx" => "javascriptreact",
        "py" | "pyi" => "python",
        "go" => "go",
        _ => "plaintext",
    }
}

/// Canonicalize-then-convert path→URI. One shared helper because
/// percent-encoding differences are the classic way diagnostics never
/// match our documents.
pub fn path_to_uri(path: &Path) -> anyhow::Result<lsp_types::Uri> {
    let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    anyhow::ensure!(
        path.is_absolute(),
        "cannot convert relative path {} to a URI",
        path.display()
    );
    let mut out = String::from("file://");
    for &b in path.as_os_str().as_encoded_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    lsp_types::Uri::from_str(&out).map_err(|e| anyhow::anyhow!("bad uri {out}: {e}"))
}

pub fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let rest = rest.split(['?', '#']).next().unwrap_or(rest);
    if !rest.starts_with('/') {
        return None; // non-empty authority (file://host/...) unsupported
    }
    let bytes = rest.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    Some(PathBuf::from(String::from_utf8(out).ok()?))
}

/// 1-based line/column (column counts UTF-8 bytes) → LSP `Position` in the
/// negotiated encoding. Out-of-range values clamp.
pub fn to_lsp_position(
    content: &str,
    line: u32,
    column: u32,
    encoding: Encoding,
) -> lsp_types::Position {
    let line0 = line.saturating_sub(1);
    let text = content.lines().nth(line0 as usize).unwrap_or("");
    let mut byte = (column.saturating_sub(1) as usize).min(text.len());
    while byte > 0 && !text.is_char_boundary(byte) {
        byte -= 1;
    }
    let character = match encoding {
        Encoding::Utf8 => byte as u32,
        Encoding::Utf16 => text[..byte].encode_utf16().count() as u32,
    };
    lsp_types::Position {
        line: line0,
        character,
    }
}

/// LSP character offset on `line_text` → 1-based UTF-8 byte column.
pub fn from_lsp_character(line_text: &str, character: u32, encoding: Encoding) -> u32 {
    match encoding {
        Encoding::Utf8 => character.min(line_text.len() as u32) + 1,
        Encoding::Utf16 => {
            let mut units = 0u32;
            for (byte, ch) in line_text.char_indices() {
                if units >= character {
                    return byte as u32 + 1;
                }
                units += ch.len_utf16() as u32;
            }
            line_text.len() as u32 + 1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn position_conversion_utf16_vs_utf8_on_non_ascii_line() {
        // "é" is 2 UTF-8 bytes but 1 UTF-16 unit; "🦀" is 4 bytes / 2 units.
        let content = "let é🦀 = 1;\nnext";
        // Column 12 (1-based, bytes) points at '='.
        assert_eq!(content.lines().next().unwrap().as_bytes()[11], b'=');
        let utf8 = to_lsp_position(content, 1, 12, Encoding::Utf8);
        assert_eq!((utf8.line, utf8.character), (0, 11));
        let utf16 = to_lsp_position(content, 1, 12, Encoding::Utf16);
        assert_eq!((utf16.line, utf16.character), (0, 8));

        // Round-trip back to 1-based byte columns.
        let line = content.lines().next().unwrap();
        assert_eq!(from_lsp_character(line, utf8.character, Encoding::Utf8), 12);
        assert_eq!(
            from_lsp_character(line, utf16.character, Encoding::Utf16),
            12
        );
    }

    #[test]
    fn position_clamps_out_of_range() {
        let p = to_lsp_position("ab\n", 1, 99, Encoding::Utf16);
        assert_eq!((p.line, p.character), (0, 2));
        let p = to_lsp_position("ab\n", 9, 1, Encoding::Utf16);
        assert_eq!((p.line, p.character), (8, 0));
        assert_eq!(from_lsp_character("ab", 99, Encoding::Utf16), 3);
    }

    #[test]
    fn uri_round_trip_with_spaces() {
        // Path does not exist, so canonicalize falls back to the raw path.
        let path = Path::new("/tmp/my project/src/lib.rs");
        let uri = path_to_uri(path).unwrap();
        assert_eq!(uri.as_str(), "file:///tmp/my%20project/src/lib.rs");
        assert_eq!(uri_to_path(uri.as_str()).unwrap(), path);
    }

    #[test]
    fn uri_rejects_relative_and_foreign() {
        assert!(path_to_uri(Path::new("no/anchor.rs")).is_err());
        assert_eq!(uri_to_path("https://example.com/x"), None);
        assert_eq!(uri_to_path("file://host/x"), None);
    }
}
