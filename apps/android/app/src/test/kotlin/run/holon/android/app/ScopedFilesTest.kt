package run.holon.android.app

import java.nio.file.Files
import kotlin.test.assertFalse
import kotlin.test.assertTrue
import org.junit.Test

class ScopedFilesTest {
    @Test fun `scope removal preserves shared references and files outside staging root`() {
        val directory = Files.createTempDirectory("holon-scope-test").toFile()
        try {
            val root = directory.resolve("outbox").apply { mkdirs() }
            val a = root.resolve("a").apply { writeText("a") }
            val shared = root.resolve("shared").apply { writeText("shared") }
            val b = root.resolve("b").apply { writeText("b") }
            val outside = directory.resolve("outside").apply { writeText("outside") }
            ScopedFiles(root).removeUnreferenced(
                setOf(a.path, shared.path, outside.path, root.path), setOf(shared.path, b.path),
            )
            assertFalse(a.exists())
            assertTrue(shared.exists())
            assertTrue(b.exists())
            assertTrue(outside.exists())
            assertTrue(root.isDirectory)
        } finally { directory.deleteRecursively() }
    }
}
