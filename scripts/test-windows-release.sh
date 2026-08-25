#!/usr/bin/env bash
# Fetch a Windows portable ZIP and attach it to the existing libvirt Windows VM.
# Wine/Proton cannot run this build (CEF + mpv). Use the win11 KVM guest.
set -euo pipefail

LIBVIRT_URI="${LIBVIRT_URI:-qemu:///system}"
VM_NAME="${FORESEER_WIN_VM:-win11}"
REPO="${FORESEER_GH_REPO:-selmant/foreseerr-desktop}"
ARTIFACT_NAME="${FORESEER_WIN_ARTIFACT:-foreseer-desktop-windows-x64}"
CACHE="${XDG_CACHE_HOME:-$HOME/.cache}/foreseer-desktop/windows-test"
# qemu:///system runs as libvirt-qemu and cannot traverse $HOME (700).
ISO="${FORESEER_WIN_ISO:-/var/tmp/foreseer-desktop/foreseer.iso}"
CDROM_TARGET="${FORESEER_WIN_CDROM:-sdb}"
ZIPS="$CACHE/zips"
STAMP="$CACHE/iso.stamp"

usage() {
  cat <<'EOF'
Usage: scripts/test-windows-release.sh [options] [zip]

Fetch a Foreseer Desktop Windows x64 ZIP, pack it into a DVD ISO, start the
libvirt Windows VM, insert the ISO, and open virt-viewer.

Cached zips are reused. The ISO lives under /var/tmp so qemu can read it.

virt-viewer fullscreen: Ctrl+Alt (release grab), then F11. Mouse at the top
edge shows the toolbar.

Options:
  --release [TAG]   Use a GitHub release asset (default: latest)
  --run [ID]        Use a workflow artifact (default: latest successful release.yml run)
  --local ZIP       Use an existing zip (same as passing ZIP as the argument)
  --no-viewer       Start/attach only; do not open virt-viewer
  --no-start        Pack and attach only; VM must already be running
  --pack-only       Download and build the ISO; do not touch the VM
  -h, --help        Show this help

Env:
  FORESEER_WIN_VM          libvirt domain (default: win11)
  FORESEER_GH_REPO         GitHub repo (default: selmant/foreseerr-desktop)
  FORESEER_WIN_ISO         ISO path (default: /var/tmp/foreseer-desktop/foreseer.iso)
  LIBVIRT_URI              default qemu:///system

Inside Windows: This PC → FORESEER DVD drive, extract the zip to Desktop,
run foreseer-desktop.exe. CEF will not run cleanly from a read-only path.
QXL is software graphics, so this is a launch/login/play smoke test.
EOF
}

need() {
  local cmd="$1"
  command -v "$cmd" >/dev/null 2>&1 || {
    echo "test-windows-release: missing command: $cmd" >&2
    exit 1
  }
}

virsh_cmd() {
  virsh -c "$LIBVIRT_URI" "$@"
}

log() {
  printf 'test-windows-release: %s\n' "$*"
}

SOURCE="latest-release"
RELEASE_TAG=""
RUN_ID=""
ZIP_PATH=""
OPEN_VIEWER=1
START_VM=1
PACK_ONLY=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    -h|--help)
      usage
      exit 0
      ;;
    --release)
      SOURCE="release"
      if [[ $# -ge 2 && "$2" != -* ]]; then
        RELEASE_TAG="$2"
        shift
      fi
      ;;
    --run)
      SOURCE="run"
      if [[ $# -ge 2 && "$2" != -* && "$2" != *.zip ]]; then
        RUN_ID="$2"
        shift
      fi
      ;;
    --local)
      SOURCE="local"
      ZIP_PATH="${2:-}"
      [[ -n "$ZIP_PATH" ]] || { echo "test-windows-release: --local needs a zip path" >&2; exit 1; }
      shift
      ;;
    --no-viewer)
      OPEN_VIEWER=0
      ;;
    --no-start)
      START_VM=0
      ;;
    --pack-only)
      PACK_ONLY=1
      OPEN_VIEWER=0
      ;;
    --)
      shift
      break
      ;;
    -*)
      echo "test-windows-release: unknown option: $1" >&2
      usage >&2
      exit 1
      ;;
    *)
      SOURCE="local"
      ZIP_PATH="$1"
      ;;
  esac
  shift
done

need virsh
need xorriso
if [[ "$PACK_ONLY" -ne 1 ]]; then
  need virt-viewer
fi

mkdir -p "$CACHE" "$ZIPS" "$(dirname "$ISO")"

zip_id() {
  stat -c '%n %s %Y' "$1"
}

find_cached_zip() {
  local name="$1"
  local candidate
  for candidate in "$ZIPS/$name" "$CACHE/download/$name"; do
    if [[ -f "$candidate" ]]; then
      printf '%s\n' "$candidate"
      return 0
    fi
  done
  return 1
}

use_zip() {
  local src="$1"
  local name
  name="$(basename "$src")"
  if [[ "$src" != "$ZIPS/$name" ]]; then
    if [[ ! -f "$ZIPS/$name" ]]; then
      log "caching $name"
      cp -a "$src" "$ZIPS/$name"
    fi
    ZIP_PATH="$ZIPS/$name"
  else
    ZIP_PATH="$src"
  fi
  log "using $ZIP_PATH"
}

