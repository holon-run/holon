import Foundation
import HolonClient
import XCTest
@testable import Holon

private final class WorkIdentityProtocol: URLProtocol, @unchecked Sendable {
    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func startLoading() {
        let response = HTTPURLResponse(url: request.url!, statusCode: 200, httpVersion: nil,
                                       headerFields: ["Content-Type": "application/json"])!
        client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
        let path = request.url!.path
        let body: String
        if path.hasSuffix("/work-items") { body = #"[{"id":"work","state":"queued"}]"# }
        else if path.hasSuffix("/tasks") { body = #"[{"id":"task","status":"running"}]"# }
        else if path.hasSuffix("/output") {
            body = #"{"retrieval_status":"success","task":{"task_id":"task","status":"running","output_preview":"partial","output_truncated":true}}"#
        } else if path.contains("/work-items/") { body = #"{"id":"work","state":"queued"}"# }
        else { body = #"{"task_id":"task","status":"running"}"# }
        client?.urlProtocol(self, didLoad: Data(body.utf8))
        client?.urlProtocolDidFinishLoading(self)
    }
    override func stopLoading() {}
}

final class WorkTransportTests: XCTestCase {
    private func fixture() async throws -> (HolonClient, WorkClientTransport) {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.protocolClasses = [WorkIdentityProtocol.self]
        let client = try HolonClient(
            endpoint: HolonEndpoint(apiBaseURL: URL(string: "https://work.invalid/api")!),
            networkID: "network", configuration: configuration)
        try await client.bindIdentity(runtimeID: "runtime", userID: "user",
                                      visibilityScopeID: "private", credential: nil)
        let authority = HolonConnectionIdentity(networkID: "network", runtimeID: "runtime",
                                                userID: "user", visibilityScopeID: "private")
        return (client, WorkClientTransport(client: client, authority: authority))
    }

    func testIndependentSDKGenerationLoadsSeparateResources() async throws {
        let (_, transport) = try await fixture()
        let items = try await transport.items(agentID: "A")
        let tasks = try await transport.tasks(agentID: "A")
        let output = try await transport.output(agentID: "A", id: "task")
        XCTAssertEqual(items.map(\.id), ["work"])
        XCTAssertEqual(tasks.map(\.id), ["task"])
        XCTAssertTrue(output.truncated)
        XCTAssertEqual(output.text, "partial")
        await transport.close()
    }

    func testSamePartitionSDKRebindInvalidatesOldTransport() async throws {
        let (client, transport) = try await fixture()
        _ = try await transport.items(agentID: "A")
        try await client.bindIdentity(runtimeID: "runtime", userID: "user",
                                      visibilityScopeID: "private", credential: nil)
        do {
            _ = try await transport.tasks(agentID: "A")
            XCTFail("An old Work transport must not adopt a rebound generation")
        } catch is CancellationError {}
        await transport.close()
    }

    func testCrossNetworkUserRuntimeOrVisibilityCannotRead() async throws {
        for changed in ["network", "user", "runtime", "visibility"] {
            let (client, _) = try await fixture()
            let authority = HolonConnectionIdentity(
                networkID: changed == "network" ? "other" : "network",
                runtimeID: changed == "runtime" ? "other" : "runtime",
                userID: changed == "user" ? "other" : "user",
                visibilityScopeID: changed == "visibility" ? "other" : "private")
            let transport = WorkClientTransport(client: client, authority: authority)
            do {
                _ = try await transport.items(agentID: "A")
                XCTFail("Cross-partition Work reads must fail")
            } catch is CancellationError {}
            await transport.close()
        }
    }
}
