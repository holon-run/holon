import AppKit
import SwiftUI
import XCTest
@testable import HolonMenu

@MainActor
final class HolonMenuLayoutTests: XCTestCase {
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
