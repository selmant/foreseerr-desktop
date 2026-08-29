# Changelog

## Unreleased

## 0.3.0 — 2026-08-29

### Added

- Standalone mode: supervise a bundled Foreseerr on an ephemeral localhost
  port, with local SQLite data, upgrade backups, and a recovery screen
  (retry, open logs, quit, switch to remote).
- Native setup can choose standalone or remote, including in-process remote
  setup and Jellyfin HTTPS fallback.
- Cache budget preference, authorized browser-cache clearing, and desktop
  mode preference capability advertised to the hosted UI.

### Fixed

- Resume video and the web UI after switching workspace or monitor. Occlusion
  left CEF on a stale frame and mpv blocked in a FIFO present wait.
- Keep the request modal open when picking a quality profile, root folder, or
  other native `<select>` option. The in-page dropdown overlay was reaching the
  modal's click-outside handler (browser-native popups do not).
- Adopt a new Jellyfin server Id after the media server is reinstalled, instead
  of stalling on ConnectionManager `ServerMismatch` because cached credentials
  and the `/login` hash gate blocked session bootstrap.
- Keep the standalone child alive across CEF readiness, playback-aware job
  drain, Windows job-object containment, and Node staging on Windows.

## 0.2.9 — 2026-08-14

### Fixed

- Open native `<select>` dropdowns in-page on Windows, matching Linux, instead
  of a DirectComposition OSR popup that closed immediately or appeared in the
  wrong place.

## 0.2.8 — 2026-08-14

### Fixed

- After native playback, Back / close media remaps the Foreseer UI on Windows
  instead of leaving a black DirectComposition surface over idle mpv.

## 0.2.7 — 2026-08-13

### Fixed

- Restore play/pause clicks and double-click fullscreen across the mpv picture
  area after the primary-web preparation veil hid Jellyfin's OSD hit target.
- Keep unselected subtitle checkmarks hidden in Jellyfin's playback action
  sheet while the transparent player presentation is active.

## 0.2.5 — 2026-08-12

### Fixed

- Restore `window.foreseerNative` on every native Foreseer frontend, including
  the first-run setup document, so connection setup can use the native bridge.
- Make the `--setup` command-line option open setup instead of being rejected
  by the embedded Jellium runtime.

## 0.2.4 — 2026-08-12

### Fixed

- Prevent the in-app **Quit Foreseer** control from deadlocking the CEF UI
  thread. Window-manager close requests now remain responsive during shutdown.

## 0.2.3 — 2026-08-12

### Fixed

- Correct Linux AppImage packaging name substitution so the staged Foreseer
  executable is stripped and bundled under its real filename.

## 0.2.2 — 2026-08-12

### Fixed

- Pin the Jellium runtime fix that lets default builds pass strict unused-import
  warnings while retaining the `host-extension` API.

## 0.2.1 — 2026-08-12

### Changed

- Integrates the generic Jellium `host-extension` runtime into the Foreseer
  Desktop product shell and pins its tested fork revision.
- Moves Foreseer protocol, authentication, session, controller, and injected
  web assets into the Desktop repository.
- Adds CI boundary and protocol gates for the pinned Jellium runtime.

## 0.2.0 — 2026-08-09

### Added

- Secure native bootstrap and setup flow using challenge-bound, short-lived Foreseer authentication tickets.

### Changed

- Pin the merged Jellium runtime revision used by release builds.

## 0.1.0 — 2026-08-02

First public source release of Foreseer Desktop.

### Supported

- Linux (Wayland primary; X11 best-effort), built from source against a pinned Jellium fork
- Discovery → native Jellyfin playback → return via Jellium `external-frontend`
- Foreseer ticket auth redemption into a private Jellyfin session

### Not yet

- Packaged AppImage / Flatpak / Windows / macOS installers
- Declared support for Windows or macOS (untested)

### Pins

- Jellium: see `jellium.rev` (`selmant/jellium-desktop`, `external-frontend` feature)
