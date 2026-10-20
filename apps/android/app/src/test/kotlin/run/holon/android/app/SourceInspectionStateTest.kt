package run.holon.android.app

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertNull
import kotlinx.serialization.json.buildJsonObject
import run.holon.android.sdk.AgentSummary
import run.holon.android.sdk.HolonConversationSnapshot
import run.holon.android.sdk.HolonCurrentUser
import run.holon.android.sdk.HolonServerInfo
import run.holon.android.sdk.HolonTurnInput

class SourceInspectionStateTest {
    private val session = ActiveSession("network", "https://fixture.example/api/",
        HolonCurrentUser("user", null, "session"), "runtime", "visibility",
        HolonServerInfo("", "local", true, emptySet()))
    private val agent = AgentSummary("agent", "Agent", false, "active", "idle", "", pending = 0,
        currentRunId = null, incarnation = 1)
    private val snapshot = HolonConversationSnapshot(buildJsonObject {}, null, emptyList(), emptyList(),
        "runtime", "epoch", false, null)
    private val source = SourceInspection(
        SourceInspectionScope(session.scopeKey, agent.id, 1, "runtime", "epoch"), "turn",
        HolonTurnInput("message", "preview", null, "operator"), 1,
    )
    private fun state(): HolonUiState {
        val base = HolonUiState(session = session, selectedAgent = agent, conversation = snapshot)
        return base.copy(conversationState = base.conversationState.copy(sourceInspection = source))
    }

    @Test fun `ordinary shell updates preserve source and independent expanded process`() {
        assertEquals(source, state().copy(error = "temporary").sourceInspection)
        assertEquals(BackTarget.Source, state().backTarget())
    }

    @Test fun `agent incarnation epoch and runtime changes retire source selection`() {
        assertNull(state().copy(selectedAgent = agent.copy(incarnation = 2)).sourceInspection)
        assertNull(state().copy(selectedAgent = agent.copy(id = "another")).sourceInspection)
        assertNull(state().copy(conversation = snapshot.copy(eventLogEpoch = "next")).sourceInspection)
        assertNull(state().copy(conversation = snapshot.copy(runtimeId = "next")).sourceInspection)
    }

    @Test fun `logout and identity transitions cannot retain source data`() {
        assertNull(state().copy(session = null).sourceInspection)
        assertNull(state().copy(session = session.copy(user = HolonCurrentUser("other", null, "session"))).sourceInspection)
        val target = NetworkProfile("other", "Other", "https://other.example/api/", false)
        assertNull(state().forSwitchingNetwork(target).sourceInspection)
    }
}
