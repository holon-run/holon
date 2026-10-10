import Foundation
import HolonClient
import XCTest

@MainActor
final class TransportTests: XCTestCase {
    private func contentReport(description: String? = nil) throws -> HolonContentReportRequest {
        try HolonContentReportRequest(agentID: "main", turnID: "turn-1", messageID: "transcript-1",
                                      category: "privacy", description: description,
                                      clientRequestID: "B206BA23-AB7E-47DD-A644-543600B45123")
    }

    func testContentReportSuccessAndExplicitIdempotentRetry() async throws {
        let receipt = Data(#"{"report_id":"report-1","status":"accepted","created_at":"2026-10-09T16:00:00.000Z"}"#.utf8)
        let (sdk, exchange) = try client([MockReply(status: 201, body: receipt), MockReply(body: receipt)])
        let report = try contentReport(description: "Explanation")
        let first = try await sdk.createContentReport(report)
        let second = try await sdk.createContentReport(report)
        XCTAssertEqual(first.value, second.value)
        XCTAssertEqual(first.identity, second.identity)
        XCTAssertEqual(first.value.reportID, "report-1")
        XCTAssertEqual(first.value.status, "accepted")
        XCTAssertEqual(first.value.createdAt, "2026-10-09T16:00:00.000Z")
        XCTAssertEqual(exchange.requests[0].httpBody, exchange.requests[1].httpBody)
        let request = exchange.requests[0]
        XCTAssertEqual(request.httpMethod, "POST")
        XCTAssertEqual(request.url?.path, "/prefix/api/content-reports")
        XCTAssertEqual(request.value(forHTTPHeaderField: "Authorization"), "Bearer test-session")
        XCTAssertFalse(request.url!.absoluteString.contains("test-session"))
        XCTAssertFalse(String(decoding: request.httpBody!, as: UTF8.self).contains("test-session"))
        XCTAssertEqual(try JSONDecoder().decode(HolonContentReportRequest.self, from: request.httpBody!), report)
        XCTAssertEqual(try JSONDecoder().decode(JSONValue.self, from: request.httpBody!)["message_id"],
                       .string("transcript-1"))
    }

    func testContentReportFailuresAndLostResponseNeverRetry() async throws {
        for status in [401, 403, 404, 409, 429] {
            let (sdk, exchange) = try client([
                MockReply(status: status, body: Data(#"{"ok":false,"code":"report_error","error":"rejected","retryable":false}"#.utf8))])
            do { _ = try await sdk.createContentReport(contentReport()); XCTFail() }
            catch {
                XCTAssertEqual((error as? HolonHTTPFailure)?.statusCode, status)
                XCTAssertEqual((error as? HolonHTTPFailure)?.apiError?.code, "report_error")
            }
            XCTAssertEqual(exchange.requests.count, 1)
        }
        let receipt = Data(#"{"report_id":"r","status":"accepted","created_at":"2026-10-09"}"#.utf8)
        let (sdk, exchange) = try client([MockReply(error: .networkConnectionLost), MockReply(body: receipt)])
        let report = try contentReport()
        do { _ = try await sdk.createContentReport(report); XCTFail() }
        catch { XCTAssertTrue(error is URLError) }
        XCTAssertEqual(exchange.requests.count, 1)
        _ = try await sdk.createContentReport(report)
        XCTAssertEqual(exchange.requests[0].httpBody, exchange.requests[1].httpBody)
    }

    func testContentReportMalformedAndIdentityFence() async throws {
        for body in ["{}", "not json",
                     #"{"report_id":"","status":"accepted","created_at":"date"}"#,
                     #"{"report_id":"r","status":"rejected","created_at":"date"}"#] {
            let (sdk, exchange) = try client([MockReply(body: Data(body.utf8))])
            do { _ = try await sdk.createContentReport(contentReport()); XCTFail() }
            catch { XCTAssertEqual(error as? HolonClientError, .malformedResponse) }
            XCTAssertEqual(exchange.requests.count, 1)
        }
        let (sdk, exchange) = try client([MockReply(body: Data("{}".utf8), delay: 2)])
        let report = try contentReport()
        let pending = Task { try await sdk.createContentReport(report) }
        for _ in 0..<100 where exchange.requests.isEmpty { try await Task.sleep(for: .milliseconds(5)) }
        XCTAssertEqual(exchange.requests.count, 1)
        _ = try await sdk.bindIdentity(runtimeID: "new", userID: "new", visibilityScopeID: "new", credential: "new")
        do { _ = try await pending.value; XCTFail() }
        catch { XCTAssertEqual(error as? HolonClientError, .staleConnection) }
    }

    func testContentReportScalarLimitsAndDecodedValidation() async throws {
        let withoutRequestID = try HolonContentReportRequest(
            agentID: "a", turnID: "t", messageID: "m", category: "privacy")
        XCTAssertNil(withoutRequestID.clientRequestID)
        let encoded = try JSONSerialization.jsonObject(
            with: JSONEncoder().encode(withoutRequestID)) as! [String: Any]
        XCTAssertNil(encoded["client_request_id"])
        XCTAssertThrowsError(try HolonContentReportRequest(
            agentID: "a", turnID: "t", messageID: "m", category: "unknown"))
        let boundary = String(repeating: "e\u{301}", count: 1_000)
        XCTAssertNoThrow(try contentReport(description: boundary))
        XCTAssertThrowsError(try contentReport(description: boundary + "x"))
        XCTAssertThrowsError(try HolonContentReportRequest(
            agentID: "", turnID: "t", messageID: "m", category: "privacy", clientRequestID: "id"))
        XCTAssertThrowsError(try HolonContentReportRequest(
            agentID: "a", turnID: "t", messageID: "m", category: "privacy", clientRequestID: "é"))
        let raw = Data(#"{"agent_id":"a","turn_id":"t","message_id":"m","category":"privacy","client_request_id":"bad key"}"#.utf8)
        let decoded = try JSONDecoder().decode(HolonContentReportRequest.self, from: raw)
        let (sdk, exchange) = try client([])
        do { _ = try await sdk.createContentReport(decoded); XCTFail() }
        catch { XCTAssertEqual(error as? HolonClientError, .invalidRequest) }
        XCTAssertTrue(exchange.requests.isEmpty)
    }

    func testPromptStableIdentityPayloadAndUnknownReceipt() async throws {
        let attachment = try HolonPromptAttachment(kind: .file, name: "note.txt",
                                                   mediaType: "text/plain", data: Data("hello".utf8))
        let prompt = try HolonPromptRequest(clientRequestID: "persisted-id", text: "",
                                           attachments: [attachment])
        let (sdk, exchange) = try client([
            MockReply(status: 503, body: Data(#"{"ok":false}"#.utf8)),
            MockReply(body: Data(#"{"ok":true,"agent_id":"main","message_id":"m","disposition":"duplicate","future":42}"#.utf8)),
            MockReply(body: Data(#"{"ok":true,"agent_id":"main","message_id":"m","disposition":"future_state"}"#.utf8))])
        do { _ = try await sdk.sendOperatorPrompt(agentID: "main", request: prompt); XCTFail() }
        catch { XCTAssertEqual((error as? HolonHTTPFailure)?.statusCode, 503) }
        XCTAssertEqual(exchange.requests.count, 1)
        let receipt = try await sdk.sendOperatorPrompt(agentID: "main", request: prompt)
        XCTAssertTrue(receipt.value.isAccepted)
        XCTAssertEqual(receipt.value.raw["future"], .integer(42))
        XCTAssertEqual(exchange.requests[0].httpBody, exchange.requests[1].httpBody)
        let body = try JSONDecoder().decode(JSONValue.self, from: exchange.requests[1].httpBody!)
        XCTAssertEqual(body, prompt.payload)
        XCTAssertEqual(body["client_request_id"], .string("persisted-id"))
        XCTAssertEqual(exchange.requests[1].url?.path, "/prefix/api/control/agents/main/prompt")
        let unknown = try await sdk.sendOperatorPrompt(agentID: "main", request: prompt)
        XCTAssertFalse(unknown.value.isAccepted)
        XCTAssertEqual(unknown.value.disposition, "future_state")
    }

    func testPromptRejectionAndMalformedSuccessNeverRetry() async throws {
        let prompt = try HolonPromptRequest(clientRequestID: "stable", text: "hello")
        for reply in [
            MockReply(status: 409, body: Data(#"{"ok":false,"code":"idempotency_conflict","retryable":false}"#.utf8)),
            MockReply(body: Data(#"{"ok":false,"agent_id":"main","message_id":"m","disposition":"accepted"}"#.utf8)),
            MockReply(body: Data(#"{"ok":true,"agent_id":"other","message_id":"m","disposition":"accepted"}"#.utf8)),
            MockReply(body: Data(#"{"ok":true,"agent_id":"main","message_id":"m"}"#.utf8))] {
            let (sdk, exchange) = try client([reply])
            do { _ = try await sdk.sendOperatorPrompt(agentID: "main", request: prompt); XCTFail() }
            catch { XCTAssertTrue(error is HolonHTTPFailure || error is HolonClientError) }
            XCTAssertEqual(exchange.requests.count, 1)
            XCTAssertEqual(prompt.clientRequestID, "stable")
        }
    }

    func testLostPromptResponseRetainsRequestAndCredential() async throws {
        let prompt = try HolonPromptRequest(clientRequestID: "durable-id", text: "immutable")
        let (sdk, exchange) = try client([
            MockReply(error: .networkConnectionLost),
            MockReply(body: Data(#"{"ok":true,"agent_id":"main","message_id":"m","disposition":"duplicate"}"#.utf8))])
        do { _ = try await sdk.sendOperatorPrompt(agentID: "main", request: prompt); XCTFail() }
        catch { XCTAssertTrue(error is URLError) }
        XCTAssertEqual(exchange.requests.count, 1)
        _ = try await sdk.sendOperatorPrompt(agentID: "main", request: prompt)
        XCTAssertEqual(exchange.requests[0].httpBody, exchange.requests[1].httpBody)
        XCTAssertEqual(exchange.requests[1].value(forHTTPHeaderField: "Authorization"), "Bearer test-session")
    }

    func testModelAndStopFailuresNeverRetryOrConfirmWrongRun() async throws {
        let (sdk, exchange) = try client([
            MockReply(status: 503, body: Data(#"{"ok":false}"#.utf8)),
            MockReply(status: 409, body: Data(#"{"ok":false,"code":"run_mismatch"}"#.utf8)),
            MockReply(body: Data(#"{"ok":true,"aborted":true,"agent_id":"main","run_id":"new-run","mode":"stop_after_abort"}"#.utf8))])
        do {
            _ = try await sdk.setAgentModel(agentID: "main", request: HolonAgentModelRequest(model: "vendor/model"))
            XCTFail()
        } catch { XCTAssertEqual((error as? HolonHTTPFailure)?.statusCode, 503) }
        XCTAssertEqual(exchange.requests.count, 1)
        do { _ = try await sdk.stopCurrentRun(agentID: "main", runID: "observed"); XCTFail() }
        catch { XCTAssertEqual((error as? HolonHTTPFailure)?.statusCode, 409) }
        XCTAssertEqual(exchange.requests.count, 2)
        do { _ = try await sdk.stopCurrentRun(agentID: "main", runID: "observed"); XCTFail() }
        catch { XCTAssertEqual(error as? HolonClientError, .malformedResponse) }
        XCTAssertEqual(exchange.requests.count, 3)
    }

    func testPromptAttachmentLimitsAndIDValidation() throws {
        XCTAssertThrowsError(try HolonPromptRequest(clientRequestID: " ", text: "hello"))
        XCTAssertThrowsError(try HolonPromptRequest(clientRequestID: String(repeating: "é", count: 101), text: "hello"))
        XCTAssertThrowsError(try HolonPromptRequest(clientRequestID: "id", text: " "))
        XCTAssertThrowsError(try HolonPromptAttachment(kind: .image, mediaType: "image/svg+xml", data: Data([1])))
        XCTAssertThrowsError(try HolonPromptAttachment(kind: .file, mediaType: "text/plain", data: Data()))
        XCTAssertThrowsError(try HolonPromptAttachment(kind: .file, mediaType: "text/plain",
            data: Data(count: HolonPromptLimits.maximumAttachmentBytes + 1)))
        let attachment = try HolonPromptAttachment(kind: .file, mediaType: "text/plain",
                                                    data: Data(count: 13 * 1024 * 1024))
        XCTAssertThrowsError(try HolonPromptRequest(clientRequestID: "id", text: "",
                                                   attachments: [attachment, attachment]))
    }

    func testModelAndExplicitCurrentRunStopPaths() async throws {
        let (sdk, exchange) = try client([
            MockReply(body: Data(#"{"available_models":["vendor/model"],"future":true}"#.utf8)),
            MockReply(body: Data(#"{"model":{"effective_model":"vendor/model","future":42}}"#.utf8)),
            MockReply(body: Data(#"{"ok":true,"model":{"effective_model":"vendor@default/new","future":43}}"#.utf8)),
            MockReply(body: Data(#"{"ok":true,"aborted":true,"agent_id":"main","run_id":"observed-run","mode":"stop_after_abort","future":true}"#.utf8))])
        let catalog = try await sdk.modelCatalog()
        XCTAssertEqual(catalog.value.raw["future"], .bool(true))
        let state = try await sdk.agentModel(agentID: "main")
        XCTAssertEqual(state.value.raw["future"], .integer(42))
        let model = try HolonAgentModelRequest(model: "vendor/new", reasoningEffort: "high")
        let updated = try await sdk.setAgentModel(agentID: "main", request: model)
        XCTAssertEqual(updated.value.effectiveModel, .string("vendor@default/new"))
        let stopped = try await sdk.stopCurrentRun(agentID: "main", runID: "observed-run")
        XCTAssertEqual(stopped.value.runID, "observed-run")
        XCTAssertEqual(exchange.requests.map { $0.url!.path }, [
            "/prefix/api/models", "/prefix/api/agents/main",
            "/prefix/api/control/agents/main/model", "/prefix/api/control/agents/main/current-run/abort"])
        let stop = try JSONDecoder().decode(JSONValue.self, from: exchange.requests.last!.httpBody!)
        XCTAssertEqual(stop["mode"], .string("stop_after_abort"))
        XCTAssertEqual(stop["run_id"], .string("observed-run"))
        XCTAssertEqual(exchange.requests[2].httpMethod, "POST")
        XCTAssertEqual(exchange.requests[3].httpMethod, "POST")
    }

    private func fixture(_ name: String) throws -> Data {
        let root = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent().deletingLastPathComponent()
            .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
        return try Data(contentsOf: root.appendingPathComponent("tests/fixtures/client-wire/\(name).json"))
    }

    func testConversationBindingAndConfirmedReadPOST() async throws {
        var raw = try JSONDecoder().decode(JSONValue.self, from: fixture("mobile-conversation-v2"))
        if case .object(var fields) = raw { fields.removeValue(forKey: "agent_id"); raw = .object(fields) }
        let (sdk, exchange) = try client([
            MockReply(body: JSONEncoder().encode(raw)),
            MockReply(body: Data(#"{"state":{},"applied_read_through_event_seq":42,"future":true}"#.utf8))])
        let snapshot = try await sdk.conversation(agentID: "bound-agent", before: "opaque+cursor", limit: 10)
        XCTAssertEqual(snapshot.value.agentID, "bound-agent")
        let read = try await sdk.markBriefRead(agentID: "bound-agent", readThroughEventSeq: 42)
        XCTAssertEqual(read.value["future"], .bool(true))
        XCTAssertEqual(exchange.requests.last?.httpMethod, "POST")
        XCTAssertEqual(exchange.requests.last?.url?.path, "/prefix/api/agents/bound-agent/brief-read-cursor")
        XCTAssertEqual(URLComponents(url: exchange.requests[0].url!, resolvingAgainstBaseURL: false)?
            .queryItems?.first(where: { $0.name == "before" })?.value, "opaque+cursor")
        let (failed, attempts) = try client([MockReply(status: 503, body: fixture("error-v1")),
                                           MockReply(body: Data(#"{"state":{},"applied_read_through_event_seq":42}"#.utf8))])
        do { _ = try await failed.markBriefRead(agentID: "main", readThroughEventSeq: 42); XCTFail("Must fail") }
        catch { XCTAssertEqual((error as? HolonHTTPFailure)?.statusCode, 503) }
        XCTAssertEqual(attempts.requests.count, 1)
    }

    func testWorkReadsPreserveOpenMetadataIdentityAndNonblockingOutput() async throws {
        let raw = #"{"owner_agent_id":"other","status":"future","plan_artifact":{"execution_root_id":"removed"},"output_truncated":true}"#
        let (sdk, exchange) = try client(Array(repeating: MockReply(body: Data(raw.utf8)), count: 5))
        let expected = await sdk.identity
        let list = try await sdk.workItems(agentID: "a/b", limit: 7)
        let detail = try await sdk.workItem(agentID: "a/b", workItemID: "w #")
        _ = try await sdk.tasks(agentID: "a/b")
        _ = try await sdk.task(agentID: "a/b", taskID: "t")
        let output = try await sdk.taskOutput(agentID: "a/b", taskID: "t")
        XCTAssertEqual(list.identity, expected)
        XCTAssertEqual(detail.value["owner_agent_id"], .string("other"))
        XCTAssertEqual(detail.value["status"], .string("future"))
        XCTAssertEqual(output.value["output_truncated"], .bool(true))
        XCTAssertTrue(exchange.requests[0].url!.absoluteString.contains("/prefix/api/agents/a%2Fb/work-items"))
        XCTAssertEqual(URLComponents(url: exchange.requests[4].url!, resolvingAgainstBaseURL: false)?
            .queryItems, [URLQueryItem(name: "block", value: "false")])
        XCTAssertTrue(exchange.requests.allSatisfy { $0.httpMethod == "GET" })
    }

    func testWorkspaceRootLocatorAndResolverEncoding() async throws {
        let (sdk, exchange) = try client([
            MockReply(body: Data(#"{"workspace":{"workspaces":[],"execution_roots":[{"id":"root"}]}}"#.utf8)),
            MockReply(body: Data(#"{"type":"directory","entries":[]}"#.utf8)),
            MockReply(body: Data(#"{"results":[{"status":"unresolved","reason":"removed"}]}"#.utf8)),
            MockReply(body: Data([0, 255, 2]), contentType: "application/octet-stream")])
        let workspaces = try await sdk.agentWorkspaces(agentID: "main")
        XCTAssertNotNil(workspaces.value["execution_roots"])
        _ = try await sdk.browseWorkspaceDirectory(workspaceID: "ws", executionRootID: "r+ &")
        let reference = try await sdk.resolveFileReference(.workspaceURI("workspace://ws/a%20b?root=r%2B"))
        XCTAssertNotNil(reference.value["results"])
        let file = try await sdk.downloadWorkspaceArtifact(locator: "workspace://ws/a%20%23%25.bin?root=r%2B%20%26")
        XCTAssertEqual(file.value.data, Data([0, 255, 2]))
        XCTAssertEqual(file.identity, workspaces.identity)
        let request = exchange.requests[3]
        XCTAssertTrue(request.url!.absoluteString.contains("/prefix/api/workspaces/ws/files/a%20%23%25.bin"))
        XCTAssertEqual(URLComponents(url: request.url!, resolvingAgainstBaseURL: false)?.queryItems?
            .first(where: { $0.name == "execution_root_id" })?.value, "r+ &")
        XCTAssertEqual(request.value(forHTTPHeaderField: "Authorization"), "Bearer test-session")
        XCTAssertEqual(request.value(forHTTPHeaderField: "Accept"), "application/octet-stream")
        XCTAssertEqual(exchange.requests[2].httpMethod, "POST")
    }

    func testBinaryBoundsTypesPermissionsAndRemovedRootAreObservable() async throws {
        let (sdk, exchange) = try client([
            MockReply(body: Data([1, 2, 3]), contentType: "application/octet-stream"),
            MockReply(body: Data([1, 2, 3]), contentType: "application/octet-stream",
                      headers: ["Content-Length": "3"]),
            MockReply(body: Data("html".utf8), contentType: "text/html"),
            MockReply(status: 403), MockReply(status: 404)])
        for _ in 0..<2 {
            do {
                _ = try await sdk.downloadWorkspaceFile(workspaceID: "ws", path: "a", maximumBytes: 2)
                XCTFail("Overflow must fail before returning bytes")
            } catch { XCTAssertEqual(error as? HolonClientError, .streamLimitExceeded) }
        }
        do { _ = try await sdk.downloadWorkspaceFile(workspaceID: "ws", path: "a"); XCTFail() }
        catch { XCTAssertEqual(error as? HolonClientError, .unexpectedContentType) }
        for status in [403, 404] {
            do {
                _ = try await sdk.downloadWorkspaceFile(workspaceID: "ws", path: "a", executionRootID: "removed")
                XCTFail()
            } catch { XCTAssertEqual((error as? HolonHTTPFailure)?.statusCode, status) }
        }
        for locator in ["https://other.test/a", "workspace://ws/../secret", "workspace://user@ws/a",
                        "workspace://ws/a?root=1&root=2"] {
            do { _ = try await sdk.downloadWorkspaceArtifact(locator: locator); XCTFail() }
            catch { XCTAssertEqual(error as? HolonClientError, .invalidRequest) }
        }
        XCTAssertEqual(exchange.requests.count, 5)
    }

    func testBinaryIdentityReplacementNeverPublishesOldBytes() async throws {
        let (sdk, _) = try client([MockReply(body: Data([1]), contentType: "application/octet-stream", delay: 0.2)])
        let pending = Task { try await sdk.downloadWorkspaceFile(workspaceID: "ws", path: "a") }
        try await Task.sleep(for: .milliseconds(30))
        _ = try await sdk.bindIdentity(runtimeID: "new", userID: nil, visibilityScopeID: nil, credential: nil)
        do { _ = try await pending.value; XCTFail("Old generation must not publish") }
        catch { /* Cancellation or stale identity are both terminal, never a successful old response. */ }
    }

    private func client(_ replies: [MockReply], credential: String? = "test-session",
                        maximumResponseBytes: Int = 16_777_216)
        throws -> (HolonClient, MockExchange) {
        let host = "\(UUID().uuidString.lowercased()).test"
        let exchange = MockExchange(replies)
        MockHTTP.register(exchange, host: host)
        let config = URLSessionConfiguration.ephemeral
        config.protocolClasses = [MockHTTP.self]
        let endpoint = try HolonEndpoint(apiBaseURL: URL(string: "https://\(host)/prefix/api")!)
        let client = try HolonClient(endpoint: endpoint, networkID: "test",
                                    credential: credential, configuration: config,
                                    maximumResponseBytes: maximumResponseBytes)
        addTeardownBlock {
            await client.close()
            MockHTTP.unregister(host: host)
        }
        return (client, exchange)
    }

    func testPrefixAndEscapedSegmentsAndQuery() throws {
        let endpoint = try HolonEndpoint(apiBaseURL: URL(string: "https://example.test/proxy/api///")!)
        let url = try endpoint.url(path: ["agents", "a/b #%", "conversation"],
                                   query: ["cursor": "x+ y&z"])
        XCTAssertEqual(url.path, "/proxy/api/agents/a/b #%/conversation")
        XCTAssertTrue(url.absoluteString.contains("a%2Fb%20%23%25"))
        XCTAssertEqual(URLComponents(url: url, resolvingAgainstBaseURL: false)?.queryItems?.first?.value,
                       "x+ y&z")
        XCTAssertThrowsError(try endpoint.url(path: [".."]))
        for value in ["file:///api", "https://user:pass@example.test/api",
                      "https://example.test/api?token=secret", "https://example.test/api#fragment",
                      "https://example.test/a/%2e%2e/api", "https://example.test/a%2fb/api"] {
            XCTAssertThrowsError(try HolonEndpoint(apiBaseURL: URL(string: value)!))
        }
        XCTAssertThrowsError(try HolonEndpoint(apiBaseURL: URL(string: "http://192.168.1.10/api")!)) {
            XCTAssertEqual($0 as? HolonClientError, .insecureHTTPRequiresConfirmation)
        }
        for value in ["http://localhost/api", "http://127.0.0.2/api", "http://[::1]/api"] {
            XCTAssertNoThrow(try HolonEndpoint(apiBaseURL: URL(string: value)!))
        }
        XCTAssertNoThrow(try HolonEndpoint(apiBaseURL: URL(string: "http://192.168.1.10/api")!,
                                          allowInsecureHTTP: true))
    }

    func testHandshakeAndRosterUseAuthenticatedSDKAndSharedFixtures() async throws {
        let (sdk, exchange) = try client([MockReply(body: fixture("handshake-v1")),
                                          MockReply(body: fixture("agent-list-future-enums"))])
        let response = try await sdk.handshake()
        let roster = try await sdk.listAgents()
        XCTAssertEqual(response.identity, roster.identity)
        XCTAssertEqual(roster.value.agents.count, 1)
        XCTAssertEqual(exchange.requests.map { $0.url!.path }, ["/prefix/api/handshake", "/prefix/api/agents/list"])
        XCTAssertEqual(exchange.requests.map { $0.value(forHTTPHeaderField: "Authorization") },
                       ["Bearer test-session", "Bearer test-session"])
    }

    func testExplicitBoundedGETRetryAndNoDefaultRetry() async throws {
        let (sdk, exchange) = try client([MockReply(status: 503, body: fixture("error-v1")),
                                          MockReply(body: fixture("handshake-v1"))])
        _ = try await sdk.handshake(retry: HolonRetryPolicy(maxAttempts: 2, baseDelay: 0))
        XCTAssertEqual(exchange.requests.count, 2)
        let (noRetry, failed) = try client([MockReply(status: 503, body: fixture("error-v1")),
                                            MockReply(body: fixture("handshake-v1"))])
        do { _ = try await noRetry.handshake(); XCTFail("Expected HTTP failure") }
        catch { XCTAssertEqual((error as? HolonHTTPFailure)?.statusCode, 503) }
        XCTAssertEqual(failed.requests.count, 1)
        XCTAssertThrowsError(try HolonRetryPolicy(maxAttempts: 0))
        XCTAssertThrowsError(try HolonRetryPolicy(maxAttempts: 9))
        XCTAssertThrowsError(try HolonRetryPolicy(baseDelay: .infinity))
    }

    func testPermissionAndNetworkFailuresDoNotClearCredentials() async throws {
        let (sdk, exchange) = try client([MockReply(status: 403, body: fixture("error-future-domain")),
                                          MockReply(body: fixture("handshake-v1"))])
        do { _ = try await sdk.handshake(retry: HolonRetryPolicy(maxAttempts: 2, baseDelay: 0)); XCTFail() }
        catch {
            let failure = try XCTUnwrap(error as? HolonHTTPFailure)
            XCTAssertEqual(failure.statusCode, 403)
            XCTAssertFalse(failure.apiError!.requiresSessionRenewal(statusCode: 403))
        }
        _ = try await sdk.handshake()
        XCTAssertEqual(exchange.requests.count, 2)
        XCTAssertEqual(exchange.requests.last?.value(forHTTPHeaderField: "Authorization"), "Bearer test-session")
        let (network, lost) = try client([MockReply(error: .notConnectedToInternet),
                                           MockReply(body: fixture("handshake-v1"))])
        do { _ = try await network.handshake(); XCTFail() } catch { XCTAssertTrue(error is URLError) }
        _ = try await network.handshake()
        XCTAssertEqual(lost.requests.last?.value(forHTTPHeaderField: "Authorization"), "Bearer test-session")
    }

    func testSessionExchangeIsUnauthenticatedAndNeverImplicitlyInstallsCredential() async throws {
        let (sdk, exchange) = try client([MockReply(body: fixture("session-response-v1")),
                                          MockReply(body: fixture("handshake-v1"))], credential: "old")
        _ = try await sdk.exchangeSession(credential: "bootstrap", nativeVerifier: "proof")
        _ = try await sdk.handshake()
        XCTAssertEqual(exchange.requests.first?.httpMethod, "POST")
        XCTAssertEqual(exchange.requests.first?.url?.path, "/prefix/api/auth/session/exchange/native")
        XCTAssertNil(exchange.requests.first?.value(forHTTPHeaderField: "Authorization"))
        let body = try JSONDecoder().decode(JSONValue.self, from: XCTUnwrap(exchange.requests.first?.httpBody))
        XCTAssertEqual(body["native_verifier"], .string("proof"))
        XCTAssertEqual(exchange.requests.last?.value(forHTTPHeaderField: "Authorization"), "Bearer old")
        let (failed, request) = try client([MockReply(status: 503, body: fixture("error-v1"))])
        do { _ = try await failed.exchangeSession(credential: "bootstrap"); XCTFail() } catch {}
        XCTAssertEqual(request.requests.count, 1)
    }

    func testChronoFractionalExpiryAndMissingExchangeCredential() async throws {
        var document = try JSONDecoder().decode(JSONValue.self, from: fixture("session-response-v1"))
        guard case .object(var fields) = document else { return XCTFail() }
        fields["expires_at"] = .string("2030-01-01T00:00:00.123456789+00:00")
        document = .object(fields)
        let (sdk, _) = try client([MockReply(body: JSONEncoder().encode(document))])
        let response = try await sdk.exchangeSession(credential: "bootstrap")
        XCTAssertNotNil(response.value.expiresAt)
        fields.removeValue(forKey: "credential")
        let (missing, _) = try client([MockReply(body: JSONEncoder().encode(JSONValue.object(fields)))])
        do { _ = try await missing.exchangeSession(credential: "bootstrap"); XCTFail() }
        catch { XCTAssertEqual(error as? HolonClientError, .malformedResponse) }
    }

    func testIdentityChangeRejectsInflightOldResponse() async throws {
        let (sdk, exchange) = try client([MockReply(body: fixture("handshake-v1"), delay: 2)])
        let before = await sdk.identity
        let pending = Task { try await sdk.handshake() }
        for _ in 0..<100 where exchange.requests.isEmpty { try await Task.sleep(for: .milliseconds(5)) }
        XCTAssertEqual(exchange.requests.count, 1)
        let after = try await sdk.bindIdentity(runtimeID: "r2", userID: "u2",
                                               visibilityScopeID: "v2", credential: "new")
        XCTAssertNotEqual(before, after)
        do { _ = try await pending.value; XCTFail("Must reject an old generation") }
        catch { XCTAssertEqual(error as? HolonClientError, .staleConnection) }
    }

    func testCancellationAndClosedClientDoNotRetry() async throws {
        let (sdk, exchange) = try client([MockReply(body: fixture("handshake-v1"), delay: 2)])
        let pending = Task { try await sdk.handshake(retry: HolonRetryPolicy(maxAttempts: 3, baseDelay: 0)) }
        for _ in 0..<100 where exchange.requests.isEmpty { try await Task.sleep(for: .milliseconds(5)) }
        pending.cancel()
        do { _ = try await pending.value; XCTFail() }
        catch { XCTAssertTrue(error is CancellationError || (error as? URLError)?.code == .cancelled) }
        XCTAssertEqual(exchange.requests.count, 1)
        await sdk.close()
        do { _ = try await sdk.handshake(); XCTFail() }
        catch { XCTAssertEqual(error as? HolonClientError, .closed) }
    }

    func testHTMLRedirectAndMalformedJSONAreNotRetried() async throws {
        for reply in [MockReply(body: Data("<html>login</html>".utf8), contentType: "text/html"),
                      MockReply(body: Data("broken".utf8)),
                      MockReply(status: 302, body: Data(), headers: ["Location": "https://other.test/api"])] {
            let (sdk, exchange) = try client([reply, MockReply(body: fixture("handshake-v1"))])
            do { _ = try await sdk.handshake(retry: HolonRetryPolicy(maxAttempts: 2, baseDelay: 0)); XCTFail() }
            catch { XCTAssertFalse(error is URLError) }
            XCTAssertEqual(exchange.requests.count, 1)
        }
    }

    func testResponseLimitFailsWithoutRetry() async throws {
        let (sdk, exchange) = try client([MockReply(body: fixture("handshake-v1"))],
                                         maximumResponseBytes: 8)
        do { _ = try await sdk.handshake(retry: HolonRetryPolicy(maxAttempts: 2, baseDelay: 0)); XCTFail() }
        catch { XCTAssertEqual(error as? HolonClientError, .streamLimitExceeded) }
        XCTAssertEqual(exchange.requests.count, 1)
    }

    func testSSEConnectionRetryAndEOFRequireExplicitRecovery() async throws {
        let text = "id: 8\nevent: future-control\ndata: 原文\n\n"
        let (sdk, exchange) = try client([MockReply(status: 503, body: fixture("error-v1")),
                                          MockReply(body: Data(text.utf8), contentType: "text/event-stream")])
        let stream = try await sdk.openEventStream(
            path: ["agents", "a/b", "events", "stream"], query: ["after_seq": "7"],
            lastEventID: "epoch:7", retry: HolonRetryPolicy(maxAttempts: 2, baseDelay: 0))
        defer { stream.close() }
        var events = stream.makeAsyncIterator()
        let event = try await events.next()
        XCTAssertEqual(event?.value.event, "future-control")
        XCTAssertEqual(event?.value.id, "8")
        XCTAssertEqual(event?.value.data, "原文")
        let identity = await sdk.identity
        XCTAssertEqual(event?.identity, identity)
        do { _ = try await events.next(); XCTFail() }
        catch { XCTAssertEqual(error as? HolonClientError, .streamEnded) }
        XCTAssertEqual(exchange.requests.count, 2, "EOF must not start a hidden reconnect")
        XCTAssertEqual(exchange.requests.last?.value(forHTTPHeaderField: "Last-Event-ID"), "epoch:7")
        XCTAssertEqual(exchange.requests.last?.value(forHTTPHeaderField: "Authorization"), "Bearer test-session")
        XCTAssertEqual(exchange.requests.last?.value(forHTTPHeaderField: "Accept"), "text/event-stream")
        XCTAssertTrue(exchange.requests.last!.url!.absoluteString.contains("a%2Fb"))
    }

    func testSSECRAndCRLFAtExactLimitsReachEOF() async throws {
        for ending in ["\r", "\r\n"] {
            let text = "data: a\(ending)\(ending)"
            let (sdk, exchange) = try client([
                MockReply(body: Data(text.utf8), contentType: "text/event-stream")
            ])
            let stream = try await sdk.openEventStream(
                path: ["events", "stream"], maximumFrameBytes: text.utf8.count)
            defer { stream.close() }
            var events = stream.makeAsyncIterator()
            let event = try await events.next()
            XCTAssertEqual(event?.value.data, "a")
            do { _ = try await events.next(); XCTFail() }
            catch { XCTAssertEqual(error as? HolonClientError, .streamEnded) }
            XCTAssertEqual(exchange.requests.count, 1)
        }
    }

    func testSSECRLFTrailingLFOverflowDoesNotPublish() async throws {
        let (sdk, _) = try client([
            MockReply(body: Data("data: a\r\n\r\n".utf8), contentType: "text/event-stream")
        ])
        let stream = try await sdk.openEventStream(
            path: ["events", "stream"], maximumFrameBytes: 10)
        defer { stream.close() }
        var events = stream.makeAsyncIterator()
        do { _ = try await events.next(); XCTFail("Oversized frame must not publish an event") }
        catch { XCTAssertEqual(error as? HolonClientError, .streamLimitExceeded) }
    }

    func testSSEOverflowAndWrongContentTypeAreObservable() async throws {
        let (sdk, exchange) = try client([
            MockReply(body: Data("data: 1\n\ndata: 2\n\ndata: 3\n\n".utf8), contentType: "text/event-stream")])
        let stream = try await sdk.openEventStream(path: ["events", "stream"], bufferCapacity: 1)
        defer { stream.close() }
        try await Task.sleep(for: .milliseconds(50))
        var events = stream.makeAsyncIterator()
        _ = try await events.next()
        do { _ = try await events.next(); XCTFail("Never silently drop frames") }
        catch { XCTAssertEqual(error as? HolonClientError, .streamLimitExceeded) }
        XCTAssertEqual(exchange.requests.count, 1)
        let (html, failed) = try client([MockReply(body: Data("html".utf8), contentType: "text/html")])
        do { _ = try await html.openEventStream(path: ["events", "stream"]); XCTFail() }
        catch { XCTAssertEqual(error as? HolonClientError, .unexpectedContentType) }
        XCTAssertEqual(failed.requests.count, 1)
        do { _ = try await html.openEventStream(path: ["events", "stream"], lastEventID: "bad\nid"); XCTFail() }
        catch { XCTAssertEqual(error as? HolonClientError, .invalidRequest) }
        XCTAssertEqual(failed.requests.count, 1)
    }
}

private struct MockReply: Sendable {
    var status = 200
    var body = Data()
    var contentType = "application/json"
    var headers: [String: String] = [:]
    var delay: TimeInterval = 0
    var error: URLError.Code? = nil
}

private final class MockExchange: @unchecked Sendable {
    private let lock = NSLock()
    private var replies: [MockReply]
    private var captured: [URLRequest] = []
    init(_ replies: [MockReply]) { self.replies = replies }
    var requests: [URLRequest] { lock.withLock { captured } }
    func reply(for request: URLRequest) -> MockReply {
        var request = request
        if request.httpBody == nil, let stream = request.httpBodyStream {
            stream.open()
            defer { stream.close() }
            var body = Data()
            var buffer = [UInt8](repeating: 0, count: 1024)
            while stream.hasBytesAvailable {
                let count = stream.read(&buffer, maxLength: buffer.count)
                guard count > 0 else { break }
                body.append(contentsOf: buffer.prefix(count))
            }
            request.httpBody = body
        }
        return lock.withLock {
            captured.append(request)
            return replies.isEmpty ? MockReply(status: 500) : replies.removeFirst()
        }
    }
}

private final class MockHTTP: URLProtocol, @unchecked Sendable {
    private static let registryLock = NSLock()
    nonisolated(unsafe) private static var registry: [String: MockExchange] = [:]
    private let lock = NSLock()
    private var stopped = false
    static func register(_ exchange: MockExchange, host: String) {
        registryLock.withLock { registry[host] = exchange }
    }
    static func unregister(host: String) { _ = registryLock.withLock { registry.removeValue(forKey: host) } }
    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func startLoading() {
        let exchange = Self.registryLock.withLock { Self.registry[request.url!.host!]! }
        let reply = exchange.reply(for: request)
        DispatchQueue.global().asyncAfter(deadline: .now() + reply.delay) { [self] in
            lock.withLock {
                guard !stopped else { return }
                if let error = reply.error { client?.urlProtocol(self, didFailWithError: URLError(error)); return }
                var headers = reply.headers
                headers["Content-Type"] = reply.contentType
                let response = HTTPURLResponse(url: request.url!, statusCode: reply.status,
                                               httpVersion: "HTTP/1.1", headerFields: headers)!
                client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
                client?.urlProtocol(self, didLoad: reply.body)
                client?.urlProtocolDidFinishLoading(self)
            }
        }
    }
    override func stopLoading() { lock.withLock { stopped = true } }
}
