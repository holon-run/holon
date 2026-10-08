package run.holon.android.app

import java.time.ZoneId
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertNull
import kotlinx.serialization.json.buildJsonObject
import run.holon.android.sdk.HolonConversationTurn
import run.holon.android.sdk.HolonPendingInput

class ConversationPresentationTest {
    @Test fun `pending operator bubbles remain separate from one compact background group`() {
        val inputs = listOf(
            pending("background-b", "internal", "2026-10-08T08:32:00Z"),
            pending("operator", "operator", "2026-10-08T08:31:00Z"),
            pending("background-a", "external", "2026-10-08T08:30:00Z"),
            pending("legacy", null, null),
        )
        val result = pendingConversationInputs(inputs)
        assertEquals(listOf("operator"), result.operator.map { it.messageId })
        assertEquals(listOf("legacy", "background-a", "background-b"), result.background.map { it.messageId })
        assertEquals(listOf("pending:operator", "pending-background"), result.keys)
        assertEquals(inputs.size, result.operator.size + result.background.size)
    }

    @Test fun `background reading anchor survives queue additions and removals`() {
        val first = pending("first", "internal", null)
        val next = pending("next", "internal", null)
        assertEquals(listOf("pending-background"), pendingConversationInputs(listOf(first)).keys)
        assertEquals(listOf("pending-background"), pendingConversationInputs(listOf(first, next)).keys)
        assertEquals(listOf("pending-background"), pendingConversationInputs(listOf(next)).keys)
    }

    @Test fun `pending inputs use message ids to break timestamp ties`() {
        val inputs = listOf(pending("b", "operator", null), pending("a", "operator", null))
        assertEquals(listOf("a", "b"), pendingConversationInputs(inputs).operator.map { it.messageId })
        assertEquals(listOf("pending:a", "pending:b"), pendingConversationInputs(inputs).keys)
    }

    @Test fun `empty queue has no background card anchor`() {
        assertEquals(emptyList(), pendingConversationInputs(emptyList()).keys)
        val input = pending("operator", "operator", null)
        assertEquals(listOf("pending:operator"), pendingConversationInputs(listOf(input)).keys)
    }

    private fun pending(id: String, presentation: String?, createdAt: String?) =
        HolonPendingInput(id, "queued", "Message $id", createdAt, presentation)

    @Test fun `saved anchor follows content id when preceding history changes`() {
        assertEquals(2, readingAnchorIndex(listOf("older", "a", "brief:turn:b", "c"), "brief:turn:b", 1))
        assertEquals(0, readingAnchorIndex(listOf("only"), "expired", 100))
        assertEquals(0, readingAnchorIndex(emptyList(), "expired", 100))
    }
    @Test fun `every brief gets a stable reading anchor including multiple briefs in a turn`() {
        val turn = turn("one", listOf("a", "b", "a"))
        val rows = conversationRows(listOf(turn))
        assertEquals(listOf("a", "b"), rows.filterIsInstance<ConversationRow.Brief>().map { it.id })
        val after = conversationRows(listOf(turn("older", listOf("old")), turn))
        val anchors = rows.filterNot { it is ConversationRow.Day }.map { it.key }
        assertEquals(anchors, after.filter { it.key in anchors }.map { it.key })
    }

    @Test fun `local calendar date and clock both convert from UTC`() {
        val zone = ZoneId.of("Asia/Shanghai")
        assertEquals("09-29 15:12", localTimestamp("2026-09-29T07:12:00Z", zone))
        assertEquals("2026-09-30", localDate("2026-09-29T23:12:00Z", zone))
        assertNull(localDate("invalid", zone))
        assertEquals("invalid", localTimestamp("invalid", zone))
    }

    @Test fun `date keys remain unique if imported timestamps are not monotonic`() {
        val rows = conversationRows(listOf(turn("a", emptyList()), turn("b", emptyList()).copy(startedAt = "2026-09-30T07:12:00Z"), turn("c", emptyList())))
        assertEquals(rows.size, rows.map { it.key }.distinct().size)
    }

    private fun turn(id: String, briefs: List<String>) = HolonConversationTurn(id, "", "operator", emptyList(), "idle", null, "available", null, briefs, "2026-09-29T07:12:00Z", "2026-09-29T07:13:00Z", true, buildJsonObject {})
}
