package run.holon.android.sdk

import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlin.test.*

class ConversationProjectionTest {
    private fun fixture(): HolonConversationSnapshot = HolonConversationSnapshot.from(HolonJsonDocument(
        HolonWire.json.parseToJsonElement(javaClass.getResource("/mobile-conversation-v2.json")!!.readText()),
    ))
    private fun event(body: String) = HolonSseEvent(id = null, event = "message", data = body)
    private val begin = """{"type":"batch_begin","batch_id":"batch","through_seq":90,"schema_version":2,"query_version":2,"runtime_id":"runtime-mobile","event_log_epoch":"epoch-mobile","visibility_scope_id":"scope-mobile"}"""
    private val checkpoint = """{"type":"checkpoint","batch_id":"batch","through_seq":90,"event_log_epoch":"epoch-mobile","visibility_scope_id":"scope-mobile","checkpoint":"opaque-stream"}"""

    @Test fun `canonical index beats wall clock and active window remains visible`() {
        val state = fixture()
        assertEquals(listOf("outside-window", "older", "newer"), state.turns.map { it.id })
        assertEquals(1200L, state.turns.last().durationMillis)
        assertTrue(state.turns.first().inputs.single().interjected)
        assertEquals(88L, state.turns.first().inputs.single().activityKey?.eventSeq)
        assertEquals("Please inspect the report", state.pendingInputs.single().preview)
        assertEquals("operator", state.pendingInputs.single().presentationClass)
    }

    @Test fun `late history does not regress revision or advance live checkpoint`() {
        val state = fixture()
        val lateTurn = state.turns.last().raw.toMutableMap().apply { put("revision", JsonPrimitive(1)) }
        val pageRaw = state.raw.toMutableMap().apply {
            put("turns", JsonArray(listOf(JsonObject(lateTurn))))
            put("active_turns", JsonArray(emptyList()))
            put("pending_inputs", JsonArray(emptyList()))
            put("snapshot_cursor", JsonPrimitive("not-a-stream-checkpoint"))
        }
        val merged = mergeConversationPage(state, HolonConversationSnapshot.from(HolonJsonDocument(JsonObject(pageRaw))), history = true)
        assertEquals(9L, merged.turns.last().revision)
        assertEquals(state.snapshotCursor, merged.snapshotCursor)
        assertEquals(state.pendingInputs, merged.pendingInputs)
    }

    @Test fun `incomplete batch is invisible and assignment commits atomically`() {
        val state = fixture()
        val reducer = ConversationStreamReducer(state)
        assertNull(reducer.accept(event(begin)))
        assertNull(reducer.accept(event("""{"type":"operator_remove","message_id":"pending-message","revision":6}""")))
        assertEquals(state, reducer.snapshot)
        val committed = reducer.accept(event(checkpoint))!!
        assertTrue(committed.pendingInputs.isEmpty())
        assertEquals("opaque-stream", committed.snapshotCursor)
    }

    @Test fun `unknown control mismatched batch and unknown versions fail closed`() {
        assertFailsWith<HolonProtocolException> { ConversationStreamReducer(fixture()).accept(event(checkpoint)) }
        val reducer = ConversationStreamReducer(fixture())
        reducer.accept(event(begin))
        assertFailsWith<HolonProtocolException> { reducer.accept(event("""{"type":"future_control"}""")) }
        assertEquals("opaque-bootstrap", reducer.snapshot.snapshotCursor)
        assertFailsWith<HolonProtocolException> { ConversationStreamReducer(fixture()).accept(event(begin.replace("\"schema_version\":2", "\"schema_version\":999"))) }
    }

    @Test fun `epoch replacement drops old identity and terminal cache is bounded`() {
        val state = fixture()
        val replaced = state.raw.toMutableMap().apply { put("event_log_epoch", JsonPrimitive("new-epoch")); put("turns", JsonArray(emptyList())); put("active_turns", JsonArray(emptyList())) }
        assertTrue(mergeConversationPage(state, HolonConversationSnapshot.from(HolonJsonDocument(JsonObject(replaced)))).turns.isEmpty())
        assertEquals(2, mergeConversationPage(state, state, maxTurns = 1).turns.size) // one terminal plus active
    }

    @Test fun `terminal state never regresses and equal revisions must agree`() {
        val terminal = fixture().turns.last()
        fun altered(key: String, value: kotlinx.serialization.json.JsonElement): HolonConversationTurn {
            val raw = terminal.raw.toMutableMap().apply { put(key, value) }
            val snapshot = fixture().raw.toMutableMap().apply {
                put("turns", JsonArray(listOf(JsonObject(raw)))); put("active_turns", JsonArray(emptyList()))
            }
            return HolonConversationSnapshot.from(HolonJsonDocument(JsonObject(snapshot))).turns.single()
        }
        val active = altered("execution", HolonWire.json.parseToJsonElement("""{"kind":"active"}"""))
        assertEquals(terminal, mergeConversationTurns(listOf(terminal), listOf(active)).single())
        val conflicting = altered("settled", JsonPrimitive(false))
        assertFailsWith<HolonProtocolException> { mergeConversationTurns(listOf(terminal), listOf(conflicting)) }
    }

    @Test fun `removed input cannot be resurrected by an equal revision`() {
        val state = fixture()
        val reducer = ConversationStreamReducer(state)
        reducer.accept(event(begin))
        reducer.accept(event("""{"type":"operator_remove","message_id":"pending-message","revision":6}"""))
        reducer.accept(event(checkpoint))
        reducer.accept(event(begin.replace("90", "91")))
        val input = state.raw["pending_inputs"]!!.let { (it as JsonArray).single() as JsonObject }.toMutableMap()
        input["revision"] = JsonPrimitive(6)
        reducer.accept(event(JsonObject(mapOf("type" to JsonPrimitive("operator_upsert"), "input" to JsonObject(input))).toString()))
        val committed = reducer.accept(event(checkpoint.replace("90", "91")))!!
        assertTrue(committed.pendingInputs.isEmpty())
    }
}
