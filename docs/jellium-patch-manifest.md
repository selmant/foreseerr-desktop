# Jellium thin-fork patch manifest

Approved commits on top of `jellium.upstream-base`
(`28f2cf16a1f1b819884dd6a72919ca55bdf9bd73`). Any additional commit must be
recorded here before `scripts/boundary-audit.sh` will pass.

| Short | Full | Kind | Summary |
| --- | --- | --- | --- |
| `c9e8deb` | `c9e8deba673b8aa16b2424f8d7be157cb6d3aaae` | feature | Generic `host-extension` seam (descriptor, transport, presentation, example) |
| `de3c381` | `de3c381519c92706937f862145ae483c52d12348` | runtime fix | Wayland full-buffer viewport during WSI resize |
| `0a12974` | `0a1297427b07b4040f5cd58a14b7ad9a0b85750d` | runtime fix | mpv-proxy protocol error logging |
| `ce5d4b5` | `ce5d4b5ce4952634fc7cabb18f7cf0a00b5e21b4` | hygiene | Drop unused `HostOptions::has_extension` |
| `ecde360` | `ecde3604b65c302a27e219d3077d9f20d5a54dfe` | runtime fix | Atomically unmap hidden GPU CEF layers and serialize presentation changes |
| `478ce60` | `478ce608b78672f9196e0eb44e0f51969d8ed56a` | runtime fix | Preserve CEF GPU compositing and align native dropdown behavior |
| `bce89c3` | `bce89c31b72d3c2d53bb0074e60d84a627a81cd4` | runtime fix | Publish CEF copy/paste through the native Wayland clipboard |
| `946e947` | `946e947bb6b1680740fcc7da4a4ffcbbb56ff7ec` | build fix | Gate the host-extension-only `Arc` import for strict default builds |
| `bf59292` | `bf592922f061b64894c3058c5995c962b36c4b94` | documentation | Describe the maintained Foreseer runtime boundary |
| `ff71888` | `ff71888cff843044956eda943b6434904a9b5f54` | runtime fix | Route host shutdown notifications through the manager to avoid extension callback deadlocks |
| `db9ca5a` | `db9ca5af5bc82ab03b05fc24cdd0a4bbfa86bdcc` | runtime fix | Run host scripts even when a frontend has no built-in Jellium scripts |
| `0579346` | `05793467b3bda161ffc43b43d66a70c334191ec7` | runtime fix | Prepare primary CEF before playback while keeping page chrome veiled |
| `bc3122d` | `bc3122d267692e1c3ada32d1fa34bcdae9f7d1c1` | runtime fix | Prepare primary CEF before playback while keeping page chrome veiled |
| `d04f440` | `d04f4404569e39ade88135f61b3a2f4324317f6f` | runtime fix | Preserve Jellyfin OSD pointer targets and hidden action-sheet placeholders during veiled playback |
| `ffa6e22` | `ffa6e228e1f66e615f2a03e48b18c05bb27d6140` | runtime fix | Unmap hidden Windows DComp CEF visuals so playback restore remaps the last host-frontend frame |
| `bf647ab` | `bf647ab54b45737c38025f734980719700f16909` | runtime fix | Open Windows `<select>` dropdowns in-page like Linux instead of the DComp OSR popup |
| `0382e02` | `0382e0217e91f93f9233a0a6e7e2798961b968a0` | runtime fix | Prevent Linux CEF mallinfo overflow abort |
| `873d1e3` | `873d1e3221be19d5140d3e9ca53972d1099dc0a2` | runtime fix | Resume mpv presentation after compositor suspension |
| `cb4e9d0` | `cb4e9d0a73358dda95555fa7f6110d83ef418110` | runtime fix | Stop in-page `<select>` clicks from dismissing page dialogs |
| `f41b823` | `f41b8232b07b5607318624fe89c7dafb46750edd` | feature | Generic CEF disk-cache byte-limit host option |
| `07f2c51` | `07f2c513ca71b5b41f9dcc88038f10fc918e6fb8` | hygiene | Format generic CEF cache initialization |
| `b6984a8` | `b6984a80ac7f2d63c42a66e96a1a67a163448c63` | feature | Generic CEF HTTP-cache clearing host API |
| `55f8036` | `55f803665f40a0540dc10555b991595ba0e89012` | test | Cover cache-limit composition with host extensions |

## Ownership rules

- Keep product protocol, Foreseer origins, tickets, and JS assets out of Jellium.
- Prefer additive generic APIs over Foreseer-named hooks.
- Runtime fixes must be stock-regressable without the host-extension feature when practical.

## Growth policy

When the delta grows (new commits or large file churn):

1. Run `scripts/patch-delta.sh docs/jellium-patch-delta.md`.
2. Update this manifest with the new SHAs and rationale.
3. Update `docs/integration-plan.md` ownership notes if the public API changed.
4. Re-run `scripts/boundary-audit.sh`.
