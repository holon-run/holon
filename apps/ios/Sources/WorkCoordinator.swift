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
    var onConnectionFailure: ((HolonHTTPFailure) -> Void)?

    @ObservationIgnored private var transport: (any WorkTransport)?
    @ObservationIgnored private var identity: HolonConnectionIdentity?
    @ObservationIgnored private var foreground = true
    @ObservationIgnored private var revision = 0
    @ObservationIgnored private var detailRevision = 0
    @ObservationIgnored private var pending: [Task<Void, Never>] = []
    @ObservationIgnored private var detailTask: Task<Void, Never>?

    init() {}

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

    private func cancel() {
        revision &+= 1
        detailRevision &+= 1
        pending.forEach { $0.cancel() }
        pending.removeAll()
        detailTask?.cancel()
        detailTask = nil
    }

    private func clear() {
        items = []
        tasks = []
        detail = nil
        output = nil
        brief = nil
        route = nil
        detailState = .idle
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
        itemsState = .loading
        tasksState = .loading
        pending.append(Task { [weak self] in
            guard let self, self.valid(generation, authority, agent) else { return }
            do {
                let result = try await transport.items(agentID: agent)
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
                let result = try await transport.tasks(agentID: agent)
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
        detailRevision &+= 1
        self.route = route
        detail = nil
        output = nil
        brief = nil
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
            } catch {
                guard self.valid(generation, authority, agent),
                      self.detailRevision == operation else { return }
                self.detailState = .failed
                self.failure(error, authority: authority)
            }
        }
    }

    private func failure(_ error: any Error, authority: HolonConnectionIdentity) {
        guard let failure = error as? HolonHTTPFailure, failure.identity == authority,
              failure.statusCode == 401 || failure.statusCode == 403 else { return }
        onConnectionFailure?(failure)
    }
}
