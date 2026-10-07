package run.holon.android.app

import android.content.Context
import java.io.IOException
import java.io.File
import java.security.MessageDigest
import java.time.Instant
import java.util.UUID
import okhttp3.Call
import okhttp3.EventListener
import okhttp3.Response
import run.holon.android.sdk.SseRetryObserver

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
        maxEvents: Int = 2_000,
        maxBytes: Long = 512 * 1024,
    ) : this(
        root = File(context.filesDir, "trace"),
        exports = File(context.cacheDir, "trace-exports"),
        maxEvents = maxEvents,
        maxBytes = maxBytes,
    )

    internal constructor(
        rootDirectory: File,
        maxEvents: Int = 2_000,
        maxBytes: Long = 512 * 1024,
    ) : this(
        root = rootDirectory,
        exports = File(rootDirectory.parentFile ?: rootDirectory, "trace-exports"),
        maxEvents = maxEvents,
        maxBytes = maxBytes,
    )

    private val lock = Any()

    init {
        root.mkdirs()
        exports.mkdirs()
        mergeLegacyScopeFiles()
    }

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
            // Diagnostics must never break the business flow; drop the event when trace I/O fails.
            runCatching {
                val file = storeFile
                val lines = file.takeIf(File::isFile)?.readLines()?.toMutableList() ?: mutableListOf()
                lines += event.toJsonLine()
                file.writeText(bound(lines).joinToString("\n", postfix = "\n"))
            }
        }
    }

    fun summary(): TraceSummary = synchronized(lock) {
        val lines = storeFile.takeIf(File::isFile)?.readLines().orEmpty()
        TraceSummary(
            eventCount = lines.size,
            oldestTimestamp = lines.firstOrNull()?.let { timestampFromJson(it) },
            newestTimestamp = lines.lastOrNull()?.let { timestampFromJson(it) },
            bytes = lines.sumOf { it.toByteArray().size + 1L },
            truncated = lines.size >= maxEvents,
        )
    }

    fun export(): File = synchronized(lock) {
        exports.mkdirs()
        pruneExports()
        val target = File(exports, "holon-trace-${UUID.randomUUID()}.jsonl")
        val lines = storeFile.takeIf(File::isFile)?.readLines().orEmpty()
        target.bufferedWriter().use { writer ->
            writer.appendLine("""{"schema":"holon.android.trace.v1","redacted":true}""")
            lines.forEach(writer::appendLine)
        }
        target
    }

    fun delete() {
        synchronized(lock) {
            runCatching { storeFile.delete() }
        }
    }

    fun deleteScope(scope: TraceScope) {
        synchronized(lock) {
            runCatching {
                val file = storeFile.takeIf(File::isFile) ?: return
                val kept = file.readLines().filterNot { it.contains("\"scope\":\"${scope.storageKey}\"") }
                file.writeText(kept.joinToString("\n", postfix = "\n"))
            }
        }
    }

    // One shared ring buffer across scopes: each event line carries its scope,
    // so a single trace keeps cross-network incidents analyzable together.
    private val storeFile: File get() = File(root, "trace.jsonl")

    private fun bound(lines: List<String>): List<String> {
        var bounded = lines
        while (bounded.size > maxEvents || bounded.sumOf { it.toByteArray().size + 1 } > maxBytes) {
            if (bounded.isEmpty()) break
            bounded = bounded.drop(1)
        }
        return bounded
    }

    // Fold legacy per-scope jsonl files into the shared store (ordered by
    // timestamp) so evidence recorded before the single-file layout survives.
    private fun mergeLegacyScopeFiles() {
        synchronized(lock) {
            runCatching {
                val legacy = root.listFiles().orEmpty()
                    .filter { file -> file.isFile && file.name.endsWith(".jsonl") && file.name != storeFile.name }
                if (legacy.isEmpty()) return
                val merged =
                    (legacy.map(File::readLines) + listOfNotNull(storeFile.takeIf(File::isFile)?.readLines()))
                        .flatten()
                        .filter { it.isNotBlank() }
                        .sortedBy { timestampFromJson(it) ?: 0L }
                storeFile.writeText(bound(merged).joinToString("\n", postfix = "\n"))
                legacy.forEach(File::delete)
            }
        }
    }

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

    private val idParentSegments = setOf("agents", "turns", "tasks")
    private val literalResourceSegments = setOf("list", "snapshot")

    fun attributes(input: Map<String, String>): Map<String, String> =
        input
            .filterKeys { !sensitiveKey.containsMatchIn(it) }
            .mapValues { (_, value) -> value.take(160).replace(Regex("\\s+"), " ") }

    fun path(value: String): String =
        runCatching {
            val raw = java.net.URI(value).path.orEmpty().ifBlank { "/" }
            val segments = raw.split('/')
            segments.mapIndexed { index, segment ->
                val previous = segments.getOrNull(index - 1).orEmpty()
                when {
                    segment.isEmpty() -> segment
                    previous in idParentSegments && segment !in literalResourceSegments -> ":id"
                    isOpaqueSegment(segment) -> ":id"
                    else -> segment
                }
            }
                .joinToString("/")
        }.getOrElse { "/" }

    private fun isOpaqueSegment(segment: String): Boolean =
        segment.isNotEmpty() &&
            (
                segment.any { it.code > 127 } ||
                    segment.length >= 16 ||
                    (segment.length >= 8 && segment.all { it in '0'..'9' || it in 'a'..'f' || it in 'A'..'F' }) ||
                    (segment.length >= 8 && segment.any { it == '-' || it == '_' || it == '%' }) ||
                    (segment.length >= 4 && segment.all { it in '0'..'9' })
            )
}

