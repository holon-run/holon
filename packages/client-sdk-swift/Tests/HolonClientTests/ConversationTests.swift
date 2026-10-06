import Foundation
@testable import HolonClient
import XCTest

final class ConversationTests: XCTestCase {
    private func initial() throws -> HolonConversationSnapshot {
        let root = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent()
            .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
        let data = try Data(contentsOf: root.appendingPathComponent("tests/fixtures/client-wire/mobile-conversation-v2.json"))
        return try HolonConversationSnapshot(raw: JSONDecoder().decode(JSONValue.self, from: data))
    }
    private func frame(_ type: String, _ fields: [String: JSONValue] = [:]) throws -> HolonSSEEvent {
        var raw = fields; raw["type"] = .string(type)
        return HolonSSEEvent(event: type, id: nil, data: String(decoding: try JSONEncoder().encode(JSONValue.object(raw)), as: UTF8.self))
    }
    private func begin(_ seq: Int64 = 100) throws -> HolonSSEEvent {
        try frame("batch_begin", ["batch_id": .string("b"), "from_seq": .integer(0), "through_seq": .integer(seq),
                                  "schema_version": .integer(2), "query_version": .integer(2),
                                  "runtime_id": .string("runtime-mobile"), "event_log_epoch": .string("epoch-mobile"),
                                  "visibility_scope_id": .string("scope-mobile")])
    }
    private func checkpoint(_ seq: Int64 = 100) throws -> HolonSSEEvent {
        try frame("checkpoint", ["batch_id": .string("b"), "through_seq": .integer(seq), "checkpoint": .string("committed"),
                                 "event_log_epoch": .string("epoch-mobile"), "visibility_scope_id": .string("scope-mobile")])
    }
    func testRequiredConversationVersionsFailClosed() throws {
        let snapshot = try initial()
        guard case .object(let original) = snapshot.raw else { return XCTFail("Expected object") }
        let event = try begin()
        let raw = try JSONDecoder().decode(JSONValue.self, from: Data(event.data.utf8))
        guard case .object(let batch) = raw else { return XCTFail("Expected object") }
        let invalid: [JSONValue?] = [nil, .null, .string("2"), .bool(true), .number(1.5),
                                    .integer(-1), .integer(0), .integer(3), .integer(Int64.max)]
        // Include both absent, each absent (partial pair), wrong type, and unsupported values.
        var pairs: [[String: JSONValue]] = [[:]]
        for key in ["schema_version", "query_version"] {
            for value in invalid {
                var pair: [String: JSONValue] = ["schema_version": .integer(2), "query_version": .integer(2)]
                pair[key] = value
                pairs.append(pair)
            }
        }
        for pair in pairs {
            var fields = original
            var beginFields = batch
            for key in ["schema_version", "query_version"] {
                fields[key] = pair[key]
                beginFields[key] = pair[key]
            }
            XCTAssertThrowsError(try HolonConversationSnapshot(raw: .object(fields))) {
                XCTAssertEqual($0 as? HolonConversationError, .malformedProtocol)
            }
            var reducer = HolonConversationReducer(snapshot: snapshot)
            XCTAssertThrowsError(try reducer.acceptCommit(frame("batch_begin", beginFields))) {
                XCTAssertEqual($0 as? HolonConversationError, .malformedProtocol)
            }
            XCTAssertThrowsError(try reducer.acceptCommit(checkpoint())) {
                XCTAssertEqual($0 as? HolonConversationError, .malformedProtocol)
            }
            XCTAssertEqual(reducer.snapshot, snapshot)
            XCTAssertNil(try reducer.acceptCommit(begin()))
            XCTAssertNotNil(try reducer.acceptCommit(checkpoint()))
        }
        for schema: Int64 in 1...2 {
            for query: Int64 in 1...2 {
                var fields = original
                var beginFields = batch
                fields["schema_version"] = .integer(schema)
                fields["query_version"] = .integer(query)
                beginFields["schema_version"] = .integer(schema)
                beginFields["query_version"] = .integer(query)
                let compatible = try HolonConversationSnapshot(raw: .object(fields))
                var reducer = HolonConversationReducer(snapshot: compatible)
                XCTAssertNil(try reducer.acceptCommit(frame("batch_begin", beginFields)))
                // Checkpoints have no versions in the Rust stream contract.
                XCTAssertNotNil(try reducer.acceptCommit(checkpoint()))
            }
        }
    }

