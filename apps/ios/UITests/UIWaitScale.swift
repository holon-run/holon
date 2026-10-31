import Foundation

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
}

/// The launch argument that asks the app to clear its own state.
///
/// The app owns the reset; the test only names the request. The app target
/// cannot import this file, so the contract test guards that both sides use
/// the same argument.
enum TestingStateResetArgument {
    static let value = "-HolonResetAppStateForUITests"
}
