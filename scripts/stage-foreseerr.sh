#!/usr/bin/env bash
# Stage the pinned Foreseerr runtime for a desktop build:
#
#   <dest>/foreseerr/                 the runtime directory the supervisor starts:
#     foreseerr (foreseerr.exe)       the Rust backend binary
#     seerr-api.yml, dist/public/,    files it reads from its app root
#     committag.json
#   <dest>/THIRD_PARTY_NOTICES.txt
#
# usage: scripts/stage-foreseerr.sh [dest]     (default: resources/)
#
# The runtime comes from a clean Foreseerr checkout at foreseerr.rev whose
# package.json version is foreseerr.version, built with the checkout's
# scripts/build-runtime.sh. That needs bun with the checkout's dependencies
# installed (bun install --frozen-lockfile) and the Rust toolchain pinned in
# its backend-rs/rust-toolchain.toml.
#
#   FORESEERR_DIR          the checkout (default: ../SeerrSuggestArr)
#   FORESEERR_TARGET       Rust target triple to build for (default: the host)
#   FORESEERR_RUNTIME_DIR  stage this prebuilt runtime directory instead of
#                          building one, e.g. an extracted foreseerr-<platform>
#                          release archive; its committag.json must name
#                          foreseerr.rev. The checkout is still required for
#                          the pin checks and the notices.
#
# Paths may be POSIX-style under Git Bash on Windows.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SOURCE="${FORESEERR_DIR:-$ROOT/../SeerrSuggestArr}"
DEST="${1:-$ROOT/resources}"
VERSION_PIN="$(tr -d '[:space:]' < "$ROOT/foreseerr.version")"
REVISION_PIN="$(tr -d '[:space:]' < "$ROOT/foreseerr.rev")"

fail() {
  echo "stage-foreseerr: $*" >&2
  exit 1
}

# Native tools (node, bun) on Windows need Windows paths.
native_path() {
  if command -v cygpath >/dev/null 2>&1; then
    cygpath -m "$1"
  else
    printf '%s\n' "$1"
  fi
}

[[ -f "$SOURCE/package.json" && -f "$SOURCE/scripts/build-runtime.sh" ]] ||
  fail "no Foreseerr checkout with scripts/build-runtime.sh at $SOURCE"
SOURCE="$(cd "$SOURCE" && pwd)"
[[ "$(git -C "$SOURCE" rev-parse HEAD)" == "$REVISION_PIN" ]] ||
  fail "Foreseerr checkout does not match foreseerr.rev"
if ! git -C "$SOURCE" diff --quiet || ! git -C "$SOURCE" diff --cached --quiet; then
  fail "Foreseerr checkout must be clean"
fi
# The first "version" key at the top level of package.json.
VERSION="$(sed -nE 's/^  "version": *"([^"]+)",?[[:space:]]*$/\1/p' "$SOURCE/package.json" | head -n1)"
[[ "$VERSION" == "$VERSION_PIN" ]] ||
  fail "Foreseerr ${VERSION:-<unknown>} does not match foreseerr.version $VERSION_PIN"

NODE="$(command -v node || command -v bun || true)"
[[ -n "$NODE" ]] || fail "node or bun is required to write the notices"
TARGET="${FORESEERR_TARGET:-$(rustc -vV | sed -n 's/^host: //p')}"
[[ -n "$TARGET" ]] || fail "could not determine the Rust host target"
EXE=foreseerr
[[ "$TARGET" == *windows* ]] && EXE=foreseerr.exe

mkdir -p "$DEST"
DEST="$(cd "$DEST" && pwd)"
RUNTIME="$DEST/foreseerr"
# A desktop tree staged before the Rust backend also holds a Node runtime.
rm -rf "$RUNTIME" "$DEST/node"

if [[ -n "${FORESEERR_RUNTIME_DIR:-}" ]]; then
  PREBUILT="$(cd "$FORESEERR_RUNTIME_DIR" && pwd)"
  COMMIT="$(sed -nE 's/.*"commitTag": *"([^"]*)".*/\1/p' "$PREBUILT/committag.json" 2>/dev/null || true)"
  [[ "$COMMIT" == "$REVISION_PIN" ]] ||
    fail "$PREBUILT was built from ${COMMIT:-<unknown>}, not foreseerr.rev"
  mkdir -p "$RUNTIME"
  cp -R "$PREBUILT/." "$RUNTIME/"
else
  [[ -d "$SOURCE/node_modules" ]] ||
    fail "install Foreseerr's dependencies first (bun install --frozen-lockfile in $SOURCE)"
  target_args=()
  [[ -n "${FORESEERR_TARGET:-}" ]] && target_args=(--target "$FORESEERR_TARGET")
  # The desktop serves Foreseerr at the root, unlike the Jellyfin plugin.
  env -u FORESEERR_BASE_PATH COMMIT_TAG="$REVISION_PIN" \
    bash "$SOURCE/scripts/build-runtime.sh" "$RUNTIME" ${target_args[@]+"${target_args[@]}"}
fi

for file in "$EXE" seerr-api.yml dist/public/index.html committag.json; do
  [[ -f "$RUNTIME/$file" ]] || fail "staged runtime lacks $file"
done
[[ -x "$RUNTIME/$EXE" ]] || fail "$RUNTIME/$EXE is not executable"

crates="$(mktemp)"
trap 'rm -f "$crates"' EXIT
(cd "$SOURCE/backend-rs" &&
  cargo tree --locked -e normal --target "$TARGET" --prefix none --format '{p}|{l}|{r}') > "$crates"
"$NODE" "$(native_path "$ROOT/scripts/generate-third-party-notices.mjs")" \
  "$(native_path "$SOURCE")" "$(native_path "$crates")" \
  "$(native_path "$DEST/THIRD_PARTY_NOTICES.txt")"
[[ -s "$DEST/THIRD_PARTY_NOTICES.txt" ]] || fail "no third-party notices written"
echo "stage-foreseerr: staged Foreseerr $VERSION_PIN ($TARGET) in $RUNTIME"
