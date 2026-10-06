import Foundation

/// Offline preview of the daemon's login URL. No credential is installed here.
public struct HolonPairingInvitation: Equatable, Sendable, CustomStringConvertible, CustomDebugStringConvertible {
    public let apiBaseURL: URL
    public let ticket: String
    /// The current URL format carries no expiry; redemption validates it server-side.
    public let expiresAt: Date?

    public var description: String { "HolonPairingInvitation(ticket: <redacted>)" }
    public var debugDescription: String { description }

    public init(payload: String) throws {
        let input = payload.trimmingCharacters(in: .whitespacesAndNewlines)
        guard input.utf8.count <= 2048,
              var parts = URLComponents(string: input),
              parts.query == nil,
              parts.percentEncodedPath.hasSuffix("/login"),
              let fragment = parts.percentEncodedFragment,
              fragment.hasPrefix("pair=") else {
            throw HolonClientError.invalidRequest
        }
        let ticket = String(fragment.dropFirst(5))
        guard ticket.utf8.count == 64,
              ticket.utf8.allSatisfy({
                  (48...57).contains($0) || (65...70).contains($0) || (97...102).contains($0)
              }),
              parts.port.map({ (1...65535).contains($0) }) ?? true else {
            throw HolonClientError.invalidRequest
        }
        parts.fragment = nil
        // Preserve a reverse proxy prefix without decoding its path segments.
        parts.percentEncodedPath = String(parts.percentEncodedPath.dropLast(6)) + "/api"
        guard let url = parts.url else { throw HolonClientError.invalidEndpoint }
        // Parsing may preview remote HTTP; only explicit confirmation may use it.
        apiBaseURL = try HolonEndpoint(apiBaseURL: url, allowInsecureHTTP: true).apiBaseURL
        self.ticket = ticket
        expiresAt = nil
    }

    public func endpoint(allowInsecureHTTP: Bool = false) throws -> HolonEndpoint {
        guard apiBaseURL.scheme != "http" || allowInsecureHTTP else {
            throw HolonClientError.insecureHTTPRequiresConfirmation
        }
        return try HolonEndpoint(apiBaseURL: apiBaseURL, allowInsecureHTTP: allowInsecureHTTP)
    }
}
