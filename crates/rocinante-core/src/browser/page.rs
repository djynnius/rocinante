//! One browser tab Rocinante drives: open, look (screenshot, text
//! snapshot), and use (click, type, keys, scroll), with the site guard
//! enforced on every navigation the page makes.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};

use super::cdp::{Connection, Event};
use super::chrome;
use super::guard::Site;

/// The page-side helpers: the text snapshot and element lookup.
const SNAPSHOT_JS: &str = include_str!("snapshot.js");

/// The viewport pages are shown at unless `resize` changes it.
pub const DEFAULT_VIEWPORT: (u32, u32) = (1280, 800);

/// How much of the page log is kept.
const LOG_CAP: usize = 200;

/// What the page did on its own since the model last looked.
#[derive(Default)]
struct PageState {
    site: Option<Site>,
    /// The last URL the guard let the tab be on.
    last_good: Option<String>,
    log: VecDeque<String>,
    /// Errors and warnings logged since `console` was last read.
    unread: usize,
    /// How many of those the model has been told about.
    announced: usize,
    /// Navigations and pop-ups the guard stopped, not yet reported.
    blocked: Vec<String>,
    /// Dialogs the page opened (dismissed), not yet reported.
    dialogs: Vec<String>,
    /// The user closed the tab or the window.
    gone: bool,
}

impl PageState {
    fn push_log(&mut self, line: String, counts: bool) {
        if self.log.len() == LOG_CAP {
            self.log.pop_front();
        }
        self.log.push_back(line);
        if counts {
            self.unread += 1;
        }
    }
}

pub struct Page {
    conn: Arc<Connection>,
    /// The browser process when this session started it (killed on drop).
    child: Option<tokio::process::Child>,
    session: String,
    target: String,
    state: Arc<Mutex<PageState>>,
    watcher: tokio::task::JoinHandle<()>,
    pub headless: bool,
    viewport: (u32, u32),
}

impl Drop for Page {
    fn drop(&mut self) {
        self.watcher.abort();
    }
}

impl Page {
    /// Start (or reuse) the browser and open a tab to drive.
    pub async fn start(exe: &Path, headless: bool) -> Result<Self, String> {
        let launched = chrome::launch(exe, headless).await?;
        let conn = Connection::connect(&launched.ws_url).await?;
        // Reuse the blank tab a fresh browser opens with; else make one.
        let targets = conn.call("Target.getTargets", json!({}), None).await?;
        let blank = targets["targetInfos"].as_array().and_then(|ts| {
            ts.iter()
                .find(|t| {
                    t["type"] == "page" && t["url"] == "about:blank" && t["attached"] == false
                })
                .and_then(|t| t["targetId"].as_str().map(String::from))
        });
        let target = match blank {
            Some(t) if launched.child.is_some() => t,
            _ => conn
                .call("Target.createTarget", json!({ "url": "about:blank" }), None)
                .await?["targetId"]
                .as_str()
                .ok_or("the browser didn't open a tab")?
                .to_string(),
        };
        let attached = conn
            .call(
                "Target.attachToTarget",
                json!({ "targetId": target, "flatten": true }),
                None,
            )
            .await?;
        let session = attached["sessionId"]
            .as_str()
            .ok_or("couldn't attach to the tab")?
            .to_string();
        let state = Arc::new(Mutex::new(PageState::default()));
        let watcher = tokio::spawn(watch(
            Arc::clone(&conn),
            conn.subscribe(),
            session.clone(),
            target.clone(),
            Arc::clone(&state),
        ));
        let mut page = Self {
            conn,
            child: launched.child,
            session,
            target,
            state,
            watcher,
            headless,
            viewport: DEFAULT_VIEWPORT,
        };
        for (method, params) in [
            ("Page.enable", json!({})),
            ("Runtime.enable", json!({})),
            ("Log.enable", json!({})),
            // Every top-level page load passes the guard first.
            (
                "Fetch.enable",
                json!({ "patterns": [{ "urlPattern": "*", "resourceType": "Document" }] }),
            ),
        ] {
            page.call(method, params).await?;
        }
        // Pop-ups and new tabs show up as new targets: the watcher closes them.
        page.conn
            .call(
                "Target.setDiscoverTargets",
                json!({ "discover": true }),
                None,
            )
            .await?;
        let _ = page
            .conn
            .call(
                "Browser.setDownloadBehavior",
                json!({ "behavior": "deny" }),
                None,
            )
            .await;
        page.set_viewport(DEFAULT_VIEWPORT.0, DEFAULT_VIEWPORT.1)
            .await?;
        Ok(page)
    }

