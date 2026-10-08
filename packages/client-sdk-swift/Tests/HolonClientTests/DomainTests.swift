import Foundation
import HolonClient
import XCTest

final class DomainTests: XCTestCase {
    func testServerFileLocationRetainsCompleteRelativeReferenceBaseAndRejectsUnknownRoots() throws {
        let fields: [String: JSONValue] = ["workspace_id": .string("w"), "execution_root_id": .string("r"),
            "path": .string("docs/readme.md"), "absolute_path": .string("/host/docs/readme.md"),
            "kind": .string("file"), "root_kind": .string("git_worktree_root")]
        let location = try HolonFileLocation(raw: .object(fields))
        XCTAssertEqual(location.payload, .object(fields))
        for (key, value) in [("root_kind", "future-root"), ("path", "../readme.md"),
                             ("kind", "directory"), ("absolute_path", "relative.md"), ("execution_root_id", "")] {
            var invalid = fields; invalid[key] = .string(value)
            XCTAssertThrowsError(try HolonFileLocation(raw: .object(invalid)))
        }
    }
    private func fixture(_ name: String) throws -> Data {
        let root = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent().deletingLastPathComponent()
            .deletingLastPathComponent().deletingLastPathComponent()
            .deletingLastPathComponent()
        return try Data(contentsOf: root.appendingPathComponent("tests/fixtures/client-wire/\(name).json"))
    }

    private func modified(_ name: String, _ change: (inout [String: Any]) -> Void) throws -> Data {
        var object = try XCTUnwrap(JSONSerialization.jsonObject(with: fixture(name)) as? [String: Any])
        change(&object)
        return try JSONSerialization.data(withJSONObject: object)
    }

    private func assertSendable<T: Sendable & Equatable>(_ value: T) {
        XCTAssertEqual(value, value)
    }

