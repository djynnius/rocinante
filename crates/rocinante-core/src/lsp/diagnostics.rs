//! Per-URI diagnostics store fed by `textDocument/publishDiagnostics`,
//! with a version-gated bounded wait so post-edit hooks report diagnostics
//! for *their* edit — stale results misattributed as "my edit failed" are
//! worse than none.

use std::collections::HashMap;
use std::time::Duration;

use lsp_types::{Diagnostic, DiagnosticSeverity};
use tokio::sync::watch;

use super::client::{Encoding, from_lsp_character};

/// Settle time after an acceptable publish: servers often publish an empty
/// set immediately and the real analysis moments later.
const DEBOUNCE: Duration = Duration::from_millis(250);
/// Most items shown in a formatted result; the rest become a count.
const MAX_ITEMS: usize = 10;

pub const PENDING_NOTE: &str = "(diagnostics pending — server still analyzing; use the lsp tool with action=diagnostics to re-check)";

struct Published {
    version: Option<i32>,
    diagnostics: Vec<Diagnostic>,
    generation: u64,
}

struct Inner {
    by_uri: HashMap<String, Published>,
    generation: u64,
}

pub struct DiagnosticsStore {
    inner: std::sync::Mutex<Inner>,
    tx: watch::Sender<u64>,
}

impl Default for DiagnosticsStore {
    fn default() -> Self {
        Self {
            inner: std::sync::Mutex::new(Inner {
                by_uri: HashMap::new(),
                generation: 0,
            }),
            tx: watch::channel(0).0,
        }
    }
}

impl DiagnosticsStore {
    /// Monotonic publish counter; capture before a change to detect
    /// publishes that happened after it.
    pub fn generation(&self) -> u64 {
        self.inner.lock().unwrap().generation
    }

    pub fn update(&self, uri: String, version: Option<i32>, diagnostics: Vec<Diagnostic>) {
        let generation = {
            let mut inner = self.inner.lock().unwrap();
            inner.generation += 1;
            let generation = inner.generation;
            inner.by_uri.insert(
                uri,
                Published {
                    version,
                    diagnostics,
                    generation,
                },
            );
            generation
        };
        let _ = self.tx.send(generation);
    }

    /// Last published diagnostics for a URI, fresh or not.
    pub fn get(&self, uri: &str) -> Option<Vec<Diagnostic>> {
        self.inner
            .lock()
            .unwrap()
            .by_uri
            .get(uri)
            .map(|p| p.diagnostics.clone())
    }

    /// Wait (bounded by `cap`) for a publish reflecting at least document
    /// version `min_version`. Servers that omit versions are accepted on
    /// the first publish after `since_generation`. A short debounce absorbs
    /// the empty-then-real double publish. With `distrust_empty` (a
    /// document's first sync, when the server may still be indexing), empty
    /// sets never resolve the wait: a cold server publishes an empty set
    /// before the real analysis lands, and a false "no errors" is worse
    /// than a pending note. None = nothing trustworthy arrived.
    pub async fn wait_for(
        &self,
        uri: &str,
        min_version: i32,
        since_generation: u64,
        cap: Duration,
        distrust_empty: bool,
    ) -> Option<Vec<Diagnostic>> {
        let deadline = tokio::time::Instant::now() + cap;
        let mut rx = self.tx.subscribe();
        let mut hit: Option<Vec<Diagnostic>> = None;
        let mut settle: Option<tokio::time::Instant> = None;
        loop {
            {
                let inner = self.inner.lock().unwrap();
                if let Some(p) = inner.by_uri.get(uri) {
                    let fresh = match p.version {
                        Some(v) => v >= min_version,
                        None => p.generation > since_generation,
                    };
                    if fresh && !(distrust_empty && p.diagnostics.is_empty()) {
                        hit = Some(p.diagnostics.clone());
                        settle.get_or_insert(tokio::time::Instant::now() + DEBOUNCE);
                    }
                }
            }
            let wake = settle.map_or(deadline, |s| s.min(deadline));
            tokio::select! {
                _ = tokio::time::sleep_until(wake) => return hit,
                changed = rx.changed() => {
                    if changed.is_err() {
                        return hit;
                    }
                }
            }
        }
    }
}

