//! `/update`: user-invoked self-update against GitHub releases. Never runs
//! unless the user asks; verifies SHA-256 against the published SHA256SUMS
//! before the executable is touched; every failure mode leaves a runnable
//! binary. Package-manager installs (brew/scoop) are deferred to their
//! package manager instead of being overwritten.

use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use sha2::{Digest, Sha256};

const REPO: &str = "djynnius/rocinante";
pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Detected package-manager install; brew/scoop own their binaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Guard {
    Homebrew,
    Scoop,
    CargoBin,
}

impl Guard {
    /// What to tell the user instead of (brew/scoop) or alongside (cargo)
    /// updating in place.
    pub fn advice(&self) -> &'static str {
        match self {
            Guard::Homebrew => "installed via Homebrew — run: brew upgrade rocinante",
            Guard::Scoop => "installed via Scoop — run: scoop update rocinante",
            Guard::CargoBin => {
                "note: this looks like a cargo install — a future `cargo install` will overwrite the updated binary"
            }
        }
    }

    /// Hard stop (package manager owns the file) vs advisory note.
    pub fn blocks_update(&self) -> bool {
        matches!(self, Guard::Homebrew | Guard::Scoop)
    }

    /// What to tell the user for `/uninstall`: brew/scoop own the file (we
    /// won't delete it); a cargo install is removable but `cargo uninstall`
    /// is tidier.
    pub fn uninstall_advice(&self) -> &'static str {
        match self {
            Guard::Homebrew => "installed via Homebrew — run: brew uninstall rocinante",
            Guard::Scoop => "installed via Scoop — run: scoop uninstall rocinante",
            Guard::CargoBin => {
                "note: this is a cargo install — `cargo uninstall rocinante` is the tidy way (leaves no stale registry entry)"
            }
        }
    }
}

/// The running executable's real path (symlinks resolved) and how it looks
/// to have been installed.
pub fn current_exe_classified() -> anyhow::Result<(PathBuf, Option<Guard>)> {
    let exe = std::env::current_exe().context("cannot locate the running executable")?;
    let exe = exe.canonicalize().unwrap_or(exe);
    let guard = classify_install(&exe, dirs::home_dir().as_deref());
    Ok((exe, guard))
}

#[derive(Debug, PartialEq)]
pub enum UpdateCheck {
    UpToDate {
        current: String,
    },
    Available {
        current: String,
        latest: String,
        tag: String,
    },
}

/// One GET to GitHub's latest-release endpoint; no retries (rate limits).
pub async fn check() -> anyhow::Result<UpdateCheck> {
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let resp = client(30)?
        .get(&url)
        .send()
        .await
        .context("cannot reach github.com — check your network")?;
    if resp.status() == 403 || resp.status() == 429 {
        bail!("GitHub rate limit hit — try again in a few minutes");
    }
    let resp = resp.error_for_status().context("release lookup failed")?;
    let body: serde_json::Value = resp.json().await.context("bad release metadata")?;
    let tag = body["tag_name"]
        .as_str()
        .context("release metadata has no tag_name")?
        .to_string();
    let latest = tag.trim_start_matches('v').to_string();
    parse_version(&latest)
        .with_context(|| format!("cannot parse release version from tag `{tag}`"))?;
    if is_newer(&latest, CURRENT_VERSION) {
        Ok(UpdateCheck::Available {
            current: CURRENT_VERSION.to_string(),
            latest,
            tag,
        })
    } else {
        Ok(UpdateCheck::UpToDate {
            current: CURRENT_VERSION.to_string(),
        })
    }
}

