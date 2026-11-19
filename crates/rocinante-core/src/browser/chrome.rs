//! Finding and starting the browser: a Chrome-family executable (Chrome,
//! Chromium, Edge, Brave) run on its own "Rocinante" profile, so it never
//! touches the user's tabs or logins. Chrome only allows DevTools control of
//! a non-default profile anyway (since Chrome 136).

use std::path::{Path, PathBuf};
use std::time::Duration;

/// The first Chrome-family browser on this machine, or the configured one.
pub fn find(configured: Option<&str>) -> Option<PathBuf> {
    if let Some(path) = configured.map(str::trim).filter(|p| !p.is_empty()) {
        let path = crate::local_models::expand_tilde(path);
        return path.is_file().then_some(path);
    }
    candidates().into_iter().find(|p| p.is_file())
}

fn candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if cfg!(target_os = "macos") {
        let apps = [
            ("Google Chrome", "Google Chrome"),
            ("Chromium", "Chromium"),
            ("Microsoft Edge", "Microsoft Edge"),
            ("Brave Browser", "Brave Browser"),
            ("Google Chrome Canary", "Google Chrome Canary"),
        ];
        let mut roots = vec![PathBuf::from("/Applications")];
        if let Some(home) = dirs::home_dir() {
            roots.push(home.join("Applications"));
        }
        for root in &roots {
            for (app, bin) in apps {
                out.push(root.join(format!("{app}.app/Contents/MacOS/{bin}")));
            }
        }
    } else if cfg!(windows) {
        let rel = [
            r"Google\Chrome\Application\chrome.exe",
            r"Microsoft\Edge\Application\msedge.exe",
            r"BraveSoftware\Brave-Browser\Application\brave.exe",
            r"Chromium\Application\chrome.exe",
        ];
        for var in ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"] {
            if let Some(base) = std::env::var_os(var) {
                for r in rel {
                    out.push(PathBuf::from(&base).join(r));
                }
            }
        }
    } else {
        for name in [
            "google-chrome-stable",
            "google-chrome",
            "chromium",
            "chromium-browser",
            "microsoft-edge-stable",
            "microsoft-edge",
            "brave-browser",
        ] {
            if let Some(p) = which(name) {
                out.push(p);
            }
        }
    }
    out
}

fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|p| p.is_file())
}

/// Whether a window can be shown: not over SSH, and on Linux a display.
pub fn has_display() -> bool {
    let remote = ["SSH_CONNECTION", "SSH_CLIENT", "SSH_TTY"]
        .iter()
        .any(|v| std::env::var_os(v).is_some());
    if remote {
        return false;
    }
    if cfg!(any(target_os = "macos", windows)) {
        return true;
    }
    ["DISPLAY", "WAYLAND_DISPLAY"]
        .iter()
        .any(|v| std::env::var_os(v).is_some_and(|s| !s.is_empty()))
}

/// The browser profile Rocinante uses: `~/.rocinante/browser`.
pub fn profile_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".rocinante").join("browser"))
}

/// A browser to talk to: its DevTools address and, when this session
/// started it, the process (killed when dropped).
pub struct Launched {
    pub ws_url: String,
    pub child: Option<tokio::process::Child>,
}

/// The arguments the browser starts with.
pub fn args(profile: &Path, headless: bool) -> Vec<String> {
    let mut args = vec![
        "--remote-debugging-port=0".to_string(),
        format!("--user-data-dir={}", profile.display()),
        "--no-first-run".into(),
        "--no-default-browser-check".into(),
        "--hide-crash-restore-bubble".into(),
        // Names the window "Rocinante" in the OS window switcher.
        "--window-name=Rocinante".into(),
        "--window-size=1300,940".into(),
    ];
    if cfg!(target_os = "macos") {
        // No keychain prompt for the profile's (empty) password store.
        args.push("--use-mock-keychain".into());
    } else if cfg!(target_os = "linux") {
        args.push("--password-store=basic".into());
    }
    if headless {
        args.push("--headless=new".into());
    }
    args.push("about:blank".into());
    args
}