fetch_zip() {
  case "$SOURCE" in
    local)
      [[ -f "$ZIP_PATH" ]] || {
        echo "test-windows-release: zip not found: $ZIP_PATH" >&2
        exit 1
      }
      use_zip "$ZIP_PATH"
      ;;
    run)
      need gh
      if [[ -z "$RUN_ID" ]]; then
        RUN_ID="$(gh run list --repo "$REPO" --workflow release.yml --status success --limit 1 --json databaseId --jq '.[0].databaseId')"
        [[ -n "$RUN_ID" ]] || {
          echo "test-windows-release: no successful release.yml run found" >&2
          exit 1
        }
      fi
      local run_dir="$ZIPS/run-$RUN_ID"
      local existing
      existing="$(find "$run_dir" -name '*.zip' -print -quit 2>/dev/null || true)"
      if [[ -n "$existing" ]]; then
        log "reusing cached artifact from run $RUN_ID"
        use_zip "$existing"
        return
      fi
      log "downloading artifact $ARTIFACT_NAME from run $RUN_ID"
      mkdir -p "$run_dir"
      gh run download "$RUN_ID" --repo "$REPO" --name "$ARTIFACT_NAME" --dir "$run_dir"
      existing="$(find "$run_dir" -name '*.zip' -print -quit)"
      [[ -n "$existing" ]] || {
        echo "test-windows-release: no zip in artifact $ARTIFACT_NAME" >&2
        exit 1
      }
      use_zip "$existing"
      ;;
    latest-release|release)
      need gh
      local view_args=()
      if [[ -n "$RELEASE_TAG" ]]; then
        view_args=("$RELEASE_TAG")
      fi
      local name
      name="$(gh release view "${view_args[@]}" --repo "$REPO" --json assets --jq '.assets[] | select(.name | test("windows-x64\\.zip$")) | .name')"
      [[ -n "$name" ]] || {
        echo "test-windows-release: no windows-x64.zip on release ${RELEASE_TAG:-latest}" >&2
        exit 1
      }
      local cached
      if cached="$(find_cached_zip "$name")"; then
        log "reusing cached $name"
        use_zip "$cached"
        return
      fi
      log "downloading $name from GitHub release ${RELEASE_TAG:-latest}"
      gh release download "${view_args[@]}" --repo "$REPO" --pattern "$name" --dir "$ZIPS"
      use_zip "$ZIPS/$name"
      ;;
    *)
      echo "test-windows-release: unknown source $SOURCE" >&2
      exit 1
      ;;
  esac
}

pack_iso() {
  local current expected
  current="$(zip_id "$ZIP_PATH") $ISO"
  if [[ -f "$ISO" && -f "$STAMP" ]]; then
    expected="$(cat "$STAMP")"
    if [[ "$expected" == "$current" ]]; then
      log "reusing ISO $ISO ($(du -h "$ISO" | awk '{print $1}'))"
      return
    fi
  fi

  log "building ISO from $(basename "$ZIP_PATH")"
  xorriso -as mkisofs -R -J -V FORESEER -o "$ISO" "$ZIP_PATH" >/dev/null 2>&1
  chmod 644 "$ISO"
  printf '%s\n' "$current" >"$STAMP"
  log "ISO $ISO ($(du -h "$ISO" | awk '{print $1}'))"
}

wait_running() {
  local i
  for i in $(seq 1 30); do
    if [[ "$(virsh_cmd domstate "$VM_NAME")" == running ]]; then
      return 0
    fi
    sleep 1
  done
  echo "test-windows-release: $VM_NAME did not reach running" >&2
  exit 1
}

insert_iso() {
  log "inserting ISO into $CDROM_TARGET"
  virsh_cmd change-media "$VM_NAME" "$CDROM_TARGET" "$ISO" --live
  log "DVD $CDROM_TARGET now has volume label FORESEER"
}

fetch_zip
pack_iso

if [[ "$PACK_ONLY" -eq 1 ]]; then
  log "packed $ISO (VM left untouched)"
  exit 0
fi

if ! virsh_cmd dominfo "$VM_NAME" >/dev/null 2>&1; then
  echo "test-windows-release: libvirt domain '$VM_NAME' not found on $LIBVIRT_URI" >&2
  exit 1
fi

state="$(virsh_cmd domstate "$VM_NAME")"
if [[ "$state" != running ]]; then
  if [[ "$START_VM" -ne 1 ]]; then
    echo "test-windows-release: $VM_NAME is $state and --no-start was set" >&2
    exit 1
  fi
  log "starting $VM_NAME"
  virsh_cmd start "$VM_NAME"
fi
wait_running
insert_iso

cat <<EOF

Smoke in the guest:
  1. This PC → FORESEER DVD drive
  2. Extract the zip to Desktop (writable path)
  3. Run foreseer-desktop.exe
  4. Login reuse, play, visible video + audio, focus, Back/close, return to Foreseer

virt-viewer: Ctrl+Alt releases grab, F11 leaves fullscreen.

Unsigned build: SmartScreen may warn. QXL is software graphics.

EOF

if [[ "$OPEN_VIEWER" -eq 1 ]]; then
  log "opening virt-viewer"
  exec virt-viewer --connect "$LIBVIRT_URI" --attach "$VM_NAME"
fi
