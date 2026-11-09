//! Toolbox: external CLIs (ferraus for audio, pixelox for images, plus any
//! cargo-dist project the user adds) that Rocinante installs on demand. The
//! know-how ships as built-in skills; the binary is fetched from the tool's
//! GitHub release only when a skill needs it, verified against the SHA-256
//! in the release's `dist-manifest.json`, and placed in `[toolbox] dir`
//! (default `~/.rocinante/bin`), which is prepended to PATH for `bash`.
//! Upstream installer scripts are never run.

mod tool;

pub use tool::{ToolboxTool, status_lines, user_command};

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};

use crate::config::{AutoInstall, Config};
use crate::selfupdate::{self, DistManifest};

/// A tool Rocinante knows how to install.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolSpec {
    pub name: String,
    /// `owner/name` on GitHub.
    pub repo: String,
    pub version_cmd: Vec<String>,
    pub doctor_cmd: Option<Vec<String>>,
    /// Arguments that start its MCP server (`{cwd}` = project dir).
    pub mcp_args: Option<Vec<String>>,
    /// Register that MCP server when installed.
    pub mcp: bool,
}

fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| s.to_string()).collect()
}

/// The compiled-in registry.
pub fn builtin_specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "ferraus".into(),
            repo: "djynnius/ferraus".into(),
            version_cmd: argv(&["ferraus", "--version"]),
            doctor_cmd: Some(argv(&["ferraus", "doctor", "--json"])),
            // 192 tools, one per command: never registered by default.
            mcp_args: None,
            mcp: false,
        },
        ToolSpec {
            name: "pixelox".into(),
            repo: "djynnius/pixelox".into(),
            version_cmd: argv(&["pixelox", "--version"]),
            doctor_cmd: Some(argv(&["pixelox", "doctor", "--json"])),
            mcp_args: Some(argv(&["mcp", "serve", "--allow", "{cwd}"])),
            mcp: false,
        },
    ]
}

/// Built-ins merged with `[toolbox.tools.<name>]` (overrides by name; new
/// names need `repo`).
pub fn registry(config: &Config) -> Vec<ToolSpec> {
    let mut specs = builtin_specs();
    for (name, user) in &config.toolbox.tools {
        let existing = specs.iter_mut().find(|s| &s.name == name);
        match existing {
            Some(spec) => {
                if let Some(repo) = &user.repo {
                    spec.repo = repo.clone();
                }
                if let Some(cmd) = &user.version_cmd {
                    spec.version_cmd = cmd.clone();
                }
                if user.doctor_cmd.is_some() {
                    spec.doctor_cmd = user.doctor_cmd.clone();
                }
                if user.mcp_args.is_some() {
                    spec.mcp_args = user.mcp_args.clone();
                }
                spec.mcp = user.mcp;
            }
            None => {
                let Some(repo) = &user.repo else {
                    tracing::warn!(tool = %name, "[toolbox.tools.{name}] needs `repo`; ignored");
                    continue;
                };
                specs.push(ToolSpec {
                    name: name.clone(),
                    repo: repo.clone(),
                    version_cmd: user
                        .version_cmd
                        .clone()
                        .unwrap_or_else(|| vec![name.clone(), "--version".into()]),
                    doctor_cmd: user.doctor_cmd.clone(),
                    mcp_args: user.mcp_args.clone(),
                    mcp: user.mcp,
                });
            }
        }
    }
    specs
}

/// Session-wide toolbox settings, set once at startup from config.
struct Settings {
    dir: PathBuf,
    auto_install: AutoInstall,
}

static SETTINGS: OnceLock<Settings> = OnceLock::new();

/// Record `[toolbox] dir` and `auto_install` for this process (first call
/// wins).
pub fn configure(config: &Config) {
    let _ = SETTINGS.set(Settings {
        dir: expand_dir(&config.toolbox.dir),
        auto_install: config.toolbox.auto_install,
    });
}

/// `~/` expansion for config paths.
pub fn expand_dir(dir: &str) -> PathBuf {
    match dir.strip_prefix("~/") {
        Some(rest) => dirs::home_dir()
            .map(|h| h.join(rest))
            .unwrap_or_else(|| PathBuf::from(dir)),
        None => PathBuf::from(dir),
    }
}

