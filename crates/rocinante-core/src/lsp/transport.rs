//! Content-Length-framed JSON-RPC 2.0 over a language server's stdio.
//! Hand-rolled, matching the providers' philosophy: the client surface is
//! ~5 requests and 3 notifications, and LSP frameworks want to own the
//! event loop. The reader task MUST answer server→client requests
//! (`workspace/configuration` and friends) or rust-analyzer stalls before
//! publishing anything.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{ChildStdin, ChildStdout};
use tokio::sync::{Mutex, oneshot};

/// JSON-RPC error object from a response.
#[derive(Debug)]
struct RpcError {
    code: i64,
    message: String,
}

type Pending = Arc<std::sync::Mutex<HashMap<i64, oneshot::Sender<Result<Value, RpcError>>>>>;
type NotifyHandler = Box<dyn Fn(&str, Value) + Send + Sync>;

pub struct Transport {
    writer: Arc<Mutex<ChildStdin>>,
    pending: Pending,
    next_id: AtomicI64,
}

impl Transport {
    /// Wire up the child's stdio and start the reader task.
    /// `on_notification(method, params)` is called for every server→client
    /// notification.
    pub fn new(
        stdin: ChildStdin,
        stdout: ChildStdout,
        server: String,
        on_notification: impl Fn(&str, Value) + Send + Sync + 'static,
    ) -> Self {
        let writer = Arc::new(Mutex::new(stdin));
        let pending: Pending = Arc::default();
        tokio::spawn(read_loop(
            stdout,
            Arc::clone(&writer),
            Arc::clone(&pending),
            server,
            Box::new(on_notification),
        ));
        Self {
            writer,
            pending,
            next_id: AtomicI64::new(1),
        }
    }

    pub async fn request(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> anyhow::Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        let msg = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        if let Err(e) = write_message(&self.writer, &msg).await {
            self.pending.lock().unwrap().remove(&id);
            return Err(e);
        }
        match tokio::time::timeout(timeout, rx).await {
            Err(_) => {
                self.pending.lock().unwrap().remove(&id);
                anyhow::bail!("{method} timed out after {}s", timeout.as_secs())
            }
            Ok(Err(_)) => anyhow::bail!("server closed the connection during {method}"),
            Ok(Ok(Err(e))) => anyhow::bail!("{method} failed: {} (code {})", e.message, e.code),
            Ok(Ok(Ok(result))) => Ok(result),
        }
    }

    pub async fn notify(&self, method: &str, params: Value) -> anyhow::Result<()> {
        let msg = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        write_message(&self.writer, &msg).await
    }
}

async fn write_message(writer: &Mutex<ChildStdin>, msg: &Value) -> anyhow::Result<()> {
    let body = serde_json::to_vec(msg)?;
    let mut w = writer.lock().await;
    w.write_all(format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes())
        .await?;
    w.write_all(&body).await?;
    w.flush().await?;
    Ok(())
}

async fn read_loop(
    mut stdout: ChildStdout,
    writer: Arc<Mutex<ChildStdin>>,
    pending: Pending,
    server: String,
    on_notification: NotifyHandler,
) {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 16 * 1024];
    'read: loop {
        match stdout.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
        loop {
            match extract_message(&mut buf) {
                Ok(Some(msg)) => dispatch(msg, &writer, &pending, &server, &on_notification).await,
                Ok(None) => break,
                Err(e) => {
                    tracing::warn!(server, error = %e, "lsp framing error; closing connection");
                    break 'read;
                }
            }
        }
    }
    // Server gone: dropping the senders fails every in-flight request.
    pending.lock().unwrap().clear();
    tracing::debug!(server, "lsp reader task ended");
}

async fn dispatch(
    msg: Value,
    writer: &Mutex<ChildStdin>,
    pending: &Pending,
    server: &str,
    on_notification: &(dyn Fn(&str, Value) + Send + Sync),
) {
    let method = msg.get("method").and_then(Value::as_str);
    let id = msg.get("id").cloned();
    match (method, id) {
        // Server→client request: must be answered or the server stalls.
        (Some(method), Some(id)) => {
            let params = msg.get("params").unwrap_or(&Value::Null);
            let reply = match answer_server_request(method, params) {
                Some(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
                None => json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": { "code": -32601, "message": format!("method not supported: {method}") }
                }),
            };
            if let Err(e) = write_message(writer, &reply).await {
                tracing::debug!(server, error = %e, "lsp reply write failed");
            }
        }
        (Some(method), None) => {
            on_notification(method, msg.get("params").cloned().unwrap_or(Value::Null));
        }
        (None, Some(id)) => {
            let Some(id) = id.as_i64() else { return };
            let Some(tx) = pending.lock().unwrap().remove(&id) else {
                return;
            };
            let outcome = match msg.get("error") {
                Some(err) => Err(RpcError {
                    code: err.get("code").and_then(Value::as_i64).unwrap_or(0),
                    message: err
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown error")
                        .to_string(),
                }),
                None => Ok(msg.get("result").cloned().unwrap_or(Value::Null)),
            };
            let _ = tx.send(outcome);
        }
        (None, None) => {}
    }
}

