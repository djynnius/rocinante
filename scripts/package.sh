#!/bin/sh
# Generate package-manager manifests (Homebrew formula, scoop manifest,
# winget manifests) from a published release's SHA256SUMS.
#
#   scripts/package.sh v0.1.1
#
# Writes: dist-pkg/rocinante.rb, dist-pkg/rocinante.json, dist-pkg/winget/*.
set -eu

TAG="${1:?usage: scripts/package.sh <tag>}"
REPO="${ROCINANTE_REPO:-djynnius/rocinante}"
VERSION="${TAG#v}"
BASE="https://github.com/$REPO/releases/download/$TAG"
OUT="dist-pkg"

mkdir -p "$OUT/winget"
sums=$(curl -fsSL "$BASE/SHA256SUMS")
sha() { printf '%s\n' "$sums" | grep " rocinante-$1\$" | cut -d' ' -f1; }

MAC_ARM=$(sha "aarch64-apple-darwin.tar.gz")
MAC_X64=$(sha "x86_64-apple-darwin.tar.gz")
LNX_ARM=$(sha "aarch64-unknown-linux-musl.tar.gz")
LNX_X64=$(sha "x86_64-unknown-linux-musl.tar.gz")
WIN_X64=$(sha "x86_64-pc-windows-msvc.zip")
[ -n "$MAC_ARM" ] && [ -n "$MAC_X64" ] && [ -n "$LNX_ARM" ] && [ -n "$LNX_X64" ] && [ -n "$WIN_X64" ] ||
    { echo "error: SHA256SUMS for $TAG is missing an artifact" >&2; exit 1; }

cat > "$OUT/rocinante.rb" <<EOF
class Rocinante < Formula
  desc "Terminal coding agent for local models with MCP, LSP, and subagents"
  homepage "https://github.com/$REPO"
  version "$VERSION"
  license "MIT"

  on_macos do
    on_arm do
      url "$BASE/rocinante-aarch64-apple-darwin.tar.gz"
      sha256 "$MAC_ARM"
    end
    on_intel do
      url "$BASE/rocinante-x86_64-apple-darwin.tar.gz"
      sha256 "$MAC_X64"
    end
  end
  on_linux do
    on_arm do
      url "$BASE/rocinante-aarch64-unknown-linux-musl.tar.gz"
      sha256 "$LNX_ARM"
    end
    on_intel do
      url "$BASE/rocinante-x86_64-unknown-linux-musl.tar.gz"
      sha256 "$LNX_X64"
    end
  end

  def install
    bin.install "rocinante"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/rocinante --version")
  end
end
EOF

cat > "$OUT/rocinante.json" <<EOF
{
    "version": "$VERSION",
    "description": "Terminal coding agent for local models with MCP, LSP, and subagents",
    "homepage": "https://github.com/$REPO",
    "license": "MIT",
    "architecture": {
        "64bit": {
            "url": "$BASE/rocinante-x86_64-pc-windows-msvc.zip",
            "hash": "$WIN_X64"
        }
    },
    "bin": "rocinante.exe",
    "checkver": { "github": "https://github.com/$REPO" },
    "autoupdate": {
        "architecture": {
            "64bit": {
                "url": "https://github.com/$REPO/releases/download/v\$version/rocinante-x86_64-pc-windows-msvc.zip"
            }
        }
    }
}
EOF

cat > "$OUT/winget/Djynnius.Rocinante.yaml" <<EOF
# Submit by PR to https://github.com/microsoft/winget-pkgs under
# manifests/d/Djynnius/Rocinante/$VERSION/ — see docs/INSTALL_HOSTING.md.
PackageIdentifier: Djynnius.Rocinante
PackageVersion: $VERSION
PackageLocale: en-US
Publisher: djynnius
PackageName: Rocinante
License: MIT
ShortDescription: Terminal coding agent for local models with MCP, LSP, and subagents
PackageUrl: https://github.com/$REPO
Installers:
  - Architecture: x64
    InstallerType: zip
    InstallerUrl: $BASE/rocinante-x86_64-pc-windows-msvc.zip
    InstallerSha256: $WIN_X64
    NestedInstallerType: portable
    NestedInstallerFiles:
      - RelativeFilePath: rocinante.exe
        PortableCommandAlias: rocinante
ManifestType: singleton
ManifestVersion: 1.6.0
EOF

echo "wrote $OUT/rocinante.rb, $OUT/rocinante.json, $OUT/winget/Djynnius.Rocinante.yaml"
