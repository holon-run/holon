import Foundation
import HolonClient
import XCTest
@testable import Holon

private actor SendingMockTransport: SendingTransport {
    struct Call: Equatable { let id: UUID; let payload: SendingPayload }
    var calls: [Call] = []
    var stops = 0
    var closes = 0
    var uploads = 0
    var catalogs = 0
    var afterUpload: (@Sendable () -> Void)?
    var loseResponse = true
    var failure: HolonHTTPFailure?
    var delayed = false
    var continuation: CheckedContinuation<String?, any Error>?
    func upload(agentID: String, attachment: SendingAttachment, file: URL) async throws -> SendingPreparedAttachment {
        uploads += 1
        afterUpload?()
        return SendingPreparedAttachment(name: attachment.name, contentType: attachment.contentType,
                                         data: try Data(contentsOf: file))
    }
    func send(agentID: String, requestID: UUID, payload: SendingPayload) async throws -> String? {
        calls.append(Call(id: requestID, payload: payload))
        if let failure { throw failure }
        if delayed { return try await withCheckedThrowingContinuation { continuation = $0 } }
        if loseResponse { throw URLError(.networkConnectionLost) }
        return "message"
    }
    func models() async throws -> [SendingModel] { catalogs += 1; return [] }
    func stop(agentID: String, runID: String) async throws { stops += 1 }
    func close() async { closes += 1 }
    func succeed() { loseResponse = false }
    func rejectAuthentication(_ identity: HolonConnectionIdentity) {
        failure = HolonHTTPFailure(statusCode: 401, apiError: nil, identity: identity)
    }
    func delay() { delayed = true }
    func release() { continuation?.resume(returning: "late"); continuation = nil }
    func callCount() -> Int { calls.count }
    func recorded() -> [Call] { calls }
    func stopCount() -> Int { stops }
    func closeCount() -> Int { closes }
    func uploadCount() -> Int { uploads }
    func catalogCount() -> Int { catalogs }
    func revokeAfterUpload(_ action: @escaping @Sendable () -> Void) { afterUpload = action }
}

@MainActor
final class SendingCoordinatorTests: XCTestCase {
    func testMissingConsentBlocksEnqueueAndNewHost() async throws {
        let folder = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        addTeardownBlock { try FileManager.default.removeItem(at: folder) }
        let suite = "MissingConsent.\(UUID())"
        let defaults = UserDefaults(suiteName: suite)!
        addTeardownBlock { defaults.removePersistentDomain(forName: suite) }
        let consent = SharingConsent(defaults: defaults)
        let transport = SendingMockTransport()
        let coordinator = SendingCoordinator(store: try makeStore(directory: folder), consent: consent)
        defer { coordinator.disconnect() }
        let url = URL(string: "https://example.test")!
        coordinator.activate(transport: transport, identity: identity(), apiBaseURL: url)
        coordinator.selectAgent("A")
        coordinator.editDraft(text: "private", modelID: nil)
        coordinator.enqueue()
        XCTAssertNotNil(coordinator.error)
        XCTAssertTrue(coordinator.entries.isEmpty)
        consent.approve(url)
        coordinator.activate(transport: transport, identity: identity(), apiBaseURL: URL(string: "https://other.test")!)
        coordinator.selectAgent("A")
        coordinator.editDraft(text: "private", modelID: nil)
        coordinator.enqueue()
        let calls = await transport.callCount()
        XCTAssertEqual(calls, 0)
        XCTAssertTrue(coordinator.entries.isEmpty)
    }

