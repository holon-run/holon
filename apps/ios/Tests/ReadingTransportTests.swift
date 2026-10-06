import Foundation
import HolonClient
import XCTest
@testable import Holon

private final class ReadingIdentityProtocol: URLProtocol, @unchecked Sendable {
    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func startLoading() {
        let response = HTTPURLResponse(url: request.url!, statusCode: 200,
                                       httpVersion: nil, headerFields: ["Content-Type": "application/json"])!
        client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
        let body: String
        if request.url!.path.hasSuffix("/agents/snapshot") {
            body = #"{"runtime_id":"runtime","visibility_scope_id":"private","event_log_epoch":"epoch","agents":[{"agent":{"identity":{"agent_id":"A","name":"Agent A"}}}]}"#
        } else if request.url!.path.hasSuffix("/conversation") {
            body = #"{"runtime_id":"runtime","visibility_scope_id":"private","agent_id":"A","event_log_epoch":"epoch","snapshot_cursor":"live-1","snapshot_through_seq":7,"event_head_seq":7,"schema_version":1,"query_version":1,"has_more":false,"next_before_cursor":null,"turns":[],"active_turns":[],"pending_inputs":[]}"#
        } else {
            body = #"{"brief_id":"brief"}"#
        }
        client?.urlProtocol(self, didLoad: Data(body.utf8))
        client?.urlProtocolDidFinishLoading(self)
    }
    override func stopLoading() {}
}

final class ReadingTransportTests: XCTestCase {
    private func fixture() async throws -> (HolonClient, ReadingClientTransport, HolonConnectionIdentity) {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.protocolClasses = [ReadingIdentityProtocol.self]
        let client = try HolonClient(
            endpoint: HolonEndpoint(apiBaseURL: URL(string: "https://reading.invalid/api")!),
            networkID: "network", configuration: configuration)
        try await client.bindIdentity(runtimeID: "runtime", userID: "user",
                                      visibilityScopeID: "private", credential: nil)
        let authority = HolonConnectionIdentity(networkID: "network", runtimeID: "runtime",
                                                userID: "user", visibilityScopeID: "private")
        return (client, ReadingClientTransport(client: client, authority: authority), authority)
    }

    func testIndependentSDKGenerationCanRead() async throws {
        let (client, transport, authority) = try await fixture()
        let sdkIdentity = await client.identity
        XCTAssertNotEqual(sdkIdentity.generation, authority.generation)
        let agents = try await transport.roster()
        XCTAssertEqual(agents.map(\.id), ["A"])
        let snapshot = try await transport.conversation(agentID: "A", before: nil)
        XCTAssertEqual(snapshot.eventLogEpoch, "epoch")
        let brief = try await transport.brief(agentID: "A", briefID: "brief")
        XCTAssertEqual(brief["brief_id"]?.readingString, "brief")
        await transport.close()
    }

    func testReboundSDKGenerationCannotReadThroughOldTransport() async throws {
        let (client, transport, _) = try await fixture()
        _ = try await transport.brief(agentID: "A", briefID: "brief")
        try await client.bindIdentity(runtimeID: "runtime", userID: "user",
                                      visibilityScopeID: "private", credential: nil)
        do {
            _ = try await transport.brief(agentID: "A", briefID: "brief")
            XCTFail("An old transport must not adopt a new SDK generation")
        } catch is CancellationError {
            // Same business partition does not authorize adopting a new generation.
        }
        await transport.close()
    }
}