    func testDetailInvalidationsCommitAtomicallyWithoutSummaryChanges() throws {
        let snapshot = try initial()
        var reducer = HolonConversationReducer(snapshot: snapshot)
        let turn = snapshot.raw["turns"]!.conversationArray.last!
        let id = turn["turn_id"]!.conversationString!
        XCTAssertNil(try reducer.acceptCommit(begin()))
        XCTAssertNil(try reducer.acceptCommit(frame("turn_summary_upsert", ["turn": turn])))
        for revision: Int64 in [5, 7, 6] {
            XCTAssertNil(try reducer.acceptCommit(frame("detail_invalidated", [
                "turn_id": .string(id), "detail_revision": .integer(revision)])))
        }
        XCTAssertEqual(reducer.snapshot, snapshot)
        let commit = try XCTUnwrap(reducer.acceptCommit(checkpoint()))
        XCTAssertEqual(commit.snapshot.raw["turns"], snapshot.raw["turns"])
        XCTAssertEqual(commit.detailInvalidations, [id: 7])
        _ = try reducer.acceptCommit(begin(99))
        _ = try reducer.acceptCommit(frame("detail_invalidated", ["turn_id": .string(id), "detail_revision": .integer(99)]))
        XCTAssertNil(try reducer.acceptCommit(checkpoint(99)))
        _ = try reducer.acceptCommit(begin(101))
        _ = try reducer.acceptCommit(frame("detail_invalidated", ["turn_id": .string(id), "detail_revision": .integer(6)]))
        XCTAssertEqual(try reducer.acceptCommit(checkpoint(101))?.detailInvalidations, [:])
    }

    func testOffWindowDetailRevisionsDoNotAccumulate() throws {
        var reducer = HolonConversationReducer(snapshot: try initial())
        for seq: Int64 in [100, 101] {
            _ = try reducer.acceptCommit(begin(seq))
            for index in 0..<4097 {
                _ = try reducer.acceptCommit(frame("detail_invalidated", [
                    "turn_id": .string("off-window-\(index)"), "detail_revision": .integer(seq)]))
            }
            XCTAssertEqual(try reducer.acceptCommit(checkpoint(seq))?.detailInvalidations.count, 4097)
        }
        _ = try reducer.acceptCommit(begin(102))
        XCTAssertEqual(try reducer.acceptCommit(checkpoint(102))?.detailInvalidations, [:])
    }

    func testFailedBatchDoesNotAdvanceDetailRevision() throws {
        var reducer = HolonConversationReducer(snapshot: try initial())
        let invalidation = try frame("detail_invalidated", ["turn_id": .string("removed-turn"), "detail_revision": .integer(9)])
        _ = try reducer.acceptCommit(begin())
        _ = try reducer.acceptCommit(invalidation)
        _ = try reducer.acceptCommit(frame("detail_invalidated", ["turn_id": .string("bad"), "detail_revision": .integer(-1)]))
        XCTAssertThrowsError(try reducer.acceptCommit(checkpoint()))
        _ = try reducer.acceptCommit(begin())
        _ = try reducer.acceptCommit(invalidation)
        XCTAssertEqual(try reducer.acceptCommit(checkpoint())?.detailInvalidations, ["removed-turn": 9])
        _ = try reducer.acceptCommit(begin(101))
        _ = try reducer.acceptCommit(invalidation)
        XCTAssertThrowsError(try reducer.acceptCommit(checkpoint(102)))
        XCTAssertThrowsError(try reducer.acceptCommit(checkpoint(101)))
    }

