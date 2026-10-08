import Foundation
import HolonClient
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
    private var readFailure = false
    private var liveCursor = "live-1"
    private var liveEpoch = "epoch"
    private var currentRunID: String?
    private var agentIDs = ["A", "B"]
    private var briefIDs = ["brief"]
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
    private(set) var historyCursors: [String] = []
    private(set) var openedAfter: [String] = []
    private(set) var maximumDetailStreams = 0
    private(set) var maximumRosterStreams = 0
    private(set) var readCalls = 0
    private(set) var closes = 0
    private(set) var maximumExpansions = 0
    private(set) var expansionCalls = 0

    init(authority: HolonConnectionIdentity) { self.authority = authority }
    func setGate(_ gate: Gate, failure: (any Error)? = nil) {
        self.gate = gate
        pendingFailure = failure
    }
    func setOffline(_ offline: Bool) { rosterFailure = offline }
    func failReads() { readFailure = true }
    func setBriefIDs(_ ids: [String]) { briefIDs = ids }
    func setRunID(_ id: String?) { currentRunID = id }
    func setAgentIDs(_ ids: [String]) { agentIDs = ids }
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
    func roster() async throws -> [ReadingAgent] {
        rosterCalls += 1
        if rosterFailure { throw Failure.offline }
        return agentIDs.map {
            ReadingAgent(id: $0, name: $0, preview: "recent", operatorPreview: "operator", unreadCount: 2,
                         currentRunID: currentRunID)
        }
    }
    func conversation(agentID: String, before: String?) async throws -> HolonConversationSnapshot {
        let failure = pendingFailure
        let shouldBlock = (before != nil && gate == .history) || gate == .conversation(agentID)
        if shouldBlock {
            gate = nil
            pendingFailure = nil
            await withCheckedContinuation { blocked = $0 }
            if let failure { throw failure }
        }
        if let before {
            historyCursors.append(before)
            return try readingSnapshot(agent: agentID, cursor: "history-not-live", more: false, before: nil)
        }
        return try readingSnapshot(agent: agentID, cursor: liveCursor, epoch: liveEpoch, turns: [
            .object(["turn_id": .string("turn"),
                     "key": .object(["turn_id": .string("turn"), "turn_index": .integer(1)]),
                     "revision": .integer(1), "brief_ids": .array(briefIDs.map(JSONValue.string))])
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
                        "created_event_seq": .integer(5)]
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
        _ = await expansion("id", id: agentID)
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
        if readFailure { throw Failure.read }
        return .object(["state": .object([
            "agent_id": .string(agentID), "event_log_epoch": .string(liveEpoch),
            "visibility_scope_id": .string("private"),
            "read_through_event_seq": .integer(through)
        ]), "applied_read_through_event_seq": .integer(through)])
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
    private func start(cache: ReadingCache? = nil) async throws
        -> (ReadingCoordinator, ReadingFakeTransport, HolonConnectionIdentity) {
        let authority = identity()
        let fake = ReadingFakeTransport(authority: authority)
        let coordinator = ReadingCoordinator(cache: cache ?? ReadingCache(file: nil))
        coordinator.activate(transport: fake, identity: authority, apiBaseURL: url)
        coordinator.selectAgent("A")
        try await wait { await fake.counts().1 == 1 && coordinator.status == .live }
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

    func testAgentSwitchFencesLateSuccessAndAuthenticationFailures() async throws {
        for statusCode: Int? in [nil, 401, 403] {
            let authority = identity()
            let fake = ReadingFakeTransport(authority: authority)
            await fake.setGate(.conversation("A"), failure: statusCode.map {
                HolonHTTPFailure(statusCode: $0, identity: authority)
            })
            let coordinator = ReadingCoordinator(cache: ReadingCache(file: nil))
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
        let coordinator = ReadingCoordinator(cache: ReadingCache(file: nil))
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

    func testCurrent401ClearsDisplayAndCallsConnectionOnce() async throws {
        let authority = identity()
        let fake = ReadingFakeTransport(authority: authority)
        await fake.setGate(.conversation("A"), failure: HolonHTTPFailure(statusCode: 401, identity: authority))
        let coordinator = ReadingCoordinator(cache: ReadingCache(file: nil))
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
