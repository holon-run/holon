import Foundation
import HolonClient
import XCTest
@testable import Holon

private final class MetadataMetrics: @unchecked Sendable {
    private let lock = NSLock()
    private var active = 0
    private var peak = 0
    private var requests = 0
    private var limits: [String?] = []
    func begin(_ limit: String?) {
        lock.lock(); defer { lock.unlock() }
        active += 1; peak = max(peak, active); requests += 1; limits.append(limit)
    }
    func end() { lock.lock(); active -= 1; lock.unlock() }
    func snapshot() -> (Int, Int, [String?]) {
        lock.lock(); defer { lock.unlock() }; return (peak, requests, limits)
    }
}

private final class MetadataProtocol: URLProtocol, @unchecked Sendable {
    static let metrics = MetadataMetrics()
    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func startLoading() {
        let url = request.url!
        let mode = url.host!
        let path = url.path
        var status = 200
        let body: String
        if path.hasSuffix("/snapshot") {
            let count = mode == "bounded.invalid" ? 81 : 1
            let entries = (0..<count).map { #"{"agent":{"identity":{"agent_id":"A\#($0)","name":"Agent"}},"latest_brief":{"preview":"brief"}}"# }
            body = #"{"runtime_id":"runtime","visibility_scope_id":"private","event_log_epoch":"epoch","agents":[\#(entries.joined(separator: ","))]}"#
        } else if path.hasSuffix("/brief-read-states") {
            if mode == "denied.invalid" { status = 403 }
            if mode == "expired.invalid" { status = 401 }
            if mode == "unavailable.invalid" { status = 503 }
            let epoch = mode == "epoch.invalid" ? "other" : "epoch"
            body = #"[{"agent_id":"A0","event_log_epoch":"\#(epoch)","visibility_scope_id":"private","unread_count":3,"reset_required":false,"retention_gap":false}]"#
        } else {
            let id = url.pathComponents[url.pathComponents.count - 2]
            let limit = URLComponents(url: url, resolvingAgainstBaseURL: false)?.queryItems?.first { $0.name == "limit" }?.value
            Self.metrics.begin(limit)
            if mode == "preview-denied.invalid" { status = 403 }
            if mode == "preview-unavailable.invalid" { status = 503 }
            body = #"{"runtime_id":"runtime","visibility_scope_id":"private","agent_id":"\#(id)","event_log_epoch":"epoch","schema_version":1,"query_version":1,"snapshot_cursor":"live","has_more":false,"turns":[],"active_turns":[],"pending_inputs":[{"message_id":"m","presentation_class":"operator","created_at":"2026-01-01T00:00:00Z","preview":"{\"type\":\"text\",\"text\":\"real operator\"}"}]}"#
        }
        let conversation = path.hasSuffix("/conversation")
        let responseStatus = status
        DispatchQueue.global().asyncAfter(deadline: .now() + (conversation ? 0.01 : 0)) {
            let response = HTTPURLResponse(url: url, statusCode: responseStatus, httpVersion: nil,
                                           headerFields: ["Content-Type": "application/json"])!
            if conversation { Self.metrics.end() }
            self.client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
            self.client?.urlProtocol(self, didLoad: Data(body.utf8))
            self.client?.urlProtocolDidFinishLoading(self)
        }
    }
    override func stopLoading() {}
}

final class ReadingMetadataTests: XCTestCase {
    private func transport(_ mode: String) async throws -> ReadingClientTransport {
        let config = URLSessionConfiguration.ephemeral
        config.protocolClasses = [MetadataProtocol.self]
        let client = try HolonClient(endpoint: HolonEndpoint(apiBaseURL: URL(string: "https://\(mode).invalid/api")!),
                                     networkID: "network", configuration: config)
        try await client.bindIdentity(runtimeID: "runtime", userID: "user", visibilityScopeID: "private", credential: nil)
        return ReadingClientTransport(client: client, authority: HolonConnectionIdentity(
            networkID: "network", runtimeID: "runtime", userID: "user", visibilityScopeID: "private"))
    }

    func testHostedMetadataAndUnknownFailures() async throws {
        for mode in ["real", "unavailable", "preview-unavailable"] {
            let transport = try await transport(mode)
            let agents = try await transport.roster()
            XCTAssertEqual(agents.first?.preview, "brief")
            XCTAssertEqual(agents.first?.unreadCount, mode == "unavailable" ? nil : 3)
            XCTAssertNil(agents.first?.operatorPreview)
            let preview = try await transport.operatorPreview(agentID: "A0")
            XCTAssertEqual(preview?.text, mode == "preview-unavailable" ? nil : "real operator")
            await transport.close()
        }
    }

    func testHostedAuthorityAndEpochFailuresPropagate() async throws {
        for mode in ["denied", "expired", "preview-denied", "epoch"] {
            let transport = try await transport(mode)
            do {
                _ = try await transport.roster()
                _ = try await transport.operatorPreview(agentID: "A0")
                XCTFail("Must not return stale-authority metadata")
            } catch let error as HolonHTTPFailure {
                XCTAssertEqual(error.statusCode, mode == "expired" ? 401 : 403)
            } catch let error as HolonConversationError {
                XCTAssertEqual(mode, "epoch")
                XCTAssertEqual(error, .bootstrapRequired)
            }
            await transport.close()
        }
    }

