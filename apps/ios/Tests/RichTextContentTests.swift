import Foundation
import XCTest
import UIKit
@testable import Holon

@MainActor
final class RichTextContentTests: XCTestCase {
    func testAbsoluteMarkdownLinksDecodeOnceAndBarePathsStayLiteral() throws {
        let content = try HolonMarkdownParser().attributedString(for:
            "中文 👩🏽‍💻 [报告](/tmp/report%2520.md#L2)；打开：/tmp/结果%20.md。 HTTPS https://example.test/a.md\n\n`/tmp/with space#1?.md`\n\n```\n/tmp/source.md\n```")
        XCTAssertEqual(content.runs.compactMap(\.link).map(RichTextLink.classify), [
            .reference("/tmp/report%20.md"), .reference("/tmp/结果%20.md"),
            .external(URL(string: "https://example.test/a.md")!), .reference("/tmp/with space#1?.md")
        ])
    }
    func testBarePathBoundariesExcludeURLsAndExistingLinks() throws {
        let content = try HolonMarkdownParser().attributedString(for:
            "/tmp/a.txt and (/tmp/b.txt), x=/tmp/c.txt; https://example.test/a //remote/a x/tmp/not-path [label](/tmp/explicit.md)")
        XCTAssertEqual(content.runs.compactMap(\.link).map(RichTextLink.classify), [
            .reference("/tmp/a.txt"), .reference("/tmp/b.txt"), .reference("/tmp/c.txt"),
            .external(URL(string: "https://example.test/a")!), .reference("/tmp/explicit.md")
        ])
    }
    func testOperatorInputRemainsVerbatimWhilePathsAreInteractive() {
        let input = "**literal**\n\n请查看：/tmp/a%20.md\nhttps://example.test/a"
        let attributed = OperatorMessageText.attributed(input)
        XCTAssertEqual(String(attributed.characters), input)
        XCTAssertEqual(attributed.runs.compactMap(\.link).map(RichTextLink.classify), [.reference("/tmp/a%20.md")])
    }
    func testNativeSelectionPreservesVerbatimBlocksAndDisablesActiveLinks() {
        let text = "# Result\n\nFirst paragraph.\n\nSecond paragraph.\n\n- one\n- two\n\n[Sibling](./note.txt)\n\n```swift\nlet x = 1\n```"
        let view = RichTextSelectionView.makeTextView(text: text)
        XCTAssertEqual(view.text, text, "Do not flatten presentation intents and concatenate blocks")
        XCTAssertFalse(view.isEditable); XCTAssertTrue(view.isSelectable)
        XCTAssertTrue(view.dataDetectorTypes.isEmpty)
        XCTAssertTrue(view.adjustsFontForContentSizeCategory)
        XCTAssertFalse(view.attributedText.containsAttribute(.link))
    }
    func testMarkdownKeepsStructureAndMixedLanguageText() throws {
        let content = try HolonMarkdownParser().attributedString(for: "# Result\n\n**Ready** 中文\n\n- first\n- second\n\n> note\n\n```swift\nlet x = 1\n```")
        XCTAssertTrue(String(content.characters).contains("Ready 中文"))
        XCTAssertFalse(String(content.characters).contains("**Ready**"))
        XCTAssertTrue(content.runs.contains { run in
            run.presentationIntent?.components.contains { if case .header = $0.kind { return true }; return false } == true
        })
        XCTAssertTrue(content.runs.contains { run in
            run.presentationIntent?.components.contains { if case .codeBlock = $0.kind { return true }; return false } == true
        })
    }

    func testImagesArePassiveLinksAndNeverAttachmentRequests() throws {
        let content = try HolonMarkdownParser().attributedString(for: "![Report](https://other.example/image.png)\n\n![Private](workspace://root/file.png)")
        XCTAssertTrue(content.runs.allSatisfy { $0.imageURL == nil })
        XCTAssertTrue(content.runs.contains { $0.link?.absoluteString == "https://other.example/image.png" })
        XCTAssertTrue(content.runs.contains { $0.link?.scheme == "workspace" })
    }

    func testTablesAndTasksRemainReadableAndExplicitURIsAreLinkedOutsideCodeBlocks() throws {
        let content = try HolonMarkdownParser().attributedString(for: "| File | State |\n| --- | --- |\n| a.md | Done |\n\n- [x] Complete\n- [ ] Next\n\nOpen workspace://w/report.md?root=r\n\n```\nworkspace://w/do-not-link.md?root=r\n```")
        XCTAssertTrue(String(content.characters).contains("Complete"))
        XCTAssertTrue(String(content.characters).contains("☑ Complete"))
        XCTAssertTrue(String(content.characters).contains("☐ Next"))
        XCTAssertTrue(content.runs.contains { run in
            run.presentationIntent?.components.contains { if case .table = $0.kind { return true }; return false } == true
        })
        XCTAssertEqual(content.runs.compactMap(\.link).map(\.absoluteString), ["workspace://w/report.md?root=r"])
    }

    func testLiteralInlinePathDoesNotDecodePercentOrLoseQueryLikeCharacters() throws {
        let path = "/tmp/report%20#1?.md"
        let content = try HolonMarkdownParser().attributedString(for: "Open `\(path)`\n\n```\n/tmp/do-not-link.md\n```")
        let links = content.runs.compactMap(\.link)
        XCTAssertEqual(links.count, 1)
        XCTAssertEqual(RichTextLink.classify(try XCTUnwrap(links.first)), .reference(path))
    }

