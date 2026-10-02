# macOS menu app keeps the standalone daemon

## Decision

The Holon macOS menu bar application uses an AppKit `NSApplication`/`NSStatusItem`
accessory lifecycle on macOS 13 or later, with SwiftUI rendering the `NSPopover`
content, and invokes the bundled Rust CLI as its daemon lifecycle adapter.
It does not register SwiftUI scenes or create a default window; see the
[menu app lifecycle RFC](../rfcs/macos-menu-app-and-daemon-lifecycle.md).
The existing standalone daemon remains the single runtime. The app registers
only itself with `SMAppService.mainApp`; it does not install a LaunchAgent.

The app and CLI share an explicit daemon desired state. Quitting the menu app
does not stop the daemon, and login launch restores the last explicit Start or
Stop choice.

## Reason

This keeps process identity, stale-state handling, graceful shutdown, and
configuration ownership in the existing Rust lifecycle implementation. Adding
launchd supervision in the same change would make Stop semantics, installation
ownership, helper registration, and update recovery interdependent before the
GUI contract is proven.

## Preserved boundary

Swift is a native presentation and macOS integration layer, not a second
runtime supervisor. A future LaunchAgent requires an explicit ownership
protocol and a separate decision.