/// Download the release asset for this platform, verify its SHA-256 against
/// the published SHA256SUMS, and atomically replace `exe_path`. The old
/// binary is never touched until the new one is verified and staged next to
/// it (same filesystem).
pub async fn apply(tag: &str, exe_path: &Path) -> anyhow::Result<()> {
    let asset = asset_for(std::env::consts::OS, std::env::consts::ARCH).with_context(|| {
        format!(
            "no prebuilt binary for {}/{} — build from source",
            std::env::consts::OS,
            std::env::consts::ARCH
        )
    })?;

    // A leftover .old from a previous Windows update is now replaceable.
    if cfg!(windows) {
        let _ = std::fs::remove_file(sibling(exe_path, "old"));
    }

    // Unpredictable name + exclusive create (fails if it already exists), so a
    // local attacker can't pre-create the dir to tamper with the extracted
    // binary between extraction and the swap.
    let scratch = std::env::temp_dir().join(format!("rocinante-update-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&scratch).context("cannot create scratch directory")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&scratch, std::fs::Permissions::from_mode(0o700))
            .context("cannot secure scratch directory")?;
    }
    let _cleanup = RemoveOnDrop(scratch.clone());

    let base = format!("https://github.com/{REPO}/releases/download/{tag}");
    let http = client(300)?;
    let sums = http
        .get(format!("{base}/SHA256SUMS"))
        .send()
        .await
        .context("cannot download SHA256SUMS")?
        .error_for_status()
        .context("SHA256SUMS missing from the release")?
        .text()
        .await?;
    let resp = http
        .get(format!("{base}/{asset}"))
        .send()
        .await
        .with_context(|| format!("cannot download {asset}"))?;
    if resp.status() == 404 {
        bail!("release {tag} has no asset {asset}");
    }
    let bytes = resp
        .error_for_status()
        .with_context(|| format!("download of {asset} failed"))?
        .bytes()
        .await
        .context("download interrupted")?;

    let expected =
        checksum_for(&sums, asset).with_context(|| format!("{asset} not listed in SHA256SUMS"))?;
    let actual = hex(&Sha256::digest(&bytes));
    if actual != expected {
        bail!(
            "checksum mismatch for {asset}\n  expected: {expected}\n  actual:   {actual}\nRefusing to update."
        );
    }

    let archive = scratch.join(asset);
    std::fs::write(&archive, &bytes).context("cannot write archive to scratch")?;
    extract(&archive, &scratch).await?;
    let bin_name = if cfg!(windows) {
        "rocinante.exe"
    } else {
        "rocinante"
    };
    let new_bin = scratch.join(bin_name);
    if !new_bin.exists() {
        bail!("archive did not contain {bin_name}");
    }

    swap(&new_bin, exe_path)
}

/// Stage next to the target (same filesystem), then atomic rename(s).
fn swap(new_bin: &Path, exe_path: &Path) -> anyhow::Result<()> {
    let staging = sibling(exe_path, "new");
    if let Err(e) = std::fs::copy(new_bin, &staging) {
        let _ = std::fs::remove_file(&staging);
        return Err(anyhow::Error::from(e)).with_context(|| {
            format!(
                "cannot write to {}",
                exe_path.parent().unwrap_or(Path::new("?")).display()
            )
        });
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(e) = std::fs::set_permissions(&staging, std::fs::Permissions::from_mode(0o755)) {
            let _ = std::fs::remove_file(&staging);
            return Err(anyhow::Error::from(e).context("cannot mark the new binary executable"));
        }
        // Atomic same-directory rename; the running process keeps its
        // (now unlinked) inode and is unaffected.
        if let Err(e) = std::fs::rename(&staging, exe_path) {
            let _ = std::fs::remove_file(&staging);
            return Err(anyhow::Error::from(e).context("cannot replace the executable"));
        }
        Ok(())
    }
    #[cfg(windows)]
    {
        // A running exe cannot be overwritten but CAN be renamed.
        let old = sibling(exe_path, "old");
        if let Err(e) = std::fs::rename(exe_path, &old) {
            let _ = std::fs::remove_file(&staging);
            return Err(anyhow::Error::from(e).context("cannot move the current executable aside"));
        }
        if let Err(e) = std::fs::rename(&staging, exe_path) {
            return match std::fs::rename(&old, exe_path) {
                Ok(()) => Err(anyhow::Error::from(e)
                    .context("update failed; the previous version was restored")),
                Err(_) => Err(anyhow::Error::from(e).context(format!(
                    "update failed mid-swap — manually rename {} back to {}",
                    old.display(),
                    exe_path.display()
                ))),
            };
        }
        // `old` stays (in use by this process); next /update pre-cleans it.
        Ok(())
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = staging;
        anyhow::bail!("self-update is not supported on this platform");
    }
}

async fn extract(archive: &Path, dest: &Path) -> anyhow::Result<()> {
    let status = if cfg!(windows) {
        tokio::process::Command::new("powershell")
            .args(["-NoProfile", "-Command"])
            .arg(format!(
                "Expand-Archive -Force -Path '{}' -DestinationPath '{}'",
                archive.display(),
                dest.display()
            ))
            .status()
            .await
    } else {
        tokio::process::Command::new("tar")
            .arg("-xzf")
            .arg(archive)
            .arg("-C")
            .arg(dest)
            .status()
            .await
    }
    .context("cannot run the archive extractor")?;
    if !status.success() {
        bail!("archive extraction failed ({status})");
    }
    Ok(())
}

fn client(timeout_secs: u64) -> anyhow::Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(concat!("rocinante/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(timeout_secs))
        .build()
        .context("cannot build HTTP client")
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Strict `MAJOR.MINOR.PATCH`, optional leading `v`. Pre-release tags fail
/// on purpose — better a clear error than a wrong comparison.
fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    let s = s.strip_prefix('v').unwrap_or(s);
    let mut parts = s.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

fn is_newer(latest: &str, current: &str) -> bool {
    match (parse_version(latest), parse_version(current)) {
        (Some(l), Some(c)) => l > c,
        _ => false,
    }
}

/// Release asset name for an (os, arch) pair. All Linux maps to the musl
/// triples — the released Linux binaries are static musl builds.
fn asset_for(os: &str, arch: &str) -> Option<&'static str> {
    match (os, arch) {
        ("macos", "x86_64") => Some("rocinante-x86_64-apple-darwin.tar.gz"),
        ("macos", "aarch64") => Some("rocinante-aarch64-apple-darwin.tar.gz"),
        ("linux", "x86_64") => Some("rocinante-x86_64-unknown-linux-musl.tar.gz"),
        ("linux", "aarch64") => Some("rocinante-aarch64-unknown-linux-musl.tar.gz"),
        ("windows", "x86_64") => Some("rocinante-x86_64-pc-windows-msvc.zip"),
        _ => None,
    }
}

/// Find `asset`'s hash in sha256sum-format output (`<hex>  <name>` lines,
/// one or two spaces).
fn checksum_for<'a>(sums: &'a str, asset: &str) -> Option<&'a str> {
    sums.lines().find_map(|line| {
        let mut parts = line.split_whitespace();
        let hash = parts.next()?;
        let name = parts.next()?;
        (name == asset).then_some(hash)
    })
}

