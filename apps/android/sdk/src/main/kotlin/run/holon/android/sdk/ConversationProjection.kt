package run.holon.android.sdk

import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.longOrNull

/** Shared mobile rules: entity revision is not a checkpoint or a timestamp. */
public fun mergeConversationTurns(
    existing: List<HolonConversationTurn>,
    incoming: List<HolonConversationTurn>,
): List<HolonConversationTurn> {
    val byId = existing.associateByTo(linkedMapOf()) { it.id }
    incoming.forEach { turn ->
        val previous = byId[turn.id]
        if (previous == null || previous.revision == null || turn.revision == null || turn.revision >= previous.revision) {
            byId[turn.id] = turn
        }
    }
    return byId.values.sortedWith(compareBy<HolonConversationTurn> { it.turnIndex ?: Long.MAX_VALUE }
        .thenBy { if (it.turnIndex == null) it.startedAt.orEmpty() else "" }.thenBy { it.id })
}

public fun sameConversationScope(a: HolonConversationSnapshot, b: HolonConversationSnapshot): Boolean =
    a.runtimeId == b.runtimeId && a.eventLogEpoch == b.eventLogEpoch &&
        a.agentId == b.agentId && a.visibilityScopeId == b.visibilityScopeId

/** A history page updates entities, never the live pending set or checkpoint. */
public fun mergeConversationPage(
    existing: HolonConversationSnapshot?,
    incoming: HolonConversationSnapshot,
    history: Boolean = false,
    maxTurns: Int = 180,
): HolonConversationSnapshot {
    if (existing == null || !sameConversationScope(existing, incoming)) return incoming
    val incomingIds = incoming.turns.mapTo(hashSetOf()) { it.id }
    val retained = if (history) existing.turns else existing.turns.filter {
        it.executionKind != "active" || it.id in incomingIds
    }
    val merged = mergeConversationTurns(retained, incoming.turns)
    val active = merged.filter { it.executionKind == "active" }
    val bounded = mergeConversationTurns(merged.filter { it.executionKind != "active" }.takeLast(maxTurns), active)
    val raw = (if (history) existing.raw else incoming.raw).toMutableMap()
    raw["turns"] = JsonArray(bounded.map { it.raw })
    raw["active_turns"] = JsonArray(active.map { it.raw })
    return HolonConversationSnapshot.from(HolonJsonDocument(JsonObject(raw)))
}

internal fun validateConversationVersions(raw: JsonObject) {
    for (key in listOf("schema_version", "query_version")) {
        val field = raw[key] ?: continue // historical additive-field fixtures
        val version = field.jsonPrimitive.longOrNull ?: throw HolonProtocolException("Malformed conversation $key")
        if (version !in 1L..2L) throw HolonProtocolException("Unsupported conversation $key: $version; upgrade the app or daemon")
    }
}

/** Do not apply/persist incomplete batches or silently accept unknown controls. */
public class ConversationBatchBoundary(private val snapshot: HolonConversationSnapshot) {
    private var batch: JsonObject? = null
    private var bytes = 0

    public fun accept(event: HolonSseEvent): Boolean {
        val raw = event.json() as? JsonObject ?: throw HolonProtocolException("Invalid conversation frame")
        val type = raw["type"]?.jsonPrimitive?.contentOrNull ?: event.event
        fun matchesScope(value: JsonObject) {
            for ((key, expected) in listOf("runtime_id" to snapshot.runtimeId,
                "event_log_epoch" to snapshot.eventLogEpoch, "visibility_scope_id" to snapshot.visibilityScopeId)) {
                if (expected != null && value[key]?.jsonPrimitive?.contentOrNull != expected) {
                    throw HolonProtocolException("Conversation stream scope changed")
                }
            }
        }
        return when (type) {
            "batch_begin" -> {
                if (batch != null) throw HolonProtocolException("Nested conversation batch")
                validateConversationVersions(raw)
                matchesScope(raw)
                if (raw["batch_id"]?.jsonPrimitive?.contentOrNull.isNullOrBlank() || raw["through_seq"]?.jsonPrimitive?.longOrNull == null) {
                    throw HolonProtocolException("Incomplete conversation batch boundary")
                }
                batch = raw
                bytes = 0
                false
            }
            "checkpoint" -> {
                val started = batch ?: throw HolonProtocolException("Conversation checkpoint without batch")
                if (started["batch_id"] != raw["batch_id"] || started["through_seq"] != raw["through_seq"]) {
                    throw HolonProtocolException("Conversation batch checkpoint mismatch")
                }
                // Checkpoint omits runtime_id; it is bound by its matching begin frame.
                for (key in listOf("event_log_epoch", "visibility_scope_id")) {
                    if (started[key] != raw[key]) throw HolonProtocolException("Conversation checkpoint scope changed")
                }
                batch = null
                true
            }
            "reset_required" -> { batch = null; true }
            "operator_upsert", "operator_remove", "turn_summary_upsert", "activity_upsert", "detail_invalidated" -> {
                if (batch == null) throw HolonProtocolException("Conversation mutation outside batch")
                bytes += event.data.toByteArray(Charsets.UTF_8).size
                if (bytes > 4 * 1024 * 1024) throw HolonProtocolException("Conversation batch exceeds mobile budget")
                false
            }
            else -> throw HolonProtocolException("Unknown conversation control: $type")
        }
    }
}
