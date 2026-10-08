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
        let list = try await sdk.workItems(agentID: environment("HOLON_UI_AGENT_ID"), limit: 100)
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

    func testPopulatedActivityWireContract() async throws {
        guard let turn = ProcessInfo.processInfo.environment["HOLON_UI_RICH_TURN_ID"] else {
            throw XCTSkip("Requires the isolated rich activity fixture")
        }
        let sdk = try await client()
        let agent = try environment("HOLON_UI_AGENT_ID")
        let snapshot = try await sdk.conversation(agentID: agent)
        let page = try await sdk.conversationActivities(agentID: agent, turnID: turn)
        XCTAssertEqual(page.value["runtime_id"], .string(snapshot.value.runtimeID))
        XCTAssertEqual(page.value["event_log_epoch"], .string(snapshot.value.eventLogEpoch))
        XCTAssertEqual(page.value["visibility_scope_id"], .string(snapshot.value.visibilityScopeID))
        XCTAssertEqual(page.value["schema_version"], snapshot.value.raw["schema_version"])
        XCTAssertEqual(page.value["query_version"], snapshot.value.raw["query_version"])
        XCTAssertEqual(page.value["turn"]?["turn_id"], .string(turn))
        XCTAssertEqual(page.value["has_more"], .bool(true))
        guard case .array(let items) = page.value["activities"],
              case .integer(let revision) = page.value["detail_revision"],
              case .string(let before) = page.value["next_before_cursor"] else {
            return XCTFail("Activity page omitted typed pagination metadata")
        }
        XCTAssertEqual(items.count, 60); XCTAssertGreaterThanOrEqual(revision, 0)
        XCTAssertFalse(before.isEmpty)
        for item in items {
            guard case .string(let id) = item["id"], case .string = item["kind"],
                  case .integer(let seq) = item["key"]?["event_seq"],
                  case .integer(let revision) = item["revision"],
                  case .string = item["summary"] else {
                return XCTFail("Activity item omitted typed identity, key or summary")
            }
            XCTAssertEqual(item["key"]?["activity_id"], .string(id))
            XCTAssertGreaterThanOrEqual(seq, 0); XCTAssertGreaterThanOrEqual(revision, 0)
        }
        let older = try await sdk.conversationActivities(agentID: agent, turnID: turn, before: before)
        XCTAssertEqual(older.value["detail_revision"], page.value["detail_revision"])
        XCTAssertNotEqual(older.value["next_before_cursor"], .string(before))
        XCTAssertTrue(try text(older.value).contains("IOS_RICH_ASSISTANT: read-only inspection batch 1."))
    }
}
