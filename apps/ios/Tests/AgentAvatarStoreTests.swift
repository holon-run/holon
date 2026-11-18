import HolonClient
import UIKit
import XCTest
@testable import Holon

private actor AvatarFixture: AgentAvatarTransport {
    let supported: Bool
    let data: Data
    private var failure: HolonHTTPFailure?
    let delay: Duration
    private(set) var handshakes = 0
    private(set) var calls: [String] = []
    private(set) var running = 0
    private(set) var maximumRunning = 0
    private var held: [CheckedContinuation<Data, any Error>] = []
    private let hold: Bool

    init(supported: Bool = true, data: Data, failure: HolonHTTPFailure? = nil,
         delay: Duration = .zero, hold: Bool = false) {
        self.supported = supported
        self.data = data
        self.failure = failure
        self.delay = delay
        self.hold = hold
    }

    func supportsAvatars() async throws -> Bool {
        handshakes += 1
        return supported
    }

    func image(agentID: String, expression: HolonAvatarExpression) async throws -> Data {
        calls.append(agentID + ":" + expression.rawValue)
        running += 1
        maximumRunning = max(maximumRunning, running)
        defer { running -= 1 }
        if hold {
            // Deliberately ignores cancellation to exercise the consumer's epoch fence.
            return try await withCheckedThrowingContinuation { held.append($0) }
        }
        if delay != .zero { try await Task.sleep(for: delay) }
        if let failure { throw failure }
        return data
    }

    func release() {
        let pending = held
        held.removeAll()
        for continuation in pending { continuation.resume(returning: data) }
    }

    func settled(after count: Int) -> Bool { calls.count == count && running == 0 }
    func deny(_ failure: HolonHTTPFailure) { self.failure = failure }
}

private actor HeldAvatarCache: AgentAvatarCaching {
    private(set) var snapshot: AgentAvatarCache.Snapshot?
    private(set) var loads = 0
    private(set) var saves = 0
    private(set) var removals = 0
    private var holdLoad: Bool
    private var holdSave: Bool
    private var loadGate: CheckedContinuation<Void, Never>?
    private var saveGate: CheckedContinuation<Void, Never>?

    init(snapshot: AgentAvatarCache.Snapshot? = nil, holdLoad: Bool = false, holdSave: Bool = false) {
        self.snapshot = snapshot
        self.holdLoad = holdLoad
        self.holdSave = holdSave
    }

    func load(_ partition: ReadingPartition) async -> AgentAvatarCache.Snapshot? {
        loads += 1
        let saved = snapshot
        if holdLoad { await withCheckedContinuation { loadGate = $0 } }
        return saved?.partition == partition ? saved : nil
    }

    func save(_ snapshot: AgentAvatarCache.Snapshot) async {
        saves += 1
        if holdSave { await withCheckedContinuation { saveGate = $0 } }
        self.snapshot = snapshot
    }

    func remove(_ partition: ReadingPartition) {
        removals += 1
        if snapshot?.partition == partition { snapshot = nil }
    }

    func releaseLoad() {
        holdLoad = false
        loadGate?.resume()
        loadGate = nil
    }

    func releaseSave() {
        holdSave = false
        saveGate?.resume()
        saveGate = nil
    }
}

@MainActor
final class AgentAvatarStoreTests: XCTestCase {
    private func png(size: CGFloat = 128) -> Data {
        let format = UIGraphicsImageRendererFormat()
        format.scale = 1
        format.preferredRange = .standard
        return UIGraphicsImageRenderer(size: CGSize(width: size, height: size), format: format).pngData { context in
            UIColor.cyan.setFill()
            context.fill(CGRect(x: 8, y: 8, width: size - 16, height: size - 16))
        }
    }

    private func waitUntil(_ condition: () async -> Bool, file: StaticString = #filePath, line: UInt = #line) async throws {
        let deadline = ContinuousClock.now + .seconds(3)
        while !(await condition()) {
            guard ContinuousClock.now < deadline else { XCTFail("Avatar condition timed out", file: file, line: line); return }
            try await Task.sleep(for: .milliseconds(5))
        }
    }

