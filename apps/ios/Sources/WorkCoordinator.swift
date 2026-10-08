import Foundation
import Observation
import HolonClient

@Observable @MainActor
final class WorkCoordinator {
    private(set) var selectedAgentID: String?
    private(set) var items: [WorkRecord] = []
    private(set) var tasks: [WorkRecord] = []
    private(set) var itemsState: WorkLoadState = .disconnected
    private(set) var tasksState: WorkLoadState = .disconnected
    private(set) var detailState: WorkLoadState = .idle
    private(set) var detail: WorkRecord?
    private(set) var output: WorkOutput?
    private(set) var brief: JSONValue?
    private(set) var route: WorkRoute?
    private(set) var itemLimit = 50
    private(set) var taskLimit = 50
    private(set) var briefFailed = false
    private(set) var outputFailed = false
    var onConnectionFailure: ((HolonHTTPFailure) -> Void)?

    @ObservationIgnored private var transport: (any WorkTransport)?
    @ObservationIgnored private var identity: HolonConnectionIdentity?
    @ObservationIgnored private var foreground = true
    @ObservationIgnored private var revision = 0
    @ObservationIgnored private var detailRevision = 0
    @ObservationIgnored private var pending: [Task<Void, Never>] = []
    @ObservationIgnored private var detailTask: Task<Void, Never>?
    @ObservationIgnored private var outputTask: Task<Void, Never>?
    @ObservationIgnored private var detailVisible = false
    @ObservationIgnored private let outputRefreshInterval: Duration

    init(outputRefreshInterval: Duration = .seconds(3)) { self.outputRefreshInterval = outputRefreshInterval }

    func activate(client: HolonClient, identity: HolonConnectionIdentity) {
        activate(transport: WorkClientTransport(client: client, authority: identity), identity: identity)
    }

    func activate(transport: any WorkTransport, identity: HolonConnectionIdentity) {
        disconnect()
        guard !identity.networkID.isEmpty,
              identity.runtimeID?.isEmpty == false, identity.userID?.isEmpty == false,
              identity.visibilityScopeID?.isEmpty == false else {
            itemsState = .incompatible
            tasksState = .incompatible
            Task { await transport.close() }
            return
        }
        self.transport = transport
        self.identity = identity
        reload()
    }

    func disconnect() {
        let old = transport
        cancel()
        transport = nil
        identity = nil
        clear()
        itemsState = .disconnected
        tasksState = .disconnected
        if let old { Task { await old.close() } }
    }

    func setForeground(_ active: Bool) {
        guard foreground != active else { return }
        foreground = active
        cancel()
        if active { reload() }
        else {
            itemsState = transport == nil ? .disconnected : .offline
            tasksState = itemsState
            detailState = .offline
        }
    }

    func selectAgent(_ id: String?) {
        guard id == nil || (id?.isEmpty == false && (id?.utf8.count ?? 0) <= 512),
              selectedAgentID != id else { return }
        cancel()
        selectedAgentID = id
        clear()
        reload()
    }

    func refresh() { cancel(); reload() }
    func loadMoreItems() { guard itemLimit < 400 else { return }; itemLimit = min(400, itemLimit * 2); refresh() }
    func loadMoreTasks() { guard taskLimit < 400 else { return }; taskLimit = min(400, taskLimit * 2); refresh() }
    func setDetailVisible(_ value: Bool, route expected: WorkRoute? = nil) {
        if !value, let expected, route != expected { return }
        detailVisible = value
        if !value { outputTask?.cancel(); outputTask = nil }
        else { startOutputRefresh() }
    }

    private func cancel() {
        revision &+= 1
        detailRevision &+= 1
        pending.forEach { $0.cancel() }
        pending.removeAll()
        detailTask?.cancel()
        detailTask = nil
        outputTask?.cancel(); outputTask = nil
    }

    private func clear() {
        items = []
        tasks = []
        detail = nil
        output = nil
        brief = nil
        route = nil
        detailState = .idle
        itemLimit = 50; taskLimit = 50; detailVisible = false; briefFailed = false; outputFailed = false
    }

    private func valid(_ generation: Int, _ authority: HolonConnectionIdentity,
                       _ agent: String) -> Bool {
        foreground && transport != nil && revision == generation
            && identity == authority && selectedAgentID == agent && !Task.isCancelled
    }

