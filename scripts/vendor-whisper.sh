#!/bin/sh
# Refresh the whisper.cpp sources that `/talk` compiles into Rocinante:
#
#   scripts/vendor-whisper.sh 1.9.5
#
# whisper.cpp is not built with its own copy of ggml: crates/rocinante-llama
# compiles whisper.cpp against the ggml that llama-cpp-sys-2 already builds
# and links, so the binary carries one ggml (and Macs get Metal for speech
# recognition for free). That is only safe when the release's ggml tree is
# byte-identical to the llama.cpp one, so this script refuses to vendor a
# release whose ggml differs. When bumping llama-cpp-sys-2, pick the
# whisper.cpp release whose ggml matches (compare ggml/CMakeLists.txt's
# GGML_VERSION_* lines) and run this again. Needs curl; only maintainers run
# it — the output is committed.
#
# Writes: crates/rocinante-llama/whisper/{whisper.h,whisper.cpp,whisper-arch.h,LICENSE,upstream-version.txt}
#         and whisper/aneforge/ (an opt-in Apple Neural Engine encoder whisper.cpp
#         always compiles; it stays off unless ANEFORGE_ENCODER is set)
set -eu

VERSION="${1:?usage: scripts/vendor-whisper.sh <whisper.cpp version, e.g. 1.9.5>}"

cd "$(dirname "$0")/.."
OUT="crates/rocinante-llama/whisper"
SYS_VERSION=$(sed -n 's/^const SYS_VERSION: &str = "\(.*\)";/\1/p' crates/rocinante-llama/build.rs)
LLAMA="${ROCINANTE_LLAMA_SRC:-}"
if [ -z "$LLAMA" ]; then
    for dir in "${CARGO_HOME:-$HOME/.cargo}"/registry/src/*/llama-cpp-sys-2-"$SYS_VERSION"/llama.cpp; do
        [ -d "$dir/ggml" ] && LLAMA="$dir"
    done
fi
[ -n "$LLAMA" ] || {
    echo "llama-cpp-sys-2 $SYS_VERSION sources not found: run \`cargo fetch\` first (or set ROCINANTE_LLAMA_SRC)" >&2
    exit 1
}

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
curl -fsSL "https://github.com/ggml-org/whisper.cpp/archive/refs/tags/v$VERSION.tar.gz" |
    tar -xz -C "$WORK"
SRC="$WORK/whisper.cpp-$VERSION"

if ! diff -rq "$SRC/ggml/include" "$LLAMA/ggml/include" >/dev/null ||
    ! diff -rq "$SRC/ggml/src" "$LLAMA/ggml/src" >/dev/null; then
    echo "whisper.cpp v$VERSION's ggml differs from llama-cpp-sys-2 $SYS_VERSION's — pick the release whose ggml matches" >&2
    exit 1
fi

mkdir -p "$OUT"
cp "$SRC/include/whisper.h" "$SRC/src/whisper.cpp" "$SRC/src/whisper-arch.h" "$SRC/LICENSE" "$OUT/"
mkdir -p "$OUT/aneforge"
cp "$SRC/src/aneforge/whisper-aneforge.h" "$SRC/src/aneforge/whisper-aneforge.cpp" "$OUT/aneforge/"
echo "$VERSION" > "$OUT/upstream-version.txt"
echo "vendored whisper.cpp v$VERSION (ggml identical to llama-cpp-sys-2 $SYS_VERSION) into $OUT"
