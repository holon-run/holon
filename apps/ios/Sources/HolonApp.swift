import SwiftUI
import HolonClient

@main
struct HolonApp: App {
    @AppStorage("ui.language") private var language = "system"
    @Environment(\.scenePhase) private var scenePhase
    @State private var coordinator: ConnectionCoordinator?

    init() {
        do {
            _coordinator = State(initialValue: ConnectionCoordinator(store: try ConnectionStore()))
        } catch {
            _coordinator = State(initialValue: nil)
        }
    }

    var body: some Scene {
        WindowGroup {
            Group {
                if let coordinator {
                    ContentView(coordinator: coordinator)
                        .task { await coordinator.restore() }
                        .onOpenURL { url in Task { await coordinator.handleCallback(url) } }
                } else {
                    NavigationStack {
                        Form {
                            Text("status.storageError").font(.headline)
                            Text("storage.errorHelp")
                            ConnectionSettings()
                        }.navigationTitle("Holon")
                    }
                }
            }
            .environment(\.locale, language == "system" ? .autoupdatingCurrent : Locale(identifier: language))
            .onChange(of: scenePhase) { _, phase in
                if phase == .active {
                    coordinator?.validatePendingLogin()
                } else {
                    coordinator?.sceneBecameInactive()
                }
            }
        }
    }
}
