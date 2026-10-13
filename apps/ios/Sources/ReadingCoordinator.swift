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

private struct ReadingReconnectToken: Sendable {
    let coordinatorID: UUID
    let attemptID: UUID
}

private enum ReadingReconnectContext {
    @TaskLocal static var token: ReadingReconnectToken? = nil
}

@Observable @MainActor
final class ReadingCoordinator {
    private(set) var agents: [ReadingAgent] = []
    private(set) var rosterRevision = 0
    // Only a successful roster response in the current context authorizes missing-agent decisions.
    private(set) var isRosterCurrent = false
    private(set) var selectedAgentID: String?
    private(set) var snapshot: HolonConversationSnapshot?
    private(set) var briefs: [String: JSONValue] = [:]
    private(set) var loadingBriefs: Set<String> = []
    private(set) var failedBriefs: Set<String> = []
    private(set) var visibleBriefIDs: Set<String> = []
    private(set) var activities: [String: JSONValue] = [:]
    private(set) var activityDetails: [String: JSONValue] = [:]
    private(set) var activityCacheRevision = 0
    var activityReadKey: String {
        "\(status.rawValue)|\(activityCacheRevision)|\(snapshot?.snapshotCursor ?? "")"
    }
    private(set) var loadingActivities: Set<String> = []
    private(set) var failedActivities: Set<String> = []
    private(set) var status: ReadingStatus = .disconnected
    private(set) var canLoadHistory = false
    private(set) var isLoadingHistory = false
    private(set) var readStatus: ReadingReadStatus = .idle
    private(set) var readingPosition: String?
    var isRefreshing: Bool { rosterRefreshing || conversationRefreshing }
    var isShowingCachedContent: Bool {
        selectedAgentID == nil ? rosterIsCached : conversationIsCached
    }
    var onConnectionFailure: ((HolonHTTPFailure) -> Void)?
    var onReconnect: (@MainActor () async -> Void)?

    @ObservationIgnored private let cache: ReadingCache
    @ObservationIgnored private var transport: (any ReadingTransport)?
    @ObservationIgnored private var identity: HolonConnectionIdentity?
    @ObservationIgnored private var partition: ReadingPartition?
    @ObservationIgnored private let reconnectCoordinatorID = UUID()
    @ObservationIgnored private var reconnectAttempt: (id: UUID, task: Task<Void, Never>)?
    @ObservationIgnored private var reportSessionID = UUID()
    @ObservationIgnored private var reportDrafts: [ContentReportKey: ContentReportDraft] = [:]
    private var foreground = true
    @ObservationIgnored private var revision = 0
    @ObservationIgnored private var tasks: [UUID: Task<Void, Never>] = [:]
    @ObservationIgnored private var streams: [ReadingStream] = []
    @ObservationIgnored private var reducer: HolonConversationReducer?
    @ObservationIgnored private var before: String?
    @ObservationIgnored private var recoveryAttempts = 0
    private var rosterRefreshing = false
    private var conversationRefreshing = false
    private var rosterIsCached = false
    private var conversationIsCached = false
    @ObservationIgnored private var rosterSavedAt: Date?
    @ObservationIgnored private var conversationSavedAt: Date?
    @ObservationIgnored private var restoration: Task<Void, Never>?
    @ObservationIgnored private let prefetchBriefLimit: Int
    private struct CacheWriteKey: Hashable {
        let partition: ReadingPartition
        let agentID: String
    }
    @ObservationIgnored private var cacheWrites: [CacheWriteKey: (UUID, Task<Void, Never>)] = [:]
    @ObservationIgnored private var rosterRefreshPending = false
    @ObservationIgnored private var expansionCount = 0
    @ObservationIgnored private var expanding: Set<String> = []
    @ObservationIgnored private var waitingBriefs: Set<String> = []
    @ObservationIgnored private var waitingActivities: Set<String> = []
    @ObservationIgnored private var briefRecency: [String] = []
    @ObservationIgnored private var visibilityRevision = 0
    @ObservationIgnored private var previewTask: Task<Void, Never>?
    @ObservationIgnored private var previewedAgentIDs: Set<String> = []
    private struct DetailVersion: Equatable {
        let turnID: String
        let pageRevision: Int64
        let activityRevision: Int64
    }
    @ObservationIgnored private var detailVersions: [String: DetailVersion] = [:]

