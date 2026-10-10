import Foundation
import HolonClient
import XCTest
@testable import Holon

private actor ExternalSendingTransport: SendingTransport {
    private var sends = 0
    func models() async throws -> [SendingModel] { [] }
    func upload(agentID: String, attachment: SendingAttachment, file: URL) async throws -> SendingPreparedAttachment {
        throw SendingFailure.rejected("An external payload is already frozen.")
    }
    func send(agentID: String, requestID: UUID, payload: SendingPayload) async throws -> String? {
        sends += 1
        return "received"
    }
    func stop(agentID: String, runID: String) async throws {}
    func close() async {}
    func sendCount() -> Int { sends }
}

@MainActor
final class ExternalSendingTests: XCTestCase {
    func testUnknownExternalOutcomeStaysUnknownAfterDurableHostAdoption() throws {
        let store = try makeStore()
        let requestID = UUID()
        let entry = try store.enqueueExternal(requestID: requestID, scope: scope(), text: "frozen",
                                              attachments: [], previousOutcomeUnknown: true)
        XCTAssertEqual(entry.state, .unknown)
        XCTAssertEqual(entry.requestID, requestID)
        XCTAssertEqual(try store.enqueueExternal(requestID: requestID, scope: scope(), text: "frozen",
                                                attachments: []).state, .unknown)
    }
    private let baseURL = URL(string: "https://example.test/api")!
    private func identity() -> HolonConnectionIdentity {
        HolonConnectionIdentity(networkID: "network-secret", runtimeID: "runtime-secret",
                                userID: "user-secret", visibilityScopeID: "visibility-secret")
    }
    private func scope(_ agent: String = "A") throws -> SendingScope {
        SendingScope(partition: try XCTUnwrap(ReadingPartition(apiBaseURL: baseURL, identity: identity())),
                     agentID: agent)
    }
    private func makeStore(maximumBytes: Int64 = 20 * 1024 * 1024) throws -> SendingStore {
        let folder = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        addTeardownBlock { try FileManager.default.removeItem(at: folder) }
        let store = try SendingStore(directory: folder, maximumAttachmentBytes: maximumBytes)
        addTeardownBlock { try await MainActor.run { try store.closeForTesting() } }
        return store
    }
    private func makeSender(_ store: SendingStore, transport: ExternalSendingTransport) -> SendingCoordinator {
        let suite = "ExternalConsentTests.\(UUID())"
        let defaults = UserDefaults(suiteName: suite)!
        let consent = SharingConsent(defaults: defaults)
        consent.approve(baseURL)
        addTeardownBlock { defaults.removePersistentDomain(forName: suite) }
        let sender = SendingCoordinator(store: store, consent: consent)
        addTeardownBlock { await MainActor.run { sender.disconnect() } }
        sender.activate(transport: transport, identity: identity(), apiBaseURL: baseURL)
        sender.selectAgent("A")
        return sender
    }

    func testIndependentImportPreservesDraftAndDeduplicatesWithoutAutomaticSend() async throws {
        let store = try makeStore()
        let transport = ExternalSendingTransport()
        let sender = makeSender(store, transport: transport)
        sender.editDraft(text: "unfinished editor", modelID: "chosen-model")
        let context = try XCTUnwrap(sender.externalContext(agentID: "B"))
        let requestID = UUID()
        let bytes = SendingPreparedAttachment(name: "photo.png", contentType: "image/png", data: Data([1, 2, 3]))
        try sender.enqueueExternal(requestID: requestID, text: "shared", attachments: [bytes], context: context)
        try sender.enqueueExternal(requestID: requestID, text: "shared", attachments: [bytes], context: context)
        XCTAssertEqual(sender.draft.text, "unfinished editor")
        XCTAssertEqual(sender.draft.modelID, "chosen-model")
        XCTAssertEqual(sender.selectedAgentID, "A")
        let entry = try XCTUnwrap(store.entries(try scope("B")).first)
        XCTAssertEqual(store.entries(try scope("B")).count, 1)
        XCTAssertEqual(entry.state, .queued)
        XCTAssertEqual(entry.payload?.attachments, [bytes])
        XCTAssertThrowsError(try sender.enqueueExternal(requestID: requestID, text: "changed", context: context))
        let other = try XCTUnwrap(sender.externalContext(agentID: "A"))
        XCTAssertThrowsError(try sender.enqueueExternal(requestID: requestID, text: "shared",
                                                       attachments: [bytes], context: other))
        await Task.yield()
        let sends = await transport.sendCount()
        XCTAssertEqual(sends, 0)
    }

