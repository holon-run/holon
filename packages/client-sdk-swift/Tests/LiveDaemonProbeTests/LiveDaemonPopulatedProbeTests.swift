import Foundation
import HolonClient
import XCTest

@MainActor
final class LiveDaemonPopulatedProbeTests: XCTestCase {
    private func environment(_ key: String) throws -> String {
        try XCTUnwrap(ProcessInfo.processInfo.environment[key], "Missing isolated fixture \(key)")
    }
    @MainActor private func client() async throws -> HolonClient {
        guard ProcessInfo.processInfo.environment["HOLON_UI_BASE_URL"] != nil else {
            throw XCTSkip("Run scripts/test-ios-ui.sh to start the isolated populated daemon")
        }
        let endpoint = try HolonEndpoint(apiBaseURL: XCTUnwrap(URL(string: environment("HOLON_UI_BASE_URL"))))
        let client = try HolonClient(endpoint: endpoint, networkID: "populated")
        // One session is shared between probes; the ticket is single-use.
        if let credential = Self.credential {
            try await client.bindIdentity(runtimeID: nil, userID: nil, visibilityScopeID: nil, credential: credential)
        } else {
            let login = try await client.redeemPairingTicket(ticket: environment("HOLON_UI_PAIRING_TICKET"))
            let credential = try XCTUnwrap(login.value.credential)
            Self.credential = credential
            try await client.bindIdentity(runtimeID: nil, userID: login.value.userId, visibilityScopeID: nil, credential: credential)
        }
        return client
    }
    @MainActor private static var credential: String?
    private func text(_ value: JSONValue) throws -> String {
        String(decoding: try JSONEncoder().encode(value), as: UTF8.self)
    }
    func testPopulatedWorkDetailAndPlan() async throws {
        let sdk = try await client()
        let id = try environment("HOLON_UI_WORK_ID")
        let list = try await sdk.workItems(agentID: environment("HOLON_UI_AGENT_ID"))
        XCTAssertTrue(try text(list.value).contains(id))
        let detail = try await sdk.workItem(agentID: environment("HOLON_UI_AGENT_ID"), workItemID: id)
        XCTAssertTrue(try text(detail.value).contains("IOS_POPULATED_PLAN"))
        XCTAssertTrue(try text(detail.value).contains("completed"))
        let planPath = try XCTUnwrap(detail.value["plan_artifact"]?["path"])
        guard case .string(let path) = planPath else { return XCTFail("Missing real plan path") }
        let resolved = try await sdk.resolveFileReference(.absolutePath(path))
        guard case .array(let results) = resolved.value["results"],
              let location = results.first?["location"],
              case .string(let workspace) = location["workspace_id"],
              case .string(let relative) = location["path"],
              case .string(let root) = location["execution_root_id"] else {
            return XCTFail("Plan did not resolve to a real workspace file")
        }
        let fullPlan = try await sdk.downloadWorkspaceFile(
            workspaceID: workspace, path: relative, executionRootID: root)
        XCTAssertTrue(String(decoding: fullPlan.value.data, as: UTF8.self).contains("IOS_POPULATED_FULL_PLAN"))
    }
    func testPopulatedWorkspaceFile() async throws {
        let sdk = try await client()
        let workspace = try environment("HOLON_UI_WORKSPACE_ID")
        let path = try environment("HOLON_UI_FILE_PATH")
        let listing = try await sdk.browseWorkspaceDirectory(workspaceID: workspace)
        XCTAssertTrue(try text(listing.value).contains(path))
        let file = try await sdk.downloadWorkspaceFile(workspaceID: workspace, path: path)
        XCTAssertEqual(String(decoding: file.value.data, as: UTF8.self), "IOS_POPULATED_FILE\n")
    }
    func testPopulatedTaskAndOutput() async throws {
        let sdk = try await client()
        let id = try environment("HOLON_UI_TASK_ID")
        let list = try await sdk.tasks(agentID: environment("HOLON_UI_AGENT_ID"))
        XCTAssertTrue(try text(list.value).contains(id))
        let detail = try await sdk.task(agentID: environment("HOLON_UI_AGENT_ID"), taskID: id)
        XCTAssertTrue(try text(detail.value).contains("command_task"))
        let output = try await sdk.taskOutput(agentID: environment("HOLON_UI_AGENT_ID"), taskID: id)
        XCTAssertTrue(try text(output.value).contains("IOS_POPULATED_OUTPUT"))
    }
    func testPopulatedBrief() async throws {
        let sdk = try await client()
        let conversation = try await sdk.conversation(agentID: environment("HOLON_UI_AGENT_ID"))
        XCTAssertTrue(try text(conversation.value.raw).contains("IOS_POPULATED_BRIEF"))
    }
}
