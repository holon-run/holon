package run.holon.android.sdk

import java.io.Closeable
import java.io.IOException
import java.util.LinkedHashSet
import java.util.concurrent.Executors
import java.util.concurrent.atomic.AtomicReference
import okhttp3.ResponseBody
import okio.BufferedSource
import okio.buffer
import okio.source

/** Parser for the wire format defined by the Server-Sent Events specification. */
public object HolonSseParser {
    public fun parse(text: String): List<HolonSseEvent> =
        parse(text.byteInputStream().source().buffer()).toList()

    internal fun parse(source: BufferedSource): Sequence<HolonSseEvent> =
        sequence {
            var event = ""
            var id: String? = null
            val data = mutableListOf<String>()

            fun dispatch(): HolonSseEvent? {
                if (data.isEmpty()) {
                    event = ""
                    id = null
                    return null
                }
                val result =
                    HolonSseEvent(
                        event = event.ifEmpty { "message" },
                        id = id,
                        data = data.joinToString("\n"),
                    )
                event = ""
                id = null
                data.clear()
                return result
            }

            while (!source.exhausted()) {
                val line = source.readUtf8Line() ?: break
                if (line.isEmpty()) {
                    dispatch()?.let { yield(it) }
                    continue
                }
                if (line.startsWith(":")) {
                    continue
                }
                val separator = line.indexOf(':')
                val field = if (separator < 0) line else line.substring(0, separator)
                var value = if (separator < 0) "" else line.substring(separator + 1)
                if (value.startsWith(" ")) {
                    value = value.substring(1)
                }
                when (field) {
                    "event" -> event = value
                    "id" -> id = value
                    "data" -> data += value
                }
            }
            dispatch()?.let { yield(it) }
        }

    internal fun parse(body: ResponseBody): Sequence<HolonSseEvent> =
        parse(body.source())
}

/** Drops duplicate SSE frames while retaining the last cursor for reconnects. */
public class HolonSseDeduplicator(
    private val maxRememberedIds: Int = 512,
) {
    private val seenIds = LinkedHashSet<String>()
    private var highestEventSeq: Long? = null

    init {
        require(maxRememberedIds > 0) { "maxRememberedIds must be positive" }
    }

    public var lastEventId: String? = null
        private set

    public fun accept(event: HolonSseEvent): Boolean {
        event.id?.let { id ->
            lastEventId = id
            if (!seenIds.add(id)) {
                return false
            }
            while (seenIds.size > maxRememberedIds) {
                seenIds.iterator().let { iterator ->
                    iterator.next()
                    iterator.remove()
                }
            }
        }
        event.eventSeq?.let { seq ->
            if (highestEventSeq != null && seq <= highestEventSeq!!) {
                return false
            }
            highestEventSeq = seq
        }
        return true
    }
}

public class HolonSseConnection internal constructor(
    private val body: ResponseBody,
    private val deduplicator: HolonSseDeduplicator?,
    private val cancelCall: () -> Unit,
) : Closeable {
    private enum class State {
        NEW,
        CLAIMED,
        READING,
        CLOSED,
    }

    private val state = AtomicReference(State.NEW)

    public fun events(): Sequence<HolonSseEvent> {
        check(state.compareAndSet(State.NEW, State.CLAIMED)) {
            "SSE connection has already been consumed or closed"
        }
        return sequence {
            check(state.compareAndSet(State.CLAIMED, State.READING)) {
                "SSE connection was closed before it could be read"
            }
            try {
                for (event in HolonSseParser.parse(body)) {
                    if (deduplicator == null || deduplicator.accept(event)) {
                        yield(event)
                    }
                }
            } finally {
                state.set(State.CLOSED)
                body.close()
            }
        }
    }

    /**
     * Cancels the call immediately. Unread bodies are released in the background;
     * an active reader retains ownership of its body's cleanup.
     */
    override fun close() {
        val previous = state.getAndSet(State.CLOSED)
        if (previous == State.CLOSED) {
            return
        }
        cancelCall()
        if (previous == State.NEW || previous == State.CLAIMED) {
            bodyCloser.execute {
                try {
                    body.close()
                } catch (_: Exception) {
                    // A cancelled body's cleanup must not escape the background task.
                }
            }
        }
    }

    private companion object {
        val bodyCloser =
            Executors.newSingleThreadExecutor { task ->
                Thread(task, "holon-sse-close").apply { isDaemon = true }
            }
    }
}

internal fun IOException.isRetryableSseFailure(): Boolean =
    when (this) {
        is HolonHttpException -> isTransientHttpStatus(statusCode)
        is HolonProtocolException -> false
        else -> message?.contains("canceled", ignoreCase = true) != true
    }