    private func reload() {
        guard let transport, let authority = identity else { return }
        guard foreground else {
            itemsState = .offline; tasksState = .offline
            return
        }
        guard let agent = selectedAgentID else {
            itemsState = .idle; tasksState = .idle
            return
        }
        let generation = revision
        let itemLimit = itemLimit, taskLimit = taskLimit
        itemsState = .loading
        tasksState = .loading
        pending.append(Task { [weak self] in
            guard let self, self.valid(generation, authority, agent) else { return }
            do {
                let result = try await transport.items(agentID: agent, limit: itemLimit)
                guard self.valid(generation, authority, agent) else { return }
                self.items = result
                self.itemsState = .loaded
            } catch {
                guard self.valid(generation, authority, agent) else { return }
                self.itemsState = .failed
                self.failure(error, authority: authority)
            }
        })
        pending.append(Task { [weak self] in
            guard let self, self.valid(generation, authority, agent) else { return }
            do {
                let result = try await transport.tasks(agentID: agent, limit: taskLimit)
                guard self.valid(generation, authority, agent) else { return }
                self.tasks = result
                self.tasksState = .loaded
            } catch {
                guard self.valid(generation, authority, agent) else { return }
                self.tasksState = .failed
                self.failure(error, authority: authority)
            }
        })
        if let route { open(route) }
    }

    func open(_ route: WorkRoute) {
        detailTask?.cancel()
        outputTask?.cancel(); outputTask = nil
        detailRevision &+= 1
        self.route = route
        detail = nil
        output = nil
        brief = nil
        briefFailed = false; outputFailed = false
        guard foreground, let transport, let authority = identity,
              let agent = selectedAgentID else {
            detailState = transport == nil ? .disconnected : .offline
            return
        }
        let generation = revision
        let operation = detailRevision
        detailState = .loading
        detailTask = Task { [weak self] in
            guard let self, self.valid(generation, authority, agent),
                  self.detailRevision == operation else { return }
            do {
                switch route {
                case .item(let id):
                    let result = try await transport.item(agentID: agent, id: id)
                    guard self.valid(generation, authority, agent),
                          self.detailRevision == operation else { return }
                    self.detail = result
                    if let id = result.briefID {
                        do {
                            let value = try await transport.brief(agentID: agent, id: id)
                            guard self.valid(generation, authority, agent), self.detailRevision == operation else { return }
                            self.brief = value
                        } catch {
                            guard self.valid(generation, authority, agent), self.detailRevision == operation else { return }
                            self.briefFailed = true; self.failure(error, authority: authority)
                        }
                    }
                case .task(let id):
                    let result = try await transport.task(agentID: agent, id: id)
                    guard self.valid(generation, authority, agent),
                          self.detailRevision == operation else { return }
                    self.detail = result
                    let output = try await transport.output(agentID: agent, id: id)
                    guard self.valid(generation, authority, agent),
                          self.detailRevision == operation else { return }
                    self.output = output
                case .brief(let id):
                    let result = try await transport.brief(agentID: agent, id: id)
                    guard self.valid(generation, authority, agent),
                          self.detailRevision == operation else { return }
                    self.brief = result
                }
                self.detailState = .loaded
                self.startOutputRefresh()
            } catch {
                guard self.valid(generation, authority, agent),
                      self.detailRevision == operation else { return }
                self.detailState = .failed
                self.failure(error, authority: authority)
            }
        }
    }

    /// Visible task output only. Three failed refreshes stop the loop until explicit retry.
    private func startOutputRefresh() {
        guard outputTask == nil, detailVisible, foreground, case .task(let id) = route,
              let detail, ["running", "active", "queued", "pending"].contains(detail.state),
              let transport, let authority = identity, let agent = selectedAgentID else { return }
        let generation = revision, operation = detailRevision
        outputTask = Task { [weak self] in
            var failures = 0
            while let self, self.valid(generation, authority, agent), self.detailRevision == operation, self.detailVisible {
                do {
                    try await Task.sleep(for: self.outputRefreshInterval)
                    let record = try await transport.task(agentID: agent, id: id)
                    let output = try await transport.output(agentID: agent, id: id)
                    guard self.valid(generation, authority, agent), self.detailRevision == operation, self.detailVisible else { return }
                    self.detail = record; self.output = output; self.outputFailed = false; failures = 0
                    if !["running", "active", "queued", "pending"].contains(record.state) { break }
                } catch {
                    guard self.valid(generation, authority, agent), self.detailRevision == operation, self.detailVisible else { return }
                    self.outputFailed = true; self.failure(error, authority: authority); failures += 1
                    if failures >= 3 { break }
                }
            }
            guard let self, self.revision == generation, self.detailRevision == operation else { return }
            self.outputTask = nil
        }
    }

    private func failure(_ error: any Error, authority: HolonConnectionIdentity) {
        guard let failure = error as? HolonHTTPFailure, failure.identity == authority,
              failure.statusCode == 401 || failure.statusCode == 403 else { return }
        onConnectionFailure?(failure)
    }
}
