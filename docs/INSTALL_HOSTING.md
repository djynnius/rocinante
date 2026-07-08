# Shipping the one-line installers

The installer scripts (`install.sh`, `install.ps1`) download prebuilt
binaries from **GitHub Releases** and verify them against the release's
`SHA256SUMS`. Everything below is about getting those releases published
and giving the installers a pretty URL.

## Step 1 — publish the repo and first release (required)

```sh
gh auth login
gh repo create rocinante --public --source . --push
git tag v0.1.0 && git push origin v0.1.0
```

The tag push triggers `.github/workflows/release.yml`, which builds five
targets (Linux x86_64/aarch64 musl, macOS x86_64/aarch64, Windows x86_64),
generates `SHA256SUMS`, attaches the installer scripts to the release, and
then **runs both installers against the fresh release on all three OSes**
as a smoke test.

From that moment these work, no domain needed:

```sh
curl -fsSL https://raw.githubusercontent.com/djynnius/rocinante/main/install.sh | sh
```

```powershell
powershell -c "irm https://raw.githubusercontent.com/djynnius/rocinante/main/install.ps1 | iex"
```

If the repo ends up under a different owner/name, change the `REPO` default
at the top of both scripts (one line each).

## Step 2 — the pretty domain (optional)

Goal: `curl -fsSL https://install.rocinante.io | sh`.

The subtlety: `curl <url> | sh` needs the URL itself to return the script
body. Options, simplest first:

1. **GitHub Pages, path form** — enable Pages on the repo (deploy from
   `main`, root). `https://djynnius.github.io/rocinante/install.sh` serves the
   raw script immediately. Add a `CNAME` file containing
   `install.rocinante.io` and a DNS CNAME record `install.rocinante.io →
   djynnius.github.io`, and you get
   `curl -fsSL https://install.rocinante.io/rocinante/install.sh | sh`.
2. **Bare-domain form** — to make the domain *root* serve the script
   (`https://install.rocinante.io | sh` exactly), Pages must serve the
   script as `index.html`. That works — shells don't care about the name —
   but browsers will render it as text. Standard trick used by several
   projects: a tiny `pages/` branch or directory whose `index.html` IS the
   shell script. `sh` ignores nothing (it's still valid POSIX), browsers
   show the source. Set Pages to deploy that directory, CNAME as above.
3. **A redirect service / Cloudflare rule** — if rocinante.io is on
   Cloudflare, a redirect rule from `install.rocinante.io/*` to the
   raw.githubusercontent URL is two clicks and keeps the repo clean
   (`curl -fsSL` follows redirects).

Recommendation: option 3 if the domain is on Cloudflare, else option 1 and
advertise `https://install.rocinante.io/install.sh` (the `/install.sh`
suffix costs nothing in practice).

## Security posture

- Both installers verify SHA-256 checksums against the release's
  `SHA256SUMS` and refuse on mismatch — tested (a corrupted checksum file
  aborts with both hashes printed and installs nothing).
- The scripts never need sudo: `~/.local/bin` on unix,
  `%LOCALAPPDATA%\Rocinante\bin` on Windows.
- Version pinning: `ROCINANTE_VERSION=v0.1.0 sh install.sh` installs a
  specific tag rather than latest.

## Later (not blocking)

- Homebrew tap (`brew install ikakke/tap/rocinante`), winget and scoop
  manifests — all consume the same release artifacts.
- crates.io: `cargo install rocinante-cli` as the from-source fallback.
- cargo-dist can replace the hand-rolled workflow wholesale if maintaining
  it ever becomes a chore.
