import AppKit
import SwiftUI

@main
enum HolonMenuApp {
    @MainActor
    static func main() {
        let application = NSApplication.shared
        let statusController = HolonMenuStatusController()
        let delegate = HolonMenuAppDelegate {
            statusController.install(
                viewModel: HolonMenuViewModel(client: HolonCLIClient()),
                updater: HolonUpdater()
            )
        }
        application.delegate = delegate
        // NSApplication holds its delegate weakly.
        withExtendedLifetime(delegate) {
            application.run()
        }
    }
}

@MainActor
final class HolonMenuStatusController: NSObject {
    private let popover = NSPopover()
    private var statusItem: NSStatusItem?
    private var settingsWindow: NSWindow?

    func install(viewModel: HolonMenuViewModel, updater: HolonUpdater) {
        guard statusItem == nil else { return }
        let item = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
        item.button?.image = HolonMenuIcon.image
        item.button?.setAccessibilityLabel("Holon")
        item.button?.action = #selector(togglePopover)
        item.button?.target = self
        statusItem = item
        popover.behavior = .transient
        popover.contentSize = NSSize(width: 320, height: 400)
        popover.contentViewController = NSHostingController(
            rootView: HolonMenuView(viewModel: viewModel, updater: updater, openSettings: { [weak self] in
                self?.showSettings(viewModel: viewModel)
            })
                .fixedSize(horizontal: false, vertical: true)
                .background {
                    GeometryReader { geometry in
                        Color.clear.preference(key: MenuContentHeightKey.self, value: geometry.size.height)
                    }
                }
                .onPreferenceChange(MenuContentHeightKey.self) { [weak self] height in
                    guard height > 0 else { return }
                    self?.popover.contentSize = NSSize(width: 320, height: height)
                }
                .task { await viewModel.bootstrap() }
        )
    }

    @discardableResult
    func showSettings(viewModel: HolonMenuViewModel) -> NSWindow {
        popover.performClose(nil)
        let window: NSWindow
        if let settingsWindow {
            window = settingsWindow
        } else {
            window = NSWindow(
                contentRect: NSRect(x: 0, y: 0, width: 440, height: 180),
                styleMask: [.titled, .closable],
                backing: .buffered,
                defer: false
            )
            window.title = L10n.text("Holon Settings")
            window.isReleasedWhenClosed = false
            window.contentViewController = NSHostingController(
                rootView: HolonMenuSettingsView(viewModel: viewModel)
            )
            window.center()
            settingsWindow = window
        }
        NSApp.activate(ignoringOtherApps: true)
        window.makeKeyAndOrderFront(nil)
        return window
    }

    @objc private func togglePopover() {
        guard let button = statusItem?.button else { return }
        if popover.isShown {
            popover.performClose(nil)
        } else {
            popover.show(relativeTo: button.bounds, of: button, preferredEdge: .minY)
        }
    }
}

struct MenuContentHeightKey: PreferenceKey {
    static let defaultValue: CGFloat = 0

    static func reduce(value: inout CGFloat, nextValue: () -> CGFloat) {
        value = nextValue()
    }
}