    init() { cache = ReadingCache(); prefetchBriefLimit = 8 }
    init(cache: ReadingCache, prefetchBriefLimit: Int = 8) {
        self.cache = cache
        self.prefetchBriefLimit = min(8, max(0, prefetchBriefLimit))
    }

    func activate(client: HolonClient, identity: HolonConnectionIdentity, apiBaseURL: URL) {
        activate(transport: ReadingClientTransport(client: client, authority: identity),
                 identity: identity, apiBaseURL: apiBaseURL)
    }

    // Injectable only inside the app module, with identical authority fencing.
    func activate(transport: any ReadingTransport, identity: HolonConnectionIdentity, apiBaseURL: URL) {
        guard acceptsReconnectMutation else { return }
        ReadingReconnectContext.$token.withValue(nil) {
            applyActivation(transport: transport, identity: identity, apiBaseURL: apiBaseURL)
        }
    }

    private func applyActivation(transport: any ReadingTransport, identity: HolonConnectionIdentity, apiBaseURL: URL) {
        let next = ReadingPartition(apiBaseURL: apiBaseURL, identity: identity)
        if partition != next || next == nil { cancelReconnect() }
        reportSessionID = UUID()
        reportDrafts.removeAll()
        let previous = self.transport
        stop()
        if let previous { Task { await previous.close() } }
        if partition != next || next == nil { clearDisplay() }
        self.transport = transport
        self.identity = identity
        partition = next
        guard partition != nil else {
            status = .incompatible
            return
        }
        markCached()
        prepareCache()
        recoveryAttempts = 0
        if foreground { bootstrap() } else { status = .offline }
    }

    /// A confirmed local context grants reading only, never network authority.
    func activateOffline(partition: ReadingPartition) {
        guard acceptsReconnectMutation else { return }
        ReadingReconnectContext.$token.withValue(nil) {
            applyOfflineActivation(partition: partition)
        }
    }

    private func applyOfflineActivation(partition: ReadingPartition) {
        if self.partition != partition { cancelReconnect() }
        reportSessionID = UUID()
        reportDrafts.removeAll()
        let previous = transport
        stop()
        transport = nil
        identity = nil
        if self.partition != partition { clearDisplay() }
        self.partition = partition
        activities.removeAll()
        clearActivityDetails()
        canLoadHistory = false
        markCached()
        status = .offline
        prepareCache()
        if let previous { Task { await previous.close() } }
    }

    func invalidate(_ partition: ReadingPartition) {
        guard acceptsReconnectMutation else { return }
        for (key, write) in cacheWrites where key.partition == partition {
            write.1.cancel()
            cacheWrites.removeValue(forKey: key)
        }
        cache.remove(partition)
        guard self.partition == partition else { return }
        disconnect()
    }

    func disconnect() {
        guard acceptsReconnectMutation else { return }
        ReadingReconnectContext.$token.withValue(nil) {
            applyDisconnect()
        }
    }

    private func applyDisconnect() {
        cancelReconnect()
        reportSessionID = UUID()
        reportDrafts.removeAll()
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
        guard partition != nil else { return }
        markCached()
        if active {
            prepareCache()
            recoveryAttempts = 0
            if transport != nil { bootstrap() } else { status = .offline }
        } else { status = .offline }
    }

    func selectAgent(_ agentID: String?) {
        guard agentID == nil || (agentID?.isEmpty == false && (agentID?.utf8.count ?? 0) <= 512) else { return }
        guard selectedAgentID != agentID else { return }
        reportSessionID = UUID()
        reportDrafts.removeAll()
        stop()
        selectedAgentID = agentID
        clearConversation()
        prepareCache()
        recoveryAttempts = 0
        if foreground, transport != nil, partition != nil { bootstrap() }
        else if partition != nil { status = .offline }
    }

