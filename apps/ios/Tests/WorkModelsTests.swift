import Foundation
import HolonClient
import XCTest
@testable import Holon

final class WorkModelsTests: XCTestCase {
    func testWorkShowsCurrentStepAndLocalizedStateWithoutChangingUnknownContract() throws {
        let record = try WorkRecord(raw: .object(["id": .string("work"), "state": .string("open"), "todo_list": .array([
            .object(["text": .string("Done"), "state": .string("completed")]),
            .object(["text": .string("Later"), "state": .string("pending")]),
            .object(["text": .string("Now"), "state": .string("in_progress")])
        ])]))
        XCTAssertEqual(record.nextStep, "Now"); XCTAssertFalse(record.completed)
        XCTAssertEqual(record.stateKey, "work.state.pending")
        let unknown = try WorkRecord(raw: .object(["id": .string("work"), "state": .string("future")]))
        XCTAssertEqual(unknown.state, "future"); XCTAssertEqual(unknown.stateKey, "work.state.unknown")
    }
    func testBriefUsesCanonicalFullTextAndNeverFallsBackToRawJSON() {
        let fullText = String(repeating: "正文\n", count: 20_000)
        XCTAssertEqual(BriefPresentation.text(.object([
            "text": .string(fullText), "body": .string("legacy")
        ])), fullText)
        XCTAssertEqual(BriefPresentation.text(.object(["body": .string("legacy")])), "legacy")
        XCTAssertEqual(BriefPresentation.text(.object(["internal": .string("not a brief")])), "")
    }
    func testOutputLocallyCapsAndRetainsServerTruncation() throws {
        let output = try WorkOutput(raw: .object(["task": .object([
            "output_preview": .string(String(repeating: "x", count: 40_000)),
            "output_truncated": .bool(false), "status": .string("new-state")
        ])]))
        XCTAssertEqual(output.text?.count, 32_768)
        XCTAssertTrue(output.truncated)
        XCTAssertEqual(output.status, "new-state")
    }
    func testMissingOutputIsNotPresentedAsAnEmptySuccess() throws {
        let output = try WorkOutput(raw: .object([:]))
        XCTAssertNil(output.text)
        XCTAssertEqual(output.status, "unknown")
    }
    func testMalformedRecordsFailRatherThanInventIdentifiers() {
        XCTAssertThrowsError(try WorkRecord(raw: .object([:])))
        XCTAssertThrowsError(try WorkRecord(raw: .string("invalid")))
    }
    func testUnknownStateAndPlanMetadataAreRetained() throws {
        let plan: JSONValue = .object(["workspace_id": .string("workspace"),
                                      "relative_path": .string("plan.md"),
                                      "execution_root_id": .string("root")])
        let record = try WorkRecord(raw: .object(["id": .string("work"),
                                                 "state": .string("future-state"),
                                                 "plan_artifact": plan]))
        XCTAssertEqual(record.state, "future-state")
        XCTAssertEqual(record.plan, plan)
    }
}
