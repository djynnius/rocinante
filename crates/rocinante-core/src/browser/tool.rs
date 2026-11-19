//! The `browser` tool: the model looks at and uses this project's pages in
//! Rocinante's own Chrome window.

use std::path::PathBuf;

use async_trait::async_trait;
use serde_json::{Value, json};

use super::guard;
use super::page::Page;
use crate::tools::{Tool, ToolCtx, ToolKind, ToolOutput};

pub struct BrowserTool {
    exe: PathBuf,
    /// `[browser] headless`; None = only without a display.
    headless: Option<bool>,
    page: tokio::sync::Mutex<Option<Page>>,
}

impl BrowserTool {
    pub fn new(exe: PathBuf, headless: Option<bool>) -> Self {
        Self {
            exe,
            headless,
            page: tokio::sync::Mutex::new(None),
        }
    }
}

const ACTIONS: &[&str] = &[
    "open",
    "screenshot",
    "snapshot",
    "click",
    "type",
    "press",
    "scroll",
    "resize",
    "console",
    "back",
    "close",
];

#[async_trait]
impl Tool for BrowserTool {
    fn name(&self) -> &'static str {
        "browser"
    }

    fn description(&self) -> &'static str {
        "Look at and use this project's web pages in a real Chrome window. \
         1. action open with url: a project file (index.html, dist/index.html) or the \
         project's local server (http://localhost:5173 — start it with bash first). \
         2. action screenshot to see the page, or snapshot for its text, problems and \
         numbered elements. 3. click, type, press, scroll using a [number] from the \
         latest snapshot (or x and y from the latest screenshot), then look again. \
         console lists the page's errors; resize tries other screen sizes (390×844 is a \
         phone). Only this project's pages open: links elsewhere are not followed."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": { "type": "string", "enum": ACTIONS },
                "url": {
                    "type": "string",
                    "description": "open: a file in this project, or http://localhost:PORT"
                },
                "ref": {
                    "type": "integer",
                    "description": "click, type: the element's [number] in the latest snapshot"
                },
                "x": { "type": "number", "description": "click: pixels from the left of the latest screenshot" },
                "y": { "type": "number", "description": "click: pixels from the top of the latest screenshot" },
                "text": { "type": "string", "description": "type: the text to enter" },
                "submit": { "type": "boolean", "description": "type: press Enter afterwards" },
                "key": {
                    "type": "string",
                    "description": "press: Enter, Tab, Escape, Backspace, Space, ArrowUp, ArrowDown, ArrowLeft, ArrowRight, PageUp, PageDown, Home or End"
                },
                "direction": { "type": "string", "enum": ["down", "up", "top", "bottom"] },
                "full_page": { "type": "boolean", "description": "screenshot: the whole page, not just the window" },
                "width": { "type": "integer", "description": "resize: window width in pixels" },
                "height": { "type": "integer", "description": "resize: window height in pixels" }
            },
            "required": ["action"]
        })
    }

    fn kind(&self) -> ToolKind {
        // Clicks and typing can change the site's data (submit a form).
        ToolKind::Edit
    }

    fn describe_call(&self, args: &Value) -> String {
        let action = args["action"].as_str().unwrap_or("?");
        let detail = match action {
            "open" => args["url"].as_str().map(String::from),
            "click" | "type" => args["ref"].as_u64().map(|n| format!("[{n}]")).or_else(|| {
                match (args["x"].as_f64(), args["y"].as_f64()) {
                    (Some(x), Some(y)) => Some(format!("({x:.0}, {y:.0})")),
                    _ => None,
                }
            }),
            "press" => args["key"].as_str().map(String::from),
            "scroll" => args["direction"].as_str().map(String::from),
            "resize" => match (args["width"].as_u64(), args["height"].as_u64()) {
                (Some(w), Some(h)) => Some(format!("{w}×{h}")),
                _ => None,
            },
            _ => None,
        };
        match detail {
            Some(d) => format!("browser: {action} {d}"),
            None => format!("browser: {action}"),
        }
    }

    async fn run(&self, args: Value, ctx: &ToolCtx) -> ToolOutput {
        let action = args["action"].as_str().unwrap_or("");
        if !ACTIONS.contains(&action) {
            return ToolOutput::error(format!(
                "unknown action `{action}` — use one of: {}",
                ACTIONS.join(", ")
            ));
        }
        let mut slot = self.page.lock().await;
        if action == "close" {
            return match slot.take() {
                Some(page) => {
                    page.shut_down().await;
                    ToolOutput::ok("Closed the browser.")
                }
                None => ToolOutput::ok("The browser isn't open."),
            };
        }
        // Validate `open` before starting a browser for it.
        let opening = if action == "open" {
            match guard::resolve_open(args["url"].as_str().unwrap_or(""), &ctx.cwd) {
                Ok(o) => Some(o),
                Err(e) => return ToolOutput::error(e),
            }
        } else {
            None
        };
        if slot.as_ref().is_some_and(|p| !p.is_alive()) {
            *slot = None;
        }
        if slot.is_none() {
            if opening.is_none() {
                return ToolOutput::error(
                    "the browser isn't open — start with action open and a url",
                );
            }
            let headless = self
                .headless
                .unwrap_or_else(|| !super::chrome::has_display());
            match Page::start(&self.exe, headless).await {
                Ok(p) => *slot = Some(p),
                Err(e) => return ToolOutput::error(format!("couldn't start the browser: {e}")),
            }
        }
        let Some(page) = slot.as_mut() else {
            return ToolOutput::error("the browser isn't open");
        };
        let result = act(page, action, &args, opening).await;
        let notes = page.take_notes();
        match result {
            Ok(Done::Text(text)) => ToolOutput::ok(format!("{text}{notes}")),
            Ok(Done::Image(text, data)) => {
                ToolOutput::ok(format!("{text}{notes}")).with_image(rocinante_providers::Image {
                    media_type: "image/jpeg".into(),
                    data,
                })
            }
            Err(e) => ToolOutput::error(format!("{e}{notes}")),
        }
    }
}