    func refresh() async {
        guard !Task.isCancelled, foreground, let partition else { return }
        guard transport != nil else {
            if let reconnectAttempt {
                await reconnectAttempt.task.value
                return
            }
            guard let onReconnect else { return }
            let id = UUID()
            let context = ReadingReconnectToken(coordinatorID: reconnectCoordinatorID, attemptID: id)
            let task = Task { [weak self] in
                guard let self, !Task.isCancelled, self.foreground,
                      self.partition == partition, self.transport == nil,
                      self.reconnectAttempt?.id == id else { return }
                await ReadingReconnectContext.$token.withValue(context) {
                    await onReconnect()
                }
            }
            reconnectAttempt = (id, task)
            await task.value
            if reconnectAttempt?.id == id { reconnectAttempt = nil }
            return
        }
        stop()
        recoveryAttempts = 0
        markCached()
        prepareCache()
        let task = bootstrap()
        await task?.value
    }

    var contentReportScope: ContentReportScope? {
        guard foreground, status == .live, identity != nil, transport != nil,
              let partition, let selectedAgentID,
              snapshot?.agentID == selectedAgentID, let epoch = snapshot?.eventLogEpoch else { return nil }
        return ContentReportScope(sessionID: reportSessionID, partition: partition,
                                  agentID: selectedAgentID, epoch: epoch)
    }

    func contentReportDraft(target: ContentReportTarget, scope: ContentReportScope) -> ContentReportDraft? {
        guard contentReportScope == scope else { return nil }
        let key = ContentReportKey(turnID: target.turnID, messageID: target.messageID)
        if let existing = reportDrafts[key], existing.scope == scope { return existing }
        if reportDrafts.count >= 20 {
            guard let disposable = reportDrafts.first(where: { $0.value.request == nil })?.key else {
                return nil
            }
            reportDrafts.removeValue(forKey: disposable)
        }
        let draft = ContentReportDraft(scope: scope, target: target)
        reportDrafts[key] = draft
        return draft
    }

    func createContentReport(_ body: HolonContentReportRequest, scope: ContentReportScope) async throws -> HolonContentReportResponse {
        guard contentReportScope == scope, body.agentID == scope.agentID, let transport else {
            throw CancellationError()
        }
        let token = revision
        do {
            let response = try await transport.createContentReport(body)
            guard contentReportScope == scope else { throw CancellationError() }
            return response
        } catch {
            guard contentReportScope == scope else { throw CancellationError() }
            failed(error, token: token, recover: false)
            throw error
        }
    }

    private var acceptsReconnectMutation: Bool {
        guard let token = ReadingReconnectContext.token,
              token.coordinatorID == reconnectCoordinatorID else { return true }
        return reconnectAttempt?.id == token.attemptID
    }

    // Same-partition activation and Agent selection preserve the callback's identity update.
    private func cancelReconnect() {
        let task = reconnectAttempt?.task
        reconnectAttempt = nil
        task?.cancel()
    }

    private func stop() {
        restoration?.cancel()
        restoration = nil
        previewTask?.cancel()
        previewTask = nil
        previewedAgentIDs = []
        revision += 1
        isRosterCurrent = false
        for task in tasks.values { task.cancel() }
        tasks.removeAll()
        for stream in streams { stream.close() }
        streams.removeAll()
        rosterRefreshing = false
        conversationRefreshing = false
        rosterRefreshPending = false
        expansionCount = 0
        expanding.removeAll()
        waitingBriefs.removeAll()
        waitingActivities.removeAll()
        loadingActivities.removeAll()
        loadingBriefs.removeAll()
        isLoadingHistory = false
        if readStatus == .pending { readStatus = .idle }
        reducer = nil
    }

    private func clearConversation() {
        snapshot = nil
        reducer = nil
        clearBriefCache()
        activities.removeAll()
        clearActivityDetails()
        failedActivities.removeAll()
        before = nil
        canLoadHistory = false
        readStatus = .idle
        readingPosition = nil
        conversationSavedAt = nil
        conversationIsCached = false
    }

    private func clearBriefCache() {
        briefs.removeAll()
        failedBriefs.removeAll()
        loadingBriefs.removeAll()
        briefRecency.removeAll()
        if !visibleBriefIDs.isEmpty { visibilityRevision += 1 }
        visibleBriefIDs.removeAll()
    }

    private func clearDisplay() {
        agents = []
        rosterSavedAt = nil
        rosterIsCached = false
        selectedAgentID = nil
        clearConversation()
    }

