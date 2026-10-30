#!/usr/bin/env bash
# Check the release pins against the Foreseerr checkout a release stages its
# runtime from (scripts/stage-foreseerr.sh).
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
FORESEERR_DIR="${FORESEERR_DIR:-$ROOT/../SeerrSuggestArr}"

fail() {
  echo "check-release-pins: $*" >&2
  exit 1
}

for pin in jellium.rev foreseerr.rev foreseerr.version; do
  [[ -s "$ROOT/$pin" ]] || fail "$pin is empty"
done
for file in package.json scripts/build-runtime.sh backend-rs/Cargo.toml; do
  [[ -f "$FORESEERR_DIR/$file" ]] || fail "no $file in the Foreseerr checkout at $FORESEERR_DIR"
done
PIN="$(tr -d '[:space:]' < "$ROOT/foreseerr.version")"
VERSION="$(sed -nE 's/^  "version": *"([^"]+)",?[[:space:]]*$/\1/p' "$FORESEERR_DIR/package.json" | head -n1)"
[[ "$PIN" == "$VERSION" ]] || fail "Foreseerr ${VERSION:-<unknown>} does not match foreseerr.version $PIN"
REVISION="$(tr -d '[:space:]' < "$ROOT/foreseerr.rev")"
[[ "$(git -C "$FORESEERR_DIR" rev-parse HEAD)" == "$REVISION" ]] ||
  fail "Foreseerr checkout does not match foreseerr.rev $REVISION"
echo "release pins: Foreseer Desktop $(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -1), Foreseerr $PIN ($REVISION)"
