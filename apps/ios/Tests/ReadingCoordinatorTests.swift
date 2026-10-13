import Foundation
import HolonClient
import Observation
import XCTest
@testable import Holon

private func readingSnapshot(
    agent: String = "A", cursor: String = "live-1", epoch: String = "epoch",
    more: Bool = true, before: String? = "older-1", turns: [JSONValue] = []
) throws -> HolonConversationSnapshot {
    try HolonConversationSnapshot(raw: .object([
        "runtime_id": .string("runtime"), "visibility_scope_id": .string("private"),
        "agent_id": .string(agent), "event_log_epoch": .string(epoch),
        "snapshot_cursor": .string(cursor), "snapshot_through_seq": .integer(7),
        "event_head_seq": .integer(7), "schema_version": .integer(1), "query_version": .integer(1),
        "has_more": .bool(more), "next_before_cursor": before.map(JSONValue.string) ?? .null,
        "turns": .array(turns), "active_turns": .array([]), "pending_inputs": .array([])
    ]))
}

private func readingEvent(_ type: String, raw: JSONValue) throws -> HolonSSEEvent {
    let data = try JSONEncoder().encode(raw)
    let text = "event: \(type)\ndata: \(String(decoding: data, as: UTF8.self))\n\n"
    var parser = try HolonSSEParser()
    for byte in text.utf8 {
        if let event = try parser.append(byte) { return event }
    }
    throw HolonConversationError.malformedProtocol
}

private func readingReadState(
    agent: String = "A", epoch: String = "epoch", unread: Int64 = 1,
    head: Int64 = 7, through: Int64 = 5, revision: Int64 = 1
) -> JSONValue {
    .object([
        "agent_id": .string(agent), "event_log_epoch": .string(epoch),
        "visibility_scope_id": .string("private"), "unread_count": .integer(unread),
        "event_head_seq": .integer(head), "read_through_event_seq": .integer(through),
        "revision": .integer(revision), "reset_required": .bool(false),
        "retention_gap": .bool(false)
    ])
}

private func readingTurn(_ id: String, index: Int64, text: String? = nil,
                         execution: String? = nil) -> JSONValue {
    var fields: [String: JSONValue] = [
        "turn_id": .string(id),
        "key": .object(["turn_id": .string(id), "turn_index": .integer(index)]),
        "revision": .integer(1), "brief_ids": .array([.string("brief")])
    ]
    if let text { fields["summary"] = .string(text) }
    if let execution { fields["execution"] = .object(["kind": .string(execution)]) }
    return .object(fields)
}

/// Closing is synchronous, just like HolonEventStream.close(), even though the fake is an actor.
private final class ReadingFakeStream: @unchecked Sendable {
    let continuation: AsyncThrowingStream<HolonSSEEvent, any Error>.Continuation
    private let lock = NSLock()
    private var closed = false
    var isClosed: Bool { lock.withLock { closed } }
    init(_ continuation: AsyncThrowingStream<HolonSSEEvent, any Error>.Continuation) {
        self.continuation = continuation
    }
    func close() {
        lock.withLock { closed = true }
        continuation.finish()
    }
}

