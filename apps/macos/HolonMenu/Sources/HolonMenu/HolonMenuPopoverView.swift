import AppKit
import SwiftUI

@MainActor
final class HolonMenuPopoverLayout: ObservableObject {
    static let width: CGFloat = 320
    @Published var maximumHeight: CGFloat = 400
    @Published private(set) var naturalHeight: CGFloat = 400

    var contentSize: NSSize {
        NSSize(width: Self.width, height: min(naturalHeight, maximumHeight))
    }

    func measure(_ height: CGFloat) {
        guard height.isFinite, height > 0, height != naturalHeight else { return }
        naturalHeight = height
    }

    static func maximumHeight(visibleFrame: NSRect, anchorFrame: NSRect) -> CGFloat {
        // Leave room for the popover arrow and window border below the menu bar.
        max(1, min(visibleFrame.height, anchorFrame.minY - visibleFrame.minY) - 16)
    }
}

struct HolonMenuPopoverView: View {
    @ObservedObject var layout: HolonMenuPopoverLayout
    @ObservedObject var viewModel: HolonMenuViewModel
    let updater: HolonUpdater
    var openSettings: () -> Void = {}
    var contentHeightChanged: () -> Void = {}

    var body: some View {
        ScrollView(.vertical) {
            HolonMenuView(viewModel: viewModel, updater: updater, openSettings: openSettings)
                .fixedSize(horizontal: false, vertical: true)
                .background {
                    GeometryReader { geometry in
                        Color.clear
                            .onAppear { measure(geometry.size.height) }
                            .onChange(of: geometry.size.height) { height in measure(height) }
                    }
                }
        }
        .frame(width: HolonMenuPopoverLayout.width, height: layout.contentSize.height)
    }

    private func measure(_ height: CGFloat) {
        layout.measure(height)
        contentHeightChanged()
    }
}