    func testWithdrawalDuringUploadBlocksRemainingUploadAndPost() async throws {
        let folder = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        addTeardownBlock { try FileManager.default.removeItem(at: folder) }
        let suite = "WithdrawConsent.\(UUID())"
        let defaults = UserDefaults(suiteName: suite)!
        addTeardownBlock { defaults.removePersistentDomain(forName: suite) }
        let consent = SharingConsent(defaults: defaults)
        let url = URL(string: "https://example.test")!
        consent.approve(url)
        let transport = SendingMockTransport()
        await transport.revokeAfterUpload {
            SharingConsent(defaults: UserDefaults(suiteName: suite)).revoke(url)
        }
        let coordinator = SendingCoordinator(store: try makeStore(directory: folder), consent: consent)
        defer { coordinator.disconnect() }
        coordinator.activate(transport: transport, identity: identity(), apiBaseURL: url)
        coordinator.selectAgent("A")
        coordinator.editDraft(text: "private", modelID: nil)
        for name in ["one.txt", "two.txt"] {
            let source = folder.appendingPathComponent(name)
            try Data(name.utf8).write(to: source)
            coordinator.stageAttachment(source: source,
                context: try XCTUnwrap(coordinator.attachmentImportContext), contentType: "text/plain")
        }
        coordinator.enqueue()
        try await waitUntil { coordinator.entries.first?.state == .unknown }
        let uploads = await transport.uploadCount()
        let posts = await transport.callCount()
        XCTAssertEqual(uploads, 1)
        XCTAssertEqual(posts, 0)
        coordinator.retry(requestID: try XCTUnwrap(coordinator.entries.first?.requestID))
        for _ in 0..<10 { await Task.yield() }
        let retryPosts = await transport.callCount()
        XCTAssertEqual(retryPosts, 0)
    }

    private func makeStore(directory: URL) throws -> SendingStore {
        let store = try SendingStore(directory: directory)
        addTeardownBlock {
            try await MainActor.run { try store.closeForTesting() }
        }
        return store
    }

    private func makeCoordinator(store: SendingStore) -> SendingCoordinator {
        let suite = "SendingConsentTests.\(UUID())"
        let defaults = UserDefaults(suiteName: suite)!
        let consent = SharingConsent(defaults: defaults)
        consent.approve(URL(string: "https://example.test")!)
        addTeardownBlock { defaults.removePersistentDomain(forName: suite) }
        let coordinator = SendingCoordinator(store: store, consent: consent)
        addTeardownBlock {
            await MainActor.run { coordinator.disconnect() }
        }
        return coordinator
    }

    func testAttachmentImportRejectsIdentityAndPartitionChanges() throws {
        let folder = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        addTeardownBlock { try FileManager.default.removeItem(at: folder) }
        let coordinator = makeCoordinator(store: try makeStore(directory: folder))
        let source = folder.appendingPathComponent("photo.txt")
        try Data("photo".utf8).write(to: source)
        let url = URL(string: "https://example.test")!
        coordinator.activate(transport: SendingMockTransport(), identity: identity(), apiBaseURL: url)
        coordinator.selectAgent("A")
        let changes: [(HolonConnectionIdentity, URL)] = [
            (identity("other"), url),
            (HolonConnectionIdentity(networkID: "cell", runtimeID: "runtime", userID: "user",
                                     visibilityScopeID: "private"), url),
            (HolonConnectionIdentity(networkID: "wifi", runtimeID: "other-runtime", userID: "user",
                                     visibilityScopeID: "private"), url),
            (HolonConnectionIdentity(networkID: "wifi", runtimeID: "runtime", userID: "user",
                                     visibilityScopeID: "other-scope"), url),
            (identity(), URL(string: "https://other.test/prefix/api")!),
            (identity(), url),
            (identity(), url)
        ]
        for (nextIdentity, nextURL) in changes {
            let context = try XCTUnwrap(coordinator.attachmentImportContext)
            coordinator.activate(transport: SendingMockTransport(), identity: nextIdentity, apiBaseURL: nextURL)
            coordinator.selectAgent("A")
            coordinator.stageAttachment(source: source, context: context, contentType: "text/plain")
            XCTAssertTrue(coordinator.draft.attachments.isEmpty)
        }
        coordinator.stageAttachment(source: source, context: try XCTUnwrap(coordinator.attachmentImportContext),
                                    contentType: "text/plain")
        XCTAssertEqual(coordinator.draft.attachments.count, 1)
    }

