import XCTest
@testable import Holon

@MainActor
final class MarkdownWebLinkTests: XCTestCase {
    func testPreservesWebVectorsWithoutDiscoveryOrDomainWhitelists() {
        let vectors = [
            "/apps/tuptup-agent-dev/voice-audition-20261019/",
            "/apps/other/not-discovered",
            "/apps/a/b/assets/试听%20(1).mp3?mode=demo%26x#中文",
            "/apps/a/b/?next=https://example.org/#part",
            "/agents/a/conversation?event_seq=1#part",
            "/files?workspace=ws&root=opaque%3Aone&path=a%23b",
            "/", "/skills", "/search", "/agents/a", "/skills/a", "/agents/a/skills/b",
            "/templates", "/templates/b", "/settings/",
            "https://example.org/apps/a/b?query#part", "http://elsewhere.test",
            "//example.org/apps/a/b?query#part", "HTTPS://EXAMPLE.org/报告%20one.html",
        ]
        for href in vectors {
            XCTAssertEqual(MarkdownWebLink.classify(href), .web(href), href)
        }
        XCTAssertEqual(MarkdownWebLink.classify("mailto:user@example.org"), .mail("mailto:user@example.org"))
    }

    func testLeavesExplicitFilesAndUnknownPathsToExistingFileContext() {
        for href in ["/apps", "/apps/a", "/apps-evil/a/b", "/unknown", "/tmp/file.md", "/home/a.txt",
                     "/settings/private", "/agents/a/unknown", "file:///apps/a/b",
                     "workspace://ws/apps/a/b?root=opaque", "relative.html", "../notes.md", "#section", "?query"] {
            XCTAssertNil(MarkdownWebLink.classify(href), href)
        }
    }

    func testRejectsConfusedUrlsBeforeNormalization() {
        for href in ["/apps/a/b/../c", "/apps/a/b/./c", "/apps/a/b/%2e%2e/c", "/apps/a/b/%252e%252e/c",
                     "/apps/a%2fb/c", "/apps/a/b/%5cfile", "/apps/a/b/%255cfile", "/apps/a/b/%25252f",
                     "/apps/a/b/%00", "/apps/a/b/%0a", "/apps/a/b/%7f", "/apps/a/b/%xx",
                     "/apps/a/b/%", "/apps/a/b/%C0%AF", "/apps/a/b/%FF", "/apps/a/b/%E2%82",
                     "/apps/a/b\\evil", "/apps/a/b/\nfile", "/apps/a/b/\tfile",
                     "///evil.test/apps/a/b", "//evil.test\\@holon.invalid/apps/a/b",
                     "https:evil.test", "http:\\\\evil.test", "https://example.org/\npath",
                     "https://user:password@example.org/apps/a/b", "//user@example.org/apps/a/b",
                     "https://example.org:65536/apps/a/b", "https://example.org:/apps/a/b",
                     "https://example.org/apps/a/b/%252f", "/apps/a/b?query=%FF", "/apps/a/b#%0a"] {
            XCTAssertEqual(MarkdownWebLink.classify(href), .rejected, href)
        }
        for href in ["javascript:alert(1)", "data:text/html,hello", "ftp://host", "tel:123"] {
            XCTAssertNil(MarkdownWebLink.classify(href), href)
            XCTAssertNil(MarkdownWebLink.resolveRuntimeWebLink(href, runtimeBaseURL: "https://runtime.test"), href)
        }
    }

    func testResolvesAgainstExactOriginNotApiPrefixAndPreservesEscapes() {
        let href = "/apps/a/b/nested/试听%20(1).mp3?q=a%26b#part"
        XCTAssertEqual(MarkdownWebLink.resolveRuntimeWebLink(href, runtimeBaseURL: "https://runtime.test:8443/prefix/api"),
                       "https://runtime.test:8443" + href)
        XCTAssertEqual(MarkdownWebLink.resolveRuntimeWebLink("//runtime.test/apps/a/b", runtimeBaseURL: "https://runtime.test/api"),
                       "https://runtime.test/apps/a/b")
        XCTAssertEqual(MarkdownWebLink.resolveRuntimeWebLink("//runtime.test/apps/a/b", runtimeBaseURL: "http://runtime.test/api"),
                       "http://runtime.test/apps/a/b")
        XCTAssertEqual(MarkdownWebLink.resolveRuntimeWebLink("/apps/a/b", runtimeBaseURL: "https://[::1]:8443/api"),
                       "https://[::1]:8443/apps/a/b")
        XCTAssertEqual(MarkdownWebLink.resolveRuntimeWebLink("https://RUNTIME.test:443/apps/a/b", runtimeBaseURL: "https://runtime.test/api"),
                       "https://RUNTIME.test:443/apps/a/b")
        XCTAssertEqual(MarkdownWebLink.resolveRuntimeWebLink("/settings?tab=one#part", runtimeBaseURL: "https://runtime.test/api"),
                       "https://runtime.test/settings?tab=one#part")
    }