    func testHostedBoundedPreviewReads() async throws {
        let before = MetadataProtocol.metrics.snapshot().1
        let transport = try await transport("bounded")
        let agents = try await transport.roster()
        let metrics = MetadataProtocol.metrics.snapshot()
        XCTAssertEqual(agents.count, 81)
        XCTAssertEqual(metrics.1 - before, 0, "Roster does not wait for per-Agent metadata")
        XCTAssertTrue(metrics.2.allSatisfy { $0 == "1" })
        XCTAssertNil(agents.last?.unreadCount)
        await transport.close()
    }

    func testOldCacheAndConservativeCounts() throws {
        let agent = try JSONDecoder().decode(ReadingAgent.self, from: Data(#"{"id":"A","name":"Agent","preview":"brief"}"#.utf8))
        XCTAssertNil(agent.operatorPreview)
        XCTAssertNil(agent.unreadCount)
        let raw = try JSONDecoder().decode(JSONValue.self, from: Data(#"[{"agent_id":"A","event_log_epoch":"epoch","visibility_scope_id":"wrong","unread_count":0,"reset_required":false,"retention_gap":false}]"#.utf8))
        XCTAssertTrue(try ReadingClientTransport.unreadCounts(raw, agentIDs: ["A"], epoch: "epoch", visibility: "private").isEmpty)
    }

    func testTurnsActiveAndPendingSelectLatestStructuredOperator() throws {
        let raw = try JSONDecoder().decode(JSONValue.self, from: Data(#"{"turns":[{"presentation_class":"operator","started_at":"2026-01-01T00:00:00Z","inputs":[{"preview":"old"}]}],"active_turns":[{"presentation_class":"operator","started_at":"2026-01-02T00:00:00Z","inputs":[{"preview":"active"}]}],"pending_inputs":[{"presentation_class":"operator","created_at":"2026-01-03T00:00:00.123Z","preview":"{\"type\":\"text\",\"text\":\"new\"}"},{"presentation_class":"internal","created_at":"2026-01-04T00:00:00Z","preview":"hidden"}]}"#.utf8))
        XCTAssertEqual(ReadingClientTransport.operatorPreview(raw), "new")
    }

    func testMalformedReadMetadataCannotClaimRead() throws {
        for changes: [String: JSONValue] in [
            ["unread_count": .integer(-1)],
            ["unread_count": .string("0")],
            ["reset_required": .bool(true)],
            ["retention_gap": .bool(true)],
            ["event_log_epoch": .null],
            ["visibility_scope_id": .null]
        ] {
            var state: [String: JSONValue] = [
                "agent_id": .string("A"), "event_log_epoch": .string("epoch"),
                "visibility_scope_id": .string("private"), "unread_count": .integer(0),
                "reset_required": .bool(false), "retention_gap": .bool(false)
            ]
            state.merge(changes) { _, new in new }
            XCTAssertTrue(try ReadingClientTransport.unreadCounts(
                .array([.object(state)]), agentIDs: ["A"], epoch: "epoch", visibility: "private").isEmpty)
        }
    }

    func testTurnInputsUseRealStartedAtWithoutInventedCreatedAt() throws {
        let raw = try JSONDecoder().decode(JSONValue.self, from: Data(#"{"turns":[{"turn_id":"turn","key":{"turn_index":1},"presentation_class":"operator","started_at":"2026-01-01T00:00:00Z","inputs":[{"preview":"first"},{"preview":"latest"},{"presentation_class":"internal","preview":"hidden"}]}],"active_turns":[],"pending_inputs":[]}"#.utf8))
        XCTAssertEqual(ReadingClientTransport.operatorPreview(raw), "latest")
    }

    func testCompletedTurnDoesNotResurfaceItsOldOperatorInput() {
        let raw: JSONValue = .object(["turns": .array([.object([
            "turn_id": .string("turn"), "key": .object(["turn_index": .integer(1)]),
            "presentation_class": .string("operator"), "started_at": .string("2026-01-01T00:00:00Z"),
            "brief_ids": .array([.string("result")]), "inputs": .array([.object(["preview": .string("old")])])
        ])])])
        XCTAssertNil(ReadingClientTransport.operatorPreview(raw))
    }
    func testOlderTurnWithoutBriefCannotReplaceNewestCompletedResult() throws {
        let raw = try JSONDecoder().decode(JSONValue.self, from: Data(#"{"turns":[{"turn_id":"old","key":{"turn_index":1},"presentation_class":"operator","started_at":"2026-01-01T00:00:00Z","brief_ids":[],"inputs":[{"preview":"old"}]},{"turn_id":"new","key":{"turn_index":2},"brief_ids":["brief"],"inputs":[]}],"pending_inputs":[]}"#.utf8))
        XCTAssertNil(ReadingClientTransport.operatorPreview(raw))
    }
}