/// Where toolbox binaries live.
pub fn bin_dir() -> PathBuf {
    match SETTINGS.get() {
        Some(s) => s.dir.clone(),
        None => expand_dir("~/.rocinante/bin"),
    }
}

pub fn auto_install() -> AutoInstall {
    SETTINGS.get().map_or(AutoInstall::Ask, |s| s.auto_install)
}

/// PATH with the toolbox dir in front, for child processes.
pub fn path_with_toolbox() -> OsString {
    prepend_path(&bin_dir(), std::env::var_os("PATH"))
}

fn prepend_path(dir: &Path, path: Option<OsString>) -> OsString {
    let mut parts = vec![dir.to_path_buf()];
    if let Some(path) = path {
        parts.extend(std::env::split_paths(&path).filter(|p| p != dir));
    }
    std::env::join_paths(parts).unwrap_or_else(|_| dir.as_os_str().to_os_string())
}

fn exe_name(name: &str) -> String {
    if cfg!(windows) && !name.ends_with(".exe") {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

/// A tool's binary: the toolbox dir first, then PATH.
pub fn locate(name: &str) -> Option<PathBuf> {
    locate_in(name, &bin_dir(), std::env::var_os("PATH"))
}

fn locate_in(name: &str, dir: &Path, path: Option<OsString>) -> Option<PathBuf> {
    let file = exe_name(name);
    let candidate = dir.join(&file);
    if candidate.is_file() {
        return Some(candidate);
    }
    std::env::split_paths(&path?)
        .map(|p| p.join(&file))
        .find(|p| p.is_file())
}

/// `~/.rocinante/toolbox.toml`: what the toolbox installed.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Records {
    #[serde(default)]
    pub tools: BTreeMap<String, Record>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Record {
    pub version: String,
    /// RFC 3339, UTC.
    pub installed_at: String,
    /// Files placed in the toolbox dir.
    pub bins: Vec<String>,
}

pub fn records_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".rocinante/toolbox.toml"))
}

pub fn load_records(path: &Path) -> Records {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| toml::from_str(&t).ok())
        .unwrap_or_default()
}

pub fn save_records(path: &Path, records: &Records) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = toml::to_string(records).map_err(std::io::Error::other)?;
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

/// Target triples this machine can run, best first. Linux prefers the glibc
/// build and falls back to the static musl one when glibc is older than
/// 2.38 (or absent), as the tools' own installers do.
pub fn target_triples(os: &str, arch: &str, glibc: Option<(u32, u32)>) -> Vec<String> {
    let arch = match arch {
        "x86_64" | "aarch64" => arch,
        _ => return Vec::new(),
    };
    match os {
        "linux" => {
            let gnu = format!("{arch}-unknown-linux-gnu");
            let musl = format!("{arch}-unknown-linux-musl");
            match glibc {
                Some(v) if v >= (2, 38) => vec![gnu, musl],
                _ => vec![musl],
            }
        }
        "macos" => vec![format!("{arch}-apple-darwin")],
        "windows" if arch == "aarch64" => vec![
            "aarch64-pc-windows-msvc".into(),
            "x86_64-pc-windows-msvc".into(),
        ],
        "windows" => vec!["x86_64-pc-windows-msvc".into()],
        _ => Vec::new(),
    }
}

/// glibc version from `ldd --version` ("ldd (GNU libc) 2.42").
pub fn glibc_version() -> Option<(u32, u32)> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    let out = std::process::Command::new("ldd")
        .arg("--version")
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    parse_glibc(text.lines().next()?)
}

fn parse_glibc(line: &str) -> Option<(u32, u32)> {
    if !line.contains("GNU") && !line.contains("GLIBC") {
        return None;
    }
    let version = line.split_whitespace().last()?;
    let (major, minor) = version.split_once('.')?;
    Some((major.parse().ok()?, minor.parse().ok()?))
}