    func testRefusesOtherOriginsDowngradesFilesAndInvalidBaseUrls() {
        for href in ["https://elsewhere.test/apps/a/b", "https://runtime.test:8443/apps/a/b",
                     "http://runtime.test/apps/a/b", "//runtime.test.evil/apps/a/b", "//elsewhere.test/apps/a/b",
                     "file:///apps/a/b", "/tmp/notes.md", "#anchor", "mailto:user@example.org",
                     "/apps/a/b/%2e%2e/secret", "https://runtime.test/apps/a/b/../secret",
                     "https://user@runtime.test/apps/a/b"] {
            XCTAssertNil(MarkdownWebLink.resolveRuntimeWebLink(href, runtimeBaseURL: "https://runtime.test/api"), href)
        }
        for base in ["https://user@runtime.test/api", "file:///runtime", "https://runtime.test\\evil", "https://runtime.test:"] {
            XCTAssertNil(MarkdownWebLink.resolveRuntimeWebLink("/apps/a/b", runtimeBaseURL: base), base)
        }
    }

    func testBarePathsKeepBalancedPunctuationAndDoNotMatchEmbeddedHostPaths() {
        let source = "(/apps/a/b/clip(1).mp3?q=x#part)。 /apps/a/b。 /apps/a/b!"
        let first = source.range(of: "/apps")!
        let expected = "/apps/a/b/clip(1).mp3?q=x#part"
        let match = BareAppLinks.at(source, first.lowerBound)
        XCTAssertEqual(match?.href, expected)
        XCTAssertEqual(match?.range.upperBound, source.index(first.lowerBound, offsetBy: expected.count))
        for embedded in ["prefix/apps/a/b", "/tmp/apps/a/b", "file:///apps/a/b", "https://host/apps/a/b"] {
            XCTAssertNil(BareAppLinks.at(embedded, embedded.range(of: "/apps")!.lowerBound), embedded)
        }
        XCTAssertNil(BareAppLinks.at("/apps/a", "/apps/a".startIndex))
        XCTAssertNil(BareAppLinks.at("/apps-evil/a/b", "/apps-evil/a/b".startIndex))
        let rejected = BareAppLinks.at("/apps/a/b/%2e%2e/c", "/apps/a/b/%2e%2e/c".startIndex)
        XCTAssertEqual(MarkdownWebLink.classify(rejected?.href ?? ""), .rejected)
    }

    func testMarkdownAppLinksRouteToRuntimeWebMarkers() throws {
        let content = try HolonMarkdownParser().attributedString(for:
            "[Demo](/apps/tuptup-agent-dev/voice-audition-20261019/) [GUI](/settings) [Agent](/agents/a/conversation?event_seq=1#part) [Net](//example.org/apps/a/b?query#part) [File](/tmp/report.md)")
        XCTAssertEqual(content.runs.compactMap(\.link).map(RichTextLink.classify), [
            .runtimeWeb("/apps/tuptup-agent-dev/voice-audition-20261019/"),
            .runtimeWeb("/settings"),
            .runtimeWeb("/agents/a/conversation?event_seq=1#part"),
            .runtimeWeb("//example.org/apps/a/b?query#part"),
            .reference("/tmp/report.md"),
        ])
    }

    func testReferenceStyleAndBareAppLinksBindToRuntimeWeb() throws {
        let source = "[listen][demo] and [collapsed][] plus bare /apps/a/b/clip(1).mp3?q=x#part.\n\n[demo]: /apps/a/b/demo%20one.mp3\n[collapsed]: /skills"
        let content = try HolonMarkdownParser().attributedString(for: source)
        XCTAssertEqual(content.runs.compactMap(\.link).map(RichTextLink.classify), [
            .runtimeWeb("/apps/a/b/demo%20one.mp3"),
            .runtimeWeb("/skills"),
            .runtimeWeb("/apps/a/b/clip(1).mp3?q=x#part"),
        ])
    }

    func testRejectedAndAppShapedLinksNeverFallThroughToHostFiles() throws {
        let content = try HolonMarkdownParser().attributedString(for:
            "Bad [traversal](/apps/a/b/%2e%2e/c) and `/apps/a/b/code%20ref` then bare /apps/a/b/%2e%2e/x")
        XCTAssertEqual(content.runs.compactMap(\.link).map(RichTextLink.classify), [.unsupported, .unsupported])
        XCTAssertFalse(content.runs.contains { if case .runtimeWeb = RichTextLink.classify($0.link ?? URL(string: "x")!) { return true }; return false })
    }

    func testAppImagesAndCodeBlocksKeepTheirOwnSemantics() throws {
        let content = try HolonMarkdownParser().attributedString(for:
            "![Web](/apps/a/b/icon.png) and\n\n```\n/apps/a/b/do-not-link\n```\n\nInline `/tmp/a%20b.md` stays a file.")
        XCTAssertTrue(content.runs.allSatisfy { $0.imageURL == nil })
        XCTAssertEqual(content.runs.compactMap(\.link).map(RichTextLink.classify), [
            .runtimeWeb("/apps/a/b/icon.png"),
            .reference("/tmp/a%20b.md"),
        ])
    }

    func testMarkerRoundTripPreservesQueryFragmentAndEncoding() {
        let href = "/apps/a/b/试听%20(1).mp3?q=a%26b#part"
        let url = RichTextLink.runtimeWebURL(href)!
        XCTAssertEqual(RichTextLink.classify(url), .runtimeWeb(href))
        XCTAssertEqual(RichTextLink.classify(URL(string: "holon-web:")!), .unsupported)
    }
}
