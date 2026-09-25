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

    @Test
    fun `conversation unwraps structured text previews and preserves unknown fields`() {
        val snapshot =
            HolonConversationSnapshot.from(
                HolonJsonDocument(
                    HolonWire.json.parseToJsonElement(
                        """{"turns":[{"turn_id":"turn-1","presentation_class":"operator","inputs":[{"message_id":"msg-1","preview":"{\"type\":\"text\",\"text\":\"hello\"}","presentation_class":"operator"}],"execution":{"kind":"terminal","outcome":"completed"},"result":{"kind":"available"},"brief_ids":[],"future":true}],"pending_inputs":[],"future_root":true}""",
                    ),
                ),
            )

        assertEquals("hello", snapshot.turns.single().inputs.single().preview)
        assertEquals("operator", snapshot.turns.single().presentationClass)
        assertEquals("Work result available", snapshot.turns.single().summary)
    }

    @Test
    fun `conversation exposes the server supplied history cursor`() {
        val snapshot = HolonConversationSnapshot.from(
            HolonJsonDocument(
                HolonWire.json.parseToJsonElement(
                    """{"turns":[],"pending_inputs":[],"has_more":true,"next_before_cursor":"before-older","event_log_epoch":"epoch-1"}""",
                ),
            ),
        )

        assertEquals(true, snapshot.hasMore)
        assertEquals("before-older", snapshot.nextBeforeCursor)
        assertEquals("epoch-1", snapshot.eventLogEpoch)
    }

    @Test
    fun `terminal turn without result is not projected as work in progress`() {
        val snapshot =
            HolonConversationSnapshot.from(
                HolonJsonDocument(
                    HolonWire.json.parseToJsonElement(
                        """{"turns":[{"turn_id":"turn-1","execution":{"kind":"terminal","outcome":"completed"},"result":{"kind":"none","reason":"no_brief"},"brief_ids":[],"completed_at":"2026-09-25T08:00:00Z"}],"pending_inputs":[]}""",
                    ),
                ),
            )

        assertEquals("terminal", snapshot.turns.single().executionKind)
        assertEquals("Work completed", snapshot.turns.single().summary)
    }

    @Test
    fun `completion timestamp wins over stale active execution`() {
        val snapshot =
            HolonConversationSnapshot.from(
                HolonJsonDocument(
                    HolonWire.json.parseToJsonElement(
                        """{"turns":[{"turn_id":"turn-1","execution":{"kind":"active"},"result":{"kind":"pending"},"brief_ids":[],"completed_at":"2026-09-25T08:00:00Z"}],"pending_inputs":[]}""",
                    ),
                ),
            )

        assertEquals("Work completed", snapshot.turns.single().summary)
    }

}
