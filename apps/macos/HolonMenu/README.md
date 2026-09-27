# HolonMenu

macOS menu bar control surface for Holon.

## Build and test

- `xcodebuild -scheme HolonMenu -destination 'platform=macOS' test`

## Runtime shape

- macOS 13+
- SwiftUI `MenuBarExtra`
- accessory app lifecycle
- bundled `holon` CLI lookup via `HOLON_BINARY_PATH` or app bundle lookup
- `SMAppService.mainApp` login item toggle
- fake client for Swift tests
- starting or restarting the bundled daemon enables desktop integration for Finder actions
- Tailscale status is shown when the `tailscale` CLI is available
- enabling Tailscale Serve requires an explicit confirmation and uses `tailscale serve --bg`
- the menu exposes `tailscale serve reset` as the rollback action
