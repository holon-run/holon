import Foundation
import HolonClient
import XCTest
@testable import Holon

final class WorkModelsTests: XCTestCase {
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
