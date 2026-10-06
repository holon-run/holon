# HolonMenu

macOS menu bar control surface for Holon.

## Build and test

- `xcodebuild -scheme HolonMenu -destination 'platform=macOS' test`
- `make macos-menu-test` (the host-native CI test target)

## Branding and languages

- The app icon reuses `Sources/HolonMenu/Resources/holon-mark.png`; packaging
  generates the full `.icns` size set with `scripts/generate-macos-app-icon.sh`.
- English and Simplified Chinese follow macOS language preferences, including
  per-app language preferences. English is the fallback for other languages.
- Localized app text lives in the target's `Resources/*.lproj/Localizable.strings`.
  New languages must also be declared in `Resources/Info.plist` and checked by
  `scripts/verify-macos-menu-app.sh` so release bundles retain the translations.

## Runtime shape

- macOS 13+
- AppKit `NSApplication`/`NSStatusItem` accessory app lifecycle
- SwiftUI content hosted in an `NSPopover`; no SwiftUI scenes or default windows
- bundled `holon` CLI lookup via `HOLON_BINARY_PATH` or app bundle lookup
- `SMAppService.mainApp` login item toggle
- fake client for Swift tests
- starting or restarting a local-only daemon enables desktop integration for Finder actions; LAN mode disables it
- Tailscale status is shown when the `tailscale` CLI is available
- enabling Tailscale Serve requires an explicit confirmation and will not replace another service's root rule
- disabling Serve removes only Holon's root HTTPS rule; other Serve paths remain untouched
