package run.holon.android.app

import java.io.File

internal const val MAX_CACHED_TURNS = 180
internal const val MAX_CACHED_BRIEFS = 180
private const val MAX_CACHED_ARTIFACT_BYTES = 200L * 1024 * 1024

/** Only re-downloadable previews are evicted. Draft/outbox files are never cache. */
internal fun trimArtifactCache(directory: File, protected: File) {
    val files = directory.listFiles().orEmpty().filter(File::isFile)
    var bytes = files.sumOf(File::length)
    files.sortedBy(File::lastModified).filterNot { it == protected }.forEach { file ->
        if (bytes > MAX_CACHED_ARTIFACT_BYTES && file.delete()) bytes -= file.length()
    }
}
