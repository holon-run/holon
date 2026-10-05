import AppKit
import SwiftUI
import XCTest
@testable import HolonMenu

@MainActor
final class HolonMenuLayoutTests: XCTestCase {
    func testMenuHasNoEditableTextField() async {
        let controller = NSHostingController(
            rootView: HolonMenuView(
                viewModel: HolonMenuViewModel(client: FakeHolonClient()),
                updater: HolonUpdater()
            )
        )
        let size = controller.sizeThatFits(in: NSSize(width: 320, height: 2_000))
        let window = NSWindow(
            contentRect: NSRect(origin: .zero, size: size),
            styleMask: [.titled],
            backing: .buffered,
            defer: false
        )
        window.isReleasedWhenClosed = false
        window.contentViewController = controller
        controller.view.layoutSubtreeIfNeeded()
        func editableFields(in view: NSView) -> [NSTextField] {
            let fields = (view as? NSTextField).map { $0.isEditable ? [$0] : [] } ?? []
            return fields + view.subviews.flatMap { editableFields(in: $0) }
        }
        XCTAssertTrue(editableFields(in: controller.view).isEmpty)
        window.close()
    }

    func testSettingsReusesWindowAndSharesPairingDestinationModel() async throws {
        let model = HolonMenuViewModel(client: FakeHolonClient())
        await model.enableLAN()
        let automaticURL = try XCTUnwrap(model.connectionURL)
        await model.showPairingCode()
        XCTAssertNotNil(model.pairingURL)

        let statusController = HolonMenuStatusController()
        let window = statusController.showSettings(viewModel: model)
        defer { window.close() }
        let hosting = try XCTUnwrap(
            window.contentViewController as? NSHostingController<HolonMenuSettingsView>
        )
        XCTAssertTrue(hosting.rootView.viewModel === model)
        hosting.rootView.viewModel.customPairingOrigin = "https://remote.example"
        XCTAssertEqual(model.connectionURL?.absoluteString, "https://remote.example")
        XCTAssertNil(model.pairingURL)

        await model.showPairingCode()
        XCTAssertNotNil(model.pairingURL)
        hosting.rootView.viewModel.customPairingOrigin = ""
        XCTAssertEqual(model.connectionURL, automaticURL)
        XCTAssertNil(model.pairingURL)
        XCTAssertTrue(statusController.showSettings(viewModel: model) === window)
        window.close()
        XCTAssertFalse(window.isVisible)
        XCTAssertTrue(statusController.showSettings(viewModel: model) === window)
        XCTAssertTrue(window.isVisible)
        model.stopPolling()
    }

    func testMenuContentShrinksAfterPairingCodeIsHidden() async {
        let viewModel = HolonMenuViewModel(client: FakeHolonClient())
        await viewModel.enableLAN()
        func contentHeight() -> CGFloat {
            let hostingController = NSHostingController(
                rootView: HolonMenuView(viewModel: viewModel, updater: HolonUpdater())
                    .fixedSize(horizontal: false, vertical: true)
            )
            return hostingController.sizeThatFits(in: NSSize(width: 320, height: 2_000)).height
        }
        await viewModel.showPairingCode()
        let expanded = contentHeight()

        viewModel.hidePairingCode()
        let collapsed = contentHeight()

        XCTAssertGreaterThan(expanded, collapsed)
        viewModel.stopPolling()
    }
}
