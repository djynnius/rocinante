#!/bin/sh
# Refresh the Vue files that the `vuejs` built-in skill ships for offline
# pages:
#
#   scripts/vendor-vue.sh 3.5.45
#
# Vue publishes classic-script builds defining a global `Vue`, which load on
# a page opened from disk (file://), so they are copied verbatim from the npm
# package — no bundling. Both builds ship: the development build reports the
# mistakes the skill's check relies on (e.g. a name missing from setup()'s
# return); the production build is for pages that get hosted. Needs npm;
# only maintainers run it — the output is committed and compiled into the
# binary. The version is named in builtin/vuejs.md; a unit test fails until
# the two match.
#
# Writes: crates/rocinante-core/src/skills/builtin/vuejs/{vue.global.js,vue.global.prod.js,LICENSE}
set -eu

VERSION="${1:?usage: scripts/vendor-vue.sh <vue version, e.g. 3.5.45>}"

cd "$(dirname "$0")/.."
OUT="crates/rocinante-core/src/skills/builtin/vuejs"
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

# npm pack fetches the published tarball without installing or running scripts.
(cd "$WORK" && npm pack --silent "vue@$VERSION" >/dev/null)
tar -xzf "$WORK"/vue-*.tgz -C "$WORK"

mkdir -p "$OUT"
cp "$WORK/package/dist/vue.global.js" "$WORK/package/dist/vue.global.prod.js" \
    "$WORK/package/LICENSE" "$OUT/"

echo "wrote $OUT:"
wc -c "$OUT/vue.global.js" "$OUT/vue.global.prod.js" "$OUT/LICENSE"
