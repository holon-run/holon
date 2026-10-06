import Foundation
import HolonClient
import HolonWire
import XCTest

final class WireContractTests: XCTestCase {
    private func fixture(_ name: String) throws -> Data {
        let root = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent().deletingLastPathComponent()
            .deletingLastPathComponent().deletingLastPathComponent()
            .deletingLastPathComponent()
        return try Data(contentsOf: root.appendingPathComponent("tests/fixtures/client-wire/\(name).json"))
    }

    func testHandshakeRetainsUnknownFields() throws {
        let document = try WireDocument<HandshakeResponse>(data: fixture("handshake-v1"))
        XCTAssertEqual(document.model._protocol.name, "holon-control")
        XCTAssertEqual(document.model._protocol.version, 1)
        XCTAssertEqual(document.raw["future_handshake_field"]?["version"], .integer(2))
        let raw = try JSONEncoder().encode(document.raw)
        XCTAssertEqual(try JSONDecoder().decode(JSONValue.self, from: raw), document.raw)
    }

    func testFutureAgentEnumsDecodeAndRetainOriginalValues() throws {
        let document = try WireDocument<[AgentListEntry]>(data: fixture("agent-list-future-enums"))
        XCTAssertEqual(document.model.count, 1)
        XCTAssertEqual(document.model[0].identity.status, .unknownDefaultOpenApi)
        XCTAssertEqual(document.model[0].status, .unknownDefaultOpenApi)
        guard case .array(let agents) = document.raw else { return XCTFail("Expected roster") }
        XCTAssertEqual(agents[0]["identity"]?["status"], .string("archived"))
        XCTAssertEqual(agents[0]["model"]?["source"], .string("policy_selected"))
        XCTAssertEqual(agents[0]["status"], .string("suspended"))
    }

    func testErrorFutureDomainIsLossless() throws {
        let document = try WireDocument<ModelErrorResponse>(data: fixture("error-future-domain"))
        XCTAssertEqual(document.model.code, "future_error")
        XCTAssertEqual(document.model.domain, .unknownDefaultOpenApi)
        XCTAssertEqual(document.raw["domain"], .string("scheduler_v2"))
    }

    func testRequiredFieldsStillFailValidation() {
        XCTAssertThrowsError(try WireDocument<HandshakeResponse>(data: Data(#"{"ok":true}"#.utf8)))
    }

    func testCurrentRosterAndFlatErrorFixtures() throws {
        let roster = try WireDocument<AgentListResponse>(data: fixture("agent-list-v1"))
        XCTAssertFalse(roster.model.isEmpty)
        let error = try WireDocument<ModelErrorResponse>(data: fixture("error-v1"))
        XCTAssertFalse(error.model.ok)
        XCTAssertEqual(error.raw["code"], .string(error.model.code))
    }

    func testRosterDecodesBothWorkspaceProjectionVariants() throws {
        let roster = try WireDocument<AgentListResponse>(data: fixture("agent-list-workspace-projections"))
        XCTAssertEqual(roster.model.count, 2)
        guard case .managedWorktreeProjectionMetadata(let managed) =
                roster.model[0].activeWorkspaceEntry?.projectionMetadata,
              case .existingGitWorktreeProjectionMetadata(let existing) =
                roster.model[1].activeWorkspaceEntry?.projectionMetadata else {
            return XCTFail("Expected distinct managed and existing worktree variants")
        }
        XCTAssertEqual(managed.originalBranch, "main")
        XCTAssertEqual(managed.originalCwd, "/repos/holon")
        XCTAssertEqual(managed.worktreeBranch, "feat/client")
        XCTAssertEqual(managed.worktreePath, "/worktrees/managed")
        XCTAssertEqual(existing.worktreeRoot, "/worktrees/existing")
        guard case .array(let rawAgents) = roster.raw else { return XCTFail("Expected roster") }
        XCTAssertEqual(rawAgents[0]["active_workspace_entry"]?["projection_metadata"]?["future_metadata_field"],
                       .bool(true))
        XCTAssertEqual(rawAgents[1]["active_workspace_entry"]?["projection_metadata"]?["future_metadata_field"],
                       .string("retained"))
        for (agent, raw) in zip(roster.model, rawAgents) {
            let encoded = try JSONEncoder().encode(XCTUnwrap(agent.activeWorkspaceEntry?.projectionMetadata))
            let actual = try JSONDecoder().decode(JSONValue.self, from: encoded)
            let expected = try XCTUnwrap(raw["active_workspace_entry"]?["projection_metadata"])
            guard case .object(var fields) = expected else { return XCTFail("Expected metadata object") }
            fields.removeValue(forKey: "future_metadata_field")
            XCTAssertEqual(actual, .object(fields))
            XCTAssertNoThrow(try JSONDecoder().decode(WorkspaceProjectionMetadata.self, from: encoded))
        }
    }

    func testWorkspaceProjectionVariantsRejectMissingOrInvalidRequiredFields() throws {
        for json in [
            #"{}"#,
            #"{"original_branch":"main","original_cwd":"/repos/holon","worktree_branch":"feat/client"}"#,
            #"{"worktree_root":null}"#,
            #"{"worktree_root":42}"#,
        ] {
            XCTAssertThrowsError(try JSONDecoder().decode(WorkspaceProjectionMetadata.self, from: Data(json.utf8)))
        }
    }

    func testSessionDateAndOpenCredentialField() throws {
        let session = try WireDocument<SessionResponse>(data: fixture("session-response-v1"))
        XCTAssertEqual(session.model.userId, "local-static-token")
        XCTAssertNotNil(session.model.expiresAt)
        XCTAssertEqual(session.raw["credential"], .string("session-credential"))
    }
}
