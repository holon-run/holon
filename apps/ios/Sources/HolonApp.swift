import SwiftUI
import HolonClient

@main
struct HolonApp: App {
    @AppStorage("ui.language") private var language = "system"
    @Environment(\.scenePhase) private var scenePhase
    @State private var coordinator: ConnectionCoordinator?
    @State private var reader: ReadingCoordinator

    init() {
        let reading = ReadingCoordinator()
        _reader = State(initialValue: reading)
        do {
            let connection = ConnectionCoordinator(store: try ConnectionStore())
            connection.onIdentityChange = { [weak reading] in reading?.disconnect() }
            _coordinator = State(initialValue: connection)
        } catch {
            _coordinator = State(initialValue: nil)
        }
    }

    var body: some Scene {
        WindowGroup {
            Group {
                if let coordinator {
                    TabView {
                        ReadingView(reader: reader)
                            .tabItem { Label("agents.title", systemImage: "bubble.left.and.bubble.right") }
                        ContentView(coordinator: coordinator)
                            .tabItem { Label("connection.title", systemImage: "network") }
                    }
                        .task { await coordinator.restore() }
                        .task(id: coordinator.identity) { [coordinator] in
                            guard let identity = coordinator.identity,
                                  let profile = coordinator.selectedProfile else {
                                reader.disconnect()
                                return
                            }
                            do {
                                let client = try await coordinator.makeReadingClient()
                                guard !Task.isCancelled, coordinator.identity == identity else {
                                    await client.close()
                                    return
                                }
                                reader.onConnectionFailure = { [weak coordinator] failure in
                                    coordinator?.handleReadingFailure(failure, expectedIdentity: identity)
                                }
                                reader.activate(client: client, identity: identity, apiBaseURL: profile.apiBaseURL)
                                reader.setForeground(scenePhase == .active)
                            } catch {
                                if coordinator.identity == identity { reader.disconnect() }
                            }
                        }
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
                reader.setForeground(phase == .active)
                if phase == .active {
                    coordinator?.validatePendingLogin()
                } else {
                    coordinator?.sceneBecameInactive()
                }
            }
        }
    }
}