    /// Whether the tab can still be driven.
    pub fn is_alive(&self) -> bool {
        !self.conn.is_closed() && !self.state.lock().unwrap().gone
    }

    /// Close the tab, and the browser if this session started it.
    pub async fn shut_down(mut self) {
        if self.child.is_some() {
            let _ = self.conn.call("Browser.close", json!({}), None).await;
            if let Some(child) = self.child.as_mut() {
                let _ = tokio::time::timeout(Duration::from_secs(3), child.wait()).await;
            }
        } else {
            let _ = self
                .conn
                .call(
                    "Target.closeTarget",
                    json!({ "targetId": self.target }),
                    None,
                )
                .await;
        }
    }

    async fn call(&self, method: &str, params: Value) -> Result<Value, String> {
        self.conn.call(method, params, Some(&self.session)).await
    }

    /// Run JavaScript in the page and return its (JSON) value.
    async fn eval(&self, expression: &str) -> Result<Value, String> {
        let r = self
            .call(
                "Runtime.evaluate",
                json!({ "expression": expression, "returnByValue": true, "awaitPromise": true }),
            )
            .await?;
        if let Some(ex) = r.get("exceptionDetails") {
            let msg = ex["exception"]["description"]
                .as_str()
                .or_else(|| ex["text"].as_str())
                .unwrap_or("script error");
            return Err(msg.lines().next().unwrap_or(msg).to_string());
        }
        Ok(r["result"]["value"].clone())
    }

    pub fn site(&self) -> Option<Site> {
        self.state.lock().unwrap().site.clone()
    }

    /// Load `url` as the start of `site`.
    pub async fn open(&mut self, url: &str, site: Site) -> Result<String, String> {
        {
            let mut st = self.state.lock().unwrap();
            st.site = Some(site);
            st.last_good = None;
            st.log.clear();
            st.unread = 0;
            st.announced = 0;
            st.blocked.clear();
            st.dialogs.clear();
        }
        let mut events = self.conn.subscribe();
        let r = self.call("Page.navigate", json!({ "url": url })).await?;
        if let Some(err) = r["errorText"].as_str().filter(|e| !e.is_empty()) {
            let hint = if err.contains("CONNECTION_REFUSED") {
                " — is the dev server running? Start it with bash first"
            } else {
                ""
            };
            return Err(format!("couldn't load {url}: {err}{hint}"));
        }
        self.wait_for_load(&mut events, Duration::from_secs(15))
            .await;
        Ok(format!("Opened {}.", self.where_now().await))
    }

    /// Wait for the tab's load event (or the timeout), then let it settle.
    async fn wait_for_load(
        &self,
        events: &mut tokio::sync::broadcast::Receiver<Event>,
        limit: Duration,
    ) {
        let session = self.session.clone();
        let _ = tokio::time::timeout(limit, async {
            loop {
                match events.recv().await {
                    Ok(ev)
                        if ev.method == "Page.loadEventFired"
                            && ev.session.as_deref() == Some(&session) =>
                    {
                        break;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    _ => {}
                }
            }
        })
        .await;
        tokio::time::sleep(Duration::from_millis(250)).await;
    }

    /// After an action that may navigate: wait until the document is
    /// complete again (or a few seconds pass).
    async fn settle(&self) {
        tokio::time::sleep(Duration::from_millis(350)).await;
        for _ in 0..40 {
            // Fails while a new document is committing: keep polling.
            let state = self.eval("document.readyState").await;
            if state.as_ref().ok().and_then(Value::as_str) == Some("complete") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }

    /// A project file's URL as its path in the project (`about.html`);
    /// other URLs as they are.
    fn short<'a>(&self, text: &'a str) -> std::borrow::Cow<'a, str> {
        let root = match self.site() {
            Some(Site::Files { root }) => root,
            _ => return std::borrow::Cow::Borrowed(text),
        };
        match url::Url::from_file_path(&root) {
            Ok(u) => std::borrow::Cow::Owned(text.replace(&format!("{}/", u.as_str()), "")),
            Err(_) => std::borrow::Cow::Borrowed(text),
        }
    }

