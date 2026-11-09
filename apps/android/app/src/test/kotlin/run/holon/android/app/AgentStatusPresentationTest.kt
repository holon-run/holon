package run.holon.android.app

import java.io.File
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.boolean
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.int
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import run.holon.android.sdk.AgentSummary
import run.holon.android.sdk.HolonAvatarExpression

class AgentStatusPresentationTest {
    private val agent = AgentSummary("agent", "Agent", false, "ready", "idle", "model", pending = 0, currentRunId = null)

    @Test fun `shared cross-client fixtures define kind icon and tone`() {
        val fixture = Json.parseToJsonElement(File(requireNotNull(System.getProperty("holon.agentStatusFixture"))).readText()).jsonObject
        val defaults = fixture.getValue("defaults").jsonObject
        for (entry in fixture.getValue("cases").jsonArray) {
            val case = entry.jsonObject
            val name = case.getValue("name").jsonPrimitive.content
            val input = defaults + case.getValue("input").jsonObject
            val kind = projectAgentStatus(
                lifecycle = input.getValue("lifecycle").jsonPrimitive.content,
                posture = input.getValue("posture").jsonPrimitive.content,
                currentRunId = input.getValue("currentRunId").jsonPrimitive.contentOrNull,
                pending = input.getValue("pending").jsonPrimitive.int,
                waitingCount = input.getValue("waitingCount").jsonPrimitive.int,
                stateIsCurrent = input.getValue("stateIsCurrent").jsonPrimitive.boolean,
            )
            val expected = case.getValue("expected").jsonObject
            assertEquals(expected.getValue("kind").jsonPrimitive.content, kind.key, name)
            assertEquals(expected.getValue("icon").jsonPrimitive.content, kind.icon, name)
            assertEquals(expected.getValue("tone").jsonPrimitive.content, kind.toneKey, name)
        }
    }

    @Test fun `primary labels are readable in English and Chinese`() {
        val labels = listOf("Stopped", "Needs input", "Running", "Runnable", "Waiting", "Blocked", "Ready", "Unknown", "Deleting")
        for ((kind, english) in AgentStatusKind.entries.zip(labels)) {
            assertEquals(english, UiCopy.translate(kind.sourceLabel, "en"))
            assertEquals(kind.sourceLabel, UiCopy.translate(kind.sourceLabel, "zh"))
        }
    }

    @Test fun `adapter ignores conflicting wait hints and offline attention`() {
        val value = agent.copy(schedulingPosture = "idle", waitingReason = "awaiting_operator_input")
        assertEquals(AgentStatusKind.Ready, value.statusPresentation().kind)
        assertFalse(value.needsReply())
        assertEquals(AgentStatusKind.Waiting, value.copy(schedulingPosture = "unknown").statusPresentation().kind)
        assertEquals(AgentStatusKind.Unknown, value.copy(schedulingPosture = "active_turn", runtimeStatus = "offline").statusPresentation().kind)
        assertEquals(AgentStatusKind.Stopped, value.copy(registryStatus = "stopped", schedulingPosture = "active_turn").statusPresentation().kind)
        assertEquals(AgentStatusKind.Deleting, value.copy(registryStatus = "deleting").statusPresentation().kind)
    }

    @Test fun `explicit posture maps to artwork and readable status together`() {
        val expressions = mapOf(
            "active_turn" to HolonAvatarExpression.Focused,
            "waiting_for_operator" to HolonAvatarExpression.Questioning,
            "waiting_for_task" to HolonAvatarExpression.Waiting,
            "waiting_for_external" to HolonAvatarExpression.Waiting,
            "blocked" to HolonAvatarExpression.Waiting,
            "idle" to HolonAvatarExpression.Neutral,
            "has_queued_input" to HolonAvatarExpression.Neutral,
            "unknown" to HolonAvatarExpression.Neutral,
        )
        for ((posture, expression) in expressions) {
            val value = agent.copy(schedulingPosture = posture)
            assertEquals(expression, value.statusPresentation().expression, posture)
            assertEquals(value.statusLabel(), value.statusPresentation().label)
            assertEquals(HolonAvatarExpression.Neutral, value.statusPresentation(live = false).expression)
        }
    }

    @Test fun `offline stopped and incomplete sleeping evidence never imply activity`() {
        for (status in listOf("offline", "stopped", "archived", "deleted", "unloaded")) {
            assertEquals(HolonAvatarExpression.Neutral,
                agent.copy(runtimeStatus = status, schedulingPosture = "active_turn").statusPresentation().expression)
        }
        assertEquals(HolonAvatarExpression.Neutral,
            agent.copy(runtimeStatus = "asleep", schedulingPosture = "idle").statusPresentation().expression)
        assertEquals(HolonAvatarExpression.Neutral,
            agent.copy(registryStatus = "deleting", schedulingPosture = "waiting_for_operator").statusPresentation().expression)
    }

    @Test fun `renaming never changes the avatar identity key`() {
        val renamed = agent.copy(displayName = "新的名字")
        assertEquals(AgentAvatarKey(agent.id, agent.statusPresentation().expression, 128),
            AgentAvatarKey(renamed.id, renamed.statusPresentation().expression, 128))
    }
}
