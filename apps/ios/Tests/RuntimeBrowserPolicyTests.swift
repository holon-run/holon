import XCTest
@testable import Holon

/// Mirrors Android `RuntimeBrowserPolicyTest` (#3608) plus the Stage 1 audit
/// contract: the provider re-validates every href before a page may load.
final class RuntimeBrowserPolicyTests: XCTestCase {
    private let base = "https://runtime.example.test:8787/api"

    func testSafeBrowserHTTPURLAcceptsOnlyCleanAbsoluteHTTPTargets() {
        XCTAssertNotNil(RuntimeBrowserPolicy.safeBrowserHTTPURL("https://runtime.example.test:8787/apps/a/b"))
        XCTAssertNotNil(RuntimeBrowserPolicy.safeBrowserHTTPURL("http://127.0.0.1:9999/x?y=1#z"))
        XCTAssertNotNil(RuntimeBrowserPolicy.safeBrowserHTTPURL("HTTPS://RUNTIME.EXAMPLE.TEST/"))
        XCTAssertNotNil(RuntimeBrowserPolicy.safeBrowserHTTPURL("https://[::1]:8787/"))
        for rejected in [
            "https://a.test/x y", "https://a.test/x\ny", "https://a.test/\r",
            "https://a.test/\\evil", "https://u:p@a.test/", "https://u@a.test/",
            "javascript:alert(1)", "ftp://a.test/", "mailto:a@b.test",
            "https:///missing-host", "//a.test/", "https://a.test/\u{7f}",
        ] {
            XCTAssertNil(RuntimeBrowserPolicy.safeBrowserHTTPURL(rejected), rejected)
        }
    }

    func testSameRuntimeOriginComparesSchemeHostAndPort() {
        XCTAssertTrue(RuntimeBrowserPolicy.sameRuntimeOrigin("https://runtime.example.test:8787/apps/a/b", base: base))
        XCTAssertTrue(RuntimeBrowserPolicy.sameRuntimeOrigin("https://RUNTIME.EXAMPLE.TEST:8787/", base: base))
        XCTAssertTrue(RuntimeBrowserPolicy.sameRuntimeOrigin("https://runtime.example.test/settings", base: "https://runtime.example.test:443/api"))
        XCTAssertFalse(RuntimeBrowserPolicy.sameRuntimeOrigin("https://runtime.example.test:8788/", base: base))
        XCTAssertFalse(RuntimeBrowserPolicy.sameRuntimeOrigin("https://other.example.test:8787/", base: base))
        XCTAssertFalse(RuntimeBrowserPolicy.sameRuntimeOrigin("http://runtime.example.test:8787/", base: base))
        XCTAssertFalse(RuntimeBrowserPolicy.sameRuntimeOrigin("javascript:alert(1)", base: base))
    }

    func testNavigationClassifiesInternalExternalBlocked() {
        XCTAssertEqual(RuntimeBrowserPolicy.navigation("https://runtime.example.test:8787/apps/a/b", base: base), .internalWeb)
        XCTAssertEqual(RuntimeBrowserPolicy.navigation("https://other.example.test/", base: base), .externalWeb)
        XCTAssertEqual(RuntimeBrowserPolicy.navigation("https://u@a.test/", base: base), .blocked)
        XCTAssertEqual(RuntimeBrowserPolicy.navigation("javascript:alert(1)", base: base), .blocked)
    }

    func testCredentialValidationMirrorsAndroidRepositoryGate() {
        XCTAssertTrue(RuntimeBrowserPolicy.isValidSessionCredential("saved-session"))
        XCTAssertTrue(RuntimeBrowserPolicy.isValidSessionCredential("aZ09-._~!#$%&'()*+=/:<>?@[]^`{|}"))
        for rejected in ["", "a b", "a;b", "a,b", "a\"b", "a\\b", "a\nb", "会话"] {
            XCTAssertFalse(RuntimeBrowserPolicy.isValidSessionCredential(rejected), rejected)
        }
    }

    func testSessionCookieHeaderMatchesApprovedContract() {
        XCTAssertEqual(RuntimeBrowserPolicy.runtimeSessionCookieHeader(credential: "marker", url: "https://runtime.example.test/"),
                       "holon_session=marker; Path=/; HttpOnly; SameSite=Lax; Secure")
        XCTAssertEqual(RuntimeBrowserPolicy.runtimeSessionCookieHeader(credential: "marker", url: "http://127.0.0.1:8787/"),
                       "holon_session=marker; Path=/; HttpOnly; SameSite=Lax")
    }

