import Foundation
import HolonClient
import XCTest
@testable import Holon

final class ActivityKindToleranceTests: XCTestCase {
    private func modelActivity(revision: Int64, id: String = "model_request:turn:1:1") -> JSONValue {
        .object([
            "id": .string(id),
            "kind": .string("model_request"),
            "key": .object(["event_seq": .integer(5), "activity_id": .string(id)]),
            "revision": .integer(revision),
            "summary": .string("fixture@default/primary-model · succeeded · 12ms"),
            "provider": .string("fixture"),
            "model_ref": .string("fixture@default/primary-model"),
            "dispatch_reason": .string("initial"),
            "status": .string("succeeded"),
            "duration_ms": .integer(12)
        ])
    }

    func testUnknownModelRequestKindDecodesGenerically() throws {
        let activity = try ReadingActivity(modelActivity(revision: 2))
        XCTAssertEqual(activity.kind, "model_request")
        XCTAssertNil(activity.detailID)
        XCTAssertEqual(activity.sequence, 5)
        XCTAssertEqual(activity.revision, 2)
        XCTAssertTrue(activity.summary.contains("succeeded"))

        let page = JSONValue.object(["activities": .array([modelActivity(revision: 2)])])
        XCTAssertEqual(ActivityPresentation.items(page).map(\.kind), ["model_request"])
    }

    func testModelRequestKindSurvivesMergeInvariant() throws {
        let current = JSONValue.object([
            "detail_revision": .integer(1),
            "next_before_cursor": .string("cursor-old"),
            "activities": .array([modelActivity(revision: 2)])
        ])
        let older = JSONValue.object([
            "detail_revision": .integer(1),
            "next_before_cursor": .string("cursor-older"),
            "activities": .array([modelActivity(revision: 1)])
        ])
        let merged = try ActivityPresentation.merging(older, into: current, requestedCursor: "cursor-old")
        let items = ActivityPresentation.items(merged)
        XCTAssertEqual(items.count, 1)
        XCTAssertEqual(items[0].kind, "model_request")
        XCTAssertEqual(items[0].revision, 2)
    }
}
