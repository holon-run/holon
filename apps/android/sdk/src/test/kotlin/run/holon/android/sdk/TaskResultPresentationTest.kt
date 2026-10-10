package run.holon.android.sdk

import kotlinx.serialization.json.*
import kotlin.test.*

class TaskResultPresentationTest {
    @Test fun `cold snapshot and pending projection decode the same optional metadata`() {
        val inputs = HolonWire.json.parseToJsonElement(javaClass.getResource("/task-result-inputs.json")!!.readText()) as JsonArray
        val raw = buildJsonObject {
            put("turns", buildJsonArray { add(buildJsonObject { put("turn_id", "turn"); put("inputs", inputs) }) })
            put("pending_inputs", JsonArray(inputs.map { JsonObject((it as JsonObject) + mapOf("state" to JsonPrimitive("queued"))) }))
        }
        val state = HolonConversationSnapshot.from(HolonJsonDocument(raw))
        assertEquals(8, state.turns.single().inputs.size)
        assertEquals(state.turns.single().inputs.map { it.taskResult }, state.pendingInputs.map { it.taskResult })
        assertEquals(listOf("completed", "failed", "cancelled", "interrupted"), state.pendingInputs.take(4).map { it.taskResult!!.status })
        assertEquals("peer-reply", state.pendingInputs[4].taskResult!!.responseMessageId)
        assertEquals(false, state.pendingInputs[5].taskResult!!.runtimeOnly)
        assertNull(state.pendingInputs.last().taskResult!!.runtimeOnly)
        assertNull(state.pendingInputs.last().taskResult!!.summary)
    }

    @Test fun `missing metadata is compatible but malformed provenance is rejected`() {
        assertNull(HolonTaskResultPresentation.from(null))
        assertNull(HolonTaskResultPresentation.from(JsonNull))
        for (value in listOf(JsonPrimitive("true"), JsonPrimitive(1), buildJsonObject {})) {
            assertFailsWith<HolonProtocolException> {
                HolonTaskResultPresentation.from(buildJsonObject {
                    put("task_id", "task"); put("status", "completed"); put("preview", "ok"); put("runtime_only", value)
                })
            }
        }
    }
}
