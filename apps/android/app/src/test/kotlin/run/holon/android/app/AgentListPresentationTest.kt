package run.holon.android.app

import kotlin.test.*
import kotlinx.serialization.json.*
import run.holon.android.sdk.*

class AgentListPresentationTest {
    private fun agent(model: String = "provider@model") = AgentSummary("tester", "Tester", false, "ready", "idle", model, pending = 0, currentRunId = null)
    private fun snapshot(presentation: String = "operator", brief: Boolean = false) = HolonConversationSnapshot.from(HolonJsonDocument(Json.parseToJsonElement("""
        {"turns":[{"turn_id":"turn","presentation_class":"$presentation","inputs":[{"message_id":"input","preview":"Review the report","presentation_class":"$presentation","created_at":"2026-10-03T00:01:00Z"}],"brief_ids":${if (brief) "[\"brief\"]" else "[]"}}]}
    """)))

    @Test fun `unfinished operator turn supplies input until a brief arrives`() {
        assertEquals("Review the report", snapshot().operatorPreview()?.text)
        assertNull(snapshot(brief = true).operatorPreview())
        assertNull(snapshot(presentation = "internal").operatorPreview())
        assertNull(snapshot(presentation = "external").operatorPreview())
    }

    @Test fun `new input takes precedence over older brief not newer brief`() {
        val preview = snapshot().operatorPreview()
        val agent = agent().copy(latestBrief = HolonLatestBrief("old", "2026-10-03T08:00:00+08:00", "Old result", 4))
        assertEquals("Review the report", agent.inputPreview(preview))
        assertNull(agent.copy(latestBrief = agent.latestBrief!!.copy(createdAt = "2026-10-03T00:02:00Z")).inputPreview(preview))
        assertEquals("Review the report", agent().inputPreview(preview))
    }

    @Test fun `common models use current roster without inventing unavailable catalog entries`() {
        val options = listOf("unused", "popular", "current").map { HolonModelOption(it, it, "provider") }
        val agents = listOf(agent("popular"), agent("popular"), agent("missing"), agent("current"))
        assertEquals(listOf("current", "popular"), commonModelOptions(options, agents, "current").map { it.model })
    }

    @Test fun `inline turn and tool expansion are not navigation destinations`() {
        val state = HolonUiState(selectedAgent = agent(), selectedTurn = snapshot().turns.single())
        assertEquals(BackTarget.Agents, state.backTarget())
        val expanded = state.copy(selectedActivity = HolonConversationActivity("activity", "tool", "Read", null, 1, buildJsonObject {}))
        assertEquals(BackTarget.Agents, expanded.backTarget())
        assertEquals(BackTarget.Activity, expanded.copy(fullScreenTurn = true).backTarget())
        assertEquals(BackTarget.FullScreenTurn, state.copy(fullScreenTurn = true).backTarget())
    }

    @Test fun `task detail goes back to work and work goes back to conversation`() {
        val state = HolonUiState(selectedAgent = agent(), agentSection = AgentSection.Work, selectedTask = HolonTaskSnapshot("task", "running", "Build", buildJsonObject {}))
        assertEquals(BackTarget.Task, state.backTarget())
        assertEquals(BackTarget.Conversation, state.copy(selectedTask = null).backTarget())
        assertEquals(BackTarget.Agents, state.copy(selectedTask = null, agentSection = AgentSection.Results).backTarget())
    }
}
