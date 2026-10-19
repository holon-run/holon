import Foundation
import HolonClient

/// Navigation and cookie policy for the identity-bound runtime browser,
/// mirroring Android `RuntimeBrowserPolicy` (#3608). Pure logic only: the
/// unit-test host cannot run WebKit networking (probe evidence, 2026-10-19),
/// so every WKWebView-facing decision routes through this file.

/// Android `RuntimeBrowserNavigation`: same-origin page, safe external site, or never loads.
enum RuntimeBrowserNavigation: Equatable {
    case internalWeb, externalWeb, blocked
}

/// The provider treats every href as untrusted input and re-validates at open
/// time (Stage 1 audit contract: a `holon-web:` marker can reach the opener via
/// an explicit markdown scheme link). Only these outcomes may load or leave the app.
enum RuntimeBrowserLinkTarget: Equatable {
    case load(String)
    case external(String)
    case unsafe
}

/// Snapshot of the online authority a browser page may bind to. Never carries
/// the credential itself: only the cookie binding path reads it from the store.
struct RuntimeBrowserLease: Equatable {
    let profile: ConnectionProfile
    let identity: HolonConnectionIdentity

    var baseURL: String { profile.apiBaseURL.absoluteString }
}

enum RuntimeBrowserCookieFailure: Error, Equatable {
    case leaseChanged
    case foreignOrigin
    case credentialMissing
    case invalidCredential
    case storageError
}

enum RuntimeBrowserPolicy {
    /// Accepts only clean absolute http(s) URLs: no control characters,
    /// backslashes, userinfo, or empty hosts (mirrors Android `safeBrowserHttpUrl`).
    static func safeBrowserHTTPURL(_ url: String) -> URL? {
        guard url.utf8.count <= 16_384,
              !url.unicodeScalars.contains(where: { $0.value <= 0x20 || $0.value == 0x7f || $0.value == 0x5c }),
              let parts = URLComponents(string: url),
              parts.user == nil, parts.password == nil,
              let scheme = parts.scheme?.lowercased(), scheme == "http" || scheme == "https",
              let host = parts.host, !host.isEmpty,
              let normalized = parts.url else { return nil }
        return normalized
    }

    /// Compare origins, not cookie domains: cookies do not isolate ports.
    static func sameRuntimeOrigin(_ url: String, base: String) -> Bool {
        guard let target = origin(of: url), let runtime = origin(of: base) else { return false }
        return target == runtime
    }

    static func navigation(_ url: String, base: String) -> RuntimeBrowserNavigation {
        guard safeBrowserHTTPURL(url) != nil else { return .blocked }
        return sameRuntimeOrigin(url, base: base) ? .internalWeb : .externalWeb
    }

    /// Mirrors the Android repository gate: only printable ASCII without cookie delimiters.
    static func isValidSessionCredential(_ credential: String) -> Bool {
        guard !credential.isEmpty else { return false }
        return credential.unicodeScalars.allSatisfy { scalar in
            (0x21...0x7e).contains(scalar.value)
                && scalar.value != 0x22 /* " */ && scalar.value != 0x2c /* , */
                && scalar.value != 0x3b /* ; */ && scalar.value != 0x5c /* \ */
        }
    }

    /// The HttpOnly session cookie contract approved for #3598/#3603: never a
    /// token in a URL, script, localStorage, or a custom request header.
    static func runtimeSessionCookieHeader(credential: String, url: String) -> String {
        var header = "holon_session=\(credential); Path=/; HttpOnly; SameSite=Lax"
        if origin(of: url)?.scheme == "https" { header += "; Secure" }
        return header
    }

    /// `HTTPCookie.cookies(withResponseHeaderFields:)` preserves HttpOnly and
    /// SameSite on this engine (macOS WebKit probe, 2026-10-19), so the binding
    /// cookie is parsed exactly the way the runtime would set it.
    static func sessionCookie(header: String, url: String) -> HTTPCookie? {
        guard let target = URL(string: url) else { return nil }
        return HTTPCookie.cookies(withResponseHeaderFields: ["Set-Cookie": header], for: target).first
    }

    /// Provider-side re-validation (Stage 1 audit contract). Classification and
    /// runtime resolution run again on the raw href before anything loads; a
    /// resolved runtime link must be same-origin, an external site only reaches
    /// the confirm dialog, and everything else is refused.
    static func linkTarget(href: String, runtimeBaseURL: String) -> RuntimeBrowserLinkTarget {
        guard case .web(let url) = MarkdownWebLink.classify(href) else { return .unsafe }
        if let resolved = MarkdownWebLink.resolveRuntimeWebLink(href, runtimeBaseURL: runtimeBaseURL),
           safeBrowserHTTPURL(resolved) != nil {
            return .load(resolved)
        }
        if let safe = safeBrowserHTTPURL(url) {
            return .external(safe.absoluteString)
        }
        // A protocol-relative target inherits the runtime scheme, like the Android host.
        if href.hasPrefix("//"), let scheme = origin(of: runtimeBaseURL)?.scheme,
           let safe = safeBrowserHTTPURL("\(scheme):\(href)") {
            return .external(safe.absoluteString)
        }
        return .unsafe
    }

    private static func origin(of url: String) -> (scheme: String, host: String, port: Int)? {
        guard let parts = URLComponents(string: url),
              parts.user == nil, parts.password == nil,
              let scheme = parts.scheme?.lowercased(), scheme == "http" || scheme == "https",
              let host = parts.host?.lowercased(), !host.isEmpty else { return nil }
        return (scheme, host, parts.port ?? (scheme == "https" ? 443 : 80))
    }
}
