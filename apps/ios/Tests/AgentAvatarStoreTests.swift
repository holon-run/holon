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
}
