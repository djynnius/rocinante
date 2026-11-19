//! What the browser may show: this project's own pages and nothing else.
//! `open` takes a file inside the project folder or the project's local
//! server (a loopback address); after that, the page may only move within
//! that site. Rocinante enforces this, not the model.

use std::path::{Component, Path, PathBuf};

use url::Url;

/// The site the browser is on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Site {
    /// Files inside the project folder (`file://`).
    Files { root: PathBuf },
    /// The project's local server: one scheme and port on a loopback host.
    Local { scheme: String, port: u16 },
}

impl Site {
    /// Whether a page at `url` belongs to this site.
    pub fn allows(&self, url: &str) -> bool {
        if is_neutral(url) {
            return true;
        }
        let Ok(u) = Url::parse(url) else {
            return false;
        };
        match self {
            Site::Files { root } => {
                u.scheme() == "file" && u.to_file_path().is_ok_and(|p| inside(&p, root))
            }
            Site::Local { scheme, port } => {
                u.scheme() == scheme
                    && u.host_str().is_some_and(is_loopback)
                    && u.port_or_known_default() == Some(*port)
            }
        }
    }

    /// How the site reads in messages.
    pub fn describe(&self) -> String {
        match self {
            Site::Files { root } => format!("files in {}", root.display()),
            Site::Local { scheme, port } => format!("{scheme}://localhost:{port}"),
        }
    }
}

/// Pages that belong to no site and are always fine to be on.
fn is_neutral(url: &str) -> bool {
    url == "about:blank" || url == "about:srcdoc" || url.starts_with("chrome-error://")
}

