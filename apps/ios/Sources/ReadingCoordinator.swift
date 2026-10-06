import Foundation
import Observation
import HolonClient

struct ReadingReadConfirmation: Equatable {
    fileprivate let revision: Int
    fileprivate let visibilityRevision: Int
    fileprivate let agentID: String
    fileprivate let epoch: String
    let through: Int64
}

@Observable @MainActor
final class ReadingCoordinator {
    private(set) var agents: [ReadingAgent] = []
    private(set) var selectedAgentID: String?
    private(set) var snapshot: HolonConversationSnapshot?
    private(set) var briefs: [String: JSONValue] = [:]
    private(set) var visibleBriefIDs: Set<String> = []
    private(set) var activities: [String: JSONValue] = [:]
    private(set) var status: ReadingStatus = .disconnected
    private(set) var canLoadHistory = false
    private(set) var isLoadingHistory = false
    private(set) var readStatus: ReadingReadStatus = .idle
    private(set) var readingPosition: String?
    var onConnectionFailure: ((HolonHTTPFailure) -> Void)?

    @ObservationIgnored private let cache: ReadingCache
    @ObservationIgnored private var transport: (any ReadingTransport)?
    @ObservationIgnored private var identity: HolonConnectionIdentity?
    @ObservationIgnored private var partition: ReadingPartition?
    @ObservationIgnored private var foreground = true
    @ObservationIgnored private var revision = 0
    @ObservationIgnored private var tasks: [UUID: Task<Void, Never>] = [:]
    @ObservationIgnored private var streams: [ReadingStream] = []
    @ObservationIgnored private var reducer: HolonConversationReducer?
    @ObservationIgnored private var before: String?
    @ObservationIgnored private var recoveryAttempts = 0
    @ObservationIgnored private var rosterRefreshing = false
    @ObservationIgnored private var rosterRefreshPending = false
    @ObservationIgnored private var expansionCount = 0
    @ObservationIgnored private var expanding: Set<String> = []
    @ObservationIgnored private var briefRecency: [String] = []
    @ObservationIgnored private var visibilityRevision = 0

    init() { cache = ReadingCache() }
    init(cache: ReadingCache) { self.cache = cache }

    func activate(client: HolonClient, identity: HolonConnectionIdentity, apiBaseURL: URL) {
        activate(transport: ReadingClientTransport(client: client, authority: identity),
                 identity: identity, apiBaseURL: apiBaseURL)
    }

    // Injectable only inside the app module, with identical authority fencing.
    func activate(transport: any ReadingTransport, identity: HolonConnectionIdentity, apiBaseURL: URL) {
        let previous = self.transport
        stop()
        if let previous { Task { await previous.close() } }
        clearDisplay()
        self.transport = transport
        self.identity = identity
        partition = ReadingPartition(apiBaseURL: apiBaseURL, identity: identity)
        guard partition != nil else {
            status = .incompatible
            return
        }
        restoreCache()
        recoveryAttempts = 0
        if foreground { bootstrap() } else { status = .offline }
    }

    func disconnect() {
        let previous = transport
        stop()
        transport = nil
        identity = nil
        partition = nil
        clearDisplay()
        status = .disconnected
        if let previous { Task { await previous.close() } }
    }

    func setForeground(_ active: Bool) {
        guard foreground != active else { return }
        foreground = active
        stop()
        guard transport != nil, partition != nil else { return }
        if active { recoveryAttempts = 0; bootstrap() }
        else { status = .offline }
    }

    func selectAgent(_ agentID: String?) {
        guard agentID == nil || (agentID?.isEmpty == false && (agentID?.utf8.count ?? 0) <= 512) else { return }
        guard selectedAgentID != agentID else { return }
        stop()
        selectedAgentID = agentID
        clearConversation()
        restoreCache()
        recoveryAttempts = 0
        if foreground, transport != nil, partition != nil { bootstrap() }
    }

