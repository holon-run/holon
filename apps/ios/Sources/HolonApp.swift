import SwiftUI
import HolonClient

@main
struct HolonApp: App {
    @AppStorage("ui.language") private var language = "system"
    @Environment(\.scenePhase) private var scenePhase
    @State private var coordinator: ConnectionCoordinator?
    @State private var reader: ReadingCoordinator
    @State private var sender: SendingCoordinator?
    @State private var work: WorkCoordinator
    @State private var files: FilesCoordinator
    @State private var imports: SharedImportCoordinator
    @State private var tab = ClientTab.reading
    @State private var settingsPath: [SettingsDestination] = []
    @State private var presentingFiles = false
    @State private var addingConnection = false
    @State private var previousProfile: ConnectionProfile?
    @State private var returningConnection = false

    private enum ClientTab: Hashable {
        case reading, work, settings
    }

    init() {
        let reading = ReadingCoordinator()
        _reader = State(initialValue: reading)
        let work = WorkCoordinator()
        let files = FilesCoordinator()
        let imports = SharedImportCoordinator()
        _work = State(initialValue: work)
        _files = State(initialValue: files)
        _imports = State(initialValue: imports)
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
            connection.onIdentityChange = { [weak reading, weak sending, weak work, weak files, weak imports] in
                reading?.disconnect()
                sending?.disconnect()
                work?.disconnect()
                files?.disconnect()
                imports?.revokeConfirmation()
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
                    Group {
                        if coordinator.launchState == .restoring {
                            ProgressView("onboarding.restoring")
                        } else if coordinator.launchState == .connection || addingConnection {
                            ConnectionWelcomeView(coordinator: coordinator, cancel: addingConnection ? {
                                returnToPreviousConnection(coordinator)
                            } : nil)
                        } else {
                            TabView(selection: $tab) {
                                ReadingView(reader: reader, sender: sender, connection: coordinator) {
                                    settingsPath = [.connections]
                                    tab = .settings
                                }
                                    .tabItem { Label("agents.title", systemImage: "bubble.left.and.bubble.right") }
                                    .tag(ClientTab.reading)
                                WorkView(coordinator: work, openPlan: { agentID, workID, plan in
                                    files.selectAgent(agentID)
                                    if files.openPlan(agentID: agentID, workID: workID, plan: plan) {
                                        presentingFiles = true
                                    }
                                }, openArtifact: { agentID, artifact in
                                    files.selectAgent(agentID)
                                    if files.openArtifact(agentID: agentID, artifact: artifact) {
                                        presentingFiles = true
                                    }
                                })
                                    .tabItem { Label("work.title", systemImage: "checklist") }
                                    .tag(ClientTab.work)
                                SettingsView(connection: coordinator, reader: reader, sender: sender,
                                             files: files, imports: imports, path: $settingsPath) {
                                    previousProfile = coordinator.selectedProfile
                                    addingConnection = true
                                }
                                    .tabItem { Label("settings.title", systemImage: "gearshape") }
                                    .tag(ClientTab.settings)
                            }
                        }
                    }
                        .task { await coordinator.restore() }
                        .onChange(of: coordinator.identity) { _, identity in
                            settingsPath = []
                            presentingFiles = false
                            if identity != nil {
                                tab = .reading
                                addingConnection = false
                                previousProfile = nil
                            }
                        }
                        .sheet(isPresented: $presentingFiles, onDismiss: {
                            files.dismissPreview()
                        }) {
                            NavigationStack {
                                FilesView(coordinator: files)
                                    .toolbar {
                                        ToolbarItem(placement: .cancellationAction) {
                                            Button("files.dismiss") { presentingFiles = false }
                                                .accessibilityIdentifier("files.dismiss")
                                        }
                                    }
                            }
                        }
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
                        .task(id: coordinator.identity) { [coordinator] in
                            guard let identity = coordinator.identity else {
                                work.disconnect()
                                return
                            }
                            do {
                                let client = try await coordinator.makeAuthenticatedClient()
                                guard !Task.isCancelled, coordinator.identity == identity else {
                                    await client.close()
                                    return
                                }
                                work.onConnectionFailure = { [weak coordinator] failure in
                                    coordinator?.handleClientFailure(failure, expectedIdentity: identity)
                                }
                                work.setForeground(scenePhase == .active)
                                work.activate(client: client, identity: identity)
                                work.selectAgent(reader.selectedAgentID)
                            } catch {
                                if coordinator.identity == identity { work.disconnect() }
                            }
                        }
                        .task(id: coordinator.identity) { [coordinator] in
                            guard let identity = coordinator.identity else {
                                files.disconnect()
                                return
                            }
                            do {
                                let client = try await coordinator.makeAuthenticatedClient()
                                guard !Task.isCancelled, coordinator.identity == identity else {
                                    await client.close()
                                    return
                                }
                                files.onConnectionFailure = { [weak coordinator] failure in
                                    coordinator?.handleClientFailure(failure, expectedIdentity: identity)
                                }
                                files.setForeground(scenePhase == .active)
                                files.activate(client: client, identity: identity)
                                files.selectAgent(reader.selectedAgentID)
                            } catch {
                                if coordinator.identity == identity { files.disconnect() }
                            }
                        }
                        .onChange(of: reader.selectedAgentID) { _, agentID in
                            sender?.selectAgent(agentID)
                            work.selectAgent(agentID)
                            files.selectAgent(agentID)
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
                work.setForeground(phase == .active)
                files.setForeground(phase == .active)
                imports.revokeConfirmation()
                if phase == .active { imports.reload() }
                if phase == .active {
                    coordinator?.validatePendingLogin()
                } else {
                    coordinator?.sceneBecameInactive()
                }
            }
        }
    }

    private func returnToPreviousConnection(_ coordinator: ConnectionCoordinator) {
        guard !returningConnection else { return }
        coordinator.cancelLogin()
        guard let profile = previousProfile,
              coordinator.identity == nil || coordinator.selectedProfile?.id != profile.id else {
            addingConnection = false
            previousProfile = nil
            return
        }
        returningConnection = true
        Task {
            await coordinator.connect(profile)
            returningConnection = false
            guard coordinator.selectedProfile?.id == profile.id else { return }
            addingConnection = false
            previousProfile = nil
        }
    }
}
