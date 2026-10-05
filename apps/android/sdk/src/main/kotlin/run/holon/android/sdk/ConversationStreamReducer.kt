package run.holon.android.sdk

import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.longOrNull

/** Immutable publication at checkpoints; partial batches never touch visible state. */
public class ConversationStreamReducer(initial: HolonConversationSnapshot, private val maxTurns: Int = 180) {
    public var snapshot: HolonConversationSnapshot = initial
        private set
    private val boundary = ConversationBatchBoundary(initial)
    private val mutations = mutableListOf<JsonObject>()
    private val inputRevisions = initial.pendingInputs.mapNotNull { input -> input.revision?.let { input.messageId to it } }.toMap().toMutableMap()
    private val removedInputs = linkedMapOf<String, Long>()

    /** Null means the batch is incomplete; reset requires a fresh authoritative bootstrap. */
    public fun accept(event: HolonSseEvent): HolonConversationSnapshot? {
        val completed = boundary.accept(event)
        when (val change = event.toConversationEvent()) {
            is HolonConversationStreamEvent.BatchBegin -> mutations.clear()
            is HolonConversationStreamEvent.Mutation -> mutations.add(change.raw)
            is HolonConversationStreamEvent.ResetRequired -> {
                mutations.clear()
                throw HolonProtocolException("Conversation reset required: ${change.reason}")
            }
            is HolonConversationStreamEvent.Checkpoint -> {
                check(completed)
                val pending = (snapshot.raw["pending_inputs"] as? JsonArray).orEmpty()
                    .mapNotNull { it as? JsonObject }.associateByTo(linkedMapOf()) { it["message_id"]?.jsonPrimitive?.contentOrNull }
                var turns = snapshot.turns
                mutations.forEach { mutation ->
                    when (mutation["type"]?.jsonPrimitive?.contentOrNull) {
                        "turn_summary_upsert" -> {
                            val turn = mutation["turn"] as? JsonObject ?: throw HolonProtocolException("Missing turn upsert")
                            val raw = snapshot.raw.toMutableMap()
                            raw["turns"] = JsonArray(listOf(turn))
                            raw["active_turns"] = JsonArray(emptyList())
                            val decoded = HolonConversationSnapshot.from(HolonJsonDocument(JsonObject(raw)))
                            turns = mergeConversationTurns(turns, decoded.turns)
                        }
                        "operator_upsert", "operator_remove" -> {
                            val input = mutation["input"] as? JsonObject
                            val id = (input?.get("message_id") ?: mutation["message_id"])?.jsonPrimitive?.contentOrNull
                                ?: throw HolonProtocolException("Missing input identity")
                            val revision = (input?.get("revision") ?: mutation["revision"])?.jsonPrimitive?.longOrNull ?: 0
                            if (input != null && revision <= (removedInputs[id] ?: -1)) return@forEach
                            val previous = inputRevisions[id] ?: -1
                            if (input != null && revision == previous && pending[id] != null && pending[id] != input) {
                                throw HolonProtocolException("Input reused revision with different content: $id")
                            }
                            if (revision >= previous) {
                                inputRevisions[id] = revision
                                if (input == null) {
                                    pending.remove(id)
                                    removedInputs.remove(id)
                                    removedInputs[id] = revision
                                } else {
                                    removedInputs.remove(id)
                                    pending[id] = input
                                }
                            }
                        }
                    }
                }
                val assigned = turns.flatMap { it.inputs }.mapTo(hashSetOf()) { it.messageId }
                assigned.forEach(pending::remove)
                val active = turns.filter { it.executionKind == "active" }
                turns = mergeConversationTurns(turns.filter { it.executionKind != "active" }.takeLast(maxTurns), active)
                val raw = snapshot.raw.toMutableMap()
                raw["turns"] = JsonArray(turns.map { it.raw })
                raw["active_turns"] = JsonArray(active.map { it.raw })
                raw["pending_inputs"] = JsonArray(pending.values.toList())
                raw["snapshot_cursor"] = JsonPrimitive(change.checkpoint)
                val next = HolonConversationSnapshot.from(HolonJsonDocument(JsonObject(raw)))
                snapshot = next
                mutations.clear()
                // Tombstones are recovery-local, not an unbounded second message ledger.
                if (inputRevisions.size > 4096) {
                    inputRevisions.keys.retainAll(pending.keys + assigned)
                }
                while (removedInputs.size > 4096) removedInputs.remove(removedInputs.keys.first())
                return next
            }
            is HolonConversationStreamEvent.Unknown -> throw HolonProtocolException("Unknown conversation control")
        }
        return null
    }
}