    func testExternalConfirmationDoesNotResurrectAfterBackgroundOrReauthentication() throws {
        let store = try makeStore()
        let sender = makeSender(store, transport: ExternalSendingTransport())
        let context = try XCTUnwrap(sender.externalContext(agentID: "A"))
        sender.setForeground(false)
        XCTAssertNil(sender.externalContext(agentID: "A"))
        sender.setForeground(true)
        XCTAssertThrowsError(try sender.enqueueExternal(requestID: UUID(), text: "stale", context: context))
        let fresh = try XCTUnwrap(sender.externalContext(agentID: "A"))
        sender.activate(transport: ExternalSendingTransport(), identity: identity(), apiBaseURL: baseURL)
        XCTAssertThrowsError(try sender.enqueueExternal(requestID: UUID(), text: "stale", context: fresh))
        XCTAssertTrue(store.entries(try scope()).isEmpty)
    }

    func testExternalCommitFailureLeavesNeitherRequestNorAttachmentAndEnforcesTotalLimit() throws {
        let store = try makeStore(maximumBytes: 4)
        let scope = try scope()
        let bytes = SendingPreparedAttachment(name: "x.txt", contentType: "text/plain", data: Data([1, 2, 3]))
        XCTAssertThrowsError(try store.enqueueExternal(requestID: UUID(), scope: scope, text: "",
                                                      attachments: [bytes, bytes]))
        store.beforeSave = { throw SendingFailure.rejected("disk failure") }
        XCTAssertThrowsError(try store.enqueueExternal(requestID: UUID(), scope: scope, text: "share",
                                                      attachments: [bytes]))
        XCTAssertTrue(store.entries(scope).isEmpty)
        store.beforeSave = nil
        let entry = try store.enqueueExternal(requestID: UUID(), scope: scope, text: "share", attachments: [bytes])
        let managed = store.attachmentURL(try XCTUnwrap(entry.draft.attachments.first)).deletingLastPathComponent()
        XCTAssertEqual(try FileManager.default.contentsOfDirectory(atPath: managed.path).count, 1)
        try store.delete(requestID: entry.requestID, scope: scope)
        XCTAssertTrue(try FileManager.default.contentsOfDirectory(atPath: managed.path).isEmpty)
    }

    func testExternalImportStillRejectsOversizedUTF8Metadata() throws {
        let store = try makeStore()
        let scope = try scope()
        let file = SendingPreparedAttachment(name: String(repeating: "界", count: 100) + ".txt",
                                             contentType: "text/plain", data: Data([1]))
        XCTAssertThrowsError(try store.enqueueExternal(requestID: UUID(), scope: scope, text: "",
                                                      attachments: [file]))
        XCTAssertTrue(store.entries(scope).isEmpty)
    }