/// How the binary at `exe` looks to have been installed.
fn classify_install(exe: &Path, home: Option<&Path>) -> Option<Guard> {
    let text = exe.to_string_lossy();
    if text.contains("/Cellar/") || text.starts_with("/opt/homebrew/") {
        return Some(Guard::Homebrew);
    }
    if exe.components().any(|c| {
        c.as_os_str()
            .to_string_lossy()
            .to_lowercase()
            .contains("scoop")
    }) {
        return Some(Guard::Scoop);
    }
    if let Some(home) = home
        && exe.starts_with(home.join(".cargo/bin"))
    {
        return Some(Guard::CargoBin);
    }
    None
}

/// `rocinante` + "old" → `rocinante.old`; `rocinante.exe` + "old" →
/// `rocinante.exe.old`. Never `with_extension` — that would clobber `.exe`.
fn sibling(exe: &Path, suffix: &str) -> PathBuf {
    let name = exe
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "rocinante".into());
    exe.with_file_name(format!("{name}.{suffix}"))
}

/// Scratch-dir cleanup on every exit path.
struct RemoveOnDrop(PathBuf);
impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

// ---------------------------------------------------------------- uninstall

/// The user-global data directory (`~/.rocinante`).
fn data_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".rocinante"))
}

pub struct UninstallOutcome {
    pub lines: Vec<String>,
    /// The binary was removed, so the caller should exit.
    pub removed: bool,
}

/// A cleanup note the frontends append; the installers only advise PATH edits
/// (Unix) or set the Windows user PATH, so uninstall can't reverse those.
fn path_note(exe: &Path) -> String {
    let dir = exe
        .parent()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    if cfg!(windows) {
        format!("you may remove {dir} from your user PATH (Settings → Environment Variables)")
    } else {
        format!(
            "you may remove {dir} from your PATH (in ~/.zshrc / ~/.bashrc / ~/.config/fish/config.fish)"
        )
    }
}

