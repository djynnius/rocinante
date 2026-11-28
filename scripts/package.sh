#!/bin/sh
# Generate package-manager manifests (Homebrew formula, scoop manifest,
# winget manifests) from a published release's SHA256SUMS.
#
#   scripts/package.sh v0.1.1
#
# Writes: dist-pkg/rocinante.rb, dist-pkg/rocinante.json, dist-pkg/winget/<version>/.
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
WIN_ARM=$(sha "aarch64-pc-windows-msvc.zip")
[ -n "$MAC_ARM" ] && [ -n "$MAC_X64" ] && [ -n "$LNX_ARM" ] && [ -n "$LNX_X64" ] && [ -n "$WIN_X64" ] && [ -n "$WIN_ARM" ] ||
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
        },
        "arm64": {
            "url": "$BASE/rocinante-aarch64-pc-windows-msvc.zip",
            "hash": "$WIN_ARM"
        }
    },
    "bin": "rocinante.exe",
    "checkver": { "github": "https://github.com/$REPO" },
    "autoupdate": {
        "architecture": {
            "64bit": {
                "url": "https://github.com/$REPO/releases/download/v\$version/rocinante-x86_64-pc-windows-msvc.zip"
            },
            "arm64": {
                "url": "https://github.com/$REPO/releases/download/v\$version/rocinante-aarch64-pc-windows-msvc.zip"
            }
        }
    }
}
EOF

# Winget: the community repo takes only multi-file manifests (version,
# installer, defaultLocale), with CRLF line endings. The directory goes to
# manifests/d/Djynnius/Rocinante/$VERSION/ in microsoft/winget-pkgs; see
# docs/INSTALL_HOSTING.md. rocinante.exe links the C runtime statically
# (.cargo/config.toml), so the installers declare no VCRedist dependency.
WG="$OUT/winget/$VERSION"
WG_ID="Djynnius.Rocinante"
WG_SCHEMA="1.28.0"
rm -rf "$OUT/winget" && mkdir -p "$WG"
crlf() { sed 's/$/\r/' > "$1"; }
upper() { printf '%s' "$1" | tr 'a-f' 'A-F'; }

crlf "$WG/$WG_ID.yaml" <<EOF
# yaml-language-server: \$schema=https://aka.ms/winget-manifest.version.$WG_SCHEMA.schema.json

PackageIdentifier: $WG_ID
PackageVersion: $VERSION
DefaultLocale: en-US
ManifestType: version
ManifestVersion: $WG_SCHEMA
EOF

crlf "$WG/$WG_ID.installer.yaml" <<EOF
# yaml-language-server: \$schema=https://aka.ms/winget-manifest.installer.$WG_SCHEMA.schema.json

PackageIdentifier: $WG_ID
PackageVersion: $VERSION
InstallerType: zip
NestedInstallerType: portable
NestedInstallerFiles:
- RelativeFilePath: rocinante.exe
  PortableCommandAlias: rocinante
Installers:
- Architecture: x64
  InstallerUrl: $BASE/rocinante-x86_64-pc-windows-msvc.zip
  InstallerSha256: $(upper "$WIN_X64")
- Architecture: arm64
  InstallerUrl: $BASE/rocinante-aarch64-pc-windows-msvc.zip
  InstallerSha256: $(upper "$WIN_ARM")
ManifestType: installer
ManifestVersion: $WG_SCHEMA
EOF

crlf "$WG/$WG_ID.locale.en-US.yaml" <<EOF
# yaml-language-server: \$schema=https://aka.ms/winget-manifest.defaultLocale.$WG_SCHEMA.schema.json

PackageIdentifier: $WG_ID
PackageVersion: $VERSION
PackageLocale: en-US
Publisher: djynnius
PublisherUrl: https://github.com/${REPO%%/*}
PublisherSupportUrl: https://github.com/$REPO/issues
PackageName: Rocinante
PackageUrl: https://github.com/$REPO
License: MIT
LicenseUrl: https://github.com/$REPO/blob/main/LICENSE
ShortDescription: Terminal coding agent for local models with MCP, LSP, and subagents
Moniker: rocinante
Tags:
- ai
- agent
- cli
- coding
- llm
- terminal
ReleaseNotesUrl: https://github.com/$REPO/releases/tag/$TAG
ManifestType: defaultLocale
ManifestVersion: $WG_SCHEMA
EOF

echo "wrote $OUT/rocinante.rb, $OUT/rocinante.json, $WG/ (winget)"