    func testUnsupportedHandshakeNeverRequestsAnImage() async throws {
        let transport = AvatarFixture(supported: false, data: png())
        let store = AgentAvatarStore()
        store.activate(transport: transport)
        store.setOnline(true)
        store.request(agentID: "agent", expression: .focused)
        try await waitUntil { await transport.handshakes == 1 }
        for _ in 0..<20 { store.request(agentID: "agent", expression: .focused); await Task.yield() }
        let calls = await transport.calls
        XCTAssertTrue(calls.isEmpty)
        XCTAssertEqual(store.cachedCount, 0)
        store.disconnect()
    }

    func testCoalescesAndRetainsNeutralForOfflineWithoutNewRequests() async throws {
        let transport = AvatarFixture(data: png())
        let store = AgentAvatarStore()
        store.activate(transport: transport)
        store.setOnline(true)
        for _ in 0..<10 { store.request(agentID: "same", expression: .focused) }
        try await waitUntil { store.cachedCount == 2 }
        let neutral = try XCTUnwrap(store.image(agentID: "same", expression: .neutral))
        let calls = await transport.calls
        XCTAssertEqual(Set(calls), ["same:neutral", "same:focused"])
        XCTAssertEqual(calls.count, 2)
        store.setOnline(false)
        XCTAssertTrue(store.image(agentID: "same", expression: .focused) === neutral)
        store.request(agentID: "other", expression: .questioning)
        let offlineCalls = await transport.calls
        XCTAssertEqual(offlineCalls, calls)
        store.disconnect()
        XCTAssertNil(store.image(agentID: "same", expression: .neutral))
        XCTAssertEqual(store.retainedBytes, 0)
    }

    func testConnectionReplacementRejectsNonCooperativeLateImages() async throws {
        let old = AvatarFixture(data: png(), hold: true)
        let replacement = AvatarFixture(supported: false, data: png())
        let store = AgentAvatarStore()
        store.activate(transport: old)
        store.setOnline(true)
        store.request(agentID: "same", expression: .neutral)
        try await waitUntil { await old.calls.count == 1 }
        let oldSession = store.sessionID
        store.activate(transport: replacement)
        store.setOnline(true)
        XCTAssertNotEqual(store.sessionID, oldSession)
        await old.release()
        try await waitUntil { await old.running == 0 }
        XCTAssertEqual(store.cachedCount, 0)
        XCTAssertNil(store.image(agentID: "same", expression: .neutral))
        store.disconnect()
    }

    func testBackgroundCancellationRejectsLateExpressionButKeepsSessionNeutral() async throws {
        let transport = AvatarFixture(data: png(), hold: true)
        let store = AgentAvatarStore()
        store.activate(transport: transport)
        store.setOnline(true)
        store.request(agentID: "same", expression: .focused)
        try await waitUntil { await transport.calls.count == 2 }
        store.setOnline(false)
        await transport.release()
        try await waitUntil { await transport.running == 0 }
        XCTAssertEqual(store.cachedCount, 0)
        store.disconnect()
    }

    func testServerEpochInvalidationPurgesAndRenegotiatesBeforeRefetch() async throws {
        let transport = AvatarFixture(data: png())
        let store = AgentAvatarStore()
        store.activate(transport: transport)
        store.setOnline(true)
        store.request(agentID: "same", expression: .neutral)
        try await waitUntil { store.cachedCount == 1 }
        let previous = store.sessionID
        store.invalidateResources()
        XCTAssertNotEqual(store.sessionID, previous)
        XCTAssertEqual(store.cachedCount, 0)
        store.request(agentID: "same", expression: .neutral)
        try await waitUntil { store.cachedCount == 1 }
        let handshakes = await transport.handshakes
        XCTAssertEqual(handshakes, 2)
        store.disconnect()
    }

    func testConcurrencyAndDecodedMemoryAreBounded() async throws {
        let transport = AvatarFixture(data: png(), delay: .milliseconds(20))
        let store = AgentAvatarStore(entryLimit: 3, byteLimit: 3 * 128 * 128 * 4)
        store.activate(transport: transport)
        store.setOnline(true)
        for number in 0..<20 { store.request(agentID: "agent-\(number)", expression: .neutral) }
        try await waitUntil { await transport.settled(after: 20) }
        let maximum = await transport.maximumRunning
        XCTAssertLessThanOrEqual(maximum, 4)
        XCTAssertEqual(store.cachedCount, 3)
        XCTAssertLessThanOrEqual(store.retainedBytes, 3 * 128 * 128 * 4)
        store.disconnect()
    }

