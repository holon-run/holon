package run.holon.android.app

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlinx.serialization.json.Json
import run.holon.android.sdk.HolonConversationSnapshot
import run.holon.android.sdk.HolonJsonDocument

class HolonRepositoryTest {
    @Test
    fun `conversation cache keeps loaded older turns while using incoming pagination state`() {
        val cached = snapshot(
            """{"runtime_id":"runtime-1","event_log_epoch":"epoch-1","turns":[{"turn_id":"new","started_at":"2026-09-27T10:00:00Z"}],"pending_inputs":[]}""",
        )
        val incoming = snapshot(
            """{"runtime_id":"runtime-1","event_log_epoch":"epoch-1","turns":[{"turn_id":"old","started_at":"2026-09-27T09:00:00Z"}],"pending_inputs":[],"has_more":true,"next_before_cursor":"cursor-1"}""",
        )

        val merged = mergeConversationSnapshots(cached, incoming)

        assertEquals(listOf("old", "new"), merged.turns.map { it.id })
        assertEquals(true, merged.hasMore)
        assertEquals("cursor-1", merged.nextBeforeCursor)
    }

    @Test
    fun `conversation cache discards data from another runtime scope`() {
        val cached = snapshot(
            """{"runtime_id":"runtime-old","event_log_epoch":"epoch-1","turns":[{"turn_id":"old"}]}""",
        )
        val incoming = snapshot(
            """{"runtime_id":"runtime-new","event_log_epoch":"epoch-1","turns":[{"turn_id":"new"}]}""",
        )

        val merged = mergeConversationSnapshots(cached, incoming)

        assertEquals(listOf("new"), merged.turns.map { it.id })
    }

    private fun snapshot(raw: String): HolonConversationSnapshot =
        HolonConversationSnapshot.from(
            HolonJsonDocument(Json.parseToJsonElement(raw)),
        )
}