    func refresh() async {
        guard foreground, transport != nil, partition != nil else { return }
        stop()
        recoveryAttempts = 0
        let task = bootstrap()
        await task?.value
    }

    private func stop() {
        revision += 1
        for task in tasks.values { task.cancel() }
        tasks.removeAll()
        for stream in streams { stream.close() }
        streams.removeAll()
        rosterRefreshing = false
        rosterRefreshPending = false
        expansionCount = 0
        expanding.removeAll()
        isLoadingHistory = false
        if readStatus == .pending { readStatus = .idle }
        reducer = nil
    }

    private func clearConversation() {
        snapshot = nil
        reducer = nil
        clearBriefCache()
        activities.removeAll()
        before = nil
        canLoadHistory = false
        readStatus = .idle
        readingPosition = nil
    }

    private func clearBriefCache() {
        briefs.removeAll()
        briefRecency.removeAll()
        if !visibleBriefIDs.isEmpty { visibilityRevision += 1 }
        visibleBriefIDs.removeAll()
    }

    private func clearDisplay() {
        agents = []
        selectedAgentID = nil
        clearConversation()
    }

    private func current(_ token: Int) -> Bool {
        token == revision && foreground && transport != nil && identity != nil && !Task.isCancelled
    }

    @discardableResult
    private func launch(_ body: @escaping @MainActor (Int) async -> Void) -> Task<Void, Never> {
        let id = UUID(), token = revision
        let task = Task { [weak self] in
            guard let self, self.current(token) else { return }
            await body(token)
            self.tasks.removeValue(forKey: id)
        }
        tasks[id] = task
        return task
    }

    private func validate(_ value: HolonConversationSnapshot) throws {
        guard value.agentID == selectedAgentID,
              value.runtimeID == identity?.runtimeID,
              value.visibilityScopeID == identity?.visibilityScopeID else {
            throw HolonConversationError.bootstrapRequired
        }
        guard (try JSONEncoder().encode(value.raw)).count <= 262_144 else {
            throw HolonConversationError.bootstrapRequired
        }
    }

    private func boundedRoster(_ roster: [ReadingAgent]) -> [ReadingAgent] {
        var seen: Set<String> = []
        return roster.filter { !$0.id.isEmpty && $0.id.utf8.count <= 512 && seen.insert($0.id).inserted }
            .prefix(80).map {
                ReadingAgent(id: $0.id, name: String($0.name.prefix(160)),
                             preview: String($0.preview.prefix(240)),
                             operatorPreview: $0.operatorPreview.map { String($0.prefix(240)) },
                             unreadCount: $0.unreadCount)
            }
    }

    @discardableResult
    private func bootstrap() -> Task<Void, Never>? {
        guard foreground, let transport, partition != nil else { return nil }
        status = .syncing
        return launch { [weak self] token in
            guard let self else { return }
            do {
                let roster = try await transport.roster()
                guard self.current(token) else { return }
                self.agents = self.boundedRoster(roster)
                if let agentID = self.selectedAgentID {
                    let value = try await transport.conversation(agentID: agentID, before: nil)
                    guard self.current(token) else { return }
                    try self.validate(value)
                    if self.snapshot?.eventLogEpoch != value.eventLogEpoch {
                        self.clearBriefCache()
                    }
                    // Summary revisions cannot prove detail freshness across a stream gap.
                    self.activities.removeAll()
                    self.snapshot = value
                    self.reducer = HolonConversationReducer(snapshot: value, maximumTurns: 180)
                    self.before = value.nextBeforeCursor
                    self.canLoadHistory = value.hasMore
                }
                self.persist()
                self.status = .live
                self.watch(agentID: nil, after: nil)
                if let agentID = self.selectedAgentID {
                    self.watch(agentID: agentID, after: self.snapshot?.snapshotCursor)
                }
            } catch {
                guard self.current(token) else { return }
                self.failed(error, token: token, recover: true)
            }
        }
    }

