import CryptoKit
import Foundation
import HolonClient

struct ConnectionProfile: Identifiable, Codable, Equatable, Sendable {
    let id: UUID
    let name: String
    let apiBaseURL: URL
    let allowInsecureHTTP: Bool

    init(id: UUID = UUID(), name: String, apiBaseURL: URL, allowInsecureHTTP: Bool = false) throws {
        _ = try HolonEndpoint(apiBaseURL: apiBaseURL, allowInsecureHTTP: allowInsecureHTTP)
        // Production profiles require confirmation even for SDK loopback probes.
        guard apiBaseURL.scheme?.lowercased() != "http" || allowInsecureHTTP else {
            throw HolonClientError.insecureHTTPRequiresConfirmation
        }
        self.id = id
        self.name = name
        self.apiBaseURL = apiBaseURL
        self.allowInsecureHTTP = allowInsecureHTTP
    }

    var endpoint: HolonEndpoint {
        get throws { try HolonEndpoint(apiBaseURL: apiBaseURL, allowInsecureHTTP: allowInsecureHTTP) }
    }

    /// Comparison only: stored URLs remain unchanged because credentials are scoped to them.
    var endpointKey: String {
        get throws {
            let endpoint = try endpoint
            guard var parts = URLComponents(url: endpoint.apiBaseURL, resolvingAgainstBaseURL: false) else {
                throw HolonClientError.invalidRequest
            }
            parts.host = parts.host?.lowercased()
            if (parts.scheme == "https" && parts.port == 443) || (parts.scheme == "http" && parts.port == 80) {
                parts.port = nil
            }
            guard let url = parts.url else { throw HolonClientError.invalidRequest }
            return url.absoluteString
        }
    }

    init(from decoder: Decoder) throws {
        let values = try decoder.container(keyedBy: CodingKeys.self)
        try self.init(id: values.decode(UUID.self, forKey: .id),
                      name: values.decode(String.self, forKey: .name),
                      apiBaseURL: values.decode(URL.self, forKey: .apiBaseURL),
                      allowInsecureHTTP: values.decode(Bool.self, forKey: .allowInsecureHTTP))
    }
}

struct StoredSession: Codable, Equatable, Sendable, CustomStringConvertible, CustomDebugStringConvertible {
    let credential: String
    let runtimeID: String
    let userID: String
    let visibilityScopeID: String
    let expiresAt: Date?

    init(credential: String, runtimeID: String, userID: String, visibilityScopeID: String, expiresAt: Date?) {
        self.credential = credential
        self.runtimeID = runtimeID
        self.userID = userID
        self.visibilityScopeID = visibilityScopeID
        self.expiresAt = expiresAt
    }

    var description: String { "StoredSession(<redacted>)" }
    var debugDescription: String { description }
}

/// A confirmed exchange awaiting authoritative identity bootstrap, not a cache identity.
struct PendingCredential: Codable, Sendable, CustomStringConvertible, CustomDebugStringConvertible {
    let apiBaseURL: URL
    let credential: String
    let userID: String
    let expiresAt: Date?
    var description: String { "PendingCredential(<redacted>)" }
    var debugDescription: String { description }
}

@MainActor
final class ConnectionStore {
    enum Failure: Error { case invalidStorage, unknownProfile, duplicateEndpoint }

    private struct State: Codable {
        var profiles: [ConnectionProfile]
        var selectedID: UUID?
    }

    private struct Scope: Codable, Equatable {
        let profileID: UUID
        let base: String
        let runtime: String
        let user: String
        let visibility: String

        var account: String {
            get throws {
                let encoder = JSONEncoder()
                encoder.outputFormatting = .sortedKeys
                let digest = SHA256.hash(data: try encoder.encode(self))
                return "session." + digest.map { String(format: "%02x", $0) }.joined()
            }
        }
    }

    private static let stateKey = "run.holon.ios.connectionProfiles.v1"
    private let defaults: UserDefaults
    private let vault: CredentialVault
    private(set) var profiles: [ConnectionProfile]
    private(set) var selectedID: UUID?

    init(defaults: UserDefaults = .standard,
         vault: CredentialVault = CredentialVault(service: "run.holon.ios.sessions")) throws {
        self.defaults = defaults
        self.vault = vault
        if let object = defaults.object(forKey: Self.stateKey) {
            guard let data = object as? Data,
                  let state = try? JSONDecoder().decode(State.self, from: data),
                  Set(state.profiles.map(\.id)).count == state.profiles.count,
                  state.selectedID == nil || state.profiles.contains(where: { $0.id == state.selectedID }) else {
                throw Failure.invalidStorage
            }
            profiles = state.profiles
            selectedID = state.selectedID
            try removeLegacyDuplicates()
        } else {
            profiles = []
            selectedID = nil
        }
    }

    private func removeLegacyDuplicates() throws {
        var retained: [String: UUID] = [:]
        for profile in profiles {
            let key = try profile.endpointKey
            if retained[key] == nil || profile.id == selectedID { retained[key] = profile.id }
        }
        let unique = try profiles.filter { retained[try $0.endpointKey] == $0.id }
        guard unique.count != profiles.count else { return }
        // Keep the selected profile and its partition. Never transplant another ID's session.
        for profile in profiles where !unique.contains(profile) { try removeSession(for: profile) }
        try persist(unique, selectedID: selectedID)
    }