internal class TraceCallMeta(
    val path: String,
    val sse: Boolean,
    val requestId: String,
    val startedAt: Long,
)

internal object TraceHttp {
    fun started(
        recorder: TraceRecorder,
        scope: TraceScope,
        method: String,
        url: String,
    ): TraceCallMeta {
        val path = TraceRedactor.path(url)
        val sse = path.endsWith("/events/stream")
        val meta = TraceCallMeta(path, sse, UUID.randomUUID().toString(), System.currentTimeMillis())
        recorder.record(
            scope,
            TraceLevel.INFO,
            if (sse) "sse" else "http",
            if (sse) "sse.connect.started" else "http.request.started",
            requestId = meta.requestId,
            attributes = mapOf("method" to method, "path" to path),
        )
        return meta
    }

    fun completed(
        recorder: TraceRecorder,
        scope: TraceScope,
        meta: TraceCallMeta,
        statusCode: Int?,
    ) {
        recorder.record(
            scope,
            TraceLevel.INFO,
            if (meta.sse) "sse" else "http",
            if (meta.sse) "sse.stream.ended" else "http.request.completed",
            requestId = meta.requestId,
            durationMs = System.currentTimeMillis() - meta.startedAt,
            statusCode = statusCode,
        )
    }

    fun failed(
        recorder: TraceRecorder,
        scope: TraceScope,
        meta: TraceCallMeta,
        errorType: String,
    ) {
        recorder.record(
            scope,
            TraceLevel.ERROR,
            if (meta.sse) "sse" else "http",
            if (meta.sse) "sse.stream.failed" else "http.request.failed",
            requestId = meta.requestId,
            durationMs = System.currentTimeMillis() - meta.startedAt,
            attributes = mapOf("errorType" to errorType.ifBlank { "Unknown" }),
        )
    }

    fun sseReconnectScheduled(
        recorder: TraceRecorder,
        scope: TraceScope,
        path: String,
        attempt: Int,
        backoffMs: Long,
    ) {
        recorder.record(
            scope,
            TraceLevel.WARN,
            "sse",
            "sse.reconnect.scheduled",
            attributes = mapOf(
                "path" to TraceRedactor.path("https://trace.local/$path"),
                "attempt" to attempt.toString(),
                "backoffMs" to backoffMs.toString(),
            ),
        )
    }
}

internal class TraceHttpEventListener(
    private val recorder: TraceRecorder,
    private val scope: TraceScope,
) : EventListener() {
    private var meta: TraceCallMeta? = null
    private var statusCode: Int? = null

    override fun callStart(call: Call) {
        val request = call.request()
        statusCode = null
        meta = TraceHttp.started(recorder, scope, request.method, request.url.toString())
    }

    override fun responseHeadersEnd(call: Call, response: Response) {
        statusCode = response.code
    }

    override fun callEnd(call: Call) {
        meta?.let { TraceHttp.completed(recorder, scope, it, statusCode) }
        meta = null
    }

    override fun callFailed(call: Call, e: IOException) {
        meta?.let { TraceHttp.failed(recorder, scope, it, e::class.simpleName.orEmpty()) }
        meta = null
    }
}

internal fun traceEventListenerFactory(recorder: TraceRecorder, scope: TraceScope): EventListener.Factory =
    EventListener.Factory { TraceHttpEventListener(recorder, scope) }

internal fun traceSseRetryObserver(recorder: TraceRecorder, scope: TraceScope): SseRetryObserver =
    SseRetryObserver { path, attempt, delayMillis ->
        TraceHttp.sseReconnectScheduled(recorder, scope, path, attempt, delayMillis)
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