    /// `"Title" — url`.
    async fn where_now(&self) -> String {
        let v = self
            .eval("[document.title, location.href]")
            .await
            .unwrap_or(Value::Null);
        let title = v[0].as_str().unwrap_or("").trim();
        let url = self.short(v[1].as_str().unwrap_or("")).into_owned();
        let url = url.as_str();
        if title.is_empty() {
            url.to_string()
        } else {
            format!("\"{title}\" — {url}")
        }
    }

    /// Notes about what the page did on its own: blocked navigations,
    /// dismissed dialogs, new console errors.
    pub fn take_notes(&self) -> String {
        let mut st = self.state.lock().unwrap();
        let mut out = String::new();
        for b in st.blocked.drain(..) {
            out.push_str(&format!("\nBlocked: {b}"));
        }
        for d in st.dialogs.drain(..) {
            out.push_str(&format!("\nThe page opened a dialog, dismissed: {d}"));
        }
        if st.unread > st.announced {
            out.push_str(&format!(
                "\n{} console error(s) or warning(s) not read yet — use action console to read them.",
                st.unread
            ));
            st.announced = st.unread;
        }
        out
    }

    /// The page as a JPEG: the window, or the whole page (up to a limit).
    pub async fn screenshot(&self, full_page: bool) -> Result<(String, String), String> {
        let m = self.call("Page.getLayoutMetrics", json!({})).await?;
        let (vw, vh) = self.viewport;
        let content_h = m["cssContentSize"]["height"].as_f64().unwrap_or(vh as f64);
        let scroll_y = m["cssVisualViewport"]["pageY"].as_f64().unwrap_or(0.0);
        let (params, what) = if full_page {
            let max = (vh * 5).min(8000) as f64;
            let h = content_h.min(max).max(1.0);
            let cut = if content_h > max {
                format!(", the first {h:.0} of {content_h:.0} px")
            } else {
                String::new()
            };
            (
                json!({
                    "format": "jpeg", "quality": 75, "captureBeyondViewport": true,
                    "clip": { "x": 0, "y": 0, "width": vw, "height": h, "scale": 1 },
                }),
                format!("whole page, {vw}×{h:.0} px{cut}"),
            )
        } else {
            (
                json!({ "format": "jpeg", "quality": 80 }),
                format!("window, {vw}×{vh} px, scrolled to {scroll_y:.0} of {content_h:.0} px"),
            )
        };
        let shot = self.call("Page.captureScreenshot", params).await?;
        let data = shot["data"]
            .as_str()
            .ok_or("the browser returned no image")?
            .to_string();
        Ok((
            format!("Screenshot of {} ({what}).", self.where_now().await),
            data,
        ))
    }

    /// The page as text: title, problems found, and its headings, text and
    /// numbered interactive elements.
    pub async fn snapshot(&self) -> Result<String, String> {
        let v = self.eval(&format!("({SNAPSHOT_JS})()")).await?;
        Ok(self.short(v.as_str().unwrap_or("")).into_owned())
    }

