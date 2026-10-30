import Foundation
import XCTest

/// Scales every UI wait budget by one factor.
///
/// The runner's wait budgets are fixed constants, but a shared macOS CI
/// runner can be several times slower than a local machine. A single
/// `HOLON_UI_WAIT_SCALE` lets the job stretch all budgets together instead
/// of failing a healthy case on a loaded runner. The fixture forwards the
/// factor to the test runner through `TEST_RUNNER_HOLON_UI_WAIT_SCALE`.
///
/// The scale must never shorten a budget: a value below 1 would turn a slow
/// but correct run into a false failure.
enum UIWaitScale {
    /// The factor every wait budget is multiplied by.
    static let factor: Double = {
        let raw = ProcessInfo.processInfo.environment["HOLON_UI_WAIT_SCALE"] ?? "1"
        guard let value = Double(raw.trimmingCharacters(in: .whitespaces)),
              value >= 1, value <= 5 else {
            // An unusable factor must not silently change the contract; the
            // fixture rejects it, and a direct local run keeps the defaults.
            return 1
        }
        return value
    }()

    /// The scaled budget for one wait.
    static func scaled(_ timeout: TimeInterval) -> TimeInterval {
        timeout * factor
    }

    /// Asserts that the runner received the factor the fixture promised.
    ///
    /// The contract only holds when the factor reaches the test runner, so a
    /// missing value is a wiring failure rather than a default to accept.
    static func assertForwarded(file: StaticString = #filePath, line: UInt = #line) {
        let raw = ProcessInfo.processInfo.environment["HOLON_UI_WAIT_SCALE"]
        XCTAssertNotNil(raw, "The UI runner must receive HOLON_UI_WAIT_SCALE", file: file, line: line)
        XCTAssertEqual(raw?.trimmingCharacters(in: .whitespaces), String(format: "%g", factor),
                       "The UI runner's wait scale must match the requested factor",
                       file: file, line: line)
    }
}
