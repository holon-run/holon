package run.holon.android.app

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertNull

class MarkdownTextTest {
    @Test
    fun parsesBriefMarkdownIntoTypedBlocks() {
        val blocks =
            parseMarkdown(
                """
                # Result

                Summary with **status** and `code`.

                - first
                2. second

                > note

                ```sh
                pwd
                ```
                """.trimIndent(),
            )

        assertEquals(
            listOf(
                MarkdownBlock.Heading(1, "Result"),
                MarkdownBlock.Paragraph("Summary with **status** and `code`."),
                MarkdownBlock.ListItem("•", "first"),
                MarkdownBlock.ListItem("2.", "second"),
                MarkdownBlock.Quote("note"),
                MarkdownBlock.Code("sh", "pwd"),
            ),
            blocks,
        )
    }

    @Test
    fun preservesParagraphLineBreaksAndUnclosedFence() {
        assertEquals(
            listOf(
                MarkdownBlock.Paragraph("line one\nline two"),
                MarkdownBlock.Code(null, "unfinished"),
            ),
            parseMarkdown("line one\nline two\n\n```\nunfinished"),
        )
    }

    @Test
    fun parsesTablesNestedListsAndReadOnlyTasks() {
        val blocks = parseMarkdown(
            """
            | Name | Status |
            | :--- | ---: |
            | Report | Done |

            - [x] Inspect report
              - [ ] Share result
            """.trimIndent(),
        )

        assertEquals(
            listOf(
                MarkdownBlock.Table(listOf("Name", "Status"), listOf(listOf("Report", "Done"))),
                MarkdownBlock.ListItem("•", "Inspect report", checked = true),
                MarkdownBlock.ListItem("•", "Share result", depth = 1, checked = false),
            ),
            blocks,
        )
    }

    @Test
    fun parsesBalancedAndEscapedLinkDestinationsWithoutGuessing() {
        val source = "See [report](https://example.test/a_(b)) and [file](docs/a\\)b.md)"
        assertEquals(
            MarkdownLinkTarget("report", "https://example.test/a_(b)", source.indexOf(" and")),
            markdownLinkAt(source, source.indexOf('[')),
        )
        assertEquals(
            MarkdownLinkTarget("file", "docs/a)b.md", source.length),
            markdownLinkAt(source, source.lastIndexOf('[')),
        )
        assertNull(markdownLinkAt("[broken](path", 0))
    }
}