/// The archive to install for one app from a cargo-dist manifest.
#[derive(Debug, Clone, PartialEq)]
pub struct Selected {
    pub archive: String,
    pub triple: String,
    pub version: String,
    pub sha256: Option<String>,
    /// Executable paths inside the archive.
    pub bins: Vec<String>,
}

/// Pick `app`'s executable archive for the first matching triple.
pub fn select_artifact(manifest: &DistManifest, app: &str, triples: &[String]) -> Option<Selected> {
    let release = manifest.releases.iter().find(|r| r.app_name == app)?;
    for triple in triples {
        let prefix = format!("{app}-{triple}.");
        let found = manifest.artifacts.values().find(|a| {
            a.kind == "executable-zip"
                && a.name.starts_with(&prefix)
                && a.target_triples.iter().any(|t| t == triple)
                && (release.artifacts.is_empty() || release.artifacts.contains(&a.name))
        });
        if let Some(artifact) = found {
            let bins: Vec<String> = artifact
                .assets
                .iter()
                .filter(|a| a.kind == "executable")
                .filter_map(|a| a.path.clone().or_else(|| a.name.clone()))
                .collect();
            if bins.is_empty() {
                continue;
            }
            return Some(Selected {
                archive: artifact.name.clone(),
                triple: triple.clone(),
                version: release.app_version.clone(),
                sha256: artifact.checksums.get("sha256").cloned(),
                bins,
            });
        }
    }
    None
}

/// Everything an install will do, resolved from the network before asking.
#[derive(Debug, Clone)]
pub struct Plan {
    pub name: String,
    pub repo: String,
    pub tag: String,
    pub selected: Selected,
    pub size: Option<u64>,
}

impl Plan {
    /// The permission summary: what, how big, from where, to where.
    pub fn describe(&self, dir: &Path) -> String {
        let size = self
            .size
            .map(|b| format!(" ({} MB)", (b + 500_000) / 1_000_000))
            .unwrap_or_default();
        format!(
            "Install {} {}{size} from github.com/{} into {}",
            self.name,
            self.selected.version,
            self.repo,
            display_dir(dir)
        )
    }
}

/// `~/…` for paths under the home directory.
pub fn display_dir(dir: &Path) -> String {
    if let Some(home) = dirs::home_dir()
        && let Ok(rest) = dir.strip_prefix(&home)
    {
        return format!("~/{}", rest.display());
    }
    dir.display().to_string()
}

/// Resolve the latest release and this machine's archive for `spec`.
pub async fn plan(spec: &ToolSpec) -> anyhow::Result<Plan> {
    let release = selfupdate::latest_release(&spec.repo).await?;
    let manifest = selfupdate::dist_manifest(&spec.repo, &release.tag).await?;
    let triples = target_triples(
        std::env::consts::OS,
        std::env::consts::ARCH,
        glibc_version(),
    );
    let selected = select_artifact(&manifest, &spec.name, &triples).with_context(|| {
        format!(
            "{} {} has no build for {}",
            spec.name,
            release.tag,
            triples
                .first()
                .map(String::as_str)
                .unwrap_or("this platform")
        )
    })?;
    Ok(Plan {
        name: spec.name.clone(),
        repo: spec.repo.clone(),
        size: release.asset_size(&selected.archive),
        tag: release.tag,
        selected,
    })
}

/// The outcome of an install, for the tool result and `/toolbox`.
#[derive(Debug, Clone)]
pub struct Installed {
    pub version: String,
    pub bins: Vec<PathBuf>,
    /// First lines of the doctor command's output, when one ran.
    pub doctor: Option<String>,
}

