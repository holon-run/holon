import Foundation
import HolonClient
import XCTest
@testable import Holon

@MainActor
final class SendingStoreTests: XCTestCase {
    private func scope(_ agent: String = "A", user: String = "user") throws -> SendingScope {
        let identity = HolonConnectionIdentity(networkID: "wifi", runtimeID: "runtime",
                                               userID: user, visibilityScopeID: "private")
        return SendingScope(partition: try XCTUnwrap(ReadingPartition(
            apiBaseURL: URL(string: "https://example.test/api")!, identity: identity)), agentID: agent)
    }

    private func makeStore(directory: URL, maximumAttachmentBytes: Int64 = 20 * 1024 * 1024) throws -> SendingStore {
        let store = try SendingStore(directory: directory, maximumAttachmentBytes: maximumAttachmentBytes)
        addTeardownBlock {
            try await MainActor.run { try store.closeForTesting() }
        }
        return store
    }

    func testAtomicEnqueueRollbackAndReopenRecovery() throws {
        let folder = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        addTeardownBlock { try FileManager.default.removeItem(at: folder) }
        let scope = try scope()
        let store = try makeStore(directory: folder)
        try store.saveDraft(SendingDraft(text: "hello", modelID: "model"), scope: scope)
        store.beforeSave = { throw SendingFailure.rejected("disk failure") }
        XCTAssertThrowsError(try store.enqueue(scope))
        XCTAssertEqual(store.draft(scope).text, "hello")
        XCTAssertTrue(store.entries(scope).isEmpty)
        store.beforeSave = nil
        var entry = try store.enqueue(scope)
        XCTAssertEqual(store.draft(scope).text, "")
        XCTAssertEqual(store.draft(scope).modelID, "model")
        entry.state = .sending
        entry.payload = SendingPayload(text: "hello", modelID: "model", attachments: [])
        try store.update(entry)
        let reopened = try makeStore(directory: folder)
        let recovered = try XCTUnwrap(reopened.entries(scope).first)
        XCTAssertEqual(recovered.state, .unknown)
        XCTAssertEqual(recovered.requestID, entry.requestID)
        XCTAssertEqual(recovered.payload, entry.payload)
        XCTAssertTrue(reopened.draft(scope).text.isEmpty)
        var mutation = recovered
        mutation.payload = SendingPayload(text: "changed", modelID: nil, attachments: [])
        XCTAssertThrowsError(try reopened.update(mutation))
    }

    func testCanonicalJoinPersistsWithoutMutatingAcceptedRequest() throws {
        let folder = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        addTeardownBlock { try FileManager.default.removeItem(at: folder) }
        let scope = try scope()
        let store = try makeStore(directory: folder)
        try store.saveDraft(SendingDraft(text: "hello"), scope: scope)
        var entry = try store.enqueue(scope)
        entry.state = .received
        entry.messageID = "canonical-message"
        try store.update(entry)
        entry.canonicalObserved = true
        try store.update(entry)
        var forbidden = entry
        forbidden.state = .unknown
        XCTAssertThrowsError(try store.update(forbidden))
        forbidden = entry
        forbidden.canonicalObserved = false
        XCTAssertThrowsError(try store.update(forbidden))
        let reopened = try makeStore(directory: folder)
        XCTAssertEqual(reopened.entries(scope).first, entry)
        XCTAssertTrue(LocalMessageProjection.visible(reopened.entries(scope), canonicalIDs: []).isEmpty)
    }

    func testCompletePromptBudgetFailurePreservesDraftAndManagedReferences() throws {
        let folder = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        addTeardownBlock { try FileManager.default.removeItem(at: folder) }
        let scope = try scope()
        let store = try makeStore(directory: folder)
        let source = folder.appendingPathComponent("source.bin")
        try Data(repeating: 0, count: 13 * 1024 * 1024).write(to: source)
        try store.saveDraft(SendingDraft(text: "hello", modelID: "model"), scope: scope)
        try store.stage(source: source, scope: scope)
        try store.stage(source: source, scope: scope)
        let original = store.draft(scope)
        XCTAssertThrowsError(try store.enqueue(scope)) {
            guard case SendingFailure.rejected = $0 else {
                return XCTFail("Expected full prompt budget rejection, got \($0)")
            }
        }
        XCTAssertEqual(store.draft(scope), original)
        XCTAssertTrue(store.entries(scope).isEmpty)
        for attachment in original.attachments {
            XCTAssertTrue(FileManager.default.fileExists(atPath: store.attachmentURL(attachment).path))
        }
        try store.closeForTesting()
        let reopened = try makeStore(directory: folder)
        XCTAssertEqual(reopened.draft(scope), original)
        XCTAssertTrue(reopened.entries(scope).isEmpty)
        var smaller = original
        smaller.attachments.removeLast()
        try reopened.saveDraft(smaller, scope: scope)
        let entry = try reopened.enqueue(scope)
        XCTAssertEqual(entry.draft, smaller)
        XCTAssertTrue(reopened.draft(scope).attachments.isEmpty)
    }