    /// Click element `n` of the latest snapshot.
    pub async fn click_ref(&self, n: u64) -> Result<String, String> {
        let t = self
            .eval(&format!(
                "window.__rcnTarget ? window.__rcnTarget({n}) : {{ error: 'nosnap' }}"
            ))
            .await?;
        if let Some(e) = t["error"].as_str() {
            return Err(match e {
                "nosnap" => "no snapshot of this page yet — take one with action snapshot, then use its numbers".into(),
                _ => format!("there is no element [{n}] on the page now — take a new snapshot"),
            });
        }
        let label = t["label"].as_str().unwrap_or("").to_string();
        if let Some(href) = t["href"].as_str() {
            let site = self.site();
            if site.as_ref().is_some_and(|s| !s.allows(href)) {
                return Err(format!(
                    "not followed: [{n}] {label} links to {href}, outside this site ({})",
                    site.map(|s| s.describe()).unwrap_or_default()
                ));
            }
        }
        // A same-site link meant for a new tab opens in this one: pop-ups
        // are closed, and only this tab is driven.
        if let (Some(href), true) = (t["href"].as_str(), t["newTab"] == true) {
            let mut events = self.conn.subscribe();
            self.call("Page.navigate", json!({ "url": href })).await?;
            self.wait_for_load(&mut events, Duration::from_secs(15))
                .await;
            return Ok(format!(
                "[{n}] {label} opens a new tab; opened it in this one instead. Now at {}.",
                self.where_now().await
            ));
        }
        if let Some(cover) = t["covered"].as_str() {
            return Err(format!(
                "didn't click: [{n}] {label} is hidden under {cover} at that spot. Close or move what covers it, or fix the layout"
            ));
        }
        let (x, y) = (
            t["x"].as_f64().unwrap_or(0.0),
            t["y"].as_f64().unwrap_or(0.0),
        );
        self.click_at(x, y).await?;
        Ok(format!(
            "Clicked [{n}] {label}. Now at {}.",
            self.where_now().await
        ))
    }

    /// Click a point of the window (CSS pixels, as on the screenshot).
    pub async fn click_point(&self, x: f64, y: f64) -> Result<String, String> {
        let (vw, vh) = self.viewport;
        if x < 0.0 || y < 0.0 || x >= vw as f64 || y >= vh as f64 {
            return Err(format!(
                "({x:.0}, {y:.0}) is outside the {vw}×{vh} window — use a point on the latest screenshot"
            ));
        }
        self.click_at(x, y).await?;
        Ok(format!(
            "Clicked at ({x:.0}, {y:.0}). Now at {}.",
            self.where_now().await
        ))
    }

    async fn click_at(&self, x: f64, y: f64) -> Result<(), String> {
        for (kind, extra) in [
            ("mouseMoved", json!({})),
            ("mousePressed", json!({ "button": "left", "clickCount": 1 })),
            (
                "mouseReleased",
                json!({ "button": "left", "clickCount": 1 }),
            ),
        ] {
            let mut p = json!({ "type": kind, "x": x, "y": y });
            if let (Some(p), Some(e)) = (p.as_object_mut(), extra.as_object()) {
                p.extend(e.clone());
            }
            self.call("Input.dispatchMouseEvent", p).await?;
        }
        self.settle().await;
        Ok(())
    }

    /// Type text where the focus is (after clicking element `n`, if given).
    pub async fn type_text(
        &self,
        n: Option<u64>,
        text: &str,
        submit: bool,
    ) -> Result<String, String> {
        let mut out = String::new();
        if let Some(n) = n {
            self.click_ref(n).await?;
            out.push_str(&format!("Typed into [{n}]"));
        } else {
            out.push_str("Typed");
        }
        self.call("Input.insertText", json!({ "text": text }))
            .await?;
        if submit {
            self.press("Enter").await?;
            out.push_str(" and pressed Enter");
        }
        out.push_str(&format!(". Now at {}.", self.where_now().await));
        Ok(out)
    }

    /// Press one key.
    pub async fn press(&self, key: &str) -> Result<String, String> {
        let (name, code, text) = key_info(key).ok_or_else(|| {
            format!("unknown key `{key}` — use Enter, Tab, Escape, Backspace, Space, ArrowUp, ArrowDown, ArrowLeft, ArrowRight, PageUp, PageDown, Home or End")
        })?;
        let mut down = json!({
            "type": if text.is_empty() { "rawKeyDown" } else { "keyDown" },
            "key": name, "code": name, "windowsVirtualKeyCode": code,
        });
        if !text.is_empty() {
            down["text"] = json!(text);
        }
        self.call("Input.dispatchKeyEvent", down).await?;
        self.call(
            "Input.dispatchKeyEvent",
            json!({ "type": "keyUp", "key": name, "code": name, "windowsVirtualKeyCode": code }),
        )
        .await?;
        self.settle().await;
        Ok(format!(
            "Pressed {name}. Now at {}.",
            self.where_now().await
        ))
    }

