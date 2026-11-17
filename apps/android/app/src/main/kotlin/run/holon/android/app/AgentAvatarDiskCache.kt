package run.holon.android.app

import java.io.DataInputStream
import java.io.File
import java.io.IOException
import java.nio.file.Files
import java.nio.file.StandardCopyOption

/** Private drawing bytes only, never a cached access decision or credential. */
internal class AgentAvatarDiskCache(
    private val root: File,
    private val maxEntries: Int = 512,
    private val maxBytes: Long = 16L * 1024 * 1024,
    private val nowMillis: () -> Long = System::currentTimeMillis,
) {
    private val lock = Any()
    private val epochs = mutableMapOf<String, Any>()

    fun scope(scopeKey: String, isCurrent: () -> Boolean): Scope = synchronized(lock) {
        val name = scopeFileKey(scopeKey)
        Scope(name, epochs.getOrPut(name) { Any() }, isCurrent)
    }

    fun clear(scopeKey: String) = synchronized(lock) {
        clearScope(scopeFileKey(scopeKey))
    }

    private fun clearScope(name: String) {
        // Retired handles cannot repopulate a scope after logout, even before UI disposal.
        epochs.remove(name)
        bestEffort { File(root, name).deleteRecursively() }
    }

    inner class Scope internal constructor(
        private val name: String,
        private val epoch: Any,
        private val isCurrent: () -> Boolean,
    ) {
        private fun active(): Boolean = epochs[name] === epoch && isCurrent()

        private fun file(key: AgentAvatarKey): File {
            // This is the fixed SDK request version, not a moving renderer default.
            val resource = "light-mech-1-vivid-2|png|${key.agentId}|${key.expression.wireValue}|${key.pixels}"
            return File(File(root, name), "${scopeFileKey(resource)}.png")
        }

        fun read(key: AgentAvatarKey): ByteArray? = synchronized(lock) {
            if (!active()) return null
            bestEffort {
                val file = file(key)
                val length = file.length()
                if (length !in 33..(2L * 1024 * 1024)) {
                    file.delete()
                    return@bestEffort null
                }
                val bytes = DataInputStream(file.inputStream()).use { input ->
                    ByteArray(length.toInt()).also {
                        input.readFully(it)
                        if (input.read() != -1) return@bestEffort null
                    }
                }
                if (!validAvatarPng(bytes, key.pixels)) {
                    file.delete()
                    return@bestEffort null
                }
                file.setLastModified(nowMillis())
                bytes.takeIf { active() }
            }
        }

        fun write(key: AgentAvatarKey, bytes: ByteArray) = synchronized(lock) {
            if (!active() || !validAvatarPng(bytes, key.pixels) || bytes.size > maxBytes) return
            bestEffort {
                val target = file(key)
                checkDirectory(target.parentFile!!)
                val partial = File.createTempFile("avatar-", ".tmp", target.parentFile)
                try {
                    partial.writeBytes(bytes)
                    if (!active()) return@bestEffort
                    Files.move(partial.toPath(), target.toPath(),
                        StandardCopyOption.ATOMIC_MOVE, StandardCopyOption.REPLACE_EXISTING)
                    target.setLastModified(nowMillis())
                    trim()
                } finally {
                    partial.delete()
                }
            }
            Unit
        }

        fun remove(key: AgentAvatarKey) = synchronized(lock) {
            if (active()) bestEffort { file(key).delete() }
            Unit
        }

        fun clear() = synchronized(lock) {
            if (active()) clearScope(name)
        }
    }

    private fun checkDirectory(directory: File) {
        if (!directory.isDirectory && !directory.mkdirs()) throw IOException("Avatar cache unavailable")
    }

    private fun trim() {
        val files = root.listFiles().orEmpty().flatMap { it.listFiles().orEmpty().toList() }
            .filter { it.isFile }.sortedBy { it.lastModified() }
        var bytes = files.sumOf { it.length() }
        var count = files.size
        for (file in files) {
            if (count <= maxEntries && bytes <= maxBytes) break
            val size = file.length()
            if (file.delete()) {
                bytes -= size
                count--
            }
        }
        root.listFiles().orEmpty().filter { it.isDirectory && it.list().orEmpty().isEmpty() }
            .forEach { it.delete() }
    }

    private inline fun <T> bestEffort(operation: () -> T): T? = try {
        operation()
    } catch (_: IOException) {
        null
    } catch (_: SecurityException) {
        null
    }
}
