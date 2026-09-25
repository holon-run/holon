package run.holon.android.app

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlinx.serialization.json.buildJsonObject
import run.holon.android.sdk.AgentSummary
import run.holon.android.sdk.HolonConversationTurn
import run.holon.android.sdk.HolonLatestBrief

class HolonAppModelTest {
    @Test
    fun `address is normalized to API root`() {
        assertEquals("https://holon.example/api/", normalizeAddress("https://holon.example"))
        assertEquals("https://holon.example/api/", normalizeAddress("https://holon.example/api/"))
        assertFailsWith<IllegalArgumentException> {
            normalizeAddress("https://holon.example/other")
        }
        assertEquals("http://10.0.2.2:7878/api/", normalizeAddress("http://10.0.2.2:7878"))
        assertFailsWith<IllegalArgumentException> {
            normalizeAddress("http://192.168.1.10:7878")
        }
        assertEquals(
            "http://192.168.1.10:7878/api/",
            normalizeAddress("http://192.168.1.10:7878", allowInsecureHttp = true),
        )
        assertEquals(
            "http://100.92.113.47:7878/api/",
            normalizeAddress("http://100.92.113.47:7878", allowInsecureHttp = true),
        )
    }

    @Test
    fun `recent conversations prioritize operator attention before recency`() {
        val ready = agent("ready", "2026-09-24T11:00:00Z")
        val olderAttention =
            agent("attention", "2026-09-23T11:00:00Z").copy(
                schedulingPosture = "waiting_for_operator",
                waitingReason = "awaiting_operator_input",
            )

        assertEquals(
            listOf("attention", "ready"),
            HolonUiState(agents = listOf(ready, olderAttention)).recentAgents.map { it.id },
        )
    }

    @Test
    fun `unread result ranks ahead of older read result without hiding attention`() {
        val read = agent("read", "2026-09-25T10:00:00Z")
        val unread = agent("unread", "2026-09-24T10:00:00Z")
        val attention = agent("attention", "2026-09-23T10:00:00Z").copy(schedulingPosture = "waiting_for_operator")
        val state = HolonUiState(
            agents = listOf(read, unread, attention),
            readBriefIds = mapOf(read.id to read.latestBrief!!.briefId),
            readBriefsLoaded = true,
        )

        assertEquals(listOf("attention", "unread", "read"), state.recentAgents.map { it.id })
        assertEquals(false, read.hasUnreadBrief(state.readBriefIds))
        assertEquals(true, unread.hasUnreadBrief(state.readBriefIds))
    }

    @Test
    fun `cache scope includes visibility boundary without separator collisions`() {
        assertEquals("1:a|2:bc|1:d", cacheScopeKey("a", "bc", "d"))
        assertEquals("2:ab|1:c|1:d", cacheScopeKey("ab", "c", "d"))
    }

    @Test
    fun `prompt body sizing includes base64 and json escaping`() {
        val attachment =
            StagedAttachment(
                kind = "file",
                name = "a\"b.txt",
                mediaType = "text/plain",
                localPath = "/unused",
                size = 3,
            )
        val expected =
            """{"text":"hi","client_request_id":"request-1","attachments":[{"kind":"file","name":"a\"b.txt","media_type":"text/plain","data_base64":"AQID"}]}"""

        assertEquals(
            expected.toByteArray(Charsets.UTF_8).size.toLong(),
            encodedPromptBodySize("hi", listOf(attachment), "request-1"),
        )
    }

    @Test
    fun `only an unfinished active turn is shown as executing`() {
        val active = turn(executionKind = "active")
        val staleActive = turn(executionKind = "active", completedAt = "2026-09-25T08:00:00Z")
        val terminal = turn(executionKind = "terminal", resultKind = "none", completedAt = "2026-09-25T08:00:00Z")

        assertEquals(true, active.isRunning())
        assertEquals(ui("执行中"), active.compactStatusText())
        assertEquals(false, staleActive.isRunning())
        assertEquals(null, staleActive.compactStatusText())
        assertEquals(false, terminal.isRunning())
        assertEquals(ui("没有结果摘要"), terminal.compactStatusText())
    }

    private fun agent(id: String, createdAt: String) =
        AgentSummary(
            id = id,
            displayName = id,
            isDefault = false,
            registryStatus = "active",
            runtimeStatus = "awake_idle",
            effectiveModel = "test",
            pending = 0,
            currentRunId = null,
            schedulingPosture = "idle",
            latestBrief = HolonLatestBrief("brief-$id", createdAt, "done", 1),
        )

    private fun turn(
        executionKind: String,
        resultKind: String = "pending",
        completedAt: String? = null,
    ) =
        HolonConversationTurn(
            id = "turn-1",
            summary = "",
            presentationClass = "operator",
            inputs = emptyList(),
            executionKind = executionKind,
            terminalOutcome = if (executionKind == "terminal") "completed" else null,
            resultKind = resultKind,
            attentionKind = null,
            briefIds = emptyList(),
            startedAt = "2026-09-25T07:59:00Z",
            completedAt = completedAt,
            settled = false,
            raw = buildJsonObject {},
        )
}
