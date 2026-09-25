package run.holon.android.app

import androidx.compose.ui.graphics.Color
import java.io.File
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertTrue

class FileReaderTest {
    @Test
    fun `indexed reader returns complete unicode text without overlap`() {
        val content = "标题\n" + "源码🙂和中文\n".repeat(3_000) + "最后一行"
        val file = temporaryFile(content)
        try {
            val reader = IndexedTextFile(file, pageBytes = 17)
            assertTrue(content.length > 20_000)
            assertEquals(content, reader.pages.indices.joinToString("") { reader.readPage(it) })
            assertTrue(reader.pages.size > 2)
            assertEquals(0L, reader.pages.first().start)
            assertEquals(file.length(), reader.pages.last().endExclusive)
        } finally {
            file.delete()
        }
    }

    @Test
    fun `indexed reader handles long lines and empty files`() {
        val content = "x".repeat(101) + "\n" + "z".repeat(13)
        val file = temporaryFile(content)
        try {
            val reader = IndexedTextFile(file, pageBytes = 11)
            assertEquals(content, reader.pages.indices.joinToString("") { reader.readPage(it) })
            file.writeText("")
            assertTrue(IndexedTextFile(file, pageBytes = 11).pages.isEmpty())
        } finally {
            file.delete()
        }
    }

    @Test
    fun `text classification includes code MIME types without treating arbitrary binary as text`() {
        assertTrue(isReadableTextFile("application/x-toml", "config.toml"))
        assertTrue(isReadableTextFile("application/javascript", "app.js"))
        assertTrue(isReadableTextFile("application/octet-stream", "lib.rs"))
        assertFalse(isReadableTextFile("application/octet-stream", "archive.zip"))
        assertEquals("rust", codeLanguage("lib.rs"))
        assertEquals("kotlin", codeLanguage("MainActivity.KT"))
    }

    @Test
    fun `highlighter keeps every source character`() {
        val source = "fn main() { let value = 42; // 你好\nprintln!(\"ok\"); }"
        val highlighted = CodeHighlighter.highlight(source, "rust", Color.Blue, Color.Gray, Color.Black)
        assertEquals(source, highlighted.text)
        assertTrue(highlighted.spanStyles.isNotEmpty())
    }

    private fun temporaryFile(content: String): File =
        File.createTempFile("holon-reader-", ".txt").apply { writeText(content) }
}
