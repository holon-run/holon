package run.holon.android.app

import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.ExperimentalTextApi
import androidx.compose.ui.text.LinkAnnotation
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertIs
import kotlin.test.assertTrue
import run.holon.android.sdk.HolonFileReference

@OptIn(ExperimentalTextApi::class)
class MarkdownWebRenderingTest {
    @Test
    fun explicitReferenceAndBareAppLinksOnlyInvokeWebCallbackAfterClick() {
        val document = extractMarkdownLinkReferences(
            "[app](/apps/a/b/) [ref][demo] /apps/other/offline/试听%20(1).mp3?q=x#part。\n\n[demo]: /apps/a/b/nested?x=1#part",
        )
        val web = mutableListOf<String>()
        val files = mutableListOf<MessageFileReference>()
        val rendered = inlineMarkdown(document.text, Color.Blue, Color.Gray, files::add, web::add, document.destinations)
        assertTrue(web.isEmpty())
        assertTrue(files.isEmpty())
        val links = rendered.links()
        assertEquals(3, links.size)
        links.forEach { click(it) }
        assertEquals(
            listOf("/apps/a/b/", "/apps/a/b/nested?x=1#part", "/apps/other/offline/试听%20(1).mp3?q=x#part"),
            web,
        )
        assertTrue(files.isEmpty())
    }

    @Test
    fun absentWebCallbackNeverConvertsRootOrNetworkLinksIntoFilesOrActionView() {
        val files = mutableListOf<MessageFileReference>()
        val rendered = inlineMarkdown("[app](/apps/a/b) [gui](/settings) [network](//runtime.test/apps/a/b) /apps/a/b", Color.Blue, Color.Gray, files::add)
        assertTrue(rendered.links().isEmpty())
        assertTrue(files.isEmpty())
    }

    @Test
    fun mailStaysSystemHandledWhileAllValidWebDomainsReachCallback() {
        val web = mutableListOf<String>()
        val rendered = inlineMarkdown(
            "[external](https://unlisted.test/page) [network](//unlisted.test/page) [mail](mailto:user@example.org)",
            Color.Blue, Color.Gray, null, web::add,
        )
        val links = rendered.links()
        click(links[0])
        click(links[1])
        assertEquals(listOf("https://unlisted.test/page", "//unlisted.test/page"), web)
        assertEquals("mailto:user@example.org", assertIs<LinkAnnotation.Url>(links[2]).url)
    }

    @Test
    fun appCodeAndRejectedLinksCannotBecomeFileLinks() {
        val files = mutableListOf<MessageFileReference>()
        val web = mutableListOf<String>()
        val rendered = inlineMarkdown(
            "`/apps/a/b` ``/apps/a/b/试听`` [bad](/apps/a/b/%252e%252e/x) /apps/a/b/%2fsecret [script](javascript:alert(1))",
            Color.Blue, Color.Gray, files::add, web::add,
        )
        assertTrue(rendered.links().isEmpty())
        assertTrue(files.isEmpty())
        assertTrue(web.isEmpty())
        assertEquals(
            listOf(MarkdownBlock.Code("md", "/apps/a/b\n[hidden]: /apps/a/b")),
            parseMarkdown("```md\n/apps/a/b\n[hidden]: /apps/a/b\n```"),
        )
        assertEquals(
            listOf(MarkdownBlock.Code("md", "```\n/apps/a/b\n[hidden]: /apps/a/b")),
            parseMarkdown("````md\n```\n/apps/a/b\n[hidden]: /apps/a/b\n````"),
        )
    }

    @Test
    fun explicitFileAndWorkspaceLinksRetainFileCallbacksAndAnchorDoesNotGuessOrigin() {
        val files = mutableListOf<MessageFileReference>()
        val web = mutableListOf<String>()
        val rendered = inlineMarkdown(
            "[file](file:///apps/a/b) [workspace](workspace://ws/report.md?root=old%3Aroot) [host](/tmp/report.md) [relative](report.md) [anchor](#part)",
            Color.Blue, Color.Gray, files::add, web::add,
        )
        rendered.links().forEach { click(it) }
        assertEquals(
            listOf(
                MessageFileReference(HolonFileReference.AbsolutePath("/apps/a/b")),
                MessageFileReference(HolonFileReference.WorkspaceUri("workspace://ws/report.md?root=old%3Aroot")),
                MessageFileReference(HolonFileReference.AbsolutePath("/tmp/report.md")),
            ),
            files,
        )
        assertTrue(web.isEmpty())
    }

    private fun AnnotatedString.links(): List<LinkAnnotation> =
        getLinkAnnotations(0, length).map { it.item }

    private fun click(link: LinkAnnotation) {
        val clickable = assertIs<LinkAnnotation.Clickable>(link)
        requireNotNull(clickable.linkInteractionListener).onClick(clickable)
    }
}
