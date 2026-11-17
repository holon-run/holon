package run.holon.android.app

import java.io.File
import java.nio.ByteBuffer
import kotlin.test.Test
import kotlin.test.assertContentEquals
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue
import org.junit.Rule
import org.junit.rules.TemporaryFolder
import run.holon.android.sdk.HolonAvatarExpression

class AgentAvatarDiskCacheTest {
    @get:Rule val temporary = TemporaryFolder()
    private val root get() = File(temporary.root, "avatars")
    private val key = AgentAvatarKey("agent/../你好", HolonAvatarExpression.Neutral, 64)
    private fun png(pixels: Int = 64) =
        ByteBuffer.allocate(33).put(byteArrayOf(0x89.toByte(), 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a))
            .putInt(13).putInt(0x49484452).putInt(pixels).putInt(pixels).array()

    @Test fun `persistent drawing bytes survive cache recreation but scopes and variants never mix`() {
        val cache = AgentAvatarDiskCache(root)
        val scope = cache.scope("server-A|user-A") { true }
        scope.write(key, png())
        assertContentEquals(png(), AgentAvatarDiskCache(root).scope("server-A|user-A") { true }.read(key))
        assertNull(cache.scope("server-B|user-A") { true }.read(key))
        assertNull(cache.scope("server-A|user-B") { true }.read(key))
        assertNull(scope.read(key.copy(agentId = "other")))
        assertNull(scope.read(key.copy(expression = HolonAvatarExpression.Focused)))
        assertNull(scope.read(key.copy(pixels = 128)))
        val files = root.walkTopDown().filter { it.isFile }.toList()
        assertEquals(1, files.size)
        assertTrue(files.single().relativeTo(root).path.matches(Regex("[0-9a-f]{64}/[0-9a-f]{64}\\.png")))
    }

    @Test fun `clearing one scope fences old handles without erasing another identity`() {
        val cache = AgentAvatarDiskCache(root)
        val old = cache.scope("A") { true }
        val other = cache.scope("B") { true }
        old.write(key, png())
        other.write(key, png())
        cache.clear("A")
        val replacement = cache.scope("A") { true }
        old.write(key, png())
        assertNull(old.read(key))
        assertNull(replacement.read(key))
        assertNotNull(other.read(key))
        replacement.write(key, png())
        old.clear()
        old.remove(key)
        assertNotNull(replacement.read(key))
    }

    @Test fun `retired authentication cannot read write remove or clear a replacement lease`() {
        val cache = AgentAvatarDiskCache(root)
        var current = true
        val old = cache.scope("A") { current }
        old.write(key, png())
        current = false
        val replacement = cache.scope("A") { true }
        old.remove(key)
        old.clear()
        assertNull(old.read(key))
        assertNotNull(replacement.read(key))
        old.write(key.copy(agentId = "late"), png())
        assertNull(replacement.read(key.copy(agentId = "late")))
    }

    @Test fun `LRU bounds bytes and entries globally and disk hits update recency`() {
        for ((entries, bytes) in listOf(2 to 1024L, 512 to 66L)) {
            var now = 1_000L
            val cache = AgentAvatarDiskCache(root, maxEntries = entries, maxBytes = bytes, nowMillis = { now++ })
            val first = cache.scope("A") { true }
            val second = cache.scope("B") { true }
            first.write(key, png())
            second.write(key, png())
            assertNotNull(first.read(key))
            second.write(key.copy(agentId = "C"), png())
            assertNull(second.read(key))
            assertNotNull(first.read(key))
            assertNotNull(second.read(key.copy(agentId = "C")))
            assertEquals(2, root.walkTopDown().count { it.isFile })
            root.deleteRecursively()
        }
    }

    @Test fun `invalid oversized and wrong dimension files are removed before decoding`() {
        val scope = AgentAvatarDiskCache(root).scope("A") { true }
        for (bad in listOf(ByteArray(33), png(128), png().copyOf(2 * 1024 * 1024 + 1), png().copyOf(20))) {
            scope.write(key, png())
            val file = root.walkTopDown().single { it.isFile }
            file.writeBytes(bad)
            assertNull(scope.read(key))
            assertFalse(file.exists())
        }
        scope.write(key, png(128))
        assertEquals(0, root.walkTopDown().count { it.isFile })
    }

    @Test fun `disk errors and an image larger than the budget never break the caller`() {
        root.writeText("not a directory")
        val scope = AgentAvatarDiskCache(root).scope("A") { true }
        assertNull(scope.read(key))
        scope.write(key, png())
        scope.remove(key)
        scope.clear()
        root.delete()
        val tiny = AgentAvatarDiskCache(root, maxBytes = 32).scope("A") { true }
        tiny.write(key, png())
        assertNull(tiny.read(key))
        assertFalse(root.exists())
    }
}