/// Download, verify, extract and place `plan`'s binaries in `dir`, run the
/// doctor command, and record the install. A checksum mismatch aborts
/// before anything is written to `dir`.
pub async fn install(
    plan: &Plan,
    spec: &ToolSpec,
    dir: &Path,
    records: &Path,
) -> anyhow::Result<Installed> {
    let bytes = selfupdate::download_verified(
        &plan.repo,
        &plan.tag,
        &plan.selected.archive,
        plan.selected.sha256.as_deref(),
    )
    .await?;
    let (scratch, _cleanup) = selfupdate::scratch_dir("rocinante-toolbox")?;
    let archive = scratch.join(&plan.selected.archive);
    std::fs::write(&archive, &bytes).context("cannot write archive to scratch")?;
    let unpacked = scratch.join("x");
    std::fs::create_dir(&unpacked)?;
    selfupdate::extract(&archive, &unpacked).await?;
    std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let mut placed = Vec::new();
    for bin in &plan.selected.bins {
        let file_name = Path::new(bin)
            .file_name()
            .with_context(|| format!("bad executable path {bin}"))?;
        let source = find_file(&unpacked, file_name)
            .with_context(|| format!("archive has no {}", file_name.to_string_lossy()))?;
        let target = dir.join(file_name);
        place_executable(&source, &target)?;
        placed.push(target);
    }
    let doctor = match &spec.doctor_cmd {
        Some(cmd) => run_capture(cmd, std::time::Duration::from_secs(60))
            .await
            .map(|out| head_lines(&out, 6))
            .ok(),
        None => None,
    };
    let mut recs = load_records(records);
    recs.tools.insert(
        plan.name.clone(),
        Record {
            version: plan.selected.version.clone(),
            installed_at: now_rfc3339(),
            bins: placed
                .iter()
                .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
                .collect(),
        },
    );
    save_records(records, &recs).context("cannot record the install")?;
    Ok(Installed {
        version: plan.selected.version.clone(),
        bins: placed,
        doctor,
    })
}

/// Delete what the toolbox installed for `name` (never binaries elsewhere
/// on PATH) and its record.
pub fn remove(name: &str, dir: &Path, records: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut recs = load_records(records);
    let Some(record) = recs.tools.remove(name) else {
        bail!("{name} was not installed by the toolbox");
    };
    let mut removed = Vec::new();
    for bin in &record.bins {
        let path = dir.join(bin);
        if path.is_file() {
            std::fs::remove_file(&path)
                .with_context(|| format!("cannot remove {}", path.display()))?;
            removed.push(path);
        }
    }
    save_records(records, &recs)?;
    Ok(removed)
}

fn find_file(root: &Path, name: &std::ffi::OsStr) -> Option<PathBuf> {
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).ok()?.flatten() {
            let path = entry.path();
            let ty = entry.file_type().ok()?;
            if ty.is_dir() {
                stack.push(path);
            } else if ty.is_file() && path.file_name() == Some(name) {
                return Some(path);
            }
        }
    }
    None
}

/// Copy next to the target, mark executable, then rename over it.
fn place_executable(source: &Path, target: &Path) -> anyhow::Result<()> {
    let staging = target.with_file_name(format!(
        ".{}.{}.tmp",
        target.file_name().unwrap_or_default().to_string_lossy(),
        uuid::Uuid::new_v4()
    ));
    std::fs::copy(source, &staging)
        .with_context(|| format!("cannot write {}", staging.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&staging, std::fs::Permissions::from_mode(0o755))?;
    }
    std::fs::rename(&staging, target).inspect_err(|_| {
        let _ = std::fs::remove_file(&staging);
    })?;
    Ok(())
}

/// Run an argv with the toolbox PATH; stdout+stderr on success.
pub async fn run_capture(cmd: &[String], timeout: std::time::Duration) -> anyhow::Result<String> {
    let (program, args) = cmd.split_first().context("empty command")?;
    let program = locate(program).unwrap_or_else(|| PathBuf::from(program));
    let mut command = tokio::process::Command::new(program);
    command
        .args(args)
        .env("PATH", path_with_toolbox())
        .stdin(std::process::Stdio::null());
    let out = tokio::time::timeout(timeout, command.output())
        .await
        .context("timed out")??;
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    if !out.status.success() {
        bail!(
            "{} exited with {}: {}",
            cmd.join(" "),
            out.status,
            head_lines(&text, 4)
        );
    }
    Ok(text)
}

/// The installed version, by running the version command.
pub async fn installed_version(spec: &ToolSpec) -> Option<String> {
    locate(&spec.name)?;
    let out = run_capture(&spec.version_cmd, std::time::Duration::from_secs(15))
        .await
        .ok()?;
    parse_version_output(&out)
}

