#!/usr/bin/env bash
# Omarchy desktop launcher for the debug binary. Captures stdout/stderr and
# points Foreseer's rotating log at $ROOT/logs so crashes are not lost.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DEBUG="$ROOT/target/debug"
BIN="$DEBUG/foreseer-desktop"
LOG_DIR="$ROOT/logs"

mkdir -p "$LOG_DIR"

shopt -s nullglob
old=( "$LOG_DIR"/launch-*.log )
if (( ${#old[@]} > 20 )); then
  printf '%s\n' "${old[@]}" | sort | head -n $(( ${#old[@]} - 20 )) | xargs -r rm -f
fi

STAMP="$(date +%Y%m%d-%H%M%S)"
LAUNCH_LOG="$LOG_DIR/launch-${STAMP}.log"
APP_LOG="$LOG_DIR/foreseer-desktop.log"

{
  echo "=== Foreseer Desktop debug launch $STAMP pid=$$ ==="
  echo "bin=$BIN"
  echo "app_log=$APP_LOG"
} >"$LAUNCH_LOG"
ln -sfn "$LAUNCH_LOG" "$LOG_DIR/launch-latest.log"

export LD_LIBRARY_PATH="$DEBUG${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
export FORESEER_LOG_FILE="$APP_LOG"
export FORESEER_LOG_LEVEL="${FORESEER_LOG_LEVEL:-debug}"

cd "$ROOT"
exec "$BIN" "$@" >>"$LAUNCH_LOG" 2>&1
