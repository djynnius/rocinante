#!/bin/sh
# Rocinante installer for Linux and macOS.
#
#   curl -fsSL https://raw.githubusercontent.com/djynnius/rocinante/main/install.sh | sh
#
# Overrides:
#   ROCINANTE_VERSION      release tag to install (default: latest)
#   ROCINANTE_INSTALL_DIR  where the binary goes (default: ~/.local/bin)
#   ROCINANTE_REPO         github owner/repo (default: djynnius/rocinante)
#   ROCINANTE_INSTALL_BASE full URL base for artifacts (testing/mirrors)
set -eu

REPO="${ROCINANTE_REPO:-djynnius/rocinante}"
VERSION="${ROCINANTE_VERSION:-latest}"
INSTALL_DIR="${ROCINANTE_INSTALL_DIR:-$HOME/.local/bin}"

say() { printf '%s\n' "$*" >&2; }
fail() {
    say "error: $*"
    exit 1
}

# --- platform detection -----------------------------------------------------
os=$(uname -s)
arch=$(uname -m)
case "$os" in
    Linux) os_part="unknown-linux-musl" ;;
    Darwin) os_part="apple-darwin" ;;
    MINGW* | MSYS* | CYGWIN*)
        fail "this is the unix installer. On Windows, run:
  powershell -c \"irm https://raw.githubusercontent.com/$REPO/main/install.ps1 | iex\"" ;;
    *) fail "unsupported operating system: $os" ;;
esac
case "$arch" in
    x86_64 | amd64) arch_part="x86_64" ;;
    aarch64 | arm64) arch_part="aarch64" ;;
    *) fail "unsupported architecture: $arch" ;;
esac
target="${arch_part}-${os_part}"
if [ "$target" = "aarch64-unknown-linux-musl" ] || [ "$target" = "x86_64-unknown-linux-musl" ] \
    || [ "$target" = "x86_64-apple-darwin" ] || [ "$target" = "aarch64-apple-darwin" ]; then
    :
else
    fail "no prebuilt binary for $target — try 'cargo install' from source"
fi
archive="rocinante-${target}.tar.gz"

# --- URL base ----------------------------------------------------------------
if [ -n "${ROCINANTE_INSTALL_BASE:-}" ]; then
    base="$ROCINANTE_INSTALL_BASE"
elif [ "$VERSION" = "latest" ]; then
    base="https://github.com/$REPO/releases/latest/download"
else
    base="https://github.com/$REPO/releases/download/$VERSION"
fi

# --- fetch tooling -----------------------------------------------------------
if command -v curl >/dev/null 2>&1; then
    fetch() { curl -fsSL -o "$2" "$1"; }
elif command -v wget >/dev/null 2>&1; then
    fetch() { wget -qO "$2" "$1"; }
else
    fail "need curl or wget"
fi
if command -v sha256sum >/dev/null 2>&1; then
    checksum() { sha256sum "$1" | cut -d' ' -f1; }
elif command -v shasum >/dev/null 2>&1; then
    checksum() { shasum -a 256 "$1" | cut -d' ' -f1; }
else
    fail "need sha256sum or shasum to verify the download"
fi

# --- download + verify + install ----------------------------------------------
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

say "downloading $archive ($VERSION)…"
fetch "$base/$archive" "$tmp/$archive" || fail "download failed: $base/$archive
(is there a published release yet?)"
fetch "$base/SHA256SUMS" "$tmp/SHA256SUMS" || fail "download failed: $base/SHA256SUMS"

expected=$(grep " $archive\$" "$tmp/SHA256SUMS" | cut -d' ' -f1)
[ -n "$expected" ] || fail "$archive not listed in SHA256SUMS"
actual=$(checksum "$tmp/$archive")
[ "$expected" = "$actual" ] || fail "checksum mismatch for $archive
  expected: $expected
  actual:   $actual
Refusing to install."
say "checksum verified."

tar -xzf "$tmp/$archive" -C "$tmp"
[ -f "$tmp/rocinante" ] || fail "archive did not contain the rocinante binary"
chmod +x "$tmp/rocinante"
mkdir -p "$INSTALL_DIR"
mv "$tmp/rocinante" "$INSTALL_DIR/rocinante"

say "installed: $("$INSTALL_DIR/rocinante" --version) → $INSTALL_DIR/rocinante"

# --- PATH hint -----------------------------------------------------------------
case ":$PATH:" in
    *":$INSTALL_DIR:"*) ;;
    *)
        say ""
        say "note: $INSTALL_DIR is not on your PATH. Add it:"
        case "${SHELL:-}" in
            */fish) say "  fish_add_path $INSTALL_DIR" ;;
            */zsh) say "  echo 'export PATH=\"$INSTALL_DIR:\$PATH\"' >> ~/.zshrc" ;;
            *) say "  echo 'export PATH=\"$INSTALL_DIR:\$PATH\"' >> ~/.bashrc" ;;
        esac
        ;;
esac
