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

    func testPlainTruncatedAndFuturePreviewRemainVerbatim() {
        for preview in [
            "plain <script>content</script>",
            #"{"type":"text","text":"truncated"#,
            #"{"type":"future","text":"Do not reinterpret"}"#
        ] {
            XCTAssertEqual(ReadingPresentation.operatorText(.object(["preview": .string(preview)])), preview)
        }
    }
}