/// Intentionally ignores task cancellation at the gate, to model already-delivered replies.
private actor ReadingFakeTransport: ReadingTransport {
    enum Failure: Error { case offline, read }
    enum Gate: Equatable { case conversation(String), history, expansions }
    private let authority: HolonConnectionIdentity
    private var gate: Gate?
    private var blocked: CheckedContinuation<Void, Never>?
    private var pendingFailure: (any Error)?
    private var rosterFailure = false
    private var rosterError: (any Error)?
    private var rosterHeld = false
    private var rosterWaiters: [CheckedContinuation<Void, Never>] = []
    private var conversationFailure: (any Error)?
    private var liveTurns: [JSONValue]?
    private var historyTurns: [JSONValue] = []
    private var readFailure = false
    private var readResponse: JSONValue?
    private var readHeld = false
    private var readWaiters: [CheckedContinuation<Void, Never>] = []
    private var briefSequences: [String: Int64] = [:]
    private var liveCursor = "live-1"
    private var liveEpoch = "epoch"
    private var currentRunID: String?
    private var agentIDs = ["A", "B"]
    private var rosterAgents: [ReadingAgent]?
    private var previewResult: Result<ReadingOperatorPreview?, any Error>?
    private var previewErrors: [String: any Error] = [:]
    private var briefIDs = ["brief"]
    private var liveTurnRevision: Int64 = 1
    private var detailRevision: Int64 = 1
    private var pagedActivities = false
    private var activityFailure = false
    private var briefFailure = false
    private var briefEncodedSize: Int?
    private var activityRevision: Int64 = 1
    private var activityBody = "first"
    private var overlapOlderActivity = false
    private(set) var activityDetailCalls = 0
    private(set) var activityCursors: [String?] = []
    private var expansionWaiters: [CheckedContinuation<Void, Never>] = []
    private var activeExpansions = 0
    private var connections: [UUID: (String, ReadingFakeStream)] = [:]
    private(set) var rosterCalls = 0
    private(set) var conversationCalls = 0
    private(set) var historyCursors: [String] = []
    private(set) var openedAfter: [String] = []
    private(set) var maximumDetailStreams = 0
    private(set) var maximumRosterStreams = 0
    private(set) var readCalls = 0
    private(set) var readThroughs: [Int64] = []
    private(set) var closes = 0
    private(set) var maximumExpansions = 0
    private(set) var expansionCalls = 0
    private(set) var reportRequests: [HolonContentReportRequest] = []

    func createContentReport(_ request: HolonContentReportRequest) async throws -> HolonContentReportResponse {
        reportRequests.append(request)
        _ = await expansion("id", id: "report")
        return try JSONDecoder().decode(HolonContentReportResponse.self, from: Data(
            #"{"report_id":"report_1","status":"accepted","created_at":"2026-10-10T00:00:00Z"}"#.utf8))
    }

    init(authority: HolonConnectionIdentity) { self.authority = authority }
    func setGate(_ gate: Gate, failure: (any Error)? = nil) {
        self.gate = gate
        pendingFailure = failure
    }
    func setOffline(_ offline: Bool) { rosterFailure = offline }
    func setRosterError(_ error: (any Error)?) { rosterError = error }
    func setConversationFailure(_ error: (any Error)?) { conversationFailure = error }
    func setLiveTurns(_ turns: [JSONValue]) { liveTurns = turns }
    func setHistoryTurns(_ turns: [JSONValue]) { historyTurns = turns }
    func holdRoster() { rosterHeld = true }
    func releaseRoster() {
        rosterHeld = false
        for waiter in rosterWaiters { waiter.resume() }
        rosterWaiters.removeAll()
    }
    func failReads() { readFailure = true }
    func allowReads() { readFailure = false }
    func setReadResponse(_ response: JSONValue) { readResponse = response }
    func holdReads() { readHeld = true }
    func releaseReads() {
        readHeld = false
        for waiter in readWaiters { waiter.resume() }
        readWaiters.removeAll()
    }
    func setBriefSequence(_ id: String, sequence: Int64) { briefSequences[id] = sequence }
    func setBriefIDs(_ ids: [String]) {
        if ids != briefIDs { liveTurnRevision += 1 }
        briefIDs = ids
    }
    func setRunID(_ id: String?) { currentRunID = id }
    func setAgentIDs(_ ids: [String]) { agentIDs = ids }
    func setRosterAgents(_ agents: [ReadingAgent]) { rosterAgents = agents }
    func setPreviewResult(_ result: Result<ReadingOperatorPreview?, any Error>) { previewResult = result }
    func setPreviewError(agentID: String, error: any Error) { previewErrors[agentID] = error }
    func setRosterEpoch(_ epoch: String) { liveEpoch = epoch }
    func setDetailRevision(_ revision: Int64) { detailRevision = revision }
    func enablePagedActivities() { pagedActivities = true }
    func setActivityDetail(revision: Int64, body: String) { activityRevision = revision; activityBody = body }
    func overlapNextOlderActivityPage() { overlapOlderActivity = true }
    func failNextActivityPage() { activityFailure = true }
    func failNextBrief() { briefFailure = true }
    func setBriefEncodedSize(_ size: Int) { briefEncodedSize = size }
    func replaceEpoch() { liveEpoch = "replacement"; liveCursor = "replacement-cursor" }
    func isBlocked() -> Bool { blocked != nil }
    func release() { blocked?.resume(); blocked = nil }
    func releaseExpansions() {
        gate = nil
        for waiter in expansionWaiters { waiter.resume() }
        expansionWaiters.removeAll()
    }
    func counts() -> (Int, Int) {
        (connections.values.filter { $0.0 == "roster" && !$0.1.isClosed }.count,
         connections.values.filter { $0.0 != "roster" && !$0.1.isClosed }.count)
    }
    func roster() async throws -> ReadingRoster {
        rosterCalls += 1
        let epoch = liveEpoch
        let error = rosterError
        if rosterHeld { await withCheckedContinuation { rosterWaiters.append($0) } }
        if let error { throw error }
        if rosterFailure { throw Failure.offline }
        if let rosterAgents { return ReadingRoster(eventLogEpoch: epoch, agents: rosterAgents) }
        return ReadingRoster(eventLogEpoch: epoch, agents: agentIDs.map {
            ReadingAgent(id: $0, name: $0, preview: "recent", operatorPreview: "operator", unreadCount: 2,
                         currentRunID: currentRunID)
        })
    }
    func conversation(agentID: String, before: String?) async throws -> HolonConversationSnapshot {
        conversationCalls += 1
        let failure = pendingFailure
        let shouldBlock = (before != nil && gate == .history) || gate == .conversation(agentID)
        if shouldBlock {
            gate = nil
            pendingFailure = nil
            await withCheckedContinuation { blocked = $0 }
            if let failure { throw failure }
        }
        if rosterFailure { throw Failure.offline }
        if let conversationFailure { throw conversationFailure }
        if let before {
            historyCursors.append(before)
            return try readingSnapshot(agent: agentID, cursor: "history-not-live",
                                       more: false, before: nil, turns: historyTurns)
        }
        return try readingSnapshot(agent: agentID, cursor: liveCursor, epoch: liveEpoch, turns: liveTurns ?? [
            .object(["turn_id": .string("turn"),
                     "key": .object(["turn_id": .string("turn"), "turn_index": .integer(1)]),
                     "revision": .integer(liveTurnRevision), "brief_ids": .array(briefIDs.map(JSONValue.string))])
        ])
    }
    private func expansion(_ key: String, id: String) async -> JSONValue {
        expansionCalls += 1
        activeExpansions += 1
        maximumExpansions = max(maximumExpansions, activeExpansions)
        if gate == .expansions { await withCheckedContinuation { expansionWaiters.append($0) } }
        activeExpansions -= 1
        return .object([key: .string(id)])
    }
    func brief(agentID: String, briefID: String) async throws -> JSONValue {
        _ = await expansion("id", id: briefID)
        if briefFailure { briefFailure = false; throw Failure.offline }
        var fields: [String: JSONValue] = ["id": .string(briefID), "agent_id": .string(agentID),
                        "created_event_seq": .integer(briefSequences[briefID] ?? 5)]
        if let briefEncodedSize {
            fields["text"] = .string("")
            let overhead = try JSONEncoder().encode(JSONValue.object(fields)).count
            fields["text"] = .string(String(repeating: "a", count: max(0, briefEncodedSize - overhead)))
        }
        return .object(fields)
    }
    func activities(agentID: String, turnID: String) async throws -> JSONValue {
        try await activities(agentID: agentID, turnID: turnID, before: nil)
    }
    func activities(agentID: String, turnID: String, before: String?) async throws -> JSONValue {
        activityCursors.append(before)
        _ = await expansion("turn", id: turnID)
        if activityFailure { activityFailure = false; throw Failure.offline }
        let first = before == nil ? 60 : (overlapOlderActivity ? 1 : 0)
        let rows: [JSONValue] = pagedActivities ? (first..<(first + 60)).map { index in
            let id = "tool:\(index)"
            return .object(["id": .string(id), "kind": .string("tool"), "revision": .integer(activityRevision),
                            "summary": .string("command \(index)"),
                            "key": .object(["event_seq": .integer(Int64(index)), "activity_id": .string(id)])])
        } : []
        return .object(["turn_id": .string(turnID), "detail_revision": .integer(detailRevision),
                        "turn": .object(["turn_id": .string(turnID)]),
                        "runtime_id": .string("runtime"), "event_log_epoch": .string(liveEpoch),
                        "visibility_scope_id": .string("private"), "schema_version": .integer(1),
                        "query_version": .integer(1), "activities": .array(rows),
                        "has_more": .bool(pagedActivities && before == nil),
                        "next_before_cursor": pagedActivities && before == nil ? .string("older") : .null])
    }
    func operatorPreview(agentID: String) async throws -> ReadingOperatorPreview? {
        let result = previewResult
        let error = previewErrors[agentID]
        _ = await expansion("id", id: agentID)
        if let error { throw error }
        if let result { return try result.get() }
        return ReadingOperatorPreview(text: "new " + agentID, createdAt: Date(timeIntervalSince1970: 10))
    }
    func activityDetail(agentID: String, turnID: String, activity: ReadingActivity) async throws -> JSONValue {
        activityDetailCalls += 1
        let body = activityBody
        _ = await expansion("id", id: activity.id)
        return .object(["output": .string(body)])
    }
    func markRead(agentID: String, through: Int64) async throws -> JSONValue {
        readCalls += 1
        readThroughs.append(through)
        let failure = readFailure
        let response = readResponse ?? .object([
            "state": readingReadState(agent: agentID, epoch: liveEpoch, head: max(7, through),
                                      through: through, revision: Int64(readCalls)),
            "applied_read_through_event_seq": .integer(through)
        ])
        if readHeld { await withCheckedContinuation { readWaiters.append($0) } }
        if failure { throw Failure.read }
        return response
    }
    func stream(agentID: String?, after: String?) async throws -> ReadingStream {
        let id = UUID(), key = agentID ?? "roster"
        let (events, continuation) = AsyncThrowingStream<HolonSSEEvent, any Error>.makeStream()
        connections = connections.filter { !$0.value.1.isClosed }
        let opened = ReadingFakeStream(continuation)
        connections[id] = (key, opened)
        if let after { openedAfter.append(after) }
        let (rosters, details) = counts()
        maximumRosterStreams = max(maximumRosterStreams, rosters)
        maximumDetailStreams = max(maximumDetailStreams, details)
        return ReadingStream(events: events, close: { opened.close() })
    }
    func send(_ event: HolonSSEEvent, to agentID: String) {
        for value in connections.values where value.0 == agentID && !value.1.isClosed {
            value.1.continuation.yield(event)
        }
    }
    func overflow(agentID: String) {
        for value in connections.values where value.0 == agentID {
            value.1.continuation.finish(throwing: HolonClientError.streamLimitExceeded)
        }
    }
    func close() {
        closes += 1
        for value in connections.values { value.1.close() }
        connections.removeAll()
    }
}

@MainActor
final class ReadingCoordinatorTests: XCTestCase {
    private let url = URL(string: "https://reading.example/api")!
    private func identity(user: String = "user", network: String = "network", runtime: String = "runtime",
                          visibility: String = "private") -> HolonConnectionIdentity {
        HolonConnectionIdentity(networkID: network, runtimeID: runtime,
                                userID: user, visibilityScopeID: visibility)
    }
    private func wait(_ predicate: () async -> Bool) async throws {
        for _ in 0..<500 {
            if await predicate() { return }
            try await Task.sleep(nanoseconds: 10_000_000)
        }
        XCTFail("Timed out waiting for reading state")
    }
    private func seed(_ cache: ReadingCache, authority: HolonConnectionIdentity,
                      body: String = "cached body", agent: String = "A") throws -> ReadingPartition {
        let partition = try XCTUnwrap(ReadingPartition(apiBaseURL: url, identity: authority))
        let savedAt = Date().addingTimeInterval(-30 * 24 * 60 * 60)
        let snapshot = try readingSnapshot(agent: agent, more: false, before: nil,
                                          turns: [readingTurn("old", index: 0), readingTurn("turn", index: 1)])
        XCTAssertTrue(cache.save(.init(partition: partition, agentID: nil,
                                      agents: [ReadingAgent(id: agent, name: "cached", preview: "cached preview")],
                                      snapshot: nil, position: nil, savedAt: savedAt)))
        XCTAssertTrue(cache.save(.init(partition: partition, agentID: agent, agents: [],
                                      snapshot: snapshot.raw, position: "old", savedAt: savedAt,
                                      briefs: ["brief": .object(["id": .string("brief"),
                                                               "agent_id": .string(agent),
                                                               "text": .string(body),
                                                               "created_event_seq": .integer(5)])],
                                      briefOrder: ["brief"])))
        return partition
    }
    private func turnIDs(_ coordinator: ReadingCoordinator) -> [String] {
        guard case .array(let turns) = coordinator.snapshot?.raw["turns"] else { return [] }
        return turns.compactMap { $0["turn_id"]?.readingString }
    }
    private func start(cache: ReadingCache? = nil, turns: [JSONValue]? = nil) async throws
        -> (ReadingCoordinator, ReadingFakeTransport, HolonConnectionIdentity) {
        let authority = identity()
        let fake = ReadingFakeTransport(authority: authority)
        if let turns { await fake.setLiveTurns(turns) }
        let cache = cache ?? ReadingCache(file: nil)
        let coordinator = ReadingCoordinator(cache: cache, prefetchBriefLimit: 0)
        coordinator.activate(transport: fake, identity: authority, apiBaseURL: url)
        coordinator.selectAgent("A")
        let partition = try XCTUnwrap(ReadingPartition(apiBaseURL: url, identity: authority))
        try await wait {
            await fake.counts().1 == 1 && coordinator.status == .live &&
            cache.load(partition, agentID: "A") != nil
        }
        return (coordinator, fake, authority)
    }

    private func beginDetailBatch(_ fake: ReadingFakeTransport, sequence: Int64, detailRevision: Int64) async throws {
        let begin: JSONValue = .object([
            "type": .string("batch_begin"), "batch_id": .string("batch-\(sequence)"),
            "schema_version": .integer(1), "query_version": .integer(1),
            "from_seq": .integer(sequence - 1), "through_seq": .integer(sequence),
            "runtime_id": .string("runtime"), "event_log_epoch": .string("epoch"),
            "visibility_scope_id": .string("private")
        ])
        let summary: JSONValue = .object([
            "type": .string("turn_summary_upsert"), "batch_id": .string("batch-\(sequence)"),
            "turn": .object([
                "turn_id": .string("turn"),
                "key": .object(["turn_id": .string("turn"), "turn_index": .integer(1)]),
                "revision": .integer(1), "brief_ids": .array([.string("brief")])
            ])
        ])
        let invalidation: JSONValue = .object([
            "type": .string("detail_invalidated"), "batch_id": .string("batch-\(sequence)"),
            "turn_id": .string("turn"), "detail_revision": .integer(detailRevision)
        ])
        await fake.send(try readingEvent("batch_begin", raw: begin), to: "A")
        await fake.send(try readingEvent("turn_summary_upsert", raw: summary), to: "A")
        await fake.send(try readingEvent("detail_invalidated", raw: invalidation), to: "A")
    }

    private func commitDetailBatch(_ fake: ReadingFakeTransport, sequence: Int64) async throws {
        let checkpoint: JSONValue = .object([
            "type": .string("checkpoint"), "batch_id": .string("batch-\(sequence)"),
            "through_seq": .integer(sequence), "event_log_epoch": .string("epoch"),
            "visibility_scope_id": .string("private"), "checkpoint": .string("live-\(sequence)")
        ])
        await fake.send(try readingEvent("checkpoint", raw: checkpoint), to: "A")
    }

    func testBackgroundClosesAllStreamsAndForegroundBootstraps() async throws {
        let (coordinator, fake, _) = try await start()
        coordinator.setForeground(false)
        try await wait { await fake.counts() == (0, 0) }
        XCTAssertEqual(coordinator.status, .offline)
        let previous = await fake.rosterCalls
        await coordinator.loadHistory()
        await coordinator.markRead()
        let reads = await fake.readCalls
        XCTAssertEqual(reads, 0)
        coordinator.setForeground(true)
        try await wait { await fake.rosterCalls > previous && coordinator.status == .live }
        coordinator.disconnect()
    }

    func testRosterPreservesValidRunControlAndRejectsUnboundedIDs() async throws {
        let (coordinator, fake, _) = try await start()
        for (value, expected) in [("run-123", "run-123"), ("", nil),
                                  (String(repeating: "r", count: 513), nil)] {
            await fake.setRunID(value)
            await coordinator.refresh()
            try await wait { coordinator.status == .live }
            XCTAssertEqual(coordinator.agents.first?.currentRunID, expected)
        }
        coordinator.disconnect()
    }

    func testVisibleMetadataBudgetAndCancellationFence() async throws {
        let (coordinator, fake, _) = try await start()
        await fake.setGate(.expansions)
        let task = Task { await coordinator.loadVisiblePreviews(agentIDs: ["A", "B"]) }
        try await wait { await fake.expansionCalls == 2 }
        XCTAssertEqual(coordinator.agents.first?.operatorPreview, "operator", "Roster is already visible")
        coordinator.disconnect()
        await fake.releaseExpansions()
        await task.value
        XCTAssertTrue(coordinator.agents.isEmpty)
        let maximum = await fake.maximumExpansions
        XCTAssertEqual(maximum, 2)
    }

    func testExpandedRosterWindowEnrichesVisibleAgentsBeyondEighty() async throws {
        let (coordinator, fake, _) = try await start()
        await fake.setAgentIDs((0..<120).map { "A\($0)" })
        await coordinator.refresh()
        XCTAssertEqual(coordinator.agents.count, 120)
        await coordinator.loadVisiblePreviews(agentIDs: ["A0", "A1"])
        await coordinator.loadVisiblePreviews(agentIDs: ["A80", "A119"])
        XCTAssertEqual(coordinator.agents.first { $0.id == "A119" }?.operatorPreview, "new A119")
        XCTAssertEqual(coordinator.agents.first { $0.id == "A0" }?.operatorPreview, "new A0")
        let calls = await fake.expansionCalls
        await coordinator.loadVisiblePreviews(agentIDs: ["A0", "A119"])
        let unchanged = await fake.expansionCalls
        XCTAssertEqual(unchanged, calls, "A viewport revisit does not refetch confirmed metadata")
        coordinator.disconnect()
    }

    func testRefreshPreservesConfirmedInputAndOrderUntilSuccessfulReplacement() async throws {
        let (coordinator, fake, _) = try await start()
        let older = Date(timeIntervalSince1970: 1), newer = Date(timeIntervalSince1970: 5)
        await fake.setRosterAgents([
            ReadingAgent(id: "A", name: "A", preview: "brief A", briefAt: older),
            ReadingAgent(id: "B", name: "B", preview: "brief B", briefAt: newer)
        ])
        coordinator.selectAgent(nil)
        try await wait { coordinator.status == .live && !coordinator.isRefreshing }
        await coordinator.loadVisiblePreviews(agentIDs: ["A"])
        XCTAssertEqual(AgentSummaryPresentation.sorted(coordinator.agents, query: "").map(\.id), ["A", "B"])
        await fake.setRosterAgents([
            ReadingAgent(id: "A", name: "renamed", preview: "updated brief", briefAt: older),
            ReadingAgent(id: "B", name: "B", preview: "brief B", briefAt: newer)
        ])
        await coordinator.refresh()
        XCTAssertEqual(coordinator.agents.first?.name, "renamed")
        XCTAssertEqual(coordinator.agents.first?.preview, "updated brief", "Roster fields stay authoritative")
        XCTAssertEqual(AgentSummaryPresentation.preview(coordinator.agents[0]), "new A")
        XCTAssertEqual(AgentSummaryPresentation.sorted(coordinator.agents, query: "").map(\.id), ["A", "B"])
        await fake.setPreviewResult(.failure(ReadingFakeTransport.Failure.offline))
        await coordinator.loadVisiblePreviews(agentIDs: ["A"])
        XCTAssertEqual(AgentSummaryPresentation.preview(coordinator.agents[0]), "new A")
        XCTAssertEqual(coordinator.status, .live, "Optional metadata failure does not take the roster offline")
        await fake.setPreviewResult(.success(nil))
        await coordinator.loadVisiblePreviews(agentIDs: ["A"])
        XCTAssertNil(coordinator.agents.first?.operatorPreview, "Successful absence clears confirmed input")
        XCTAssertNil(coordinator.agents.first?.operatorAt)
        XCTAssertEqual(AgentSummaryPresentation.sorted(coordinator.agents, query: "").map(\.id), ["B", "A"])
        coordinator.disconnect()
    }

    func testRosterHintPreservesConfirmedInputAndOrder() async throws {
        let (coordinator, fake, _) = try await start()
        await fake.setRosterAgents([
            ReadingAgent(id: "A", name: "A", preview: "brief A", briefAt: Date(timeIntervalSince1970: 1)),
            ReadingAgent(id: "B", name: "B", preview: "brief B", briefAt: Date(timeIntervalSince1970: 5))
        ])
        coordinator.selectAgent(nil)
        try await wait { coordinator.status == .live && !coordinator.isRefreshing }
        await coordinator.loadVisiblePreviews(agentIDs: ["A"])
        let calls = await fake.rosterCalls
        await fake.send(try readingEvent("agent_roster_hint", raw: .object([:])), to: "roster")
        try await wait { await fake.rosterCalls > calls && !coordinator.isRefreshing }
        XCTAssertEqual(AgentSummaryPresentation.preview(coordinator.agents[0]), "new A")
        XCTAssertEqual(AgentSummaryPresentation.sorted(coordinator.agents, query: "").map(\.id), ["A", "B"])
        coordinator.disconnect()
    }

    func testNewEpochAndRemovedAgentDoNotInheritConfirmedInput() async throws {
        let (coordinator, fake, _) = try await start()
        let agent = ReadingAgent(id: "A", name: "A", preview: "brief")
        await fake.setRosterAgents([agent])
        coordinator.selectAgent(nil)
        try await wait { coordinator.status == .live && !coordinator.isRefreshing }
        await coordinator.loadVisiblePreviews(agentIDs: ["A"])
        XCTAssertEqual(coordinator.agents.first?.operatorPreview, "new A")
        await fake.setRosterEpoch("replacement")
        await coordinator.refresh()
        XCTAssertNil(coordinator.agents.first?.operatorPreview)
        XCTAssertNil(coordinator.agents.first?.operatorAt)
        await coordinator.loadVisiblePreviews(agentIDs: ["A"])
        await fake.setRosterAgents([])
        await coordinator.refresh()
        XCTAssertTrue(coordinator.agents.isEmpty)
        await fake.setRosterAgents([agent])
        await coordinator.refresh()
        XCTAssertNil(coordinator.agents.first?.operatorPreview, "A removed identity cannot reuse old metadata")
        coordinator.disconnect()
    }

    func testCachedInputOnlySurvivesMatchingConfirmedRosterEpoch() async throws {
        for cachedEpoch: String? in [nil, "epoch", "old"] {
            let authority = identity()
            let partition = try XCTUnwrap(ReadingPartition(apiBaseURL: url, identity: authority))
            let cache = ReadingCache(file: nil)
            let savedAt = Date(timeIntervalSinceNow: -100)
            XCTAssertTrue(cache.save(.init(partition: partition, agentID: nil,
                agents: [ReadingAgent(id: "A", name: "cached", preview: "brief",
                                      operatorPreview: "confirmed", operatorAt: savedAt)],
                snapshot: nil, position: nil, savedAt: savedAt, rosterEpoch: cachedEpoch)))
            let fake = ReadingFakeTransport(authority: authority)
            await fake.setRosterAgents([ReadingAgent(id: "A", name: "live", preview: "brief")])
            await fake.holdRoster()
            let coordinator = ReadingCoordinator(cache: cache, prefetchBriefLimit: 0)
            coordinator.activate(transport: fake, identity: authority, apiBaseURL: url)
            try await wait { await fake.rosterCalls > 0 && !coordinator.agents.isEmpty }
            XCTAssertEqual(coordinator.agents.first?.operatorPreview, "confirmed", "Offline cache stays readable")
            await fake.releaseRoster()
            try await wait { coordinator.status == .live && !coordinator.isRefreshing }
            XCTAssertEqual(coordinator.agents.first?.operatorPreview, cachedEpoch == "epoch" ? "confirmed" : nil)
            XCTAssertEqual(coordinator.agents.first?.operatorAt, cachedEpoch == "epoch" ? savedAt : nil)
            XCTAssertEqual(cache.load(partition, agentID: nil)?.rosterEpoch, "epoch")
            coordinator.disconnect()
        }
    }

    func testOnePreviewFailurePreservesInputAndContinuesVisibleWindow() async throws {
        let (coordinator, fake, _) = try await start()
        await fake.setRosterAgents(["A", "B", "C"].map { ReadingAgent(id: $0, name: $0, preview: "brief") })
        coordinator.selectAgent(nil)
        try await wait { coordinator.status == .live && !coordinator.isRefreshing }
        await coordinator.loadVisiblePreviews(agentIDs: ["A"])
        await fake.setPreviewError(agentID: "A", error: ReadingFakeTransport.Failure.offline)
        await coordinator.refresh()
        XCTAssertEqual(coordinator.agents.first?.operatorPreview, "new A", "Refresh preserves confirmed input")
        await fake.setGate(.expansions)
        let calls = await fake.expansionCalls
        let task = Task { await coordinator.loadVisiblePreviews(agentIDs: ["A", "B", "C"]) }
        try await wait { await fake.expansionCalls == calls + 2 }
        await fake.releaseExpansions()
        try await wait { await fake.expansionCalls == calls + 3 }
        await task.value
        XCTAssertEqual(coordinator.agents.map(\.operatorPreview), ["new A", "new B", "new C"])
        XCTAssertEqual(coordinator.status, .live)
        let maximum = await fake.maximumExpansions
        XCTAssertEqual(maximum, 2)
        coordinator.disconnect()
    }

    func testLatePreviewCannotCrossRosterEpoch() async throws {
        let (coordinator, fake, _) = try await start()
        await fake.setRosterAgents([ReadingAgent(id: "A", name: "A", preview: "brief")])
        coordinator.selectAgent(nil)
        try await wait { coordinator.status == .live && !coordinator.isRefreshing }
        await fake.setGate(.expansions)
        let calls = await fake.expansionCalls
        let delayed = Task { await coordinator.loadVisiblePreviews(agentIDs: ["A"]) }
        try await wait { await fake.expansionCalls > calls }
        await fake.setRosterEpoch("replacement")
        await coordinator.refresh()
        await fake.releaseExpansions()
        await delayed.value
        XCTAssertNil(coordinator.agents.first?.operatorPreview)
        XCTAssertNil(coordinator.agents.first?.operatorAt)
        await coordinator.loadVisiblePreviews(agentIDs: ["A"])
        XCTAssertEqual(coordinator.agents.first?.operatorPreview, "new A")
        coordinator.disconnect()
    }

    func testPreviewAuthorizationFailureInvalidatesConfirmedRoster() async throws {
        for code in [401, 403] {
            let cache = ReadingCache(file: nil)
            let (coordinator, fake, authority) = try await start(cache: cache)
            coordinator.selectAgent(nil)
            try await wait { coordinator.status == .live && !coordinator.isRefreshing }
            await fake.setPreviewResult(.failure(HolonHTTPFailure(statusCode: code, identity: authority)))
            await coordinator.loadVisiblePreviews(agentIDs: ["A"])
            XCTAssertEqual(coordinator.status, code == 401 ? .sessionExpired : .permissionDenied)
            XCTAssertTrue(coordinator.agents.isEmpty)
            let partition = try XCTUnwrap(ReadingPartition(apiBaseURL: url, identity: authority))
            XCTAssertNil(cache.load(partition, agentID: nil))
            coordinator.disconnect()
        }
    }

    func testAgentSwitchFencesLateSuccessAndAuthenticationFailures() async throws {
        for statusCode: Int? in [nil, 401, 403] {
            let authority = identity()
            let fake = ReadingFakeTransport(authority: authority)
            await fake.setGate(.conversation("A"), failure: statusCode.map {
                HolonHTTPFailure(statusCode: $0, identity: authority)
            })
            let coordinator = ReadingCoordinator(cache: ReadingCache(file: nil), prefetchBriefLimit: 0)
            var failures = 0
            coordinator.onConnectionFailure = { _ in failures += 1 }
            coordinator.activate(transport: fake, identity: authority, apiBaseURL: url)
            coordinator.selectAgent("A")
            try await wait { await fake.isBlocked() }
            coordinator.selectAgent("B")
            try await wait { coordinator.snapshot?.agentID == "B" }
            await fake.release()
            await Task.yield()
            XCTAssertEqual(coordinator.snapshot?.agentID, "B")
            XCTAssertEqual(coordinator.status, .live)
            XCTAssertEqual(failures, 0)
            coordinator.disconnect()
        }
    }

    func testOnlyOneSelectedConversationStream() async throws {
        let (coordinator, fake, _) = try await start()
        coordinator.selectAgent("B")
        try await wait { await fake.counts() == (1, 1) && coordinator.snapshot?.agentID == "B" }
        let detailMaximum = await fake.maximumDetailStreams
        let rosterMaximum = await fake.maximumRosterStreams
        XCTAssertEqual(detailMaximum, 1)
        XCTAssertEqual(rosterMaximum, 1)
        coordinator.selectAgent(nil)
        try await wait { await fake.counts() == (1, 0) }
        XCTAssertNil(coordinator.snapshot)
        coordinator.disconnect()
    }

    func testOfflineCacheIsPartitionedAndReadOnly() async throws {
        let cache = ReadingCache(file: nil)
        let (coordinator, _, authority) = try await start(cache: cache)
        coordinator.rememberPosition(turnID: "turn-7")
        let partition = try XCTUnwrap(ReadingPartition(apiBaseURL: url, identity: authority))
        try await wait { cache.load(partition, agentID: "A")?.position == "turn-7" }
        coordinator.disconnect()
        let offline = ReadingFakeTransport(authority: authority)
        await offline.setOffline(true)
        coordinator.activate(transport: offline, identity: authority, apiBaseURL: url)
        coordinator.selectAgent("A")
        XCTAssertEqual(coordinator.snapshot?.snapshotCursor, "live-1")
        XCTAssertEqual(coordinator.readingPosition, "turn-7")
        coordinator.setForeground(false)
        await coordinator.markRead()
        let reads = await offline.readCalls
        XCTAssertEqual(reads, 0)
        for changed in [identity(user: "other"), identity(network: "other"),
                        identity(runtime: "other"), identity(visibility: "other")] {
            coordinator.activate(transport: ReadingFakeTransport(authority: changed),
                                 identity: changed, apiBaseURL: url)
            coordinator.selectAgent("A")
            XCTAssertNil(coordinator.snapshot)
            XCTAssertTrue(coordinator.agents.isEmpty)
        }
        coordinator.activate(transport: ReadingFakeTransport(authority: authority), identity: authority,
                             apiBaseURL: URL(string: "https://another.example/api")!)
        coordinator.selectAgent("A")
        XCTAssertNil(coordinator.snapshot)
        let unconfirmed = HolonConnectionIdentity(networkID: "network")
        coordinator.activate(transport: ReadingFakeTransport(authority: unconfirmed),
                             identity: unconfirmed, apiBaseURL: url)
        XCTAssertEqual(coordinator.status, .incompatible)
        XCTAssertTrue(coordinator.agents.isEmpty)
        coordinator.disconnect()
    }

    func testHistoryCursorNeverBecomesLiveCursor() async throws {
        let (coordinator, fake, _) = try await start()
        await coordinator.loadHistory()
        XCTAssertEqual(coordinator.snapshot?.snapshotCursor, "live-1")
        XCTAssertFalse(coordinator.canLoadHistory)
        let cursors = await fake.historyCursors
        XCTAssertEqual(cursors, ["older-1"])
        coordinator.disconnect()
    }

    func testStaleHistoryCannotReplaceNewBootstrap() async throws {
        let (coordinator, fake, _) = try await start()
        await fake.setGate(.history)
        let history = Task { await coordinator.loadHistory() }
        try await wait { await fake.isBlocked() }
        await coordinator.refresh()
        await fake.release()
        await history.value
        XCTAssertEqual(coordinator.snapshot?.snapshotCursor, "live-1")
        XCTAssertTrue(coordinator.canLoadHistory)
        XCTAssertFalse(coordinator.isLoadingHistory)
        coordinator.disconnect()
    }

    func testHistoryCompletingAfterLiveCheckpointPreservesLiveCursor() async throws {
        let (coordinator, fake, _) = try await start()
        await coordinator.loadBrief("brief")
        let expanded = coordinator.briefs["brief"]
        await fake.setGate(.history)
        let history = Task { await coordinator.loadHistory() }
        try await wait { await fake.isBlocked() }
        let begin: JSONValue = .object([
            "type": .string("batch_begin"), "batch_id": .string("batch-8"),
            "schema_version": .integer(1), "query_version": .integer(1),
            "from_seq": .integer(7), "through_seq": .integer(8),
            "runtime_id": .string("runtime"), "event_log_epoch": .string("epoch"),
            "visibility_scope_id": .string("private")
        ])
        let checkpoint: JSONValue = .object([
            "type": .string("checkpoint"), "batch_id": .string("batch-8"),
            "through_seq": .integer(8), "event_log_epoch": .string("epoch"),
            "visibility_scope_id": .string("private"), "checkpoint": .string("live-8")
        ])
        await fake.send(try readingEvent("batch_begin", raw: begin), to: "A")
        await fake.send(try readingEvent("checkpoint", raw: checkpoint), to: "A")
        try await wait { coordinator.snapshot?.snapshotCursor == "live-8" }
        await fake.release()
        await history.value
        XCTAssertEqual(coordinator.snapshot?.snapshotCursor, "live-8")
        XCTAssertEqual(coordinator.snapshot?.raw["snapshot_through_seq"], .integer(8))
        XCTAssertEqual(coordinator.briefs["brief"], expanded)
        XCTAssertNotNil(expanded)
        XCTAssertFalse(coordinator.canLoadHistory)
        coordinator.disconnect()
    }

    func testEpochReplacementAndOverflowRequireNewBootstrap() async throws {
        for overflow in [false, true] {
            let (coordinator, fake, _) = try await start()
            await fake.replaceEpoch()
            if overflow { await fake.overflow(agentID: "A") }
            else {
                await fake.send(try readingEvent("reset_required", raw: .object([
                    "type": .string("reset_required")
                ])), to: "A")
            }
            try await wait { coordinator.snapshot?.eventLogEpoch == "replacement" }
            XCTAssertEqual(coordinator.snapshot?.snapshotCursor, "replacement-cursor")
            XCTAssertEqual(coordinator.status, .live)
            coordinator.disconnect()
        }
    }

    func testConnectionGenerationReplacementRejectsLate401() async throws {
        let old = identity()
        let fake = ReadingFakeTransport(authority: old)
        await fake.setGate(.conversation("A"), failure: HolonHTTPFailure(statusCode: 401, identity: old))
        let coordinator = ReadingCoordinator(cache: ReadingCache(file: nil), prefetchBriefLimit: 0)
        var failures = 0
        coordinator.onConnectionFailure = { _ in failures += 1 }
        coordinator.activate(transport: fake, identity: old, apiBaseURL: url)
        coordinator.selectAgent("A")
        try await wait { await fake.isBlocked() }
        let new = identity()
        XCTAssertNotEqual(old.generation, new.generation)
        coordinator.activate(transport: ReadingFakeTransport(authority: new), identity: new, apiBaseURL: url)
        coordinator.selectAgent("B")
        try await wait { coordinator.snapshot?.agentID == "B" }
        await fake.release()
        try await Task.sleep(nanoseconds: 20_000_000)
        XCTAssertEqual(coordinator.snapshot?.agentID, "B")
        XCTAssertEqual(coordinator.status, .live)
        XCTAssertEqual(failures, 0)
        coordinator.disconnect()
    }

    func testOnDemandExpansionsHaveSharedConcurrencyBound() async throws {
        let (coordinator, fake, _) = try await start()
        await fake.setGate(.expansions)
        let brief = Task { await coordinator.loadBrief("brief") }
        let activities = Task { await coordinator.loadActivities("turn") }
        try await wait { await fake.expansionCalls == 2 }
        XCTAssertEqual(coordinator.loadingBriefs, ["brief"])
        let third = Task { await coordinator.loadBrief("third") }
        let calls = await fake.expansionCalls
        XCTAssertEqual(calls, 2)
        await fake.releaseExpansions()
        await brief.value
        await activities.value
        await third.value
        XCTAssertNotNil(coordinator.briefs["third"])
        XCTAssertNotNil(coordinator.briefs["brief"])
        XCTAssertTrue(coordinator.loadingBriefs.isEmpty)
        XCTAssertTrue(coordinator.failedBriefs.isEmpty)
        XCTAssertNotNil(coordinator.activities["turn"])
        let maximum = await fake.maximumExpansions
        XCTAssertEqual(maximum, 2)
        coordinator.disconnect()
    }

    func testBriefFailureIsSeparateFromLoadingAndClearsOnExplicitRetry() async throws {
        let (coordinator, fake, _) = try await start()
        await fake.failNextBrief()
        await coordinator.loadBrief("brief")
        XCTAssertNil(coordinator.briefs["brief"])
        XCTAssertEqual(coordinator.failedBriefs, ["brief"])
        XCTAssertTrue(coordinator.loadingBriefs.isEmpty)
        XCTAssertEqual(coordinator.status, .live)
        await coordinator.loadBrief("brief")
        XCTAssertNotNil(coordinator.briefs["brief"])
        XCTAssertTrue(coordinator.failedBriefs.isEmpty)
        XCTAssertTrue(coordinator.loadingBriefs.isEmpty)
        coordinator.disconnect()
    }

    func testActivityReadWaitsForSharedBriefSlots() async throws {
        let (coordinator, fake, _) = try await start()
        await fake.setGate(.expansions)
        let first = Task { await coordinator.loadBrief("first") }
        let second = Task { await coordinator.loadBrief("second") }
        try await wait { await fake.expansionCalls == 2 }
        let activity = Task { await coordinator.loadActivities("turn") }
        try await wait { coordinator.loadingActivities.contains("turn") }
        let calls = await fake.expansionCalls
        XCTAssertEqual(calls, 2)
        await fake.releaseExpansions()
        await first.value; await second.value; await activity.value
        XCTAssertNotNil(coordinator.activities["turn"])
        XCTAssertTrue(coordinator.loadingActivities.isEmpty)
        let maximum = await fake.maximumExpansions
        XCTAssertEqual(maximum, 2)
        coordinator.disconnect()
    }

    func testCancelledQueuedActivityReadClearsLoading() async throws {
        let (coordinator, fake, _) = try await start()
        await fake.setGate(.expansions)
        let first = Task { await coordinator.loadBrief("first") }
        let second = Task { await coordinator.loadBrief("second") }
        try await wait { await fake.expansionCalls == 2 }
        let activity = Task { await coordinator.loadActivities("turn") }
        try await wait { coordinator.loadingActivities.contains("turn") }
        activity.cancel(); await activity.value
        XCTAssertTrue(coordinator.loadingActivities.isEmpty)
        XCTAssertNil(coordinator.activities["turn"])
        await fake.releaseExpansions(); await first.value; await second.value
        await coordinator.loadActivities("turn")
        XCTAssertNotNil(coordinator.activities["turn"])
        coordinator.disconnect()
    }

    func testNewActivityReaderTakesOverCancelledInlineRequest() async throws {
        let (coordinator, fake, _) = try await start()
        await fake.setGate(.expansions)
        let inline = Task { await coordinator.loadActivities("turn") }
        try await wait { await fake.expansionCalls == 1 }
        let fullScreen = Task { await coordinator.loadActivities("turn") }
        inline.cancel()
        await fake.releaseExpansions()
        await inline.value; await fullScreen.value
        XCTAssertNotNil(coordinator.activities["turn"])
        XCTAssertTrue(coordinator.loadingActivities.isEmpty)
        let calls = await fake.expansionCalls
        XCTAssertEqual(calls, 2)
        coordinator.disconnect()
    }

    func testActivityReaderKeyChangesAfterQuietForegroundBootstrap() async throws {
        let (coordinator, fake, _) = try await start()
        await coordinator.loadActivities("turn")
        let cursor = coordinator.snapshot?.snapshotCursor
        let loadedKey = coordinator.activityReadKey
        coordinator.setForeground(false)
        XCTAssertNotEqual(coordinator.activityReadKey, loadedKey)
        coordinator.setForeground(true)
        try await wait { coordinator.status == .live }
        XCTAssertEqual(coordinator.snapshot?.snapshotCursor, cursor)
        XCTAssertNil(coordinator.activities["turn"])
        XCTAssertNotEqual(coordinator.activityReadKey, loadedKey)
        await coordinator.loadActivities("turn")
        XCTAssertNotNil(coordinator.activities["turn"])
        let calls = await fake.expansionCalls
        XCTAssertEqual(calls, 2)
        coordinator.disconnect()
    }

    func testBriefSizeBoundaryEndsLoadingWithExplicitFailureAboveLimit() async throws {
        for bytes in [262_144, 262_145] {
            let (coordinator, fake, _) = try await start()
            await fake.setBriefEncodedSize(bytes)
            await coordinator.loadBrief("brief")
            XCTAssertTrue(coordinator.loadingBriefs.isEmpty)
            XCTAssertEqual(coordinator.status, .live)
            XCTAssertEqual(coordinator.briefs["brief"] != nil, bytes == 262_144)
            XCTAssertEqual(coordinator.failedBriefs.contains("brief"), bytes > 262_144)
            coordinator.disconnect()
        }
    }

    func testActivityPagesMergeInStableOrderAndKeepCursorOutOfLiveResume() async throws {
        let (coordinator, fake, _) = try await start()
        await fake.enablePagedActivities()
        await coordinator.loadActivities("turn")
        XCTAssertEqual(ActivityPresentation.items(coordinator.activities["turn"]).count, 60)
        await coordinator.loadOlderActivities("turn")
        let items = ActivityPresentation.items(coordinator.activities["turn"])
        XCTAssertEqual(items.count, 120)
        XCTAssertEqual(items.first?.sequence, 0); XCTAssertEqual(items.last?.sequence, 119)
        XCTAssertEqual(coordinator.activities["turn"]?["has_more"], .bool(false))
        XCTAssertEqual(coordinator.snapshot?.snapshotCursor, "live-1")
        let cursors = await fake.activityCursors
        XCTAssertEqual(cursors, [nil, "older"])
        await coordinator.reloadActivities("turn")
        XCTAssertEqual(ActivityPresentation.items(coordinator.activities["turn"]).count, 60)
        coordinator.disconnect()
    }

    func testActivityPagingFailureKeepsCurrentWindowAndReloadResetsCursor() async throws {
        let (coordinator, fake, _) = try await start()
        await fake.enablePagedActivities()
        await coordinator.loadActivities("turn")
        let original = coordinator.activities["turn"]
        await fake.failNextActivityPage()
        await coordinator.loadOlderActivities("turn")
        XCTAssertEqual(coordinator.activities["turn"], original)
        XCTAssertTrue(coordinator.failedActivities.contains("turn"))
        await coordinator.reloadActivities("turn")
        XCTAssertFalse(coordinator.failedActivities.contains("turn"))
        let cursors = await fake.activityCursors
        XCTAssertEqual(cursors, [nil, "older", nil])
        coordinator.disconnect()
    }

    func testExpandedActivityDetailReloadsEvenWhenServerRevisionsAreIdentical() async throws {
        let (coordinator, fake, _) = try await start()
        await fake.enablePagedActivities()
        await coordinator.loadActivities("turn")
        await coordinator.loadActivityDetail(turnID: "turn", activityID: "tool:60")
        XCTAssertEqual(coordinator.activityDetails["tool:60"]?["output"], .string("first"))
        await fake.setActivityDetail(revision: 1, body: "refreshed")
        let cacheRevision = coordinator.activityCacheRevision
        await coordinator.reloadActivities("turn")
        XCTAssertNil(coordinator.activityDetails["tool:60"])
        XCTAssertGreaterThan(coordinator.activityCacheRevision, cacheRevision)
        await coordinator.loadActivityDetail(turnID: "turn", activityID: "tool:60")
        XCTAssertEqual(coordinator.activityDetails["tool:60"]?["output"], .string("refreshed"))
        coordinator.disconnect()
    }

    func testDetailRevisionChangeDuringPagingInvalidatesExpandedOutput() async throws {
        let (coordinator, fake, _) = try await start()
        await fake.enablePagedActivities()
        await coordinator.loadActivities("turn")
        await coordinator.loadActivityDetail(turnID: "turn", activityID: "tool:60")
        await fake.setActivityDetail(revision: 2, body: "revised")
        // Same turn/detail revision; overlap merge may independently update an activity revision.
        await fake.overlapNextOlderActivityPage()
        await coordinator.loadOlderActivities("turn")
        await coordinator.loadActivityDetail(turnID: "turn", activityID: "tool:60")
        XCTAssertEqual(coordinator.activityDetails["tool:60"]?["output"], .string("revised"))
        coordinator.disconnect()
    }

    func testInFlightDetailCannotRepopulateCacheAfterExplicitReload() async throws {
        let (coordinator, fake, _) = try await start()
        await fake.enablePagedActivities()
        await coordinator.loadActivities("turn")
        await fake.setGate(.expansions)
        let detail = Task { await coordinator.loadActivityDetail(turnID: "turn", activityID: "tool:60") }
        try await wait { await fake.activityDetailCalls == 1 }
        let refresh = Task { await coordinator.reloadActivities("turn") }
        try await wait { await fake.expansionCalls == 3 }
        await fake.releaseExpansions()
        await detail.value; await refresh.value
        XCTAssertNil(coordinator.activityDetails["tool:60"])
        coordinator.disconnect()
    }

    func testCancelledHeldDetailAndPageReleaseSharedExpansionSlots() async throws {
        let (coordinator, fake, _) = try await start()
        await fake.enablePagedActivities()
        await coordinator.loadActivities("turn")
        await fake.setGate(.expansions)
        let detail = Task { await coordinator.loadActivityDetail(turnID: "turn", activityID: "tool:60") }
        let page = Task { await coordinator.loadOlderActivities("turn") }
        try await wait { await fake.expansionCalls == 3 }
        detail.cancel(); page.cancel()
        await fake.releaseExpansions()
        await detail.value; await page.value
        XCTAssertTrue(coordinator.loadingActivities.isEmpty)
        XCTAssertNil(coordinator.activityDetails["tool:60"])
        await coordinator.loadBrief("brief")
        XCTAssertNotNil(coordinator.briefs["brief"], "Cancelled requests must not exhaust the shared budget")
        coordinator.disconnect()
    }

    func testExpansionCancelledBeforeChildStartsReleasesReservation() async throws {
        let (coordinator, fake, _) = try await start()
        await fake.enablePagedActivities()
        await coordinator.loadActivities("turn")
        let detail = Task { await coordinator.loadActivityDetail(turnID: "turn", activityID: "tool:60") }
        let page = Task { await coordinator.loadOlderActivities("turn") }
        detail.cancel(); page.cancel()
        await detail.value; await page.value
        XCTAssertTrue(coordinator.loadingActivities.isEmpty)
        await coordinator.loadBrief("brief")
        XCTAssertNotNil(coordinator.briefs["brief"])
        coordinator.disconnect()
    }

    func testLateOlderActivityResponseCannotWriteIntoAnotherAgent() async throws {
        let (coordinator, fake, _) = try await start()
        await fake.enablePagedActivities()
        await coordinator.loadActivities("turn")
        await fake.setGate(.expansions)
        let older = Task { await coordinator.loadOlderActivities("turn") }
        try await wait { await fake.expansionCalls == 2 }
        coordinator.selectAgent("B")
        try await wait { coordinator.snapshot?.agentID == "B" }
        await fake.releaseExpansions(); await older.value
        XCTAssertTrue(coordinator.activities.isEmpty)
        XCTAssertEqual(coordinator.selectedAgentID, "B")
        coordinator.disconnect()
    }

    func testReadFailureDoesNotPolluteConversation() async throws {
        let (coordinator, fake, _) = try await start()
        await coordinator.loadBrief("brief")
        coordinator.setBriefVisible("brief", visible: true)
        await fake.failReads()
        await coordinator.markRead()
        XCTAssertEqual(coordinator.readStatus, .failed)
        XCTAssertEqual(coordinator.status, .live)
        XCTAssertEqual(coordinator.snapshot?.snapshotCursor, "live-1")
        coordinator.disconnect()
    }

    func testReadReceiptUpdatesRemainingUnreadWhenRosterFails() async throws {
        let cache = ReadingCache(file: nil)
        let (coordinator, fake, authority) = try await start(cache: cache)
        let partition = try XCTUnwrap(ReadingPartition(apiBaseURL: url, identity: authority))
        await coordinator.loadBrief("brief")
        coordinator.setBriefVisible("brief", visible: true)
        await fake.setReadResponse(.object([
            "state": readingReadState(), "applied_read_through_event_seq": .integer(5)
        ]))
        await fake.setRosterError(ReadingFakeTransport.Failure.offline)
        await coordinator.markRead()
        try await wait { coordinator.status == .offline }
        XCTAssertEqual(coordinator.readStatus, .confirmed)
        XCTAssertEqual(coordinator.agents.first?.unreadCount, 1)
        XCTAssertEqual(cache.load(partition, agentID: nil)?.agents.first?.unreadCount, 1)
        XCTAssertEqual(cache.load(partition, agentID: nil)?.agents.first?.readState?.readThroughEventSeq, 5)
        XCTAssertEqual(coordinator.agents.first?.operatorPreview, "operator")
        coordinator.disconnect()
    }

    func testInFlightRosterCannotUndoReadReceipt() async throws {
        let (coordinator, fake, _) = try await start()
        await coordinator.loadBrief("brief")
        coordinator.setBriefVisible("brief", visible: true)
        await fake.setReadResponse(.object([
            "state": readingReadState(), "applied_read_through_event_seq": .integer(5)
        ]))
        let calls = await fake.rosterCalls
        await fake.holdRoster()
        await fake.send(try readingEvent("agent_roster_hint", raw: .object([:])), to: "roster")
        try await wait { await fake.rosterCalls == calls + 1 }
        await coordinator.markRead()
        XCTAssertEqual(coordinator.agents.first?.unreadCount, 1)
        await fake.releaseRoster()
        try await wait { await fake.rosterCalls == calls + 2 && !coordinator.isRefreshing }
        XCTAssertEqual(coordinator.agents.first?.unreadCount, 1)
        XCTAssertEqual(coordinator.readStatus, .confirmed)
        coordinator.disconnect()
    }

    func testAutomaticReadRequiresLoadedVisibleReferencedBodyWithinHead() async throws {
        let (coordinator, fake, _) = try await start()
        coordinator.setBriefVisible("brief", visible: true)
        coordinator.markVisibleBriefsReadAutomatically()
        await coordinator.loadBrief("brief")
        coordinator.markVisibleBriefsReadAutomatically()
        await coordinator.loadBrief("unreferenced")
        coordinator.setBriefVisible("unreferenced", visible: true)
        coordinator.markVisibleBriefsReadAutomatically()
        let none = await fake.readCalls
        XCTAssertEqual(none, 0)

        coordinator.setBriefVisible("brief", visible: true)
        coordinator.markVisibleBriefsReadAutomatically()
        try await wait { coordinator.readStatus == .confirmed }
        for _ in 0..<3 {
            coordinator.setBriefVisible("brief", visible: false)
            coordinator.setBriefVisible("brief", visible: true)
            coordinator.markVisibleBriefsReadAutomatically()
        }
        let throughs = await fake.readThroughs
        XCTAssertEqual(throughs, [5])
        XCTAssertEqual(coordinator.agents.first?.unreadCount, 1)
        coordinator.disconnect()
    }

    func testAutomaticReadPersistsCursorDespiteFreshOrCachedBatchHead() async throws {
        for cached in [false, true] {
            let authority = identity()
            let state = try XCTUnwrap(ReadingReadState(
                readingReadState(unread: 0, through: 7, revision: 0),
                agentID: "A", epoch: "epoch", visibility: "private"))
            let agent = ReadingAgent(id: "A", name: "A", preview: "brief",
                                     unreadCount: 0, readState: state)
            let cache = ReadingCache(file: nil)
            let partition = try XCTUnwrap(ReadingPartition(apiBaseURL: url, identity: authority))
            if cached {
                XCTAssertTrue(cache.save(.init(partition: partition, agentID: nil,
                    agents: [agent], snapshot: nil, position: nil, savedAt: Date(), rosterEpoch: "epoch")))
            }
            let fake = ReadingFakeTransport(authority: authority)
            if !cached { await fake.setRosterAgents([agent]) }
            // The real server initializes a new cursor at head, even when the
            // first visible body is older. A batch GET alone never persists it.
            await fake.setReadResponse(.object([
                "state": readingReadState(unread: 0, through: 7, revision: 0),
                "applied_read_through_event_seq": .integer(7)
            ]))
            let coordinator = ReadingCoordinator(cache: cache, prefetchBriefLimit: 0)
            coordinator.activate(transport: fake, identity: authority, apiBaseURL: url)
            coordinator.selectAgent("A")
            try await wait { coordinator.status == .live && coordinator.agents.first?.readState == state }
            await coordinator.loadBrief("brief")
            coordinator.setBriefVisible("brief", visible: true)
            let request = coordinator.markVisibleBriefsReadAutomatically()
            XCTAssertNotNil(request, "Batch head is not a persisted receipt; cached=\(cached)")
            await request?.value
            XCTAssertEqual(coordinator.readStatus, .confirmed)
            XCTAssertNil(coordinator.markVisibleBriefsReadAutomatically())
            let throughs = await fake.readThroughs
            XCTAssertEqual(throughs, [5])
            XCTAssertEqual(cache.load(partition, agentID: nil)?.agents.first?.readState, state)
            coordinator.disconnect()
        }
    }

    func testFirstAutomaticReceiptPreservesBriefArrivingWhilePending() async throws {
        let (coordinator, fake, _) = try await start()
        let initial = try XCTUnwrap(ReadingReadState(
            readingReadState(unread: 0, through: 7, revision: 0),
            agentID: "A", epoch: "epoch", visibility: "private"))
        await fake.setRosterAgents([ReadingAgent(id: "A", name: "A", preview: "brief", readState: initial)])
        await coordinator.refresh()
        await coordinator.loadBrief("brief")
        coordinator.setBriefVisible("brief", visible: true)
        await fake.setReadResponse(.object([
            "state": readingReadState(unread: 0, through: 7, revision: 0),
            "applied_read_through_event_seq": .integer(7)
        ]))
        await fake.holdReads()
        let request = coordinator.markVisibleBriefsReadAutomatically()
        try await wait { await fake.readCalls == 1 }
        let later = try XCTUnwrap(ReadingReadState(
            readingReadState(unread: 1, head: 8, through: 7, revision: 0),
            agentID: "A", epoch: "epoch", visibility: "private"))
        await fake.setRosterAgents([ReadingAgent(id: "A", name: "A", preview: "new", readState: later)])
        let rosterCalls = await fake.rosterCalls
        await fake.send(try readingEvent("agent_roster_hint", raw: .object([:])), to: "roster")
        try await wait { await fake.rosterCalls > rosterCalls && coordinator.agents.first?.readState == later }
        await fake.releaseReads()
        await request?.value
        XCTAssertEqual(coordinator.readStatus, .confirmed)
        XCTAssertEqual(coordinator.agents.first?.unreadCount, 1)
        XCTAssertNil(coordinator.markVisibleBriefsReadAutomatically())
        let throughs = await fake.readThroughs
        XCTAssertEqual(throughs, [5])
        coordinator.disconnect()
    }

    func testAutomaticReadDoesNotRepeatSuccessfulManualReceipt() async throws {
        let (coordinator, fake, _) = try await start()
        await coordinator.loadBrief("brief")
        coordinator.setBriefVisible("brief", visible: true)
        await coordinator.markRead()
        XCTAssertEqual(coordinator.readStatus, .confirmed)
        XCTAssertNil(coordinator.markVisibleBriefsReadAutomatically())
        let calls = await fake.readCalls
        XCTAssertEqual(calls, 1)
        coordinator.disconnect()
    }

    func testAutomaticReadFailureDoesNotClearOrLoopAndAllowsManualRetry() async throws {
        let (coordinator, fake, _) = try await start()
        await fake.failReads()
        await coordinator.loadBrief("brief")
        coordinator.setBriefVisible("brief", visible: true)
        coordinator.markVisibleBriefsReadAutomatically()
        try await wait { coordinator.readStatus == .failed }
        XCTAssertEqual(coordinator.agents.first?.unreadCount, 2)
        XCTAssertNil(coordinator.agents.first?.readState)
        coordinator.markVisibleBriefsReadAutomatically()
        let failedCalls = await fake.readCalls
        XCTAssertEqual(failedCalls, 1)
        await fake.allowReads()
        await coordinator.markRead()
        XCTAssertEqual(coordinator.readStatus, .confirmed)
        let retried = await fake.readCalls
        XCTAssertEqual(retried, 2)
        coordinator.disconnect()
    }

    func testAutomaticReadCoalescesOnlyStillVisibleHigherPendingTargets() async throws {
        for hideNewTarget in [false, true] {
            let turns: [JSONValue] = [.object([
                "turn_id": .string("turn"), "key": .object(["turn_id": .string("turn"), "turn_index": .integer(1)]),
                "revision": .integer(1), "brief_ids": .array([.string("brief"), .string("next")])
            ])]
            let (coordinator, fake, _) = try await start(turns: turns)
            await fake.setBriefSequence("next", sequence: 6)
            await coordinator.loadBrief("brief")
            await coordinator.loadBrief("next")
            await fake.holdReads()
            coordinator.setBriefVisible("brief", visible: true)
            coordinator.markVisibleBriefsReadAutomatically()
            try await wait { await fake.readCalls == 1 }
            coordinator.setBriefVisible("next", visible: true)
            coordinator.markVisibleBriefsReadAutomatically()
            coordinator.markVisibleBriefsReadAutomatically()
            if hideNewTarget { coordinator.setBriefVisible("next", visible: false) }
            let pendingCalls = await fake.readCalls
            XCTAssertEqual(pendingCalls, 1)
            await fake.releaseReads()
            try await wait { coordinator.readStatus == .confirmed }
            let throughs = await fake.readThroughs
            XCTAssertEqual(throughs, hideNewTarget ? [5] : [5, 6])
            XCTAssertEqual(coordinator.agents.first?.unreadCount, 1)
            coordinator.disconnect()
        }
    }

    func testAutomaticReadDoesNotSendOfflineBackgroundOrFutureBody() async throws {
        for mode in ["offline", "background", "future"] {
            let (coordinator, fake, _) = try await start()
            if mode == "future" { await fake.setBriefSequence("brief", sequence: 8) }
            await coordinator.loadBrief("brief")
            coordinator.setBriefVisible("brief", visible: true)
            if mode == "background" { coordinator.setForeground(false) }
            if mode == "offline" {
                await fake.setOffline(true)
                await coordinator.refresh()
                try await wait { coordinator.status == .offline }
            }
            coordinator.markVisibleBriefsReadAutomatically()
            let calls = await fake.readCalls
            XCTAssertEqual(calls, 0, mode)
            XCTAssertEqual(coordinator.agents.first?.unreadCount, 2)
            coordinator.disconnect()
        }
    }

    func testLateReadResponseCannotOverwriteNewUnreadRosterState() async throws {
        let (coordinator, fake, _) = try await start()
        await coordinator.loadBrief("brief")
        coordinator.setBriefVisible("brief", visible: true)
        await fake.holdReads()
        let read = Task { await coordinator.markRead() }
        try await wait { await fake.readCalls == 1 }
        let newer = try XCTUnwrap(ReadingReadState(
            readingReadState(unread: 2, head: 8), agentID: "A", epoch: "epoch", visibility: "private"))
        await fake.setRosterAgents([ReadingAgent(id: "A", name: "A", preview: "new", readState: newer)])
        let rosterCalls = await fake.rosterCalls
        await fake.send(try readingEvent("agent_roster_hint", raw: .object([:])), to: "roster")
        try await wait { await fake.rosterCalls > rosterCalls && coordinator.agents.first?.readState == newer }
        await fake.releaseReads()
        await read.value
        XCTAssertEqual(coordinator.readStatus, .confirmed)
        XCTAssertEqual(coordinator.agents.first?.readState, newer)
        XCTAssertEqual(coordinator.agents.first?.unreadCount, 2)
        coordinator.disconnect()
    }

    func testReadReceiptCannotCrossAgentBackgroundOrPartition() async throws {
        for mode in ["agent", "background", "partition", "epoch"] {
            let (coordinator, fake, _) = try await start()
            await coordinator.loadBrief("brief")
            coordinator.setBriefVisible("brief", visible: true)
            await fake.holdReads()
            let read = coordinator.markVisibleBriefsReadAutomatically()
            try await wait { await fake.readCalls == 1 }
            if mode == "agent" {
                coordinator.selectAgent("B")
                try await wait { coordinator.snapshot?.agentID == "B" }
            } else if mode == "background" {
                coordinator.setForeground(false)
            } else if mode == "epoch" {
                await fake.replaceEpoch()
                await coordinator.refresh()
                try await wait { coordinator.snapshot?.eventLogEpoch == "replacement" }
            } else {
                let next = identity(user: "other")
                coordinator.activate(transport: ReadingFakeTransport(authority: next), identity: next, apiBaseURL: url)
                try await wait { coordinator.status == .live }
            }
            await fake.releaseReads()
            await read?.value
            XCTAssertNotEqual(coordinator.readStatus, .confirmed, mode)
            XCTAssertTrue(coordinator.agents.allSatisfy { $0.readState == nil }, mode)
            coordinator.disconnect()
        }
    }

    func testInvalidReadStateDoesNotApplyReceipt() async throws {
        let (coordinator, fake, _) = try await start()
        await coordinator.loadBrief("brief")
        coordinator.setBriefVisible("brief", visible: true)
        await fake.setReadResponse(.object([
            "state": readingReadState(unread: 0, head: 4),
            "applied_read_through_event_seq": .integer(5)
        ]))
        await coordinator.markRead()
        XCTAssertEqual(coordinator.readStatus, .failed)
        XCTAssertEqual(coordinator.agents.first?.unreadCount, 2)
        XCTAssertNil(coordinator.agents.first?.readState)
        coordinator.disconnect()
    }

    func testCachedReadStateSurvivesSameEpochButNotReplacement() async throws {
        let cache = ReadingCache(file: nil)
        let (coordinator, _, authority) = try await start(cache: cache)
        await coordinator.loadBrief("brief")
        coordinator.setBriefVisible("brief", visible: true)
        await coordinator.markRead()
        coordinator.disconnect()

        let fake = ReadingFakeTransport(authority: authority)
        let restored = ReadingCoordinator(cache: cache, prefetchBriefLimit: 0)
        restored.activate(transport: fake, identity: authority, apiBaseURL: url)
        try await wait { restored.status == .live }
        XCTAssertEqual(restored.agents.first?.unreadCount, 1)
        XCTAssertEqual(restored.agents.first?.readState?.readThroughEventSeq, 5)
        await fake.replaceEpoch()
        await restored.refresh()
        XCTAssertEqual(restored.agents.first?.unreadCount, 2)
        XCTAssertNil(restored.agents.first?.readState)
        restored.disconnect()
    }

    func testReadRequiresServerConfirmation() async throws {
        let (coordinator, _, _) = try await start()
        XCTAssertEqual(coordinator.readStatus, .idle)
        coordinator.rememberPosition(turnID: "turn")
        XCTAssertEqual(coordinator.readStatus, .idle)
        await coordinator.markRead()
        XCTAssertEqual(coordinator.readStatus, .idle)
        await coordinator.loadBrief("brief")
        coordinator.setBriefVisible("brief", visible: true)
        await coordinator.markRead()
        XCTAssertEqual(coordinator.readStatus, .confirmed)
        coordinator.disconnect()
    }

    func testReadRequiresVisibleLoadedAndReferencedBrief() async throws {
        let (coordinator, fake, _) = try await start()
        coordinator.setBriefVisible("brief", visible: true)
        XCTAssertNil(coordinator.readConfirmationForVisibleBriefs)
        await coordinator.loadBrief("unreferenced")
        coordinator.setBriefVisible("unreferenced", visible: true)
        XCTAssertNil(coordinator.readConfirmationForVisibleBriefs)
        await coordinator.loadBrief("brief")
        XCTAssertNil(coordinator.readConfirmationForVisibleBriefs)
        coordinator.setBriefVisible("brief", visible: true)
        XCTAssertEqual(coordinator.readConfirmationForVisibleBriefs?.through, 5)
        let confirmation = coordinator.readConfirmationForVisibleBriefs
        coordinator.setBriefVisible("brief", visible: false)
        await coordinator.markRead(confirmation: confirmation)
        XCTAssertEqual(coordinator.readStatus, .idle)
        let reads = await fake.readCalls
        XCTAssertEqual(reads, 0)
        coordinator.disconnect()
    }

    func testReadConfirmationCannotCrossAgentOrGeneration() async throws {
        let (coordinator, fake, _) = try await start()
        XCTAssertEqual(coordinator.agents.first?.operatorPreview, "operator")
        XCTAssertEqual(coordinator.agents.first?.unreadCount, 2)
        await coordinator.loadBrief("brief")
        coordinator.setBriefVisible("brief", visible: true)
        let confirmation = try XCTUnwrap(coordinator.readConfirmationForVisibleBriefs)
        coordinator.selectAgent("B")
        try await wait { coordinator.snapshot?.agentID == "B" && coordinator.status == .live }
        await coordinator.loadBrief("brief")
        coordinator.setBriefVisible("brief", visible: true)
        await coordinator.markRead(confirmation: confirmation)
        XCTAssertEqual(coordinator.readStatus, .idle)
        let newConfirmation = try XCTUnwrap(coordinator.readConfirmationForVisibleBriefs)
        await coordinator.refresh()
        await coordinator.loadBrief("brief")
        coordinator.setBriefVisible("brief", visible: true)
        await coordinator.markRead(confirmation: newConfirmation)
        XCTAssertEqual(coordinator.readStatus, .idle)
        let reads = await fake.readCalls
        XCTAssertEqual(reads, 0)
        coordinator.disconnect()
    }

    func testHiddenReadConfirmationCannotReviveWhenBriefIsVisibleAgain() async throws {
        let (coordinator, fake, _) = try await start()
        await coordinator.loadBrief("brief")
        coordinator.setBriefVisible("brief", visible: true)
        let original = try XCTUnwrap(coordinator.readConfirmationForVisibleBriefs)
        coordinator.setBriefVisible("brief", visible: true)
        XCTAssertEqual(coordinator.readConfirmationForVisibleBriefs, original)
        coordinator.setBriefVisible("brief", visible: false)
        coordinator.setBriefVisible("brief", visible: true)
        let renewed = try XCTUnwrap(coordinator.readConfirmationForVisibleBriefs)
        XCTAssertEqual(renewed.through, original.through)
        XCTAssertNotEqual(renewed, original)
        await coordinator.markRead(confirmation: original)
        let oldReads = await fake.readCalls
        XCTAssertEqual(oldReads, 0)
        XCTAssertEqual(coordinator.readStatus, .idle)
        XCTAssertEqual(coordinator.status, .live)
        let streams = await fake.counts()
        XCTAssertEqual(streams.0, 1)
        XCTAssertEqual(streams.1, 1)
        await coordinator.markRead(confirmation: renewed)
        let newReads = await fake.readCalls
        XCTAssertEqual(newReads, 1)
        XCTAssertEqual(coordinator.readStatus, .confirmed)
        coordinator.disconnect()
    }

    func testBriefCacheEvictsHiddenEntriesAndKeepsVisibleBrief() async throws {
        let (coordinator, fake, _) = try await start()
        await coordinator.loadBrief("brief")
        coordinator.setBriefVisible("brief", visible: true)
        let confirmation = try XCTUnwrap(coordinator.readConfirmationForVisibleBriefs)
        for index in 0..<79 { await coordinator.loadBrief("cached-\(index)") }
        XCTAssertEqual(coordinator.briefs.count, 80)
        await coordinator.loadBrief("cached-79")
        XCTAssertEqual(coordinator.briefs.count, 80)
        XCTAssertNil(coordinator.briefs["cached-0"])
        XCTAssertNotNil(coordinator.briefs["cached-79"])
        XCTAssertNotNil(coordinator.briefs["brief"])
        XCTAssertEqual(coordinator.readConfirmationForVisibleBriefs, confirmation)
        await coordinator.loadBrief("cached-0")
        XCTAssertEqual(coordinator.briefs.count, 80)
        XCTAssertNotNil(coordinator.briefs["cached-0"])
        XCTAssertNil(coordinator.briefs["cached-1"])
        let calls = await fake.expansionCalls
        XCTAssertEqual(calls, 82)
        coordinator.disconnect()
    }

    func testDetailRevisionInvalidatesOnlyAtCommitWithoutSummaryRevisionChange() async throws {
        let (coordinator, fake, _) = try await start()
        await coordinator.loadActivities("turn")
        let original = try XCTUnwrap(coordinator.activities["turn"])
        try await beginDetailBatch(fake, sequence: 8, detailRevision: 2)
        // Give the stream owner a chance to consume the uncommitted mutation frames.
        try await Task.sleep(nanoseconds: 30_000_000)
        XCTAssertEqual(coordinator.activities["turn"], original)
        XCTAssertEqual(coordinator.snapshot?.snapshotCursor, "live-1")
        try await commitDetailBatch(fake, sequence: 8)
        try await wait { coordinator.snapshot?.snapshotCursor == "live-8" }
        XCTAssertNil(coordinator.activities["turn"])
        if case .array(let turns) = coordinator.snapshot?.raw["turns"] {
            XCTAssertEqual(turns.first?["revision"], .integer(1))
        } else { XCTFail("Missing committed turns") }
        await fake.setDetailRevision(2)
        await coordinator.loadActivities("turn")
        let refreshed = try XCTUnwrap(coordinator.activities["turn"])
        XCTAssertEqual(refreshed["detail_revision"], .integer(2))
        for (sequence, revision) in [(Int64(9), Int64(2)), (10, 1)] {
            try await beginDetailBatch(fake, sequence: sequence, detailRevision: revision)
            try await commitDetailBatch(fake, sequence: sequence)
            try await wait { coordinator.snapshot?.snapshotCursor == "live-\(sequence)" }
            XCTAssertEqual(coordinator.activities["turn"], refreshed)
        }
        let calls = await fake.expansionCalls
        XCTAssertEqual(calls, 2)
        let rosterCalls = await fake.rosterCalls
        XCTAssertEqual(rosterCalls, 1)
        coordinator.disconnect()
    }

    func testDetailResponseCannotRepopulateCacheAfterCommittedInvalidation() async throws {
        let (coordinator, fake, _) = try await start()
        await fake.setGate(.expansions)
        let expansion = Task { await coordinator.loadActivities("turn") }
        try await wait { await fake.expansionCalls == 1 }
        try await beginDetailBatch(fake, sequence: 8, detailRevision: 2)
        try await commitDetailBatch(fake, sequence: 8)
        try await wait { coordinator.snapshot?.snapshotCursor == "live-8" }
        await fake.releaseExpansions()
        await expansion.value
        XCTAssertNil(coordinator.activities["turn"])
        await fake.setDetailRevision(2)
        await coordinator.loadActivities("turn")
        XCTAssertEqual(coordinator.activities["turn"]?["detail_revision"], .integer(2))
        coordinator.disconnect()
    }

    func testDetailCacheHonorsAlreadyFetchedRevisionAndIsClearedAcrossBootstrap() async throws {
        let (coordinator, fake, _) = try await start()
        await fake.setDetailRevision(3)
        await coordinator.loadActivities("turn")
        let fresh = try XCTUnwrap(coordinator.activities["turn"])
        try await beginDetailBatch(fake, sequence: 8, detailRevision: 2)
        try await commitDetailBatch(fake, sequence: 8)
        try await wait { coordinator.snapshot?.snapshotCursor == "live-8" }
        XCTAssertEqual(coordinator.activities["turn"], fresh)
        await coordinator.refresh()
        XCTAssertNil(coordinator.activities["turn"])
        await coordinator.loadActivities("turn")
        XCTAssertEqual(coordinator.activities["turn"], fresh)
        let calls = await fake.expansionCalls
        XCTAssertEqual(calls, 2)
        coordinator.disconnect()
    }

    func testBriefCacheTouchesExistingEntriesAndBoundsConcurrentInsertions() async throws {
        let (coordinator, fake, _) = try await start()
        for index in 0..<80 { await coordinator.loadBrief("cached-\(index)") }
        await coordinator.loadBrief("cached-0")
        let callsBefore = await fake.expansionCalls
        XCTAssertEqual(callsBefore, 80)
        await fake.setGate(.expansions)
        let first = Task { await coordinator.loadBrief("cached-80") }
        let second = Task { await coordinator.loadBrief("cached-81") }
        try await wait { await fake.expansionCalls == 82 }
        await fake.releaseExpansions()
        await first.value
        await second.value
        XCTAssertEqual(coordinator.briefs.count, 80)
        XCTAssertNotNil(coordinator.briefs["cached-0"])
        XCTAssertNotNil(coordinator.briefs["cached-80"])
        XCTAssertNotNil(coordinator.briefs["cached-81"])
        XCTAssertNil(coordinator.briefs["cached-1"])
        XCTAssertNil(coordinator.briefs["cached-2"])
        coordinator.disconnect()
    }

    func testFullVisibleBriefCacheAllowsLoadingAfterAnEntryIsHidden() async throws {
        let (coordinator, fake, _) = try await start()
        let ids = (0..<81).map { "brief-\($0)" }
        await fake.setBriefIDs(ids)
        await coordinator.refresh()
        for id in ids.prefix(80) {
            await coordinator.loadBrief(id)
            coordinator.setBriefVisible(id, visible: true)
        }
        XCTAssertEqual(coordinator.visibleBriefIDs.count, 80)
        await coordinator.loadBrief(ids[80])
        XCTAssertEqual(coordinator.briefs.count, 80)
        XCTAssertNil(coordinator.briefs[ids[80]])
        XCTAssertTrue(ids.prefix(80).allSatisfy { coordinator.briefs[$0] != nil })
        coordinator.setBriefVisible(ids[0], visible: false)
        await coordinator.loadBrief(ids[80])
        XCTAssertEqual(coordinator.briefs.count, 80)
        XCTAssertNotNil(coordinator.briefs[ids[80]])
        XCTAssertNil(coordinator.briefs[ids[0]])
        coordinator.disconnect()
    }

    func testUnknownControlBootstrapsAndRecoveryIsBounded() async throws {
        let (coordinator, fake, _) = try await start()
        let previous = await fake.rosterCalls
        let unknown = try readingEvent("new_control", raw: .object(["type": .string("new_control")]))
        await fake.setOffline(true)
        await fake.send(unknown, to: "A")
        try await wait { await fake.rosterCalls == previous + 3 }
        // No unbounded reconnect loop after the three explicit delayed recoveries.
        try await Task.sleep(nanoseconds: 800_000_000)
        let calls = await fake.rosterCalls
        XCTAssertEqual(calls, previous + 3)
        XCTAssertEqual(coordinator.status, .offline)
        try await wait { await fake.counts() == (0, 0) }
        coordinator.disconnect()
    }

    private func reportTarget(messageID: String = "transcript_1") throws -> ContentReportTarget {
        let activity = try ReadingActivity(.object([
            "id": .string("assistant:\(messageID)"), "kind": .string("assistant"),
            "revision": .integer(1), "summary": .string("preview"),
            "key": .object(["event_seq": .integer(1), "activity_id": .string("assistant:\(messageID)")])
        ]))
        let detail: JSONValue = .object(["id": .string(messageID), "kind": .string("assistant_round"),
                                        "data": .object(["text": .string("full response")])])
        return try XCTUnwrap(ContentReportTarget(turnID: "turn", activity: activity, detail: detail))
    }

    private func reportDraft(_ coordinator: ReadingCoordinator, messageID: String = "transcript_1") throws -> ContentReportDraft {
        let scope = try XCTUnwrap(coordinator.contentReportScope)
        let target = try reportTarget(messageID: messageID)
        return try XCTUnwrap(coordinator.contentReportDraft(target: target, scope: scope))
    }

    func testReportDraftSurvivesReopeningAndUsesOriginalScope() async throws {
        let (coordinator, fake, _) = try await start()
        let draft = try reportDraft(coordinator)
        XCTAssertTrue(try reportDraft(coordinator) === draft)
        draft.category = .privacy
        await draft.submit { try await coordinator.createContentReport($0, scope: $1) }
        XCTAssertEqual(draft.receipt?.reportID, "report_1")
        let requests = await fake.reportRequests
        XCTAssertEqual(requests.count, 1)
        XCTAssertEqual(requests.first?.agentID, "A")
        XCTAssertTrue(try reportDraft(coordinator) === draft)
        coordinator.disconnect()
    }

    func testAcceptedReportScopeNotifiesObserversAfterForegroundRestoration() async throws {
        let (coordinator, fake, _) = try await start()
        defer { coordinator.disconnect() }
        let draft = try reportDraft(coordinator)
        draft.category = .privacy
        await draft.submit { try await coordinator.createContentReport($0, scope: $1) }
        XCTAssertEqual(draft.receipt?.reportID, "report_1")
        coordinator.setForeground(false)
        XCTAssertNil(coordinator.contentReportScope)

        // A report sheet renders while inactive and must observe its return.
        let scopeChanged = expectation(description: "Report scope observation resumes")
        withObservationTracking {
            _ = coordinator.contentReportScope
        } onChange: {
            scopeChanged.fulfill()
        }
        coordinator.setForeground(true)
        await fulfillment(of: [scopeChanged], timeout: 1)
        try await wait { coordinator.status == .live }
        XCTAssertEqual(coordinator.contentReportScope, draft.scope)
        let restored = try reportDraft(coordinator)
        XCTAssertTrue(restored === draft)
        XCTAssertFalse(restored.canSubmit)
        await restored.submit { try await coordinator.createContentReport($0, scope: $1) }
        let requests = await fake.reportRequests
        XCTAssertEqual(requests.count, 1, "Foreground restoration must not resend an accepted report")
    }

    func testAttemptedReportDraftsCannotBeEvictedAtCapacity() async throws {
        let (coordinator, fake, _) = try await start()
        var drafts: [ContentReportDraft] = []
        for index in 0..<20 {
            let draft = try reportDraft(coordinator, messageID: "report_\(index)")
            draft.category = .privacy
            if index.isMultiple(of: 2) {
                await draft.submit { try await coordinator.createContentReport($0, scope: $1) }
                XCTAssertNotNil(draft.receipt)
            } else {
                await draft.submit { _, _ in throw URLError(.timedOut) }
                XCTAssertEqual(draft.errorKey, "report.unknownOutcome")
            }
            XCTAssertNotNil(draft.request)
            drafts.append(draft)
        }
        let scope = try XCTUnwrap(coordinator.contentReportScope)
        let extra = try reportTarget(messageID: "report_extra")
        XCTAssertNil(coordinator.contentReportDraft(target: extra, scope: scope))
        for (index, original) in drafts.enumerated() {
            let reopened = try reportDraft(coordinator, messageID: "report_\(index)")
            XCTAssertTrue(reopened === original)
            XCTAssertEqual(reopened.request, original.request)
            XCTAssertEqual(reopened.receipt?.reportID, original.receipt?.reportID)
            if index.isMultiple(of: 2) {
                XCTAssertFalse(reopened.canSubmit)
                await reopened.submit { try await coordinator.createContentReport($0, scope: $1) }
            }
        }
        let requests = await fake.reportRequests
        XCTAssertEqual(requests.count, 10)
        coordinator.disconnect()
    }

    func testUnattemptedReportDraftsCanBeEvictedWithoutLosingReceipt() async throws {
        let (coordinator, fake, _) = try await start()
        let accepted = try reportDraft(coordinator)
        accepted.category = .privacy
        await accepted.submit { try await coordinator.createContentReport($0, scope: $1) }
        XCTAssertNotNil(accepted.receipt)
        for index in 0..<40 {
            let unattempted = try reportDraft(coordinator, messageID: "unattempted_\(index)")
            XCTAssertNil(unattempted.request)
            XCTAssertTrue(try reportDraft(coordinator) === accepted)
        }
        let reopened = try reportDraft(coordinator)
        XCTAssertEqual(reopened.receipt?.reportID, "report_1")
        XCTAssertFalse(reopened.canSubmit)
        await reopened.submit { try await coordinator.createContentReport($0, scope: $1) }
        let requests = await fake.reportRequests
        XCTAssertEqual(requests.count, 1)
        coordinator.disconnect()
    }

    func testOldReportScopeCannotSendAfterAgentOrConnectionChange() async throws {
        let (coordinator, fake, authority) = try await start()
        let draft = try reportDraft(coordinator); draft.category = .privacy
        coordinator.selectAgent("B")
        await draft.submit { try await coordinator.createContentReport($0, scope: $1) }
        XCTAssertEqual(draft.errorKey, "report.contextChanged")
        let firstRequests = await fake.reportRequests
        XCTAssertTrue(firstRequests.isEmpty)
        try await wait { coordinator.status == .live }
        let other = try reportDraft(coordinator); other.category = .privacy
        coordinator.activate(transport: fake, identity: authority, apiBaseURL: URL(string: "https://other.example/api")!)
        await other.submit { try await coordinator.createContentReport($0, scope: $1) }
        XCTAssertEqual(other.errorKey, "report.contextChanged")
        let requests = await fake.reportRequests
        XCTAssertTrue(requests.isEmpty)
        coordinator.disconnect()
    }

    func testLateReportReceiptAfterDisconnectIsNotPresentedAsSuccess() async throws {
        let (coordinator, fake, _) = try await start()
        let draft = try reportDraft(coordinator); draft.category = .privacy
        await fake.setGate(.expansions)
        let task = Task { await draft.submit { try await coordinator.createContentReport($0, scope: $1) } }
        try await wait { await fake.reportRequests.count == 1 }
        coordinator.disconnect()
        await fake.releaseExpansions()
        await task.value
        XCTAssertNil(draft.receipt)
        XCTAssertEqual(draft.errorKey, "report.contextChanged")
    }

    func testColdOfflineRestoresHistoryBodiesAndPositionAcrossRestart() async throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: directory) }
        let file = directory.appendingPathComponent("reading.json")
        let saved = ReadingCache(file: file)
        let partition = try seed(saved, authority: identity())
        let flushed = await saved.flush()
        XCTAssertTrue(flushed)

        let restored = ReadingCache(file: file)
        let coordinator = ReadingCoordinator(cache: restored, prefetchBriefLimit: 0)
        coordinator.activateOffline(partition: partition)
        coordinator.selectAgent("A")
        try await wait { coordinator.snapshot != nil }
        XCTAssertFalse(coordinator.isRosterCurrent)
        XCTAssertEqual(coordinator.agents.first?.name, "cached")
        XCTAssertEqual(turnIDs(coordinator), ["old", "turn"])
        XCTAssertEqual(coordinator.briefs["brief"]?["text"], .string("cached body"))
        XCTAssertEqual(coordinator.readingPosition, "old")
        XCTAssertTrue(coordinator.isShowingCachedContent)
        XCTAssertFalse(coordinator.isRefreshing)
        XCTAssertFalse(coordinator.canLoadHistory)
        XCTAssertNil(coordinator.contentReportScope)
        coordinator.setBriefVisible("brief", visible: true)
        XCTAssertNil(coordinator.readConfirmationForVisibleBriefs)

        let snapshot = coordinator.snapshot
        coordinator.activateOffline(partition: partition)
        XCTAssertEqual(coordinator.snapshot, snapshot)
        coordinator.setForeground(false)
        coordinator.setForeground(true)
        try await wait { coordinator.snapshot == snapshot }
        XCTAssertFalse(coordinator.isRosterCurrent)
        XCTAssertEqual(coordinator.briefs["brief"]?["text"], .string("cached body"))
        let savedAt = try XCTUnwrap(restored.load(partition, agentID: "A")?.savedAt)
        XCTAssertLessThan(savedAt, Date().addingTimeInterval(-29 * 24 * 60 * 60))
        let restoredFlush = await restored.flush()
        XCTAssertTrue(restoredFlush)
        coordinator.disconnect()

        let secondCache = ReadingCache(file: file)
        let second = ReadingCoordinator(cache: secondCache, prefetchBriefLimit: 0)
        second.activateOffline(partition: partition)
        second.selectAgent("A")
        try await wait { second.briefs["brief"] != nil }
        XCTAssertEqual(turnIDs(second), ["old", "turn"])
        XCTAssertEqual(second.readingPosition, "old")
        second.disconnect()
        _ = await secondCache.flush()
    }

    func testOfflineActivationRetainsReadingButDetachesAllNetworkAuthority() async throws {
        let cache = ReadingCache(file: nil)
        let (coordinator, fake, authority) = try await start(cache: cache)
        await coordinator.loadBrief("brief")
        await coordinator.loadActivities("turn")
        let scope = try XCTUnwrap(coordinator.contentReportScope)
        let draft = try reportDraft(coordinator)
        let partition = try XCTUnwrap(ReadingPartition(apiBaseURL: url, identity: authority))
        let snapshot = coordinator.snapshot
        let calls = await fake.expansionCalls
        let conversations = await fake.conversationCalls
        let rosters = await fake.rosterCalls

        coordinator.activateOffline(partition: partition)
        XCTAssertFalse(coordinator.isRosterCurrent)
        XCTAssertEqual(coordinator.snapshot, snapshot)
        XCTAssertNotNil(coordinator.briefs["brief"])
        XCTAssertTrue(coordinator.activities.isEmpty)
        XCTAssertTrue(coordinator.activityDetails.isEmpty)
        XCTAssertTrue(coordinator.isShowingCachedContent)
        XCTAssertNil(coordinator.contentReportScope)
        coordinator.setBriefVisible("brief", visible: true)
        await coordinator.loadBrief("missing")
        await coordinator.loadActivities("turn")
        await coordinator.reloadActivities("turn")
        await coordinator.loadActivityDetail(turnID: "turn", activityID: "activity")
        await coordinator.loadHistory()
        await coordinator.loadVisiblePreviews(agentIDs: ["A"])
        await coordinator.markRead()
        draft.category = .privacy
        await draft.submit { body, _ in try await coordinator.createContentReport(body, scope: scope) }
        XCTAssertEqual(draft.errorKey, "report.contextChanged")
        coordinator.setForeground(false)
        coordinator.setForeground(true)
        await coordinator.refresh()
        let finalExpansions = await fake.expansionCalls
        let finalConversations = await fake.conversationCalls
        let finalRosters = await fake.rosterCalls
        let reads = await fake.readCalls
        let reports = await fake.reportRequests
        XCTAssertEqual(finalExpansions, calls)
        XCTAssertEqual(finalConversations, conversations)
        XCTAssertEqual(finalRosters, rosters)
        XCTAssertEqual(reads, 0)
        XCTAssertTrue(reports.isEmpty)
        XCTAssertTrue(coordinator.loadingBriefs.isEmpty)
        XCTAssertTrue(coordinator.loadingActivities.isEmpty)
        XCTAssertEqual(coordinator.status, .offline)
        coordinator.disconnect()
    }

    func testOfflineRefreshAwaitsReconnectWithoutGrantingNetworkAuthority() async throws {
        let cache = ReadingCache(file: nil)
        let partition = try seed(cache, authority: identity())
        let coordinator = ReadingCoordinator(cache: cache, prefetchBriefLimit: 0)
        coordinator.activateOffline(partition: partition)
        coordinator.selectAgent("A")
        try await wait { coordinator.snapshot != nil }
        let snapshot = coordinator.snapshot
        var attempts = 0
        var completed = false
        coordinator.onReconnect = {
            attempts += 1
            await Task.yield()
            completed = true
        }

        await coordinator.refresh()
        XCTAssertEqual(attempts, 1)
        XCTAssertTrue(completed)
        XCTAssertEqual(coordinator.status, .offline)
        XCTAssertEqual(coordinator.snapshot, snapshot)
        XCTAssertNotNil(coordinator.briefs["brief"])
        XCTAssertTrue(coordinator.isShowingCachedContent)
        XCTAssertFalse(coordinator.isRefreshing)
        XCTAssertNil(coordinator.contentReportScope)
        coordinator.disconnect()
    }

    func testOfflineRefreshAllowsVerifiedReconnectToActivateOnlineReading() async throws {
        let authority = identity()
        let fake = ReadingFakeTransport(authority: authority)
        let cache = ReadingCache(file: nil)
        let partition = try seed(cache, authority: authority)
        let coordinator = ReadingCoordinator(cache: cache, prefetchBriefLimit: 0)
        coordinator.activateOffline(partition: partition)
        coordinator.selectAgent("A")
        var attempts = 0
        let apiBaseURL = url
        coordinator.onReconnect = { [weak coordinator] in
            attempts += 1
            coordinator?.activateOffline(partition: partition)
            await Task.yield()
            XCTAssertFalse(Task.isCancelled)
            coordinator?.activate(transport: fake, identity: authority, apiBaseURL: apiBaseURL)
            XCTAssertFalse(Task.isCancelled)
        }

        await coordinator.refresh()
        try await wait { await fake.counts().1 == 1 && coordinator.status == .live }
        XCTAssertEqual(attempts, 1)
        XCTAssertFalse(coordinator.isShowingCachedContent)
        XCTAssertNotNil(coordinator.contentReportScope)
        coordinator.onReconnect = nil
        coordinator.disconnect()
    }

    func testReconnectCallbackRequiresForegroundOfflinePartition() async throws {
        let (coordinator, fake, authority) = try await start()
        let partition = try XCTUnwrap(ReadingPartition(apiBaseURL: url, identity: authority))
        var attempts = 0
        coordinator.onReconnect = { attempts += 1 }
        let rosters = await fake.rosterCalls
        await coordinator.refresh()
        let refreshedRosters = await fake.rosterCalls
        XCTAssertGreaterThan(refreshedRosters, rosters)
        XCTAssertEqual(coordinator.status, .live)
        XCTAssertEqual(attempts, 0)

        coordinator.setForeground(false)
        coordinator.activateOffline(partition: partition)
        await coordinator.refresh()
        XCTAssertEqual(attempts, 0)
        coordinator.setForeground(true)
        XCTAssertEqual(attempts, 0)
        await coordinator.refresh()
        XCTAssertEqual(attempts, 1)
        XCTAssertEqual(coordinator.status, .offline)
        coordinator.disconnect()
        await coordinator.refresh()
        XCTAssertEqual(attempts, 1)
    }

    func testSamePartitionOfflineActivationAndAgentSelectionPreserveReconnectToken() async throws {
        let authority = identity()
        let fake = ReadingFakeTransport(authority: authority)
        let cache = ReadingCache(file: nil)
        let partition = try seed(cache, authority: authority)
        let coordinator = ReadingCoordinator(cache: cache, prefetchBriefLimit: 0)
        coordinator.activateOffline(partition: partition)
        coordinator.selectAgent("A")
        var gate: CheckedContinuation<Void, Never>?
        var cancelled = false
        let apiBaseURL = url
        coordinator.onReconnect = { [weak coordinator] in
            await withCheckedContinuation { gate = $0 }
            cancelled = Task.isCancelled
            coordinator?.activate(transport: fake, identity: authority, apiBaseURL: apiBaseURL)
        }
        let refresh = Task { await coordinator.refresh() }
        try await wait { gate != nil }
        coordinator.selectAgent("B")
        coordinator.activateOffline(partition: partition)
        gate?.resume()
        await refresh.value
        try await wait { coordinator.status == .live && coordinator.snapshot?.agentID == "B" }
        XCTAssertFalse(cancelled)
        XCTAssertEqual(coordinator.contentReportScope?.partition, partition)
        XCTAssertEqual(coordinator.contentReportScope?.agentID, "B")
        coordinator.onReconnect = nil
        coordinator.disconnect()
    }

    func testCommittedReconnectReadingTasksCanStillRevokeAuthorization() async throws {
        let authority = identity()
        let fake = ReadingFakeTransport(authority: authority)
        let cache = ReadingCache(file: nil)
        let partition = try seed(cache, authority: authority)
        await fake.setGate(.conversation("A"), failure: HolonHTTPFailure(statusCode: 401, identity: authority))
        let coordinator = ReadingCoordinator(cache: cache, prefetchBriefLimit: 0)
        coordinator.activateOffline(partition: partition)
        coordinator.selectAgent("A")
        let apiBaseURL = url
        coordinator.onReconnect = { [weak coordinator] in
            coordinator?.activate(transport: fake, identity: authority, apiBaseURL: apiBaseURL)
        }
        var failures = 0
        coordinator.onConnectionFailure = { _ in failures += 1 }

        await coordinator.refresh()
        try await wait { await fake.isBlocked() }
        await fake.release()
        try await wait { coordinator.status == .sessionExpired }
        XCTAssertNil(coordinator.snapshot)
        XCTAssertNil(coordinator.contentReportScope)
        XCTAssertNil(cache.load(partition, agentID: "A"))
        XCTAssertNil(cache.load(partition, agentID: nil))
        XCTAssertEqual(failures, 1)
        coordinator.onReconnect = nil
        coordinator.disconnect()
    }

    func testConcurrentOfflineRefreshesShareReconnectAttempt() async throws {
        let cache = ReadingCache(file: nil)
        let partition = try seed(cache, authority: identity())
        let coordinator = ReadingCoordinator(cache: cache, prefetchBriefLimit: 0)
        coordinator.activateOffline(partition: partition)
        var gates: [CheckedContinuation<Void, Never>] = []
        coordinator.onReconnect = {
            await withCheckedContinuation { gates.append($0) }
        }
        let first = Task { await coordinator.refresh() }
        try await wait { gates.count == 1 }
        var joined = false
        let second = Task {
            joined = true
            await coordinator.refresh()
        }
        try await wait { joined }
        XCTAssertEqual(gates.count, 1)
        gates[0].resume()
        await first.value
        await second.value
        XCTAssertEqual(coordinator.status, .offline)
        coordinator.onReconnect = nil
        coordinator.disconnect()
    }

    func testOldPartitionReconnectCannotActivateOrClearNewAttempt() async throws {
        let cache = ReadingCache(file: nil)
        let authority = identity()
        let otherAuthority = identity(user: "other")
        let partition = try seed(cache, authority: authority)
        let otherPartition = try seed(cache, authority: otherAuthority, body: "other body")
        let oldTransport = ReadingFakeTransport(authority: authority)
        let newTransport = ReadingFakeTransport(authority: otherAuthority)
        let coordinator = ReadingCoordinator(cache: cache, prefetchBriefLimit: 0)
        coordinator.activateOffline(partition: partition)
        var gates: [CheckedContinuation<Void, Never>] = []
        var cancelled: [Bool] = []
        let apiBaseURL = url
        coordinator.onReconnect = { [weak coordinator] in
            let index = cancelled.count
            cancelled.append(false)
            await withCheckedContinuation { gates.append($0) }
            cancelled[index] = Task.isCancelled
            let update = Task { [weak coordinator] in
                XCTAssertFalse(Task.isCancelled)
                if index == 0 {
                    // An uncancelled child inherits the stale callback token, not mutation authority.
                    coordinator?.activateOffline(partition: partition)
                    coordinator?.disconnect()
                }
                coordinator?.activate(transport: index == 0 ? oldTransport : newTransport,
                                      identity: index == 0 ? authority : otherAuthority,
                                      apiBaseURL: apiBaseURL)
            }
            await update.value
        }
        let oldRefresh = Task { await coordinator.refresh() }
        try await wait { gates.count == 1 }
        coordinator.activateOffline(partition: otherPartition)
        coordinator.selectAgent("A")
        let newRefresh = Task { await coordinator.refresh() }
        try await wait { gates.count == 2 }
        gates[0].resume()
        await oldRefresh.value
        XCTAssertTrue(cancelled[0])
        XCTAssertEqual(coordinator.status, .offline)
        XCTAssertNil(coordinator.contentReportScope)
        var joined = false
        let joinedRefresh = Task {
            joined = true
            await coordinator.refresh()
        }
        try await wait { joined }
        XCTAssertEqual(gates.count, 2, "An old completion must not clear the new attempt")
        gates[1].resume()
        await newRefresh.value
        await joinedRefresh.value
        try await wait { coordinator.status == .live && coordinator.contentReportScope != nil }
        XCTAssertFalse(cancelled[1])
        XCTAssertEqual(coordinator.contentReportScope?.partition, otherPartition)
        let oldCalls = await oldTransport.counts()
        XCTAssertEqual(oldCalls.0, 0)
        XCTAssertEqual(oldCalls.1, 0)
        coordinator.onReconnect = nil
        coordinator.disconnect()
    }

    func testDisconnectCancelsPendingOfflineReconnect() async throws {
        let cache = ReadingCache(file: nil)
        let authority = identity()
        let partition = try seed(cache, authority: authority)
        let fake = ReadingFakeTransport(authority: authority)
        let coordinator = ReadingCoordinator(cache: cache, prefetchBriefLimit: 0)
        coordinator.activateOffline(partition: partition)
        var gate: CheckedContinuation<Void, Never>?
        var cancelled = false
        let apiBaseURL = url
        coordinator.onReconnect = { [weak coordinator] in
            await withCheckedContinuation { gate = $0 }
            cancelled = Task.isCancelled
            coordinator?.activateOffline(partition: partition)
            coordinator?.activate(transport: fake, identity: authority, apiBaseURL: apiBaseURL)
        }
        let refresh = Task { await coordinator.refresh() }
        try await wait { gate != nil }
        coordinator.disconnect()
        gate?.resume()
        await refresh.value
        XCTAssertTrue(cancelled)
        XCTAssertEqual(coordinator.status, .disconnected)
        XCTAssertNil(coordinator.snapshot)
        XCTAssertNil(coordinator.contentReportScope)
        let calls = await fake.counts()
        XCTAssertEqual(calls.0, 0)
        XCTAssertEqual(calls.1, 0)
        coordinator.onReconnect = nil
    }

    func testReturningConversationRestoresBodiesWithoutWaitingForRoster() async throws {
        let cache = ReadingCache(file: nil)
        let (coordinator, fake, authority) = try await start(cache: cache)
        await coordinator.loadBrief("brief")
        coordinator.rememberPosition(turnID: "turn")
        let partition = try XCTUnwrap(ReadingPartition(apiBaseURL: url, identity: authority))
        try await wait { cache.load(partition, agentID: "A")?.position == "turn" }
        coordinator.selectAgent("B")
        try await wait { coordinator.snapshot?.agentID == "B" && !coordinator.isRefreshing }
        await fake.holdRoster()
        coordinator.selectAgent("A")
        XCTAssertFalse(coordinator.isRosterCurrent)
        XCTAssertEqual(coordinator.snapshot?.agentID, "A")
        XCTAssertNotNil(coordinator.briefs["brief"])
        XCTAssertEqual(coordinator.readingPosition, "turn")
        XCTAssertTrue(coordinator.isShowingCachedContent)
        XCTAssertTrue(coordinator.isRefreshing)
        try await wait { coordinator.status == .live }
        XCTAssertFalse(coordinator.isRosterCurrent)
        XCTAssertTrue(coordinator.isRefreshing)
        XCTAssertFalse(coordinator.isShowingCachedContent)
        XCTAssertNotNil(coordinator.briefs["brief"])
        await fake.releaseRoster()
        try await wait { !coordinator.isRefreshing }
        XCTAssertTrue(coordinator.isRosterCurrent)
        coordinator.disconnect()
    }

    func testRosterFailureCannotBlockSuccessfulConversationRefresh() async throws {
        let (coordinator, fake, _) = try await start()
        await fake.setRosterError(ReadingFakeTransport.Failure.offline)
        await coordinator.refresh()
        XCTAssertEqual(coordinator.status, .live)
        XCTAssertFalse(coordinator.isRosterCurrent)
        XCTAssertNotNil(coordinator.snapshot)
        XCTAssertFalse(coordinator.isRefreshing)
        XCTAssertFalse(coordinator.isShowingCachedContent)
        try await wait { await fake.counts().1 == 1 }
        coordinator.disconnect()
    }

    func testFullRefreshInvalidatesRosterUntilCurrentRosterSucceeds() async throws {
        let (coordinator, fake, _) = try await start()
        try await wait { coordinator.isRosterCurrent && !coordinator.isRefreshing }
        let calls = await fake.rosterCalls
        await fake.holdRoster()
        let refresh = Task { await coordinator.refresh() }
        try await wait { await fake.rosterCalls > calls && coordinator.status == .live }
        XCTAssertFalse(coordinator.isRosterCurrent)
        XCTAssertEqual(coordinator.agents.map(\.id), ["A", "B"])
        await fake.releaseRoster()
        await refresh.value
        XCTAssertTrue(coordinator.isRosterCurrent)
        coordinator.setForeground(false)
        XCTAssertFalse(coordinator.isRosterCurrent)
        coordinator.disconnect()
        XCTAssertFalse(coordinator.isRosterCurrent)
    }

    func testRosterHintRefreshInvalidatesAuthorityWhileConversationRemainsLive() async throws {
        let (coordinator, fake, _) = try await start()
        try await wait { await fake.counts().0 == 1 && coordinator.isRosterCurrent && !coordinator.isRefreshing }
        let calls = await fake.rosterCalls
        await fake.holdRoster()
        await fake.send(try readingEvent("agent_roster_hint", raw: .object([:])), to: "roster")
        try await wait { await fake.rosterCalls > calls }
        XCTAssertEqual(coordinator.status, .live)
        XCTAssertFalse(coordinator.isRosterCurrent)
        XCTAssertTrue(coordinator.isRefreshing)
        XCTAssertEqual(coordinator.agents.map(\.id), ["A", "B"])
        await fake.releaseRoster()
        try await wait { !coordinator.isRefreshing }
        XCTAssertTrue(coordinator.isRosterCurrent)
        coordinator.disconnect()
    }

    func testFailedRosterHintRefreshDoesNotRetainRosterAuthority() async throws {
        let (coordinator, fake, _) = try await start()
        try await wait { await fake.counts().0 == 1 && coordinator.isRosterCurrent && !coordinator.isRefreshing }
        let calls = await fake.rosterCalls
        await fake.setRosterError(ReadingFakeTransport.Failure.offline)
        await fake.holdRoster()
        await fake.send(try readingEvent("agent_roster_hint", raw: .object([:])), to: "roster")
        try await wait { await fake.rosterCalls > calls }
        XCTAssertEqual(coordinator.status, .live)
        XCTAssertFalse(coordinator.isRosterCurrent)
        await fake.releaseRoster()
        try await wait { coordinator.status == .offline }
        XCTAssertFalse(coordinator.isRosterCurrent)
        XCTAssertEqual(coordinator.agents.map(\.id), ["A", "B"])
        coordinator.disconnect()
    }

    func testLateRosterSuccessCannotAuthorizeAnotherOfflinePartition() async throws {
        let cache = ReadingCache(file: nil)
        let (coordinator, fake, _) = try await start(cache: cache)
        try await wait { coordinator.isRosterCurrent && !coordinator.isRefreshing }
        let other = try seed(cache, authority: identity(user: "other"), agent: "B")
        let calls = await fake.rosterCalls
        await fake.holdRoster()
        let refresh = Task { await coordinator.refresh() }
        try await wait { await fake.rosterCalls > calls }
        XCTAssertFalse(coordinator.isRosterCurrent)
        coordinator.activateOffline(partition: other)
        XCTAssertEqual(coordinator.agents.map(\.id), ["B"])
        XCTAssertFalse(coordinator.isRosterCurrent)
        await fake.releaseRoster()
        await refresh.value
        XCTAssertEqual(coordinator.agents.map(\.id), ["B"])
        XCTAssertFalse(coordinator.isRosterCurrent)
        XCTAssertEqual(coordinator.status, .offline)
        coordinator.disconnect()
    }

    func testLateRosterSuccessCannotAuthorizeNewGenerationInSamePartition() async throws {
        let (coordinator, fake, authority) = try await start()
        try await wait { coordinator.isRosterCurrent && !coordinator.isRefreshing }
        let calls = await fake.rosterCalls
        await fake.holdRoster()
        let refresh = Task { await coordinator.refresh() }
        try await wait { await fake.rosterCalls > calls }
        XCTAssertFalse(coordinator.isRosterCurrent)

        let replacementAuthority = identity()
        XCTAssertNotEqual(authority.generation, replacementAuthority.generation)
        let replacement = ReadingFakeTransport(authority: replacementAuthority)
        await replacement.setRosterError(ReadingFakeTransport.Failure.offline)
        coordinator.activate(transport: replacement, identity: replacementAuthority, apiBaseURL: url)
        try await wait { await replacement.counts().1 == 1 && !coordinator.isRefreshing }
        XCTAssertEqual(coordinator.status, .live)
        XCTAssertFalse(coordinator.isRosterCurrent)
        XCTAssertEqual(coordinator.agents.map(\.id), ["A", "B"])

        await fake.releaseRoster()
        await refresh.value
        XCTAssertEqual(coordinator.status, .live)
        XCTAssertFalse(coordinator.isRosterCurrent)
        XCTAssertEqual(coordinator.agents.map(\.id), ["A", "B"])
        coordinator.disconnect()
    }

    func testRosterStreamFailureClearsAuthorityWithoutDiscardingCachedAgents() async throws {
        let (coordinator, fake, _) = try await start()
        try await wait { await fake.counts().0 == 1 && coordinator.isRosterCurrent && !coordinator.isRefreshing }
        let snapshot = coordinator.snapshot
        await fake.overflow(agentID: "roster")
        try await wait { coordinator.status == .offline }
        XCTAssertFalse(coordinator.isRosterCurrent)
        coordinator.setForeground(false)
        XCTAssertFalse(coordinator.isRosterCurrent)
        XCTAssertEqual(coordinator.agents.map(\.id), ["A", "B"])
        XCTAssertEqual(coordinator.snapshot, snapshot)
        XCTAssertTrue(coordinator.isShowingCachedContent)
        coordinator.disconnect()
    }

    func testSameEpochRefreshRetainsLoadedHistoryBodiesAndPosition() async throws {
        let (coordinator, fake, _) = try await start()
        await fake.setHistoryTurns([readingTurn("old", index: 0)])
        await coordinator.loadHistory()
        await coordinator.loadBrief("brief")
        coordinator.rememberPosition(turnID: "old")
        await coordinator.refresh()
        XCTAssertEqual(turnIDs(coordinator), ["old", "turn"])
        XCTAssertEqual(coordinator.snapshot?.snapshotCursor, "live-1")
        XCTAssertFalse(coordinator.canLoadHistory)
        XCTAssertNotNil(coordinator.briefs["brief"])
        XCTAssertEqual(coordinator.readingPosition, "old")
        await fake.replaceEpoch()
        await coordinator.refresh()
        XCTAssertEqual(turnIDs(coordinator), ["turn"])
        XCTAssertEqual(coordinator.snapshot?.eventLogEpoch, "replacement")
        XCTAssertTrue(coordinator.briefs.isEmpty)
        XCTAssertNil(coordinator.readingPosition)
        coordinator.disconnect()
    }

    func testConversationRefreshFailurePreservesCacheAndStillPublishesRoster() async throws {
        let failures: [any Error] = [ReadingFakeTransport.Failure.offline, HolonConversationError.malformedProtocol]
        for error in failures {
            let cache = ReadingCache(file: nil)
            let (coordinator, fake, authority) = try await start(cache: cache)
            await coordinator.loadBrief("brief")
            let partition = try XCTUnwrap(ReadingPartition(apiBaseURL: url, identity: authority))
            let snapshot = coordinator.snapshot
            await fake.setAgentIDs(["B"])
            await fake.setConversationFailure(error)
            await coordinator.refresh()
            XCTAssertEqual(coordinator.agents.map(\.id), ["B"])
            XCTAssertEqual(coordinator.snapshot, snapshot)
            XCTAssertNotNil(coordinator.briefs["brief"])
            XCTAssertTrue(coordinator.isShowingCachedContent)
            XCTAssertFalse(coordinator.isRefreshing)
            XCTAssertNotNil(cache.load(partition, agentID: "A"))
            coordinator.setForeground(false)
            coordinator.disconnect()
        }
    }

    func testInvalidateFencesColdRestoreAndPendingPositionPersistence() async throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: directory) }
        let file = directory.appendingPathComponent("reading.json")
        let saved = ReadingCache(file: file)
        let partition = try seed(saved, authority: identity())
        let flushed = await saved.flush()
        XCTAssertTrue(flushed)
        let cache = ReadingCache(file: file)
        let cold = ReadingCoordinator(cache: cache, prefetchBriefLimit: 0)
        cold.activateOffline(partition: partition)
        cold.selectAgent("A")
        cold.invalidate(partition)
        await cache.prepare()
        await Task.yield()
        XCTAssertNil(cold.snapshot)
        XCTAssertNil(cache.load(partition, agentID: "A"))
        XCTAssertEqual(cold.status, .disconnected)
        _ = await cache.flush()

        let memory = ReadingCache(file: nil)
        let (coordinator, _, authority) = try await start(cache: memory)
        let current = try XCTUnwrap(ReadingPartition(apiBaseURL: url, identity: authority))
        let other = try seed(memory, authority: identity(user: "other"))
        coordinator.rememberPosition(turnID: "pending")
        coordinator.invalidate(other)
        XCTAssertNotNil(coordinator.snapshot, "Invalidating another partition must not clear current reading")
        coordinator.invalidate(current)
        try await Task.sleep(nanoseconds: 20_000_000)
        XCTAssertNil(memory.load(current, agentID: "A"))
        XCTAssertNil(memory.load(current, agentID: nil))
        XCTAssertNil(memory.load(other, agentID: "A"))
        XCTAssertNil(coordinator.snapshot)
        XCTAssertFalse(coordinator.isRefreshing)
    }

    func testAuthorizationDenialsInvalidateCacheAndDoNotReconnectOnForeground() async throws {
        for code in [401, 403] {
            let cache = ReadingCache(file: nil)
            let (coordinator, fake, authority) = try await start(cache: cache)
            await coordinator.loadBrief("brief")
            let partition = try XCTUnwrap(ReadingPartition(apiBaseURL: url, identity: authority))
            var failures = 0
            coordinator.onConnectionFailure = { _ in failures += 1 }
            await fake.setGate(.conversation("A"), failure: HolonHTTPFailure(statusCode: code, identity: authority))
            let refresh = Task { await coordinator.refresh() }
            try await wait { await fake.isBlocked() }
            XCTAssertNotNil(coordinator.snapshot)
            await fake.release()
            await refresh.value
            XCTAssertEqual(coordinator.status, code == 401 ? .sessionExpired : .permissionDenied)
            XCTAssertNil(coordinator.snapshot)
            XCTAssertTrue(coordinator.briefs.isEmpty)
            XCTAssertTrue(coordinator.agents.isEmpty)
            XCTAssertNil(cache.load(partition, agentID: "A"))
            XCTAssertNil(cache.load(partition, agentID: nil))
            XCTAssertNil(coordinator.contentReportScope)
            XCTAssertEqual(failures, 1)
            let calls = await fake.conversationCalls
            coordinator.setForeground(false)
            coordinator.setForeground(true)
            await coordinator.refresh()
            let afterForeground = await fake.conversationCalls
            XCTAssertEqual(afterForeground, calls)
            coordinator.disconnect()
        }
    }

    func testLateOnlineFailureCannotRevokeAnotherOfflinePartition() async throws {
        let cache = ReadingCache(file: nil)
        let authority = identity()
        let other = try seed(cache, authority: identity(user: "other"), body: "other account")
        let fake = ReadingFakeTransport(authority: authority)
        await fake.setGate(.conversation("A"), failure: HolonHTTPFailure(statusCode: 401, identity: authority))
        let coordinator = ReadingCoordinator(cache: cache, prefetchBriefLimit: 0)
        var failures = 0
        coordinator.onConnectionFailure = { _ in failures += 1 }
        coordinator.activate(transport: fake, identity: authority, apiBaseURL: url)
        coordinator.selectAgent("A")
        try await wait { await fake.isBlocked() }
        coordinator.activateOffline(partition: other)
        coordinator.selectAgent("A")
        XCTAssertEqual(coordinator.briefs["brief"]?["text"], .string("other account"))
        await fake.release()
        try await Task.sleep(nanoseconds: 20_000_000)
        XCTAssertEqual(coordinator.status, .offline)
        XCTAssertEqual(coordinator.briefs["brief"]?["text"], .string("other account"))
        XCTAssertNotNil(cache.load(other, agentID: "A"))
        XCTAssertEqual(failures, 0)
        coordinator.disconnect()
    }

    func testLateBriefCannotCrossEpochOrOfflinePartition() async throws {
        for replaceEpoch in [false, true] {
            let cache = ReadingCache(file: nil)
            let (coordinator, fake, _) = try await start(cache: cache)
            let other = try seed(cache, authority: identity(user: "other"), body: "other account")
            await fake.setGate(.expansions)
            let brief = Task { await coordinator.loadBrief("brief") }
            try await wait { await fake.expansionCalls == 1 }
            if replaceEpoch {
                await fake.replaceEpoch()
                await coordinator.refresh()
            } else {
                coordinator.activateOffline(partition: other)
                coordinator.selectAgent("A")
            }
            await fake.releaseExpansions()
            await brief.value
            if replaceEpoch {
                XCTAssertEqual(coordinator.snapshot?.eventLogEpoch, "replacement")
                XCTAssertNil(coordinator.briefs["brief"])
            } else {
                XCTAssertEqual(coordinator.briefs["brief"]?["text"], .string("other account"))
                XCTAssertEqual(coordinator.status, .offline)
            }
            XCTAssertTrue(coordinator.loadingBriefs.isEmpty)
            coordinator.disconnect()
        }
    }

    func testBoundedPrefetchSharesExpansionBudgetAndPersistsBodies() async throws {
        let cache = ReadingCache(file: nil)
        let authority = identity()
        let partition = try XCTUnwrap(ReadingPartition(apiBaseURL: url, identity: authority))
        let fake = ReadingFakeTransport(authority: authority)
        let ids = (0..<10).map { "brief-\($0)" }
        await fake.setBriefIDs(ids)
        await fake.setGate(.expansions)
        let coordinator = ReadingCoordinator(cache: cache)
        coordinator.activate(transport: fake, identity: authority, apiBaseURL: url)
        coordinator.selectAgent("A")
        try await wait { await fake.expansionCalls == 2 && coordinator.status == .live }
        let visible = Task { await coordinator.loadBrief(ids[0]) }
        await fake.releaseExpansions()
        await visible.value
        try await wait { coordinator.briefs.count == 9 && coordinator.loadingBriefs.isEmpty }
        let maximum = await fake.maximumExpansions
        let calls = await fake.expansionCalls
        XCTAssertEqual(maximum, 2)
        XCTAssertEqual(calls, 9, "Only eight tail bodies plus the explicit visible-body read are fetched")
        XCTAssertNotNil(cache.load(partition, agentID: "A")?.briefs[ids[0]])
        coordinator.disconnect()
        coordinator.activateOffline(partition: partition)
        coordinator.selectAgent("A")
        XCTAssertEqual(coordinator.briefs.count, 9)
        XCTAssertFalse(coordinator.isRefreshing)
        coordinator.disconnect()
    }

    func testOversizedHistoryCacheFallsBackToConfirmedLiveWindow() async throws {
        let cache = ReadingCache(file: nil)
        let authority = identity()
        let partition = try XCTUnwrap(ReadingPartition(apiBaseURL: url, identity: authority))
        let fake = ReadingFakeTransport(authority: authority)
        await fake.setLiveTurns([readingTurn("turn", index: 1, text: String(repeating: "a", count: 150_000))])
        await fake.setHistoryTurns([readingTurn("old", index: 0, text: String(repeating: "b", count: 150_000))])
        let coordinator = ReadingCoordinator(cache: cache, prefetchBriefLimit: 0)
        coordinator.activate(transport: fake, identity: authority, apiBaseURL: url)
        coordinator.selectAgent("A")
        try await wait { coordinator.status == .live }
        await coordinator.loadHistory()
        XCTAssertEqual(turnIDs(coordinator), ["old", "turn"])
        let displayed = try XCTUnwrap(coordinator.snapshot)
        XCTAssertGreaterThan(try JSONEncoder().encode(displayed.raw).count, ReadingSnapshotWindow.maximumBytes)
        let entry = try XCTUnwrap(cache.load(partition, agentID: "A"))
        let persisted = try HolonConversationSnapshot(raw: XCTUnwrap(entry.snapshot))
        XCTAssertLessThanOrEqual(try JSONEncoder().encode(persisted.raw).count, ReadingSnapshotWindow.maximumBytes)
        XCTAssertEqual(persisted.snapshotCursor, "live-1")
        XCTAssertEqual(persisted.nextBeforeCursor, "older-1")
        coordinator.disconnect()
        coordinator.activateOffline(partition: partition)
        coordinator.selectAgent("A")
        XCTAssertEqual(turnIDs(coordinator), ["turn"])
        coordinator.disconnect()
    }

    func testSameEpochDisjointWindowsKeepLiveHistoryBoundary() throws {
        let oldTurns = (1...20).map { readingTurn("turn-\($0)", index: Int64($0)) }
        let newTurns = (41...100).map { readingTurn("turn-\($0)", index: Int64($0)) }
        var active = readingTurn("active", index: 200)
        if case .object(var fields) = active {
            fields["execution"] = .object(["kind": .string("active")])
            active = .object(fields)
        }
        // A shared active execution does not bridge disjoint terminal history.
        for sharedActive in [false, true] {
            let tail = sharedActive ? [active] : []
            let history = try readingSnapshot(cursor: "cached", more: false, before: nil,
                                              turns: oldTurns + tail)
            let live = try readingSnapshot(cursor: "latest", before: "before-41", turns: newTurns + tail)
            let merged = try ReadingSnapshotWindow.merging(live, history: history)
            XCTAssertEqual(merged.raw["turns"], .array(oldTurns + newTurns + tail))
            XCTAssertEqual(merged.snapshotCursor, "latest")
            XCTAssertTrue(merged.hasMore)
            XCTAssertEqual(merged.nextBeforeCursor, "before-41")
        }
    }

    func testSameEpochRefreshUsesOnlyLiveActiveTurns() throws {
        let cachedClosed = (1...20).map { readingTurn("turn-\($0)", index: Int64($0)) }
        // A long-running execution can finish outside the latest terminal window.
        let staleActive = readingTurn("stale-active", index: 0, execution: "active")
        let currentActive = readingTurn("current-active", index: 101, execution: "active")
        let history = try readingSnapshot(cursor: "cached", more: false, before: nil,
                                          turns: cachedClosed + [staleActive])
        XCTAssertEqual(history.raw["active_turns"], .array([staleActive]))
        for firstIndex in [16, 41] {
            let liveClosed = (firstIndex...100).map { readingTurn("turn-\($0)", index: Int64($0)) }
            let live = try readingSnapshot(cursor: "latest", before: "before-\(firstIndex)",
                                           turns: liveClosed + [currentActive])
            let merged = try ReadingSnapshotWindow.merging(live, history: history)
            let expectedIndices = firstIndex == 16 ? Array(1...100) : Array(1...20) + Array(41...100)
            let expectedClosed = expectedIndices.map { readingTurn("turn-\($0)", index: Int64($0)) }
            XCTAssertEqual(merged.raw["turns"], .array(expectedClosed + [currentActive]))
            XCTAssertEqual(merged.raw["active_turns"], .array([currentActive]))
            XCTAssertEqual(merged.snapshotCursor, "latest")
        }
    }

    func testSameEpochRefreshPersistsEndedActiveRemovalAcrossColdRestart() async throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: directory) }
        let file = directory.appendingPathComponent("reading.json")
        let cache = ReadingCache(file: file)
        let closed = (1...20).map { readingTurn("turn-\($0)", index: Int64($0)) }
        let endedActive = readingTurn("turn-21", index: 21, execution: "active")
        let (coordinator, fake, authority) = try await start(cache: cache, turns: closed + [endedActive])
        let partition = try XCTUnwrap(ReadingPartition(apiBaseURL: url, identity: authority))
        XCTAssertEqual(coordinator.snapshot?.raw["active_turns"], .array([endedActive]))
        await fake.setOffline(true)
        await coordinator.refresh()
        XCTAssertEqual(coordinator.status, .offline)
        XCTAssertEqual(coordinator.snapshot?.raw["active_turns"], .array([endedActive]))

        let latest = (41...100).map { readingTurn("turn-\($0)", index: Int64($0)) }
        let expected = closed + latest
        await fake.setLiveTurns(latest)
        await fake.setOffline(false)
        await coordinator.refresh()
        XCTAssertEqual(coordinator.status, .live)
        XCTAssertEqual(coordinator.snapshot?.raw["turns"], .array(expected))
        XCTAssertEqual(coordinator.snapshot?.raw["active_turns"], .array([]))
        let saved = try XCTUnwrap(cache.load(partition, agentID: "A"))
        let persisted = try HolonConversationSnapshot(raw: XCTUnwrap(saved.snapshot))
        XCTAssertEqual(persisted.raw["turns"], .array(expected))
        XCTAssertEqual(persisted.raw["active_turns"], .array([]))
        let flushed = await cache.flush()
        XCTAssertTrue(flushed)
        coordinator.disconnect()

        let restored = ReadingCoordinator(cache: ReadingCache(file: file), prefetchBriefLimit: 0)
        restored.activateOffline(partition: partition)
        restored.selectAgent("A")
        try await wait { restored.snapshot != nil }
        XCTAssertEqual(restored.status, .offline)
        XCTAssertTrue(restored.isShowingCachedContent)
        XCTAssertEqual(restored.snapshot?.raw["turns"], .array(expected))
        XCTAssertEqual(restored.snapshot?.raw["active_turns"], .array([]))
        restored.disconnect()
    }

    func testSameEpochContainedCacheKeepsOlderLiveHistoryBoundary() throws {
        let live = try readingSnapshot(more: false, before: nil,
                                       turns: (1...60).map { readingTurn("turn-\($0)", index: Int64($0)) })
        let history = try readingSnapshot(cursor: "cached", before: "before-41",
                                          turns: (41...60).map { readingTurn("turn-\($0)", index: Int64($0)) })
        XCTAssertEqual(try ReadingSnapshotWindow.merging(live, history: history), live)
    }

    func testSameEpochReconnectLoadsGapBetweenCachedAndLiveWindows() async throws {
        let (coordinator, fake, _) = try await start(
            turns: (1...20).map { readingTurn("turn-\($0)", index: Int64($0)) })
        await fake.setHistoryTurns([])
        await coordinator.loadHistory()
        XCTAssertFalse(coordinator.canLoadHistory)
        await fake.setOffline(true)
        await coordinator.refresh()
        XCTAssertEqual(coordinator.status, .offline)

        await fake.setLiveTurns((41...100).map { readingTurn("turn-\($0)", index: Int64($0)) })
        await fake.setOffline(false)
        await coordinator.refresh()
        XCTAssertEqual(turnIDs(coordinator).count, 80)
        XCTAssertTrue(coordinator.canLoadHistory)
        await fake.setHistoryTurns((1...40).map { readingTurn("turn-\($0)", index: Int64($0)) })
        await coordinator.loadHistory()
        XCTAssertEqual(turnIDs(coordinator), (1...100).map { "turn-\($0)" })
        XCTAssertEqual(coordinator.snapshot?.snapshotCursor, "live-1")
        XCTAssertFalse(coordinator.canLoadHistory)
        let cursors = await fake.historyCursors
        XCTAssertEqual(cursors, ["older-1", "older-1"])
        coordinator.disconnect()
    }

    func testSameEpochHistoryCapacityUsesServerWindowWithoutInventingCursor() throws {
        let live = try readingSnapshot(turns: (1...180).map { readingTurn("turn-\($0)", index: Int64($0)) })
        let history = try readingSnapshot(cursor: "old", more: false, before: nil,
                                          turns: [readingTurn("old", index: 0)])
        let bounded = try ReadingSnapshotWindow.merging(live, history: history)
        XCTAssertEqual(bounded, live)
        XCTAssertEqual(bounded.nextBeforeCursor, "older-1")
        let replacement = try readingSnapshot(epoch: "other")
        XCTAssertEqual(try ReadingSnapshotWindow.merging(replacement, history: history), replacement)
    }

    func testSamePartitionOnlineActivationDoesNotClearReadableProjection() async throws {
        let cache = ReadingCache(file: nil)
        let (coordinator, _, authority) = try await start(cache: cache)
        await coordinator.loadBrief("brief")
        let snapshot = coordinator.snapshot
        let renewed = identity()
        let fake = ReadingFakeTransport(authority: renewed)
        await fake.holdRoster()
        await fake.setGate(.conversation("A"))
        coordinator.activate(transport: fake, identity: renewed, apiBaseURL: url)
        XCTAssertEqual(coordinator.selectedAgentID, "A")
        XCTAssertEqual(coordinator.snapshot, snapshot)
        XCTAssertNotNil(coordinator.briefs["brief"])
        XCTAssertTrue(coordinator.isShowingCachedContent)
        XCTAssertTrue(coordinator.isRefreshing)
        XCTAssertNil(coordinator.contentReportScope)
        try await wait { await fake.isBlocked() }
        await fake.release()
        try await wait { coordinator.status == .live }
        XCTAssertFalse(coordinator.isShowingCachedContent)
        XCTAssertTrue(coordinator.isRefreshing)
        await fake.releaseRoster()
        try await wait { !coordinator.isRefreshing }
        XCTAssertNotEqual(renewed.generation, authority.generation)
        coordinator.disconnect()
    }

    func testFailedConnectionCanRetryAndRecoverAfterForegroundReentry() async throws {
        let cache = ReadingCache(file: nil)
        let authority = identity()
        let partition = try seed(cache, authority: authority)
        let fake = ReadingFakeTransport(authority: authority)
        await fake.setOffline(true)
        let coordinator = ReadingCoordinator(cache: cache, prefetchBriefLimit: 0)
        coordinator.activateOffline(partition: partition)
        coordinator.selectAgent("A")
        let cached = coordinator.snapshot
        coordinator.activate(transport: fake, identity: authority, apiBaseURL: url)
        try await wait { coordinator.status == .offline }
        XCTAssertEqual(coordinator.snapshot, cached)
        XCTAssertNotNil(coordinator.briefs["brief"])
        await fake.setOffline(false)
        await coordinator.refresh()
        XCTAssertEqual(coordinator.status, .live)
        XCTAssertEqual(turnIDs(coordinator), ["old", "turn"])
        coordinator.setForeground(false)
        XCTAssertTrue(coordinator.isShowingCachedContent)
        coordinator.setForeground(true)
        try await wait { coordinator.status == .live && !coordinator.isRefreshing }
        XCTAssertNotNil(coordinator.briefs["brief"])
        coordinator.disconnect()
    }

    func testColdOnlineActivationShowsDiskCacheDuringIndependentRefreshFailure() async throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: directory) }
        let file = directory.appendingPathComponent("reading.json")
        let saved = ReadingCache(file: file)
        let authority = identity()
        let partition = try seed(saved, authority: authority)
        let flushed = await saved.flush()
        XCTAssertTrue(flushed)
        let cache = ReadingCache(file: file)
        let fake = ReadingFakeTransport(authority: authority)
        await fake.holdRoster()
        await fake.setGate(.conversation("A"), failure: ReadingFakeTransport.Failure.offline)
        let coordinator = ReadingCoordinator(cache: cache, prefetchBriefLimit: 0)
        coordinator.activate(transport: fake, identity: authority, apiBaseURL: url)
        coordinator.selectAgent("A")
        try await wait {
            let blocked = await fake.isBlocked()
            return coordinator.briefs["brief"] != nil && blocked
        }
        XCTAssertEqual(turnIDs(coordinator), ["old", "turn"])
        XCTAssertEqual(coordinator.readingPosition, "old")
        XCTAssertTrue(coordinator.isShowingCachedContent)
        XCTAssertTrue(coordinator.isRefreshing)
        await fake.release()
        await fake.releaseRoster()
        try await wait { coordinator.status == .offline && !coordinator.isRefreshing }
        XCTAssertEqual(turnIDs(coordinator), ["old", "turn"])
        XCTAssertEqual(coordinator.briefs["brief"]?["text"], .string("cached body"))
        XCTAssertNotNil(cache.load(partition, agentID: "A"))
        coordinator.setForeground(false)
        coordinator.disconnect()
        _ = await cache.flush()
    }

    func testCacheRosterWindowIsBoundedWithoutTruncatingOnlineDisplay() async throws {
        let cache = ReadingCache(file: nil)
        let (coordinator, fake, authority) = try await start(cache: cache)
        await fake.setAgentIDs((0..<700).map { "agent-\($0)" })
        await coordinator.refresh()
        let partition = try XCTUnwrap(ReadingPartition(apiBaseURL: url, identity: authority))
        XCTAssertEqual(coordinator.agents.count, 700)
        let cached = try XCTUnwrap(cache.load(partition, agentID: nil))
        XCTAssertFalse(cached.agents.isEmpty)
        XCTAssertLessThanOrEqual(cached.agents.count, 512)
        coordinator.disconnect()
        coordinator.activateOffline(partition: partition)
        XCTAssertEqual(coordinator.agents, cached.agents)
        coordinator.disconnect()
    }

    func testCurrent401ClearsDisplayAndCallsConnectionOnce() async throws {
        let authority = identity()
        let fake = ReadingFakeTransport(authority: authority)
        await fake.setGate(.conversation("A"), failure: HolonHTTPFailure(statusCode: 401, identity: authority))
        let coordinator = ReadingCoordinator(cache: ReadingCache(file: nil), prefetchBriefLimit: 0)
        var failures: [HolonHTTPFailure] = []
        coordinator.onConnectionFailure = { failures.append($0) }
        coordinator.activate(transport: fake, identity: authority, apiBaseURL: url)
        coordinator.selectAgent("A")
        try await wait { await fake.isBlocked() }
        await fake.release()
        try await wait { coordinator.status == .sessionExpired }
        XCTAssertNil(coordinator.snapshot)
        XCTAssertTrue(coordinator.agents.isEmpty)
        XCTAssertEqual(failures.count, 1)
        coordinator.disconnect()
    }
}
