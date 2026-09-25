package run.holon.android.app

import kotlin.test.Test
import kotlin.test.assertEquals

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
}
