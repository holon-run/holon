package run.holon.android.sdk

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.jsonObject
import kotlin.test.*

class ConversationActivityToleranceTest {
    @Test fun `unknown model_request kind decodes and stays generic`() {
        val raw = Json.parseToJsonElement(
            """{"activities":[{"id":"model_request:t:1:1","kind":"model_request","summary":"fixture@default/primary-model · succeeded · 12ms","key":{"event_seq":5,"activity_id":"model_request:t:1:1"},"revision":2,"provider":"fixture","model_ref":"fixture@default/primary-model","dispatch_reason":"initial","status":"succeeded","duration_ms":12}],"coverage":{"kind":"complete"},"has_more":false}"""
        ).jsonObject
        val activities = parseConversationActivities(raw)
        assertEquals(1, activities.size)
        val activity = activities.single()
        assertEquals("model_request", activity.kind)
        assertNull(activity.toolExecutionId)
        assertEquals(5L, activity.eventSeq)
        assertEquals(2L, activity.revision)
        assertTrue(activity.summary.contains("succeeded"))
    }
}
