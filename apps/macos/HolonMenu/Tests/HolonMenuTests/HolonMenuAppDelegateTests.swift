import AppKit
import XCTest
@testable import HolonMenu

@MainActor
final class HolonMenuAppDelegateTests: XCTestCase {
    func testLaunchInstallsMenuWithoutCreatingAWindow() async {
        let application = NSApplication.shared
        let previousPolicy = application.activationPolicy()
        defer { application.setActivationPolicy(previousPolicy) }
        let windows = application.windows
        var installations = 0
        let delegate = HolonMenuAppDelegate { installations += 1 }

        delegate.applicationDidFinishLaunching(
            Notification(name: NSApplication.didFinishLaunchingNotification, object: application)
        )

        XCTAssertEqual(installations, 1)
        XCTAssertEqual(application.activationPolicy(), .accessory)
        XCTAssertEqual(application.windows, windows)
    }

    func testReopenDoesNotCreateAWindowOrReinstallMenu() async {
        let application = NSApplication.shared
        let windows = application.windows
        var installations = 0
        let delegate = HolonMenuAppDelegate { installations += 1 }

        XCTAssertFalse(delegate.applicationShouldHandleReopen(application, hasVisibleWindows: false))
        XCTAssertFalse(delegate.applicationShouldHandleReopen(application, hasVisibleWindows: true))
        XCTAssertEqual(installations, 0)
        XCTAssertEqual(application.windows, windows)
    }

    func testClosingLastWindowDoesNotTerminateMenuApp() async {
        let delegate = HolonMenuAppDelegate {}

        XCTAssertFalse(delegate.applicationShouldTerminateAfterLastWindowClosed(NSApplication.shared))
    }
}
