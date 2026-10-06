import Foundation
import HolonClient
import XCTest
@testable import Holon

private final class FilesIdentityProtocol: URLProtocol, @unchecked Sendable {
    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func startLoading() {
        let root = URLComponents(url: request.url!, resolvingAgainstBaseURL: false)?
            .queryItems?.first { $0.name == "execution_root_id" }?.value
        let status = root == "denied" ? 403 : 200
        let body: String
        if request.url!.path.hasSuffix("/state") {
            body = #"{"workspace":{"workspaces":[{"workspace_id":"ws","execution_root_id":"source-root","repo_name":"Source"}]}}"#
        } else if status == 403 {
            body = #"{"error":{"code":"forbidden","message":"private"}}"#
        } else {
            body = #"{"type":"directory","workspace_id":"ws","execution_root_id":"source-root","path":"","entries":[{"name":"README.md","type":"file"},{"name":"docs","type":"directory"}]}"#
        }
        let response = HTTPURLResponse(url: request.url!, statusCode: status, httpVersion: nil,
                                       headerFields: ["Content-Type": "application/json"])!
        client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: Data(body.utf8))
        client?.urlProtocolDidFinishLoading(self)
    }
    override func stopLoading() {}
}

private final class FilesPlanProtocol: URLProtocol, @unchecked Sendable {
    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func startLoading() {
        let url = request.url!
        let query = URLComponents(url: url, resolvingAgainstBaseURL: false)?.queryItems ?? []
        let root = query.first { $0.name == "execution_root_id" }?.value
        let download = query.first { $0.name == "download" }?.value == "true"
        let status: Int
        let body: String
        let type: String
        if url.path.hasSuffix("/state") {
            // Real state DTO: only the active project has execution-root information.
            status = 200
            type = "application/json"
            body = """
            {"workspace":{"workspaces":[
              {"workspace_id":"project","execution_root_id":"active-worktree","is_active":true},
              {"workspace_id":"agent_home:A","workspace_alias":"agent_home","execution_root_id":null,"is_active":false}
            ]}}
            """
        } else if download {
            status = root == "canonical_root:agent_home:A" &&
                url.path.hasSuffix("/work-items/work/plan.md") ? 200 : 409
            type = "text/markdown"
            body = "# Full plan"
        } else {
            status = root == nil ? 200 : 409
            type = "application/json"
            body = """
            {"type":"directory","workspace_id":"agent_home:A",
             "execution_root_id":"canonical_root:agent_home:A","path":"",
             "entries":[{"name":"work-items","type":"directory"}]}
            """
        }
        let response = HTTPURLResponse(url: url, statusCode: status, httpVersion: nil,
                                       headerFields: ["Content-Type": type])!
        client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: Data(body.utf8))
        client?.urlProtocolDidFinishLoading(self)
    }
    override func stopLoading() {}
}

final class FilesTransportTests: XCTestCase {
    private static func fixture(protocolClass: URLProtocol.Type = FilesIdentityProtocol.self)
        async throws -> (HolonClient, FilesClientTransport, HolonConnectionIdentity) {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.protocolClasses = [protocolClass]
        let client = try HolonClient(
            endpoint: HolonEndpoint(apiBaseURL: URL(string: "https://files.invalid/api")!),
            networkID: "network", configuration: configuration)
        try await client.bindIdentity(runtimeID: "runtime", userID: "user",
                                      visibilityScopeID: "private", credential: nil)
        let authority = HolonConnectionIdentity(networkID: "network", runtimeID: "runtime",
                                                userID: "user", visibilityScopeID: "private")
        return (client, FilesClientTransport(client: client, authority: authority), authority)
    }

    func testIndependentGenerationAndDirectoryKeepOriginalRoot() async throws {
        let (client, transport, authority) = try await Self.fixture()
        let sdkIdentity = await client.identity
        XCTAssertNotEqual(sdkIdentity.generation, authority.generation)
        let workspaces = try await transport.workspaces(agentID: "A")
        let workspace = try XCTUnwrap(workspaces.first)
        let directory = try await transport.directory(workspace: workspace, path: "")
        XCTAssertEqual(directory.workspace.executionRootID, "source-root")
        XCTAssertEqual(directory.entries.map(\.path), ["README.md", "docs"])
        await transport.close()
    }

    @MainActor
    func testRealInactiveHomeDTOOpensFullPlanUsingDirectoryRoot() async throws {
        let (_, transport, authority) = try await Self.fixture(protocolClass: FilesPlanProtocol.self)
        let coordinator = FilesCoordinator()
        coordinator.activate(transport: transport, identity: authority)
        coordinator.selectAgent("A")
        await settle(coordinator)
        XCTAssertEqual(coordinator.workspaces.map(\.executionRootID), ["active-worktree", nil])
        let plan: JSONValue = .object([
            "owner_agent_id": .string("A"), "workspace_id": .string("agent_home:A"),
            "workspace_alias": .string("agent_home"),
            "relative_path": .string("work-items/work/plan.md"),
            "path": .string("/server/home/work-items/work/plan.md")
        ])
        XCTAssertTrue(coordinator.openPlan(agentID: "A", workID: "work", plan: plan))
        await settle(coordinator)
        XCTAssertNil(coordinator.failure)
        XCTAssertEqual(coordinator.prepared?.text, "# Full plan")
        coordinator.disconnect()
    }

    @MainActor
    private func settle(_ coordinator: FilesCoordinator) async {
        for _ in 0..<200 {
            if !coordinator.isLoading { return }
            try? await Task.sleep(for: .milliseconds(10))
        }
        XCTFail("Files operation did not settle")
    }

    func testSameUserSDKRebindCannotBeAdoptedByOldTransport() async throws {
        let (client, transport, _) = try await Self.fixture()
        try await transport.validate()
        try await client.bindIdentity(runtimeID: "runtime", userID: "user",
                                      visibilityScopeID: "private", credential: nil)
        do {
            _ = try await transport.workspaces(agentID: "A")
            XCTFail("An old Files transport may not adopt a new SDK generation")
        } catch is CancellationError {}
        await transport.close()
    }

    func testHTTPFailureIsReboundToConnectionAuthority() async throws {
        let (_, transport, authority) = try await Self.fixture()
        do {
            _ = try await transport.directory(
                workspace: FilesWorkspace(workspaceID: "ws", executionRootID: "denied", name: "denied"), path: "")
            XCTFail("Expected forbidden")
        } catch let http as HolonHTTPFailure {
            XCTAssertEqual(http.statusCode, 403)
            XCTAssertEqual(http.identity, authority)
        }
        await transport.close()
    }

    func testResolverMustSupplySourceRootRatherThanFilenameFallback() {
        let raw: JSONValue = .object(["results": .array([.object([
            "status": .string("resolved"), "location": .object([
                "workspace_id": .string("ws"), "path": .string("plan.md"), "kind": .string("file")
            ])
        ])])])
        XCTAssertThrowsError(try FilesClientTransport.resolved(raw)) {
            XCTAssertEqual($0 as? FilesFailure, .invalidReference)
        }
    }
}