/// "ferraus 0.1.4" / "pixelox v0.4.0 (abc)" → "0.1.4" / "0.4.0".
pub fn parse_version_output(out: &str) -> Option<String> {
    out.split_whitespace()
        .map(|w| w.trim_start_matches('v').trim_end_matches([',', ')']))
        .find(|w| {
            let parts: Vec<&str> = w.split('.').collect();
            parts.len() >= 2
                && parts
                    .iter()
                    .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
        })
        .map(str::to_string)
}

/// `a` is a newer dotted version than `b`.
pub fn is_newer(a: &str, b: &str) -> bool {
    let parse = |v: &str| -> Vec<u64> {
        v.trim_start_matches('v')
            .split('.')
            .map(|p| p.parse().unwrap_or(0))
            .collect()
    };
    parse(a) > parse(b)
}

fn head_lines(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().take(n).collect();
    let mut out = lines.join("\n");
    if out.len() > 600 {
        let mut cut = 600;
        while !out.is_char_boundary(cut) {
            cut -= 1;
        }
        out.truncate(cut);
        out.push('…');
    }
    out
}

fn now_rfc3339() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = secs / 86_400;
    let rem = secs % 86_400;
    let (y, m, d) = civil_from_days(days as i64);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// Days since 1970-01-01 → (year, month, day), proleptic Gregorian.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// The header a skill gets when a tool it `requires` is missing.
pub fn missing_tool_header(missing: &[String]) -> Option<String> {
    if missing.is_empty() {
        return None;
    }
    let list = missing
        .iter()
        .map(|m| format!("`{m}`"))
        .collect::<Vec<_>>()
        .join(", ");
    let verb = if missing.len() == 1 { "is" } else { "are" };
    Some(match auto_install() {
        AutoInstall::Never => format!(
            "[{list} {verb} not installed, and installing is disabled ([toolbox] auto_install = \"never\"). Tell the user to install it from its GitHub release before you use the commands below.]"
        ),
        _ => {
            let calls = missing
                .iter()
                .map(|m| format!("{{\"action\":\"install\",\"name\":\"{m}\"}}"))
                .collect::<Vec<_>>()
                .join(" then ");
            format!(
                "[{list} {verb} not installed. Call `toolbox` with {calls} before using the commands below. The user will be asked to approve the download.]"
            )
        }
    })
}

/// Tools in `requires` that cannot be found.
pub fn missing(requires: &[String]) -> Vec<String> {
    requires
        .iter()
        .filter(|name| locate(name).is_none())
        .cloned()
        .collect()
}