/// Start the browser on the Rocinante profile, or reuse one already running
/// on it (another Rocinante session's).
pub async fn launch(exe: &Path, headless: bool) -> Result<Launched, String> {
    let profile = profile_dir().ok_or("no home directory for the browser profile")?;
    std::fs::create_dir_all(&profile)
        .map_err(|e| format!("couldn't create {}: {e}", profile.display()))?;
    name_profile(&profile);
    let port_file = profile.join("DevToolsActivePort");
    if let Some(url) = live_endpoint(&port_file).await {
        return Ok(Launched {
            ws_url: url,
            child: None,
        });
    }
    let _ = std::fs::remove_file(&port_file);
    let mut command = tokio::process::Command::new(exe);
    command
        .args(args(&profile, headless))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    // Its own process group: a Ctrl+C meant for Rocinante isn't for it.
    #[cfg(unix)]
    command.process_group(0);
    let mut child = command
        .spawn()
        .map_err(|e| format!("couldn't start {}: {e}", exe.display()))?;
    if let Some(pid) = child.id() {
        watch_over(pid, exe);
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(url) = endpoint(&port_file) {
            return Ok(Launched {
                ws_url: url,
                child: Some(child),
            });
        }
        if let Ok(Some(status)) = child.try_wait() {
            // Handed off to a browser already running on this profile?
            if let Some(url) = live_endpoint(&port_file).await {
                return Ok(Launched {
                    ws_url: url,
                    child: None,
                });
            }
            return Err(format!(
                "{} exited before it was ready ({status})",
                exe.display()
            ));
        }
        if tokio::time::Instant::now() > deadline {
            return Err(format!("{} didn't start in time", exe.display()));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Close the browser if Rocinante goes away without closing it (the
/// terminal closed, a crash): a small watchdog process outside Rocinante's
/// process group waits for either to exit, and stops the browser only if
/// Rocinante went first.
fn watch_over(browser: u32, exe: &Path) {
    let me = std::process::id();
    #[cfg(unix)]
    let spawned = {
        let script = format!(
            "trap '' HUP INT; \
             while kill -0 {me} 2>/dev/null && kill -0 {browser} 2>/dev/null; do sleep 2; done; \
             kill -0 {me} 2>/dev/null || kill {browser} 2>/dev/null"
        );
        let _ = exe;
        let mut c = tokio::process::Command::new("/bin/sh");
        c.arg("-c").arg(script).process_group(0);
        c
    };
    #[cfg(windows)]
    let spawned = {
        // Stop it only if the id still belongs to this browser.
        let script = format!(
            "try {{ Wait-Process -Id {me} -ErrorAction Stop }} catch {{}}; \
             Get-Process -Id {browser} -ErrorAction SilentlyContinue | \
             Where-Object {{ $_.Path -eq '{}' }} | Stop-Process -Force",
            exe.display().to_string().replace('\'', "''")
        );
        let mut c = tokio::process::Command::new("powershell");
        c.args(["-NoProfile", "-NonInteractive", "-Command", script.as_str()])
            .creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        c
    };
    let mut command = spawned;
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    match command.spawn() {
        // Reaped when it ends, so it never lingers as a zombie.
        Ok(mut watchdog) => {
            tokio::spawn(async move {
                let _ = watchdog.wait().await;
            });
        }
        Err(e) => tracing::warn!(error = %e, "browser watchdog didn't start"),
    }
}

/// The DevTools WebSocket address from `DevToolsActivePort`: the port on
/// the first line, the browser's path on the second.
fn endpoint(port_file: &Path) -> Option<String> {
    let text = std::fs::read_to_string(port_file).ok()?;
    parse_port_file(&text)
}

fn parse_port_file(text: &str) -> Option<String> {
    let mut lines = text.lines();
    let port: u16 = lines.next()?.trim().parse().ok()?;
    let path = lines.next()?.trim();
    path.starts_with("/devtools/browser/")
        .then(|| format!("ws://127.0.0.1:{port}{path}"))
}

/// An endpoint left by a browser that is still running and answers.
async fn live_endpoint(port_file: &Path) -> Option<String> {
    let url = endpoint(port_file)?;
    let answered = tokio::time::timeout(
        Duration::from_secs(2),
        tokio_tungstenite::connect_async(url.as_str()),
    )
    .await;
    matches!(answered, Ok(Ok(_))).then_some(url)
}

/// Name a new profile "Rocinante", so the window's profile button says so.
/// Best effort: Chrome owns these files once it has run.
fn name_profile(profile: &Path) {
    let local_state = profile.join("Local State");
    if !local_state.exists() {
        let _ = std::fs::write(
            &local_state,
            r#"{"profile":{"info_cache":{"Default":{"name":"Rocinante","is_using_default_name":false}}}}"#,
        );
    }
    let prefs = profile.join("Default").join("Preferences");
    if !prefs.exists() && std::fs::create_dir_all(profile.join("Default")).is_ok() {
        let _ = std::fs::write(&prefs, r#"{"profile":{"name":"Rocinante"}}"#);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_file_gives_the_browser_socket() {
        assert_eq!(
            parse_port_file("51234\n/devtools/browser/0b5c-77\n").as_deref(),
            Some("ws://127.0.0.1:51234/devtools/browser/0b5c-77")
        );
        assert_eq!(parse_port_file("51234\n"), None);
        assert_eq!(parse_port_file("x\n/devtools/browser/1"), None);
    }

    #[test]
    fn args_use_their_own_profile_and_headless_only_when_asked() {
        let a = args(Path::new("/p"), false);
        assert!(a.contains(&"--user-data-dir=/p".to_string()));
        assert!(a.contains(&"--remote-debugging-port=0".to_string()));
        assert!(!a.iter().any(|x| x.starts_with("--headless")));
        assert!(args(Path::new("/p"), true).contains(&"--headless=new".to_string()));
    }

    #[test]
    fn a_configured_path_must_exist() {
        assert_eq!(find(Some("/no/such/chrome")), None);
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("chrome");
        std::fs::write(&exe, b"").unwrap();
        assert_eq!(find(Some(&exe.display().to_string())), Some(exe));
    }
}