    private func watch(agentID: String?, after: String?) {
        guard let transport else { return }
        launch { [weak self] token in
            guard let self else { return }
            do {
                let opened = try await transport.stream(agentID: agentID, after: after)
                guard self.current(token) else { opened.close(); return }
                self.streams.append(opened)
                defer { opened.close() }
                for try await event in opened.events {
                    guard self.current(token) else { return }
                    if agentID == nil {
                        guard event.event == "agent_roster_hint" else {
                            throw HolonConversationError.bootstrapRequired
                        }
                        self.refreshRoster(token: token)
                    } else if var reducer = self.reducer {
                        let commit = try reducer.acceptCommit(event)
                        self.reducer = reducer
                        if let commit {
                            let updated = commit.snapshot
                            try self.validate(updated)
                            let previous = self.snapshot
                            if let displayed = previous {
                                self.snapshot = try mergeConversationHistory(updated, page: displayed, maximumTurns: 180)
                            } else { self.snapshot = updated }
                            if previous?.eventLogEpoch != updated.eventLogEpoch {
                                self.clearBriefCache()
                                self.activities.removeAll()
                            } else {
                                self.activities = self.activities.filter { id, _ in
                                    guard case .array(let oldTurns) = previous?.raw["turns"],
                                          case .array(let newTurns) = self.snapshot?.raw["turns"] else { return false }
                                    let old = oldTurns.first { $0["turn_id"]?.readingString == id }
                                    let new = newTurns.first { $0["turn_id"]?.readingString == id }
                                    guard old != nil && new != nil,
                                          old?["revision"] == new?["revision"] else { return false }
                                    guard let invalidated = commit.detailInvalidations[id] else { return true }
                                    return (self.activities[id]?["detail_revision"]?.readingInteger ?? -1) >= invalidated
                                }
                            }
                            self.persist()
                        }
                    }
                }
                throw HolonClientError.streamEnded
            } catch {
                guard self.current(token) else { return }
                self.failed(error, token: token, recover: true)
            }
        }
    }

    private func refreshRoster(token: Int) {
        guard let transport else { return }
        if rosterRefreshing { rosterRefreshPending = true; return }
        rosterRefreshing = true
        launch { [weak self] captured in
            guard let self else { return }
            defer {
                if self.current(captured) {
                    self.rosterRefreshing = false
                    if self.rosterRefreshPending {
                        self.rosterRefreshPending = false
                        self.refreshRoster(token: captured)
                    }
                }
            }
            do {
                let value = try await transport.roster()
                guard self.current(token) else { return }
                self.agents = self.boundedRoster(value)
                self.persist()
            } catch {
                if self.current(token) { self.failed(error, token: token, recover: true) }
            }
        }
    }

    private func failed(_ error: any Error, token: Int, recover: Bool) {
        guard current(token) else { return }
        if let failure = error as? HolonHTTPFailure {
            guard failure.identity == identity else { return }
            if failure.statusCode == 401 || failure.statusCode == 403 {
                let target: ReadingStatus = failure.statusCode == 401 ? .sessionExpired : .permissionDenied
                stop()
                if let partition { cache.remove(partition) }
                clearDisplay()
                status = target
                onConnectionFailure?(failure)
                return
            }
        }
        guard recover else { return }
        stop()
        if error is HolonConversationError {
            if let partition { cache.remove(partition) }
            clearConversation()
        }
        status = .offline
        guard recoveryAttempts < 3 else {
            if error as? HolonConversationError == .malformedProtocol ||
                error as? HolonClientError == .malformedResponse ||
                error as? HolonClientError == .unexpectedContentType {
                status = .incompatible
            }
            return
        }
        recoveryAttempts += 1
        let delay = UInt64(recoveryAttempts) * 500_000_000
        launch { [weak self] captured in
            do { try await Task.sleep(nanoseconds: delay) } catch { return }
            guard let self, self.current(captured) else { return }
            self.bootstrap()
        }
    }

