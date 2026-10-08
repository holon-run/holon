import Foundation
import HolonClient
import XCTest
@testable import Holon

@MainActor
final class ActivityPresentationTests: XCTestCase {
    private func item(_ index: Int, revision: Int64 = 1, kind: String = "tool") -> JSONValue {
        let id = kind + ":" + String(index)
        return .object(["id": .string(id), "kind": .string(kind), "revision": .integer(revision),
                        "key": .object(["event_seq": .integer(Int64(index)), "activity_id": .string(id)]),
                        "summary": .string("summary \(index)")])
    }
    private func page(_ range: Range<Int>, cursor: String?, more: Bool = true, revision: Int64 = 1) -> JSONValue {
        .object(["runtime_id": .string("runtime"), "visibility_scope_id": .string("private"),
                 "event_log_epoch": .string("epoch"), "schema_version": .integer(1), "query_version": .integer(1),
                 "turn": .object(["turn_id": .string("turn")]), "detail_revision": .integer(revision),
                 "activities": .array(range.map { item($0) }), "has_more": .bool(more),
                 "next_before_cursor": cursor.map(JSONValue.string) ?? .null])
    }

    func testActivityUsesStableIdentityRevisionAndStructuredSummary() throws {
        let value = try ReadingActivity(item(2, revision: 3))
        XCTAssertEqual(value.id, "tool:2"); XCTAssertEqual(value.detailID, "2"); XCTAssertEqual(value.revision, 3)
        var malformed = item(2)
        if case .object(var fields) = malformed { fields["key"] = .null; malformed = .object(fields) }
        XCTAssertThrowsError(try ReadingActivity(malformed))
        XCTAssertNil(try ReadingActivity(item(2, kind: "future")).detailID)
    }

    func testActivityPageRejectsWrongAuthorityVersionTurnAndRepeatedIDs() throws {
        let snapshot = try HolonConversationSnapshot(raw: .object([
            "runtime_id": .string("runtime"), "visibility_scope_id": .string("private"),
            "agent_id": .string("A"), "event_log_epoch": .string("epoch"),
            "snapshot_cursor": .string("live"), "snapshot_through_seq": .integer(7), "event_head_seq": .integer(7),
            "schema_version": .integer(1), "query_version": .integer(1), "has_more": .bool(false),
            "turns": .array([]), "active_turns": .array([]), "pending_inputs": .array([])]))
        let valid = page(0..<60, cursor: nil, more: false)
        XCTAssertNoThrow(try ActivityPresentation.validate(valid, snapshot: snapshot, turnID: "turn"))
        for (key, value) in [("runtime_id", JSONValue.string("other")), ("event_log_epoch", .string("other")),
                             ("visibility_scope_id", .string("other")), ("schema_version", .integer(99)),
                             ("turn", .object(["turn_id": .string("other")])),
                             ("activities", .array([item(1), item(1)])), ("has_more", .bool(true))] {
            guard case .object(var fields) = valid else { return XCTFail() }
            fields[key] = value
            XCTAssertThrowsError(try ActivityPresentation.validate(.object(fields), snapshot: snapshot, turnID: "turn"), key)
        }
    }

    func testMoreThanSixtyActivitiesStayReachableAcrossBoundedWindows() throws {
        var current = page(240..<300, cursor: "240")
        for start in [180, 120, 60, 0] {
            let cursor = String(start + 60)
            current = try ActivityPresentation.merging(page(start..<(start + 60), cursor: start == 0 ? nil : String(start), more: start != 0),
                                                       into: current, requestedCursor: cursor)
            XCTAssertLessThanOrEqual(ActivityPresentation.items(current).count, 180)
            XCTAssertEqual(ActivityPresentation.items(current).first?.sequence, Int64(start))
        }
        XCTAssertEqual(current["has_more"], .bool(false))
        XCTAssertEqual(current["client_window_trimmed"], .bool(true))
    }