    /// Scroll the page: down/up by most of a window, or to the top/bottom.
    pub async fn scroll(&self, direction: &str, amount: Option<f64>) -> Result<String, String> {
        let (vw, vh) = self.viewport;
        match direction {
            "top" => {
                self.eval("window.scrollTo(0, 0)").await?;
            }
            "bottom" => {
                self.eval("window.scrollTo(0, document.documentElement.scrollHeight)")
                    .await?;
            }
            "down" | "up" => {
                let step = amount.unwrap_or(vh as f64 * 0.8).abs();
                let dy = if direction == "down" { step } else { -step };
                self.call(
                    "Input.dispatchMouseEvent",
                    json!({
                        "type": "mouseWheel", "x": vw / 2, "y": vh / 2,
                        "deltaX": 0, "deltaY": dy,
                    }),
                )
                .await?;
            }
            other => {
                return Err(format!(
                    "unknown direction `{other}` — use down, up, top or bottom"
                ));
            }
        }
        tokio::time::sleep(Duration::from_millis(350)).await;
        let v = self
            .eval("[Math.round(scrollY), document.documentElement.scrollHeight, innerHeight]")
            .await?;
        Ok(format!(
            "Scrolled {direction}: now at {} of {} px (window {} px high).",
            v[0], v[1], v[2]
        ))
    }

    /// The window size pages are shown at (a phone: 390×844).
    pub async fn set_viewport(&mut self, width: u32, height: u32) -> Result<String, String> {
        let width = width.clamp(320, 2560);
        let height = height.clamp(300, 2000);
        self.call(
            "Emulation.setDeviceMetricsOverride",
            json!({
                "width": width, "height": height, "deviceScaleFactor": 1,
                "mobile": width < 600,
            }),
        )
        .await?;
        self.viewport = (width, height);
        Ok(format!("The window is now {width}×{height} px."))
    }

    /// The page's console log, errors and failed loads.
    pub fn console(&self) -> String {
        let mut st = self.state.lock().unwrap();
        st.unread = 0;
        st.announced = 0;
        if st.log.is_empty() {
            return "The console is empty: no errors, warnings or messages since the page opened."
                .into();
        }
        let skip = st.log.len().saturating_sub(40);
        let mut out = format!(
            "Console since the page opened ({} entries{}):",
            st.log.len(),
            if skip > 0 { ", the last 40" } else { "" }
        );
        for line in st.log.iter().skip(skip) {
            out.push_str("\n- ");
            out.push_str(line);
        }
        out
    }

    /// Go back one page, if the page before is on this site.
    pub async fn back(&self) -> Result<String, String> {
        let h = self.call("Page.getNavigationHistory", json!({})).await?;
        let idx = h["currentIndex"].as_u64().unwrap_or(0) as usize;
        let Some(prev) = idx
            .checked_sub(1)
            .and_then(|i| h["entries"].as_array().and_then(|e| e.get(i)))
        else {
            return Err("there is no page to go back to".into());
        };
        let url = prev["url"].as_str().unwrap_or("");
        if self.site().is_some_and(|s| !s.allows(url)) {
            return Err(format!("not going back to {url}: it's outside this site"));
        }
        let mut events = self.conn.subscribe();
        self.call(
            "Page.navigateToHistoryEntry",
            json!({ "entryId": prev["id"] }),
        )
        .await?;
        self.wait_for_load(&mut events, Duration::from_secs(10))
            .await;
        Ok(format!("Went back. Now at {}.", self.where_now().await))
    }
}

