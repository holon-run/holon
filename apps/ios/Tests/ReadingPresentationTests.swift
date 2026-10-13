import HolonClient
import XCTest
@testable import Holon

@MainActor
final class ReadingPresentationTests: XCTestCase {
    func testCachedContentDistinguishesBackgroundRefreshFromOffline() {
        XCTAssertEqual(ReadingPresentation.connectionStatusKey(
            status: .syncing, hasContent: true, isCached: true, isRefreshing: true
        ), "reading.cache.refreshing")
        XCTAssertEqual(ReadingPresentation.connectionStatusKey(
            status: .offline, hasContent: true, isCached: true, isRefreshing: false
        ), "reading.cache.offline")
        XCTAssertEqual(ReadingPresentation.connectionStatusKey(
            status: .offline, hasContent: false, isCached: false, isRefreshing: false
        ), "reading.status.offline")
        XCTAssertFalse(ReadingPresentation.showsConnectionRetry(status: .offline, isRefreshing: true))
        XCTAssertTrue(ReadingPresentation.showsConnectionRetry(status: .offline, isRefreshing: false))
    }

    func testAuthorizationAndProtocolErrorsAreNotDisguisedAsCacheRefresh() {
        for status in [ReadingStatus.permissionDenied, .sessionExpired, .incompatible] {
            XCTAssertEqual(ReadingPresentation.connectionStatusKey(
                status: status, hasContent: true, isCached: true, isRefreshing: true
            ), "reading.status." + status.rawValue)
        }
        XCTAssertFalse(ReadingPresentation.showsConnectionRetry(status: .sessionExpired, isRefreshing: false))
        XCTAssertFalse(ReadingPresentation.showsConnectionRetry(status: .permissionDenied, isRefreshing: false))
        XCTAssertTrue(ReadingPresentation.showsConnectionRetry(status: .incompatible, isRefreshing: false))
    }

    func testLiveContentHasNoCacheBannerUnlessStillUsingSavedContent() {
        XCTAssertNil(ReadingPresentation.connectionStatusKey(
            status: .live, hasContent: true, isCached: false, isRefreshing: false
        ))
        XCTAssertEqual(ReadingPresentation.connectionStatusKey(
            status: .live, hasContent: false, isCached: false, isRefreshing: false
        ), "reading.loadingConversation")
        XCTAssertEqual(ReadingPresentation.connectionStatusKey(
            status: .live, hasContent: true, isCached: true, isRefreshing: false
        ), "reading.cache.saved")
    }

    func testTextEnvelopeDisplaysContentWithoutTranslatingIt() {
        let input: JSONValue = .object([
            "preview": .string(#"{"type":"text","text":"Please inspect the report\n保持原文"}"#)
        ])
        XCTAssertEqual(ReadingPresentation.operatorText(input), "Please inspect the report\n保持原文")
    }

    func testPlainMalformedAndFuturePreviewRemainVerbatim() {
        for preview in [
            "plain <script>content</script>",
            #"{"type":"text","text":"invalid\q"#,
            #"{"type":"text","text":"invalid\u4G"#,
            #"{"type":"text","text":"invalid\uDE00"#,
            #"{"type":"text","text":"closed",unexpected"#,
            #"{"unknown":"shape""#,
            #"{"type":"future","text":"Do not reinterpret"}"#
        ] {
            XCTAssertEqual(ReadingPresentation.operatorText(.object(["preview": .string(preview)])), preview)
        }
    }

    func testLongTruncatedTextEnvelopeShowsReadablePrefix() throws {
        let text = "First line\n\nQuoted \"value\" and a path\\name. 保持原文🙂 "
            + String(repeating: "More detail. ", count: 300)
        let encoded = String(decoding: try JSONEncoder().encode(text), as: UTF8.self)
        let preview = String((#"{"type":"text","text":"# + encoded + "}").prefix(2_048))
        let shown = ReadingPresentation.operatorText(.object(["preview": .string(preview)]))
        XCTAssertTrue(shown.hasPrefix("First line\n\nQuoted \"value\" and a path\\name. 保持原文🙂 "))
        XCTAssertTrue(shown.hasSuffix("…"))
        XCTAssertFalse(shown.contains(#"{"type":"text""#))
    }

    func testTruncatedTextEnvelopeDropsOnlyIncompleteEscapeTail() {
        for tail in ["", "\\", "\\u", "\\u4", "\\u4F", "\\u4F6",
                     "\\uD83D", "\\uD83D\\", "\\uD83D\\u", "\\uD83D\\uDE0", "\""] {
            let preview = #"{"type":"text","text":"Readable "# + tail
            XCTAssertEqual(ReadingPresentation.operatorText(.object(["preview": .string(preview)])),
                           "Readable …", "tail: \(tail)")
        }
        XCTAssertEqual(ReadingPresentation.operatorText(.object([
            "preview": .string(#"{"type":"text","text":"Readable \u4F60"#)
        ])), "Readable 你…")
        XCTAssertEqual(ReadingPresentation.operatorText(.object([
            "preview": .string(#"{"type":"text","text":"Readable \uD83D\uDE00"#)
        ])), "Readable 😀…")
    }

    func testTruncatedTextEnvelopePreservesLeadingCombiningScalars() {
        for text in ["\u{0301}Readable", "\u{0301}\u{0327}Readable", "\u{FE0F}Readable", "\u{200D}Readable"] {
            let preview = #"{"type":"text","text":""# + text
            XCTAssertEqual(ReadingPresentation.operatorText(.object(["preview": .string(preview)])),
                           text + "…")
        }
    }

    func testTruncatedSurrogateTailRequiresCompletableLowSurrogate() {
        for low in ["0", "00", "004", "C", "D0", "DB", "DBF", "E", "FF"] {
            let preview = #"{"type":"text","text":"Readable \uD83D\u"# + low
            XCTAssertEqual(ReadingPresentation.operatorText(.object(["preview": .string(preview)])),
                           preview, "invalid low surrogate prefix: \(low)")
        }
        for low in ["", "D", "d", "DC", "dc", "DD", "De", "Df", "DFF"] {
            let preview = #"{"type":"text","text":"Readable \uD83D\u"# + low
            XCTAssertEqual(ReadingPresentation.operatorText(.object(["preview": .string(preview)])),
                           "Readable …", "completable low surrogate prefix: \(low)")
        }
    }
}