/// Canned answers for server→client requests. `workspace/configuration`
/// gets one null per requested item ("use your defaults"); registrations
/// and progress-token creation are acknowledged with null. Anything else
/// gets a MethodNotFound error (None).
fn answer_server_request(method: &str, params: &Value) -> Option<Value> {
    match method {
        "workspace/configuration" => {
            let n = params
                .get("items")
                .and_then(Value::as_array)
                .map_or(0, Vec::len);
            Some(Value::Array(vec![Value::Null; n]))
        }
        "client/registerCapability"
        | "client/unregisterCapability"
        | "window/workDoneProgress/create"
        | "window/showMessageRequest"
        | "workspace/semanticTokens/refresh"
        | "workspace/inlayHint/refresh"
        | "workspace/codeLens/refresh" => Some(Value::Null),
        _ => None,
    }
}

/// Pop one complete framed message off the front of `buf`, if present.
fn extract_message(buf: &mut Vec<u8>) -> anyhow::Result<Option<Value>> {
    let Some(header_end) = find(buf, b"\r\n\r\n") else {
        return Ok(None);
    };
    let mut content_length = None;
    for line in std::str::from_utf8(&buf[..header_end])?.split("\r\n") {
        if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            content_length = Some(value.trim().parse::<usize>()?);
        }
    }
    let len = content_length.ok_or_else(|| anyhow::anyhow!("missing Content-Length header"))?;
    let body_start = header_end + 4;
    if buf.len() < body_start + len {
        return Ok(None);
    }
    let msg = serde_json::from_slice(&buf[body_start..body_start + len])?;
    buf.drain(..body_start + len);
    Ok(Some(msg))
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(v: &Value) -> Vec<u8> {
        let body = serde_json::to_vec(v).unwrap();
        let mut out = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
        out.extend(body);
        out
    }

    #[test]
    fn two_messages_in_one_buffer() {
        let a = json!({ "jsonrpc": "2.0", "id": 1, "result": "one" });
        let b = json!({ "jsonrpc": "2.0", "method": "n", "params": [2] });
        let mut buf = frame(&a);
        buf.extend(frame(&b));
        assert_eq!(extract_message(&mut buf).unwrap(), Some(a));
        assert_eq!(extract_message(&mut buf).unwrap(), Some(b));
        assert!(buf.is_empty());
        assert_eq!(extract_message(&mut buf).unwrap(), None);
    }

    #[test]
    fn partial_buffer_waits_for_the_rest() {
        let msg = json!({ "jsonrpc": "2.0", "id": 7, "result": { "k": "vvvv" } });
        let full = frame(&msg);
        // Feed byte-by-byte: never a false parse, exactly one message out.
        let mut buf = Vec::new();
        let mut got = Vec::new();
        for byte in full {
            buf.push(byte);
            if let Some(m) = extract_message(&mut buf).unwrap() {
                got.push(m);
            }
        }
        assert_eq!(got, vec![msg]);
        assert!(buf.is_empty());
    }

    #[test]
    fn extra_headers_are_tolerated() {
        let msg = json!({ "ok": true });
        let body = serde_json::to_vec(&msg).unwrap();
        let mut buf = format!(
            "Content-Type: application/vscode-jsonrpc\r\ncontent-length: {}\r\n\r\n",
            body.len()
        )
        .into_bytes();
        buf.extend(body);
        assert_eq!(extract_message(&mut buf).unwrap(), Some(msg));
    }

    #[test]
    fn workspace_configuration_answers_null_per_item() {
        let params = json!({ "items": [{ "section": "rust-analyzer" }, { "section": "x" }] });
        let answer = answer_server_request("workspace/configuration", &params).unwrap();
        assert_eq!(answer, json!([null, null]));
        assert_eq!(
            answer_server_request("window/workDoneProgress/create", &json!({})),
            Some(Value::Null)
        );
        assert_eq!(
            answer_server_request("window/showDocument", &json!({})),
            None
        );
    }
}
