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
    @State private var router = AppRouter()
    @State private var addingConnection = false
    @State private var previousProfile: ConnectionProfile?
    @State private var returningConnection = false
    @State private var consentRevision = 0
    @State private var displayedReadingPartition: ReadingPartition?

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
            connection.onIdentityChange = { [weak connection, weak reading, weak sending, weak work, weak files, weak imports] in
                if let context = connection?.readingContext {
                    reading?.activateOffline(partition: context.partition)
                } else {
                    reading?.disconnect()
                }
                sending?.disconnect()
                work?.disconnect()
                files?.disconnect()
                imports?.resetDestination()
            }
            connection.onReadingInvalidation = { [weak connection, weak reading] partition in
                reading?.invalidate(partition)
                if connection?.readingContext == nil { reading?.disconnect() }
            }
            reading.onReconnect = { [weak connection] in
                await connection?.refresh()
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
                    let readingActivation = ReadingActivation(coordinator)
                    Group {
                        if addingConnection || (coordinator.launchState == .connection && coordinator.readingContext == nil) {
                            ConnectionWelcomeView(coordinator: coordinator, cancel: addingConnection ? {
                                returnToPreviousConnection(coordinator)
                            } : nil)
                        } else if coordinator.launchState == .restoring && coordinator.readingContext == nil {
                            ProgressView("onboarding.restoring")
                        } else if let url = coordinator.readingContext?.profile.apiBaseURL ?? coordinator.selectedProfile?.apiBaseURL,
                                  !SharingConsent.shared.approved(url) {
                            ConnectionSharingConsentView(apiBaseURL: url, onApproved: {
                                consentRevision += 1
                            }, onChangeConnection: {
                                previousProfile = coordinator.selectedProfile
                                addingConnection = true
                            })
                            .id(url)
                        } else if displayedReadingPartition != coordinator.readingContext?.partition {
                            ProgressView("onboarding.restoring")
                        } else {
                            NavigationStack(path: $router.path) {
                                ReadingView(reader: reader, connection: coordinator)
                                    .navigationDestination(for: AppRoute.self) { route in
                                        destination(route, connection: coordinator)
                                    }
                            }
                            .safeAreaInset(edge: .top, spacing: 0) {
                                if !imports.pending.isEmpty, router.path.last != .tools {
                                    Button {
                                        router.path.append(.tools)
                                    } label: {
                                        HStack {
                                            Image(systemName: "square.and.arrow.down")
                                            Text("share.pending \(imports.pending.count)")
                                            Spacer()
                                            Image(systemName: "chevron.right").font(.caption)
                                        }.font(.callout).padding(.horizontal, 16).frame(minHeight: 44)
                                    }
                                    .background(.bar)
                                    .accessibilityIdentifier("share.inboxBanner")
                                }
                            }
                        }
                    }
                        .task { await coordinator.restore() }
                        .onChange(of: coordinator.identity) { _, identity in
                            imports.activate(identity.flatMap { value in
                                coordinator.selectedProfile.flatMap {
                                    ReadingPartition(apiBaseURL: $0.apiBaseURL, identity: value)
                                }
                            })
                            if identity != nil {
                                addingConnection = false
                                previousProfile = nil
                            }
                        }
                        .id(consentRevision)
                        .onChange(of: reader.agents) { _, _ in
                            restoreReadingRoute()
                        }
                        .onChange(of: reader.status) { _, status in
                            if status == .permissionDenied || status == .sessionExpired {
                                router.withdrawAgent()
                            }
                            restoreReadingRoute()
                        }
                        .onChange(of: reader.isShowingCachedContent) { _, _ in
                            restoreReadingRoute()
                        }
                        .onChange(of: reader.isRosterCurrent) { _, _ in
                            restoreReadingRoute()
                        }
                        .onChange(of: router.path) { _, _ in
                            reader.selectAgent(router.agentID)
                            router.remember()
                            if case .file = router.path.last {} else if files.request != nil {
                                files.dismissPreview()
                            }
                        }
                        .task(id: readingActivation) { [coordinator] in
                            guard !Task.isCancelled, ReadingActivation(coordinator) == readingActivation else { return }
                            let partition = readingActivation.context?.partition
                            if router.partition != partition { router.activate(partition) }
                            guard let context = readingActivation.context else {
                                reader.disconnect()
                                displayedReadingPartition = nil
                                return
                            }
                            reader.activateOffline(partition: context.partition)
                            displayedReadingPartition = context.partition
                            reader.setForeground(scenePhase == .active)
                            restoreReadingRoute()
                            guard let identity = readingActivation.identity,
                                  let profile = readingActivation.profile,
                                  context.profile == profile,
                                  ReadingPartition(apiBaseURL: profile.apiBaseURL, identity: identity) == context.partition else {
                                return
                            }
                            reader.markOnlineActivationPending()
                            do {
                                let client = try await coordinator.makeAuthenticatedClient()
                                guard !Task.isCancelled, ReadingActivation(coordinator) == readingActivation else {
                                    await client.close()
                                    return
                                }
                                reader.onConnectionFailure = { [weak coordinator] failure in
                                    guard let coordinator, ReadingActivation(coordinator) == readingActivation else { return }
                                    coordinator.handleClientFailure(failure, expectedIdentity: identity)
                                }
                                reader.activate(client: client, identity: identity, apiBaseURL: profile.apiBaseURL)
                                reader.setForeground(scenePhase == .active)
                            } catch {
                                guard !Task.isCancelled, ReadingActivation(coordinator) == readingActivation else { return }
                                reader.activateOffline(partition: context.partition)
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
            .task { imports.reload() }
            .onChange(of: scenePhase) { _, phase in
                reader.setForeground(phase == .active)
                sender?.setForeground(phase == .active)
                work.setForeground(phase == .active)
                files.setForeground(phase == .active)
                imports.revokeConfirmation()
                if phase == .active { imports.reload() }
                if phase == .active {
                    coordinator?.validatePendingLogin()
                    if let coordinator, coordinator.identity == nil,
                       coordinator.readingContext != nil, coordinator.status != .connecting {
                        Task { await coordinator.refresh() }
                    }
                } else {
                    coordinator?.sceneBecameInactive()
                }
            }
        }
    }

    private struct ReadingActivation: Equatable {
        let context: ConfirmedReadingContext?
        let identity: HolonConnectionIdentity?
        let profile: ConnectionProfile?

        @MainActor init(_ coordinator: ConnectionCoordinator) {
            context = coordinator.readingContext
            identity = coordinator.identity
            profile = coordinator.selectedProfile
        }
    }

    private func restoreReadingRoute() {
        guard let partition = coordinator?.readingContext?.partition,
              router.partition == partition, displayedReadingPartition == partition else { return }
        // A confirmed cached roster permits local navigation, not server mutations.
        router.restore(agents: reader.agents,
                       authoritative: reader.isRosterCurrent)
    }

    @ViewBuilder
    private func destination(_ route: AppRoute, connection: ConnectionCoordinator) -> some View {
        switch route {
        case .conversation(let agent):
            if reader.selectedAgentID == agent {
                ConversationReadingView(reader: reader, sender: sender?.selectedAgentID == agent ? sender : nil,
                    openReference: { reference in
                        guard files.selectedAgentID == agent else { return }
                        router.path.append(.file(agent, .source(.reference(reference))))
                    }, openWork: { workID in router.path.append(.workDetail(agent, .item(workID))) },
                    openTask: { taskID in router.path.append(.workDetail(agent, .task(taskID))) })
                    .id(agent)
            } else { ProgressView("reading.loadingConversation") }
        case .work(let agent), .workDetail(let agent, _):
            if work.selectedAgentID == agent {
            WorkView(coordinator: work, route: {
                if case .workDetail(_, let detail) = route { return detail }
                return nil
            }(), openReference: { reference in
                guard files.selectedAgentID == agent else { return }
                router.path.append(.file(agent, .source(.reference(reference))))
            }, openPlan: { agent, workID, plan in
                if files.selectedAgentID == agent, let locator = FilesPlanLocator(agentID: agent, workID: workID, plan: plan) {
                    router.path.append(.file(agent, .plan(locator)))
                }
            }, openArtifact: { agent, artifact in
                if let request = files.artifactRequest(agentID: agent, artifact: artifact) { router.path.append(.file(agent, request)) }
            })
            } else { ProgressView("work.loading") }
        case .files(let agent):
            if files.selectedAgentID == agent {
                FilesView(coordinator: files, openFile: { router.path.append(.file(agent, $0)) })
            }
            else { ProgressView("work.loading") }
        case .file(let agent, let request):
            if files.selectedAgentID == agent {
                FileReaderView(coordinator: files, request: request, openFile: { router.path.append(.file(agent, $0)) })
            } else { ProgressView("work.loading") }
        case .settings:
            SettingsView(connection: connection)
        case .connections:
            ContentView(coordinator: connection) {
                previousProfile = connection.selectedProfile
                addingConnection = true
            }
        case .tools:
            SystemExperienceView(connection: connection, reader: reader, sender: sender, imports: imports) { agent in
                guard reader.status == .live, reader.agents.contains(where: { $0.id == agent }) else { return }
                router.path = [.conversation(agent)]
            }
        case .diagnostics:
            DiagnosticView(connection: connection, reader: reader, sender: sender)
        }
    }

    private func returnToPreviousConnection(_ coordinator: ConnectionCoordinator) {
        guard !returningConnection else { return }
        coordinator.cancelLogin()
        guard let profile = previousProfile, coordinator.profiles.contains(profile),
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
