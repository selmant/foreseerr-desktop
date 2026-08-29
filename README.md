# Foreseer Desktop

Native Foreseer shell backed by Jellium's opt-in `host-extension` runtime.
Foreseer Desktop owns protocol v1, product policy, and injected assets; Jellium
supplies CEF/mpv and a generic extension seam.

**0.3 support:** Linux (Wayland primary, X11 best-effort), from source or
CI AppImage. An unsigned Windows x64 portable ZIP is also produced. macOS is
not released.

This binary links GPL-2.0-only Jellium code and is therefore GPL-2.0-only.
See [LICENSE](LICENSE).

## How this fits the Foreseer product

Foreseer Desktop defaults to standalone mode: it starts a bundled Foreseerr
server on an ephemeral `127.0.0.1` port and owns its local data. Remote mode
remains available for an existing Foreseerr deployment.

| Component | Owns |
| --- | --- |
| [Foreseerr](https://github.com/selmant/foreseerr) | Hosted UI, sign-in, linked Jellyfin identity, discovery, requests, library browsing, and browser fallback. |
| Foreseer Desktop | Native protocol v1, secure desktop bootstrap, desktop configuration, and the product release pin. |
| [Jellium](https://github.com/selmant/jellium-desktop) | Generic CEF/mpv runtime, compositor/window lifecycle, and the `host-extension` API. |
| Jellyfin Web | Media resolution, resume position, stream selection, and playback reporting. |

The same Foreseerr page works in both environments. In a browser, play controls
remain ordinary links. In this Desktop client, a compatible signed-in Jellyfin
play action is passed to the native runtime; unsupported media and any native
failure retain the browser fallback. User-facing setup and troubleshooting are
documented in Foreseerr's [Native Desktop guide](https://github.com/selmant/foreseerr/blob/develop/docs/using-seerr/native-desktop.md).

## Requirements

- Adjacent [Jellium](https://github.com/selmant/jellium-desktop) checkout at the
  commit in [`jellium.rev`](jellium.rev) (default layout:
  `../jellium-desktop`)
- Rust stable, system `libmpv`, and the usual Linux native build deps
  (Wayland/X11, clang for bindgen)

```text
Projects/
  foreseer-desktop/              # this repo
  jellium-desktop/               # pinned thin fork tip in jellium.rev
```

```sh
git -C ../jellium-desktop fetch origin
git -C ../jellium-desktop checkout "$(tr -d '[:space:]' < jellium.rev)"
git -C ../jellium-desktop submodule update --init --recursive
JELLIUM_DIR=../jellium-desktop ./scripts/boundary-audit.sh
```

Architecture: [docs/integration-plan.md](docs/integration-plan.md).  
Fork upgrades: [docs/upgrade-runbook.md](docs/upgrade-runbook.md).

## Configuration & CLI

Foreseer Desktop persists its configuration in a standard OS config directory:
- **Linux**: `~/.config/Foreseer/config.json`
- **macOS**: `~/Library/Application Support/com.selmantrabzon.Foreseer/config.json`
- **Windows**: `%APPDATA%\selmantrabzon\Foreseer\config.json`

```json
{
  "schema_version": 2,
  "mode": "standalone",
  "remote": { "server_url": "https://foreseer.example.com", "allow_insecure_http": false },
  "standalone": { "cache_limit_bytes": 2147483648 }
}
```

### CLI Commands & Environment Variables

```sh
# Run the saved standalone or remote mode:
cargo run

# Launch the graphical server setup GUI:
cargo run -- --setup

# View current configuration and file location:
cargo run -- --show-config

# Switch modes:
cargo run -- --standalone
cargo run -- --remote https://foreseer.example.com

# Set the combined transient cache budget (images + CEF HTTP cache):
cargo run -- --cache-limit 2147483648

# HTTP or HTTPS — the URL scheme is the choice:
cargo run -- --set-url http://192.168.1.50:5055

# Temporary environment variable override (does not modify config.json):
FORESEER_URL=https://foreseer.example cargo run
```

### Standalone data and recovery

Standalone Foreseerr data is separate from the Jellium profile:

```text
<Foreseer config>/standalone/
  settings.json
  db/db.sqlite3
  logs/
  state/
  backups/
```

Before starting a bundled Foreseerr version different from the last verified
standalone version, the desktop creates a timestamped backup of `settings.json`
and the SQLite database (including WAL/SHM files). Caches and logs are never
included in those backups, and only the three newest automatic backups are
kept.

If migration or startup fails, use the recovery screen’s **Open Logs** action,
then stop the desktop before attempting manual recovery. Do not copy an older
database over a database while a newer bundled binary is running: automatic
restore is intentionally not implemented because schema downgrades are unsafe.
Keep the failed database, logs, and backup together until recovery is complete.

## Test / lint

```sh
cargo test
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings

# Deterministic protocol/integration harness (no network, CEF, mpv, or secrets):
node scripts/protocol-v1-harness.mjs
```

Protocol v1 is canonical in `protocol/protocol-v1.json` (byte-equivalent copy in
Foreseerr). The Desktop client accepts only protocol v1; a browser or an
incompatible native runtime falls back to ordinary web playback.

The harness covers fixture shape, command set, and package version. Before a
release, run the Wayland and X11 visible-video/audio/focus matrix (including
resize, fullscreen, mixed DPI, suspend/resume, and renderer recovery), then a
50-cycle discovery → play → Back soak while checking for hidden audio, surface
leaks, focus loss, and Jellyfin UI flashes.

## Release pins

| Pin | Location |
| --- | --- |
| Version | `Cargo.toml` (`0.3.0`) |
| Jellium revision | `jellium.rev` |

CI checks out that Jellium revision as a sibling of this repo and runs format,
tests, and Clippy on Linux.

## Docs

Shared auth, playback routing, and lifecycle roadmap:
[docs/integration-plan.md](docs/integration-plan.md).
