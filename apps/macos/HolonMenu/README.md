# HolonMenu

Phase 2 macOS menu bar skeleton for Holon.

## Build and test

- `xcodebuild -scheme HolonMenu -destination 'platform=macOS' test`

## Runtime shape

- macOS 13+
- SwiftUI `MenuBarExtra`
- accessory app lifecycle
- bundled `holon` CLI lookup via `HOLON_BINARY_PATH` or app bundle lookup
- `SMAppService.mainApp` login item toggle
- fake client for Swift tests
