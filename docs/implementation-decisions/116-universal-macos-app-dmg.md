# macOS app ships as one universal DMG

## Decision

The release workflow builds `Holon-<version>.dmg` as a universal (x86_64 +
arm64) artifact. The Rust CLI is compiled for both darwin targets and merged
with `lipo`; the Swift menu app is built with `swift build --arch arm64
--arch x86_64`. `scripts/verify-macos-menu-app.sh` rejects any bundle whose
`HolonMenu` or bundled `holon` binary is missing either slice.

## Reason

A single DMG keeps one Sparkle feed URL, one download link, and one checksum
set, and Intel Mac users get the GUI app without Rosetta. Sparkle already
ships prebuilt universal binaries, so no third-party slice is missing.

## Preserved boundary

`swift test` still runs the host-native slice because tests must execute.
Separate per-architecture DMGs would require per-architecture Sparkle feeds
and stay out of scope until a size-sensitive distribution channel exists.
