import Foundation
import HolonClient
import XCTest
@testable import Holon

@MainActor
final class FilesPresentationTests: XCTestCase {
    func testFilterAndDirectoryOrderingRetainSourceRoot() {
        let workspace = FilesWorkspace(workspaceID: "ws", executionRootID: "worktree", name: "Source")
        let directory = FilesDirectory(workspace: workspace, path: "", entries: [
            FilesEntry(name: ".hidden", path: ".hidden", isDirectory: false),
            FilesEntry(name: "Readme.md", path: "Readme.md", isDirectory: false),
            FilesEntry(name: "docs", path: "docs", isDirectory: true)
        ])
        XCTAssertEqual(directory.filtered(query: "", showHidden: false).map(\.name), ["docs", "Readme.md"])
        XCTAssertEqual(directory.filtered(query: "README", showHidden: true).map(\.name), ["Readme.md"])
        XCTAssertEqual(directory.filtered(query: "hidden", showHidden: true).count, 1)
        XCTAssertEqual(directory.workspace.executionRootID, "worktree")
    }

    func testHTMLJavaScriptAndMarkdownArePassiveLiteralTextAndSVGIsNotDecoded() {
        XCTAssertEqual(FilesPreviewKind.classify(mediaType: "text/html", name: "index.html"), .text)
        XCTAssertEqual(FilesPreviewKind.classify(mediaType: "application/javascript", name: "app.js"), .text)
        XCTAssertEqual(FilesPreviewKind.classify(mediaType: "text/markdown", name: "README.md"), .text)
        XCTAssertEqual(FilesPreviewKind.classify(mediaType: "image/svg+xml", name: "image.svg"), .downloadOnly)
        XCTAssertEqual(FilesPreviewKind.classify(mediaType: "application/pdf", name: "artifact.pdf"), .pdf)
    }