    func testAttachmentImportIsRevokedByNavigationAndEnqueue() throws {
        let folder = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        addTeardownBlock { try FileManager.default.removeItem(at: folder) }
        let coordinator = makeCoordinator(store: try makeStore(directory: folder))
        let source = folder.appendingPathComponent("photo.txt")
        try Data("photo".utf8).write(to: source)
        coordinator.setForeground(false)
        coordinator.activate(transport: SendingMockTransport(), identity: identity(),
                             apiBaseURL: URL(string: "https://example.test")!)
        coordinator.selectAgent("A")
        let beforeNavigation = try XCTUnwrap(coordinator.attachmentImportContext)
        coordinator.selectAgent("B")
        coordinator.selectAgent("A")
        coordinator.stageAttachment(source: source, context: beforeNavigation, contentType: "text/plain")
        XCTAssertTrue(coordinator.draft.attachments.isEmpty)
        let beforeEnqueue = try XCTUnwrap(coordinator.attachmentImportContext)
        coordinator.stageAttachment(source: source, context: beforeEnqueue, contentType: "text/plain")
        coordinator.enqueue()
        XCTAssertEqual(coordinator.entries.first?.state, .queued)
        coordinator.stageAttachment(source: source, context: beforeEnqueue, contentType: "text/plain")
        XCTAssertTrue(coordinator.draft.attachments.isEmpty)
    }

    func testAttachmentImportSurvivesPickerInactivityWithinSameScope() throws {
        let folder = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        addTeardownBlock { try FileManager.default.removeItem(at: folder) }
        let coordinator = makeCoordinator(store: try makeStore(directory: folder))
        let source = folder.appendingPathComponent("photo.txt")
        try Data("photo".utf8).write(to: source)
        coordinator.activate(transport: SendingMockTransport(), identity: identity(),
                             apiBaseURL: URL(string: "https://example.test")!)
        coordinator.selectAgent("A")
        let context = try XCTUnwrap(coordinator.attachmentImportContext)
        coordinator.setForeground(false)
        coordinator.setForeground(true)
        coordinator.stageAttachment(source: source, context: context, contentType: "text/plain")
        XCTAssertEqual(coordinator.draft.attachments.count, 1)
    }

    func testBackgroundActivationDefersCatalogWithoutAutomaticallySending() async throws {
        let folder = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        addTeardownBlock { try FileManager.default.removeItem(at: folder) }
        let coordinator = makeCoordinator(store: try makeStore(directory: folder))
        let transport = SendingMockTransport()
        coordinator.setForeground(false)
        coordinator.activate(transport: transport, identity: identity(), apiBaseURL: URL(string: "https://example.test")!)
        coordinator.selectAgent("A")
        coordinator.editDraft(text: "prompt", modelID: nil)
        coordinator.enqueue()
        for _ in 0..<10 { await Task.yield() }
        let catalogs = await transport.catalogCount()
        XCTAssertEqual(catalogs, 0)
        XCTAssertEqual(coordinator.status, .offline)
        coordinator.setForeground(true)
        try await waitUntil { await transport.catalogCount() == 1 }
        let sends = await transport.callCount()
        XCTAssertEqual(sends, 0)
        XCTAssertEqual(coordinator.entries.first?.state, .queued)
    }

    func testDisconnectClosesOldTransportWithoutStoppingAgent() async throws {
        let folder = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        addTeardownBlock { try FileManager.default.removeItem(at: folder) }
        let coordinator = makeCoordinator(store: try makeStore(directory: folder))
        let transport = SendingMockTransport()
        coordinator.activate(transport: transport, identity: identity(), apiBaseURL: URL(string: "https://example.test")!)
        coordinator.disconnect()
        XCTAssertEqual(coordinator.status, .disconnected)
        XCTAssertTrue(coordinator.models.isEmpty)
        XCTAssertTrue(coordinator.entries.isEmpty)
        try await waitUntil { await transport.closeCount() == 1 }
        let stops = await transport.stopCount()
        XCTAssertEqual(stops, 0)
    }