    func loadHistory() async {
        guard foreground, status == .live, !isLoadingHistory, canLoadHistory,
              let agentID = selectedAgentID, let cursor = before, let transport else { return }
        isLoadingHistory = true
        let task = launch { [weak self] token in
            guard let self else { return }
            defer { if self.current(token) { self.isLoadingHistory = false } }
            do {
                let page = try await transport.conversation(agentID: agentID, before: cursor)
                guard self.current(token), self.before == cursor, let existing = self.snapshot else { return }
                try self.validate(page)
                guard page.eventLogEpoch == existing.eventLogEpoch else { return }
                self.snapshot = try mergeConversationHistory(existing, page: page, maximumTurns: 180)
                self.before = page.nextBeforeCursor
                self.canLoadHistory = page.hasMore && page.nextBeforeCursor != cursor
                self.persist()
            } catch {
                if self.current(token) {
                    if error is HolonConversationError { self.canLoadHistory = false }
                    self.failed(error, token: token, recover: false)
                }
            }
        }
        await task.value
    }

    func loadBrief(_ briefID: String) async {
        if briefs[briefID] != nil { touchBrief(briefID) }
        else { await expand(briefID, brief: true) }
    }
    func loadActivities(_ turnID: String) async { await expand(turnID, brief: false) }

    private func touchBrief(_ id: String) {
        briefRecency.removeAll { $0 == id }
        briefRecency.append(id)
    }

    private func cacheBrief(_ value: JSONValue, id: String) {
        if briefs[id] == nil, briefs.count >= 80 {
            guard let evicted = briefRecency.first(where: { !visibleBriefIDs.contains($0) }) else { return }
            briefs.removeValue(forKey: evicted)
            briefRecency.removeAll { $0 == evicted }
        }
        briefs[id] = value
        touchBrief(id)
    }

    private func expand(_ id: String, brief: Bool) async {
        let key = (brief ? "brief:" : "turn:") + id
        guard foreground, status == .live, !id.isEmpty, id.utf8.count <= 512,
              brief ? briefs[id] == nil : activities[id] == nil,
              expansionCount < 2, !expanding.contains(key),
              let agentID = selectedAgentID, let transport else { return }
        expansionCount += 1
        expanding.insert(key)
        let liveCursor = snapshot?.snapshotCursor
        let epoch = snapshot?.eventLogEpoch
        let task = launch { [weak self] token in
            guard let self else { return }
            defer {
                if self.current(token) {
                    self.expansionCount -= 1
                    self.expanding.remove(key)
                }
            }
            do {
                let value = try await (brief ? transport.brief(agentID: agentID, briefID: id) :
                    transport.activities(agentID: agentID, turnID: id))
                guard self.current(token), self.snapshot?.eventLogEpoch == epoch,
                      brief || self.snapshot?.snapshotCursor == liveCursor,
                      (try JSONEncoder().encode(value)).count <= 262_144 else { return }
                if brief {
                    self.cacheBrief(value, id: id)
                } else {
                    if self.activities.count >= 8 { self.activities.removeAll() }
                    self.activities[id] = value
                }
            } catch {
                if self.current(token) { self.failed(error, token: token, recover: false) }
            }
        }
        await withTaskCancellationHandler {
            await task.value
        } onCancel: {
            task.cancel()
        }
    }

    private var referencedBriefIDs: Set<String> {
        guard let snapshot else { return [] }
        let turns = [snapshot.raw["turns"], snapshot.raw["active_turns"]].flatMap { value -> [JSONValue] in
            if case .array(let turns) = value { return turns }
            return []
        }
        return Set(turns.flatMap { turn -> [String] in
            if case .array(let ids) = turn["brief_ids"] { return ids.compactMap(\.readingString) }
            return []
        })
    }