    private func current(_ token: Int) -> Bool {
        token == revision && foreground && transport != nil && identity != nil && !Task.isCancelled
    }

    @discardableResult
    private func launch(onFinish: (@MainActor (Int) -> Void)? = nil,
                        _ body: @escaping @MainActor (Int) async -> Void) -> Task<Void, Never> {
        let id = UUID(), token = revision
        let task = Task { [weak self] in
            guard let self else { return }
            defer { self.tasks.removeValue(forKey: id); onFinish?(token) }
            guard self.current(token) else { return }
            await body(token)
        }
        tasks[id] = task
        return task
    }

    private func validate(_ value: HolonConversationSnapshot) throws {
        guard value.agentID == selectedAgentID,
              value.runtimeID == partition?.runtime,
              value.visibilityScopeID == partition?.visibility else {
            throw HolonConversationError.bootstrapRequired
        }
        guard (try JSONEncoder().encode(value.raw)).count <= 262_144 else {
            throw HolonConversationError.bootstrapRequired
        }
    }

    private func boundedRoster(_ roster: [ReadingAgent]) -> [ReadingAgent] {
        var seen: Set<String> = []
        return roster.filter { !$0.id.isEmpty && $0.id.utf8.count <= 512 && seen.insert($0.id).inserted }
            .map {
                var agent = $0
                agent.name = String(agent.name.prefix(160))
                agent.preview = String(agent.preview.prefix(240))
                agent.operatorPreview = agent.operatorPreview.map { String($0.prefix(240)) }
                agent.currentRunID = agent.currentRunID.flatMap { !$0.isEmpty && $0.utf8.count <= 512 ? $0 : nil }
                agent.posture = agent.posture.map { String($0.prefix(64)) }
                agent.effectiveModel = agent.effectiveModel.map { String($0.prefix(512)) }
                return agent
            }
    }

    /// Metadata never holds up roster publication. Only the displayed window is enriched,
    /// at most two reads concurrently, and every result belongs to this exact snapshot.
    func loadVisiblePreviews(agentIDs: [String]) async {
        previewTask?.cancel()
        guard let transport, status == .live else { return }
        let token = revision, roster = rosterRevision
        let ids = Array(agentIDs.filter { id in
            !previewedAgentIDs.contains(id) && agents.contains { $0.id == id }
        }.prefix(80))
        let task = Task { [weak self] in
            guard let self else { return }
            do {
                try await withThrowingTaskGroup(of: (String, ReadingOperatorPreview?).self) { group in
                    var remaining = ids.makeIterator()
                    for _ in 0..<2 {
                        if let id = remaining.next() {
                            group.addTask { (id, try await transport.operatorPreview(agentID: id)) }
                        }
                    }
                    while let (id, value) = try await group.next() {
                        guard self.current(token), self.rosterRevision == roster else { group.cancelAll(); return }
                        if let index = self.agents.firstIndex(where: { $0.id == id }) {
                            self.agents[index].operatorPreview = value?.text
                            self.agents[index].operatorAt = value?.createdAt
                            self.previewedAgentIDs.insert(id)
                        }
                        if let id = remaining.next() {
                            group.addTask { (id, try await transport.operatorPreview(agentID: id)) }
                        }
                    }
                }
                if self.current(token), self.rosterRevision == roster {
                    self.rosterSavedAt = Date()
                    self.persistRoster()
                }
            } catch {
                if self.current(token), self.rosterRevision == roster { self.failed(error, token: token, recover: true) }
            }
        }
        previewTask = task
        await withTaskCancellationHandler { await task.value } onCancel: { task.cancel() }
    }

    private enum RefreshResponse: Sendable {
        case roster(Result<[ReadingAgent], any Error>)
        case conversation(Result<HolonConversationSnapshot, any Error>)
    }