    func testAuthoritativeOrderingHistoryAndWindow() throws {
        let snapshot = try initial()
        XCTAssertEqual(snapshot.raw["turns"]!.conversationArray.map { $0["turn_id"] }, [.string("outside-window"), .string("older"), .string("newer")])
        let merged = try mergeConversationHistory(snapshot, page: snapshot)
        XCTAssertEqual(merged.snapshotCursor, snapshot.snapshotCursor)
        if case .object(var pageFields) = snapshot.raw {
            pageFields["snapshot_cursor"] = .string("history-not-live")
            pageFields["has_more"] = .bool(false)
            pageFields["next_before_cursor"] = .null
            pageFields["pending_inputs"] = .array([])
            let history = try mergeConversationHistory(snapshot, page: HolonConversationSnapshot(raw: .object(pageFields)))
            XCTAssertEqual(history.snapshotCursor, snapshot.snapshotCursor)
            XCTAssertEqual(history.raw["pending_inputs"], snapshot.raw["pending_inputs"])
            XCTAssertFalse(history.hasMore)
            XCTAssertNil(history.nextBeforeCursor)
        }
        XCTAssertThrowsError(try mergeConversationHistory(snapshot, page: snapshot, maximumTurns: 1))
        var raw = snapshot.raw
        if case .object(var fields) = raw { fields["event_log_epoch"] = .string("other"); raw = .object(fields) }
        XCTAssertThrowsError(try mergeConversationHistory(snapshot, page: HolonConversationSnapshot(raw: raw)))
    }
    func testAtomicCheckpointOldBatchAndTerminalProtection() throws {
        let snapshot = try initial()
        var reducer = HolonConversationReducer(snapshot: snapshot)
        XCTAssertNil(try reducer.accept(begin()))
        var turn = snapshot.raw["turns"]!.conversationArray.last!
        if case .object(var fields) = turn { fields["revision"] = .integer(8); turn = .object(fields) }
        XCTAssertNil(try reducer.accept(frame("turn_summary_upsert", ["turn": turn])))
        XCTAssertEqual(reducer.snapshot, snapshot)
        XCTAssertEqual(try reducer.accept(checkpoint())?.snapshotCursor, "committed")
        XCTAssertEqual(reducer.snapshot.raw["turns"], snapshot.raw["turns"])
        XCTAssertNil(try reducer.accept(begin(99)))
        XCTAssertNil(try reducer.accept(checkpoint(99)))
        XCTAssertEqual(reducer.snapshot.snapshotCursor, "committed")
        if case .object(var fields) = turn { fields["revision"] = .integer(99); fields["execution"] = .object(["kind": .string("active")]); turn = .object(fields) }
        _ = try reducer.accept(begin(101))
        _ = try reducer.accept(frame("turn_summary_upsert", ["turn": turn]))
        _ = try reducer.accept(checkpoint(101))
        XCTAssertEqual(reducer.snapshot.raw["turns"], snapshot.raw["turns"])
    }
    func testUnknownMismatchOverflowAndRollback() throws {
        let snapshot = try initial()
        var reducer = HolonConversationReducer(snapshot: snapshot)
        XCTAssertThrowsError(try reducer.accept(frame("future_control")))
        _ = try reducer.accept(begin())
        XCTAssertThrowsError(try reducer.accept(checkpoint(101)))
        XCTAssertEqual(reducer.snapshot, snapshot)
        _ = try reducer.accept(begin())
        XCTAssertThrowsError(try reducer.accept(frame("detail_invalidated", ["future": .string(String(repeating: "x", count: 4 * 1024 * 1024))])))
        XCTAssertEqual(reducer.snapshot, snapshot)
        var bounded = HolonConversationReducer(snapshot: snapshot, maximumTurns: 1)
        _ = try bounded.accept(begin())
        XCTAssertThrowsError(try bounded.accept(checkpoint()))
        XCTAssertEqual(bounded.snapshot, snapshot)
    }
    func testPendingRemovalRevisionAndUnknownFields() throws {
        var reducer = HolonConversationReducer(snapshot: try initial())
        _ = try reducer.accept(begin())
        _ = try reducer.accept(frame("operator_remove", ["message_id": .string("pending-message"), "revision": .integer(5)]))
        _ = try reducer.accept(frame("operator_upsert", ["input": .object(["message_id": .string("pending-message"), "revision": .integer(4), "future": .bool(true)])]))
        _ = try reducer.accept(checkpoint())
        XCTAssertEqual(reducer.snapshot.raw["pending_inputs"], .array([]))
        _ = try reducer.accept(begin(101))
        _ = try reducer.accept(frame("operator_upsert", ["input": .object(["message_id": .string("new"), "revision": .integer(6), "future": .bool(true)])]))
        _ = try reducer.accept(checkpoint(101))
        XCTAssertEqual(reducer.snapshot.raw["pending_inputs"]!.conversationArray.first?["future"], .bool(true))
    }

    func testConflictingRevisionAndEpochNeverPublish() throws {
        let snapshot = try initial()
        var reducer = HolonConversationReducer(snapshot: snapshot)
        _ = try reducer.accept(begin())
        var turn = snapshot.raw["turns"]!.conversationArray.last!
        if case .object(var fields) = turn { fields["future"] = .bool(true); turn = .object(fields) }
        _ = try reducer.accept(frame("turn_summary_upsert", ["turn": turn]))
        XCTAssertThrowsError(try reducer.accept(checkpoint()))
        XCTAssertEqual(reducer.snapshot, snapshot)
        let changedEpoch = try frame("batch_begin", [
            "batch_id": .string("b"), "from_seq": .integer(0), "through_seq": .integer(100),
            "schema_version": .integer(2), "query_version": .integer(2),
            "runtime_id": .string("runtime-mobile"), "event_log_epoch": .string("new-epoch"),
            "visibility_scope_id": .string("scope-mobile")])
        XCTAssertThrowsError(try reducer.accept(changedEpoch)) {
            XCTAssertEqual($0 as? HolonConversationError, .bootstrapRequired)
        }
        XCTAssertEqual(reducer.snapshot, snapshot)
        XCTAssertThrowsError(try reducer.accept(checkpoint()))
    }
}