    private func persist(_ profiles: [ConnectionProfile], selectedID: UUID?) throws {
        let data = try JSONEncoder().encode(State(profiles: profiles, selectedID: selectedID))
        defaults.set(data, forKey: Self.stateKey)
        self.profiles = profiles
        self.selectedID = selectedID
    }

    @discardableResult
    func saveProfile(_ profile: ConnectionProfile) throws -> ConnectionProfile {
        let key = try profile.endpointKey
        if let existing = try profiles.first(where: { try $0.endpointKey == key && $0.id != profile.id }) {
            guard !profiles.contains(where: { $0.id == profile.id }) else { throw Failure.duplicateEndpoint }
            return existing
        }
        var updated = profiles
        if let position = updated.firstIndex(where: { $0.id == profile.id }) {
            let previous = updated[position]
            if previous.apiBaseURL != profile.apiBaseURL || previous.allowInsecureHTTP != profile.allowInsecureHTTP {
                try removeSession(for: previous)
            }
            updated[position] = profile
        } else {
            updated.append(profile)
        }
        try persist(updated, selectedID: selectedID)
        return profile
    }

    func select(_ id: UUID?) throws {
        guard id == nil || profiles.contains(where: { $0.id == id }) else { throw Failure.unknownProfile }
        try persist(profiles, selectedID: id)
    }

    func removeProfile(_ id: UUID) throws {
        guard let profile = profiles.first(where: { $0.id == id }) else { throw Failure.unknownProfile }
        try removeSession(for: profile)
        try persist(profiles.filter { $0.id != id }, selectedID: selectedID == id ? nil : selectedID)
    }

    private func validate(_ profile: ConnectionProfile) throws {
        guard profiles.contains(profile) else { throw Failure.unknownProfile }
        _ = try profile.endpoint
    }

    private func indexAccount(_ profile: ConnectionProfile) -> String { "session-index.\(profile.id.uuidString)" }
    private func pendingAccount(_ profile: ConnectionProfile) -> String { "pending-session.\(profile.id.uuidString)" }

    func pendingSession(for profile: ConnectionProfile, now: Date = Date()) throws -> PendingCredential? {
        try validate(profile)
        let key = pendingAccount(profile)
        guard let data = try vault.read(account: key) else { return nil }
        guard let pending = try? JSONDecoder().decode(PendingCredential.self, from: data),
              pending.apiBaseURL == profile.apiBaseURL, !pending.credential.isEmpty,
              !pending.userID.isEmpty else { throw Failure.invalidStorage }
        if let expiry = pending.expiresAt, expiry <= now {
            try vault.remove(account: key)
            return nil
        }
        return pending
    }

    func stageSession(_ pending: PendingCredential, for profile: ConnectionProfile) throws {
        try validate(profile)
        guard pending.apiBaseURL == profile.apiBaseURL, !pending.credential.isEmpty,
              !pending.userID.isEmpty, pending.expiresAt.map({ $0 > Date() }) ?? true else {
            throw Failure.invalidStorage
        }
        try vault.write(try JSONEncoder().encode(pending), account: pendingAccount(profile))
    }

    private func scope(for profile: ConnectionProfile) throws -> Scope? {
        guard let data = try vault.read(account: indexAccount(profile)) else { return nil }
        guard let scope = try? JSONDecoder().decode(Scope.self, from: data),
              scope.profileID == profile.id, scope.base == profile.apiBaseURL.absoluteString else {
            throw Failure.invalidStorage
        }
        return scope
    }

    func session(for profile: ConnectionProfile, now: Date = Date()) throws -> StoredSession? {
        try validate(profile)
        guard let scope = try scope(for: profile), let data = try vault.read(account: scope.account) else { return nil }
        guard let session = try? JSONDecoder().decode(StoredSession.self, from: data),
              session.runtimeID == scope.runtime, session.userID == scope.user,
              session.visibilityScopeID == scope.visibility else { throw Failure.invalidStorage }
        if let expiry = session.expiresAt, expiry <= now {
            try removeConfirmedSession(for: profile)
            return nil
        }
        return session
    }

    func saveSession(_ session: StoredSession, for profile: ConnectionProfile) throws {
        try validate(profile)
        let scope = Scope(profileID: profile.id, base: profile.apiBaseURL.absoluteString,
                          runtime: session.runtimeID, user: session.userID, visibility: session.visibilityScopeID)
        let data = try JSONEncoder().encode(session)
        let index = try JSONEncoder().encode(scope)
        // Retain the staged credential until complete-scope promotion succeeds.
        try removeConfirmedSession(for: profile)
        try vault.write(data, account: scope.account)
        do { try vault.write(index, account: indexAccount(profile)) }
        catch {
            try vault.remove(account: scope.account)
            throw error
        }
        try vault.remove(account: pendingAccount(profile))
    }

    func removeSession(for profile: ConnectionProfile) throws {
        try validate(profile)
        try vault.remove(account: pendingAccount(profile))
        try removeConfirmedSession(for: profile)
    }

    private func removeConfirmedSession(for profile: ConnectionProfile) throws {
        if let scope = try scope(for: profile) { try vault.remove(account: scope.account) }
        try vault.remove(account: indexAccount(profile))
    }
}