/// Describe what `/uninstall` would do — deletes nothing.
pub fn uninstall_preview(purge: bool) -> Vec<String> {
    let (exe, guard) = match current_exe_classified() {
        Ok(v) => v,
        Err(e) => return vec![format!("uninstall: {e:#}")],
    };
    if let Some(g) = guard
        && g.blocks_update()
    {
        return vec![
            g.uninstall_advice().to_string(),
            "nothing was removed.".into(),
        ];
    }
    let mut out = vec![format!(
        "this will delete the rocinante binary at {}",
        exe.display()
    )];
    if purge {
        if let Some(d) = data_dir() {
            out.push(format!(
                "…and wipe your data at {} (config, learned rules, trust, logs)",
                d.display()
            ));
        }
    } else {
        out.push("your ~/.rocinante data (config, learned rules, trust) is kept — add --purge to delete it too".into());
    }
    out.push(
        "project .rocinante/ folders (PILOT.md, BRAINBOX.md, sessions) are left untouched.".into(),
    );
    out.push(format!(
        "irreversible. run `/uninstall confirm{}` to proceed.",
        if purge { " --purge" } else { "" }
    ));
    out
}

/// Perform the uninstall. Returns the binary removed and user-facing lines;
/// brew/scoop are refused (their package manager owns the file).
pub fn uninstall_apply(purge: bool) -> anyhow::Result<UninstallOutcome> {
    let (exe, guard) = current_exe_classified()?;
    if let Some(g) = guard
        && g.blocks_update()
    {
        return Ok(UninstallOutcome {
            lines: vec![
                g.uninstall_advice().to_string(),
                "nothing was removed.".into(),
            ],
            removed: false,
        });
    }

    let purge_dir = if purge { data_dir() } else { None };
    remove_binary(&exe, purge_dir.as_deref())?;

    let mut lines = vec![format!("removed the binary at {}", exe.display())];
    if let Some(Guard::CargoBin) = guard {
        lines.push(Guard::CargoBin.uninstall_advice().to_string());
    }
    match &purge_dir {
        Some(d) => lines.push(format!("removed your data at {}", d.display())),
        None => lines.push("kept your ~/.rocinante data (use --purge to remove it).".into()),
    }
    lines.push(path_note(&exe));
    lines.push("rocinante has been uninstalled.".into());
    Ok(UninstallOutcome {
        lines,
        removed: true,
    })
}

/// Delete the running binary (and, when `purge_dir` is set, the data dir).
#[cfg(unix)]
fn remove_binary(exe: &Path, purge_dir: Option<&Path>) -> anyhow::Result<()> {
    // A running process keeps its (now unlinked) inode; deletion is safe.
    std::fs::remove_file(exe).with_context(|| format!("cannot remove {}", exe.display()))?;
    if let Some(dir) = purge_dir {
        let _ = std::fs::remove_dir_all(dir); // best-effort; the log file just unlinks
    }
    Ok(())
}

