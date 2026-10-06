import Foundation
import XCTest
@testable import Holon

final class SharedImportStoreTests: XCTestCase {
    func testAttachmentByteCountsRejectCorruptAndOverBudgetRecords() throws {
        for counts in [[Int.max], [Int.min], [-1], Array(repeating: Int.max, count: 10),
                       Array(repeating: SharedImportStore.maxBytes, count: 10),
                       [SharedImportStore.maxBytes, 1]] {
            XCTAssertThrowsError(try SharedImportStore.validate(text: "", urls: [], attachmentBytes: counts)) {
                XCTAssertEqual($0 as? SharedImportError, .limitExceeded)
            }
        }
        XCTAssertNoThrow(try SharedImportStore.validate(text: "", urls: [],
            attachmentBytes: [SharedImportStore.maxBytes - 1, 1]))
    }

    func testSafeNamesUseUTF8BudgetAndPreserveWholeCharactersAndShortSuffixes() {
        let cases = [
            (String(repeating: "界", count: 100) + ".txt", String(repeating: "界", count: 83) + ".txt"),
            (String(repeating: "👩🏽‍💻", count: 30) + ".png", String(repeating: "👩🏽‍💻", count: 16) + ".png"),
            ("👩🏽‍💻.txt", "👩🏽‍💻.txt"),
            ("safe\u{202E}/\\:\u{0}\n.txt", "safe.txt"),
            (String(repeating: "a", count: 251) + ".txt", String(repeating: "a", count: 251) + ".txt"),
            (String(repeating: "a", count: 252) + ".txt", String(repeating: "a", count: 251) + ".txt"),
            (String(repeating: "界", count: 100), String(repeating: "界", count: 85)),
            (String(repeating: "界", count: 100) + "." + String(repeating: "x", count: 40),
             String(repeating: "界", count: 85)),
            ("e" + String(repeating: "\u{301}", count: 300) + ".txt", "attachment.txt"),
            ("../\\:\u{0}\n", "attachment")
        ]
        for (name, expected) in cases {
            let safe = SharedImportStore.safeName(name)
            XCTAssertEqual(safe, expected)
            XCTAssertLessThanOrEqual(safe.utf8.count, 255)
            XCTAssertEqual(SharedImportStore.safeName(safe), safe)
        }
    }

    func testSendingLimitsIncludeURLSeparatorsAndFileCount() throws {
        let store = try SharedImportStore(container: container())
        let file = SharedImportFile(name: "a", typeIdentifier: "public.data", data: Data())
        XCTAssertNoThrow(try store.stage(text: "", urls: [], files: Array(repeating: file, count: 10)))
        XCTAssertThrowsError(try store.stage(text: "", urls: [], files: Array(repeating: file, count: 11)))
        let url = URL(string: "https://example.test")!
        let text = String(repeating: "a", count: SharedImportStore.maxTextBytes - url.absoluteString.utf8.count - 2)
        XCTAssertNoThrow(try store.stage(text: text, urls: [url], files: []))
        XCTAssertThrowsError(try store.stage(text: text + "a", urls: [url], files: []))
        XCTAssertThrowsError(try store.stage(text: String(repeating: "界", count: 21846), urls: [], files: []))
        XCTAssertNoThrow(try SharedImportStore.validate(text: "text", urls: [], attachmentBytes: [SharedImportStore.maxBytes]))
        XCTAssertThrowsError(try SharedImportStore.validate(text: "", urls: [], attachmentBytes: [SharedImportStore.maxBytes, 1]))
    }

    func testRejectedInputsLeaveNoPartialStaging() throws {
        let directory = try container()
        let store = try SharedImportStore(container: directory)
        let file = SharedImportFile(name: "file", typeIdentifier: "public.data", data: Data())
        XCTAssertThrowsError(try store.stage(text: "", urls: [], files: Array(repeating: file, count: 11)))
        XCTAssertThrowsError(try store.stage(text: String(repeating: "界", count: 21846), urls: [], files: []))
        XCTAssertThrowsError(try store.stage(text: "", urls: [], files: [
            .init(name: "large", typeIdentifier: "public.data", data: Data(count: SharedImportStore.maxBytes)),
            .init(name: "extra", typeIdentifier: "public.data", data: Data([1]))
        ]))
        XCTAssertThrowsError(try SharedImportStore.readFile(directory.appendingPathComponent("missing"),
                                                           name: "missing", typeIdentifier: "public.data"))
        XCTAssertTrue(try store.load().isEmpty)
        XCTAssertEqual(try FileManager.default.contentsOfDirectory(
            atPath: directory.appendingPathComponent("SharedImports").path), [".lock"])
    }

    private func container() throws -> URL {
        let url = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
        addTeardownBlock { try FileManager.default.removeItem(at: url) }
        return url
    }