    func setBriefVisible(_ briefID: String, visible: Bool) {
        if !visible {
            if visibleBriefIDs.remove(briefID) != nil { visibilityRevision += 1 }
            return
        }
        guard referencedBriefIDs.contains(briefID), let brief = briefs[briefID],
              brief["id"]?.readingString == briefID,
              brief["agent_id"]?.readingString == selectedAgentID else { return }
        if visibleBriefIDs.insert(briefID).inserted { visibilityRevision += 1 }
        touchBrief(briefID)
    }

    var readThroughForVisibleBriefs: Int64? {
        guard foreground, status == .live, readStatus != .pending,
              let agentID = selectedAgentID,
              let snapshot,
              let through = visibleBriefIDs.compactMap({ id -> Int64? in
                  guard referencedBriefIDs.contains(id),
                        let value = briefs[id], value["id"]?.readingString == id,
                        value["agent_id"]?.readingString == agentID,
                        let seq = value["created_event_seq"]?.readingInteger, seq > 0 else { return nil }
                  return seq
              }).max(),
              let head = snapshot.raw["snapshot_through_seq"]?.readingInteger,
              through <= head else { return nil }
        return through
    }

    var readConfirmationForVisibleBriefs: ReadingReadConfirmation? {
        guard let through = readThroughForVisibleBriefs, let agentID = selectedAgentID,
              let epoch = snapshot?.eventLogEpoch else { return nil }
        return ReadingReadConfirmation(revision: revision, visibilityRevision: visibilityRevision,
                                       agentID: agentID, epoch: epoch, through: through)
    }

    func markRead(confirmation requested: ReadingReadConfirmation? = nil) async {
        guard let current = readConfirmationForVisibleBriefs,
              requested == nil || requested == current,
              let agentID = selectedAgentID, let transport, let snapshot else { return }
        let through = current.through
        readStatus = .pending
        let task = launch { [weak self] token in
            guard let self else { return }
            do {
                let value = try await transport.markRead(agentID: agentID, through: through)
                guard self.current(token) else { return }
                guard let confirmed = value["applied_read_through_event_seq"]?.readingInteger,
                      confirmed >= through,
                      let state = value["state"],
                      state["agent_id"]?.readingString == agentID,
                      state["event_log_epoch"]?.readingString == snapshot.eventLogEpoch,
                      state["visibility_scope_id"]?.readingString == snapshot.visibilityScopeID,
                      state["read_through_event_seq"]?.readingInteger == confirmed else {
                    self.readStatus = .failed
                    return
                }
                self.readStatus = .confirmed
                // Reload authoritative unread metadata, never optimistically clear it.
                self.refreshRoster(token: token)
            } catch {
                guard self.current(token) else { return }
                self.readStatus = .failed
                self.failed(error, token: token, recover: false)
            }
        }
        await task.value
    }

    func rememberPosition(turnID: String) {
        guard selectedAgentID != nil, turnID.utf8.count <= 512 else { return }
        readingPosition = turnID
        persist()
    }

    private func restoreCache() {
        guard let partition else { return }
        if let roster = cache.load(partition, agentID: nil) { agents = boundedRoster(roster.agents) }
        if let entry = cache.load(partition, agentID: selectedAgentID), let raw = entry.snapshot,
           let value = try? HolonConversationSnapshot(raw: raw),
           (try? validate(value)) != nil {
            snapshot = value
            readingPosition = entry.position
            // Cached history is readable, but pagination and writes require a new bootstrap.
            canLoadHistory = false
        }
    }

    private func persist() {
        guard let partition else { return }
        cache.save(.init(partition: partition, agentID: nil, agents: agents, snapshot: nil,
                         position: nil, savedAt: Date()))
        if let selectedAgentID, let snapshot {
            cache.save(.init(partition: partition, agentID: selectedAgentID, agents: [],
                             snapshot: snapshot.raw, position: readingPosition, savedAt: Date()))
        }
    }
}
