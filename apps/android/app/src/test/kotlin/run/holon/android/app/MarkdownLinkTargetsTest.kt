package run.holon.android.app

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertIs
import kotlin.test.assertNull

class MarkdownLinkTargetsTest {
    @Test
    fun preservesWebVectorsWithoutDiscoveryOrDomainWhitelists() {
        listOf(
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
        ).forEach { href ->
            assertEquals(MarkdownWebLink.Web(href), classifyMarkdownWebLink(href), href)
        }
        assertEquals(MarkdownWebLink.Mail("mailto:user@example.org"), classifyMarkdownWebLink("mailto:user@example.org"))
    }

    @Test
    fun leavesExplicitFilesAndUnknownPathsToExistingFileContext() {
        listOf(
            "/apps", "/apps/a", "/apps-evil/a/b", "/unknown", "/tmp/file.md", "/home/a.txt",
            "/settings/private", "/agents/a/unknown", "file:///apps/a/b",
            "workspace://ws/apps/a/b?root=opaque", "relative.html", "../notes.md", "#section", "?query",
        ).forEach { assertNull(classifyMarkdownWebLink(it), it) }
    }

    @Test
    fun rejectsConfusedUrlsBeforeNormalization() {
        listOf(
            "/apps/a/b/../c", "/apps/a/b/./c", "/apps/a/b/%2e%2e/c", "/apps/a/b/%252e%252e/c",
            "/apps/a%2fb/c", "/apps/a/b/%5cfile", "/apps/a/b/%255cfile", "/apps/a/b/%25252f",
            "/apps/a/b/%00", "/apps/a/b/%0a", "/apps/a/b/%7f", "/apps/a/b/%xx",
            "/apps/a/b/%", "/apps/a/b/%C0%AF", "/apps/a/b/%FF", "/apps/a/b/%E2%82",
            "/apps/a/b\\evil", "/apps/a/b/\nfile", "/apps/a/b/\tfile",
            "///evil.test/apps/a/b", "//evil.test\\@holon.invalid/apps/a/b",
            "https:evil.test", "http:\\\\evil.test", "https://example.org/\npath",
            "https://user:password@example.org/apps/a/b", "//user@example.org/apps/a/b",
            "https://example.org:65536/apps/a/b", "https://example.org:/apps/a/b",
            "https://example.org/apps/a/b/%252f", "/apps/a/b?query=%FF", "/apps/a/b#%0a",
        ).forEach { assertEquals(MarkdownWebLink.Rejected, classifyMarkdownWebLink(it), it) }
        listOf("javascript:alert(1)", "data:text/html,hello", "ftp://host", "tel:123").forEach {
            assertNull(classifyMarkdownWebLink(it))
            assertNull(resolveRuntimeWebLink(it, "https://runtime.test"))
        }
    }

    @Test
    fun resolvesAgainstExactOriginNotApiPrefixAndPreservesEscapes() {
        val href = "/apps/a/b/nested/试听%20(1).mp3?q=a%26b#part"
        assertEquals("https://runtime.test:8443$href", resolveRuntimeWebLink(href, "https://runtime.test:8443/prefix/api"))
        assertEquals("https://runtime.test/apps/a/b", resolveRuntimeWebLink("//runtime.test/apps/a/b", "https://runtime.test/api"))
        assertEquals("http://runtime.test/apps/a/b", resolveRuntimeWebLink("//runtime.test/apps/a/b", "http://runtime.test/api"))
        assertEquals("https://[::1]:8443/apps/a/b", resolveRuntimeWebLink("/apps/a/b", "https://[::1]:8443/api"))
        assertEquals("https://RUNTIME.test:443/apps/a/b", resolveRuntimeWebLink("https://RUNTIME.test:443/apps/a/b", "https://runtime.test/api"))
        assertEquals("https://runtime.test/settings?tab=one#part", resolveRuntimeWebLink("/settings?tab=one#part", "https://runtime.test/api"))
    }

