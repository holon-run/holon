package run.holon.android.app

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertTrue
import kotlinx.serialization.json.buildJsonObject
import run.holon.android.sdk.HolonConversationTurn

class TurnExpansionPolicyTest {
    private val active = HolonConversationTurn(
        "active", "", "operator", emptyList(), "active", null, "pending", null,
        emptyList(), null, null, false, buildJsonObject {},
    )
    private val ended = active.copy(
        executionKind = "terminal", terminalOutcome = "completed", settled = true,
        resultKind = "available", briefIds = listOf("result"), completedAt = "2026-10-20T16:00:00Z",
    )

    @Test fun `active defaults open but a manual close is respected`() {
        assertEquals(active.id, preferredExpandedTurn(listOf(active), null, null, emptySet(), null, emptySet(), false))
        assertEquals(null, preferredExpandedTurn(listOf(active), null, null, setOf(active.id), null, emptySet(), false))
    }

    @Test fun `manual historical choice wins over a new active turn`() {
        assertEquals("history", preferredExpandedTurn(listOf(active), null, "history", emptySet(), null, emptySet(), false))
    }

    @Test fun `reading history without a selection prevents automatic expansion and loading`() {
        assertEquals(null, preferredExpandedTurn(listOf(ended, active), null, null, emptySet(), null, setOf("result"), true))
        assertEquals(active.id, preferredExpandedTurn(listOf(ended, active), null, null, emptySet(), null, setOf("result"), false))
        assertEquals("history", preferredExpandedTurn(listOf(ended, active), null, "history", emptySet(), null, setOf("result"), true))
    }

    @Test fun `automatic details stay open until every result is readable`() {
        assertEquals(ended.id, preferredExpandedTurn(listOf(ended), ended, null, emptySet(), ended.id, emptySet(), false))
        assertEquals(null, preferredExpandedTurn(listOf(ended), ended, null, emptySet(), ended.id, setOf("result"), false))
        assertFalse(canAutoCollapseTurn(ended.copy(briefIds = listOf("result", "later")), setOf("result")))
    }

    @Test fun `reading manual selection and deep links are not collapsed on delivery`() {
        assertEquals(ended.id, preferredExpandedTurn(listOf(ended), ended, null, emptySet(), ended.id, setOf("result"), true))
        assertEquals(ended.id, preferredExpandedTurn(listOf(ended), ended, ended.id, emptySet(), ended.id, setOf("result"), false))
        assertEquals(ended.id, preferredExpandedTurn(listOf(ended), ended, null, emptySet(), null, setOf("result"), false))
    }

    @Test fun `attention failures unavailable and truncated input never auto collapse`() {
        assertFalse(canAutoCollapseTurn(ended.copy(attentionKind = "operator"), setOf("result")))
        assertFalse(canAutoCollapseTurn(ended.copy(terminalOutcome = "provider_failed_needs_recovery"), setOf("result")))
        assertFalse(canAutoCollapseTurn(ended.copy(resultKind = "unavailable"), setOf("result")))
        assertFalse(canAutoCollapseTurn(ended.copy(inputsTruncated = true), setOf("result")))
        assertTrue(canAutoCollapseTurn(ended.copy(resultKind = "none", briefIds = emptyList()), emptySet()))
    }
}