enum Done {
    Text(String),
    /// Text plus a base64 JPEG.
    Image(String, String),
}

async fn act(
    page: &mut Page,
    action: &str,
    args: &Value,
    opening: Option<(String, guard::Site)>,
) -> Result<Done, String> {
    let text = |t: String| Ok(Done::Text(t));
    match action {
        "open" => {
            let (url, site) = opening.ok_or("give url")?;
            let mut out = page.open(&url, site).await?;
            if page.headless {
                out.push_str(" (The browser window is hidden.)");
            }
            out.push_str(" Next: screenshot to see it, or snapshot for its text and elements.");
            text(out)
        }
        _ if page.site().is_none() => {
            Err("no page is open — start with action open and a url".into())
        }
        "screenshot" => {
            let (t, data) = page.screenshot(args["full_page"] == true).await?;
            Ok(Done::Image(t, data))
        }
        "snapshot" => text(page.snapshot().await?),
        "click" => {
            if let Some(n) = args["ref"].as_u64() {
                text(page.click_ref(n).await?)
            } else if let (Some(x), Some(y)) = (args["x"].as_f64(), args["y"].as_f64()) {
                text(page.click_point(x, y).await?)
            } else {
                Err("click needs ref (a [number] from the snapshot) or x and y".into())
            }
        }
        "type" => {
            let Some(t) = args["text"].as_str() else {
                return Err("type needs text".into());
            };
            text(
                page.type_text(args["ref"].as_u64(), t, args["submit"] == true)
                    .await?,
            )
        }
        "press" => {
            let key = args["key"].as_str().ok_or("press needs key, e.g. Enter")?;
            text(page.press(key).await?)
        }
        "scroll" => text(
            page.scroll(
                args["direction"].as_str().unwrap_or("down"),
                args["amount"].as_f64(),
            )
            .await?,
        ),
        "resize" => {
            let (Some(w), Some(h)) = (args["width"].as_u64(), args["height"].as_u64()) else {
                return Err("resize needs width and height, e.g. 390 and 844 for a phone".into());
            };
            text(page.set_viewport(w as u32, h as u32).await?)
        }
        "console" => text(page.console()),
        "back" => text(page.back().await?),
        other => Err(format!("unknown action `{other}`")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calls_read_well_in_prompts() {
        let t = BrowserTool::new(PathBuf::from("/chrome"), None);
        assert_eq!(
            t.describe_call(&json!({"action":"open","url":"http://localhost:5173"})),
            "browser: open http://localhost:5173"
        );
        assert_eq!(
            t.describe_call(&json!({"action":"click","ref":3})),
            "browser: click [3]"
        );
        assert_eq!(
            t.describe_call(&json!({"action":"click","x":10.4,"y":20})),
            "browser: click (10, 20)"
        );
        assert_eq!(
            t.describe_call(&json!({"action":"screenshot"})),
            "browser: screenshot"
        );
    }

    #[test]
    fn allowing_it_once_covers_every_action() {
        use crate::permissions::{Decision, PermissionEngine};
        let t = BrowserTool::new(PathBuf::from("/chrome"), None);
        let engine = PermissionEngine::from_config(&Default::default());
        let normal = crate::config::Mode::Normal;
        let click = json!({"action":"click","ref":2});
        assert!(matches!(engine.evaluate(normal, &t, &click), Decision::Ask));
        engine.remember_allow("browser", &json!({"action":"open","url":"index.html"}));
        assert!(matches!(
            engine.evaluate(normal, &t, &click),
            Decision::Allow
        ));
        assert!(matches!(
            engine.evaluate(crate::config::Mode::Plan, &t, &click),
            Decision::Deny { .. }
        ));
    }

    #[tokio::test]
    async fn bad_calls_fail_before_starting_a_browser() {
        let t = BrowserTool::new(PathBuf::from("/no/such/chrome"), Some(true));
        let dir = tempfile::tempdir().unwrap();
        let ctx = ToolCtx {
            cwd: dir.path().to_path_buf(),
            events: crate::agent::events::EventSender::new(tokio::sync::broadcast::channel(4).0),
            cancel: tokio_util::sync::CancellationToken::new(),
            depth: 0,
            router: std::sync::Arc::new(crate::agent::events::ReplyRouter::default()),
            lsp: None,
            mode: crate::config::Mode::Normal,
        };
        let out = t.run(json!({"action":"dance"}), &ctx).await;
        assert!(out.is_error && out.content.contains("unknown action"));
        let out = t.run(json!({"action":"screenshot"}), &ctx).await;
        assert!(
            out.is_error && out.content.contains("action open"),
            "{}",
            out.content
        );
        let out = t
            .run(json!({"action":"open","url":"https://example.com"}), &ctx)
            .await;
        assert!(
            out.is_error && out.content.starts_with("not opened"),
            "{}",
            out.content
        );
        let out = t.run(json!({"action":"close"}), &ctx).await;
        assert!(!out.is_error);
    }

    /// Drives a real Chrome through a test site. Run with
    /// `cargo test -p rocinante-core live_chrome -- --ignored --nocapture`
    /// (ROCINANTE_TEST_WINDOW=1 shows the window; ROCINANTE_TEST_SHOTS=<dir>
    /// saves the screenshots).
    #[tokio::test]
    #[ignore = "starts Chrome"]
    async fn live_chrome_session() {
        let Some(exe) = super::super::chrome::find(None) else {
            eprintln!("no Chrome-family browser installed; skipping");
            return;
        };
        let site = tempfile::tempdir().unwrap();
        std::fs::write(
            site.path().join("index.html"),
            r#"<!doctype html><title>Test shop</title>
<style>.wide{width:1600px;height:20px;background:#c33}</style>
<h1>Welcome</h1>
<p>Some text about the shop.</p>
<a href="about.html">About us</a>
<a href="https://example.com/">Outside</a>
<a href="about.html" target="_blank">About in a new tab</a>
<button onclick="document.querySelector('h1').textContent='Clicked!'">Press me</button>
<button onclick="location.href='https://example.com/'">Leave</button>
<input id="email" placeholder="Email">
<div style="position:relative;height:40px"><button>Hidden</button><div style="position:absolute;top:0;left:0;width:200px;height:40px;background:#fc0" class="badge">SALE</div></div>
<img src="missing.png" alt="Logo">
<div class="wide"></div>
<script>console.error('boom from page')</script>"#,
        )
        .unwrap();
        std::fs::write(
            site.path().join("about.html"),
            r#"<!doctype html><title>About</title><h1>About</h1><a href="file:///etc/hosts">hosts</a>"#,
        )
        .unwrap();
        let headless = std::env::var_os("ROCINANTE_TEST_WINDOW").is_none();
        let t = BrowserTool::new(exe, Some(headless));
        let ctx = ToolCtx {
            cwd: site.path().to_path_buf(),
            events: crate::agent::events::EventSender::new(tokio::sync::broadcast::channel(4).0),
            cancel: tokio_util::sync::CancellationToken::new(),
            depth: 0,
            router: std::sync::Arc::new(crate::agent::events::ReplyRouter::default()),
            lsp: None,
            mode: crate::config::Mode::Normal,
        };
        let shots = std::env::var_os("ROCINANTE_TEST_SHOTS").map(PathBuf::from);
        let run = |a: Value| {
            let t = &t;
            let ctx = &ctx;
            async move {
                let out = t.run(a.clone(), ctx).await;
                eprintln!(
                    "--- {a}\n{}{}",
                    out.content,
                    if out.images.is_empty() {
                        ""
                    } else {
                        " [+image]"
                    }
                );
                out
            }
        };
        let save = |name: &str, out: &ToolOutput| {
            if let (Some(dir), Some(img)) = (&shots, out.images.first()) {
                let bytes = base64_decode(&img.data);
                std::fs::write(dir.join(name), bytes).unwrap();
            }
        };

        let out = run(json!({"action":"open","url":"index.html"})).await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("Test shop"));
        assert!(
            out.content.contains("console error"),
            "the page's error is flagged"
        );
        let out = run(json!({"action":"snapshot"})).await;
        assert!(
            !out.content.contains("not read yet"),
            "flagged once, not every time"
        );

        let snap = run(json!({"action":"snapshot"})).await.content;
        assert!(snap.contains("wider than the window"), "{snap}");
        assert!(snap.contains("failed to load: missing.png"), "{snap}");
        assert!(snap.contains("[1] link \"About us\""), "{snap}");
        assert!(
            snap.contains("button \"Hidden\" (covered by <div.badge>"),
            "{snap}"
        );
        let num = |label: &str| -> u64 {
            let line = snap.lines().find(|l| l.contains(label)).unwrap();
            line[1..line.find(']').unwrap()].parse().unwrap()
        };

        let out = run(json!({"action":"screenshot"})).await;
        assert!(!out.is_error && out.images.len() == 1);
        assert!(out.images[0].data.len() > 1000);
        save("window.jpg", &out);
        let out = run(json!({"action":"screenshot","full_page":true})).await;
        assert!(out.content.contains("whole page"));

        let out = run(json!({"action":"click","ref": num("Press me")})).await;
        assert!(!out.is_error, "{}", out.content);
        assert!(
            run(json!({"action":"snapshot"}))
                .await
                .content
                .contains("Clicked!")
        );

        let out = run(json!({"action":"click","ref": num("\"Outside\"")})).await;
        assert!(
            out.is_error && out.content.starts_with("not followed"),
            "{}",
            out.content
        );

        let out = run(json!({"action":"click","ref": num("Leave")})).await;
        assert!(
            out.content
                .contains("Blocked: navigation to https://example.com/"),
            "{}",
            out.content
        );
        assert!(
            out.content.contains("Test shop"),
            "stayed on the site: {}",
            out.content
        );

        let out = run(json!({"action":"type","ref": num("Email"),"text":"me@example.com"})).await;
        assert!(!out.is_error, "{}", out.content);
        assert!(
            run(json!({"action":"snapshot"}))
                .await
                .content
                .contains("value=\"me@example.com\"")
        );

        let out = run(json!({"action":"console"})).await;
        assert!(out.content.contains("boom from page"), "{}", out.content);

        let snap = run(json!({"action":"snapshot"})).await.content;
        let about = snap.lines().find(|l| l.contains("\"About us\"")).unwrap();
        let about: u64 = about[1..about.find(']').unwrap()].parse().unwrap();
        let out = run(json!({"action":"click","ref": about})).await;
        assert!(out.content.contains("\"About\""), "{}", out.content);
        let snap = run(json!({"action":"snapshot"})).await.content;
        assert!(snap.contains("[1] link \"hosts\""), "{snap}");
        let out = run(json!({"action":"click","ref": 1})).await;
        assert!(
            out.is_error && out.content.starts_with("not followed"),
            "{}",
            out.content
        );

        let out = run(json!({"action":"back"})).await;
        assert!(out.content.contains("Test shop"), "{}", out.content);
        let snap = run(json!({"action":"snapshot"})).await.content;
        let newtab = snap.lines().find(|l| l.contains("new tab")).unwrap();
        let newtab: u64 = newtab[1..newtab.find(']').unwrap()].parse().unwrap();
        let out = run(json!({"action":"click","ref": newtab})).await;
        assert!(
            out.content.contains("opened it in this one"),
            "{}",
            out.content
        );

        let out = run(json!({"action":"resize","width":390,"height":844})).await;
        assert!(out.content.contains("390×844"));
        let snap = run(json!({"action":"snapshot"})).await.content;
        assert!(snap.contains("No viewport meta tag"), "{snap}");
        assert!(
            snap.starts_with("Page: \"About\" — about.html"),
            "project-relative: {snap}"
        );
        let out = run(json!({"action":"screenshot"})).await;
        save("phone.jpg", &out);

        let out = run(json!({"action":"scroll","direction":"down"})).await;
        assert!(!out.is_error, "{}", out.content);
        let out = run(json!({"action":"open","url":"https://example.com"})).await;
        assert!(out.is_error);
        let out = run(json!({"action":"close"})).await;
        assert_eq!(out.content, "Closed the browser.");
    }

    fn base64_decode(s: &str) -> Vec<u8> {
        const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = Vec::new();
        let (mut buf, mut bits) = (0u32, 0);
        for c in s.bytes().filter(|&c| c != b'=') {
            let Some(v) = T.iter().position(|&t| t == c) else {
                continue;
            };
            buf = (buf << 6) | v as u32;
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                out.push((buf >> bits) as u8);
                buf &= (1 << bits) - 1;
            }
        }
        out
    }
}
