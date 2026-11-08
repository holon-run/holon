package run.holon.android.app

import kotlin.test.Test
import kotlin.test.assertEquals
import run.holon.android.sdk.AgentSummary
import run.holon.android.sdk.HolonAvatarExpression

class AgentStatusPresentationTest {
    private val agent = AgentSummary("agent", "Agent", false, "ready", "idle", "model", pending = 0, currentRunId = null)

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