/// Windows can't delete a running exe, so hand the deletion to a detached
/// `cmd` that waits for this process to exit (releasing the file and log
/// handles) and then deletes.
#[cfg(windows)]
fn remove_binary(exe: &Path, purge_dir: Option<&Path>) -> anyhow::Result<()> {
    let mut script = format!("ping 127.0.0.1 -n 3 >nul & del /f /q \"{}\"", exe.display());
    if let Some(dir) = purge_dir {
        script.push_str(&format!(" & rd /s /q \"{}\"", dir.display()));
    }
    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    std::process::Command::new("cmd")
        .args(["/C", &script])
        .creation_flags(DETACHED_PROCESS)
        .spawn()
        .context("cannot schedule the binary deletion")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uninstall_advice_per_guard() {
        assert!(
            Guard::Homebrew
                .uninstall_advice()
                .contains("brew uninstall")
        );
        assert!(Guard::Scoop.uninstall_advice().contains("scoop uninstall"));
        assert!(
            Guard::CargoBin
                .uninstall_advice()
                .contains("cargo uninstall")
        );
    }

    #[test]
    fn uninstall_preview_describes_and_confirms() {
        // Under `cargo test` the exe lives in target/ (classified as plain),
        // so the preview describes deletion + the confirm gate.
        let out = uninstall_preview(false).join("\n");
        assert!(out.contains("delete the rocinante binary") || out.contains("uninstall:"));
        if out.contains("delete the rocinante binary") {
            assert!(out.contains("/uninstall confirm"));
            assert!(out.contains("--purge"));
            assert!(out.contains("left untouched"));
        }
        let purged = uninstall_preview(true).join("\n");
        if purged.contains("delete the rocinante binary") {
            assert!(purged.contains("confirm --purge"));
        }
    }

    #[test]
    fn parses_strict_semver_only() {
        assert_eq!(parse_version("0.8.0"), Some((0, 8, 0)));
        assert_eq!(parse_version("v0.8.0"), Some((0, 8, 0)));
        assert_eq!(parse_version("v12.34.56"), Some((12, 34, 56)));
        assert_eq!(parse_version("0.8"), None);
        assert_eq!(parse_version("abc"), None);
        assert_eq!(parse_version("0.8.0-rc1"), None);
        assert_eq!(parse_version("0.8.0.1"), None);
    }

    #[test]
    fn newer_comparison() {
        assert!(is_newer("0.8.1", "0.8.0"));
        assert!(is_newer("0.9.0", "0.8.9"));
        assert!(is_newer("1.0.0", "0.99.99"));
        assert!(is_newer("v0.9.0", "0.8.0"));
        assert!(!is_newer("0.8.0", "0.8.0"));
        assert!(!is_newer("0.7.9", "0.8.0"));
        assert!(!is_newer("garbage", "0.8.0"));
    }

    #[test]
    fn asset_names_match_release_artifacts() {
        assert_eq!(
            asset_for("macos", "aarch64"),
            Some("rocinante-aarch64-apple-darwin.tar.gz")
        );
        assert_eq!(
            asset_for("macos", "x86_64"),
            Some("rocinante-x86_64-apple-darwin.tar.gz")
        );
        assert_eq!(
            asset_for("linux", "x86_64"),
            Some("rocinante-x86_64-unknown-linux-musl.tar.gz")
        );
        assert_eq!(
            asset_for("linux", "aarch64"),
            Some("rocinante-aarch64-unknown-linux-musl.tar.gz")
        );
        assert_eq!(
            asset_for("windows", "x86_64"),
            Some("rocinante-x86_64-pc-windows-msvc.zip")
        );
        assert_eq!(asset_for("linux", "riscv64"), None);
        assert_eq!(asset_for("freebsd", "x86_64"), None);
    }

    #[test]
    fn finds_checksums_in_sha256sum_format() {
        let sums = "aaaa  rocinante-x86_64-apple-darwin.tar.gz\n\
                    bbbb rocinante-aarch64-apple-darwin.tar.gz\n\
                    cccc  rocinante-x86_64-pc-windows-msvc.zip\n";
        assert_eq!(
            checksum_for(sums, "rocinante-aarch64-apple-darwin.tar.gz"),
            Some("bbbb")
        );
        assert_eq!(
            checksum_for(sums, "rocinante-x86_64-pc-windows-msvc.zip"),
            Some("cccc")
        );
        assert_eq!(checksum_for(sums, "rocinante-missing.tar.gz"), None);
    }

    #[test]
    fn classifies_install_locations() {
        let home = Path::new("/Users/x");
        let class = |p: &str| classify_install(Path::new(p), Some(home));
        assert_eq!(class("/opt/homebrew/bin/rocinante"), Some(Guard::Homebrew));
        assert_eq!(
            class("/usr/local/Cellar/rocinante/0.8.0/bin/rocinante"),
            Some(Guard::Homebrew)
        );
        assert_eq!(
            class("/Users/x/scoop/apps/rocinante/current/rocinante.exe"),
            Some(Guard::Scoop)
        );
        assert_eq!(
            class("/Users/x/Scoop/apps/rocinante/current/rocinante.exe"),
            Some(Guard::Scoop)
        );
        assert_eq!(
            class("/Users/x/.cargo/bin/rocinante"),
            Some(Guard::CargoBin)
        );
        assert_eq!(class("/Users/x/.local/bin/rocinante"), None);
        assert_eq!(class("/usr/local/bin/rocinante"), None);
    }

    #[test]
    fn sibling_preserves_full_name() {
        assert_eq!(
            sibling(Path::new("/bin/rocinante"), "old"),
            PathBuf::from("/bin/rocinante.old")
        );
        assert_eq!(
            sibling(Path::new("C:/r/rocinante.exe"), "old"),
            PathBuf::from("C:/r/rocinante.exe.old")
        );
    }

    #[test]
    fn hex_encodes_lowercase() {
        assert_eq!(hex(&[0x00, 0xab, 0xff]), "00abff");
    }
}
