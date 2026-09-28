package run.holon.android.app

import java.time.ZoneId
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertNull
import kotlinx.serialization.json.buildJsonObject
import run.holon.android.sdk.HolonConversationTurn

class ConversationPresentationTest {
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