/// Key name → (DOM key, Windows key code, text it types).
fn key_info(key: &str) -> Option<(&'static str, u32, &'static str)> {
    let k = key.trim().to_ascii_lowercase();
    Some(match k.as_str() {
        "enter" | "return" => ("Enter", 13, "\r"),
        "tab" => ("Tab", 9, ""),
        "escape" | "esc" => ("Escape", 27, ""),
        "backspace" => ("Backspace", 8, ""),
        "delete" => ("Delete", 46, ""),
        "space" | " " => (" ", 32, " "),
        "arrowup" | "up" => ("ArrowUp", 38, ""),
        "arrowdown" | "down" => ("ArrowDown", 40, ""),
        "arrowleft" | "left" => ("ArrowLeft", 37, ""),
        "arrowright" | "right" => ("ArrowRight", 39, ""),
        "pageup" => ("PageUp", 33, ""),
        "pagedown" => ("PageDown", 34, ""),
        "home" => ("Home", 36, ""),
        "end" => ("End", 35, ""),
        _ => return None,
    })
}

/// Follow the tab's events: guard its navigations, dismiss its dialogs,
/// close its pop-ups, and keep its console log.
async fn watch(
    conn: Arc<Connection>,
    mut events: tokio::sync::broadcast::Receiver<Event>,
    session: String,
    target: String,
    state: Arc<Mutex<PageState>>,
) {
    use tokio::sync::broadcast::error::RecvError;
    loop {
        let ev = match events.recv().await {
            Ok(ev) => ev,
            Err(RecvError::Lagged(_)) => continue,
            Err(RecvError::Closed) => break,
        };
        let ours = ev.session.as_deref() == Some(session.as_str());
        let p = &ev.params;
        match ev.method.as_str() {
            "Fetch.requestPaused" if ours => {
                let url = p["request"]["url"].as_str().unwrap_or("");
                let main_frame = p["frameId"].as_str() == Some(target.as_str());
                let refused = main_frame && {
                    let st = state.lock().unwrap();
                    st.site.as_ref().is_some_and(|s| !s.allows(url))
                };
                let id = p["requestId"].clone();
                let (method, params) = if refused {
                    let site = state
                        .lock()
                        .unwrap()
                        .site
                        .as_ref()
                        .map(Site::describe)
                        .unwrap_or_default();
                    state
                        .lock()
                        .unwrap()
                        .blocked
                        .push(format!("navigation to {url} (outside this site, {site})"));
                    // Aborted, not blocked: no error page, the tab stays put.
                    (
                        "Fetch.failRequest",
                        json!({ "requestId": id, "errorReason": "Aborted" }),
                    )
                } else {
                    ("Fetch.continueRequest", json!({ "requestId": id }))
                };
                let _ = conn.call(method, params, Some(&session)).await;
            }
            // `file://` loads skip Fetch: check where the tab ended up.
            "Page.frameNavigated" if ours && p["frame"].get("parentId").is_none() => {
                let url = p["frame"]["url"].as_str().unwrap_or("").to_string();
                let (allowed, back) = {
                    let st = state.lock().unwrap();
                    (
                        st.site.as_ref().is_none_or(|s| s.allows(&url)),
                        st.last_good.clone(),
                    )
                };
                if allowed {
                    if !url.starts_with("chrome-error://") {
                        state.lock().unwrap().last_good = Some(url);
                    }
                } else {
                    state.lock().unwrap().blocked.push(format!(
                        "navigation to {url} (outside this site) — went back"
                    ));
                    let to = back.unwrap_or_else(|| "about:blank".into());
                    let _ = conn
                        .call("Page.navigate", json!({ "url": to }), Some(&session))
                        .await;
                }
            }
            "Page.javascriptDialogOpening" if ours => {
                let kind = p["type"].as_str().unwrap_or("dialog");
                let message = p["message"].as_str().unwrap_or("");
                // Leaving a page is fine; anything that asks yes/no is not
                // answered yes on the user's behalf.
                let accept = matches!(kind, "alert" | "beforeunload");
                state
                    .lock()
                    .unwrap()
                    .dialogs
                    .push(format!("{kind} \"{message}\""));
                let _ = conn
                    .call(
                        "Page.handleJavaScriptDialog",
                        json!({ "accept": accept }),
                        Some(&session),
                    )
                    .await;
            }
            "Runtime.consoleAPICalled" if ours => {
                let kind = p["type"].as_str().unwrap_or("log");
                let text: Vec<String> = p["args"]
                    .as_array()
                    .map(|args| args.iter().map(remote_text).collect())
                    .unwrap_or_default();
                let counts = matches!(kind, "error" | "warning" | "assert");
                state
                    .lock()
                    .unwrap()
                    .push_log(format!("console.{kind}: {}", clip(&text.join(" "))), counts);
            }
            "Runtime.exceptionThrown" if ours => {
                let d = &p["exceptionDetails"];
                let text = d["exception"]["description"]
                    .as_str()
                    .or_else(|| d["text"].as_str())
                    .unwrap_or("exception");
                let line = text.lines().next().unwrap_or(text);
                state
                    .lock()
                    .unwrap()
                    .push_log(format!("uncaught error: {}", clip(line)), true);
            }
            "Log.entryAdded" if ours => {
                let e = &p["entry"];
                let level = e["level"].as_str().unwrap_or("info");
                let mut text = e["text"].as_str().unwrap_or("").to_string();
                if let Some(url) = e["url"].as_str().filter(|u| !u.is_empty()) {
                    text.push_str(&format!(" ({url})"));
                }
                let counts = matches!(level, "error" | "warning");
                state
                    .lock()
                    .unwrap()
                    .push_log(format!("{level}: {}", clip(&text)), counts);
            }
            "Target.targetCreated" => {
                let info = &p["targetInfo"];
                if info["type"] == "page" && info["openerId"].as_str() == Some(target.as_str()) {
                    let url = info["url"].as_str().unwrap_or("").to_string();
                    state.lock().unwrap().blocked.push(format!(
                        "a new tab/pop-up{} — closed; open pages in this tab with action open",
                        if url.is_empty() || url == "about:blank" {
                            String::new()
                        } else {
                            format!(" to {url}")
                        }
                    ));
                    let _ = conn
                        .call(
                            "Target.closeTarget",
                            json!({ "targetId": info["targetId"] }),
                            None,
                        )
                        .await;
                }
            }
            "Target.targetDestroyed" if p["targetId"].as_str() == Some(target.as_str()) => {
                state.lock().unwrap().gone = true;
            }
            "Target.detachedFromTarget" if p["sessionId"].as_str() == Some(session.as_str()) => {
                state.lock().unwrap().gone = true;
            }
            _ => {}
        }
    }
}

