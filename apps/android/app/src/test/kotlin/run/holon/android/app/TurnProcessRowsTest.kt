package run.holon.android.app

import kotlin.test.*
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.Json
import run.holon.android.sdk.*

class TurnProcessRowsTest {
    @Test fun `interjection keeps its canonical position and actor`() {
        val state = HolonConversationSnapshot.from(HolonJsonDocument(Json.parseToJsonElement(
            """{"turns":[{"turn_id":"turn","inputs":[{"message_id":"late-input","presentation_class":"operator","preview":"additional requirement","actor_display_name":"Alice","interjected":true,"activity_key":{"event_seq":12,"activity_id":"operator:late-input"}}]}]}"""
        )))
        val activities = listOf(HolonConversationActivity("before", "tool", "", 10, 1, kotlinx.serialization.json.buildJsonObject {}),
            HolonConversationActivity("after", "assistant", "", 14, 1, kotlinx.serialization.json.buildJsonObject {}))
        val rows = turnProcessRows(state.turns.single(), activities)
        assertEquals(listOf(10L, 12L, 14L), rows.map { it.seq })
        assertEquals("Alice", (rows[1] as TurnProcessRow.Interjection).input.actorDisplayName)
    }
    @Test fun `task input stays in its receipt turn before activities and later inputs use canonical order`() {
        val state = HolonConversationSnapshot.from(HolonJsonDocument(Json.parseToJsonElement(
            """{"turns":[{"turn_id":"receipt","inputs":[
                {"message_id":"initial","task_result":{"task_id":"task","status":"failed","preview":"Failure reason","runtime_only":true}},
                {"message_id":"late","interjected":true,"activity_key":{"event_seq":12,"activity_id":"operator:late"},"task_result":{"task_id":"late-task","status":"completed","preview":"Later result"}}
            ]}]}"""
        )))
        val activities = listOf(HolonConversationActivity("before", "tool", "", 10, 1, buildJsonObject {}),
            HolonConversationActivity("after", "assistant", "", 14, 1, buildJsonObject {}))
        val rows = turnProcessRows(state.turns.single(), activities)
        assertEquals(listOf("input:initial", "activity:before", "input:late", "activity:after"), rows.map { it.key })
        assertEquals("Failure reason", state.turns.single().taskResultHeader()!!.taskResult!!.preview)
        assertEquals("failed", state.turns.single().taskResultHeader()!!.taskResult!!.status)
    }

    @Test fun `legacy brief uses provenance and links while explicit model response survives`() {
        val state = HolonConversationSnapshot.from(HolonJsonDocument(Json.parseToJsonElement(
            """{"turns":[{"turn_id":"receipt","inputs":[{"message_id":"initial","task_result":{"task_id":"task","status":"completed","preview":"Same text","runtime_only":true}}],"brief_ids":["brief"]}]}"""
        )))
        val turn = state.turns.single()
        val brief = HolonBrief("brief", "agent", null, "result", "", "Same text", emptyList(), null, "initial")
        assertTrue(turn.isRuntimeTaskBrief(brief))
        assertFalse(turn.isRuntimeTaskBrief(brief.copy(relatedMessageId = "other")))
        val modelTurn = turn.copy(inputs = turn.inputs.map { it.copy(taskResult = it.taskResult!!.copy(runtimeOnly = false)) })
        assertFalse(modelTurn.isRuntimeTaskBrief(brief.copy(relatedTaskId = "task")))
        assertTrue(conversationRows(listOf(turn), briefs = mapOf("brief" to brief)).none { it is ConversationRow.Brief })
        assertTrue(conversationRows(listOf(modelTurn), briefs = mapOf("brief" to brief)).any { it is ConversationRow.Brief })
    }

    @Test fun `command failure header prefers a cause and excludes the host output path`() {
        assertEquals("Missing manifest", taskResultFailureReason("command task failed: Build\noutput_path: /host/output\nexit_status: 7\noutput_summary:\nstderr:\nMissing manifest"))
        assertEquals("exit_status: 7", taskResultFailureReason("command task failed: Build\noutput_path: /host/output\nexit_status: 7"))
    }

    @Test fun `command process preview retains output line breaks without host metadata`() {
        assertEquals("first\nsecond", taskResultPreview("command task completed: Check\noutput_path: /host/output\nexit_status: 0\noutput_summary:\nfirst\nsecond"))
    }

}
