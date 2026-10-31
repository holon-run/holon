import Foundation

/// Clears the app's own state so a UI case starts from a known baseline.
///
/// The UI sweep runs many cases against one simulator, and several of them
/// install a profile, approve sharing consent, or cache a session. A case
/// that assumes a clean install then inherits that state. Removing the state
/// through the app itself keeps the contract in one place: the app decides
/// what "clean" means, and the test only asks for it.
///
/// The reset is opt-in through a launch argument, so a normal launch is
/// untouched.
enum TestingStateReset {
    /// The key the app records a reset failure under, in the App Group.
    ///
    /// The app runs in its own process, so an assertion there would not fail
    /// the test that asked for the reset. The failure is published where the
    /// test can see it instead.
    static let failureKey = "testing.state.reset.failure"

    /// The launch argument that requests a reset before the UI appears.
    ///
    /// The UI test target cannot import the app target, so it names the same
    /// argument in its own helper; the contract test keeps the two in step.
    static let launchArgument = "-HolonResetAppStateForUITests"

    /// Whether this launch requested a reset.
    static var isRequested: Bool {
        CommandLine.arguments.contains(launchArgument)
    }

    /// Removes every stored profile, session, credential, and consent entry.
    ///
    /// Failures are reported rather than swallowed: a case that silently ran
    /// against dirty state would be a false pass.
    @MainActor
    static func resetIfRequested() throws {
        guard isRequested else { return }
        var failures: [String] = []

        if let store = try? ConnectionStore() {
            for profile in store.profiles {
                do {
                    try store.removeSession(for: profile)
                } catch {
                    failures.append("session for \(profile.id.uuidString): \(error)")
                }
                do {
                    try store.removeProfile(profile.id)
                } catch {
                    failures.append("profile \(profile.id.uuidString): \(error)")
                }
            }
        } else {
            failures.append("profile store could not be opened")
        }

        // Consent lives in the App Group container, which outlives the app
        // install; only the app can revoke it.
        if SharingConsent.shared.defaults != nil {
            SharingConsent.shared.revokeAll()
        } else {
            failures.append("sharing consent storage is unavailable")
        }

        if !failures.isEmpty {
            publish(failures)
            throw TestingStateResetError(failures: failures)
        }
        // A previous attempt's failure must not be mistaken for this one's.
        SharingConsent.shared.defaults?.removeObject(forKey: failureKey)
        SharingConsent.shared.defaults?.synchronize()
    }

    /// Records the failure so the test that requested the reset can see it.
    private static func publish(_ failures: [String]) {
        SharingConsent.shared.defaults?.set(failures.joined(separator: "; "),
                                            forKey: failureKey)
        SharingConsent.shared.defaults?.synchronize()
    }
}

/// The reset could not clear every piece of state.
struct TestingStateResetError: Error, CustomStringConvertible {
    let failures: [String]

    var description: String {
        "Could not reset the app state for UI testing: " + failures.joined(separator: "; ")
    }
}