    func testTextPreviewTrimsOnlyUTF8ScalarCrossingByteBudget() throws {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: root) }
        let cache = FilesCache(root: root)
        try cache.bind(HolonConnectionIdentity(networkID: "network", runtimeID: "runtime", userID: "user",
                                               visibilityScopeID: "scope"))
        for scalar in ["é", "中", "😀"] {
            for includedBytes in 1..<scalar.utf8.count {
                let expected = String(repeating: "a", count: FilesCache.maximumTextBytes - includedBytes)
                let source = Data((expected + scalar + "tail").utf8)
                let preview = try cache.prepare(FilesDownload(data: source, mediaType: "text/plain", name: "large.txt"))
                XCTAssertEqual(preview.text, expected, "\(scalar), \(includedBytes) bytes")
                XCTAssertTrue(preview.truncated)
                XCTAssertLessThanOrEqual(try XCTUnwrap(preview.text).utf8.count, FilesCache.maximumTextBytes)
                XCTAssertEqual(try Data(contentsOf: preview.url), source)
            }
        }
        cache.clear()
    }

    func testTextPreviewKeepsCompleteUTF8ScalarAtByteBudget() throws {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: root) }
        let cache = FilesCache(root: root)
        try cache.bind(HolonConnectionIdentity(networkID: "network", runtimeID: "runtime", userID: "user",
                                               visibilityScopeID: "scope"))
        for scalar in ["é", "中", "😀"] {
            let expected = String(repeating: "a", count: FilesCache.maximumTextBytes - scalar.utf8.count) + scalar
            for suffix in ["", "tail"] {
                let preview = try cache.prepare(FilesDownload(data: Data((expected + suffix).utf8),
                                                              mediaType: "text/plain", name: "boundary.txt"))
                XCTAssertEqual(preview.text, expected)
                XCTAssertEqual(preview.text?.utf8.count, FilesCache.maximumTextBytes)
                XCTAssertEqual(preview.truncated, !suffix.isEmpty)
            }
        }
        cache.clear()
    }

    func testTextPreviewRejectsInvalidUTF8WithoutReplacement() throws {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: root) }
        let cache = FilesCache(root: root)
        try cache.bind(HolonConnectionIdentity(networkID: "network", runtimeID: "runtime", userID: "user",
                                               visibilityScopeID: "scope"))
        let invalidCrossing = Data(repeating: 65, count: FilesCache.maximumTextBytes - 1) + Data([0xE0, 0x80, 0x80])
        let invalidPrefix = Data([0xFF]) + Data(repeating: 65, count: FilesCache.maximumTextBytes)
        for source in [Data([0xFF]), Data([0xC3]), Data([0xC0, 0xAF]), Data([0xED, 0xA0, 0x80]),
                       invalidCrossing, invalidPrefix] {
            let preview = try cache.prepare(FilesDownload(data: source, mediaType: "text/plain", name: "invalid.txt"))
            XCTAssertNil(preview.text)
            XCTAssertEqual(preview.truncated, source.count > FilesCache.maximumTextBytes)
        }
        cache.clear()
    }

    func testCacheIsIdentityPartitionedBoundedAndRemovedOnRebind() throws {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: root) }
        let cache = FilesCache(root: root)
        let first = HolonConnectionIdentity(networkID: "network", runtimeID: "runtime", userID: "user-secret",
                                            visibilityScopeID: "scope")
        try cache.bind(first)
        let script = "<script>alert('not executed')</script>"
        let artifact = try cache.prepare(FilesDownload(data: Data(script.utf8),
                                                       mediaType: "text/html", name: "../../index.html"))
        XCTAssertEqual(artifact.text, script)
        XCTAssertFalse(artifact.url.path.contains("user-secret"))
        XCTAssertFalse(artifact.name.contains("/"))
        XCTAssertTrue(cache.owns(artifact))
        XCTAssertThrowsError(try cache.prepare(FilesDownload(data: Data(count: FilesCache.maximumBytes + 1),
                                                            mediaType: "text/plain", name: "large"))) {
            XCTAssertEqual($0 as? FilesFailure, .tooLarge)
        }
        try cache.bind(HolonConnectionIdentity(networkID: "other", runtimeID: "runtime", userID: "user",
                                               visibilityScopeID: "scope"))
        XCTAssertFalse(cache.owns(artifact))
        XCTAssertFalse(FileManager.default.fileExists(atPath: artifact.url.path))
        let truncated = try cache.prepare(FilesDownload(data: Data(repeating: 65, count: FilesCache.maximumTextBytes + 1),
                                                        mediaType: "text/plain", name: "large.txt"))
        XCTAssertTrue(truncated.truncated)
        XCTAssertEqual(truncated.text?.utf8.count, FilesCache.maximumTextBytes)
        cache.clear()
        XCTAssertFalse(FileManager.default.fileExists(atPath: truncated.url.path))
    }

    func testReferenceErrorsDoNotInventCanonicalFallback() throws {
        let raw: JSONValue = .object(["results": .array([.object([
            "status": .string("resolved"), "location": .object([
                "workspace_id": .string("ws"), "execution_root_id": .string("source-root"),
                "path": .string("plan.md"), "kind": .string("file")
            ])
        ])])])
        let (workspace, path) = try FilesClientTransport.resolved(raw)
        XCTAssertEqual(workspace.executionRootID, "source-root")
        XCTAssertEqual(path, "plan.md")
        for (reason, expected) in [("forbidden", FilesFailure.forbidden), ("root_removed", .rootUnavailable),
                                   ("unsupported_reference", .unsupported), ("ambiguous_root", .invalidReference)] {
            let result: JSONValue = .object(["results": .array([.object([
                "status": .string("unresolved"), "reason": .string(reason)
            ])])])
            XCTAssertThrowsError(try FilesClientTransport.resolved(result)) {
                XCTAssertEqual($0 as? FilesFailure, expected)
            }
        }
    }
}
