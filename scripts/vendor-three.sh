#!/bin/sh
# Rebuild the three.js bundle that the `threejs` built-in skill ships for
# offline use:
#
#   scripts/vendor-three.sh 0.187.0
#
# three.js publishes ES modules only, and browsers refuse to load ES modules
# into a page opened from disk (file://). So this bundles three.js plus the
# addons below into ONE classic script defining a global `THREE`, which
# works on a double-clicked page with no internet. Needs Node/npm; only
# maintainers run it — the output is committed and compiled into the binary.
# The revision is named in builtin/threejs.md; a unit test fails until the
# two match.
#
# Writes: crates/rocinante-core/src/skills/builtin/threejs/{three.min.js,LICENSE}
set -eu

VERSION="${1:?usage: scripts/vendor-three.sh <three version, e.g. 0.187.0>}"
ESBUILD_VERSION="0.28.2"
REVISION=$(printf '%s' "$VERSION" | cut -d. -f2)

cd "$(dirname "$0")/.."
OUT="crates/rocinante-core/src/skills/builtin/threejs"
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

npm install --prefix "$WORK" --no-audit --no-fund --loglevel=error \
    "three@$VERSION" "esbuild@$ESBUILD_VERSION"

# Addons exposed on the global: keep this list in sync with the Rules
# section of builtin/threejs.md.
cat > "$WORK/entry.js" <<'EOF'
export * from 'three';
export { OrbitControls } from 'three/addons/controls/OrbitControls.js';
export { CSS2DRenderer, CSS2DObject } from 'three/addons/renderers/CSS2DRenderer.js';
EOF

mkdir -p "$OUT"
"$WORK/node_modules/.bin/esbuild" "$WORK/entry.js" \
    --bundle --minify --format=iife --global-name=THREE \
    --legal-comments=eof \
    --banner:js="/*! three.js r$REVISION ($VERSION) + OrbitControls, CSS2DRenderer as one classic script (global THREE), bundled for Rocinante by scripts/vendor-three.sh. MIT License, Copyright 2010-present three.js authors. */" \
    --outfile="$OUT/three.min.js"
cp "$WORK/node_modules/three/LICENSE" "$OUT/LICENSE"

echo "wrote $OUT/three.min.js ($(wc -c < "$OUT/three.min.js" | tr -d ' ') bytes) and $OUT/LICENSE"