    func testBareURIsExcludeClosingDelimitersAndSentencePunctuation() throws {
        let content = try HolonMarkdownParser().attributedString(for: "See workspace://w/report.md?root=r. (file:///tmp/a.txt), 'workspace://w/b.md?root=r'; 中文 workspace://w/c.md?root=r。")
        XCTAssertEqual(content.runs.compactMap(\.link).map(\.absoluteString), [
            "workspace://w/report.md?root=r", "file:///tmp/a.txt", "workspace://w/b.md?root=r", "workspace://w/c.md?root=r"
        ])
        let literal = try HolonMarkdownParser().attributedString(for: "[Exact](workspace://w/a.?root=r.) and `workspace://w/b.?root=r.`")
        XCTAssertEqual(literal.runs.compactMap(\.link).map(\.absoluteString), ["workspace://w/a.?root=r.", "workspace://w/b.?root=r."])
    }

    func testCheckboxOnlyChangesAListItemPrefixNotBoldOrInlineCodeText() throws {
        let content = try HolonMarkdownParser().attributedString(for: "- Ordinary **[x] literal** remains.\n- `[x] code` remains.\n- **[x] bold prefix** remains.\n- [[x] link prefix](https://example.com) remains.\n- [ ] **Real** task\n- [x] Done")
        let text = String(content.characters)
        XCTAssertTrue(text.contains("Ordinary [x] literal remains."))
        XCTAssertTrue(text.contains("[x] code remains."))
        XCTAssertTrue(text.contains("[x] bold prefix remains.")); XCTAssertTrue(text.contains("[x] link prefix remains."))
        XCTAssertTrue(text.contains("☐ Real task")); XCTAssertTrue(text.contains("☑ Done"))
    }

    func testUnsafeSchemesCredentialsAndNonlocalFileHostsFailClosed() {
        for link in ["javascript:alert(1)", "data:text/html,test", "file://remote/tmp/a", "file:///tmp/a?secret=x",
                     "https://user:password@example.com", "holon-path:///a?query=1", "relative.md?query=1"] {
            XCTAssertEqual(RichTextLink.classify(URL(string: link)!), .unsupported, link)
        }
        XCTAssertEqual(RichTextLink.classify(URL(string: "file://localhost/tmp/a%20b.md#L1")!), .reference("/tmp/a b.md"))
        XCTAssertEqual(RichTextLink.classify(URL(string: "workspace://w/a.md?root=r#L1")!), .reference("workspace://w/a.md?root=r"))
        if case .external = RichTextLink.classify(URL(string: "http://example.com")!) {} else { XCTFail("HTTP remains a supported external link") }
    }

    func testRelativeReferencesRequireAReaderBaseAndDecodeOnlyOnce() throws {
        XCTAssertEqual(RichTextLink.classify(URL(string: "../notes%2520.md#L2")!), .relative("../notes%20.md"))
        let path = "../notes%20#1?.md"
        XCTAssertEqual(RichTextLink.classify(try XCTUnwrap(RichTextLink.literalRelativeURL(path))), .relative(path))
        XCTAssertNil(RichTextLink.literalRelativeURL("bare.md"))
        let input = "`../notes.md`\n\n```\n../literal.md\n```"
        XCTAssertTrue(try HolonMarkdownParser().attributedString(for: input).runs.compactMap(\.link).isEmpty)
        XCTAssertEqual(try HolonMarkdownParser(allowsRelativePaths: true).attributedString(for: input)
            .runs.compactMap(\.link).map(RichTextLink.classify), [.relative("../notes.md")])
    }

    func testAttachmentDoesNotTreatUnknownPayloadAsAHostPath() {
        XCTAssertNil(ReadingPresentation.attachmentReference(.object(["value": .string("/tmp/untyped")])) )
        XCTAssertNil(ReadingPresentation.attachmentReference(.object(["uri": .string("https://example.com/report")])) )
        XCTAssertEqual(ReadingPresentation.attachmentReference(.object(["uri": .string("workspace://w/report.md?root=r")])), "workspace://w/report.md?root=r")
    }

    func testExplicitRelativeMarkdownLinksUseOpaqueRoutableScheme() throws {
        let content = try HolonMarkdownParser(allowsRelativePaths: true).attributedString(for:
            "[Sibling](./notes%2520.md#L2) and [Bare](notes.md) and [Invalid](relative.md?secret=x)")
        let links = content.runs.compactMap(\.link)
        XCTAssertEqual(links.map(\.scheme), ["holon-relative", "holon-relative", "holon-unsupported"])
        XCTAssertEqual(links.map(RichTextLink.classify), [.relative("./notes%20.md"), .relative("notes.md"), .unsupported])
        XCTAssertNil(RichTextLink.relativeURL("/absolute"))
        XCTAssertNil(RichTextLink.relativeURL("../bad\\name"))
    }
}

private extension NSAttributedString {
    func containsAttribute(_ name: NSAttributedString.Key) -> Bool {
        var found = false
        enumerateAttribute(name, in: NSRange(location: 0, length: length)) { value, _, _ in
            if value != nil { found = true }
        }
        return found
    }
}