    @discardableResult
    private func bootstrap() -> Task<Void, Never>? {
        guard foreground, let transport, identity != nil, partition != nil else { return nil }
        isRosterCurrent = false
        status = .syncing
        rosterRefreshing = true
        conversationRefreshing = selectedAgentID != nil
        let restoration = restoration
        let agentID = selectedAgentID
        return launch { [weak self] token in
            guard let self else { return }
            await restoration?.value
            guard self.current(token) else { return }
            var refreshFailure: (any Error)?
            await withTaskGroup(of: RefreshResponse.self) { group in
                group.addTask {
                    do { return .roster(.success(try await transport.roster())) }
                    catch { return .roster(.failure(error)) }
                }
                if let agentID {
                    group.addTask {
                        do { return .conversation(.success(try await transport.conversation(agentID: agentID, before: nil))) }
                        catch { return .conversation(.failure(error)) }
                    }
                }
                for await response in group {
                    guard self.current(token) else { group.cancelAll(); return }
                    do {
                        switch response {
                        case .roster(let result):
                            self.rosterRefreshing = false
                            self.agents = self.boundedRoster(try result.get())
                            self.rosterRevision &+= 1
                            self.isRosterCurrent = true
                            self.previewedAgentIDs = []
                            self.rosterSavedAt = Date()
                            self.rosterIsCached = false
                            self.persistRoster()
                            if agentID == nil { self.status = .live }
                            self.watch(agentID: nil, after: nil)
                        case .conversation(let result):
                            self.conversationRefreshing = false
                            let value = try result.get()
                            try self.validate(value)
                            if let old = self.snapshot, old.eventLogEpoch != value.eventLogEpoch {
                                self.clearBriefCache()
                                self.readingPosition = nil
                            }
                            let displayed = try ReadingSnapshotWindow.merging(value, history: self.snapshot)
                            // A stream gap invalidates execution detail, not confirmed reading history.
                            self.activities.removeAll()
                            self.clearActivityDetails()
                            self.snapshot = displayed
                            self.reducer = HolonConversationReducer(snapshot: value, maximumTurns: 180)
                            self.before = displayed.nextBeforeCursor
                            self.canLoadHistory = displayed.hasMore
                            self.conversationSavedAt = Date()
                            self.conversationIsCached = false
                            self.status = .live
                            self.watch(agentID: agentID, after: value.snapshotCursor)
                            self.prefetchBriefs()
                            await self.persist()?.value
                            guard self.current(token) else { group.cancelAll(); return }
                        }
                    } catch {
                        refreshFailure = error
                        self.failed(error, token: token, recover: false)
                        guard self.current(token) else { group.cancelAll(); return }
                        if case .conversation = response {
                            self.status = .offline
                            self.conversationIsCached = self.snapshot != nil
                        } else if agentID == nil {
                            self.status = .offline
                            self.rosterIsCached = !self.agents.isEmpty
                        }
                    }
                }
            }
            guard self.current(token) else { return }
            // A roster failure cannot cancel a successful conversation refresh (or vice versa).
            if let refreshFailure, self.status != .live {
                self.failed(refreshFailure, token: token, recover: true)
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
                            let previousActivities = self.activities
                            let updated = commit.snapshot
                            try self.validate(updated)
                            let previous = self.snapshot
                            if let displayed = previous {
                                self.snapshot = try ReadingSnapshotWindow.merging(updated, history: displayed)
                            } else { self.snapshot = updated }
                            if previous?.eventLogEpoch != updated.eventLogEpoch {
                                self.clearBriefCache()
                                self.activities.removeAll()
                                self.readingPosition = nil
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
                            if previousActivities != self.activities { self.clearActivityDetails() }
                            self.conversationSavedAt = Date()
                            self.conversationIsCached = false
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
        isRosterCurrent = false
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
                self.rosterRevision &+= 1
                self.isRosterCurrent = true
                self.previewedAgentIDs = []
                self.rosterSavedAt = Date()
                self.rosterIsCached = false
                self.persistRoster()
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
                if let partition { invalidate(partition) } else { disconnect() }
                status = target
                onConnectionFailure?(failure)
                return
            }
        }
        guard recover else { return }
        stop()
        markCached()
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
                self.conversationSavedAt = Date()
                await self.persist()?.value
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
        if briefs[briefID] != nil { touchBrief(briefID); return }
        guard !briefID.isEmpty, briefID.utf8.count <= 512 else { return }
        guard foreground, status == .live, identity != nil, transport != nil else {
            failedBriefs.insert(briefID)
            return
        }
        guard !waitingBriefs.contains(briefID), waitingBriefs.count < 16 else { return }
        let captured = revision
        waitingBriefs.insert(briefID)
        loadingBriefs.insert(briefID)
        failedBriefs.remove(briefID)
        defer {
            if captured == revision { waitingBriefs.remove(briefID); loadingBriefs.remove(briefID) }
        }
        while expansionCount >= 2 {
            do { try await Task.sleep(for: .milliseconds(50)) } catch { return }
            guard current(captured) else { return }
        }
        guard current(captured) else { return }
        await expand(briefID, brief: true)
    }
    func loadActivities(_ turnID: String) async {
        guard activities[turnID] == nil else { return }
        await queueActivityRead(turnID)
    }

    private func queueActivityRead(_ turnID: String, before: String? = nil) async {
        guard !turnID.isEmpty, turnID.utf8.count <= 512 else { return }
        guard foreground, status == .live, identity != nil, transport != nil else {
            failedActivities.insert(turnID)
            return
        }
        let captured = revision
        // A full-screen reader can take over an inline read during presentation.
        // Wait for that request to settle rather than dropping the new owner.
        while waitingActivities.contains(turnID) || loadingActivities.contains(turnID) {
            do { try await Task.sleep(for: .milliseconds(50)) } catch { return }
            guard current(captured) else { return }
        }
        guard waitingActivities.count < 16 else { return }
        waitingActivities.insert(turnID); loadingActivities.insert(turnID)
        defer {
            if captured == revision { waitingActivities.remove(turnID); loadingActivities.remove(turnID) }
        }
        while expansionCount >= 2 {
            do { try await Task.sleep(for: .milliseconds(50)) } catch { return }
            guard current(captured) else { return }
        }
        guard current(captured), !Task.isCancelled else { return }
        if let before, activities[turnID]?["next_before_cursor"] != .string(before) { return }
        await expand(turnID, brief: false, before: before)
    }

    func loadOlderActivities(_ turnID: String) async {
        guard let page = activities[turnID], page["has_more"] == .bool(true),
              let cursor = page["next_before_cursor"]?.readingString else { return }
        await queueActivityRead(turnID, before: cursor)
    }

    func reloadActivities(_ turnID: String) async {
        guard status == .live, identity != nil, transport != nil,
              !loadingActivities.contains(turnID) else { return }
        clearActivityDetails()
        activities.removeValue(forKey: turnID)
        await loadActivities(turnID)
    }

    private func clearActivityDetails() {
        activityDetails.removeAll()
        detailVersions.removeAll()
        activityCacheRevision &+= 1
    }

    private func detailVersion(turnID: String, activity: ReadingActivity) -> DetailVersion {
        DetailVersion(turnID: turnID, pageRevision: activities[turnID]?["detail_revision"]?.readingInteger ?? -1,
                      activityRevision: activity.revision)
    }

    func loadActivityDetail(turnID: String, activityID: String) async {
        let key = "detail:" + activityID
        guard foreground, status == .live, identity != nil, expansionCount < 2, !expanding.contains(key),
              let transport, let agent = selectedAgentID,
              let activity = ActivityPresentation.items(activities[turnID]).first(where: { $0.id == activityID }),
              activity.detailID != nil else { return }
        let version = detailVersion(turnID: turnID, activity: activity)
        guard activityDetails[activityID] == nil || detailVersions[activityID] != version else { return }
        let page = activities[turnID], epoch = snapshot?.eventLogEpoch, cacheRevision = activityCacheRevision
        expansionCount += 1; expanding.insert(key); loadingActivities.insert(activityID)
        failedActivities.remove(activityID)
        let task = launch(onFinish: { [weak self] token in
            guard let self, token == self.revision else { return }
            self.expansionCount -= 1; self.expanding.remove(key); self.loadingActivities.remove(activityID)
        }) { [weak self] token in
            guard let self else { return }
            do {
                let detail = try await transport.activityDetail(agentID: agent, turnID: turnID, activity: activity)
                guard self.current(token), self.snapshot?.eventLogEpoch == epoch,
                      self.activities[turnID] == page, self.activityCacheRevision == cacheRevision,
                      (try JSONEncoder().encode(detail)).count <= 1_048_576 else { return }
                if self.activityDetails.count >= 8 {
                    self.activityDetails.removeAll(); self.detailVersions.removeAll()
                }
                self.activityDetails[activityID] = detail
                self.detailVersions[activityID] = version
            } catch {
                if self.current(token) {
                    self.failedActivities.insert(activityID)
                    self.failed(error, token: token, recover: false)
                }
            }
        }
        await withTaskCancellationHandler { await task.value } onCancel: { task.cancel() }
    }

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

    private func expand(_ id: String, brief: Bool, before: String? = nil) async {
        let key = (brief ? "brief:" : "turn:") + id
        guard foreground, status == .live, identity != nil, !id.isEmpty, id.utf8.count <= 512,
              brief ? briefs[id] == nil : (before != nil || activities[id] == nil),
              expansionCount < 2, !expanding.contains(key),
              let agentID = selectedAgentID, let transport else { return }
        expansionCount += 1
        expanding.insert(key)
        if brief { loadingBriefs.insert(id); failedBriefs.remove(id) }
        else { loadingActivities.insert(id); failedActivities.remove(id) }
        let liveCursor = snapshot?.snapshotCursor
        let epoch = snapshot?.eventLogEpoch
        let previous = activities[id]
        let task = launch(onFinish: { [weak self] token in
            guard let self, token == self.revision else { return }
            self.expansionCount -= 1; self.expanding.remove(key)
            if brief { self.loadingBriefs.remove(id) }
            else { self.loadingActivities.remove(id) }
        }) { [weak self] token in
            guard let self else { return }
            do {
                let value = try await (brief ? transport.brief(agentID: agentID, briefID: id) :
                    transport.activities(agentID: agentID, turnID: id, before: before))
                guard self.current(token), self.snapshot?.eventLogEpoch == epoch,
                      brief || self.snapshot?.snapshotCursor == liveCursor else { return }
                guard (try JSONEncoder().encode(value)).count <= (brief ? 262_144 : 4_194_304) else {
                    if brief { self.failedBriefs.insert(id) }
                    else { self.failedActivities.insert(id) }
                    return
                }
                if brief {
                    guard value["id"]?.readingString == id,
                          value["agent_id"]?.readingString == agentID,
                          value["event_log_epoch"] == nil || value["event_log_epoch"]?.readingString == epoch else {
                        throw HolonConversationError.malformedProtocol
                    }
                    self.cacheBrief(value, id: id)
                    self.conversationSavedAt = Date()
                    await self.persist()?.value
                } else {
                    guard let snapshot = self.snapshot else { return }
                    try ActivityPresentation.validate(value, snapshot: snapshot, turnID: id)
                    let page: JSONValue
                    if let before, let previous {
                        guard self.activities[id] == previous else { return }
                        page = try ActivityPresentation.merging(value, into: previous, requestedCursor: before)
                    } else { page = value }
                    if self.activities[id] == nil && self.activities.count >= 8 {
                        self.activities.removeAll(); self.clearActivityDetails()
                    }
                    self.activities[id] = page
                    let items = ActivityPresentation.items(page)
                    for activity in items where self.detailVersions[activity.id] != self.detailVersion(turnID: id, activity: activity) {
                        self.activityDetails.removeValue(forKey: activity.id)
                        self.detailVersions.removeValue(forKey: activity.id)
                    }
                    // An explicit reload may return identical revisions; expanded rows must read it again.
                    self.activityCacheRevision &+= 1
                }
            } catch {
                if self.current(token) {
                    if brief { self.failedBriefs.insert(id) }
                    else { self.failedActivities.insert(id) }
                    self.failed(error, token: token, recover: false)
                }
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
        guard foreground, status == .live, identity != nil, transport != nil, readStatus != .pending,
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
                guard self.snapshot?.eventLogEpoch == snapshot.eventLogEpoch else {
                    self.readStatus = .idle
                    return
                }
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
        guard selectedAgentID != nil, !turnID.isEmpty,
              turnID.utf8.count <= 512, readingPosition != turnID else { return }
        readingPosition = turnID
        persist()
    }

    private func restoreCache() {
        guard let partition else { return }
        if rosterSavedAt == nil, let roster = cache.load(partition, agentID: nil) {
            agents = boundedRoster(roster.agents)
            rosterRevision &+= 1
            rosterSavedAt = roster.savedAt
            rosterIsCached = true
        }
        guard let selectedAgentID, snapshot == nil,
              cacheWrites[CacheWriteKey(partition: partition, agentID: selectedAgentID)] == nil else { return }
        if let entry = cache.load(partition, agentID: selectedAgentID), let raw = entry.snapshot,
           let value = try? HolonConversationSnapshot(raw: raw),
           (try? validate(value)) != nil {
            snapshot = value
            readingPosition = entry.position
            conversationSavedAt = entry.savedAt
            conversationIsCached = true
            briefs = entry.briefs.filter { id, body in
                body["id"]?.readingString == id && body["agent_id"]?.readingString == selectedAgentID &&
                (body["event_log_epoch"] == nil || body["event_log_epoch"]?.readingString == value.eventLogEpoch)
            }
            briefRecency = entry.briefOrder.filter { briefs[$0] != nil }
            briefRecency += briefs.keys.filter { !briefRecency.contains($0) }.sorted()
            // Cached history is readable, but pagination and writes require a new bootstrap.
            canLoadHistory = false
        }
    }

    private func markCached() {
        isRosterCurrent = false
        rosterIsCached = rosterSavedAt != nil
        conversationIsCached = snapshot != nil
        canLoadHistory = false
    }

    private func prepareCache() {
        guard let partition else { return }
        restoreCache()
        let token = revision, agentID = selectedAgentID
        let write = agentID.flatMap { cacheWrites[CacheWriteKey(partition: partition, agentID: $0)]?.1 }
        restoration = Task { [weak self, cache] in
            await cache.prepare()
            await write?.value
            guard let self, !Task.isCancelled, self.revision == token,
                  self.partition == partition, self.selectedAgentID == agentID else { return }
            self.restoreCache()
        }
    }

    private func prefetchBriefs() {
        guard prefetchBriefLimit > 0, case .array(let turns) = snapshot?.raw["turns"] else { return }
        var ids: [String] = []
        for turn in turns.reversed() {
            guard case .array(let values) = turn["brief_ids"] else { continue }
            for id in values.reversed().compactMap(\.readingString)
                where briefs[id] == nil && !ids.contains(id) && ids.count < prefetchBriefLimit {
                ids.append(id)
            }
            if ids.count == prefetchBriefLimit { break }
        }
        for id in ids {
            launch { [weak self] _ in await self?.loadBrief(id) }
        }
    }

    private func persistRoster() {
        guard let partition, let savedAt = rosterSavedAt else { return }
        var window = Array(agents.prefix(512))
        while !cache.save(.init(partition: partition, agentID: nil, agents: window, snapshot: nil,
                               position: nil, savedAt: savedAt)) {
            guard !window.isEmpty else { return }
            window.removeLast(max(1, window.count / 2))
        }
    }

    @discardableResult
    private func persist() -> Task<Void, Never>? {
        guard let partition, let selectedAgentID, let snapshot, let savedAt = conversationSavedAt else { return nil }
        let key = CacheWriteKey(partition: partition, agentID: selectedAgentID), id = UUID()
        cacheWrites[key]?.1.cancel()
        let position = readingPosition, bodies = briefs, order = briefRecency, liveWindow = reducer?.snapshot
        let work = Task.detached { ReadingSnapshotWindow.cacheCandidates(snapshot, liveWindow: liveWindow) }
        let task = Task { [weak self] in
            let candidates = await withTaskCancellationHandler { await work.value } onCancel: { work.cancel() }
            guard let self, !Task.isCancelled, self.cacheWrites[key]?.0 == id else { return }
            defer { self.cacheWrites.removeValue(forKey: key) }
            for raw in candidates {
                if self.cache.save(.init(partition: partition, agentID: selectedAgentID, agents: [],
                                         snapshot: raw, position: position, savedAt: savedAt,
                                         briefs: bodies, briefOrder: order)) { break }
            }
        }
        cacheWrites[key] = (id, task)
        return task
    }
}