    func testActivityVersionsMatchKnownConversationVersions() throws {
        for version: Int64 in [1, 2] {
            let snapshot = try HolonConversationSnapshot(raw: .object([
                "runtime_id": .string("runtime"), "visibility_scope_id": .string("private"),
                "agent_id": .string("A"), "event_log_epoch": .string("epoch"),
                "snapshot_cursor": .string("live"), "schema_version": .integer(version),
                "query_version": .integer(version), "has_more": .bool(false),
                "turns": .array([]), "active_turns": .array([]), "pending_inputs": .array([])]))
            guard case .object(var fields) = page(0..<60, cursor: nil, more: false) else { return XCTFail() }
            fields["schema_version"] = .integer(version); fields["query_version"] = .integer(version)
            XCTAssertNoThrow(try ActivityPresentation.validate(.object(fields), snapshot: snapshot, turnID: "turn"))
            for key in ["schema_version", "query_version"] {
                for mismatch: Int64 in [0, version == 1 ? 2 : 1, 3] {
                    var invalid = fields; invalid[key] = .integer(mismatch)
                    XCTAssertThrowsError(try ActivityPresentation.validate(.object(invalid), snapshot: snapshot, turnID: "turn"))
                }
            }
        }
    }

    func testCursorAndDetailRevisionCannotBeMixedOrLooped() {
        let initial = page(60..<120, cursor: "60")
        XCTAssertThrowsError(try ActivityPresentation.merging(page(0..<60, cursor: "60"), into: initial, requestedCursor: "60"))
        XCTAssertThrowsError(try ActivityPresentation.merging(page(0..<60, cursor: nil, more: false, revision: 2), into: initial, requestedCursor: "60"))
        XCTAssertThrowsError(try ActivityPresentation.merging(page(0..<60, cursor: nil, more: false), into: initial, requestedCursor: "wrong"))
    }

    func testOverlappingActivitiesKeepHighestRevisionAndStableOrder() throws {
        var older = page(0..<60, cursor: nil, more: false)
        if case .object(var fields) = older {
            fields["activities"] = .array((0..<59).map { item($0) } + [item(70, revision: 4)])
            older = .object(fields)
        }
        let merged = try ActivityPresentation.merging(older, into: page(60..<120, cursor: "60"), requestedCursor: "60")
        let records = ActivityPresentation.items(merged)
        XCTAssertEqual(records.filter { $0.id == "tool:70" }.count, 1)
        XCTAssertEqual(records.first { $0.id == "tool:70" }?.revision, 4)
        XCTAssertEqual(records.map(\.sequence), records.map(\.sequence).sorted())
    }

    func testAssistantBlocksAreReadableButNonTextBlocksAreNotReinterpreted() {
        let blocks: JSONValue = .object(["blocks": .array([
            .object(["type": .string("text"), "text": .string("**Hello**")]),
            .object(["type": .string("tool_use"), "text": .string("Not assistant prose")]),
            .object(["type": .string("text"), "text": .string("中文")])])])
        XCTAssertEqual(ActivityPresentation.assistantText(blocks), "**Hello**\n\n中文")
        XCTAssertEqual(ActivityPresentation.assistantText(.string("plain\ntext")), "plain\ntext")
        XCTAssertEqual(ActivityPresentation.assistantText(.object(["future": .string("unknown")])), "")
    }

    func testToolInputAndNestedOutputDoNotExposeEnvelopeAsPrimaryContent() {
        let detail: JSONValue = .object([
            "input": .object(["cmd": .string("rg --files")]),
            "output": .object(["envelope": .object(["result": .object(["stdout": .string("file.md"), "stderr": .string("")])])])])
        let blocks = ActivityPresentation.toolBlocks(detail)
        XCTAssertEqual(blocks.map(\.key), ["reading.command", "reading.stdout", "reading.stderr"])
        XCTAssertEqual(blocks.first?.text, "rg --files")
        XCTAssertFalse(blocks.contains { $0.text.contains("envelope") })
    }

    func testCompletedTurnDoesNotGetActiveOrRepeatedSuccessLabels() {
        XCTAssertNil(ReadingPresentation.turnStatus(.object(["execution": .object(["kind": .string("terminal"), "outcome": .string("completed")])])))
        XCTAssertNil(ReadingPresentation.turnStatus(.object(["result": .object(["kind": .string("pending")])])))
        XCTAssertEqual(ReadingPresentation.turnStatus(.object(["execution": .object(["kind": .string("active")])])), "reading.activeTurn")
        XCTAssertEqual(ReadingPresentation.turnStatus(.object(["attention": .object(["kind": .string("failed")])])), "reading.failedTurn")
    }
}