    func testParsedSessionCookieKeepsHttpOnlyAndSameSite() {
        let https = RuntimeBrowserPolicy.sessionCookie(
            header: RuntimeBrowserPolicy.runtimeSessionCookieHeader(credential: "marker", url: "https://runtime.example.test/"),
            url: "https://runtime.example.test/")
        XCTAssertEqual(https?.name, "holon_session")
        XCTAssertEqual(https?.value, "marker")
        XCTAssertEqual(https?.path, "/")
        XCTAssertEqual(https?.isHTTPOnly, true)
        XCTAssertEqual(https?.isSecure, true)
        XCTAssertEqual(https?.sameSitePolicy?.rawValue.lowercased(), "lax")

        let http = RuntimeBrowserPolicy.sessionCookie(
            header: RuntimeBrowserPolicy.runtimeSessionCookieHeader(credential: "marker", url: "http://127.0.0.1:8787/"),
            url: "http://127.0.0.1:8787/")
        XCTAssertEqual(http?.isSecure, false)
        XCTAssertEqual(http?.sameSitePolicy?.rawValue.lowercased(), "lax")
    }

    func testLinkTargetResolvesRuntimePagesToSameOriginLoads() {
        XCTAssertEqual(RuntimeBrowserPolicy.linkTarget(href: "/apps/alice/wiki", runtimeBaseURL: base),
                       .load("https://runtime.example.test:8787/apps/alice/wiki"))
        XCTAssertEqual(RuntimeBrowserPolicy.linkTarget(href: "/", runtimeBaseURL: base),
                       .load("https://runtime.example.test:8787/"))
        XCTAssertEqual(RuntimeBrowserPolicy.linkTarget(href: "/settings", runtimeBaseURL: base),
                       .load("https://runtime.example.test:8787/settings"))
        XCTAssertEqual(RuntimeBrowserPolicy.linkTarget(href: "//runtime.example.test:8787/apps/a/b", runtimeBaseURL: base),
                       .load("https://runtime.example.test:8787/apps/a/b"))
        XCTAssertEqual(RuntimeBrowserPolicy.linkTarget(href: "https://runtime.example.test:8787/files", runtimeBaseURL: base),
                       .load("https://runtime.example.test:8787/files"))
    }

    func testLinkTargetSendsSafeExternalSitesToConfirmationOnly() {
        XCTAssertEqual(RuntimeBrowserPolicy.linkTarget(href: "https://evil.example.test/path", runtimeBaseURL: base),
                       .external("https://evil.example.test/path"))
        XCTAssertEqual(RuntimeBrowserPolicy.linkTarget(href: "http://127.0.0.1:9999/x", runtimeBaseURL: base),
                       .external("http://127.0.0.1:9999/x"))
        XCTAssertEqual(RuntimeBrowserPolicy.linkTarget(href: "//evil.example.test/p", runtimeBaseURL: base),
                       .external("https://evil.example.test/p"))
        XCTAssertEqual(RuntimeBrowserPolicy.linkTarget(href: "https://runtime.example.test:8788/apps/a/b", runtimeBaseURL: base),
                       .external("https://runtime.example.test:8788/apps/a/b"))
    }

    func testLinkTargetRefusesEverythingElse() {
        for href in [
            "mailto:a@b.test",
            "/not/a/registered/web/path",
            "holon-web:/apps/a/b",
            "https://[::1]evil.com/",
            "/apps/a/b/../../../etc",
            "workspace://a/b",
            "file:///etc/passwd",
            "https://runtime.example.test:8787/apps/a/b\\x",
        ] {
            XCTAssertEqual(RuntimeBrowserPolicy.linkTarget(href: href, runtimeBaseURL: base), .unsafe, href)
        }
    }

    func testLinkTargetNeverRebuildsARuntimeLoadFromAnUnsafeMarker() {
        // The marker scheme bypasses parse-time classification (Stage 1 audit);
        // it must not become a load target even when it names a runtime path.
        XCTAssertEqual(RuntimeBrowserPolicy.linkTarget(href: "holon-web:https://evil.example.test/x", runtimeBaseURL: base), .unsafe)
        XCTAssertEqual(RuntimeBrowserPolicy.linkTarget(href: "holon-web://runtime.example.test:8787/apps/a/b", runtimeBaseURL: base), .unsafe)
    }
}
