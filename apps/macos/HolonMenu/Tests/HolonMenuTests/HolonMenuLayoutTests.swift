import AppKit
import SwiftUI
import XCTest
@testable import HolonMenu

@MainActor
final class HolonMenuLayoutTests: XCTestCase {
    func testPopoverHeightUsesTheAnchorScreensVisibleArea() {
        let visible = NSRect(x: 0, y: 40, width: 800, height: 600)
        let anchor = NSRect(x: 720, y: 650, width: 24, height: 22)
        XCTAssertEqual(
            HolonMenuPopoverLayout.maximumHeight(visibleFrame: visible, anchorFrame: anchor), 584
        )
        XCTAssertEqual(
            HolonMenuPopoverLayout.maximumHeight(
                visibleFrame: visible.offsetBy(dx: -1_920, dy: -900),
                anchorFrame: anchor.offsetBy(dx: -1_920, dy: -900)
            ), 584
        )
        XCTAssertEqual(
            HolonMenuPopoverLayout.maximumHeight(
                visibleFrame: visible,
                anchorFrame: NSRect(x: 720, y: 400, width: 24, height: 22)
            ), 344
        )
    }

    func testPopoverFitsShortContentAndRecapsWhenTheScreenChanges() {
        let layout = HolonMenuPopoverLayout()
        layout.maximumHeight = 500
        layout.measure(300)
        XCTAssertEqual(layout.contentSize, NSSize(width: 320, height: 300))
        layout.measure(1_200)
        XCTAssertEqual(layout.contentSize.height, 500)
        layout.maximumHeight = 280
        XCTAssertEqual(layout.contentSize.height, 280)
        layout.measure(160)
        XCTAssertEqual(layout.contentSize.height, 160)
        layout.measure(.infinity)
        layout.measure(0)
        XCTAssertEqual(layout.contentSize.height, 160)
    }

    func testOversizedMenuScrollsWithinTheViewportAndRemeasuresQRChanges() async throws {
        let model = HolonMenuViewModel(client: FakeHolonClient())
        await model.enableLAN()
        await model.refresh()
        let layout = HolonMenuPopoverLayout()
        layout.maximumHeight = 320
        let hosting = NSHostingController(
            rootView: HolonMenuPopoverView(
                layout: layout, viewModel: model, updater: HolonUpdater()
            )
        )
        let window = NSWindow(
            contentRect: NSRect(origin: .zero, size: layout.contentSize),
            styleMask: [.borderless], backing: .buffered, defer: false
        )
        window.isReleasedWhenClosed = false
        window.contentViewController = hosting
        defer { window.close(); model.stopPolling() }
        window.orderFront(nil)
        func settle() async throws {
            for _ in 0..<20 {
                hosting.view.layoutSubtreeIfNeeded()
                try await Task.sleep(for: .milliseconds(10))
            }
        }
        try await settle()
        let collapsed = layout.naturalHeight
        XCTAssertGreaterThan(collapsed, layout.maximumHeight)
        XCTAssertEqual(hosting.view.frame.height, 320, accuracy: 1)

        func scrollView(in view: NSView) -> NSScrollView? {
            if let scroll = view as? NSScrollView { return scroll }
            return view.subviews.lazy.compactMap { scrollView(in: $0) }.first
        }
        let scroll = try XCTUnwrap(scrollView(in: hosting.view))
        let document = try XCTUnwrap(scroll.documentView)
        XCTAssertGreaterThan(document.frame.height, scroll.contentView.bounds.height)
        document.scroll(NSPoint(x: 0, y: document.frame.height))
        XCTAssertGreaterThan(scroll.contentView.bounds.minY, 0)
        document.scroll(.zero)
        XCTAssertEqual(scroll.contentView.bounds.minY, 0, accuracy: 1)

        await model.showPairingCode()
        try await settle()
        XCTAssertGreaterThan(layout.naturalHeight, collapsed)
        XCTAssertEqual(layout.contentSize.height, 320)
        model.hidePairingCode()
        try await settle()
        XCTAssertEqual(layout.naturalHeight, collapsed, accuracy: 1)
    }

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