/// Hosts that are this machine.
fn is_loopback(host: &str) -> bool {
    let host = host.trim_start_matches('[').trim_end_matches(']');
    host.eq_ignore_ascii_case("localhost")
        || host.to_ascii_lowercase().ends_with(".localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// Turn what `open` was given into a URL and the site it starts, or say why
/// it can't be opened.
pub fn resolve_open(input: &str, project: &Path) -> Result<(String, Site), String> {
    let input = input.trim();
    if input.is_empty() {
        return Err("give `url`: a file in this project (index.html) or its local server (http://localhost:5173)".into());
    }
    let root = project
        .canonicalize()
        .unwrap_or_else(|_| project.to_path_buf());
    let lower = input.to_ascii_lowercase();
    // `localhost:5173`, `127.0.0.1:8080/x`: a server address without a scheme.
    let input = if lower.starts_with("localhost") || lower.starts_with("127.") {
        format!("http://{input}")
    } else {
        input.to_string()
    };
    if input.starts_with("http://") || input.starts_with("https://") {
        let u = Url::parse(&input).map_err(|e| format!("`{input}` isn't a valid address: {e}"))?;
        if !u.host_str().is_some_and(is_loopback) {
            return Err(format!(
                "not opened: {input} isn't this project. The browser only opens the project's own pages — a file in the project folder, or its local server (http://localhost:PORT)"
            ));
        }
        let port = u
            .port_or_known_default()
            .ok_or_else(|| format!("`{input}` has no port"))?;
        let site = Site::Local {
            scheme: u.scheme().to_string(),
            port,
        };
        return Ok((u.to_string(), site));
    }
    if input.starts_with("file:") {
        let u = Url::parse(&input).map_err(|e| format!("`{input}` isn't a valid address: {e}"))?;
        let path = u
            .to_file_path()
            .map_err(|_| format!("`{input}` isn't a file path"))?;
        return open_file(&path, &root);
    }
    if input.contains("://") {
        return Err(format!(
            "not opened: {input} — only this project's files or its local server (http://localhost:PORT)"
        ));
    }
    let path = crate::local_models::expand_tilde(&input);
    let path = if path.is_relative() {
        root.join(path)
    } else {
        path
    };
    open_file(&path, &root)
}

fn open_file(path: &Path, root: &Path) -> Result<(String, Site), String> {
    let mut path = path
        .canonicalize()
        .map_err(|_| format!("no file at {}", path.display()))?;
    if !inside(&path, root) {
        return Err(format!(
            "not opened: {} is outside this project ({})",
            path.display(),
            root.display()
        ));
    }
    if path.is_dir() {
        let index = path.join("index.html");
        if !index.is_file() {
            return Err(format!(
                "{} is a folder with no index.html — open an .html file in it",
                path.display()
            ));
        }
        path = index;
    }
    let url = Url::from_file_path(&path)
        .map_err(|_| format!("{} can't be opened as a page", path.display()))?;
    Ok((
        url.to_string(),
        Site::Files {
            root: root.to_path_buf(),
        },
    ))
}

/// Whether `path` is `root` or below it. Existing paths are compared after
/// resolving symlinks (a link out of the project is outside); others by
/// their normalized form.
fn inside(path: &Path, root: &Path) -> bool {
    let path = path.canonicalize().unwrap_or_else(|_| normalize(path));
    let root = root.canonicalize().unwrap_or_else(|_| normalize(root));
    path.starts_with(&root)
}

/// `a/./b/../c` → `a/c`, without touching the disk.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("index.html"), "<h1>hi</h1>").unwrap();
        std::fs::create_dir(dir.path().join("dist")).unwrap();
        std::fs::write(dir.path().join("dist/index.html"), "<h1>built</h1>").unwrap();
        std::fs::create_dir(dir.path().join("empty")).unwrap();
        dir
    }

    #[test]
    fn local_servers_open_and_pin_their_port() {
        let p = project();
        let (url, site) = resolve_open("http://localhost:5173/", p.path()).unwrap();
        assert_eq!(url, "http://localhost:5173/");
        assert!(site.allows("http://localhost:5173/pricing"));
        assert!(
            site.allows("http://127.0.0.1:5173/about"),
            "same machine, same port"
        );
        assert!(
            !site.allows("http://localhost:8080/"),
            "another port is another site"
        );
        assert!(!site.allows("https://localhost:5173/"), "another scheme");
        assert!(!site.allows("https://example.com/"));
        assert!(!site.allows("file:///etc/passwd"));
        assert!(site.allows("about:blank"));

        // Without a scheme.
        let (url, _) = resolve_open("localhost:3000", p.path()).unwrap();
        assert_eq!(url, "http://localhost:3000/");
        let (_, site) = resolve_open("http://[::1]:8000", p.path()).unwrap();
        assert!(site.allows("http://localhost:8000/"));
    }

    #[test]
    fn other_hosts_are_refused() {
        let p = project();
        for url in [
            "https://example.com",
            "http://192.168.1.10:3000",
            "http://localhost.evil.com:3000",
            "ftp://localhost/x",
        ] {
            let err = resolve_open(url, p.path()).unwrap_err();
            assert!(err.starts_with("not opened"), "{url}: {err}");
        }
    }

    #[test]
    fn project_files_open_and_stay_inside() {
        let p = project();
        let (url, site) = resolve_open("index.html", p.path()).unwrap();
        assert!(
            url.starts_with("file://") && url.ends_with("/index.html"),
            "{url}"
        );
        let root = p.path().canonicalize().unwrap();
        let inner = Url::from_file_path(root.join("dist/index.html")).unwrap();
        assert!(site.allows(inner.as_str()));
        let sneaky = format!("{}/../outside.html", Url::from_file_path(&root).unwrap());
        assert!(!site.allows(&sneaky), "{sneaky}");
        assert!(!site.allows("http://localhost:5173/"));

        // A folder opens its index.html.
        let (url, _) = resolve_open("dist", p.path()).unwrap();
        assert!(url.ends_with("/dist/index.html"), "{url}");
        assert!(
            resolve_open("empty", p.path())
                .unwrap_err()
                .contains("no index.html")
        );
        assert!(
            resolve_open("missing.html", p.path())
                .unwrap_err()
                .starts_with("no file")
        );
    }

    #[test]
    fn files_outside_the_project_are_refused() {
        let p = project();
        let other = tempfile::tempdir().unwrap();
        let page = other.path().join("page.html");
        std::fs::write(&page, "x").unwrap();
        let err = resolve_open(&page.display().to_string(), p.path()).unwrap_err();
        assert!(err.contains("outside this project"), "{err}");
        let err = resolve_open("../", p.path()).unwrap_err();
        assert!(err.contains("outside this project"), "{err}");
        let as_url = Url::from_file_path(page.canonicalize().unwrap()).unwrap();
        assert!(resolve_open(as_url.as_str(), p.path()).is_err());
    }

    #[test]
    fn loopback_hosts() {
        for h in [
            "localhost",
            "LOCALHOST",
            "app.localhost",
            "127.0.0.1",
            "127.4.5.6",
            "[::1]",
            "::1",
        ] {
            assert!(is_loopback(h), "{h}");
        }
        for h in ["example.com", "10.0.0.1", "localhost.evil.com", "0.0.0.0"] {
            assert!(!is_loopback(h), "{h}");
        }
    }
}