/// Render diagnostics for a tool result: errors then warnings,
/// `path:line:col severity message`, capped with counts. `content` is the
/// document text, needed to convert encoded columns back to byte columns.
pub fn format_diagnostics(
    display_path: &str,
    content: &str,
    diagnostics: &[Diagnostic],
    encoding: Encoding,
) -> String {
    // Missing severity means error per the LSP spec's guidance to clients.
    let mut errors: Vec<&Diagnostic> = diagnostics
        .iter()
        .filter(|d| d.severity.is_none() || d.severity == Some(DiagnosticSeverity::ERROR))
        .collect();
    let mut warnings: Vec<&Diagnostic> = diagnostics
        .iter()
        .filter(|d| d.severity == Some(DiagnosticSeverity::WARNING))
        .collect();
    if errors.is_empty() && warnings.is_empty() {
        return "diagnostics: no errors or warnings".into();
    }
    let key = |d: &&Diagnostic| (d.range.start.line, d.range.start.character);
    errors.sort_by_key(key);
    warnings.sort_by_key(key);

    let total = errors.len() + warnings.len();
    let mut out = format!(
        "diagnostics: {} error(s), {} warning(s)",
        errors.len(),
        warnings.len()
    );
    let lines: Vec<&str> = content.lines().collect();
    for (shown, (severity, d)) in errors
        .iter()
        .map(|d| ("error", *d))
        .chain(warnings.iter().map(|d| ("warning", *d)))
        .enumerate()
    {
        if shown == MAX_ITEMS {
            out.push_str(&format!("\n… {} more not shown", total - MAX_ITEMS));
            break;
        }
        let line0 = d.range.start.line as usize;
        let col = lines.get(line0).map_or(d.range.start.character + 1, |l| {
            from_lsp_character(l, d.range.start.character, encoding)
        });
        out.push_str(&format!(
            "\n{display_path}:{}:{} {severity} {}",
            line0 + 1,
            col,
            d.message.replace('\n', " ")
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use lsp_types::{Position, Range};

    fn diag(line: u32, severity: DiagnosticSeverity, message: &str) -> Diagnostic {
        Diagnostic {
            range: Range {
                start: Position { line, character: 0 },
                end: Position { line, character: 1 },
            },
            severity: Some(severity),
            message: message.into(),
            ..Default::default()
        }
    }

    #[test]
    fn formatting_orders_caps_and_counts() {
        let mut diags: Vec<Diagnostic> = (0..8)
            .map(|i| diag(20 + i, DiagnosticSeverity::WARNING, &format!("warn {i}")))
            .collect();
        for i in 0..7 {
            diags.push(diag(i, DiagnosticSeverity::ERROR, &format!("err {i}")));
        }
        diags.push(diag(50, DiagnosticSeverity::HINT, "hint ignored"));
        let out = format_diagnostics("src/lib.rs", "", &diags, Encoding::Utf16);

        assert!(out.starts_with("diagnostics: 7 error(s), 8 warning(s)"));
        assert!(out.contains("… 5 more not shown"));
        assert!(!out.contains("hint ignored"));
        // Errors come first even though warnings were pushed first.
        let body: Vec<&str> = out.lines().skip(1).collect();
        assert!(body[0].contains("src/lib.rs:1:1 error err 0"));
        assert_eq!(body.iter().filter(|l| l.contains(" error ")).count(), 7);
        assert_eq!(body.iter().filter(|l| l.contains(" warning ")).count(), 3);
    }

    #[test]
    fn clean_and_pending_wording() {
        let out = format_diagnostics("x.rs", "", &[], Encoding::Utf16);
        assert_eq!(out, "diagnostics: no errors or warnings");
        assert!(PENDING_NOTE.contains("diagnostics pending"));
        assert!(PENDING_NOTE.contains("action=diagnostics"));
    }

    #[tokio::test]
    async fn wait_is_version_gated() {
        let store = std::sync::Arc::new(DiagnosticsStore::default());
        store.update(
            "u".into(),
            Some(1),
            vec![diag(0, DiagnosticSeverity::ERROR, "old")],
        );
        let generation = store.generation();

        // Only a stale publish (version 1) exists: a wait for version 2
        // must time out rather than misattribute it.
        let stale = store
            .wait_for("u", 2, generation, Duration::from_millis(100), false)
            .await;
        assert!(stale.is_none());

        let publisher = std::sync::Arc::clone(&store);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            publisher.update(
                "u".into(),
                Some(2),
                vec![diag(0, DiagnosticSeverity::ERROR, "new")],
            );
        });
        let got = store
            .wait_for("u", 2, generation, Duration::from_secs(2), false)
            .await
            .expect("fresh publish accepted");
        assert_eq!(got[0].message, "new");
    }

    #[tokio::test]
    async fn first_sync_distrusts_empty_publishes() {
        let store = std::sync::Arc::new(DiagnosticsStore::default());
        store.update("u".into(), Some(1), vec![]);
        // Cold server published an empty set: with distrust_empty this must
        // become "pending" (None), not a false "no errors".
        let got = store
            .wait_for("u", 1, 0, Duration::from_millis(100), true)
            .await;
        assert!(got.is_none());

        // The real analysis landing resolves the wait even mid-flight.
        let publisher = std::sync::Arc::clone(&store);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            publisher.update(
                "u".into(),
                Some(1),
                vec![diag(0, DiagnosticSeverity::ERROR, "real")],
            );
        });
        let got = store
            .wait_for("u", 1, 0, Duration::from_secs(2), true)
            .await
            .expect("non-empty publish accepted");
        assert_eq!(got[0].message, "real");
    }
}
