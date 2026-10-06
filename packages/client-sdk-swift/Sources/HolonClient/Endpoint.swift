import Foundation

public enum HolonClientError: Error, Equatable, Sendable {
    case invalidEndpoint
    case insecureHTTPRequiresConfirmation
    case invalidRequest
    case invalidRetryPolicy
    case malformedResponse
    case unexpectedContentType
    case staleConnection
    case closed
    case streamEnded
    case streamLimitExceeded
}

/// The complete API base, including `/api` or a reverse proxy's prefix.
public struct HolonEndpoint: Equatable, Sendable {
    public let apiBaseURL: URL

    public init(apiBaseURL: URL, allowInsecureHTTP: Bool = false) throws {
        guard var parts = URLComponents(url: apiBaseURL, resolvingAgainstBaseURL: false),
              let scheme = parts.scheme?.lowercased(), ["http", "https"].contains(scheme),
              let host = parts.host?.lowercased(), !host.isEmpty,
              parts.user == nil, parts.password == nil,
              parts.query == nil, parts.fragment == nil else {
            throw HolonClientError.invalidEndpoint
        }
        let loopback = host == "localhost" || host == "::1" || host == "[::1]" ||
            Self.isIPv4Loopback(host)
        guard scheme == "https" || loopback || allowInsecureHTTP else {
            throw HolonClientError.insecureHTTPRequiresConfirmation
        }
        // A prefix may not smuggle traversal or an encoded path separator.
        for segment in parts.percentEncodedPath.split(separator: "/") {
            guard let decoded = String(segment).removingPercentEncoding,
                  decoded != ".", decoded != "..", !decoded.contains("/"),
                  !decoded.contains("\\") else { throw HolonClientError.invalidEndpoint }
        }
        parts.scheme = scheme
        while parts.percentEncodedPath.hasSuffix("/") { parts.percentEncodedPath.removeLast() }
        parts.percentEncodedPath += "/"
        guard let url = parts.url else { throw HolonClientError.invalidEndpoint }
        self.apiBaseURL = url
    }

    public func url(path: [String], query: [String: String] = [:]) throws -> URL {
        guard !path.isEmpty,
              path.allSatisfy({ !$0.isEmpty && $0 != "." && $0 != ".." }) else {
            throw HolonClientError.invalidRequest
        }
        var parts = URLComponents(url: apiBaseURL, resolvingAgainstBaseURL: false)!
        let unreserved = CharacterSet(charactersIn:
            "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-._~")
        parts.percentEncodedPath += try path.map {
            guard let encoded = $0.addingPercentEncoding(withAllowedCharacters: unreserved) else {
                throw HolonClientError.invalidRequest
            }
            return encoded
        }.joined(separator: "/")
        parts.queryItems = query.isEmpty ? nil : query.sorted { $0.key < $1.key }.map {
            URLQueryItem(name: $0.key, value: $0.value)
        }
        guard let url = parts.url else { throw HolonClientError.invalidRequest }
        return url
    }

    private static func isIPv4Loopback(_ host: String) -> Bool {
        let octets = host.split(separator: ".", omittingEmptySubsequences: false)
        return octets.count == 4 && octets[0] == "127" && octets.allSatisfy {
            !$0.isEmpty && $0.allSatisfy(\.isNumber) &&
                ($0.count == 1 || !$0.hasPrefix("0")) && (Int($0).map { (0...255).contains($0) } ?? false)
        }
    }
}

public struct HolonConnectionIdentity: Equatable, Hashable, Sendable {
    public let networkID: String
    public let runtimeID: String?
    public let userID: String?
    public let visibilityScopeID: String?
    public let generation: UUID

    public init(networkID: String, runtimeID: String? = nil, userID: String? = nil,
                visibilityScopeID: String? = nil) {
        self.networkID = networkID
        self.runtimeID = runtimeID
        self.userID = userID
        self.visibilityScopeID = visibilityScopeID
        generation = UUID()
    }
}

public struct HolonResponse<Value: Sendable>: Sendable {
    public let identity: HolonConnectionIdentity
    public let value: Value
}

public struct HolonRetryPolicy: Equatable, Sendable {
    public static let none = try! HolonRetryPolicy()
    public let maxAttempts: Int
    public let baseDelay: TimeInterval
    public let maximumDelay: TimeInterval

    public init(maxAttempts: Int = 1, baseDelay: TimeInterval = 0.5,
                maximumDelay: TimeInterval = 30) throws {
        guard (1...8).contains(maxAttempts), baseDelay.isFinite, maximumDelay.isFinite,
              baseDelay >= 0, maximumDelay >= baseDelay, maximumDelay <= 300 else {
            throw HolonClientError.invalidRetryPolicy
        }
        self.maxAttempts = maxAttempts
        self.baseDelay = baseDelay
        self.maximumDelay = maximumDelay
    }

    func delay(afterAttempt attempt: Int) -> TimeInterval {
        min(maximumDelay, baseDelay * pow(2, Double(attempt - 1)))
    }
}

public struct HolonRetryNotice: Sendable {
    public let identity: HolonConnectionIdentity
    public let nextAttempt: Int
    public let delay: TimeInterval
}

public struct HolonHTTPFailure: Error, Equatable, Sendable {
    public let statusCode: Int
    public let apiError: HolonAPIError?
    public let identity: HolonConnectionIdentity
}