    func testHandshakeCompatibilityAndLimits() throws {
        let handshake = try HolonHandshake(data: fixture("handshake-v1"))
        assertSendable(handshake)
        XCTAssertEqual(handshake.checkCompatibility(), .compatible(server: handshake.server))
        XCTAssertEqual(handshake.server.authMode, "bearer")
        XCTAssertNil(handshake.server.limits)
        XCTAssertEqual(handshake.checkCompatibility(requiredCapabilities: ["agents.list", "future"]),
                       .missingCapabilities(["future"]))
        let rejected = try HolonHandshake(data: modified("handshake-v1") { $0["ok"] = false })
        XCTAssertEqual(rejected.checkCompatibility(requiredCapabilities: ["missing"]), .rejectedHandshake)
        let future = try HolonHandshake(data: modified("handshake-v1") {
            $0["protocol"] = ["name": "future-control", "version": 2]
            $0["auth"] = ["mode": "future-auth", "required": false]
            $0["limits"] = ["prompt_body_max_bytes": 123, "prompt_file_attachment_max_bytes": 456,
                            "prompt_image_attachment_max_bytes": 789]
        })
        XCTAssertEqual(future.checkCompatibility(), .unsupportedProtocol(actualName: "future-control", actualVersion: 2))
        XCTAssertEqual(future.server.authMode, "future-auth")
        XCTAssertEqual(future.server.limits?.promptBodyMaxBytes, 123)
        XCTAssertEqual(future.server.limits?.promptFileAttachmentMaxBytes, 456)
        XCTAssertEqual(future.server.limits?.promptImageAttachmentMaxBytes, 789)
        for proto: [String: Any] in [
            ["name": "holon-control", "version": 2], ["name": "different", "version": 1],
        ] {
            let value = try HolonHandshake(data: modified("handshake-v1") { $0["protocol"] = proto })
            XCTAssertEqual(value.checkCompatibility(),
                           .unsupportedProtocol(actualName: proto["name"] as! String, actualVersion: proto["version"] as! Int))
        }
        XCTAssertThrowsError(try HolonHandshake(data: Data(#"{"ok":true}"#.utf8)))
    }

    func testSessionRetainsCredentialWithoutLoggingIt() throws {
        let session = try HolonSession(data: fixture("session-response-v1"))
        assertSendable(session)
        XCTAssertTrue(session.ok)
        XCTAssertEqual(session.userId, "local-static-token")
        XCTAssertEqual(session.credential, "session-credential")
        XCTAssertEqual(session.expiresAt, ISO8601DateFormatter().date(from: "2030-01-01T00:00:00Z"))
        XCTAssertFalse(String(describing: session).contains("session-credential"))
        XCTAssertFalse(String(reflecting: session).contains("session-credential"))
        let withoutCredential = try HolonSession(data: modified("session-response-v1") {
            $0.removeValue(forKey: "credential")
            $0.removeValue(forKey: "expires_at")
        })
        XCTAssertNil(withoutCredential.credential)
        XCTAssertNil(withoutCredential.expiresAt)
    }

    func testAllSharedRosterVariants() throws {
        let current = try HolonRoster(data: fixture("agent-list-v1"))
        assertSendable(current)
        XCTAssertEqual(current.agents[0].id, "main")
        XCTAssertEqual(current.agents[0].displayName, "Primary")
        XCTAssertTrue(current.agents[0].isDefault)
        XCTAssertEqual(current.agents[0].pending, 2)
        XCTAssertEqual(current.agents[0].raw["future_agent_field"], .bool(true))
        let future = try HolonRoster(data: fixture("agent-list-future-enums"))
        XCTAssertEqual(future.agents[0].registryStatus, "archived")
        XCTAssertEqual(future.agents[0].runtimeStatus, "suspended")
        XCTAssertEqual(future.agents[0].modelSource, "policy_selected")
        XCTAssertEqual(future.agents[0].displayName, "future")
        XCTAssertEqual(future.agents[0].pending, 0)
        XCTAssertEqual(future.agents[0].schedulingPosture, "unknown")
        let projections = try HolonRoster(data: fixture("agent-list-workspace-projections"))
        XCTAssertEqual(projections.agents.count, 2)
        XCTAssertEqual(projections.agents[0].executionRootId, "root-managed")
        XCTAssertEqual(projections.agents[0].workspaceId, "workspace-holon")
        XCTAssertEqual(projections.agents[0].workspaceProjectionKind, "git_worktree_root")
        XCTAssertEqual(projections.agents[0].workspaceProjectionMetadata?["future_metadata_field"], .bool(true))
        XCTAssertEqual(projections.agents[1].workspaceProjectionMetadata?["worktree_root"], .string("/worktrees/existing"))
        XCTAssertEqual(projections.agents[1].workspaceProjectionMetadata?["future_metadata_field"], .string("retained"))
    }

    func testFlatAndFutureErrorsAreLossless() throws {
        let error = try HolonAPIError(data: fixture("error-v1"))
        assertSendable(error)
        XCTAssertEqual(error.code, "invalid_json")
        XCTAssertEqual(error.domain, "http")
        XCTAssertEqual(error.detail, "unknown field `kind`")
        XCTAssertFalse(error.retryable)
        XCTAssertEqual(error.extensions["endpoint_specific_field"], .object(["ignored": .bool(true)]))
        let future = try HolonAPIError(data: fixture("error-future-domain"))
        XCTAssertEqual(future.domain, "scheduler_v2")
        XCTAssertFalse(future.retryable)
        XCTAssertEqual(future.context, [:])
        let encoded = try JSONEncoder().encode(error.raw)
        XCTAssertEqual(try JSONDecoder().decode(JSONValue.self, from: encoded), error.raw)
    }

    func testOnlyRecognizedAuthenticationFailuresRenewSession() throws {
        for code in ["auth_required", "invalid_static_token", "pairing_invalid_or_expired",
                     "session_invalid_or_expired", "session_expired_or_revoked", "session_user_disabled"] {
            let error = try HolonAPIError(data: modified("error-v1") { $0["code"] = code })
            XCTAssertTrue(error.requiresSessionRenewal(statusCode: 401))
            XCTAssertFalse(error.requiresSessionRenewal(statusCode: 403))
            XCTAssertFalse(error.requiresSessionRenewal(statusCode: 500))
        }
        XCTAssertTrue(HolonAPIError.requiresSessionRenewal(statusCode: 401, code: nil))
        XCTAssertFalse(HolonAPIError.requiresSessionRenewal(statusCode: 403, code: nil))
        XCTAssertFalse(HolonAPIError.requiresSessionRenewal(statusCode: 401, code: "future_error"))
    }

    func testCurrentUserDecodingPreservesFutureFields() throws {
        let user = try HolonCurrentUser(data: Data(#"{"ok":true,"user_id":"local","display_name":"Local User","auth_method":"future_method","future_field":true}"#.utf8))
        assertSendable(user)
        XCTAssertTrue(user.ok)
        XCTAssertEqual(user.userId, "local")
        XCTAssertEqual(user.displayName, "Local User")
        XCTAssertEqual(user.authMethod, "future_method")
        XCTAssertEqual(user.raw["future_field"], .bool(true))
        for displayName in ["", #","display_name":null"#] {
            let unnamed = try HolonCurrentUser(data: Data("""
            {"ok":true,"user_id":"local","auth_method":"session"\(displayName)}
            """.utf8))
            XCTAssertNil(unnamed.displayName)
        }
        XCTAssertThrowsError(try HolonCurrentUser(data: Data(#"{"ok":true,"user_id":"local"}"#.utf8)))
    }
}
