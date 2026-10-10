package run.holon.android.app

import java.net.ConnectException
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertNull
import kotlinx.serialization.json.JsonObject
import run.holon.android.sdk.HolonContentReportCategory
import run.holon.android.sdk.HolonConversationActivity
import run.holon.android.sdk.HolonHttpException

class ContentReportTest {
    @Test
    fun `assistant activity derives the evidence id target`() {
        val target = contentReportTarget("agent-1", "turn-1", activity("assistant:ev-9", "assistant"))

        assertEquals(ContentReportTarget("agent-1", "turn-1", "ev-9"), target)
    }

    @Test
    fun `report action is exposed only for reportable activities and forwards the target`() {
        val captured = mutableListOf<ContentReportTarget>()

        val action = contentReportAction(activity("assistant:ev-9", "assistant"), "agent-1", "turn-1") { captured += it }

        assertEquals(true, action != null)
        action?.invoke()
        assertEquals(listOf(ContentReportTarget("agent-1", "turn-1", "ev-9")), captured)
        assertNull(contentReportAction(activity("tool:ev-9", "tool"), "agent-1", "turn-1") { captured += it })
    }

    @Test
    fun `non assistant activities are not reportable`() {
        assertNull(contentReportTarget("agent-1", "turn-1", activity("tool:ev-9", "tool")))
        assertNull(contentReportTarget("agent-1", "turn-1", activity("operator:ev-9", "operator")))
    }

    @Test
    fun `assistant activity without an evidence prefix is not reportable`() {
        assertNull(contentReportTarget("agent-1", "turn-1", activity("ev-9", "assistant")))
        assertNull(contentReportTarget("agent-1", "turn-1", activity("assistant:", "assistant")))
    }

    @Test
    fun `blank agent or turn is not reportable`() {
        assertNull(contentReportTarget(null, "turn-1", activity("assistant:ev-9", "assistant")))
        assertNull(contentReportTarget(" ", "turn-1", activity("assistant:ev-9", "assistant")))
        assertNull(contentReportTarget("agent-1", null, activity("assistant:ev-9", "assistant")))
    }

    @Test
    fun `category options mirror the sdk enum exactly once`() {
        assertEquals(
            HolonContentReportCategory.entries.toList(),
            contentReportCategoryOptions.map { it.category },
        )
        assertEquals(7, contentReportCategoryOptions.map { it.category }.toSet().size)
        assertEquals(true, contentReportCategoryOptions.all { it.sourceLabel.isNotBlank() })
    }

    @Test
    fun `request passes through the target and trims the description`() {
        val target = ContentReportTarget("agent-1", "turn-1", "ev-9")

        val request = contentReportRequest(target, HolonContentReportCategory.PRIVACY, "  leaking address  ", "req-1")

        assertEquals("agent-1", request.agentId)
        assertEquals("turn-1", request.turnId)
        assertEquals("ev-9", request.messageId)
        assertEquals(HolonContentReportCategory.PRIVACY, request.category)
        assertEquals("leaking address", request.description)
        assertEquals("req-1", request.clientRequestId)
    }

    @Test
    fun `blank description becomes null`() {
        val target = ContentReportTarget("agent-1", "turn-1", "ev-9")

        assertNull(contentReportRequest(target, HolonContentReportCategory.SPAM_OR_OTHER, "   ", "req-1").description)
        assertNull(contentReportRequest(target, HolonContentReportCategory.SPAM_OR_OTHER, null, "req-1").description)
    }

    @Test
    fun `description is capped at the server limit`() {
        assertEquals(CONTENT_REPORT_DESCRIPTION_LIMIT, normalizeContentReportDescription("a".repeat(5_000)).length)
        assertEquals("short", normalizeContentReportDescription("short"))
    }

    @Test
    fun `error mapping is report specific for known statuses`() {
        assertEquals("举报信息无效，请重新选择原因后提交", contentReportError(HolonHttpException(400, null)))
        assertEquals("这条内容已不可举报，可能已被移除", contentReportError(HolonHttpException(404, null)))
        assertEquals("举报过于频繁，请稍后再试", contentReportError(HolonHttpException(429, null)))
    }

    @Test
    fun `error mapping falls back to the shared transport mapping`() {
        assertEquals("服务端错误（HTTP 500）", contentReportError(HolonHttpException(500, null)))
        assertEquals("无法连接 Holon 主机，请确认 daemon 已启动", contentReportError(ConnectException("refused")))
    }

    @Test
    fun `an open report sheet owns the system back target and state round trips`() {
        val state = HolonUiState().copy(reportTarget = ContentReportTarget("agent-1", "turn-1", "ev-9"), reportSubmitting = true)

        assertEquals(ContentReportTarget("agent-1", "turn-1", "ev-9"), state.reportTarget)
        assertEquals(true, state.reportSubmitting)
        assertEquals(BackTarget.Report, state.backTarget())
    }

    private fun activity(id: String, kind: String): HolonConversationActivity =
        HolonConversationActivity(
            id = id,
            kind = kind,
            summary = "summary",
            eventSeq = 1,
            revision = 1,
            raw = JsonObject(emptyMap()),
        )
}
