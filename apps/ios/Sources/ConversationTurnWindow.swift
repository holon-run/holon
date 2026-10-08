import Foundation

/// Window navigation is reading, even when the newer window includes the tail.
struct ConversationFollowState {
    private(set) var followsLatest = true

    mutating func reviewHistory() { followsLatest = false }
    mutating func showLatest() { followsLatest = true }
    mutating func userEndedScroll(nearBottom: Bool, newestWindow: Bool) {
        followsLatest = nearBottom && newestWindow
    }
}

/// Bound native layout/accessibility work; authoritative history stays in the reader.
enum ConversationTurnWindow {
    static let capacity = 20
    static let stride = 15
    static func range(ids: [String], endingAt: String?) -> Range<Int> {
        let end = endingAt.flatMap { ids.firstIndex(of: $0).map { $0 + 1 } } ?? ids.count
        return max(0, end - capacity)..<end
    }
    static func olderEnd(ids: [String], current: Range<Int>) -> String? {
        guard current.lowerBound > 0 else { return nil }
        return ids[max(capacity, current.upperBound - stride) - 1]
    }
    static func newerEnd(ids: [String], current: Range<Int>) -> String? {
        let end = min(ids.count, current.upperBound + stride)
        return end < ids.count ? ids[end - 1] : nil
    }
    static func restoringEnd(ids: [String], turnID: String) -> String? {
        guard let index = ids.firstIndex(of: turnID) else { return nil }
        let end = min(ids.count, index + capacity)
        return end < ids.count ? ids[end - 1] : nil
    }
}