/// MCP servers to register for installed toolbox tools with `mcp = true`.
pub fn mcp_servers(config: &Config, cwd: &Path) -> Vec<(String, crate::config::McpServerConfig)> {
    registry(config)
        .into_iter()
        .filter(|spec| spec.mcp)
        .filter_map(|spec| {
            let binary = locate(&spec.name)?;
            let args = spec.mcp_args.clone()?;
            let cwd = cwd.display().to_string();
            Some((
                spec.name.clone(),
                crate::config::McpServerConfig {
                    command: Some(binary.display().to_string()),
                    args: args.into_iter().map(|a| a.replace("{cwd}", &cwd)).collect(),
                    ..Default::default()
                },
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const FERRAUS_MANIFEST: &str = include_str!("ferraus-0.1.4-dist-manifest.json");

    fn manifest() -> DistManifest {
        serde_json::from_str(FERRAUS_MANIFEST).unwrap()
    }

    #[test]
    fn triples_prefer_glibc_and_fall_back_to_musl() {
        assert_eq!(
            target_triples("linux", "x86_64", Some((2, 42))),
            vec!["x86_64-unknown-linux-gnu", "x86_64-unknown-linux-musl"]
        );
        assert_eq!(
            target_triples("linux", "x86_64", Some((2, 35))),
            vec!["x86_64-unknown-linux-musl"]
        );
        assert_eq!(
            target_triples("linux", "aarch64", None),
            vec!["aarch64-unknown-linux-musl"],
            "no glibc (Alpine) → static musl"
        );
        assert_eq!(
            target_triples("macos", "aarch64", None),
            vec!["aarch64-apple-darwin"]
        );
        assert_eq!(
            target_triples("windows", "x86_64", None),
            vec!["x86_64-pc-windows-msvc"]
        );
        assert!(target_triples("linux", "riscv64", Some((2, 40))).is_empty());
    }

    #[test]
    fn glibc_line_parses() {
        assert_eq!(parse_glibc("ldd (GNU libc) 2.42"), Some((2, 42)));
        assert_eq!(
            parse_glibc("ldd (Ubuntu GLIBC 2.35-0ubuntu3.8) 2.35"),
            Some((2, 35))
        );
        assert_eq!(parse_glibc("musl libc (x86_64)"), None);
    }

    #[test]
    fn real_ferraus_manifest_selects_per_platform() {
        let m = manifest();
        let gnu = select_artifact(
            &m,
            "ferraus",
            &target_triples("linux", "x86_64", Some((2, 42))),
        )
        .unwrap();
        assert_eq!(gnu.archive, "ferraus-x86_64-unknown-linux-gnu.tar.xz");
        assert_eq!(gnu.version, "0.1.4");
        assert_eq!(
            gnu.sha256.as_deref(),
            Some("522b589732009ddb46a52cce4f7b278f2bf8cc172927678e83d7ae039e1747fb")
        );
        assert_eq!(gnu.bins, vec!["ferraus", "ferraus-plughost"]);

        let musl = select_artifact(
            &m,
            "ferraus",
            &target_triples("linux", "x86_64", Some((2, 31))),
        )
        .unwrap();
        assert_eq!(musl.archive, "ferraus-x86_64-unknown-linux-musl.tar.xz");

        let mac =
            select_artifact(&m, "ferraus", &target_triples("macos", "aarch64", None)).unwrap();
        assert_eq!(mac.archive, "ferraus-aarch64-apple-darwin.tar.xz");

        let win =
            select_artifact(&m, "ferraus", &target_triples("windows", "x86_64", None)).unwrap();
        assert_eq!(
            win.archive, "ferraus-x86_64-pc-windows-msvc.zip",
            "zip, not the msi"
        );
        assert!(win.bins.iter().all(|b| b.ends_with(".exe")));

        // Other apps in the same release are not confused with ferraus.
        let engrave = select_artifact(
            &m,
            "ferraus-engrave",
            &target_triples("linux", "x86_64", Some((2, 42))),
        )
        .unwrap();
        assert_eq!(
            engrave.archive,
            "ferraus-engrave-x86_64-unknown-linux-gnu.tar.xz"
        );
        assert!(select_artifact(&m, "nope", &["x86_64-unknown-linux-gnu".into()]).is_none());
    }

    #[test]
    fn records_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/toolbox.toml");
        assert_eq!(load_records(&path), Records::default());
        let mut recs = Records::default();
        recs.tools.insert(
            "ferraus".into(),
            Record {
                version: "0.1.4".into(),
                installed_at: "2026-11-09T12:00:00Z".into(),
                bins: vec!["ferraus".into(), "ferraus-plughost".into()],
            },
        );
        save_records(&path, &recs).unwrap();
        assert_eq!(load_records(&path), recs);
    }

    #[test]
    fn remove_deletes_only_recorded_files() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        std::fs::write(bin.join("ferraus"), "x").unwrap();
        std::fs::write(bin.join("other"), "x").unwrap();
        let recs_path = dir.path().join("toolbox.toml");
        let mut recs = Records::default();
        recs.tools.insert(
            "ferraus".into(),
            Record {
                version: "0.1.4".into(),
                installed_at: now_rfc3339(),
                bins: vec!["ferraus".into()],
            },
        );
        save_records(&recs_path, &recs).unwrap();
        let removed = remove("ferraus", &bin, &recs_path).unwrap();
        assert_eq!(removed, vec![bin.join("ferraus")]);
        assert!(bin.join("other").exists());
        assert!(load_records(&recs_path).tools.is_empty());
        assert!(
            remove("ferraus", &bin, &recs_path).is_err(),
            "not installed any more"
        );
    }

    #[test]
    fn path_prefix_puts_the_toolbox_first_once() {
        let dir = PathBuf::from("/opt/tb");
        let path = std::env::join_paths(["/usr/bin", "/opt/tb", "/bin"]).unwrap();
        let joined = prepend_path(&dir, Some(path));
        let parts: Vec<PathBuf> = std::env::split_paths(&joined).collect();
        assert_eq!(
            parts,
            vec![PathBuf::from("/opt/tb"), "/usr/bin".into(), "/bin".into()]
        );
    }

    #[test]
    fn locate_prefers_the_toolbox_dir() {
        let dir = tempfile::tempdir().unwrap();
        let tb = dir.path().join("tb");
        let other = dir.path().join("other");
        std::fs::create_dir_all(&tb).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join(exe_name("mytool")), "x").unwrap();
        let path = std::env::join_paths([&other]).unwrap();
        assert_eq!(
            locate_in("mytool", &tb, Some(path.clone())),
            Some(other.join(exe_name("mytool")))
        );
        std::fs::write(tb.join(exe_name("mytool")), "x").unwrap();
        assert_eq!(
            locate_in("mytool", &tb, Some(path)),
            Some(tb.join(exe_name("mytool")))
        );
        assert_eq!(locate_in("absent", &tb, None), None);
    }

    #[test]
    fn versions_parse_and_compare() {
        assert_eq!(
            parse_version_output("ferraus 0.1.4\n").as_deref(),
            Some("0.1.4")
        );
        assert_eq!(
            parse_version_output("pixelox v0.4.0 (e0b319a)").as_deref(),
            Some("0.4.0")
        );
        assert_eq!(parse_version_output("no version here"), None);
        assert!(is_newer("0.1.10", "0.1.4"));
        assert!(!is_newer("0.4.0", "0.4.0"));
        assert!(is_newer("v1.0.0", "0.9.9"));
    }

    #[test]
    fn plan_description_names_size_source_and_target() {
        let plan = Plan {
            name: "ferraus".into(),
            repo: "djynnius/ferraus".into(),
            tag: "v0.1.4".into(),
            size: Some(55_200_000),
            selected: Selected {
                archive: "ferraus-x86_64-unknown-linux-gnu.tar.xz".into(),
                triple: "x86_64-unknown-linux-gnu".into(),
                version: "0.1.4".into(),
                sha256: None,
                bins: vec!["ferraus".into()],
            },
        };
        assert_eq!(
            plan.describe(Path::new("/opt/tb")),
            "Install ferraus 0.1.4 (55 MB) from github.com/djynnius/ferraus into /opt/tb"
        );
    }

    #[test]
    fn registry_merges_user_tools() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("p.toml");
        std::fs::write(
            &project,
            "[toolbox.tools.pixelox]\nmcp = true\n[toolbox.tools.mytool]\nrepo = \"me/mytool\"\n[toolbox.tools.norepo]\nmcp = true\n",
        )
        .unwrap();
        let config = crate::config::load_from(&dir.path().join("none.toml"), &project).unwrap();
        let specs = registry(&config);
        let pixelox = specs.iter().find(|s| s.name == "pixelox").unwrap();
        assert!(pixelox.mcp);
        assert_eq!(pixelox.repo, "djynnius/pixelox");
        let mine = specs.iter().find(|s| s.name == "mytool").unwrap();
        assert_eq!(mine.version_cmd, vec!["mytool", "--version"]);
        assert!(specs.iter().all(|s| s.name != "norepo"));
    }

    #[test]
    fn header_names_the_install_call() {
        let h = missing_tool_header(&["ferraus".into()]).unwrap();
        assert!(h.contains("`ferraus` is not installed"));
        assert!(h.contains(r#"{"action":"install","name":"ferraus"}"#));
        assert!(h.contains("asked to approve"));
        assert!(missing_tool_header(&[]).is_none());
    }

    #[test]
    fn rfc3339_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(20_401), (2025, 11, 9));
        assert!(now_rfc3339().ends_with('Z'));
    }
}
