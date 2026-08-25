#!/usr/bin/env bash
# Copy the complete main Foreseer settings profile into a standalone profile.
# This includes Jellyfin, Radarr, Sonarr, notification, and linked providers;
# run it only when the standalone is intentionally meant to use those live
# services.
set -euo pipefail

REMOTE="${FORESEER_MAIN_REMOTE:-root@pve}"
CONTAINER="${FORESEER_MAIN_CONTAINER:-420}"
TEST_ROOT="${FORESEER_TEST_ROOT:-$HOME/.local/share/foreseer-desktop-test}"
SETTINGS="$TEST_ROOT/standalone/settings.json"

if [[ ! -f "$SETTINGS" ]]; then
  echo "Standalone settings not found: $SETTINGS" >&2
  exit 1
fi

umask 077
payload="$(mktemp)"
trap 'rm -f "$payload"' EXIT

# Keep sensitive values out of terminal output. The payload is written with
# owner-only permissions and contains the full settings profile.
tailscale ssh "$REMOTE" \
  "pct exec $CONTAINER -- /run/current-system/sw/bin/cat /opt/foreseer/config/settings.json" \
  >"$payload"

node - "$SETTINGS" "$payload" <<'NODE'
const fs = require('fs');
const [settingsPath, payloadPath] = process.argv.slice(2);
const settings = JSON.parse(fs.readFileSync(payloadPath, 'utf8'));
fs.writeFileSync(settingsPath, `${JSON.stringify(settings, null, 2)}\n`, {
  mode: 0o600,
});
NODE

echo "Seeded the complete main Foreseer settings profile into the standalone profile."
