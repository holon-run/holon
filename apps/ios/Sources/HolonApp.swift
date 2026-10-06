import SwiftUI
import HolonClient

@main
struct HolonApp: App {
    @AppStorage("ui.language") private var language = "system"
    @Environment(\.scenePhase) private var scenePhase
    @State private var coordinator: ConnectionCoordinator?
    @State private var reader: ReadingCoordinator
    @State private var sender: SendingCoordinator?

    init() {
        let reading = ReadingCoordinator()
        _reader = State(initialValue: reading)
        let sending: SendingCoordinator?
        do {
            let directory = try FileManager.default.url(for: .applicationSupportDirectory,
                                                       in: .userDomainMask, appropriateFor: nil,
                                                       create: true)
                .appendingPathComponent("HolonSending", isDirectory: true)
            sending = SendingCoordinator(store: try SendingStore(directory: directory))
        } catch {
            sending = nil
        }
        _sender = State(initialValue: sending)
        do {
            let connection = ConnectionCoordinator(store: try ConnectionStore())
            connection.onIdentityChange = { [weak reading, weak sending] in
                reading?.disconnect()
                sending?.disconnect()
            }
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
                        ReadingView(reader: reader, sender: sender)
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
                                let client = try await coordinator.makeAuthenticatedClient()
                                guard !Task.isCancelled, coordinator.identity == identity else {
                                    await client.close()
                                    return
                                }
                                reader.onConnectionFailure = { [weak coordinator] failure in
                                    coordinator?.handleClientFailure(failure, expectedIdentity: identity)
                                }
                                reader.activate(client: client, identity: identity, apiBaseURL: profile.apiBaseURL)
                                reader.setForeground(scenePhase == .active)
                            } catch {
                                if coordinator.identity == identity { reader.disconnect() }
                            }
                        }
                        .task(id: coordinator.identity) { [coordinator] in
                            guard let sender, let identity = coordinator.identity,
                                  let profile = coordinator.selectedProfile else {
                                sender?.disconnect()
                                return
                            }
                            do {
                                let client = try await coordinator.makeAuthenticatedClient()
                                guard !Task.isCancelled, coordinator.identity == identity else {
                                    await client.close()
                                    return
                                }
                                sender.onConnectionFailure = { [weak coordinator] failure in
                                    coordinator?.handleClientFailure(failure, expectedIdentity: identity)
                                }
                                sender.setForeground(scenePhase == .active)
                                sender.activate(client: client, identity: identity,
                                                apiBaseURL: profile.apiBaseURL)
                                sender.selectAgent(reader.selectedAgentID)
                            } catch {
                                if coordinator.identity == identity { sender.disconnect() }
                            }
                        }
                        .onChange(of: reader.selectedAgentID) { _, agentID in
                            sender?.selectAgent(agentID)
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
                sender?.setForeground(phase == .active)
                if phase == .active {
                    coordinator?.validatePendingLogin()
                } else {
                    coordinator?.sceneBecameInactive()
                }
            }
        }
    }
}