    func testTextLinkFilePreviewAndRestartRecovery() throws {
        let directory = try container()
        let store = try SharedImportStore(container: directory)
        let bytes = Data("file preview".utf8)
        let payload = try store.stage(text: "Shared text", urls: [URL(string: "https://example.test/a")!],
                                      files: [.init(name: "../a/b.txt", typeIdentifier: "public.plain-text", data: bytes)])
        let reopened = try SharedImportStore(container: directory)
        XCTAssertEqual(try reopened.load(), [payload])
        XCTAssertEqual(payload.text, "Shared text")
        XCTAssertEqual(payload.urls.first?.host, "example.test")
        XCTAssertFalse(payload.attachments[0].name.contains("/"))
        XCTAssertEqual(try reopened.attachmentData(payloadID: payload.id, attachmentID: payload.attachments[0].id), bytes)
        try reopened.consume(id: payload.id, enqueueSucceeded: false)
        XCTAssertEqual(try reopened.load().count, 1)
        try reopened.consume(id: payload.id, enqueueSucceeded: true)
        XCTAssertTrue(try reopened.load().isEmpty)
    }

    func testPreviewCancellationDoesNotStageOrLeaveFiles() throws {
        let directory = try container()
        let store = try SharedImportStore(container: directory)
        let source = directory.appendingPathComponent("provider.txt")
        try Data("preview".utf8).write(to: source)
        let preview = try SharedImportStore.readFile(source, name: "provider.txt", typeIdentifier: "public.text")
        XCTAssertEqual(String(data: preview.data, encoding: .utf8), "preview")
        // Cancellation never invokes stage; provider data remains only in memory.
        XCTAssertTrue(try store.load().isEmpty)
        let entries = try FileManager.default.contentsOfDirectory(atPath: directory.appendingPathComponent("SharedImports").path)
        XCTAssertEqual(entries, [".lock"])
    }

    func testUnreadableOversizeUnsupportedAndRecordLimits() throws {
        let directory = try container()
        let store = try SharedImportStore(container: directory)
        XCTAssertThrowsError(try SharedImportStore.readFile(directory.appendingPathComponent("missing"), name: "x", typeIdentifier: "public.data"))
        XCTAssertThrowsError(try SharedImportStore.readFile(directory, name: "directory", typeIdentifier: "public.data"))
        XCTAssertThrowsError(try store.stage(text: "", urls: [URL(fileURLWithPath: "/private/secret")], files: []))
        XCTAssertThrowsError(try store.stage(text: "", urls: [], files: [.init(name: "large", typeIdentifier: "public.data", data: Data(count: SharedImportStore.maxBytes + 1))]))
        XCTAssertThrowsError(try store.stage(text: "", urls: Array(repeating: URL(string: "https://example.test")!, count: 21), files: []))
        XCTAssertTrue(try store.load().isEmpty)
        for _ in 0..<SharedImportStore.maxRecords { try store.stage(text: "text", urls: [], files: []) }
        XCTAssertThrowsError(try store.stage(text: "overflow", urls: [], files: []))
        XCTAssertEqual(try store.load().count, SharedImportStore.maxRecords)
    }

    func testAtomicRecordAndInterruptedWriterRecovery() throws {
        let directory = try container()
        let store = try SharedImportStore(container: directory)
        let pending = directory.appendingPathComponent("SharedImports/.pending-" + UUID().uuidString)
        try FileManager.default.createDirectory(at: pending, withIntermediateDirectories: true)
        try Data("incomplete".utf8).write(to: pending.appendingPathComponent("file"))
        XCTAssertTrue(try store.load().isEmpty)
        XCTAssertFalse(FileManager.default.fileExists(atPath: pending.path))
        let payload = try store.stage(text: "complete", urls: [], files: [])
        let record = directory.appendingPathComponent("SharedImports/\(payload.id.uuidString)/record.json")
        let object = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(contentsOf: record)) as? [String: Any])
        XCTAssertEqual(Set(object.keys), ["id", "createdAt", "text", "urls", "attachments"])
        XCTAssertEqual(try store.load(), [payload])
    }

    func testExternalSymlinkCannotBecomeManagedAttachment() throws {
        let directory = try container()
        let store = try SharedImportStore(container: directory)
        let payload = try store.stage(text: "", urls: [], files: [.init(name: "file", typeIdentifier: "public.data", data: Data("a".utf8))])
        let attachment = directory.appendingPathComponent("SharedImports/\(payload.id.uuidString)/\(payload.attachments[0].id.uuidString)")
        try FileManager.default.removeItem(at: attachment)
        try FileManager.default.createSymbolicLink(at: attachment, withDestinationURL: directory.appendingPathComponent("outside"))
        XCTAssertThrowsError(try store.attachmentData(payloadID: payload.id, attachmentID: payload.attachments[0].id))
        XCTAssertEqual(try store.load().count, 1)
        try store.discard(id: payload.id)
        XCTAssertTrue(try store.load().isEmpty)
    }

    func testUnconfiguredGroupFailsClosed() {
        XCTAssertThrowsError(try SharedImportStore.configured(bundle: Bundle(for: Self.self))) { error in
            XCTAssertEqual(error as? SharedImportError, .unavailableAppGroup)
        }
    }

    func testIndependentStoresSerializeConcurrentWriters() throws {
        let directory = try container()
        let first = try SharedImportStore(container: directory)
        let second = try SharedImportStore(container: directory)
        DispatchQueue.concurrentPerform(iterations: 10) { index in
            _ = try? (index.isMultiple(of: 2) ? first : second).stage(text: "item \(index)", urls: [], files: [])
        }
        let records = try first.load()
        XCTAssertEqual(records.count, 10)
        XCTAssertEqual(Set(records.map(\.id)).count, 10)
    }
}