    func testMalformedOrWrongSizeImagesDoNotRetryOnAppearance() async throws {
        for data in [Data("not a PNG".utf8), png(size: 512)] {
            let transport = AvatarFixture(data: data)
            let store = AgentAvatarStore()
            store.activate(transport: transport)
            store.setOnline(true)
            store.request(agentID: "bad", expression: .neutral)
            try await waitUntil { await transport.settled(after: 1) }
            try await waitUntil { store.inFlightCount == 0 }
            for _ in 0..<10 { store.request(agentID: "bad", expression: .neutral); await Task.yield() }
            let calls = await transport.calls
            XCTAssertEqual(calls.count, 1)
            XCTAssertEqual(store.cachedCount, 0)
            store.retry()
            store.request(agentID: "bad", expression: .neutral)
            try await waitUntil { await transport.calls.count == 2 }
            store.disconnect()
        }
    }

    func testAuthorizationDenialPurgesEverythingAndNotifiesReader() async throws {
        let identity = HolonConnectionIdentity(networkID: "network")
        let failure = HolonHTTPFailure(statusCode: 403, apiError: nil, identity: identity)
        let transport = AvatarFixture(data: png())
        let store = AgentAvatarStore()
        var received: HolonHTTPFailure?
        store.onAuthorizationFailure = { received = $0 }
        store.activate(transport: transport)
        store.setOnline(true)
        store.request(agentID: "agent", expression: .neutral)
        try await waitUntil { store.cachedCount == 1 }
        await transport.deny(failure)
        store.request(agentID: "denied", expression: .neutral)
        try await waitUntil { received != nil }
        XCTAssertEqual(received?.identity, identity)
        XCTAssertFalse(store.isOnline)
        XCTAssertEqual(store.cachedCount, 0)
        store.request(agentID: "agent", expression: .neutral)
        let calls = await transport.calls
        XCTAssertEqual(calls.count, 2)
    }

    private func partition(user: String = "reader") throws -> ReadingPartition {
        try XCTUnwrap(ReadingPartition(apiBaseURL: URL(string: "https://avatar.example/api")!,
            identity: HolonConnectionIdentity(networkID: "network", runtimeID: "runtime",
                                             userID: user, visibilityScopeID: "private")))
    }

    func testNeutralRemainsVisibleWhileAnotherExpressionLoads() async throws {
        let transport = AvatarFixture(data: png())
        let store = AgentAvatarStore()
        store.activate(transport: transport)
        store.setOnline(true)
        store.request(agentID: "same", expression: .neutral)
        try await waitUntil { store.cachedCount == 1 }
        let neutral = try XCTUnwrap(store.image(agentID: "same", expression: .neutral))
        XCTAssertTrue(store.image(agentID: "same", expression: .focused) === neutral)
        XCTAssertTrue(store.image(agentID: "same", expression: .waiting) === neutral)
        store.disconnect()
    }

