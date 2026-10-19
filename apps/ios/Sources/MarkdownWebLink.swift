import Foundation

/// Web link classification aligned with Android `MarkdownLinkTargets` and the web GUI routes.
/// `nil` leaves the existing host-file/document classifier in charge; `.rejected`
/// must never fall through to a host-file link.
enum MarkdownWebLink: Equatable {
    case web(String)
    case mail(String)
    case rejected

    static func classify(_ href: String) -> MarkdownWebLink? {
        let network = href.hasPrefix("//")
        let absolute = href.range(of: #"^https?:"#, options: [.regularExpression, .caseInsensitive]) != nil
        let mail = href.range(of: #"^mailto:"#, options: [.regularExpression, .caseInsensitive]) != nil
        let root = href.hasPrefix("/") && !network
        let path = webPathOnly(href)
        let registered = root && (isAppPathShape(path) || isMarkdownGUIPath(path))
        let appShaped = root && path.range(of: #"^/apps/[^/]+/[^/]+"#, options: .regularExpression) != nil
        if !network && !absolute && !mail && !registered && !appShaped { return nil }
        guard hasNoUnsafeCharacters(href) else { return .rejected }
        do {
            if mail {
                let recipient = String(href.dropFirst("mailto:".count))
                guard !recipient.trimmingCharacters(in: .whitespaces).isEmpty else { return .rejected }
                try validateDecodedComponent(recipient)
                return .mail(href)
            }
            if root {
                // A root-relative href carries no authority by construction.
                guard registered else { return .rejected }
                try validateWebTarget(path: path, href: href)
            } else {
                guard !href.hasPrefix("///") else { return .rejected }
                guard network || href.range(of: #"^https?://"#, options: [.regularExpression, .caseInsensitive]) != nil,
                      let authority = webAuthority(href, network: network), isValidWebAuthority(authority),
                      try validDecodedAuthority(authority) else {
                    return .rejected
                }
                try validateWebTarget(path: path, href: href)
            }
            return .web(href)
        } catch {
            return .rejected
        }
    }

    /// Resolve against the runtime origin, not an API base path. Never normalize a suspicious path into safety.
    static func resolveRuntimeWebLink(_ href: String, runtimeBaseURL: String) -> String? {
        guard case .web = classify(href) else { return nil }
        guard hasNoUnsafeCharacters(runtimeBaseURL),
              let base = ParsedWebOrigin(runtimeBaseURL) else { return nil }
        let absolute: String
        if href.hasPrefix("//") {
            absolute = base.scheme + ":" + href
        } else if href.hasPrefix("/") {
            absolute = base.scheme + "://" + base.authority + href
        } else {
            absolute = href
        }
        guard let target = ParsedWebOrigin(absolute),
              target.scheme.lowercased() == base.scheme.lowercased(),
              target.host.lowercased() == base.host.lowercased(),
              target.effectivePort == base.effectivePort else { return nil }
        return absolute
    }

    private static func validateWebTarget(path: String, href: String) throws {
        try validateWebPath(path)
        if let query = webComponent(href, delimiter: "?") { try validateDecodedComponent(query) }
        if let fragment = webComponent(href, delimiter: "#") { try validateDecodedComponent(fragment) }
    }

    /// Each segment must stay a real path segment after strict decoding.
    private static func validateWebPath(_ path: String) throws {
        guard !path.isEmpty else { return }
        for segment in path.split(separator: "/", omittingEmptySubsequences: false) {
            let decoded = try strictlyDecoded(String(segment))
            guard decoded != ".", decoded != "..",
                  !decoded.unicodeScalars.contains(where: { $0 == "/" || $0 == "\\" || isControl($0) }),
                  decoded.range(of: #"%(?:2e|2f|5c|25)"#, options: [.regularExpression, .caseInsensitive]) == nil
            else { throw WebLinkError.confused }
        }
    }

    private static func validateDecodedComponent(_ raw: String) throws {
        let decoded = try strictlyDecoded(raw)
        if decoded.unicodeScalars.contains(where: { isControl($0) || $0 == "\\" }) {
            throw WebLinkError.confused
        }
    }

    /// An authority must not smuggle separators or userinfo through percent escapes.
    private static func validDecodedAuthority(_ authority: String) throws -> Bool {
        let decoded = try strictlyDecoded(authority)
        return !decoded.unicodeScalars.contains(where: { $0 == "/" || $0 == "\\" || $0 == "@" })
    }

    private static func hasNoUnsafeCharacters(_ value: String) -> Bool {
        !value.unicodeScalars.contains(where: { $0.value <= 0x20 || isControl($0) || $0 == "\\" })
    }

    private static func isControl(_ scalar: Unicode.Scalar) -> Bool {
        scalar.value <= 0x1f || (0x7f...0x9f).contains(scalar.value)
    }
}

private enum WebLinkError: Error { case confused }

/// Keep in step with web-gui/app/src/app/routes.ts: unknown absolute paths are host files.
func isMarkdownGUIPath(_ path: String) -> Bool {
    var trimmed = Substring(path)
    while trimmed.hasSuffix("/") { trimmed = trimmed.dropLast() }
    let route = trimmed.isEmpty ? "/" : String(trimmed)
    let simpleRoutes: Set<String> = ["/", "/files", "/search", "/skills", "/templates", "/settings"]
    if simpleRoutes.contains(route) { return true }
    return route.range(of: #"^/(?:skills|templates)/[^/]+$"#, options: .regularExpression) != nil ||
        route.range(of: #"^/agents/[^/]+(?:/conversation|/skills/[^/]+)?$"#, options: .regularExpression) != nil
}

func isAppPathShape(_ path: String) -> Bool {
    path.range(of: #"^/apps/[^/]+/[^/]+(?:/.*)?$"#, options: .regularExpression) != nil
}

private func webPathOnly(_ href: String) -> String {
    let beforeQuery = href.split(separator: "?", maxSplits: 1, omittingEmptySubsequences: false)[0]
    return String(beforeQuery.split(separator: "#", maxSplits: 1, omittingEmptySubsequences: false)[0])
}

private func webComponent(_ href: String, delimiter: Character) -> String? {
    guard let start = href.firstIndex(of: delimiter == "?" ? "?" : "#") else { return nil }
    let remainder = href.index(after: start)...
    guard delimiter == "?" else { return String(href[remainder]) }
    let query = String(href[remainder])
    guard let hash = query.firstIndex(of: "#") else { return query }
    return String(query[..<hash])
}

private func webAuthority(_ href: String, network: Bool) -> String? {
    let afterScheme: Substring
    if network {
        afterScheme = href.dropFirst(2)
    } else if let range = href.range(of: #"^https?://"#, options: [.regularExpression, .caseInsensitive]) {
        afterScheme = href[range.upperBound...]
    } else {
        return nil
    }
    guard let end = afterScheme.firstIndex(where: { $0 == "/" || $0 == "?" || $0 == "#" }) else {
        return String(afterScheme)
    }
    let authority = String(afterScheme[..<end])
    return authority.isEmpty ? nil : authority
}

private func isValidWebAuthority(_ authority: String) -> Bool {
    guard !authority.contains("@"), !authority.hasSuffix(":") else { return false }
    // A bracketed authority must be a well-formed IPv6 literal, as java.net.URI enforces on
    // Android; it never falls back to plain-host parsing.
    let suffix: String
    if authority.hasPrefix("[") {
        guard let closed = bracketedHostSuffix(authority) else { return false }
        suffix = closed
    } else {
        guard !authority.contains("]") else { return false }
        suffix = authority
    }
    guard let separator = suffix.lastIndex(of: ":") else { return true }
    let host = String(suffix[..<separator])
    let port = String(suffix[suffix.index(after: separator)...])
    return !host.isEmpty && !port.isEmpty && port.allSatisfy(\.isNumber) && (Int(port) ?? 65_536) <= 65_535
}

/// `inet_pton` shares java.net.URI's strict IPv6 literal grammar (no zone IDs, full group rules).
private func isIPv6Literal(_ literal: String) -> Bool {
    var address = in6_addr()
    return literal.withCString { inet_pton(AF_INET6, $0, &address) == 1 }
}

/// For `[v6.host]`-style authorities validates the literal plus optional numeric
/// port, and returns the `]`-onward suffix.
private func bracketedHostSuffix(_ authority: String) -> String? {
    guard authority.hasPrefix("[") else { return nil }
    guard let close = authority.firstIndex(of: "]"),
          authority.distance(from: authority.startIndex, to: close) > 1,
          isIPv6Literal(String(authority[authority.index(after: authority.startIndex)..<close])) else { return nil }
    let trailing = authority[authority.index(after: close)...]
    // java.net.URI accepts nothing after `]` except an optional non-empty numeric port.
    guard trailing.isEmpty ||
          (trailing.first == ":" && (1...5).contains(trailing.count - 1) && trailing.dropFirst().allSatisfy(\.isNumber)) else { return nil }
    return String(authority[close...])
}

/// Decode only for validation, with strict UTF-8; navigation always keeps the original href.
private func strictlyDecoded(_ raw: String) throws -> String {
    var bytes = [UInt8]()
    var index = raw.startIndex
    while index < raw.endIndex {
        if raw[index] == "%" {
            var cursor = index
            while cursor < raw.endIndex, raw[cursor] == "%" {
                let second = raw.index(after: cursor)
                let third = second < raw.endIndex ? raw.index(after: second) : raw.endIndex
                guard third < raw.endIndex,
                      let high = hexDigit(raw[second]), let low = hexDigit(raw[third]) else {
                    throw WebLinkError.confused
                }
                bytes.append(UInt8(high << 4 | low))
                cursor = raw.index(after: third)
            }
            index = cursor
        } else {
            bytes.append(contentsOf: String(raw[index]).utf8)
            index = raw.index(after: index)
        }
    }
    guard let decoded = String(bytes: bytes, encoding: .utf8) else { throw WebLinkError.confused }
    return decoded
}

private func hexDigit(_ character: Character) -> Int? {
    guard let scalar = character.unicodeScalars.first, character.isASCII else { return nil }
    switch scalar {
    case "0"..."9": return Int(scalar.value - 48)
    case "a"..."f": return Int(scalar.value - 87)
    case "A"..."F": return Int(scalar.value - 55)
    default: return nil
    }
}

/// A parsed http(s) origin: scheme, raw authority substring, comparable host, and effective port.
private struct ParsedWebOrigin {
    let scheme: String
    let authority: String
    let host: String
    let effectivePort: Int

    init?(_ value: String) {
        guard let schemeRange = value.range(of: #"^https?://"#, options: [.regularExpression, .caseInsensitive]) else {
            return nil
        }
        scheme = String(value[..<schemeRange.upperBound].dropLast(3))
        let afterScheme = value[schemeRange.upperBound...]
        let end = afterScheme.firstIndex(where: { $0 == "/" || $0 == "?" || $0 == "#" }) ?? afterScheme.endIndex
        authority = String(afterScheme[..<end])
        guard isValidWebAuthority(authority) else { return nil }
        let portPart: String
        if authority.hasPrefix("["), let close = authority.firstIndex(of: "]") {
            host = String(authority[authority.index(after: authority.startIndex)..<close])
            portPart = String(authority[authority.index(after: close)...])
        } else if let separator = authority.lastIndex(of: ":") {
            portPart = String(authority[separator...])
            host = String(authority[..<separator])
        } else {
            portPart = ""
            host = authority
        }
        let portText = portPart.hasPrefix(":") ? String(portPart.dropFirst()) : portPart
        effectivePort = portText.isEmpty ? (scheme.lowercased() == "https" ? 443 : 80) : (Int(portText) ?? -1)
        guard !host.isEmpty, (1...65_535).contains(effectivePort) else { return nil }
    }
}

/// Port of Android `bareMarkdownAppLinkAt`: bare `/apps/…` spans keep balanced punctuation
/// and stop at CJK sentence boundaries. Invalid candidates still surface so they
/// cannot fall through to host-file links.
enum BareAppLinks {
    struct Match: Equatable {
        let href: String
        let range: Range<String.Index>
    }

    private static let boundary = "(（[【:：，。；！？、"
    private static let terminators = "<>\"'，。；！？、（）【】《》"

    static func at(_ text: String, _ start: String.Index, preceding: Character? = nil) -> Match? {
        guard text[start...].hasPrefix("/apps/") else { return nil }
        if start > text.startIndex {
            let previous = text[text.index(before: start)]
            guard previous.isWhitespace || boundary.contains(previous) else { return nil }
        } else if let preceding, !preceding.isWhitespace, !boundary.contains(preceding) {
            return nil
        }
        var end = start
        while end < text.endIndex {
            let character = text[end]
            if character.isWhitespace || terminators.contains(character) { break }
            end = text.index(after: end)
        }
        var candidate = String(text[start..<end])
        while let last = candidate.last, ".,;!?:".contains(last) { candidate.removeLast() }
        while let last = candidate.last, last == ")" || last == "]" {
            let open: Character = last == ")" ? "(" : "["
            if candidate.filter({ $0 == last }).count <= candidate.filter({ $0 == open }).count { break }
            candidate.removeLast()
        }
        guard isAppPathShape(webPathOnly(candidate)) else { return nil }
        let upper = text.index(start, offsetBy: candidate.count)
        return Match(href: candidate, range: start..<upper)
    }
}
