package run.holon.android.sdk

import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.jsonObject
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertNull
import kotlin.test.assertTrue

class HolonConversationTurnFieldsTest {
    private fun snapshot(turn: JsonElement, active: Boolean = false): HolonConversationSnapshot =
        HolonConversationSnapshot.from(
            HolonJsonDocument(
                JsonObject(
                    mapOf(
                        "turns" to JsonArray(if (active) emptyList() else listOf(turn)),
                        "active_turns" to JsonArray(if (active) listOf(turn) else emptyList()),
                    ),
                ),
            ),
        )

    private fun turn(
        reason: String = """{"kind":"tool_only_wait"}""",
        truncated: Boolean = true,
        revision: Int = 2,
        execution: String = "terminal",
    ): JsonObject = HolonWire.json.parseToJsonElement(
        """{"turn_id":"turn","revision":$revision,"execution":{"kind":"$execution"},
            "result":{"kind":"none","reason":$reason},"inputs_truncated":$truncated}""",
    ).jsonObject

    @Test fun `legacy constructors and snapshots retain additive field defaults`() {
        val oldConstructor = HolonConversationTurn(
            "turn", "summary", null, emptyList(), "terminal", null, "none",
            null, emptyList(), null, null, false, JsonObject(emptyMap()), 1, 2, 3,
        )
        assertNull(oldConstructor.resultReasonKind)
        assertFalse(oldConstructor.inputsTruncated)
        val historical = snapshot(HolonWire.json.parseToJsonElement("""{"turn_id":"turn","result":{"kind":"none"}}"""))
        assertNull(historical.turns.single().resultReasonKind)
        assertFalse(historical.turns.single().inputsTruncated)
        val existingFixture = HolonConversationSnapshot.from(
            HolonJsonDocument(HolonWire.json.parseToJsonElement(fixture("mobile-conversation-v2.json"))),
        )
        assertTrue(existingFixture.turns.all { it.resultReasonKind == null && !it.inputsTruncated })
    }

    @Test fun `completed and active windows decode reason and truncation without changing raw`() {
        for (active in listOf(false, true)) {
            val raw = turn(execution = if (active) "active" else "terminal")
            val decoded = snapshot(raw, active).turns.single()
            assertEquals("tool_only_wait", decoded.resultReasonKind)
            assertTrue(decoded.inputsTruncated)
            assertEquals(raw, decoded.raw)
        }
        for (reason in listOf("reducer_only", "aborted", "interrupted", "future_reason")) {
            assertEquals(reason, snapshot(turn("""{"kind":"$reason"}""")).turns.single().resultReasonKind)
        }
        // ResultUnavailableReason uses a scalar string rather than NoBriefReason's tagged object.
        val unavailable = JsonObject(turn(reason = "\"retention_gap\"") + (
            "result" to HolonWire.json.parseToJsonElement(
                """{"kind":"unavailable","reason":"retention_gap","retryable":false}""",
            )
        ))
        assertEquals("retention_gap", snapshot(unavailable).turns.single().resultReasonKind)
        assertNull(snapshot(turn(reason = "null", truncated = false)).turns.single().resultReasonKind)
        assertFalse(snapshot(turn(truncated = false)).turns.single().inputsTruncated)
    }

    @Test fun `history merge keeps newest fields and raw based reconstruction preserves them`() {
        val initial = snapshot(turn())
        val history = snapshot(turn(reason = """{"kind":"interrupted"}""", truncated = false, revision = 1))
        val merged = mergeConversationPage(initial, history, history = true).turns.single()
        assertEquals("tool_only_wait", merged.resultReasonKind)
        assertTrue(merged.inputsTruncated)
        val updated = mergeConversationPage(initial, snapshot(turn(
            reason = """{"kind":"aborted"}""", truncated = false, revision = 3,
        ))).turns.single()
        assertEquals("aborted", updated.resultReasonKind)
        assertFalse(updated.inputsTruncated)
    }

    @Test fun `stream publishes added fields only at the matching checkpoint`() {
        val initial = snapshot(turn(truncated = false, revision = 1))
        val reducer = ConversationStreamReducer(initial)
        fun event(raw: String) = HolonSseEvent(id = null, event = "message", data = raw)
        reducer.accept(event("""{"type":"batch_begin","batch_id":"batch","through_seq":9}"""))
        val incoming = turn(reason = """{"kind":"interrupted"}""")
        reducer.accept(event(JsonObject(mapOf(
            "type" to JsonPrimitive("turn_summary_upsert"),
            "turn" to incoming,
        )).toString()))
        assertFalse(reducer.snapshot.turns.single().inputsTruncated)
        val result = reducer.accept(event(
            """{"type":"checkpoint","batch_id":"batch","through_seq":9,"checkpoint":"cursor"}""",
        ))!!
        assertEquals("interrupted", result.turns.single().resultReasonKind)
        assertTrue(result.turns.single().inputsTruncated)
        assertEquals(incoming, result.turns.single().raw)
        assertEquals("cursor", result.snapshotCursor)
    }
}