    func testLastDraftReferenceRemovalCleansOnlyManagedCopy() throws {
        let folder = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        addTeardownBlock { try FileManager.default.removeItem(at: folder) }
        let store = try makeStore(directory: folder)
        let a = try scope()
        let source = folder.appendingPathComponent("source.txt")
        try Data("abc".utf8).write(to: source)
        try store.stage(source: source, scope: a)
        let attachment = try XCTUnwrap(store.draft(a).attachments.first)
        let inFlight = folder.appendingPathComponent("attachments").appendingPathComponent(UUID().uuidString)
        try Data("pending".utf8).write(to: inFlight)
        try store.saveDraft(SendingDraft(), scope: a)
        XCTAssertFalse(FileManager.default.fileExists(atPath: store.attachmentURL(attachment).path))
        XCTAssertTrue(FileManager.default.fileExists(atPath: source.path))
        XCTAssertTrue(FileManager.default.fileExists(atPath: inFlight.path))
    }

    func testSharedReferencesAcrossScopesDraftsAndQueuesProtectCopy() throws {
        let folder = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        addTeardownBlock { try FileManager.default.removeItem(at: folder) }
        let store = try makeStore(directory: folder)
        let a = try scope()
        let b = try scope("B", user: "other")
        let source = folder.appendingPathComponent("source.txt")
        try Data("abc".utf8).write(to: source)
        try store.stage(source: source, scope: a)
        let shared = store.draft(a)
        let attachment = try XCTUnwrap(shared.attachments.first)
        let path = store.attachmentURL(attachment).path
        try store.saveDraft(shared, scope: b)
        let first = try store.enqueue(a)
        XCTAssertTrue(FileManager.default.fileExists(atPath: path))
        try store.delete(requestID: first.requestID, scope: a)
        XCTAssertTrue(FileManager.default.fileExists(atPath: path))
        try store.saveDraft(shared, scope: a)
        let second = try store.enqueue(b)
        let third = try store.enqueue(a)
        try store.delete(requestID: second.requestID, scope: b)
        XCTAssertTrue(FileManager.default.fileExists(atPath: path))
        try store.delete(requestID: third.requestID, scope: a)
        XCTAssertFalse(FileManager.default.fileExists(atPath: path))
    }

    func testFailedSavesPreserveReferencesAndCleanNewStagedCopy() throws {
        let folder = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        addTeardownBlock { try FileManager.default.removeItem(at: folder) }
        let store = try makeStore(directory: folder)
        let a = try scope()
        let source = folder.appendingPathComponent("source.txt")
        try Data("abc".utf8).write(to: source)
        try store.stage(source: source, scope: a)
        let shared = store.draft(a)
        let attachment = try XCTUnwrap(shared.attachments.first)
        store.beforeSave = { throw SendingFailure.rejected("disk failure") }
        XCTAssertThrowsError(try store.saveDraft(SendingDraft(), scope: a))
        XCTAssertEqual(store.draft(a), shared)
        XCTAssertNoThrow(try store.validate(attachment))
        XCTAssertThrowsError(try store.stage(source: source, scope: a))
        let files = try FileManager.default.contentsOfDirectory(
            at: folder.appendingPathComponent("attachments"), includingPropertiesForKeys: nil)
        XCTAssertEqual(files.map(\.lastPathComponent), [attachment.id.uuidString])
        store.beforeSave = nil
        let entry = try store.enqueue(a)
        store.beforeSave = { throw SendingFailure.rejected("disk failure") }
        XCTAssertThrowsError(try store.delete(requestID: entry.requestID, scope: a))
        XCTAssertEqual(store.entries(a), [entry])
        XCTAssertNoThrow(try store.validate(attachment))
        let reopened = try makeStore(directory: folder)
        XCTAssertEqual(reopened.entries(a), [entry])
        XCTAssertNoThrow(try reopened.validate(attachment))
    }

    func testNavigationIsolationAndOwnedMissingOversizedAttachments() throws {
        let folder = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        addTeardownBlock { try FileManager.default.removeItem(at: folder) }
        let store = try makeStore(directory: folder, maximumAttachmentBytes: 3)
        let a = try scope()
        let b = try scope("B")
        let other = try scope(user: "other")
        let source = folder.appendingPathComponent("source.txt")
        try Data("abc".utf8).write(to: source)
        try store.stage(source: source, scope: a)
        try FileManager.default.removeItem(at: source)
        let attachment = try XCTUnwrap(store.draft(a).attachments.first)
        XCTAssertNoThrow(try store.validate(attachment))
        XCTAssertTrue(store.draft(b).attachments.isEmpty)
        XCTAssertTrue(store.draft(other).attachments.isEmpty)
        try Data("abcd".utf8).write(to: source)
        XCTAssertThrowsError(try store.stage(source: source, scope: a))
        XCTAssertEqual(store.draft(a).attachments.count, 1)
        try FileManager.default.removeItem(at: store.attachmentURL(attachment))
        XCTAssertThrowsError(try store.enqueue(a))
        XCTAssertEqual(store.draft(a).attachments.count, 1)
        XCTAssertTrue(store.entries(a).isEmpty)
    }
}
