package run.holon.android.app

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import run.holon.android.sdk.AgentSummary
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
}
