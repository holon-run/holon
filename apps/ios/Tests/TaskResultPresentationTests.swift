import Foundation
import HolonClient
import XCTest
@testable import Holon

@MainActor
final class TaskResultPresentationTests: XCTestCase {
    private func inputs() throws -> [JSONValue] {
        var root = URL(fileURLWithPath: #filePath)
        for _ in 0..<4 { root.deleteLastPathComponent() }
        let raw = try JSONDecoder().decode(JSONValue.self, from: Data(contentsOf: root.appendingPathComponent("tests/fixtures/client-wire/task-result-inputs.json")))
        return raw.workArray ?? []
    }

    func testOptionalMetadataAndAllOutcomesSurviveColdProjection() throws {
        let results = TaskResultInput.items(try inputs())
        XCTAssertEqual(results.count, 8)
        XCTAssertEqual(Array(results.prefix(4)).map(\.status), ["completed", "failed", "cancelled", "interrupted"])
        XCTAssertEqual(results[1].reason, "Missing package manifest")
        XCTAssertEqual(results[4].responseMessageID, "peer-reply")
        XCTAssertNil(results[4].reason)
        XCTAssertEqual(results[5].runtimeOnly, false)
        XCTAssertNil(results.last?.runtimeOnly)
        XCTAssertNil(results.last?.summary)
        XCTAssertEqual(TaskResultInput.header(try inputs())?.status, "failed")
        XCTAssertNil(TaskResultInput(.object(["message_id": .string("older"), "preview": .string("old input")])))
    }

    func testRuntimeBriefUsesProvenanceAndLinksNeverTextEquality() throws {
        let values = try inputs()
        let legacy: JSONValue = .object(["related_message_id": .string("legacy"), "text": .string("Historical preview")])
        XCTAssertTrue(TaskResultInput.isRuntimeBrief(legacy, inputs: values))
        let unrelated: JSONValue = .object(["related_message_id": .string("other"), "text": .string("Historical preview")])
        XCTAssertFalse(TaskResultInput.isRuntimeBrief(unrelated, inputs: values))
        let model: JSONValue = .object(["related_message_id": .string("model"), "related_task_id": .string("task-model"), "text": .string("Same text as the model response")])
        XCTAssertFalse(TaskResultInput.isRuntimeBrief(model, inputs: values))
    }

    func testProcessKeepsReceiptIdentityAndCanonicalLateInputOrder() throws {
        var late = (try inputs())[1]
        if case .object(var fields) = late {
            fields["interjected"] = .bool(true)
            fields["activity_key"] = .object(["event_seq": .integer(12), "activity_id": .string("operator:failure")])
            late = .object(fields)
        }
        func activity(_ seq: Int64) throws -> ReadingActivity {
            let id = "tool:" + String(seq)
            return try ReadingActivity(.object(["id": .string(id), "kind": .string("tool"), "revision": .integer(1), "summary": .string("Tool"), "key": .object(["event_seq": .integer(seq), "activity_id": .string(id)])]))
        }
        let rows = ReadingProcessEntry.items(inputs: [(try inputs())[0], late], activities: try [activity(10), activity(14)])
        XCTAssertEqual(rows.map(\.id), ["input:command", "activity:tool:10", "input:failure", "activity:tool:14"])
    }
    func testCommandFailureHeaderPrefersCauseWithoutHostOutputPath() throws {
        var input = (try inputs())[1]
        if case .object(var fields) = input, case .object(var result) = fields["task_result"] {
            result["preview"] = .string("command task failed: Build\noutput_path: /host/output\nexit_status: 7\noutput_summary:\nstderr:\nMissing manifest")
            fields["task_result"] = .object(result); input = .object(fields)
        }
        XCTAssertEqual(TaskResultInput(input)?.reason, "Missing manifest")
        XCTAssertEqual(TaskResultInput(input)?.displayPreview, "stderr:\nMissing manifest")
    }

}
