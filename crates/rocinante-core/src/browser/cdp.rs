//! A small Chrome DevTools Protocol client over one WebSocket: each command
//! gets an id and waits on a oneshot for its answer; events fan out on a
//! broadcast channel. Tabs are flattened sessions, addressed by `sessionId`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::stream::SplitSink;
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio::sync::{broadcast, oneshot};
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

/// How long one command may take (a full-page screenshot is the slowest).
const CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// A protocol event.
#[derive(Debug, Clone)]
pub struct Event {
    pub method: String,
    pub params: Value,
    /// The tab session it came from; None for browser-level events.
    pub session: Option<String>,
}

type Reply = Result<Value, String>;
type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Reply>>>>;
type Sink = SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, WsMessage>;

pub struct Connection {
    next_id: AtomicU64,
    pending: Pending,
    sink: tokio::sync::Mutex<Sink>,
    events: broadcast::Sender<Event>,
    closed: Arc<AtomicBool>,
    reader: tokio::task::JoinHandle<()>,
}

impl Connection {
    /// Connect to a browser's DevTools WebSocket (`ws://127.0.0.1:…`).
    pub async fn connect(url: &str) -> Result<Arc<Self>, String> {
        let (ws, _) = tokio::time::timeout(
            Duration::from_secs(5),
            tokio_tungstenite::connect_async(url),
        )
        .await
        .map_err(|_| "the browser didn't answer".to_string())?
        .map_err(|e| format!("couldn't connect to the browser: {e}"))?;
        let (sink, mut stream) = ws.split();
        let pending: Pending = Arc::default();
        let (events, _) = broadcast::channel(1024);
        let closed = Arc::new(AtomicBool::new(false));
        let reader = {
            let pending = Arc::clone(&pending);
            let events = events.clone();
            let closed = Arc::clone(&closed);
            tokio::spawn(async move {
                while let Some(msg) = stream.next().await {
                    match msg {
                        Ok(WsMessage::Text(text)) => dispatch(text.as_str(), &pending, &events),
                        Ok(WsMessage::Close(_)) | Err(_) => break,
                        Ok(_) => {}
                    }
                }
                closed.store(true, Ordering::SeqCst);
                for (_, tx) in pending.lock().unwrap().drain() {
                    let _ = tx.send(Err("the browser closed".into()));
                }
            })
        };
        Ok(Arc::new(Self {
            next_id: AtomicU64::new(1),
            pending,
            sink: tokio::sync::Mutex::new(sink),
            events,
            closed,
            reader,
        }))
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.events.subscribe()
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    /// Send a command and wait for its result.
    pub async fn call(&self, method: &str, params: Value, session: Option<&str>) -> Reply {
        if self.is_closed() {
            return Err("the browser closed".into());
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        let mut msg = json!({ "id": id, "method": method, "params": params });
        if let Some(s) = session {
            msg["sessionId"] = json!(s);
        }
        let sent = self
            .sink
            .lock()
            .await
            .send(WsMessage::text(msg.to_string()))
            .await;
        if let Err(e) = sent {
            self.pending.lock().unwrap().remove(&id);
            return Err(format!("the browser connection failed: {e}"));
        }
        match tokio::time::timeout(CALL_TIMEOUT, rx).await {
            Ok(Ok(reply)) => reply,
            Ok(Err(_)) => Err("the browser closed".into()),
            Err(_) => {
                self.pending.lock().unwrap().remove(&id);
                Err(format!("the browser didn't answer {method} in time"))
            }
        }
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.reader.abort();
    }
}

/// Route one incoming message: an answer to its waiting caller, an event to
/// the subscribers.
fn dispatch(text: &str, pending: &Pending, events: &broadcast::Sender<Event>) {
    let Ok(msg) = serde_json::from_str::<Value>(text) else {
        return;
    };
    if let Some(id) = msg.get("id").and_then(Value::as_u64) {
        let reply = match msg.get("error") {
            Some(err) => Err(err
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("unknown error")
                .to_string()),
            None => Ok(msg.get("result").cloned().unwrap_or(Value::Null)),
        };
        if let Some(tx) = pending.lock().unwrap().remove(&id) {
            let _ = tx.send(reply);
        }
        return;
    }
    if let Some(method) = msg.get("method").and_then(Value::as_str) {
        let _ = events.send(Event {
            method: method.to_string(),
            params: msg.get("params").cloned().unwrap_or(Value::Null),
            session: msg
                .get("sessionId")
                .and_then(Value::as_str)
                .map(String::from),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_reach_their_caller_and_events_fan_out() {
        let pending: Pending = Arc::default();
        let (events, mut rx) = broadcast::channel(8);
        let (tx, mut answer) = oneshot::channel();
        pending.lock().unwrap().insert(7, tx);
        dispatch(r#"{"id":7,"result":{"ok":1}}"#, &pending, &events);
        assert_eq!(answer.try_recv().unwrap().unwrap()["ok"], 1);

        let (tx, mut answer) = oneshot::channel();
        pending.lock().unwrap().insert(8, tx);
        dispatch(
            r#"{"id":8,"error":{"code":-32000,"message":"No node"}}"#,
            &pending,
            &events,
        );
        assert_eq!(answer.try_recv().unwrap().unwrap_err(), "No node");

        dispatch(
            r#"{"method":"Page.loadEventFired","params":{"timestamp":1},"sessionId":"S"}"#,
            &pending,
            &events,
        );
        let ev = rx.try_recv().unwrap();
        assert_eq!(ev.method, "Page.loadEventFired");
        assert_eq!(ev.session.as_deref(), Some("S"));
        // Junk is ignored.
        dispatch("not json", &pending, &events);
    }
}
