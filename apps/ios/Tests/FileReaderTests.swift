import Foundation
import UIKit
import XCTest
@testable import Holon

@MainActor
final class FileReaderTests: XCTestCase {
    private func file(_ data: Data) throws -> URL {
        let url = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try data.write(to: url); return url
    }

    func testDiskPagesReachBeyondOldPreviewAndPreserveEveryUTF8Byte() async throws {
        let source = String(repeating: "中😀é word\n", count: 60_000) + "FINAL 全文末尾"
        let url = try file(Data(source.utf8)); defer { try? FileManager.default.removeItem(at: url) }
        let index = try FileTextIndex.build(url: url)
        XCTAssertGreaterThan(index.byteCount, 512 * 1024)
        XCTAssertGreaterThan(index.pages.count, 30)
        let reader = FileTextReader(url: url, index: index)
        var restored = ""
        for page in index.pages.indices {
            let text = try await reader.page(page)
            XCTAssertLessThanOrEqual(text.utf8.count, FileTextIndex.pageBytes)
            restored += text
        }
        XCTAssertEqual(restored, source)
        let final = try await reader.page(index.pages.count - 1)
        XCTAssertTrue(final.hasSuffix("FINAL 全文末尾"))
    }

    func testLongSingleLineSplitsOnlyAtScalarBoundaries() throws {
        for scalar in ["é", "中", "😀"] {
            let source = Data((String(repeating: scalar, count: 40_000) + "END").utf8)
            let url = try file(source); defer { try? FileManager.default.removeItem(at: url) }
            let index = try FileTextIndex.build(url: url)
            XCTAssertEqual(index.pages.map(\.count).reduce(0, +), source.count)
            for range in index.pages { XCTAssertNotNil(String(data: source.subdata(in: range), encoding: .utf8)) }
        }
    }

    func testInvalidUTF8NearTheEndIsNotSilentlyReplaced() throws {
        let url = try file(Data(repeating: 65, count: 600_000) + Data([0xC3]))
        defer { try? FileManager.default.removeItem(at: url) }
        XCTAssertThrowsError(try FileTextIndex.build(url: url)) { XCTAssertEqual($0 as? FilesFailure, .invalidText) }
    }

    func testSearchFindsLiteralAcrossChunkBoundaryAndAtEnd() async throws {
        let source = String(repeating: "a", count: FileTextIndex.pageBytes - 3) + "Needle中" + String(repeating: "b", count: 40_000) + "final needle"
        let url = try file(Data(source.utf8)); defer { try? FileManager.default.removeItem(at: url) }
        let index = try FileTextIndex.build(url: url), reader = FileTextReader(url: url, index: index)
        let result = try await reader.matches("needle")
        XCTAssertTrue(result.contains(0)); XCTAssertTrue(result.contains(index.pages.count - 1))
        let empty = try await reader.matches(""); XCTAssertTrue(empty.isEmpty)
    }

    func testCodeFenceCannotEscapeIntoLinksOrMarkup() throws {
        let source = "let x = 1\n```\n[not a link](https://example.com)\n````\n"
        let parsed = try HolonMarkdownParser().attributedString(for: FileCodePresentation.markdownSource(source, language: "swift"))
        XCTAssertTrue(String(parsed.characters).contains(source.trimmingCharacters(in: .newlines)))
        XCTAssertTrue(parsed.runs.compactMap(\.link).isEmpty)
        XCTAssertEqual(FileCodePresentation.language(name: "hello.rs"), "rust")
        XCTAssertNil(FileCodePresentation.language(name: "unknown.bin"))
    }

    func testSplitSearchHitIsVisibleInBoundedPageContext() async throws {
        let source = String(repeating: "a", count: FileTextIndex.pageBytes - 2) + "needle" + String(repeating: "b", count: 40_000)
        let url = try file(Data(source.utf8)); defer { try? FileManager.default.removeItem(at: url) }
        let reader = FileTextReader(url: url, index: try FileTextIndex.build(url: url))
        let matches = try await reader.matches("needle")
        XCTAssertEqual(matches, [0])
        let context = try await reader.content(try XCTUnwrap(matches.first), searchQuery: "needle")
        XCTAssertTrue(context.includesNextPage); XCTAssertTrue(context.text.contains("needle"))
        XCTAssertLessThanOrEqual(context.text.utf8.count, 2 * FileTextIndex.pageBytes)
        let normal = try await reader.content(0)
        XCTAssertFalse(normal.includesNextPage); XCTAssertFalse(normal.text.contains("needle"))
    }

    func testSearchContextDoesNotAppendUnrelatedOrInvalidQueries() async throws {
        let url = try file(Data(("needle" + String(repeating: "b", count: 40_000)).utf8))
        defer { try? FileManager.default.removeItem(at: url) }
        let reader = FileTextReader(url: url, index: try FileTextIndex.build(url: url))
        for query in ["needle", "absent", "", String(repeating: "b", count: 513)] {
            let value = try await reader.content(0, searchQuery: query)
            XCTAssertFalse(value.includesNextPage)
            XCTAssertLessThanOrEqual(value.text.utf8.count, FileTextIndex.pageBytes)
        }
    }

    func testPDFRasterizationReachesEveryPageWithoutInteractiveActions() async throws {
        let url = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString + ".pdf")
        defer { try? FileManager.default.removeItem(at: url) }
        var box = CGRect(x: 0, y: 0, width: 100, height: 160)
        let context = try XCTUnwrap(CGContext(url as CFURL, mediaBox: &box, nil))
        for gray in [CGFloat(0), CGFloat(0.5)] {
            context.beginPDFPage(nil); context.setFillColor(CGColor(gray: gray, alpha: 1))
            context.fill(CGRect(x: 10, y: 10, width: 80, height: 140)); context.endPDFPage()
        }
        context.closePDF()
        let reader = FileRasterReader(url: url)
        let count = try await reader.pdfPageCount(); XCTAssertEqual(count, 2)
        let first = try await reader.image(page: 0), second = try await reader.image(page: 1)
        XCTAssertNotNil(UIImage(data: first)); XCTAssertNotNil(UIImage(data: second)); XCTAssertNotEqual(first, second)
        do { _ = try await reader.image(page: 2); XCTFail("Invalid page") } catch {}
    }

    func testModifiedSortStillKeepsFoldersFirst() {
        let directory = FilesDirectory(workspace: .init(workspaceID: "w", executionRootID: "r", name: "W"), path: "", entries: [
            .init(name: "old", path: "old", isDirectory: false, modified: Date(timeIntervalSince1970: 1)),
            .init(name: "folder", path: "folder", isDirectory: true),
            .init(name: "new", path: "new", isDirectory: false, modified: Date(timeIntervalSince1970: 2))
        ])
        XCTAssertEqual(directory.filtered(query: "", showHidden: false, sort: .modified).map(\.name), ["folder", "new", "old"])
    }
}