    @Test
    fun refusesOtherOriginsDowngradesFilesAndInvalidBaseUrls() {
        listOf(
            "https://elsewhere.test/apps/a/b", "https://runtime.test:8443/apps/a/b",
            "http://runtime.test/apps/a/b", "//runtime.test.evil/apps/a/b", "//elsewhere.test/apps/a/b",
            "file:///apps/a/b", "/tmp/notes.md", "#anchor", "mailto:user@example.org",
            "/apps/a/b/%2e%2e/secret", "https://runtime.test/apps/a/b/../secret",
            "https://user@runtime.test/apps/a/b",
        ).forEach { assertNull(resolveRuntimeWebLink(it, "https://runtime.test/api"), it) }
        listOf("https://user@runtime.test/api", "file:///runtime", "https://runtime.test\\evil", "https://runtime.test:").forEach {
            assertNull(resolveRuntimeWebLink("/apps/a/b", it), it)
        }
    }

    @Test
    fun barePathsKeepBalancedPunctuationAndDoNotMatchEmbeddedHostPaths() {
        val source = "(/apps/a/b/clip(1).mp3?q=x#part)。 /apps/a/b。 /apps/a/b!"
        val first = source.indexOf("/apps")
        val expected = "/apps/a/b/clip(1).mp3?q=x#part"
        assertEquals(BareMarkdownAppLink(expected, first + expected.length), bareMarkdownAppLinkAt(source, first))
        listOf("prefix/apps/a/b", "/tmp/apps/a/b", "file:///apps/a/b", "https://host/apps/a/b").forEach {
            assertNull(bareMarkdownAppLinkAt(it, it.indexOf("/apps")), it)
        }
        listOf("/apps/a", "/apps-evil/a/b").forEach { assertNull(bareMarkdownAppLinkAt(it, 0)) }
        val rejected = assertIs<BareMarkdownAppLink>(bareMarkdownAppLinkAt("/apps/a/b/%2e%2e/c", 0))
        assertEquals(MarkdownWebLink.Rejected, classifyMarkdownWebLink(rejected.href))
    }

    @Test
    fun referenceDefinitionsAreCaseInsensitiveFirstWinsAndOutsideFences() {
        val source = """
            [listen][DEMO clip] [demo clip][] [demo clip]

            [demo   clip]: </apps/a/b/试听%20(1).mp3?q=x#part> "Preview"
            [demo clip]: https://ignored.test
            ```md
            [hidden]: /apps/a/code
            ```
        """.trimIndent()
        val document = extractMarkdownLinkReferences(source)
        assertEquals(mapOf("demo clip" to "/apps/a/b/试听%20(1).mp3?q=x#part"), document.destinations)
        assertEquals("/apps/a/b/试听%20(1).mp3?q=x#part", markdownLinkAt(document.text, 0, document.destinations)?.destination)
        val collapsed = document.text.indexOf("[demo clip][]")
        assertEquals(collapsed + "[demo clip][]".length, markdownLinkAt(document.text, collapsed, document.destinations)?.next)
        val shortcut = document.text.indexOf("[demo clip]", collapsed + 1)
        assertEquals(shortcut + "[demo clip]".length, markdownLinkAt(document.text, shortcut, document.destinations)?.next)
    }

    @Test
    fun fenceLengthAndMalformedAngleDestinationsDoNotExposeHiddenLinks() {
        val document = extractMarkdownLinkReferences("````md\n```\n[hidden]: /apps/a/b\n````\n[visible]: /apps/a/c")
        assertEquals(mapOf("visible" to "/apps/a/c"), document.destinations)
        assertEquals("````", markdownFenceAt("````md"))
        assertEquals(false, isClosingMarkdownFence("```", "````"))
        assertEquals(false, isClosingMarkdownFence("````not-a-close", "````"))
        assertEquals("/apps/a/b", markdownLinkDestination("</apps/a/b> \"title\""))
        assertEquals("</apps/a/b>trailing", markdownLinkDestination("</apps/a/b>trailing"))
    }
}
