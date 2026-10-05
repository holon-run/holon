package run.holon.android.app

import java.io.File
import java.security.MessageDigest

/** File ownership follows database references, not whichever network is selected. */
internal class ScopedFiles(private val root: File) {
    fun removeUnreferenced(removed: Set<String>, retained: Set<String>) {
        val canonicalRoot = root.canonicalFile.toPath()
        val live = retained.map { File(it).canonicalFile.toPath() }.toSet()
        removed.forEach { path ->
            val file = File(path).canonicalFile
            if (file.toPath().startsWith(canonicalRoot) && file.toPath() != canonicalRoot && file.toPath() !in live) {
                file.delete()
            }
        }
    }
}

internal fun scopeFileKey(scope: String): String =
    MessageDigest.getInstance("SHA-256").digest(scope.toByteArray()).joinToString("") { "%02x".format(it) }
