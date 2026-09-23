package run.holon.android.sdk

import kotlinx.serialization.json.JsonArray
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

public data class HolonConversationTurn(
    public val id: String,
    public val summary: String,
    public val raw: JsonObject,
)

public data class HolonConversationSnapshot(
    public val raw: JsonObject,
    public val snapshotCursor: String?,
    public val turns: List<HolonConversationTurn>,
) {
    public companion object {
        public fun from(document: HolonJsonDocument): HolonConversationSnapshot {
            val raw =
                document.objectOrNull
                    ?: throw HolonProtocolException("Holon conversation snapshot is not an object")
            val turns =
                (raw["turns"] as? JsonArray).orEmpty().mapIndexed { index, element ->
                    val turn = element as? JsonObject
                        ?: throw HolonProtocolException("Holon conversation turn $index is not an object")
                    HolonConversationTurn(
                        id = turn.stringValue("turn_id") ?: turn.stringValue("id") ?: "turn-$index",
                        summary =
                            turn.stringValue("title")
                                ?: turn.stringValue("summary")
                                ?: turn.stringValue("status")
                                ?: turn.toString(),
                        raw = turn,
                    )
                }
            return HolonConversationSnapshot(raw, raw.stringValue("snapshot_cursor"), turns)
        }
    }
}

public sealed interface HolonConversationStreamEvent {
    public data class BatchBegin(
        public val batchId: String?,
        public val throughSeq: Long?,
    ) : HolonConversationStreamEvent

    public data class Checkpoint(
        public val checkpoint: String,
        public val throughSeq: Long?,
    ) : HolonConversationStreamEvent

    public data class ResetRequired(
        public val reason: String?,
        public val hint: String?,
    ) : HolonConversationStreamEvent

    public data class Mutation(
        public val type: String,
        public val raw: JsonObject,
    ) : HolonConversationStreamEvent

    public data class Unknown(
        public val type: String,
        public val raw: JsonElement?,
    ) : HolonConversationStreamEvent
}

public fun HolonSseEvent.toConversationEvent(): HolonConversationStreamEvent {
    val objectValue = json() as? JsonObject
    val type = objectValue.stringValue("type") ?: event
    return when (type) {
        "batch_begin" ->
            HolonConversationStreamEvent.BatchBegin(
                batchId = objectValue.stringValue("batch_id"),
                throughSeq = objectValue.longValue("through_seq"),
            )
        "checkpoint" ->
            HolonConversationStreamEvent.Checkpoint(
                checkpoint =
                    objectValue.stringValue("checkpoint")
                        ?: id
                        ?: throw HolonProtocolException("Conversation checkpoint is missing its cursor"),
                throughSeq = objectValue.longValue("through_seq"),
            )
        "reset_required" ->
            HolonConversationStreamEvent.ResetRequired(
                reason = objectValue.stringValue("reason"),
                hint = objectValue.stringValue("hint"),
            )
        "operator_upsert", "operator_remove", "turn_summary_upsert",
        "activity_upsert", "detail_invalidated" ->
            HolonConversationStreamEvent.Mutation(type, objectValue ?: JsonObject(emptyMap()))
        else -> HolonConversationStreamEvent.Unknown(type, objectValue ?: json())
    }
}

private fun JsonObject?.stringValue(name: String): String? =
    this?.get(name)?.jsonPrimitive?.contentOrNull

private fun JsonObject?.longValue(name: String): Long? =
    this?.get(name)?.jsonPrimitive?.longOrNull

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
