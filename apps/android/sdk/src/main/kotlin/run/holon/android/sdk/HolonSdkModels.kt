package run.holon.android.sdk

import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.longOrNull

/** A forward-compatible JSON response from a Holon route. */
public data class HolonJsonDocument(
    public val raw: JsonElement,
) {
    public val objectOrNull: JsonObject?
        get() = raw as? JsonObject
}

public data class HolonEnqueueResult(
    public val ok: Boolean,
    public val agentId: String,
    public val messageId: String?,
    public val raw: JsonObject,
)

public data class HolonArtifact(
    public val artifactIndex: Int,
    public val size: Long,
    public val content: String,
)

public data class HolonSseEvent(
    public val event: String = "message",
    public val id: String? = null,
    public val data: String,
) {
    public fun json(): JsonElement? =
        runCatching { HolonWire.json.parseToJsonElement(data) }.getOrNull()

    /**
     * Durable event ordering supplied by the envelope payload.
     *
     * SSE `id` is the reconnect cursor (`Last-Event-ID`); `event_seq` is the
     * only event ordering source and is never inferred from nested payloads.
     */
    public val eventSeq: Long?
        get() =
            json()
                ?.let { element -> (element as? JsonObject)?.get("event_seq")?.jsonPrimitive?.longOrNull }
}

public data class SseReconnectPolicy(
    public val maxAttempts: Int = 5,
    public val initialDelayMillis: Long = 250,
    public val maxDelayMillis: Long = 5_000,
) {
    init {
        require(maxAttempts >= 0) { "maxAttempts must be non-negative" }
        require(initialDelayMillis >= 0) { "initialDelayMillis must be non-negative" }
        require(maxDelayMillis >= initialDelayMillis) {
            "maxDelayMillis must be at least initialDelayMillis"
        }
    }
}

internal fun JsonObject.string(name: String): String? =
    this[name]?.jsonPrimitive?.contentOrNull

internal fun JsonObject.long(name: String): Long? =
    this[name]?.jsonPrimitive?.longOrNull

internal fun JsonObject.int(name: String): Int? =
    long(name)?.takeIf { it in Int.MIN_VALUE..Int.MAX_VALUE }?.toInt()
