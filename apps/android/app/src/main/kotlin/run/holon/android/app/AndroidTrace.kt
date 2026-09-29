package run.holon.android.app

import android.content.Context
import java.io.File
import java.security.MessageDigest
import java.time.Instant
import java.util.UUID

internal enum class TraceLevel { DEBUG, INFO, WARN, ERROR }

internal data class TraceEvent(
    val timestamp: Long,
    val level: TraceLevel,
    val category: String,
    val name: String,
    val scope: String,
    val requestId: String? = null,
    val durationMs: Long? = null,
    val statusCode: Int? = null,
    val attributes: Map<String, String> = emptyMap(),
) {
    fun toJsonLine(): String {
        val fields = linkedMapOf<String, String?>(
            "timestamp" to Instant.ofEpochMilli(timestamp).toString(),
            "level" to level.name,
            "category" to category,
            "name" to name,
            "scope" to scope,
            "requestId" to requestId,
            "durationMs" to durationMs?.toString(),
            "statusCode" to statusCode?.toString(),
        )
        val attributesJson =
            attributes.entries.joinToString(",") { (key, value) ->
                "${jsonString(key)}:${jsonString(value)}"
            }
        return buildString {
            append("{")
            append(fields.entries.filter { it.value != null }.joinToString(",") { (key, value) ->
                "${jsonString(key)}:${jsonString(value!!)}"
            })
            if (attributesJson.isNotEmpty()) {
                if (length > 1) append(",")
                append("\"attributes\":{$attributesJson}")
            }
            append("}")
        }
    }
}

internal data class TraceSummary(
    val eventCount: Int,
    val oldestTimestamp: Long?,
    val newestTimestamp: Long?,
    val bytes: Long,
    val truncated: Boolean,
)

internal class TraceRecorder private constructor(
    private val root: File,
    private val exports: File,
    private val maxEvents: Int,
    private val maxBytes: Long,
) {
    constructor(
        context: Context,
        maxEvents: Int = 1_000,
        maxBytes: Long = 512 * 1024,
    ) : this(
        root = File(context.filesDir, "trace"),
        exports = File(context.cacheDir, "shared-artifacts"),
        maxEvents = maxEvents,
        maxBytes = maxBytes,
    )

    internal constructor(
        rootDirectory: File,
        maxEvents: Int = 1_000,
        maxBytes: Long = 512 * 1024,
    ) : this(
        root = rootDirectory,
        exports = File(rootDirectory.parentFile ?: rootDirectory, "shared-artifacts"),
        maxEvents = maxEvents,
        maxBytes = maxBytes,
    )

    init {
        root.mkdirs()
        exports.mkdirs()
    }
    private val lock = Any()

    fun record(
        scope: TraceScope,
        level: TraceLevel,
        category: String,
        name: String,
        requestId: String? = null,
        durationMs: Long? = null,
        statusCode: Int? = null,
        attributes: Map<String, String> = emptyMap(),
    ) {
        val event =
            TraceEvent(
                timestamp = System.currentTimeMillis(),
                level = level,
                category = category,
                name = name,
                scope = scope.storageKey,
                requestId = requestId?.let(::shortId),
                durationMs = durationMs,
                statusCode = statusCode,
                attributes = TraceRedactor.attributes(attributes),
            )
        synchronized(lock) {
            val file = fileFor(scope)
            val lines = file.takeIf(File::isFile)?.readLines()?.toMutableList() ?: mutableListOf()
            lines += event.toJsonLine()
            while (lines.size > maxEvents || lines.sumOf { it.toByteArray().size + 1 } > maxBytes) {
                if (lines.isEmpty()) break
                lines.removeAt(0)
            }
            file.writeText(lines.joinToString("\n", postfix = "\n"))
        }
    }

    fun summary(scope: TraceScope): TraceSummary = synchronized(lock) {
        val lines = fileFor(scope).takeIf(File::isFile)?.readLines().orEmpty()
        TraceSummary(
            eventCount = lines.size,
            oldestTimestamp = lines.firstOrNull()?.let { timestampFromJson(it) },
            newestTimestamp = lines.lastOrNull()?.let { timestampFromJson(it) },
            bytes = lines.sumOf { it.toByteArray().size + 1L },
            truncated = lines.size >= maxEvents,
        )
    }

    fun export(scope: TraceScope): File = synchronized(lock) {
        pruneExports()
        val target = File(exports, "holon-trace-${scope.storageKey}-${UUID.randomUUID()}.jsonl")
        val lines = fileFor(scope).takeIf(File::isFile)?.readLines().orEmpty()
        target.bufferedWriter().use { writer ->
            writer.appendLine("""{"schema":"holon.android.trace.v1","scope":"${scope.storageKey}","redacted":true}""")
            lines.forEach(writer::appendLine)
        }
        target
    }

    private fun fileFor(scope: TraceScope): File = File(root, "${scope.storageKey}.jsonl")

    private fun pruneExports() {
        val files = exports.listFiles().orEmpty().filter(File::isFile).sortedByDescending(File::lastModified)
        files.drop(2).forEach(File::delete)
        files.filter { System.currentTimeMillis() - it.lastModified() > 24 * 60 * 60 * 1_000L }
            .forEach(File::delete)
    }

    private fun timestampFromJson(line: String): Long? =
        Regex(""""timestamp":"([^"]+)"""").find(line)?.groupValues?.get(1)
            ?.let { runCatching { Instant.parse(it).toEpochMilli() }.getOrNull() }

    private fun shortId(value: String): String = value.take(12)
}

internal sealed class TraceScope {
    abstract val storageKey: String

    data object Global : TraceScope() {
        override val storageKey: String = "global"
    }

    class Network(profileId: String) : TraceScope() {
        override val storageKey: String = "network-${hash(profileId)}"
    }

    companion object {
        private fun hash(value: String): String =
            MessageDigest.getInstance("SHA-256").digest(value.toByteArray())
                .joinToString("") { "%02x".format(it) }.take(16)
    }
}

internal object TraceRedactor {
    private val sensitiveKey =
        Regex("(?i)(token|authorization|cookie|password|secret|credential|body|content|payload|prompt|attachment)")

    fun attributes(input: Map<String, String>): Map<String, String> =
        input
            .filterKeys { !sensitiveKey.containsMatchIn(it) }
            .mapValues { (_, value) -> value.take(160).replace(Regex("\\s+"), " ") }

    fun path(value: String): String =
        runCatching {
            val uri = java.net.URI(value)
            uri.path.orEmpty().ifBlank { "/" }.replace(Regex("/[0-9a-fA-F-]{8,}"), "/:id")
        }.getOrElse { "/" }
}

private fun jsonString(value: String): String =
    buildString {
        append('"')
        value.forEach { char ->
            when (char) {
                '\\' -> append("\\\\")
                '"' -> append("\\\"")
                '\n' -> append("\\n")
                '\r' -> append("\\r")
                '\t' -> append("\\t")
                else -> append(char)
            }
        }
        append('"')
    }