/// A console argument as text.
fn remote_text(v: &Value) -> String {
    match v.get("value") {
        Some(Value::String(s)) => s.clone(),
        Some(other) if !other.is_null() => other.to_string(),
        _ => v["description"]
            .as_str()
            .or_else(|| v["type"].as_str())
            .unwrap_or("")
            .to_string(),
    }
}

fn clip(s: &str) -> String {
    const MAX: usize = 300;
    if s.chars().count() <= MAX {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(MAX).collect::<String>())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_map_to_codes() {
        assert_eq!(key_info("enter"), Some(("Enter", 13, "\r")));
        assert_eq!(key_info("ArrowDown"), Some(("ArrowDown", 40, "")));
        assert_eq!(key_info("esc").map(|k| k.0), Some("Escape"));
        assert_eq!(key_info("F13"), None);
    }

    #[test]
    fn console_arguments_read_as_text() {
        assert_eq!(remote_text(&json!({"type":"string","value":"hi"})), "hi");
        assert_eq!(remote_text(&json!({"type":"number","value":3})), "3");
        assert_eq!(
            remote_text(&json!({"type":"object","description":"Error: boom"})),
            "Error: boom"
        );
        assert_eq!(clip(&"x".repeat(400)).chars().count(), 301);
    }

    #[test]
    fn the_log_is_capped_and_counts_problems() {
        let mut st = PageState::default();
        for i in 0..(LOG_CAP + 5) {
            st.push_log(format!("line {i}"), i % 2 == 0);
        }
        assert_eq!(st.log.len(), LOG_CAP);
        assert_eq!(st.log.front().map(String::as_str), Some("line 5"));
        assert_eq!(st.unread, (LOG_CAP + 5).div_ceil(2));
    }
}
