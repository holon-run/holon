package run.holon.android.sdk

import kotlin.test.*
import kotlinx.serialization.json.*

class HolonTaskModelsTest {
    @Test fun `task list and status retain command and child progress`() {
        val listed = HolonTaskSnapshot.from(Json.parseToJsonElement("""{"id":"task-1","kind":"command","status":"running","summary":"Build","detail":{"cmd":"make web"}}""").jsonObject)
        assertEquals("task-1", listed.taskId)
        assertEquals("make web", listed.command)
        val detail = HolonTaskSnapshot.from(Json.parseToJsonElement("""{"task_id":"task-2","kind":"child_agent","status":"running","child_agent_id":"child","child_observability":{"last_progress_brief":"Tests passing"},"future_field":true}""").jsonObject)
        assertEquals("child", detail.childAgentId)
        assertEquals("Tests passing", detail.progress)
    }

    @Test fun `task output preserves truncation and result without claiming full content`() {
        val output = HolonTaskOutputSnapshot.from(Json.parseToJsonElement("""{"task_id":"task-1","status":"completed","output_preview":"First lines","output_truncated":true,"result_summary":"Build passed"}""").jsonObject)
        assertTrue(output.truncated)
        assertEquals("First lines", output.outputPreview)
        assertEquals("Build passed", output.resultSummary)
    }
}
