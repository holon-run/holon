import Foundation
import HolonClient
import XCTest
@testable import Holon

private final class AvatarIdentityProtocol: URLProtocol, @unchecked Sendable {
    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func startLoading() {
        let host = request.url!.host!
        let handshake = request.url!.lastPathComponent == "handshake"
        let status = host == "denied.test" && !handshake ? 403 : 200
        let body: Data
        if handshake {
            body = Data("""
            {"ok":true,"protocol":{"name":"holon-control","version":\(host == "future.test" ? 2 : 1)},
             "auth":{"mode":"bearer","required":true},
             "capabilities":\(host == "unsupported.test" ? "[]" : "[\"agent_avatar_v1\"]"),
             "runtime":{"default_agent":"main","home_dir":"/home","workspace_dir":"/work","listen":"127.0.0.1:8787"}}
            """.utf8)
        } else {
            body = status == 403 ? Data(#"{"error":{"code":"forbidden","message":"Denied"}}"#.utf8) : Data([1, 2, 3])
        }
        let response = HTTPURLResponse(url: request.url!, statusCode: status, httpVersion: nil,
                                       headerFields: ["Content-Type": handshake || status != 200 ? "application/json" : "image/png"])!
        client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: body)
        client?.urlProtocolDidFinishLoading(self)
    }
    override func stopLoading() {}
}

final class AgentAvatarTransportTests: XCTestCase {
    private func fixture(host: String = "supported.test") async throws
        -> (HolonClient, AgentAvatarClientTransport, HolonConnectionIdentity) {
        let config = URLSessionConfiguration.ephemeral
        config.protocolClasses = [AvatarIdentityProtocol.self]
        let client = try HolonClient(endpoint: .init(apiBaseURL: URL(string: "https://\(host)/api")!),
                                     networkID: "network", configuration: config)
        _ = try await client.bindIdentity(runtimeID: "runtime", userID: "user", visibilityScopeID: "private", credential: "test")
        let authority = HolonConnectionIdentity(networkID: "network", runtimeID: "runtime",
                                                userID: "user", visibilityScopeID: "private")
        return (client, AgentAvatarClientTransport(client: client, authority: authority), authority)
    }

    func testCapabilityComesFromAuthenticatedCompatibleHandshake() async throws {
        for (host, expected) in [("supported.test", true), ("unsupported.test", false), ("future.test", false)] {
            let (client, transport, _) = try await fixture(host: host)
            let supported = try await transport.supportsAvatars()
            XCTAssertEqual(supported, expected)
            await client.close()
        }
    }

    func testIndependentSDKGenerationIsAcceptedButRebindingEvenSameIdentityIsNot() async throws {
        let (client, transport, authority) = try await fixture()
        let sdkIdentity = await client.identity
        XCTAssertNotEqual(sdkIdentity.generation, authority.generation)
        let supported = try await transport.supportsAvatars()
        XCTAssertTrue(supported)
        let bytes = try await transport.image(agentID: "stable-id", expression: .focused)
        XCTAssertEqual(bytes, Data([1, 2, 3]))
        _ = try await client.bindIdentity(runtimeID: "runtime", userID: "user", visibilityScopeID: "private", credential: "new")
        do {
            _ = try await transport.image(agentID: "stable-id", expression: .neutral)
            XCTFail("The prior SDK generation must not be reused")
        } catch is CancellationError {} catch { XCTFail("Unexpected error: \(error)") }
        await client.close()
    }

    func testAuthorizationFailureIsMappedBackToUIAuthority() async throws {
        let (client, transport, authority) = try await fixture(host: "denied.test")
        _ = try await transport.supportsAvatars()
        do {
            _ = try await transport.image(agentID: "private", expression: .neutral)
            XCTFail("Expected denial")
        } catch let failure as HolonHTTPFailure {
            XCTAssertEqual(failure.statusCode, 403)
            XCTAssertEqual(failure.identity, authority)
        }
        await client.close()
    }

    func testAnotherUserCannotStartAvatarNegotiation() async throws {
        let (client, _, authority) = try await fixture()
        _ = try await client.bindIdentity(runtimeID: "runtime", userID: "another", visibilityScopeID: "private", credential: "test")
        let transport = AgentAvatarClientTransport(client: client, authority: authority)
        do {
            _ = try await transport.supportsAvatars()
            XCTFail("Mismatched authorization must be fenced")
        } catch is CancellationError {} catch { XCTFail("Unexpected error: \(error)") }
        await client.close()
    }
}
