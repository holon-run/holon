package run.holon.android.sdk

import kotlinx.serialization.decodeFromString
import run.holon.client.wire.generated.models.AgentListEntry
import run.holon.client.wire.generated.models.ErrorResponse
import run.holon.client.wire.generated.models.HandshakeResponse
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertIs
import kotlin.test.assertTrue

class SdkFoundationTest {
    @Test
    fun `handshake fixture decodes unknown fields and checks compatibility`() {
        val handshake =
            HolonWire.json.decodeFromString<HandshakeResponse>(
                fixture("handshake-v1.json"),
            )

        val result =
            handshake.checkCompatibility(
                requiredCapabilities = setOf("agents.list"),
            )
        val compatible = assertIs<CompatibilityResult.Compatible>(result)

        assertEquals("main", compatible.server.defaultAgentId)
        assertEquals("bearer", compatible.server.authMode)
        assertTrue(compatible.server.authRequired)
    }

    @Test
    fun `agent roster fixture maps generated wire model to stable summary`() {
        val agents =
            HolonWire.json.decodeFromString<List<AgentListEntry>>(
                fixture("agent-list-v1.json"),
            )

        assertEquals(
            AgentSummary(
                id = "main",
                displayName = "Primary",
                isDefault = true,
                registryStatus = "active",
                runtimeStatus = "awake_idle",
                effectiveModel = "openai/gpt-5.6",
                pending = 2,
                currentRunId = null,
            ),
            agents.single().toAgentSummary(),
        )
    }

    @Test
    fun `agent roster preserves future enum values`() {
        val agent =
            HolonWire.json.decodeFromString<List<AgentListEntry>>(
                fixture("agent-list-future-enums.json"),
            ).single()

        assertEquals("policy_selected", agent.model.source.value)
        assertEquals("archived", agent.toAgentSummary().registryStatus)
        assertEquals("suspended", agent.toAgentSummary().runtimeStatus)
    }

    @Test
    fun `error fixture preserves machine code and diagnostic detail`() {
        val error =
            HolonWire.json.decodeFromString<ErrorResponse>(
                fixture("error-v1.json"),
            ).toHolonApiError()

        assertEquals("invalid_json", error.code)
        assertEquals("unknown field `kind`", error.detail)
        assertEquals("http", error.domain)
        assertEquals(false, error.retryable)
    }

    @Test
    fun `error fixture preserves a future domain value`() {
        val error =
            HolonWire.json.decodeFromString<ErrorResponse>(
                fixture("error-future-domain.json"),
            ).toHolonApiError()

        assertEquals("scheduler_v2", error.domain)
    }

    private fun fixture(name: String): String =
        requireNotNull(javaClass.getResource("/$name")) {
            "missing fixture: $name"
        }.readText()
}