    func testAuthenticationFailureOnlyNotifiesForCurrentIdentity() async throws {
        let folder = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        addTeardownBlock { try FileManager.default.removeItem(at: folder) }
        let coordinator = makeCoordinator(store: try makeStore(directory: folder))
        let transport = SendingMockTransport()
        let authority = identity()
        var failures: [HolonHTTPFailure] = []
        coordinator.onConnectionFailure = { failures.append($0) }
        coordinator.activate(transport: transport, identity: authority, apiBaseURL: URL(string: "https://example.test")!)
        coordinator.selectAgent("A")
        coordinator.editDraft(text: "prompt", modelID: nil)
        // The same user/network tuple with another generation is still stale.
        await transport.rejectAuthentication(identity())
        coordinator.enqueue()
        try await waitUntil { coordinator.entries.first?.state == .unknown }
        XCTAssertTrue(failures.isEmpty)
        await transport.rejectAuthentication(authority)
        coordinator.retry(requestID: try XCTUnwrap(coordinator.entries.first?.requestID))
        try await waitUntil { failures.count == 1 }
        XCTAssertEqual(failures.first?.statusCode, 401)
        XCTAssertEqual(coordinator.entries.first?.state, .unknown)
    }

    private func identity(_ user: String = "user") -> HolonConnectionIdentity {
        HolonConnectionIdentity(networkID: "wifi", runtimeID: "runtime", userID: user,
                                visibilityScopeID: "private")
    }
    private func waitUntil(_ predicate: @escaping @MainActor () async -> Bool) async throws {
        for _ in 0..<200 {
            if await predicate() { return }
            try await Task.sleep(for: .milliseconds(5))
        }
        XCTFail("Timed out waiting for sending transition")
    }

    func testLostResponseExplicitRetryPreservesIDPayloadAndReceivedCannotResend() async throws {
        let folder = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        addTeardownBlock { try FileManager.default.removeItem(at: folder) }
        let store = try makeStore(directory: folder)
        let transport = SendingMockTransport()
        let coordinator = makeCoordinator(store: store)
        coordinator.activate(transport: transport, identity: identity(), apiBaseURL: URL(string: "https://example.test")!)
        coordinator.selectAgent("A")
        coordinator.editDraft(text: "original", modelID: "model")
        coordinator.enqueue()
        try await waitUntil { coordinator.entries.first?.state == .unknown }
        let id = try XCTUnwrap(coordinator.entries.first?.requestID)
        XCTAssertEqual(coordinator.status, .offline)
        XCTAssertTrue(coordinator.canRetry(requestID: id), "The queue button must allow an explicit offline retry.")
        coordinator.editDraft(text: "new draft", modelID: nil)
        await transport.succeed()
        let automaticCalls = await transport.callCount()
        XCTAssertEqual(automaticCalls, 1, "Network recovery must not automatically resend unknown entries.")
        XCTAssertEqual(coordinator.status, .offline)
        coordinator.retry(requestID: id)
        XCTAssertFalse(coordinator.canRetry(requestID: id), "An active request disables the queue button.")
        try await waitUntil { coordinator.entries.first?.state == .received }
        XCTAssertFalse(coordinator.canRetry(requestID: id))
        let calls = await transport.recorded()
        XCTAssertEqual(calls.count, 2)
        XCTAssertEqual(calls[0], calls[1])
        coordinator.retry(requestID: id)
        let count = await transport.callCount()
        XCTAssertEqual(count, 2)
        XCTAssertEqual(coordinator.draft.text, "new draft")
    }