    func testDiskCacheSurvivesColdLaunchAndSameIdentityReconnectWithoutDownloads() async throws {
        let file = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: file) }
        let cache = AgentAvatarCache(file: file), partition = try partition()
        let transport = AvatarFixture(data: png())
        let original = AgentAvatarStore(cache: cache)
        original.activate(transport: transport, partition: partition)
        original.observeResourceEpoch("epoch")
        original.setOnline(true)
        original.request(agentID: "same", expression: .focused)
        try await waitUntil { original.cachedCount == 2 }
        await original.flushCache()
        original.disconnect()

        let restored = AgentAvatarStore(cache: AgentAvatarCache(file: file))
        restored.activateOffline(partition: partition)
        try await waitUntil { restored.cachedCount == 2 }
        XCTAssertNotNil(restored.image(agentID: "same", expression: .neutral))
        let replacement = AvatarFixture(data: png())
        restored.activate(transport: replacement, partition: partition)
        restored.observeResourceEpoch("epoch")
        restored.setOnline(true)
        restored.request(agentID: "same", expression: .focused)
        try await waitUntil { await replacement.handshakes == 1 }
        let downloads = await replacement.calls
        XCTAssertTrue(downloads.isEmpty)
        XCTAssertEqual(restored.cachedCount, 2)
        restored.disconnect()
    }

    func testPersistedAvatarsCannotCrossIdentityOrServerEpochAndPurgeIsDurable() async throws {
        let file = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: file) }
        let cache = AgentAvatarCache(file: file), partition = try partition()
        let store = AgentAvatarStore(cache: cache), transport = AvatarFixture(data: png())
        store.activate(transport: transport, partition: partition)
        store.observeResourceEpoch("old")
        store.setOnline(true)
        store.request(agentID: "same", expression: .neutral)
        try await waitUntil { store.cachedCount == 1 }
        await store.flushCache()
        let foreignPartition = try self.partition(user: "other")
        let foreign = await cache.load(foreignPartition)
        XCTAssertNil(foreign)
        store.observeResourceEpoch("new")
        XCTAssertEqual(store.cachedCount, 0)
        await store.flushCache()
        let invalidated = await cache.load(partition)
        XCTAssertNil(invalidated)
        store.request(agentID: "same", expression: .neutral)
        try await waitUntil { store.cachedCount == 1 }
        store.purge(partition)
        await store.flushCache()
        let removed = await cache.load(partition)
        XCTAssertNil(removed, "An older queued write must not resurrect a signed-out cache")
        XCTAssertEqual(store.cachedCount, 0)
    }

    func testEpochArrivingBeforeColdRestoreDeletesOldSnapshotBeforeOfflineRelaunch() async throws {
        let partition = try partition()
        let saved = AgentAvatarCache.Snapshot(partition: partition, epoch: "old",
            images: [.init(agentID: "same", expression: .neutral, png: png())])
        let cache = HeldAvatarCache(snapshot: saved, holdLoad: true)
        let store = AgentAvatarStore(cache: cache)
        store.activateOffline(partition: partition)
        store.observeResourceEpoch("new")
        try await waitUntil { await cache.loads == 1 }
        await cache.releaseLoad()
        try await waitUntil { await cache.removals == 1 }
        await store.flushCache()
        XCTAssertEqual(store.cachedCount, 0)
        store.disconnect()

        let relaunched = AgentAvatarStore(cache: cache)
        relaunched.activateOffline(partition: partition)
        try await waitUntil { await cache.loads == 2 }
        let persisted = await cache.snapshot
        XCTAssertNil(persisted)
        XCTAssertEqual(relaunched.cachedCount, 0)
        relaunched.disconnect()
    }

    func testSlowPersistenceCoalescesToOneLatestPendingSnapshot() async throws {
        let cache = HeldAvatarCache(holdSave: true)
        let store = AgentAvatarStore(entryLimit: 2, cache: cache)
        store.activate(transport: AvatarFixture(data: png()), partition: try partition())
        store.observeResourceEpoch("epoch")
        store.setOnline(true)
        store.request(agentID: "agent-0", expression: .neutral)
        try await waitUntil { await cache.saves == 1 }
        for index in 1..<24 {
            let agent = "agent-\(index)"
            store.request(agentID: agent, expression: .neutral)
            try await waitUntil { store.image(agentID: agent, expression: .neutral) != nil }
        }
        await cache.releaseSave()
        await store.flushCache()
        let saves = await cache.saves
        let savedAgents = await cache.snapshot?.images.map(\.agentID)
        XCTAssertEqual(saves, 2, "Slow storage must not retain one full snapshot task per download")
        XCTAssertEqual(Set(savedAgents ?? []), ["agent-22", "agent-23"])
        XCTAssertEqual(store.cachedCount, 2)
        store.disconnect()
    }

    func testPurgeDiscardsPendingSnapshotAndRunsAfterBlockedWrite() async throws {
        let cache = HeldAvatarCache(holdSave: true)
        let partition = try partition()
        let store = AgentAvatarStore(cache: cache)
        store.activate(transport: AvatarFixture(data: png()), partition: partition)
        store.observeResourceEpoch("epoch")
        store.setOnline(true)
        store.request(agentID: "first", expression: .neutral)
        try await waitUntil { await cache.saves == 1 }
        store.request(agentID: "second", expression: .neutral)
        try await waitUntil { store.image(agentID: "second", expression: .neutral) != nil }
        store.purge(partition)
        await cache.releaseSave()
        await store.flushCache()
        let saves = await cache.saves
        let removals = await cache.removals
        let persisted = await cache.snapshot
        XCTAssertEqual(saves, 1)
        XCTAssertEqual(removals, 1)
        XCTAssertNil(persisted, "A blocked old write must not resurrect a purged cache")
    }
}
