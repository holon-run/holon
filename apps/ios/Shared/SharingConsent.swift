import Foundation

/// Shared with the extension. Missing App Group storage always denies sharing.
// UserDefaults is thread-safe; the wrapper has no independently mutable state.
struct SharingConsent: @unchecked Sendable {
    static let version = 1
    static let shared: SharingConsent = {
        guard let group = Bundle.main.object(forInfoDictionaryKey: "HolonSharedAppGroup") as? String,
              !group.isEmpty, !group.contains("$("),
              FileManager.default.containerURL(forSecurityApplicationGroupIdentifier: group) != nil else {
            return SharingConsent(defaults: nil)
        }
        return SharingConsent(defaults: UserDefaults(suiteName: group))
    }()

    let defaults: UserDefaults?

    private func key(_ url: URL) -> String {
        "sharing.consent.v\(Self.version).\(url.absoluteString)"
    }

    func approved(_ url: URL) -> Bool {
        defaults?.synchronize()
        return defaults?.bool(forKey: key(url)) == true
    }

    func approve(_ url: URL) {
        defaults?.set(true, forKey: key(url))
        defaults?.synchronize()
    }

    func revoke(_ url: URL) {
        defaults?.removeObject(forKey: key(url))
        defaults?.synchronize()
    }

    func require(_ url: URL) throws {
        guard approved(url) else { throw SharingConsentRequired() }
    }
}

struct SharingConsentRequired: LocalizedError {
    var errorDescription: String? {
        NSLocalizedString("privacy.required", comment: "Sharing needs explicit consent in the main app")
    }
}