    func testUnknownRetryRequiresForegroundAndCurrentIdentity() async throws {
        let folder = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        addTeardownBlock { try FileManager.default.removeItem(at: folder) }
        let transport = SendingMockTransport()
        let coordinator = makeCoordinator(store: try makeStore(directory: folder))
        coordinator.activate(transport: transport, identity: identity(), apiBaseURL: URL(string: "https://example.test")!)
        coordinator.selectAgent("A")
        coordinator.editDraft(text: "unknown", modelID: nil)
        coordinator.enqueue()
        try await waitUntil { coordinator.entries.first?.state == .unknown }
        let id = try XCTUnwrap(coordinator.entries.first?.requestID)
        coordinator.setForeground(false)
        XCTAssertFalse(coordinator.canRetry(requestID: id))
        coordinator.retry(requestID: id)
        coordinator.setForeground(true)
        XCTAssertTrue(coordinator.canRetry(requestID: id))
        coordinator.disconnect()
        XCTAssertFalse(coordinator.canRetry(requestID: id))
        coordinator.retry(requestID: id)
        let calls = await transport.callCount()
        XCTAssertEqual(calls, 1)
    }

    func testAgentSwitchBeforeSendStartsLeavesQueuedEntryOperable() async throws {
        try await assertUnstartedSendCancellation {
            $0.selectAgent("B")
            $0.selectAgent("A")
        }
    }

    func testDisconnectBeforeSendStartsLeavesQueuedEntryOperable() async throws {
        try await assertUnstartedSendCancellation { $0.disconnect() }
    }

    func testBackgroundBeforeSendStartsLeavesQueuedEntryOperable() async throws {
        try await assertUnstartedSendCancellation { $0.setForeground(false) }
    }

    private func assertUnstartedSendCancellation(_ cancel: (SendingCoordinator) -> Void) async throws {
        let folder = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        addTeardownBlock { try FileManager.default.removeItem(at: folder) }
        let store = try makeStore(directory: folder)
        let transport = SendingMockTransport()
        let coordinator = makeCoordinator(store: store)
        let authority = identity()
        let url = URL(string: "https://example.test")!
        let scope = SendingScope(partition: try XCTUnwrap(ReadingPartition(apiBaseURL: url, identity: authority)),
                                 agentID: "A")
        coordinator.activate(transport: transport, identity: authority, apiBaseURL: url)
        coordinator.selectAgent("A")
        let source = folder.appendingPathComponent("photo.txt")
        try Data("photo".utf8).write(to: source)
        coordinator.stageAttachment(source: source, context: try XCTUnwrap(coordinator.attachmentImportContext))
        coordinator.editDraft(text: "queued", modelID: nil)
        coordinator.enqueue()
        let id = try XCTUnwrap(coordinator.entries.first?.requestID)
        var writes = 0
        store.beforeSave = { writes += 1 }
        // No suspension: cancel while the MainActor task has not started.
        cancel(coordinator)
        await transport.succeed()
        for _ in 0..<10 { await Task.yield() }
        XCTAssertEqual(writes, 0, "An invalidated task must not mutate persistent state.")
        XCTAssertEqual(store.entries(scope).first?.state, .queued)
        let uploads = await transport.uploadCount()
        let calls = await transport.callCount()
        XCTAssertEqual(uploads, 0)
        XCTAssertEqual(calls, 0)
        store.beforeSave = nil
        coordinator.activate(transport: transport, identity: authority, apiBaseURL: url)
        coordinator.setForeground(true)
        coordinator.selectAgent("A")
        XCTAssertTrue(coordinator.canRetry(requestID: id))
        coordinator.retry(requestID: id)
        try await waitUntil { coordinator.entries.first?.state == .received }
        coordinator.delete(requestID: id)
        XCTAssertTrue(coordinator.entries.isEmpty)
    }

