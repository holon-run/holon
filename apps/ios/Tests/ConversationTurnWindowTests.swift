import XCTest
@testable import Holon

final class ConversationTurnWindowTests: XCTestCase {
    func testNewerWindowDoesNotResumeTailFollowingDuringBriefHydration() {
        var follow = ConversationFollowState()
        XCTAssertTrue(follow.followsLatest)
        follow.reviewHistory()
        // Loading a newer window can include the tail while the old viewport
        // still reports near-bottom. Neither event is user scroll intent.
        XCTAssertFalse(follow.followsLatest)
        follow.userEndedScroll(nearBottom: true, newestWindow: false)
        XCTAssertFalse(follow.followsLatest)
        follow.userEndedScroll(nearBottom: false, newestWindow: true)
        XCTAssertFalse(follow.followsLatest)
        follow.userEndedScroll(nearBottom: true, newestWindow: true)
        XCTAssertTrue(follow.followsLatest)
        follow.reviewHistory()
        follow.showLatest()
        XCTAssertTrue(follow.followsLatest)
    }
    func testPagingKeepsOverlapAndMakesEveryTurnReachableWithoutGrowingLayout() {
        let ids = (0..<91).map { "turn-\($0)" }
        var end: String?
        var seen: Set<String> = []
        var range = ConversationTurnWindow.range(ids: ids, endingAt: end)
        XCTAssertEqual(range, 71..<91)
        while true {
            XCTAssertLessThanOrEqual(range.count, 20)
            seen.formUnion(ids[range])
            guard range.lowerBound > 0 else { break }
            end = ConversationTurnWindow.olderEnd(ids: ids, current: range)
            range = ConversationTurnWindow.range(ids: ids, endingAt: end)
        }
        XCTAssertEqual(seen, Set(ids))
        while range.upperBound < ids.count {
            end = ConversationTurnWindow.newerEnd(ids: ids, current: range)
            range = ConversationTurnWindow.range(ids: ids, endingAt: end)
            XCTAssertLessThanOrEqual(range.count, 20)
        }
        XCTAssertNil(end)
    }
    func testHistoricalWindowSurvivesNewTurnAndRestoreIncludesBookmark() {
        let ids = (0..<60).map { "turn-\($0)" }
        let end = ConversationTurnWindow.restoringEnd(ids: ids, turnID: "turn-5")
        let before = ConversationTurnWindow.range(ids: ids, endingAt: end)
        XCTAssertTrue(ids[before].contains("turn-5"))
        XCTAssertEqual(ConversationTurnWindow.range(ids: ids + ["new"], endingAt: end), before)
        XCTAssertEqual(ConversationTurnWindow.range(ids: ids, endingAt: "removed"), 40..<60)
        XCTAssertEqual(ConversationTurnWindow.range(ids: [], endingAt: nil), 0..<0)
    }
}
