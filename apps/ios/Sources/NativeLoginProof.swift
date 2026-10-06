import CryptoKit
import Foundation
import Security

/// Pending login proof is bound to one API base URL, before an identity exists.
struct NativeLoginProof: Codable, Equatable, Sendable, CustomStringConvertible, CustomDebugStringConvertible {
    static let callbackScheme = "run.holon.ios"
    static let lifetime: TimeInterval = 5 * 60

    let apiBaseURL: URL
    let state: String
    let verifier: String
    let createdAt: Date

    var description: String { "NativeLoginProof(<redacted>)" }
    var debugDescription: String { description }

    enum Failure: Error {
        case randomGeneration(OSStatus)
        case invalidBaseURL
        case invalidProof
        case invalidCallback
    }

    static func make(apiBaseURL: URL, now: Date = Date()) throws -> Self {
        let proof = Self(
            apiBaseURL: apiBaseURL,
            state: try randomSecret(),
            verifier: try randomSecret(),
            createdAt: now
        )
        _ = try proof.startURL()
        return proof
    }

    var challenge: String {
        Self.base64URL(Data(SHA256.hash(data: Data(verifier.utf8))))
    }

    func isValid(at now: Date) -> Bool {
        let age = now.timeIntervalSince(createdAt)
        return age >= 0 && age < Self.lifetime && Self.isSecret(state) && Self.isSecret(verifier)
    }

    /// API base includes `/api/` and any reverse-proxy prefix.
    func startURL() throws -> URL {
        guard let base = URLComponents(url: apiBaseURL, resolvingAgainstBaseURL: false),
              base.scheme == "https", let host = base.host, !host.isEmpty,
              base.user == nil, base.password == nil, base.query == nil, base.fragment == nil
        else { throw Failure.invalidBaseURL }
        var url = URLComponents(
            url: apiBaseURL.appendingPathComponent("auth/oidc/native/start"),
            resolvingAgainstBaseURL: false
        )!
        url.queryItems = [
            URLQueryItem(name: "client", value: "ios"),
            URLQueryItem(name: "state", value: state),
            URLQueryItem(name: "code_challenge", value: challenge),
            URLQueryItem(name: "code_challenge_method", value: "S256"),
        ]
        return url.url!
    }

    func ticket(from callback: URL, now: Date = Date()) throws -> String {
        guard isValid(at: now) else { throw Failure.invalidProof }
        guard let url = URLComponents(url: callback, resolvingAgainstBaseURL: false),
              url.scheme == Self.callbackScheme, url.host == "oidc", url.path == "/callback",
              url.user == nil, url.password == nil, url.port == nil, url.fragment == nil
        else { throw Failure.invalidCallback }
        let items = url.queryItems ?? []
        guard Set(items.map(\.name)).count == items.count,
              items.first(where: { $0.name == "state" })?.value == state,
              items.first(where: { $0.name == "code_challenge_method" })?.value == "S256",
              let ticket = items.first(where: { $0.name == "ticket" })?.value,
              ticket.utf8.count == 64, ticket.utf8.allSatisfy({ (48...57).contains($0) || (97...102).contains($0) })
        else { throw Failure.invalidCallback }
        return ticket
    }

    private static func randomSecret() throws -> String {
        var bytes = [UInt8](repeating: 0, count: 32)
        let status = SecRandomCopyBytes(kSecRandomDefault, bytes.count, &bytes)
        guard status == errSecSuccess else { throw Failure.randomGeneration(status) }
        return base64URL(Data(bytes))
    }

    private static func isSecret(_ value: String) -> Bool {
        value.utf8.count == 43 && value.utf8.allSatisfy {
            (65...90).contains($0) || (97...122).contains($0) || (48...57).contains($0) || $0 == 45 || $0 == 95
        }
    }

    private static func base64URL(_ data: Data) -> String {
        data.base64EncodedString().replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_").replacingOccurrences(of: "=", with: "")
    }
}

struct NativeLoginProofStore {
    let vault: CredentialVault

    func save(_ proof: NativeLoginProof, now: Date = Date()) throws {
        guard proof.isValid(at: now) else { throw NativeLoginProof.Failure.invalidProof }
        _ = try proof.startURL()
        let previous = try loadPending(apiBaseURL: proof.apiBaseURL, now: now)
        let key = account(base: proof.apiBaseURL, state: proof.state)
        try vault.write(try JSONEncoder().encode(proof), account: key)
        do {
            try vault.write(Data(proof.state.utf8), account: pendingAccount(base: proof.apiBaseURL))
        } catch {
            try vault.remove(account: key)
            throw error
        }
        if let previous, previous.state != proof.state { try remove(previous) }
    }

    func load(apiBaseURL: URL, state: String, now: Date = Date()) throws -> NativeLoginProof? {
        let key = account(base: apiBaseURL, state: state)
        guard let data = try vault.read(account: key) else { return nil }
        let proof = try JSONDecoder().decode(NativeLoginProof.self, from: data)
        guard proof.apiBaseURL == apiBaseURL, proof.state == state, proof.isValid(at: now) else {
            try vault.remove(account: key)
            return nil
        }
        return proof
    }

    /// The restart locator is sensitive too: it never belongs in UserDefaults.
    func loadPending(apiBaseURL: URL, now: Date = Date()) throws -> NativeLoginProof? {
        let key = pendingAccount(base: apiBaseURL)
        guard let data = try vault.read(account: key) else { return nil }
        guard let state = String(data: data, encoding: .utf8),
              let proof = try load(apiBaseURL: apiBaseURL, state: state, now: now) else {
            try vault.remove(account: key)
            return nil
        }
        return proof
    }

    /// Remove after cancellation or confirmed exchange, not a lost HTTP response.
    func remove(_ proof: NativeLoginProof) throws {
        try vault.remove(account: account(base: proof.apiBaseURL, state: proof.state))
        let key = pendingAccount(base: proof.apiBaseURL)
        if try vault.read(account: key) == Data(proof.state.utf8) {
            try vault.remove(account: key)
        }
    }

    private func pendingAccount(base: URL) -> String {
        "native-oidc-active/" + digest(base)
    }

    private func account(base: URL, state: String) -> String {
        "native-oidc/\(digest(base))/\(state)"
    }

    private func digest(_ base: URL) -> String {
        SHA256.hash(data: Data(base.absoluteString.utf8)).map { String(format: "%02x", $0) }.joined()
    }
}
