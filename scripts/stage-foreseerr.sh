#!/usr/bin/env bash
# Stage a target-native, production-only Foreseerr bundle for a desktop build.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SOURCE="${FORESEERR_DIR:-$ROOT/../SeerrSuggestArr}"
NODE_BIN="${FORESEERR_NODE_BIN:-$(command -v node)}"
DEST="${1:-$ROOT/resources}"
VERSION_PIN="$(tr -d '[:space:]' < "$ROOT/foreseerr.version")"
REVISION_PIN="$(tr -d '[:space:]' < "$ROOT/foreseerr.rev")"
NODE_PIN="$(tr -d '[:space:]' < "$ROOT/node.rev")"
NODE_RUNTIME_NAME="node"
if [[ "${OS:-}" == "Windows_NT" || "${NODE_BIN##*/}" == "node.exe" ]]; then
  NODE_RUNTIME_NAME="node.exe"
fi

if [[ ! -x "$NODE_BIN" ]]; then
  echo "stage-foreseerr: provide FORESEERR_NODE_BIN or install Node 22" >&2
  exit 1
fi
if [[ "$($NODE_BIN --version)" != "$NODE_PIN" ]]; then
  echo "stage-foreseerr: Node $NODE_PIN is required" >&2
  exit 1
fi
if [[ ! -f "$SOURCE/package.json" ]]; then
  echo "stage-foreseerr: no Foreseerr checkout at $SOURCE" >&2
  exit 1
fi
if [[ "$(git -C "$SOURCE" rev-parse HEAD)" != "$REVISION_PIN" ]]; then
  echo "stage-foreseerr: Foreseerr checkout does not match foreseerr.rev" >&2
  exit 1
fi
if ! git -C "$SOURCE" diff --quiet || ! git -C "$SOURCE" diff --cached --quiet; then
  echo "stage-foreseerr: Foreseerr checkout must be clean" >&2
  exit 1
fi
VERSION="$($NODE_BIN -p "require('$SOURCE/package.json').version")"
if [[ "$VERSION" != "$VERSION_PIN" ]]; then
  echo "stage-foreseerr: Foreseerr $VERSION does not match foreseerr.version $VERSION_PIN" >&2
  exit 1
fi

pnpm --dir "$SOURCE" build
rm -rf "$DEST/foreseerr" "$DEST/node"
mkdir -p "$DEST/foreseerr" "$DEST/node"
install -m 0755 "$NODE_BIN" "$DEST/node/$NODE_RUNTIME_NAME"
# `deploy --prod` gives the managed server an isolated, target-native production
# dependency tree. Do not copy the development checkout's node_modules: it
# contains Cypress, compiler tooling, package-manager stores, and host-native
# modules which are unsafe to ship in a release artifact.
pnpm --dir "$SOURCE" --filter foreseerr --prod deploy --legacy "$DEST/foreseerr"
# `pnpm deploy` retains repository-level files beside its production
# node_modules tree. Keep only the runtime manifest before adding the compiled
# application payload; source, docs, CI metadata, and development tooling do
# not belong in a desktop release.
find "$DEST/foreseerr" -mindepth 1 -maxdepth 1 \
  ! -name node_modules \
  ! -name package.json \
  -exec rm -rf {} +
for item in launcher.js dist .next public seerr-api.yml; do
  [[ -e "$SOURCE/$item" ]] && cp -a "$SOURCE/$item" "$DEST/foreseerr/"
done
# Next.js' custom-server production startup still requires a pages/ or app/
# directory to exist even when every route is already compiled in .next.
# Keep an empty runtime marker so the staged desktop bundle can boot without
# shipping the source route tree.
mkdir -p "$DEST/foreseerr/pages"
find "$DEST/foreseerr" -type d \( -name '.cache' -o -name 'cypress' -o -name 'test' -o -name 'tests' \) -prune -exec rm -rf {} +
find "$DEST/foreseerr" -type f \( -name '*.map' -o -name '*.ts' -o -name '*.tsx' -o -name '*.tsbuildinfo' \) -delete
rm -rf "$DEST/foreseerr/.next/cache" "$DEST/foreseerr/.next/turbopack" "$DEST/foreseerr/.next/dev"

# The official Node distribution places its license beside bin/node. Preserve
# it with deterministic notices for every deployed production dependency.
NODE_LICENSE="$(dirname "$NODE_BIN")/../LICENSE"
if [[ -f "$NODE_LICENSE" ]]; then
  install -m 0644 "$NODE_LICENSE" "$DEST/node/LICENSE"
fi
"$NODE_BIN" "$ROOT/scripts/generate-third-party-notices.mjs" \
  "$DEST/foreseerr" "$NODE_PIN" "$DEST/THIRD_PARTY_NOTICES.txt"
test -x "$DEST/node/$NODE_RUNTIME_NAME"
test -f "$DEST/foreseerr/launcher.js"
test -d "$DEST/foreseerr/dist"
test -f "$DEST/THIRD_PARTY_NOTICES.txt"
for excluded in docs .github .husky .vscode Cypress; do
  test ! -e "$DEST/foreseerr/$excluded"
done
if find "$DEST/foreseerr" -path "$DEST/foreseerr/node_modules" -prune -o \
  -type d \( -name '.cache' -o -name 'cypress' -o -name 'test' -o -name 'tests' -o -name 'turbopack' -o -name 'dev' \) \
  -print -quit | grep -q .; then
  echo "stage-foreseerr: development files remain in staged runtime" >&2
  exit 1
fi
echo "stage-foreseerr: staged $VERSION_PIN in $DEST"
