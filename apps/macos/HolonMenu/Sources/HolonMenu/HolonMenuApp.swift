import SwiftUI

@main
struct HolonMenuApp: App {
    @NSApplicationDelegateAdaptor(HolonMenuAppDelegate.self) private var appDelegate
    @StateObject private var viewModel = HolonMenuViewModel(client: HolonCLIClient())
    private let updater = HolonUpdater()

    var body: some Scene {
        MenuBarExtra("Holon", systemImage: "bolt.horizontal.circle") {
            HolonMenuView(viewModel: viewModel, updater: updater)
                .task {
                    await viewModel.bootstrap()
                }
        }
        .menuBarExtraStyle(.window)
    }
}
