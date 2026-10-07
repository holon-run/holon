import Foundation
import HolonClient

/// Both camera and user-initiated paste use this offline, bounded parser.
enum ConnectionInput: Equatable {
    case pairing(HolonPairingInvitation)
    case address(URL)

    init(_ payload: String) throws {
        let input = payload.trimmingCharacters(in: .whitespacesAndNewlines)
        guard input.utf8.count <= 2048,
              var parts = URLComponents(string: input),
              parts.port.map({ (1...65535).contains($0) }) ?? true else {
            throw HolonClientError.invalidRequest
        }
        if parts.fragment != nil {
            self = .pairing(try HolonPairingInvitation(payload: input))
            return
        }
        // Address codes cannot contain tokens, userinfo, or arbitrary queries.
        guard let url = parts.url else { throw HolonClientError.invalidEndpoint }
        _ = try HolonEndpoint(apiBaseURL: url, allowInsecureHTTP: true)
        while parts.percentEncodedPath.hasSuffix("/") { parts.percentEncodedPath.removeLast() }
        if !parts.percentEncodedPath.hasSuffix("/api") { parts.percentEncodedPath += "/api" }
        guard let api = parts.url else { throw HolonClientError.invalidEndpoint }
        self = .address(try HolonEndpoint(apiBaseURL: api, allowInsecureHTTP: true).apiBaseURL)
    }
}