    func testScopeSwitchRejectsLateReceiptAndCancelIsNotStop() async throws {
        let folder = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        addTeardownBlock { try FileManager.default.removeItem(at: folder) }
        let store = try makeStore(directory: folder)
        let transport = SendingMockTransport()
        await transport.delay()
        let coordinator = makeCoordinator(store: store)
        let url = URL(string: "https://example.test")!
        coordinator.activate(transport: transport, identity: identity(), apiBaseURL: url)
        coordinator.selectAgent("A")
        coordinator.editDraft(text: "old scope", modelID: nil)
        coordinator.enqueue()
        try await waitUntil { await transport.callCount() == 1 }
        coordinator.activate(transport: transport, identity: identity("other"), apiBaseURL: url)
        XCTAssertTrue(coordinator.entries.isEmpty)
        await transport.release()
        for _ in 0..<5 { await Task.yield() }
        XCTAssertTrue(coordinator.entries.isEmpty)
        let old = SendingScope(partition: try XCTUnwrap(ReadingPartition(apiBaseURL: url, identity: identity())), agentID: "A")
        XCTAssertEqual(store.entries(old).first?.state, .unknown)
        coordinator.setForeground(false)
        coordinator.disconnect()
        let stops = await transport.stopCount()
        XCTAssertEqual(stops, 0)
        coordinator.activate(transport: transport, identity: identity(), apiBaseURL: url)
        coordinator.setForeground(true)
        coordinator.selectAgent("A")
        await coordinator.stopAgent(runID: "observed-run")
        let explicitStops = await transport.stopCount()
        XCTAssertEqual(explicitStops, 1)
    }

    func testPreparedAttachmentPersistsAndUnknownRetryDoesNotReadMissingFile() async throws {
        let folder = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        addTeardownBlock { try FileManager.default.removeItem(at: folder) }
        let store = try makeStore(directory: folder)
        let transport = SendingMockTransport()
        let coordinator = makeCoordinator(store: store)
        let url = URL(string: "https://example.test")!
        coordinator.activate(transport: transport, identity: identity(), apiBaseURL: url)
        coordinator.selectAgent("A")
        let source = folder.appendingPathComponent("original.txt")
        try Data("immutable".utf8).write(to: source)
        coordinator.stageAttachment(source: source, context: try XCTUnwrap(coordinator.attachmentImportContext),
                                    contentType: "text/plain")
        let attachment = try XCTUnwrap(coordinator.draft.attachments.first)
        coordinator.enqueue()
        try await waitUntil { coordinator.entries.first?.state == .unknown }
        let entry = try XCTUnwrap(coordinator.entries.first)
        XCTAssertEqual(entry.payload?.attachments.first?.data, Data("immutable".utf8))
        coordinator.disconnect()
        try FileManager.default.removeItem(at: store.attachmentURL(attachment))
        let reopened = makeCoordinator(store: try makeStore(directory: folder))
        reopened.activate(transport: transport, identity: identity(), apiBaseURL: url)
        reopened.selectAgent("A")
        await transport.succeed()
        reopened.retry(requestID: entry.requestID)
        try await waitUntil { reopened.entries.first?.state == .received }
        let calls = await transport.recorded()
        XCTAssertEqual(calls.count, 2)
        XCTAssertEqual(calls[0], calls[1])
        let uploads = await transport.uploadCount()
        XCTAssertEqual(uploads, 1)
    }

    func testOfflineEnqueueAndNavigationPreserveDrafts() throws {
        let folder = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        addTeardownBlock { try FileManager.default.removeItem(at: folder) }
        let coordinator = makeCoordinator(store: try makeStore(directory: folder))
        coordinator.activate(transport: SendingMockTransport(), identity: identity(), apiBaseURL: URL(string: "https://example.test")!)
        coordinator.selectAgent("A")
        coordinator.editDraft(text: "A", modelID: "model")
        coordinator.selectAgent("B")
        coordinator.editDraft(text: "B", modelID: nil)
        coordinator.selectAgent("A")
        XCTAssertEqual(coordinator.draft.text, "A")
        coordinator.setForeground(false)
        coordinator.enqueue()
        XCTAssertEqual(coordinator.entries.first?.state, .queued)
        XCTAssertEqual(coordinator.status, .offline)
        coordinator.selectAgent("B")
        XCTAssertEqual(coordinator.draft.text, "B")
    }
}
