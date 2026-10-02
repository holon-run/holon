import AppKit

@MainActor
final class HolonMenuAppDelegate: NSObject, NSApplicationDelegate {
    private let installStatusItem: () -> Void

    init(installStatusItem: @escaping () -> Void) {
        self.installStatusItem = installStatusItem
        super.init()
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        NSApp.setActivationPolicy(.accessory)
        installStatusItem()
    }

    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows flag: Bool) -> Bool {
        // Reopening a menu-only app must not create a default window.
        false
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool {
        false
    }
}
