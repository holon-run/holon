import CryptoKit
import Foundation
import HolonClient
import XCTest
@testable import Holon

@MainActor
final class ConnectionStoreTests: XCTestCase {
    private func fixture(_ body: (UserDefaults, CredentialVault) throws -> Void) throws {
        let suite = "run.holon.ios.tests.\(UUID())"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }
        try body(defaults, CredentialVault(service: suite))
    }

    private func profile(id: UUID = UUID(), base: String = "https://example.com/api") throws -> ConnectionProfile {
        try ConnectionProfile(id: id, name: "Test", apiBaseURL: XCTUnwrap(URL(string: base)))
    }

    private func session(runtime: String = "runtime", user: String = "user", visibility: String = "private",
                         expiry: Date? = nil) -> StoredSession {
        StoredSession(credential: "session-secret", runtimeID: runtime, userID: user,
                      visibilityScopeID: visibility, expiresAt: expiry)
    }

    private func account(_ profile: ConnectionProfile, _ session: StoredSession) throws -> String {
        let encoder = JSONEncoder()
        encoder.outputFormatting = .sortedKeys
        let data = try encoder.encode(["profileID": profile.id.uuidString,
                                       "base": profile.apiBaseURL.absoluteString,
                                       "runtime": session.runtimeID, "user": session.userID,
                                       "visibility": session.visibilityScopeID])
        return "session." + SHA256.hash(data: data).map { String(format: "%02x", $0) }.joined()
    }

    func testStagedCredentialSurvivesReopenAndPromotesWithoutIdentityLeak() throws {
        try fixture { defaults, vault in
            let store = try ConnectionStore(defaults: defaults, vault: vault)
            let profile = try profile()
            try store.saveProfile(profile)
            defer { try? store.removeProfile(profile.id) }
            let pending = PendingCredential(apiBaseURL: profile.apiBaseURL, credential: "issued-secret",
                                            userID: "user", expiresAt: Date().addingTimeInterval(60))
            try store.stageSession(pending, for: profile)
            let reopened = try ConnectionStore(defaults: defaults, vault: vault)
            XCTAssertNil(try reopened.session(for: profile))
            XCTAssertEqual(try reopened.pendingSession(for: profile)?.credential, "issued-secret")
            XCTAssertFalse(String(reflecting: pending).contains("issued-secret"))
            let confirmed = StoredSession(credential: pending.credential, runtimeID: "runtime", userID: "user",
                                          visibilityScopeID: "private", expiresAt: pending.expiresAt)
            try reopened.saveSession(confirmed, for: profile)
            XCTAssertNil(try reopened.pendingSession(for: profile))
            XCTAssertEqual(try reopened.session(for: profile), confirmed)
            try reopened.stageSession(pending, for: profile)
            try reopened.removeProfile(profile.id)
            XCTAssertNil(try vault.read(account: "pending-session.\(profile.id.uuidString)"))
        }
    }

    func testConfirmedExpiryDoesNotDeleteNewStagedCredentialAndPendingExpiryCleansUp() throws {
        try fixture { defaults, vault in
            let store = try ConnectionStore(defaults: defaults, vault: vault)
            let profile = try profile()
            try store.saveProfile(profile)
            defer { try? store.removeProfile(profile.id) }
            try store.saveSession(session(expiry: Date(timeIntervalSince1970: 100)), for: profile)
            let pending = PendingCredential(apiBaseURL: profile.apiBaseURL, credential: "issued",
                                            userID: "user", expiresAt: Date().addingTimeInterval(60))
            try store.stageSession(pending, for: profile)
            XCTAssertNil(try store.session(for: profile))
            XCTAssertNotNil(try store.pendingSession(for: profile))
            XCTAssertNil(try store.pendingSession(for: profile, now: pending.expiresAt!))
            XCTAssertNil(try vault.read(account: "pending-session.\(profile.id.uuidString)"))
        }
    }

    func testReopenSelectionExpiryAndRemoval() throws {
        try fixture { defaults, vault in
            let store = try ConnectionStore(defaults: defaults, vault: vault)
            let profile = try profile()
            defer { try? store.removeProfile(profile.id) }
            try store.saveProfile(profile)
            try store.select(profile.id)
            let saved = session(expiry: Date(timeIntervalSince1970: 100))
            try store.saveSession(saved, for: profile)
            let reopened = try ConnectionStore(defaults: defaults, vault: vault)
            XCTAssertEqual(reopened.profiles, [profile])
            XCTAssertEqual(reopened.selectedID, profile.id)
            XCTAssertEqual(try reopened.session(for: profile, now: Date(timeIntervalSince1970: 99)), saved)
            XCTAssertNil(try reopened.session(for: profile, now: Date(timeIntervalSince1970: 100)))
            XCTAssertNil(try vault.read(account: account(profile, saved)))
            try reopened.saveSession(session(), for: profile)
            try reopened.removeProfile(profile.id)
            XCTAssertNil(try vault.read(account: account(profile, session())))
            XCTAssertNil(try vault.read(account: "session-index.\(profile.id.uuidString)"))
            let empty = try ConnectionStore(defaults: defaults, vault: vault)
            XCTAssertTrue(empty.profiles.isEmpty)
            XCTAssertNil(empty.selectedID)
        }
    }

    func testFullIdentityPartitionAndRevocation() throws {
        try fixture { defaults, vault in
            let store = try ConnectionStore(defaults: defaults, vault: vault)
            let first = try profile()
            let second = try profile(base: "https://other.example.com/api")
            defer { try? store.removeProfile(first.id); try? store.removeProfile(second.id) }
            try store.saveProfile(first)
            try store.saveProfile(second)
            let original = session()
            try store.saveSession(original, for: first)
            try store.saveSession(original, for: second)
            XCTAssertNotEqual(try account(first, original), try account(second, original))
            var previous = original
            for replacement in [session(runtime: "other"), session(user: "other"), session(visibility: "shared")] {
                XCTAssertNotEqual(try account(first, previous), try account(first, replacement))
                try store.saveSession(replacement, for: first)
                XCTAssertNil(try vault.read(account: try account(first, previous)))
                XCTAssertEqual(try store.session(for: first), replacement)
                XCTAssertEqual(try store.session(for: second), original)
                previous = replacement
            }
            let changed = try profile(id: first.id, base: "https://example.com/proxy/api")
            XCTAssertNotEqual(try account(first, previous), try account(changed, previous))
            try store.saveProfile(changed)
            XCTAssertNil(try vault.read(account: try account(first, previous)))
            XCTAssertNil(try store.session(for: changed))
            XCTAssertThrowsError(try store.session(for: first))
            try store.saveSession(original, for: changed)
            let confirmed = try ConnectionProfile(id: changed.id, name: changed.name,
                                                  apiBaseURL: changed.apiBaseURL, allowInsecureHTTP: true)
            try store.saveProfile(confirmed)
            XCTAssertNil(try store.session(for: confirmed))
        }
    }

    func testProfilesContainNoCredentialsAndDescriptionsRedact() throws {
        try fixture { defaults, vault in
            let store = try ConnectionStore(defaults: defaults, vault: vault)
            let profile = try profile()
            defer { try? store.removeProfile(profile.id) }
            try store.saveProfile(profile)
            try store.saveProfile(profile)
            XCTAssertEqual(store.profiles.count, 1)
            let saved = session()
            try store.saveSession(saved, for: profile)
            let data = try XCTUnwrap(defaults.data(forKey: "run.holon.ios.connectionProfiles.v1"))
            let text = try XCTUnwrap(String(data: data, encoding: .utf8))
            XCTAssertFalse(text.contains(saved.credential))
            XCTAssertFalse(text.contains("credential"))
            XCTAssertFalse(text.contains("runtimeID"))
            XCTAssertFalse(String(describing: saved).contains(saved.credential))
            XCTAssertFalse(String(reflecting: saved).contains(saved.credential))
        }
    }

    func testRestoreRejectsDuplicateIDsAndInsecureEndpointTampering() throws {
        try fixture { defaults, vault in
            let profile = try profile()
            let encoded = try JSONEncoder().encode(profile)
            let object = try XCTUnwrap(JSONSerialization.jsonObject(with: encoded) as? [String: Any])
            let key = "run.holon.ios.connectionProfiles.v1"
            defaults.set(try JSONSerialization.data(withJSONObject: ["profiles": [object, object]]), forKey: key)
            XCTAssertThrowsError(try ConnectionStore(defaults: defaults, vault: vault))
            var tampered = object
            tampered["apiBaseURL"] = "http://example.com/api"
            defaults.set(try JSONSerialization.data(withJSONObject: ["profiles": [tampered]]), forKey: key)
            XCTAssertThrowsError(try ConnectionStore(defaults: defaults, vault: vault))
            XCTAssertThrowsError(try ConnectionProfile(name: "HTTP", apiBaseURL: XCTUnwrap(URL(string: "http://example.com/api"))))
        }
    }

    func testHTTPProfilesRequireConfirmationIncludingLoopback() throws {
        for address in ["http://localhost/api", "http://127.0.0.1/api", "http://[::1]/api", "http://example.com/api"] {
            let url = try XCTUnwrap(URL(string: address))
            XCTAssertThrowsError(try ConnectionProfile(name: "HTTP", apiBaseURL: url)) {
                XCTAssertEqual($0 as? HolonClientError, .insecureHTTPRequiresConfirmation)
            }
            let confirmed = try ConnectionProfile(name: "HTTP", apiBaseURL: url, allowInsecureHTTP: true)
            XCTAssertNoThrow(try confirmed.endpoint)
            let encoded = try JSONEncoder().encode(confirmed)
            var object = try XCTUnwrap(JSONSerialization.jsonObject(with: encoded) as? [String: Any])
            object["allowInsecureHTTP"] = false
            let tampered = try JSONSerialization.data(withJSONObject: object)
            XCTAssertThrowsError(try JSONDecoder().decode(ConnectionProfile.self, from: tampered))
        }
    }

    func testEquivalentAddressesReuseProfileWithoutChangingSessionOrName() throws {
        try fixture { defaults, vault in
            let store = try ConnectionStore(defaults: defaults, vault: vault)
            let original = try profile(base: "https://EXAMPLE.com:443/api")
            try store.saveProfile(original)
            defer { try? store.removeProfile(original.id) }
            try store.select(original.id)
            try store.saveSession(session(), for: original)
            for base in ["https://example.com/api/", "https://example.com:443/api///"] {
                let duplicate = try ConnectionProfile(name: "New name", apiBaseURL: XCTUnwrap(URL(string: base)))
                XCTAssertEqual(try store.saveProfile(duplicate), original)
            }
            XCTAssertEqual(store.profiles, [original])
            XCTAssertEqual(store.selectedID, original.id)
            XCTAssertEqual(try store.session(for: original), session())
            XCTAssertEqual(try ConnectionStore(defaults: defaults, vault: vault).profiles, [original])
        }
    }

    func testDifferentSchemesPortsAndPathsRemainSeparate() throws {
        try fixture { defaults, vault in
            let store = try ConnectionStore(defaults: defaults, vault: vault)
            defer { for p in store.profiles { try? store.removeProfile(p.id) } }
            for base in ["https://example.com/api", "http://example.com/api", "https://example.com:7878/api",
                         "https://example.com/proxy/api", "https://example.com/API"] {
                try store.saveProfile(ConnectionProfile(name: "Host", apiBaseURL: XCTUnwrap(URL(string: base)),
                                                        allowInsecureHTTP: true))
            }
            XCTAssertEqual(store.profiles.count, 5)
            let http = try ConnectionProfile(name: "HTTP", apiBaseURL: XCTUnwrap(URL(string: "http://EXAMPLE.com:80/api/")),
                                             allowInsecureHTTP: true)
            XCTAssertEqual(try store.saveProfile(http).id, store.profiles[1].id)
        }
    }

    func testLegacyDedupKeepsSelectedPartitionAndRemovesDiscardedCredentials() throws {
        try fixture { defaults, vault in
            let first = try profile()
            let selected = try profile(base: "https://EXAMPLE.com:443/api/")
            // Seed the previous format with separate UUID-scoped sessions, without migrating credentials.
            for p in [first, selected] {
                let scope = ["profileID": p.id.uuidString, "base": p.apiBaseURL.absoluteString,
                             "runtime": "runtime", "user": "user", "visibility": "private"]
                try vault.write(JSONEncoder().encode(session()), account: account(p, session()))
                try vault.write(JSONEncoder().encode(scope), account: "session-index.\(p.id.uuidString)")
                let pending = PendingCredential(apiBaseURL: p.apiBaseURL, credential: "pending", userID: "user", expiresAt: nil)
                try vault.write(JSONEncoder().encode(pending), account: "pending-session.\(p.id.uuidString)")
            }
            struct LegacyState: Encodable { let profiles: [ConnectionProfile]; let selectedID: UUID? }
            defaults.set(try JSONEncoder().encode(LegacyState(profiles: [first, selected], selectedID: selected.id)),
                         forKey: "run.holon.ios.connectionProfiles.v1")
            let store = try ConnectionStore(defaults: defaults, vault: vault)
            defer { try? store.removeProfile(selected.id) }
            XCTAssertEqual(store.profiles, [selected])
            XCTAssertEqual(store.selectedID, selected.id)
            XCTAssertEqual(try store.session(for: selected), session())
            XCTAssertEqual(try store.pendingSession(for: selected)?.credential, "pending")
            XCTAssertNil(try vault.read(account: account(first, session())))
            XCTAssertNil(try vault.read(account: "session-index.\(first.id.uuidString)"))
            XCTAssertNil(try vault.read(account: "pending-session.\(first.id.uuidString)"))
            XCTAssertEqual(try ConnectionStore(defaults: defaults, vault: vault).profiles, [selected])
        }
    }

    func testLegacyDedupWithoutSelectionKeepsFirstAndEditingCannotCollide() throws {
        try fixture { defaults, vault in
            let first = try profile()
            let duplicate = try profile(base: "https://example.com/api/")
            struct LegacyState: Encodable { let profiles: [ConnectionProfile] }
            defaults.set(try JSONEncoder().encode(LegacyState(profiles: [first, duplicate])),
                         forKey: "run.holon.ios.connectionProfiles.v1")
            let store = try ConnectionStore(defaults: defaults, vault: vault)
            let other = try profile(base: "https://other.example.com/api")
            try store.saveProfile(other)
            defer { for p in store.profiles { try? store.removeProfile(p.id) } }
            XCTAssertEqual(store.profiles, [first, other])
            XCTAssertNil(store.selectedID)
            try store.saveSession(session(), for: other)
            let collision = try profile(id: other.id)
            XCTAssertThrowsError(try store.saveProfile(collision))
            XCTAssertEqual(try store.session(for: other), session())
        }
    }
}
