package run.holon.android.app

import kotlin.test.*
import kotlinx.serialization.json.Json
import run.holon.android.sdk.*

class TurnProcessRowsTest {
    @Test fun `interjection keeps its canonical position and actor`() {
        val state = HolonConversationSnapshot.from(HolonJsonDocument(Json.parseToJsonElement(
            """{"turns":[{"turn_id":"turn","inputs":[{"message_id":"late-input","presentation_class":"operator","preview":"additional requirement","actor_display_name":"Alice","interjected":true,"activity_key":{"event_seq":12,"activity_id":"operator:late-input"}}]}]}"""
        )))
        val activities = listOf(HolonConversationActivity("before", "tool", "", 10, 1, kotlinx.serialization.json.buildJsonObject {}),
            HolonConversationActivity("after", "assistant", "", 14, 1, kotlinx.serialization.json.buildJsonObject {}))
        val rows = turnProcessRows(state.turns.single(), activities)
        assertEquals(listOf(10L, 12L, 14L), rows.map { it.seq })
        assertEquals("Alice", (rows[1] as TurnProcessRow.Interjection).input.actorDisplayName)
    }
}