    func testDiagnosticAllowlistExcludesAllIdentityPayloadAndErrorFields() throws {
        var entry = SendingEntry(requestID: UUID(), scope: try scope("agent-secret"),
                                 draft: SendingDraft(text: "token-secret ticket-secret verifier-secret"))
        entry.state = .unknown
        entry.error = "https://user:password@example.test/?session-secret"
        let report = DiagnosticReport(createdAt: Date(timeIntervalSince1970: 0), connection: .connected,
                                      reading: .disconnected, sending: .offline, agentCount: 3, entries: [entry])
        let text = try report.text()
        for secret in ["network-secret", "runtime-secret", "user-secret", "visibility-secret", "agent-secret",
                       "token-secret", "ticket-secret", "verifier-secret", "session-secret", "password",
                       entry.requestID.uuidString, "example.test"] {
            XCTAssertFalse(text.contains(secret))
        }
        let json = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(text.utf8)) as? [String: Any])
        XCTAssertEqual(json["unknownCount"] as? Int, 1)
        XCTAssertEqual(json["redacted"] as? Bool, true)
        XCTAssertEqual(json.count, 10)
    }

    private func makeImportStore() throws -> SharedImportStore {
        let folder = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        addTeardownBlock { try FileManager.default.removeItem(at: folder) }
        return try SharedImportStore(container: folder)
    }

    func testHostShareConfirmationQueuesChosenAgentWithoutChangingSelectionOrDraft() async throws {
        let shared = try makeImportStore()
        let payload = try shared.stage(text: "shared text", urls: [URL(string: "https://example.test/path")!],
                                       files: [SharedImportFile(name: "file.txt", typeIdentifier: "public.plain-text",
                                                                data: Data("attachment".utf8))])
        let store = try makeStore()
        let transport = ExternalSendingTransport()
        let sender = makeSender(store, transport: transport)
        sender.editDraft(text: "keep editor", modelID: "model")
        let host = SharedImportCoordinator(store: shared)
        host.reload()
        host.prepare(payloadID: payload.id, agentID: "B", sender: sender)
        XCTAssertNotNil(host.confirmation)
        host.confirm(sender: sender)
        XCTAssertEqual(host.outcome, "share.queued")
        XCTAssertTrue(try shared.load().isEmpty)
        XCTAssertEqual(sender.draft.text, "keep editor")
        XCTAssertEqual(sender.selectedAgentID, "A")
        let entry = try XCTUnwrap(store.entries(try scope("B")).first)
        XCTAssertEqual(entry.requestID, payload.id)
        XCTAssertEqual(entry.payload?.text, "shared text\n\nhttps://example.test/path")
        XCTAssertEqual(entry.payload?.attachments.first?.contentType, "text/plain")
        await Task.yield()
        let sends = await transport.sendCount()
        XCTAssertEqual(sends, 0)
    }

    func testHostShareQueuesLongChineseAttachmentName() async throws {
        try await assertHostShareQueuesAttachment(
            name: String(repeating: "界", count: 100) + ".txt",
            expected: String(repeating: "界", count: 83) + ".txt")
    }

    func testHostShareQueuesLongCompoundEmojiAttachmentName() async throws {
        try await assertHostShareQueuesAttachment(
            name: String(repeating: "👩🏽‍💻", count: 30) + ".txt",
            expected: String(repeating: "👩🏽‍💻", count: 16) + ".txt")
    }

    private func assertHostShareQueuesAttachment(name: String, expected: String) async throws {
        XCTAssertLessThanOrEqual(name.count, 120)
        XCTAssertGreaterThan(name.utf8.count, 255)
        let shared = try makeImportStore()
        let data = Data("unchanged attachment bytes".utf8)
        let payload = try shared.stage(text: "", urls: [], files: [
            SharedImportFile(name: name, typeIdentifier: "public.plain-text", data: data)
        ])
        XCTAssertEqual(payload.attachments.first?.name, expected)
        let store = try makeStore()
        let transport = ExternalSendingTransport()
        let sender = makeSender(store, transport: transport)
        let host = SharedImportCoordinator(store: shared)
        host.reload()
        XCTAssertEqual(host.pending, [payload])
        host.prepare(payloadID: payload.id, agentID: "A", sender: sender)
        XCTAssertNotNil(host.confirmation)
        host.confirm(sender: sender)
        XCTAssertEqual(host.outcome, "share.queued")
        XCTAssertTrue(try shared.load().isEmpty)
        XCTAssertTrue(host.pending.isEmpty)
        let entries = store.entries(try scope())
        XCTAssertEqual(entries.count, 1)
        let entry = try XCTUnwrap(entries.first)
        XCTAssertEqual(entry.requestID, payload.id)
        let attachment = try XCTUnwrap(entry.payload?.attachments.first)
        XCTAssertEqual(attachment.name, expected)
        XCTAssertEqual(attachment.data, data)
        let storedAttachment = try XCTUnwrap(entry.draft.attachments.first)
        XCTAssertEqual(storedAttachment.name, expected)
        XCTAssertEqual(try Data(contentsOf: store.attachmentURL(storedAttachment)), data)
        await Task.yield()
        let sends = await transport.sendCount()
        XCTAssertEqual(sends, 0)
    }

    func testHostShareUsesTheValidatedCanonicalTextAtTheUTF8Limit() async throws {
        let shared = try makeImportStore()
        let url = URL(string: "https://example.test/path")!
        let text = String(repeating: "a", count: SharedImportStore.maxTextBytes - url.absoluteString.utf8.count - 2)
        let payload = try shared.stage(text: text, urls: [url], files: [])
        let store = try makeStore()
        let transport = ExternalSendingTransport()
        let sender = makeSender(store, transport: transport)
        let host = SharedImportCoordinator(store: shared)
        host.reload()
        host.prepare(payloadID: payload.id, agentID: "A", sender: sender)
        host.confirm(sender: sender)
        let entry = try XCTUnwrap(store.entries(try scope()).first)
        XCTAssertEqual(entry.payload?.text, SharedImportStore.sendingText(text: text, urls: [url]))
        XCTAssertEqual(entry.payload?.text.utf8.count, SharedImportStore.maxTextBytes)
        XCTAssertTrue(try shared.load().isEmpty)
        await Task.yield()
        let sends = await transport.sendCount()
        XCTAssertEqual(sends, 0)
    }

    func testHostShareKeepsRecoverableRecordOnFailedSaveAndRejectsStaleConfirmation() throws {
        let shared = try makeImportStore()
        let payload = try shared.stage(text: "recover me", urls: [], files: [])
        let store = try makeStore()
        let sender = makeSender(store, transport: ExternalSendingTransport())
        let host = SharedImportCoordinator(store: shared)
        host.reload()
        host.prepare(payloadID: payload.id, agentID: "A", sender: sender)
        store.beforeSave = { throw SendingFailure.rejected("save failed") }
        host.confirm(sender: sender)
        XCTAssertEqual(try shared.load(), [payload])
        XCTAssertTrue(store.entries(try scope()).isEmpty)
        XCTAssertNil(host.confirmation)
        store.beforeSave = nil
        host.prepare(payloadID: payload.id, agentID: "A", sender: sender)
        sender.setForeground(false)
        sender.setForeground(true)
        host.confirm(sender: sender)
        XCTAssertEqual(try shared.load(), [payload])
        XCTAssertTrue(store.entries(try scope()).isEmpty)
        host.prepare(payloadID: payload.id, agentID: "A", sender: sender)
        host.confirm(sender: sender)
        XCTAssertTrue(try shared.load().isEmpty)
        XCTAssertEqual(store.entries(try scope()).count, 1)
    }
}
